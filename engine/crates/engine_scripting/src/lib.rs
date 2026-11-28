//! Rhai scripting runtime, ECS reflection bridge, gameplay API, hot-reload, and diagnostics.
//!
//! Provides:
//! - Pluggable [`ScriptBackend`] trait and sandboxed [`RhaiBackend`] with compiled AST caching
//! - Reflected [`ScriptComponent`] and inspector-editable [`ScriptProperties`]
//! - `Rhai <-> ECS Reflect` bridge via `AppTypeRegistry` and gameplay APIs (`transform`, `entity`,
//!   `input`, `physics`, `audio`, `world`, `events`)
//! - Lifecycle hooks (`on_start`, `on_update`, `on_fixed_update`, `on_collision`,
//!   `on_trigger_enter`, `on_trigger_exit`)
//! - Hot-reload for `.rhai` files on disk with structured [`ScriptDiagnostic`] reporting

pub mod api;
pub mod backend;
pub mod component;
pub mod diagnostics;
pub mod hot_reload;
pub mod reflect_bridge;
pub mod rhai_backend;

/// Commonly used scripting types, resources, components, and helpers.
pub mod prelude {
    pub use crate::{
        api::{ActiveScriptVoices, PendingScriptImpulses, ScriptCommand, ScriptCustomEvent},
        backend::{
            ScriptBackend, ScriptCompiledInfo, ScriptContactInfo, ScriptError, ScriptHook,
            ScriptHookArgs, ScriptHookSet, ScriptSandboxConfig, ScriptSourceKey,
        },
        component::{ScriptComponent, ScriptProperties, ScriptPropertyValue},
        diagnostics::{
            ScriptDiagnostic, ScriptDiagnosticEvent, ScriptDiagnosticSeverity,
            ScriptDiagnosticStage, ScriptDiagnostics,
        },
        hot_reload::{
            reload_modified_scripts, ScriptFileTracker, ScriptHotReloadConfig,
            ScriptHotReloadWatcher, ScriptReloadedEvent,
        },
        rhai_backend::RhaiBackend,
        EngineScriptingPlugin, ScriptExecutionOptions, ScriptRuntime,
        allow_in_scene_filter, register_components, reset_runtime_state,
        reset_runtime_state_with_diagnostics,
    };
}

use std::collections::{BTreeMap, HashSet};

use bevy_app::{App, FixedUpdate, Plugin, Update};
use bevy_ecs::{
    entity::Entity,
    name::Name,
    observer::On,
    reflect::{AppTypeRegistry, ReflectResource},
    resource::Resource,
    schedule::IntoScheduleConfigs,
    system::{Res, ResMut},
    world::World,
};
use bevy_math::{Vec2, Vec3};
use bevy_reflect::{std_traits::ReflectDefault, Reflect};
use bevy_time::{Fixed, Time};
use engine_assets::AssetRegistry;
use engine_core::{
    schedule::{EngineFixedSet, EngineSet},
    AssetUuid, EntityGuid, ProjectPaths,
};
use engine_physics::{CollisionEvent, ContactPhase, TriggerEvent};
use engine_scene::SceneSerializationFilter;

pub use prelude::*;

use crate::{
    api::{
        flush_pending_impulses, flush_script_commands, stop_active_script_voices,
        ScriptCallContext,
    },
    diagnostics::DiagnosticEntityContext,
    hot_reload::{ensure_script_compiled, poll_script_hot_reload_system, resolve_script_source},
};

/// Controls whether entity scripts execute in the current runtime mode (for example PIE vs Edit).
#[derive(Resource, Reflect, Debug, Clone, Copy, PartialEq, Eq)]
#[reflect(Resource, Default)]
pub struct ScriptExecutionOptions {
    /// When `true`, script lifecycle hooks run during `Update` and `FixedUpdate`.
    ///
    /// Set to `false` in Editor Edit mode and `true` during PIE Play or standalone runtime.
    pub enabled: bool,
    /// When set `true` (for example via [`Self::request_reset`]), clears all per-entity runtime
    /// state on the next scripting tick before invoking hooks.
    pub reset_requested: bool,
}

impl Default for ScriptExecutionOptions {
    fn default() -> Self {
        Self::gameplay()
    }
}

impl ScriptExecutionOptions {
    /// Configuration for Editor Edit mode (scripts paused until PIE starts).
    #[must_use]
    pub const fn editor() -> Self {
        Self {
            enabled: false,
            reset_requested: false,
        }
    }

    /// Configuration for standalone Play or PIE active gameplay.
    #[must_use]
    pub const fn gameplay() -> Self {
        Self {
            enabled: true,
            reset_requested: false,
        }
    }

    /// Requests a clean runtime state reset on the next tick (useful when only mutable access
    /// to [`ScriptExecutionOptions`] is available).
    pub fn request_reset(&mut self) {
        self.reset_requested = true;
    }
}

/// ECS resource owning the active [`ScriptBackend`], shared call context, and tracked instances.
#[derive(Resource)]
pub struct ScriptRuntime {
    pub(crate) backend: Box<dyn ScriptBackend>,
    pub(crate) call_context: ScriptCallContext,
    pub(crate) scratch_world: Option<World>,
    active_entities: HashSet<Entity>,
    applied_sandbox: ScriptSandboxConfig,
}

impl Default for ScriptRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptRuntime {
    /// Creates a [`ScriptRuntime`] backed by [`RhaiBackend`] with default sandbox limits.
    #[must_use]
    pub fn new() -> Self {
        Self::with_sandbox(ScriptSandboxConfig::default())
    }

    /// Creates a [`ScriptRuntime`] backed by [`RhaiBackend`] using `sandbox` limits.
    #[must_use]
    pub fn with_sandbox(sandbox: ScriptSandboxConfig) -> Self {
        let call_context = ScriptCallContext::new();
        let backend = RhaiBackend::new(sandbox.clone(), call_context.clone());
        Self {
            backend: Box::new(backend),
            call_context,
            scratch_world: Some(World::new()),
            active_entities: HashSet::new(),
            applied_sandbox: sandbox,
        }
    }

    /// Creates a [`ScriptRuntime`] wrapping a custom [`ScriptBackend`] implementation.
    #[must_use]
    pub fn with_backend(backend: impl ScriptBackend, call_context: ScriptCallContext) -> Self {
        let applied_sandbox = backend.sandbox_config().clone();
        Self {
            backend: Box::new(backend),
            call_context,
            scratch_world: Some(World::new()),
            active_entities: HashSet::new(),
            applied_sandbox,
        }
    }

    /// Returns read-only access to the underlying [`ScriptBackend`].
    #[must_use]
    pub fn backend(&self) -> &dyn ScriptBackend {
        self.backend.as_ref()
    }

    /// Returns mutable access to the underlying [`ScriptBackend`].
    pub fn backend_mut(&mut self) -> &mut dyn ScriptBackend {
        self.backend.as_mut()
    }

    /// Returns the active [`ScriptSandboxConfig`].
    #[must_use]
    pub fn sandbox_config(&self) -> &ScriptSandboxConfig {
        self.backend.sandbox_config()
    }

    /// Updates sandbox limits on the active backend.
    pub fn set_sandbox_config(&mut self, config: ScriptSandboxConfig) {
        self.applied_sandbox = config.clone();
        self.backend.set_sandbox_config(config);
    }

    /// Compiles and caches an in-memory script under a newly generated [`AssetUuid`],
    /// accessible via `ScriptComponent::from_uuid(uuid)`.
    pub fn register_inline_script(
        &mut self,
        name: impl Into<String>,
        source_code: &str,
    ) -> Result<AssetUuid, ScriptError> {
        let uuid = AssetUuid::new();
        self.register_inline_script_with_uuid(uuid, name, source_code)?;
        Ok(uuid)
    }

    /// Compiles and caches an in-memory script under an explicit [`AssetUuid`],
    /// accessible via `ScriptComponent::from_uuid(uuid)`.
    pub fn register_inline_script_with_uuid(
        &mut self,
        uuid: AssetUuid,
        source_label: impl Into<String>,
        source_code: &str,
    ) -> Result<ScriptCompiledInfo, ScriptError> {
        let label = source_label.into();
        let key = ScriptSourceKey::Asset(uuid);
        self.backend.compile_source(&key, &label, source_code, None)
    }

    /// Compiles and caches `source_code` under an explicit [`ScriptSourceKey`].
    pub fn compile_source(
        &mut self,
        key: &ScriptSourceKey,
        source_label: &str,
        source_code: &str,
    ) -> Result<ScriptCompiledInfo, ScriptError> {
        self.backend
            .compile_source(key, source_label, source_code, None)
    }

    /// Clears all per-entity script scopes and `on_start` flags while keeping compiled AST caches warm.
    ///
    /// Intended for Play-In-Editor `Stop` transitions so the next `Play` session starts cleanly.
    pub fn reset_runtime_state(&mut self) {
        self.backend.reset_all_instances();
        self.active_entities.clear();
        self.call_context.discard_commands();
    }

    fn prune_stale_entities(&mut self, world: &World) {
        let stale: Vec<Entity> = self
            .active_entities
            .iter()
            .copied()
            .filter(|&entity| {
                world
                    .get::<ScriptComponent>(entity)
                    .is_none_or(|comp| !comp.enabled || comp.is_unassigned())
            })
            .collect();
        for entity in stale {
            self.active_entities.remove(&entity);
            self.backend.remove_entity_instance(entity);
        }
    }
}

/// Queue of physics collision and trigger events captured by Bevy observers for script dispatch.
#[derive(Resource, Default, Debug)]
pub struct PendingScriptContacts {
    collisions: Vec<CollisionEvent>,
    triggers: Vec<TriggerEvent>,
}

impl PendingScriptContacts {
    /// Clears all buffered collision and trigger contacts.
    pub fn clear(&mut self) {
        self.collisions.clear();
        self.triggers.clear();
    }

    /// Returns the total number of queued contact events awaiting dispatch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.collisions.len() + self.triggers.len()
    }

    /// Returns `true` when no contact events are queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.collisions.is_empty() && self.triggers.is_empty()
    }
}

/// Registers [`ScriptComponent`] and related scripting types in `AppTypeRegistry`.
pub fn register_components(app: &mut App) {
    if !app.world().contains_resource::<AppTypeRegistry>() {
        app.init_resource::<AppTypeRegistry>();
    }
    app.register_type::<Vec2>()
        .register_type::<Vec3>()
        .register_type::<AssetUuid>()
        .register_type::<Option<AssetUuid>>()
        .register_type::<ScriptPropertyValue>()
        .register_type::<BTreeMap<String, ScriptPropertyValue>>()
        .register_type::<ScriptProperties>()
        .register_type::<ScriptComponent>()
        .register_type::<ScriptExecutionOptions>()
        .register_type::<ScriptSandboxConfig>();
}

/// Adds [`ScriptComponent`] to a [`SceneSerializationFilter`] so `.level.ron` / `.prefab.ron`
/// and PIE snapshots persist scripts and their properties.
pub fn allow_in_scene_filter(filter: &mut SceneSerializationFilter) {
    filter.allow::<ScriptComponent>();
}

fn reset_world_runtime_buffers(world: &mut World) {
    if let Some(mut contacts) = world.get_resource_mut::<PendingScriptContacts>() {
        contacts.clear();
    }
    if let Some(mut impulses) = world.get_resource_mut::<PendingScriptImpulses>() {
        impulses.clear();
    }
    stop_active_script_voices(world);
    if let Some(mut options) = world.get_resource_mut::<ScriptExecutionOptions>() {
        options.reset_requested = false;
    }
}

/// Resets live per-entity scripting runtime state, queued physics contacts/impulses, and active
/// script-triggered audio voices without discarding the compiled AST cache (called on PIE Stop).
pub fn reset_runtime_state(world: &mut World) {
    reset_runtime_state_with_diagnostics(world, false);
}

/// Resets live per-entity scripting runtime state, optionally also clearing [`ScriptDiagnostics`].
pub fn reset_runtime_state_with_diagnostics(world: &mut World, clear_diagnostics: bool) {
    if let Some(mut runtime) = world.get_resource_mut::<ScriptRuntime>() {
        runtime.reset_runtime_state();
    }
    reset_world_runtime_buffers(world);
    if clear_diagnostics
        && let Some(mut diagnostics) = world.get_resource_mut::<ScriptDiagnostics>()
    {
        diagnostics.clear();
    }
}

/// Plugin installing the Rhai scripting runtime, reflection registration, physics contact
/// observers, hot-reload watcher, and `Update`/`FixedUpdate` execution systems.
#[derive(Debug, Default, Clone, Copy)]
pub struct EngineScriptingPlugin;

impl Plugin for EngineScriptingPlugin {
    fn build(&self, app: &mut App) {
        register_components(app);

        if let Some(mut filter) = app.world_mut().get_resource_mut::<SceneSerializationFilter>() {
            allow_in_scene_filter(&mut filter);
        }

        if !app.world().contains_resource::<ScriptSandboxConfig>() {
            app.init_resource::<ScriptSandboxConfig>();
        }
        let sandbox = app.world().resource::<ScriptSandboxConfig>().clone();

        if !app.world().contains_resource::<ScriptExecutionOptions>() {
            app.init_resource::<ScriptExecutionOptions>();
        }
        if !app.world().contains_resource::<ScriptRuntime>() {
            app.insert_resource(ScriptRuntime::with_sandbox(sandbox));
        }
        if !app.world().contains_resource::<ScriptDiagnostics>() {
            app.init_resource::<ScriptDiagnostics>();
        }
        if !app.world().contains_resource::<ScriptHotReloadConfig>() {
            app.init_resource::<ScriptHotReloadConfig>();
        }
        if !app.world().contains_resource::<ScriptFileTracker>() {
            app.init_resource::<ScriptFileTracker>();
        }
        if !app.world().contains_resource::<PendingScriptContacts>() {
            app.init_resource::<PendingScriptContacts>();
        }
        if !app.world().contains_resource::<PendingScriptImpulses>() {
            app.init_resource::<PendingScriptImpulses>();
        }
        if !app.world().contains_resource::<ActiveScriptVoices>() {
            app.init_resource::<ActiveScriptVoices>();
        }
        if !app.world().contains_resource::<ScriptHotReloadWatcher>() {
            let mut watcher = ScriptHotReloadWatcher::new();
            if let Some(paths) = app.world().get_resource::<ProjectPaths>() {
                let scripts_dir = paths.scripts_dir.clone();
                let assets_dir = paths.assets_dir.clone();
                watcher.watch_dir(&scripts_dir);
                watcher.watch_dir(&assets_dir);
            } else if let Some(registry) = app.world().get_resource::<AssetRegistry>() {
                let assets_dir = registry.shared().assets_dir();
                watcher.watch_dir(&assets_dir);
            }
            app.insert_resource(watcher);
        }

        app.add_observer(record_collision_event)
            .add_observer(record_trigger_event)
            .add_systems(
                Update,
                (poll_script_hot_reload_system, run_update_scripts_system)
                    .chain()
                    .in_set(EngineSet::Gameplay),
            )
            .add_systems(
                FixedUpdate,
                run_fixed_update_scripts_system.in_set(EngineFixedSet::ScriptFixed),
            )
            .add_systems(
                FixedUpdate,
                dispatch_physics_script_hooks_system.in_set(EngineFixedSet::Gameplay),
            );
    }
}

fn record_collision_event(
    event: On<CollisionEvent>,
    options: Option<Res<ScriptExecutionOptions>>,
    mut pending: ResMut<PendingScriptContacts>,
) {
    if options.is_some_and(|opts| !opts.enabled) {
        return;
    }
    pending.collisions.push(*event.event());
}

fn record_trigger_event(
    event: On<TriggerEvent>,
    options: Option<Res<ScriptExecutionOptions>>,
    mut pending: ResMut<PendingScriptContacts>,
) {
    if options.is_some_and(|opts| !opts.enabled) {
        return;
    }
    pending.triggers.push(*event.event());
}

fn handle_pending_reset_and_sandbox(world: &mut World, runtime: &mut ScriptRuntime) -> bool {
    let options = world
        .get_resource::<ScriptExecutionOptions>()
        .copied()
        .unwrap_or_default();

    if options.reset_requested {
        runtime.reset_runtime_state();
        reset_world_runtime_buffers(world);
    }

    if let Some(sandbox) = world.get_resource::<ScriptSandboxConfig>()
        && *sandbox != runtime.applied_sandbox
    {
        runtime.set_sandbox_config(sandbox.clone());
    }

    options.enabled
}

fn collect_script_entities(world: &World) -> Vec<Entity> {
    let mut entities: Vec<Entity> = world
        .archetypes()
        .iter()
        .flat_map(|arch| arch.entities())
        .map(|ae| ae.id())
        .filter(|&entity| {
            world
                .get::<ScriptComponent>(entity)
                .is_some_and(|comp| comp.enabled && !comp.is_unassigned())
        })
        .collect();
    entities.sort_unstable();
    entities
}

fn entity_diagnostic_context(world: &World, entity: Entity) -> DiagnosticEntityContext {
    DiagnosticEntityContext {
        entity: Some(entity),
        entity_name: world.get::<Name>(entity).map(|n| n.as_str().to_owned()),
        entity_guid: world.get::<EntityGuid>(entity).copied(),
    }
}

fn invoke_entity_hook(
    world: &mut World,
    runtime: &mut ScriptRuntime,
    entity: Entity,
    hook: ScriptHook,
    args: ScriptHookArgs,
) {
    let Some(component) = world.get::<ScriptComponent>(entity).cloned() else {
        return;
    };
    if !component.enabled || component.is_unassigned() {
        return;
    }

    let entity_ctx = entity_diagnostic_context(world, entity);
    let Some(resolved) = resolve_script_source(world, runtime.backend.as_ref(), &component) else {
        return;
    };

    if !ensure_script_compiled(world, runtime.backend.as_mut(), &resolved, &entity_ctx) {
        return;
    }

    runtime.active_entities.insert(entity);

    // Ensure `on_start()` executes once before any other lifecycle hook.
    if hook != ScriptHook::OnStart && !runtime.backend.entity_started(entity) {
        let started_ok = execute_single_hook(
            world,
            runtime,
            entity,
            &resolved.key,
            &resolved.source_label,
            ScriptHook::OnStart,
            ScriptHookArgs::None,
            &entity_ctx,
        );
        if !started_ok || world.get_entity(entity).is_err() {
            return;
        }
    }

    let _ = execute_single_hook(
        world,
        runtime,
        entity,
        &resolved.key,
        &resolved.source_label,
        hook,
        args,
        &entity_ctx,
    );
}

#[allow(clippy::too_many_arguments)]
fn execute_single_hook(
    world: &mut World,
    runtime: &mut ScriptRuntime,
    entity: Entity,
    key: &ScriptSourceKey,
    source_label: &str,
    hook: ScriptHook,
    args: ScriptHookArgs,
    entity_ctx: &DiagnosticEntityContext,
) -> bool {
    let Some(mut properties) = world
        .get::<ScriptComponent>(entity)
        .map(|comp| comp.properties.clone())
    else {
        return false;
    };

    let call_result = {
        let _guard =
            runtime
                .call_context
                .enter(world, &mut runtime.scratch_world, entity, hook.is_fixed_step());
        runtime.backend.invoke_hook(
            entity,
            key,
            &mut properties,
            hook,
            args,
            &runtime.call_context,
        )
    };

    match call_result {
        Ok(_ran) => {
            if let Some(mut comp) = world.get_mut::<ScriptComponent>(entity)
                && comp.properties != properties
            {
                comp.properties = properties;
            }
            let commands = runtime.call_context.take_commands();
            flush_script_commands(world, commands, source_label, entity_ctx, hook.fn_name());
            true
        }
        Err(err) => {
            runtime.call_context.discard_commands();
            let (file, line, column, stage, message) = match err {
                ScriptError::Compile {
                    file,
                    line,
                    column,
                    message,
                } => (file, line, column, ScriptDiagnosticStage::Compile, message),
                ScriptError::Runtime {
                    file,
                    line,
                    column,
                    message,
                    ..
                } => (file, line, column, ScriptDiagnosticStage::Runtime, message),
                ScriptError::Io { path, message } => {
                    (path, None, None, ScriptDiagnosticStage::Load, message)
                }
                ScriptError::NotFound(missing) => (
                    source_label.to_owned(),
                    None,
                    None,
                    ScriptDiagnosticStage::Load,
                    format!("script not found: {missing}"),
                ),
            };

            let diag = world.resource_mut::<ScriptDiagnostics>().record(
                ScriptDiagnosticSeverity::Error,
                stage,
                file,
                line,
                column,
                entity_ctx,
                Some(hook.fn_name().to_owned()),
                message,
            );
            world.trigger(ScriptDiagnosticEvent(diag));
            false
        }
    }
}

/// Exclusive `Update` system running `on_start()` (if needed) and `on_update(dt)` for active scripts.
pub fn run_update_scripts_system(world: &mut World) {
    let Some(mut runtime) = world.remove_resource::<ScriptRuntime>() else {
        return;
    };

    if handle_pending_reset_and_sandbox(world, &mut runtime) {
        runtime.prune_stale_entities(world);
        let dt = world
            .get_resource::<Time>()
            .map_or(1.0 / 60.0, Time::delta_secs_f64);
        let entities = collect_script_entities(world);
        for entity in entities {
            if world.get_entity(entity).is_ok() {
                invoke_entity_hook(
                    world,
                    &mut runtime,
                    entity,
                    ScriptHook::OnUpdate,
                    ScriptHookArgs::Delta(dt),
                );
            }
        }
    }

    world.insert_resource(runtime);
}

/// Exclusive `FixedUpdate` system running `on_start()` (if needed) and `on_fixed_update(dt)`
/// prior to physics stepping.
pub fn run_fixed_update_scripts_system(world: &mut World) {
    let Some(mut runtime) = world.remove_resource::<ScriptRuntime>() else {
        return;
    };

    if handle_pending_reset_and_sandbox(world, &mut runtime) {
        runtime.prune_stale_entities(world);
        let dt = world
            .get_resource::<Time<Fixed>>()
            .map_or(1.0 / 60.0, Time::delta_secs_f64);
        let entities = collect_script_entities(world);
        for entity in entities {
            if world.get_entity(entity).is_ok() {
                invoke_entity_hook(
                    world,
                    &mut runtime,
                    entity,
                    ScriptHook::OnFixedUpdate,
                    ScriptHookArgs::Delta(dt),
                );
            }
        }
    }

    world.insert_resource(runtime);
}

/// Exclusive `FixedUpdate` post-physics system flushing deferred impulses and dispatching
/// `on_collision`, `on_trigger_enter`, and `on_trigger_exit` hooks from buffered `engine_physics`
/// observer events.
pub fn dispatch_physics_script_hooks_system(world: &mut World) {
    let Some(mut runtime) = world.remove_resource::<ScriptRuntime>() else {
        return;
    };

    if handle_pending_reset_and_sandbox(world, &mut runtime) {
        flush_pending_impulses(world);

        let (collisions, triggers) = world
            .get_resource_mut::<PendingScriptContacts>()
            .map(|mut pending| {
                (
                    std::mem::take(&mut pending.collisions),
                    std::mem::take(&mut pending.triggers),
                )
            })
            .unwrap_or_default();

        for event in collisions {
            if event.phase != ContactPhase::Started {
                continue;
            }
            let [a, b] = event.entities;
            dispatch_pair_hook(
                world,
                &mut runtime,
                a,
                b,
                ScriptHook::OnCollision,
                event.dimension,
                event.phase,
            );
            dispatch_pair_hook(
                world,
                &mut runtime,
                b,
                a,
                ScriptHook::OnCollision,
                event.dimension,
                event.phase,
            );
        }

        for event in triggers {
            let hook = match event.phase {
                ContactPhase::Started => ScriptHook::OnTriggerEnter,
                ContactPhase::Stopped => ScriptHook::OnTriggerExit,
            };
            let [a, b] = event.entities;
            dispatch_pair_hook(
                world,
                &mut runtime,
                a,
                b,
                hook,
                event.dimension,
                event.phase,
            );
            dispatch_pair_hook(
                world,
                &mut runtime,
                b,
                a,
                hook,
                event.dimension,
                event.phase,
            );
        }
    }

    world.insert_resource(runtime);
}

fn dispatch_pair_hook(
    world: &mut World,
    runtime: &mut ScriptRuntime,
    target: Entity,
    other: Entity,
    hook: ScriptHook,
    dimension: engine_physics::PhysicsDimension,
    phase: ContactPhase,
) {
    if world.get_entity(target).is_err() {
        return;
    }
    let has_enabled_script = world
        .get::<ScriptComponent>(target)
        .is_some_and(|comp| comp.enabled && !comp.is_unassigned());
    if !has_enabled_script {
        return;
    }
    let contact = ScriptContactInfo {
        other_entity: other,
        other_name: world.get::<Name>(other).map(|n| n.as_str().to_owned()),
        other_guid: world.get::<EntityGuid>(other).copied(),
        dimension,
        phase,
    };
    invoke_entity_hook(
        world,
        runtime,
        target,
        hook,
        ScriptHookArgs::Contact(contact),
    );
}
