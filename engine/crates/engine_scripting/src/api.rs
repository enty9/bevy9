//! Gameplay API exposed to Rhai scripts, deferred [`ScriptCommand`] queue, and [`ScriptCustomEvent`].

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use bevy_ecs::{entity::Entity, event::Event, name::Name, resource::Resource, world::World};
use bevy_math::{EulerRot, Quat, Vec2, Vec3};
use bevy_transform::components::{GlobalTransform, Transform};
use engine_assets::AssetRegistry;
use engine_audio::{
    resolve_clip, AudioBus, AudioEmitter, AudioEngine, AudioSpace, PlaySound, SpatialSound, VoiceId,
};
use engine_core::{AssetUuid, EntityGuid, EntityGuidMap, ProjectPaths};
use engine_input::{ActionSample, ActionState, FixedActionState};
use engine_physics::{
    BodyKind, CastFilter, CharacterMotion2D, CharacterMotion3D, ContactPhase, PhysicsDimension,
    PhysicsWorld2D, PhysicsWorld3D, RigidBody2D, RigidBody3D,
};
use engine_scene::spawn_prefab_from_file;
use rhai::{Array, Dynamic, Engine, EvalAltResult, ImmutableString, Map, Position};
use serde_json::Value;

use crate::{
    backend::ScriptContactInfo,
    diagnostics::{
        DiagnosticEntityContext, ScriptDiagnosticEvent, ScriptDiagnosticSeverity,
        ScriptDiagnosticStage, ScriptDiagnostics,
    },
    reflect_bridge::{
        dynamic_to_display_string, dynamic_to_f32, dynamic_to_json, dynamic_to_quat,
        dynamic_to_vec3, has_reflected_component, quat_to_dynamic, read_component_dynamic,
        read_component_field_dynamic, validate_component_field_target, write_component_dynamic,
        write_component_field_dynamic,
    },
};

/// Custom gameplay event emitted from Rhai via `events.emit(name, payload)`.
#[derive(Event, Debug, Clone, PartialEq)]
pub struct ScriptCustomEvent {
    /// Entity whose script emitted the event.
    pub source_entity: Entity,
    /// Optional `Name` of the source entity.
    pub source_name: Option<String>,
    /// Optional [`EntityGuid`] of the source entity.
    pub source_guid: Option<EntityGuid>,
    /// Event name string.
    pub name: String,
    /// Arbitrary JSON payload converted from Rhai.
    pub payload: Value,
}

/// Deferred ECS mutation enqueued by a script hook and flushed safely against [`World`].
#[derive(Debug, Clone)]
pub enum ScriptCommand {
    /// Updates `Transform` on `entity`.
    SetTransform {
        /// Target entity.
        entity: Entity,
        /// Replacement local transform.
        transform: Transform,
    },
    /// Inserts or replaces a reflected component on `entity`.
    SetComponent {
        /// Target entity.
        entity: Entity,
        /// Full or short component type path.
        type_path: String,
        /// Rhai value converted during flush.
        value: Dynamic,
    },
    /// Mutates a single reflected field on `entity`.
    SetComponentField {
        /// Target entity.
        entity: Entity,
        /// Full or short component type path.
        type_path: String,
        /// Dot-separated field path.
        field: String,
        /// New Rhai field value.
        value: Dynamic,
    },
    /// Applies a 3D physics impulse to a dynamic body.
    ApplyImpulse3D {
        /// Target entity.
        entity: Entity,
        /// World-space impulse vector.
        impulse: Vec3,
    },
    /// Applies a 2D physics impulse to a dynamic body.
    ApplyImpulse2D {
        /// Target entity.
        entity: Entity,
        /// World-space impulse vector.
        impulse: Vec2,
    },
    /// Sets desired velocity on a 3D character controller.
    MoveCharacter3D {
        /// Target entity.
        entity: Entity,
        /// Desired linear velocity.
        velocity: Vec3,
    },
    /// Sets desired velocity on a 2D character controller.
    MoveCharacter2D {
        /// Target entity.
        entity: Entity,
        /// Desired linear velocity.
        velocity: Vec2,
    },
    /// Plays an audio clip addressed by UUID or path.
    PlayAudioClip {
        /// Originating entity.
        entity: Entity,
        /// Asset UUID string or relative/absolute audio clip path.
        clip: String,
        /// Mixer bus route.
        bus: AudioBus,
        /// Linear gain multiplier.
        gain: f32,
        /// Pitch multiplier.
        pitch: f32,
        /// Repeat playback.
        looping: bool,
        /// Stream from disk rather than predecoding.
        streaming: bool,
    },
    /// Plays the authored [`AudioEmitter`] attached to `entity`.
    PlayAudioEmitter {
        /// Entity carrying [`AudioEmitter`].
        entity: Entity,
    },
    /// Instantiates a `.prefab.ron` file at `position`.
    SpawnPrefab {
        /// Prefab UUID string or relative/absolute `.prefab.ron` path.
        path: String,
        /// World spawn translation.
        position: Vec3,
        /// Optional parent entity.
        parent: Option<Entity>,
    },
    /// Despawns `entity` if still alive.
    DespawnEntity(Entity),
    /// Emits a [`ScriptCustomEvent`] observer trigger.
    EmitCustomEvent(ScriptCustomEvent),
}

/// Shared interior state backing Rhai API handle objects during a single script hook invocation.
#[derive(Default)]
pub(crate) struct ScriptCallState {
    pub(crate) world: Option<World>,
    pub(crate) active_entity: Option<Entity>,
    pub(crate) active_entity_name: Option<String>,
    pub(crate) active_entity_guid: Option<EntityGuid>,
    pub(crate) fixed_step: bool,
    pub(crate) staged_transform: Option<Transform>,
    pub(crate) staged_fields: HashMap<(Entity, String, String), Dynamic>,
    pub(crate) commands: Vec<ScriptCommand>,
}

/// Thread-safe handle shared between a script backend and its active hook execution frame.
#[derive(Clone, Default)]
pub struct ScriptCallContext {
    pub(crate) inner: Arc<Mutex<ScriptCallState>>,
}

impl ScriptCallContext {
    /// Creates a new empty [`ScriptCallContext`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Temporarily binds `world` and execution metadata for a single entity hook, restoring
    /// `world` unconditionally when the returned [`WorldSwapGuard`] drops.
    pub(crate) fn enter<'w>(
        &self,
        world: &'w mut World,
        scratch_world: &'w mut Option<World>,
        entity: Entity,
        fixed_step: bool,
    ) -> WorldSwapGuard<'w> {
        let active_entity_name = world.get::<Name>(entity).map(|n| n.as_str().to_owned());
        let active_entity_guid = world.get::<EntityGuid>(entity).copied();
        let staged_transform = world.get::<Transform>(entity).copied();

        let mut moved_world = scratch_world.take().unwrap_or_default();
        std::mem::swap(world, &mut moved_world);

        {
            let mut state = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.world = Some(moved_world);
            state.active_entity = Some(entity);
            state.active_entity_name = active_entity_name;
            state.active_entity_guid = active_entity_guid;
            state.fixed_step = fixed_step;
            state.staged_transform = staged_transform;
            state.staged_fields.clear();
            state.commands.clear();
        }

        WorldSwapGuard {
            world_ref: world,
            scratch_ref: scratch_world,
            inner: self.inner.clone(),
        }
    }

    /// Takes all commands queued during the most recent hook invocation.
    pub(crate) fn take_commands(&self) -> Vec<ScriptCommand> {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut state.commands)
    }

    /// Discards any commands queued during a failed hook invocation.
    pub(crate) fn discard_commands(&self) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.commands.clear();
    }
}

/// RAII guard restoring [`World`] back to the Bevy exclusive system even on panic or error.
pub(crate) struct WorldSwapGuard<'w> {
    world_ref: &'w mut World,
    scratch_ref: &'w mut Option<World>,
    inner: Arc<Mutex<ScriptCallState>>,
}

impl Drop for WorldSwapGuard<'_> {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(mut original_world) = state.world.take() {
            std::mem::swap(self.world_ref, &mut original_world);
            *self.scratch_ref = Some(original_world);
        }
        state.active_entity = None;
        state.active_entity_name = None;
        state.active_entity_guid = None;
        state.staged_transform = None;
        state.staged_fields.clear();
    }
}

fn rhai_err(msg: impl Into<String>) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(
        Dynamic::from(msg.into()),
        Position::NONE,
    ))
}

fn parse_entity_bits(bits: i64) -> Result<Entity, Box<EvalAltResult>> {
    Entity::try_from_bits(bits as u64)
        .ok_or_else(|| rhai_err(format!("invalid entity bits `{bits}`")))
}

fn read_action_sample(world: &World, action: &str, fixed_step: bool) -> ActionSample {
    if fixed_step
        && let Some(fixed) = world.get_resource::<FixedActionState>()
    {
        return fixed.get(action);
    }
    world
        .get_resource::<ActionState>()
        .map(|state| state.get(action))
        .unwrap_or_default()
}

/// Converts a [`ScriptContactInfo`] into a Rhai `Map` passed to collision/trigger hooks.
pub fn contact_info_to_dynamic(contact: &ScriptContactInfo) -> Dynamic {
    let mut map = Map::new();
    let bits = contact.other_entity.to_bits() as i64;
    map.insert("entity".into(), Dynamic::from(bits));
    map.insert("entity_bits".into(), Dynamic::from(bits));
    map.insert("id".into(), Dynamic::from(bits));
    map.insert(
        "name".into(),
        Dynamic::from(contact.other_name.clone().unwrap_or_default()),
    );
    map.insert(
        "guid".into(),
        Dynamic::from(
            contact
                .other_guid
                .map(|g| g.to_string())
                .unwrap_or_default(),
        ),
    );
    map.insert(
        "dimension".into(),
        Dynamic::from(match contact.dimension {
            PhysicsDimension::D2 => "2d",
            PhysicsDimension::D3 => "3d",
        }),
    );
    map.insert(
        "phase".into(),
        Dynamic::from(match contact.phase {
            ContactPhase::Started => "started",
            ContactPhase::Stopped => "stopped",
        }),
    );
    Dynamic::from_map(map)
}

/// Rhai handle exposing the active entity's `Transform` (`transform` variable in scripts).
#[derive(Clone)]
pub struct ScriptTransformHandle(pub(crate) Arc<Mutex<ScriptCallState>>);

/// Rhai handle exposing entity identity and reflected component access (`entity` variable in scripts).
#[derive(Clone)]
pub struct ScriptEntityHandle(pub(crate) Arc<Mutex<ScriptCallState>>);

/// Rhai handle exposing `engine_input` actions (`input` variable in scripts).
#[derive(Clone)]
pub struct ScriptInputHandle(pub(crate) Arc<Mutex<ScriptCallState>>);

/// Rhai handle exposing `engine_physics` raycasts, impulses, and character controllers (`physics` variable in scripts).
#[derive(Clone)]
pub struct ScriptPhysicsHandle(pub(crate) Arc<Mutex<ScriptCallState>>);

/// Rhai handle exposing `engine_audio` playback (`audio` variable in scripts).
#[derive(Clone)]
pub struct ScriptAudioHandle(pub(crate) Arc<Mutex<ScriptCallState>>);

/// Rhai handle exposing world queries, prefab spawning, and despawning (`world` variable in scripts).
#[derive(Clone)]
pub struct ScriptWorldHandle(pub(crate) Arc<Mutex<ScriptCallState>>);

/// Rhai handle exposing custom event emission and logging (`events` variable in scripts).
#[derive(Clone)]
pub struct ScriptEventsHandle(pub(crate) Arc<Mutex<ScriptCallState>>);

fn with_transform_mut(
    state_arc: &Arc<Mutex<ScriptCallState>>,
    f: impl FnOnce(&mut Transform),
) -> Result<(), Box<EvalAltResult>> {
    let mut state = state_arc
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entity = state
        .active_entity
        .ok_or_else(|| rhai_err("no active script entity"))?;
    let mut tfm = state.staged_transform.unwrap_or(Transform::IDENTITY);
    f(&mut tfm);
    state.staged_transform = Some(tfm);
    state.commands.push(ScriptCommand::SetTransform {
        entity,
        transform: tfm,
    });
    Ok(())
}

fn read_transform(state_arc: &Arc<Mutex<ScriptCallState>>) -> Transform {
    let state = state_arc
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.staged_transform.unwrap_or(Transform::IDENTITY)
}

fn read_component_for_entity(
    state_arc: &Arc<Mutex<ScriptCallState>>,
    target_entity: Option<Entity>,
    type_path: &str,
) -> Result<Dynamic, Box<EvalAltResult>> {
    let state = state_arc
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entity = target_entity
        .or(state.active_entity)
        .ok_or_else(|| rhai_err("no active script entity"))?;
    if Some(entity) == state.active_entity
        && (type_path == "Transform"
            || type_path == "bevy_transform::components::transform::Transform")
        && let Some(tfm) = state.staged_transform
    {
        let mut map = Map::new();
        map.insert("translation".into(), Dynamic::from(tfm.translation));
        map.insert("rotation".into(), quat_to_dynamic(tfm.rotation));
        map.insert("scale".into(), Dynamic::from(tfm.scale));
        return Ok(Dynamic::from_map(map));
    }
    let world = state
        .world
        .as_ref()
        .ok_or_else(|| rhai_err("world unavailable outside hook execution"))?;
    let result = read_component_dynamic(world, entity, type_path).map_err(rhai_err)?;
    Ok(result.unwrap_or(Dynamic::UNIT))
}

fn read_field_for_entity(
    state_arc: &Arc<Mutex<ScriptCallState>>,
    target_entity: Option<Entity>,
    type_path: &str,
    field: &str,
) -> Result<Dynamic, Box<EvalAltResult>> {
    let state = state_arc
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entity = target_entity
        .or(state.active_entity)
        .ok_or_else(|| rhai_err("no active script entity"))?;

    if Some(entity) == state.active_entity
        && (type_path == "Transform"
            || type_path == "bevy_transform::components::transform::Transform")
        && let Some(tfm) = state.staged_transform
    {
        match field {
            "translation" => return Ok(Dynamic::from(tfm.translation)),
            "translation.x" => return Ok(Dynamic::from(f64::from(tfm.translation.x))),
            "translation.y" => return Ok(Dynamic::from(f64::from(tfm.translation.y))),
            "translation.z" => return Ok(Dynamic::from(f64::from(tfm.translation.z))),
            "scale" => return Ok(Dynamic::from(tfm.scale)),
            "scale.x" => return Ok(Dynamic::from(f64::from(tfm.scale.x))),
            "scale.y" => return Ok(Dynamic::from(f64::from(tfm.scale.y))),
            "scale.z" => return Ok(Dynamic::from(f64::from(tfm.scale.z))),
            "rotation" => return Ok(quat_to_dynamic(tfm.rotation)),
            _ => {}
        }
    }

    let world = state
        .world
        .as_ref()
        .ok_or_else(|| rhai_err("world unavailable outside hook execution"))?;

    if let Ok(canonical) = validate_component_field_target(world, entity, type_path, field)
        && let Some(staged) = state
            .staged_fields
            .get(&(entity, canonical, field.to_owned()))
    {
        return Ok(staged.clone());
    }

    let result = read_component_field_dynamic(world, entity, type_path, field).map_err(rhai_err)?;
    Ok(result.unwrap_or(Dynamic::UNIT))
}

fn queue_set_field_for_entity(
    state_arc: &Arc<Mutex<ScriptCallState>>,
    target_entity: Option<Entity>,
    type_path: &str,
    field: &str,
    value: Dynamic,
) -> Result<(), Box<EvalAltResult>> {
    let mut state = state_arc
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entity = target_entity
        .or(state.active_entity)
        .ok_or_else(|| rhai_err("no active script entity"))?;

    let canonical = {
        let world = state
            .world
            .as_ref()
            .ok_or_else(|| rhai_err("world unavailable outside hook execution"))?;
        validate_component_field_target(world, entity, type_path, field).map_err(rhai_err)?
    };

    // Keep staged_transform coherent when scripts mutate Transform through reflection.
    if Some(entity) == state.active_entity
        && canonical == "bevy_transform::components::transform::Transform"
        && let Some(mut tfm) = state.staged_transform
    {
        match field {
            "translation" => {
                if let Some(v) = dynamic_to_vec3(&value) {
                    tfm.translation = v;
                }
            }
            "translation.x" => {
                if let Some(v) = dynamic_to_f32(&value) {
                    tfm.translation.x = v;
                }
            }
            "translation.y" => {
                if let Some(v) = dynamic_to_f32(&value) {
                    tfm.translation.y = v;
                }
            }
            "translation.z" => {
                if let Some(v) = dynamic_to_f32(&value) {
                    tfm.translation.z = v;
                }
            }
            "scale" => {
                if let Some(v) = dynamic_to_vec3(&value) {
                    tfm.scale = v;
                }
            }
            "rotation" => {
                if let Some(q) = dynamic_to_quat(&value) {
                    tfm.rotation = q;
                }
            }
            _ => {}
        }
        state.staged_transform = Some(tfm);
    }

    state
        .staged_fields
        .insert((entity, canonical.clone(), field.to_owned()), value.clone());
    state.commands.push(ScriptCommand::SetComponentField {
        entity,
        type_path: canonical,
        field: field.to_owned(),
        value,
    });
    Ok(())
}

fn queue_set_component_for_entity(
    state_arc: &Arc<Mutex<ScriptCallState>>,
    target_entity: Option<Entity>,
    type_path: &str,
    value: Dynamic,
) -> Result<(), Box<EvalAltResult>> {
    let mut state = state_arc
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entity = target_entity
        .or(state.active_entity)
        .ok_or_else(|| rhai_err("no active script entity"))?;

    // Validate the component type exists right away so typos fail at the call site.
    let canonical = {
        let world = state
            .world
            .as_ref()
            .ok_or_else(|| rhai_err("world unavailable outside hook execution"))?;
        let Some(registry) = world.get_resource::<bevy_ecs::reflect::AppTypeRegistry>() else {
            return Err(rhai_err("AppTypeRegistry resource is missing"));
        };
        let guard = registry.read();
        let (reg, _) = crate::reflect_bridge::resolve_component_registration(&guard, type_path)
            .map_err(rhai_err)?;
        reg.type_info().type_path().to_owned()
    };

    state.commands.push(ScriptCommand::SetComponent {
        entity,
        type_path: canonical,
        value,
    });
    Ok(())
}

fn parse_audio_bus(name: &str) -> AudioBus {
    match name.to_ascii_lowercase().as_str() {
        "master" => AudioBus::Master,
        "music" => AudioBus::Music,
        "ui" => AudioBus::Ui,
        "voice" => AudioBus::Voice,
        _ => AudioBus::Sfx,
    }
}

fn do_raycast_3d(
    state_arc: &Arc<Mutex<ScriptCallState>>,
    origin: Vec3,
    direction: Vec3,
    max_distance: f32,
    exclude_self: bool,
    include_sensors: bool,
) -> Result<Dynamic, Box<EvalAltResult>> {
    let state = state_arc
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = state
        .world
        .as_ref()
        .ok_or_else(|| rhai_err("world unavailable outside hook execution"))?;
    let Some(physics) = world.get_resource::<PhysicsWorld3D>() else {
        return Ok(Dynamic::UNIT);
    };
    let filter = CastFilter {
        exclude: exclude_self.then_some(state.active_entity).flatten(),
        include_sensors,
        ..Default::default()
    };
    let Some(hit) = physics.raycast(origin, direction, max_distance, filter) else {
        return Ok(Dynamic::UNIT);
    };
    let dir_norm = direction.try_normalize().unwrap_or(Vec3::ZERO);
    let point = origin + dir_norm * hit.distance;
    let name = world
        .get::<Name>(hit.entity)
        .map(|n| n.as_str().to_owned())
        .unwrap_or_default();
    let guid = world
        .get::<EntityGuid>(hit.entity)
        .map(|g| g.to_string())
        .unwrap_or_default();

    let mut map = Map::new();
    let bits = hit.entity.to_bits() as i64;
    map.insert("hit".into(), Dynamic::from(true));
    map.insert("entity".into(), Dynamic::from(bits));
    map.insert("entity_bits".into(), Dynamic::from(bits));
    map.insert("name".into(), Dynamic::from(name));
    map.insert("guid".into(), Dynamic::from(guid));
    map.insert("distance".into(), Dynamic::from(f64::from(hit.distance)));
    map.insert("point".into(), Dynamic::from(point));
    map.insert("normal".into(), Dynamic::from(hit.normal));
    Ok(Dynamic::from_map(map))
}

fn do_raycast_2d(
    state_arc: &Arc<Mutex<ScriptCallState>>,
    origin: Vec2,
    direction: Vec2,
    max_distance: f32,
    exclude_self: bool,
    include_sensors: bool,
) -> Result<Dynamic, Box<EvalAltResult>> {
    let state = state_arc
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = state
        .world
        .as_ref()
        .ok_or_else(|| rhai_err("world unavailable outside hook execution"))?;
    let Some(physics) = world.get_resource::<PhysicsWorld2D>() else {
        return Ok(Dynamic::UNIT);
    };
    let filter = CastFilter {
        exclude: exclude_self.then_some(state.active_entity).flatten(),
        include_sensors,
        ..Default::default()
    };
    let Some(hit) = physics.raycast(origin, direction, max_distance, filter) else {
        return Ok(Dynamic::UNIT);
    };
    let dir_norm = direction.try_normalize().unwrap_or(Vec2::ZERO);
    let point = origin + dir_norm * hit.distance;
    let name = world
        .get::<Name>(hit.entity)
        .map(|n| n.as_str().to_owned())
        .unwrap_or_default();
    let guid = world
        .get::<EntityGuid>(hit.entity)
        .map(|g| g.to_string())
        .unwrap_or_default();

    let mut map = Map::new();
    let bits = hit.entity.to_bits() as i64;
    map.insert("hit".into(), Dynamic::from(true));
    map.insert("entity".into(), Dynamic::from(bits));
    map.insert("entity_bits".into(), Dynamic::from(bits));
    map.insert("name".into(), Dynamic::from(name));
    map.insert("guid".into(), Dynamic::from(guid));
    map.insert("distance".into(), Dynamic::from(f64::from(hit.distance)));
    map.insert("point".into(), Dynamic::from(point));
    map.insert("normal".into(), Dynamic::from(hit.normal));
    Ok(Dynamic::from_map(map))
}

/// Registers math types, gameplay API handle types, and top-level helper functions on `engine`.
pub(crate) fn register_script_api(engine: &mut Engine, context: &ScriptCallContext) {
    register_math_api(engine);
    register_transform_api(engine);
    register_entity_api(engine);
    register_input_api(engine);
    register_physics_api(engine);
    register_audio_api(engine);
    register_world_api(engine);
    register_events_api(engine);
    register_global_helpers(engine, &context.inner);

    engine.on_print(|text| {
        tracing::info!(target: "engine_scripting", "{text}");
    });
    engine.on_debug(|text, source, pos| {
        tracing::debug!(
            target: "engine_scripting",
            source = source.unwrap_or("<script>"),
            position = %pos,
            "{text}"
        );
    });
}

fn register_math_api(engine: &mut Engine) {
    // Vec2
    engine.register_type_with_name::<Vec2>("Vec2");
    engine.register_fn("vec2", |x: Dynamic, y: Dynamic| -> Result<Vec2, Box<EvalAltResult>> {
        let x = dynamic_to_f32(&x).ok_or_else(|| rhai_err("vec2 x must be a number"))?;
        let y = dynamic_to_f32(&y).ok_or_else(|| rhai_err("vec2 y must be a number"))?;
        Ok(Vec2::new(x, y))
    });
    engine.register_get("x", |v: &mut Vec2| f64::from(v.x));
    engine.register_set(
        "x",
        |v: &mut Vec2, val: Dynamic| -> Result<(), Box<EvalAltResult>> {
            v.x = dynamic_to_f32(&val).ok_or_else(|| rhai_err("Vec2.x must be a number"))?;
            Ok(())
        },
    );
    engine.register_get("y", |v: &mut Vec2| f64::from(v.y));
    engine.register_set(
        "y",
        |v: &mut Vec2, val: Dynamic| -> Result<(), Box<EvalAltResult>> {
            v.y = dynamic_to_f32(&val).ok_or_else(|| rhai_err("Vec2.y must be a number"))?;
            Ok(())
        },
    );
    engine.register_fn("+", |a: Vec2, b: Vec2| a + b);
    engine.register_fn("-", |a: Vec2, b: Vec2| a - b);
    engine.register_fn("-", |a: Vec2| -a);
    engine.register_fn("*", |a: Vec2, s: f64| a * (s as f32));
    engine.register_fn("*", |s: f64, a: Vec2| a * (s as f32));
    engine.register_fn("*", |a: Vec2, s: i64| a * (s as f32));
    engine.register_fn("*", |s: i64, a: Vec2| a * (s as f32));
    engine.register_fn("/", |a: Vec2, s: f64| a / (s as f32));
    engine.register_fn("==", |a: Vec2, b: Vec2| a == b);
    engine.register_fn("!=", |a: Vec2, b: Vec2| a != b);
    engine.register_fn("length", |v: &mut Vec2| f64::from(v.length()));
    engine.register_fn("length_squared", |v: &mut Vec2| f64::from(v.length_squared()));
    engine.register_fn("normalize", |v: &mut Vec2| v.normalize_or_zero());
    engine.register_fn("dot", |a: &mut Vec2, b: Vec2| f64::from(a.dot(b)));
    engine.register_fn("distance", |a: &mut Vec2, b: Vec2| f64::from(a.distance(b)));
    engine.register_fn("to_string", |v: &mut Vec2| format!("Vec2({}, {})", v.x, v.y));

    // Vec3
    engine.register_type_with_name::<Vec3>("Vec3");
    engine.register_fn(
        "vec3",
        |x: Dynamic, y: Dynamic, z: Dynamic| -> Result<Vec3, Box<EvalAltResult>> {
            let x = dynamic_to_f32(&x).ok_or_else(|| rhai_err("vec3 x must be a number"))?;
            let y = dynamic_to_f32(&y).ok_or_else(|| rhai_err("vec3 y must be a number"))?;
            let z = dynamic_to_f32(&z).ok_or_else(|| rhai_err("vec3 z must be a number"))?;
            Ok(Vec3::new(x, y, z))
        },
    );
    engine.register_get("x", |v: &mut Vec3| f64::from(v.x));
    engine.register_set(
        "x",
        |v: &mut Vec3, val: Dynamic| -> Result<(), Box<EvalAltResult>> {
            v.x = dynamic_to_f32(&val).ok_or_else(|| rhai_err("Vec3.x must be a number"))?;
            Ok(())
        },
    );
    engine.register_get("y", |v: &mut Vec3| f64::from(v.y));
    engine.register_set(
        "y",
        |v: &mut Vec3, val: Dynamic| -> Result<(), Box<EvalAltResult>> {
            v.y = dynamic_to_f32(&val).ok_or_else(|| rhai_err("Vec3.y must be a number"))?;
            Ok(())
        },
    );
    engine.register_get("z", |v: &mut Vec3| f64::from(v.z));
    engine.register_set(
        "z",
        |v: &mut Vec3, val: Dynamic| -> Result<(), Box<EvalAltResult>> {
            v.z = dynamic_to_f32(&val).ok_or_else(|| rhai_err("Vec3.z must be a number"))?;
            Ok(())
        },
    );
    engine.register_fn("+", |a: Vec3, b: Vec3| a + b);
    engine.register_fn("-", |a: Vec3, b: Vec3| a - b);
    engine.register_fn("-", |a: Vec3| -a);
    engine.register_fn("*", |a: Vec3, s: f64| a * (s as f32));
    engine.register_fn("*", |s: f64, a: Vec3| a * (s as f32));
    engine.register_fn("*", |a: Vec3, s: i64| a * (s as f32));
    engine.register_fn("*", |s: i64, a: Vec3| a * (s as f32));
    engine.register_fn("/", |a: Vec3, s: f64| a / (s as f32));
    engine.register_fn("==", |a: Vec3, b: Vec3| a == b);
    engine.register_fn("!=", |a: Vec3, b: Vec3| a != b);
    engine.register_fn("length", |v: &mut Vec3| f64::from(v.length()));
    engine.register_fn("length_squared", |v: &mut Vec3| f64::from(v.length_squared()));
    engine.register_fn("normalize", |v: &mut Vec3| v.normalize_or_zero());
    engine.register_fn("dot", |a: &mut Vec3, b: Vec3| f64::from(a.dot(b)));
    engine.register_fn("cross", |a: &mut Vec3, b: Vec3| a.cross(b));
    engine.register_fn("distance", |a: &mut Vec3, b: Vec3| f64::from(a.distance(b)));
    engine.register_fn("to_string", |v: &mut Vec3| {
        format!("Vec3({}, {}, {})", v.x, v.y, v.z)
    });
}

fn register_transform_api(engine: &mut Engine) {
    engine.register_type_with_name::<ScriptTransformHandle>("TransformApi");

    engine.register_get("translation", |h: &mut ScriptTransformHandle| {
        read_transform(&h.0).translation
    });
    engine.register_set(
        "translation",
        |h: &mut ScriptTransformHandle, val: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let v = dynamic_to_vec3(&val)
                .ok_or_else(|| rhai_err("transform.translation expects Vec3, #{x,y,z}, or [x,y,z]"))?;
            with_transform_mut(&h.0, |t| t.translation = v)
        },
    );
    engine.register_get("position", |h: &mut ScriptTransformHandle| {
        read_transform(&h.0).translation
    });
    engine.register_set(
        "position",
        |h: &mut ScriptTransformHandle, val: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let v = dynamic_to_vec3(&val)
                .ok_or_else(|| rhai_err("transform.position expects Vec3, #{x,y,z}, or [x,y,z]"))?;
            with_transform_mut(&h.0, |t| t.translation = v)
        },
    );
    engine.register_get("scale", |h: &mut ScriptTransformHandle| {
        read_transform(&h.0).scale
    });
    engine.register_set(
        "scale",
        |h: &mut ScriptTransformHandle, val: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let v = dynamic_to_vec3(&val)
                .ok_or_else(|| rhai_err("transform.scale expects Vec3, #{x,y,z}, or [x,y,z]"))?;
            with_transform_mut(&h.0, |t| t.scale = v)
        },
    );
    engine.register_get("rotation", |h: &mut ScriptTransformHandle| {
        quat_to_dynamic(read_transform(&h.0).rotation)
    });
    engine.register_set(
        "rotation",
        |h: &mut ScriptTransformHandle, val: Dynamic| -> Result<(), Box<EvalAltResult>> {
            if let Some(q) = dynamic_to_quat(&val) {
                return with_transform_mut(&h.0, |t| t.rotation = q);
            }
            if let Some(deg) = dynamic_to_vec3(&val) {
                let q = Quat::from_euler(
                    EulerRot::XYZ,
                    deg.x.to_radians(),
                    deg.y.to_radians(),
                    deg.z.to_radians(),
                );
                return with_transform_mut(&h.0, |t| t.rotation = q);
            }
            Err(rhai_err(
                "transform.rotation expects quaternion #{x,y,z,w} or Euler degrees Vec3",
            ))
        },
    );
    engine.register_get("rotation_euler_deg", |h: &mut ScriptTransformHandle| {
        let (x, y, z) = read_transform(&h.0).rotation.to_euler(EulerRot::XYZ);
        Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees())
    });
    engine.register_set(
        "rotation_euler_deg",
        |h: &mut ScriptTransformHandle, val: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let deg = dynamic_to_vec3(&val)
                .ok_or_else(|| rhai_err("rotation_euler_deg expects Vec3"))?;
            let q = Quat::from_euler(
                EulerRot::XYZ,
                deg.x.to_radians(),
                deg.y.to_radians(),
                deg.z.to_radians(),
            );
            with_transform_mut(&h.0, |t| t.rotation = q)
        },
    );
    engine.register_get("rotation_euler", |h: &mut ScriptTransformHandle| {
        let (x, y, z) = read_transform(&h.0).rotation.to_euler(EulerRot::XYZ);
        Vec3::new(x, y, z)
    });
    engine.register_set(
        "rotation_euler",
        |h: &mut ScriptTransformHandle, val: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let rad = dynamic_to_vec3(&val).ok_or_else(|| rhai_err("rotation_euler expects Vec3"))?;
            let q = Quat::from_euler(EulerRot::XYZ, rad.x, rad.y, rad.z);
            with_transform_mut(&h.0, |t| t.rotation = q)
        },
    );

    engine.register_fn("get_translation", |h: &mut ScriptTransformHandle| {
        read_transform(&h.0).translation
    });
    engine.register_fn(
        "set_translation",
        |h: &mut ScriptTransformHandle, x: Dynamic, y: Dynamic, z: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let x = dynamic_to_f32(&x).ok_or_else(|| rhai_err("x must be a number"))?;
            let y = dynamic_to_f32(&y).ok_or_else(|| rhai_err("y must be a number"))?;
            let z = dynamic_to_f32(&z).ok_or_else(|| rhai_err("z must be a number"))?;
            with_transform_mut(&h.0, |t| t.translation = Vec3::new(x, y, z))
        },
    );
    engine.register_fn(
        "set_translation",
        |h: &mut ScriptTransformHandle, v: Vec3| -> Result<(), Box<EvalAltResult>> {
            with_transform_mut(&h.0, |t| t.translation = v)
        },
    );
    engine.register_fn(
        "translate",
        |h: &mut ScriptTransformHandle, dx: Dynamic, dy: Dynamic, dz: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let dx = dynamic_to_f32(&dx).ok_or_else(|| rhai_err("dx must be a number"))?;
            let dy = dynamic_to_f32(&dy).ok_or_else(|| rhai_err("dy must be a number"))?;
            let dz = dynamic_to_f32(&dz).ok_or_else(|| rhai_err("dz must be a number"))?;
            with_transform_mut(&h.0, |t| t.translation += Vec3::new(dx, dy, dz))
        },
    );
    engine.register_fn(
        "translate",
        |h: &mut ScriptTransformHandle, delta: Vec3| -> Result<(), Box<EvalAltResult>> {
            with_transform_mut(&h.0, |t| t.translation += delta)
        },
    );
    engine.register_fn(
        "rotate_x",
        |h: &mut ScriptTransformHandle, rad: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let rad = dynamic_to_f32(&rad).ok_or_else(|| rhai_err("angle must be a number"))?;
            with_transform_mut(&h.0, |t| t.rotate_x(rad))
        },
    );
    engine.register_fn(
        "rotate_y",
        |h: &mut ScriptTransformHandle, rad: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let rad = dynamic_to_f32(&rad).ok_or_else(|| rhai_err("angle must be a number"))?;
            with_transform_mut(&h.0, |t| t.rotate_y(rad))
        },
    );
    engine.register_fn(
        "rotate_z",
        |h: &mut ScriptTransformHandle, rad: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let rad = dynamic_to_f32(&rad).ok_or_else(|| rhai_err("angle must be a number"))?;
            with_transform_mut(&h.0, |t| t.rotate_z(rad))
        },
    );
    engine.register_fn(
        "set_rotation_euler_deg",
        |h: &mut ScriptTransformHandle, x: Dynamic, y: Dynamic, z: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let x = dynamic_to_f32(&x).ok_or_else(|| rhai_err("x must be a number"))?;
            let y = dynamic_to_f32(&y).ok_or_else(|| rhai_err("y must be a number"))?;
            let z = dynamic_to_f32(&z).ok_or_else(|| rhai_err("z must be a number"))?;
            let q = Quat::from_euler(EulerRot::XYZ, x.to_radians(), y.to_radians(), z.to_radians());
            with_transform_mut(&h.0, |t| t.rotation = q)
        },
    );
    engine.register_fn(
        "set_rotation_quat",
        |h: &mut ScriptTransformHandle, x: Dynamic, y: Dynamic, z: Dynamic, w: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let x = dynamic_to_f32(&x).ok_or_else(|| rhai_err("x must be a number"))?;
            let y = dynamic_to_f32(&y).ok_or_else(|| rhai_err("y must be a number"))?;
            let z = dynamic_to_f32(&z).ok_or_else(|| rhai_err("z must be a number"))?;
            let w = dynamic_to_f32(&w).ok_or_else(|| rhai_err("w must be a number"))?;
            let q = Quat::from_xyzw(x, y, z, w);
            if !q.is_finite() || q.length_squared() <= 1e-10 {
                return Err(rhai_err("invalid zero or non-finite quaternion"));
            }
            with_transform_mut(&h.0, |t| t.rotation = q.normalize())
        },
    );
    engine.register_fn(
        "set_scale",
        |h: &mut ScriptTransformHandle, x: Dynamic, y: Dynamic, z: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let x = dynamic_to_f32(&x).ok_or_else(|| rhai_err("x must be a number"))?;
            let y = dynamic_to_f32(&y).ok_or_else(|| rhai_err("y must be a number"))?;
            let z = dynamic_to_f32(&z).ok_or_else(|| rhai_err("z must be a number"))?;
            with_transform_mut(&h.0, |t| t.scale = Vec3::new(x, y, z))
        },
    );
    engine.register_fn(
        "set_scale",
        |h: &mut ScriptTransformHandle, scale: Vec3| -> Result<(), Box<EvalAltResult>> {
            with_transform_mut(&h.0, |t| t.scale = scale)
        },
    );
    engine.register_fn(
        "look_at",
        |h: &mut ScriptTransformHandle, x: Dynamic, y: Dynamic, z: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let x = dynamic_to_f32(&x).ok_or_else(|| rhai_err("x must be a number"))?;
            let y = dynamic_to_f32(&y).ok_or_else(|| rhai_err("y must be a number"))?;
            let z = dynamic_to_f32(&z).ok_or_else(|| rhai_err("z must be a number"))?;
            with_transform_mut(&h.0, |t| t.look_at(Vec3::new(x, y, z), Vec3::Y))
        },
    );
    engine.register_fn(
        "look_at",
        |h: &mut ScriptTransformHandle, target: Vec3| -> Result<(), Box<EvalAltResult>> {
            with_transform_mut(&h.0, |t| t.look_at(target, Vec3::Y))
        },
    );
    engine.register_fn("forward", |h: &mut ScriptTransformHandle| {
        read_transform(&h.0).forward().as_vec3()
    });
    engine.register_fn("right", |h: &mut ScriptTransformHandle| {
        read_transform(&h.0).right().as_vec3()
    });
    engine.register_fn("up", |h: &mut ScriptTransformHandle| {
        read_transform(&h.0).up().as_vec3()
    });
}

fn register_entity_api(engine: &mut Engine) {
    engine.register_type_with_name::<ScriptEntityHandle>("EntityApi");

    engine.register_get("id", |h: &mut ScriptEntityHandle| -> i64 {
        let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active_entity.map_or(0, |e| e.to_bits() as i64)
    });
    engine.register_get("bits", |h: &mut ScriptEntityHandle| -> i64 {
        let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active_entity.map_or(0, |e| e.to_bits() as i64)
    });
    engine.register_get("name", |h: &mut ScriptEntityHandle| -> String {
        let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active_entity_name.clone().unwrap_or_default()
    });
    engine.register_get("guid", |h: &mut ScriptEntityHandle| -> String {
        let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .active_entity_guid
            .map(|g| g.to_string())
            .unwrap_or_default()
    });

    engine.register_fn(
        "has_component",
        |h: &mut ScriptEntityHandle, type_path: ImmutableString| -> Result<bool, Box<EvalAltResult>> {
            let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let entity = state
                .active_entity
                .ok_or_else(|| rhai_err("no active script entity"))?;
            let world = state
                .world
                .as_ref()
                .ok_or_else(|| rhai_err("world unavailable outside hook execution"))?;
            has_reflected_component(world, entity, type_path.as_str()).map_err(rhai_err)
        },
    );
    engine.register_fn(
        "get_component",
        |h: &mut ScriptEntityHandle, type_path: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            read_component_for_entity(&h.0, None, type_path.as_str())
        },
    );
    engine.register_fn(
        "set_component",
        |h: &mut ScriptEntityHandle, type_path: ImmutableString, value: Dynamic| -> Result<(), Box<EvalAltResult>> {
            queue_set_component_for_entity(&h.0, None, type_path.as_str(), value)
        },
    );
    engine.register_fn(
        "get_component_field",
        |h: &mut ScriptEntityHandle, type_path: ImmutableString, field: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            read_field_for_entity(&h.0, None, type_path.as_str(), field.as_str())
        },
    );
    engine.register_fn(
        "get_field",
        |h: &mut ScriptEntityHandle, type_path: ImmutableString, field: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            read_field_for_entity(&h.0, None, type_path.as_str(), field.as_str())
        },
    );
    engine.register_fn(
        "set_component_field",
        |h: &mut ScriptEntityHandle, type_path: ImmutableString, field: ImmutableString, value: Dynamic| -> Result<(), Box<EvalAltResult>> {
            queue_set_field_for_entity(&h.0, None, type_path.as_str(), field.as_str(), value)
        },
    );
    engine.register_fn(
        "set_field",
        |h: &mut ScriptEntityHandle, type_path: ImmutableString, field: ImmutableString, value: Dynamic| -> Result<(), Box<EvalAltResult>> {
            queue_set_field_for_entity(&h.0, None, type_path.as_str(), field.as_str(), value)
        },
    );
    engine.register_fn("despawn", |h: &mut ScriptEntityHandle| -> Result<(), Box<EvalAltResult>> {
        let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let entity = state
            .active_entity
            .ok_or_else(|| rhai_err("no active script entity"))?;
        state.commands.push(ScriptCommand::DespawnEntity(entity));
        Ok(())
    });
}

fn register_input_api(engine: &mut Engine) {
    engine.register_type_with_name::<ScriptInputHandle>("InputApi");

    engine.register_fn(
        "action_pressed",
        |h: &mut ScriptInputHandle, name: ImmutableString| -> bool {
            let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .world
                .as_ref()
                .is_some_and(|w| read_action_sample(w, name.as_str(), state.fixed_step).pressed())
        },
    );
    engine.register_fn(
        "action_just_pressed",
        |h: &mut ScriptInputHandle, name: ImmutableString| -> bool {
            let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .world
                .as_ref()
                .is_some_and(|w| read_action_sample(w, name.as_str(), state.fixed_step).just_pressed)
        },
    );
    engine.register_fn(
        "action_just_released",
        |h: &mut ScriptInputHandle, name: ImmutableString| -> bool {
            let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .world
                .as_ref()
                .is_some_and(|w| read_action_sample(w, name.as_str(), state.fixed_step).just_released)
        },
    );
    engine.register_fn(
        "axis_1d",
        |h: &mut ScriptInputHandle, name: ImmutableString| -> f64 {
            let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.world.as_ref().map_or(0.0, |w| {
                f64::from(read_action_sample(w, name.as_str(), state.fixed_step).value.x)
            })
        },
    );
    engine.register_fn(
        "axis_2d",
        |h: &mut ScriptInputHandle, name: ImmutableString| -> Vec2 {
            let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.world.as_ref().map_or(Vec2::ZERO, |w| {
                read_action_sample(w, name.as_str(), state.fixed_step).axis_2d()
            })
        },
    );
    engine.register_fn(
        "axis_3d",
        |h: &mut ScriptInputHandle, name: ImmutableString| -> Vec3 {
            let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.world.as_ref().map_or(Vec3::ZERO, |w| {
                read_action_sample(w, name.as_str(), state.fixed_step).value
            })
        },
    );
}

fn register_physics_api(engine: &mut Engine) {
    engine.register_type_with_name::<ScriptPhysicsHandle>("PhysicsApi");

    engine.register_fn(
        "raycast_3d",
        |h: &mut ScriptPhysicsHandle,
         ox: Dynamic,
         oy: Dynamic,
         oz: Dynamic,
         dx: Dynamic,
         dy: Dynamic,
         dz: Dynamic,
         max_distance: Dynamic|
         -> Result<Dynamic, Box<EvalAltResult>> {
            let origin = Vec3::new(
                dynamic_to_f32(&ox).ok_or_else(|| rhai_err("raycast_3d ox must be a number"))?,
                dynamic_to_f32(&oy).ok_or_else(|| rhai_err("raycast_3d oy must be a number"))?,
                dynamic_to_f32(&oz).ok_or_else(|| rhai_err("raycast_3d oz must be a number"))?,
            );
            let dir = Vec3::new(
                dynamic_to_f32(&dx).ok_or_else(|| rhai_err("raycast_3d dx must be a number"))?,
                dynamic_to_f32(&dy).ok_or_else(|| rhai_err("raycast_3d dy must be a number"))?,
                dynamic_to_f32(&dz).ok_or_else(|| rhai_err("raycast_3d dz must be a number"))?,
            );
            let max_dist = dynamic_to_f32(&max_distance)
                .ok_or_else(|| rhai_err("raycast_3d max_distance must be a number"))?;
            do_raycast_3d(&h.0, origin, dir, max_dist, true, false)
        },
    );
    engine.register_fn(
        "raycast_3d",
        |h: &mut ScriptPhysicsHandle, origin: Vec3, dir: Vec3, max_distance: Dynamic| -> Result<Dynamic, Box<EvalAltResult>> {
            let max_dist = dynamic_to_f32(&max_distance)
                .ok_or_else(|| rhai_err("raycast_3d max_distance must be a number"))?;
            do_raycast_3d(&h.0, origin, dir, max_dist, true, false)
        },
    );
    engine.register_fn(
        "raycast_3d_with_options",
        |h: &mut ScriptPhysicsHandle,
         origin: Vec3,
         dir: Vec3,
         max_distance: Dynamic,
         options: Map|
         -> Result<Dynamic, Box<EvalAltResult>> {
            let max_dist = dynamic_to_f32(&max_distance)
                .ok_or_else(|| rhai_err("raycast_3d max_distance must be a number"))?;
            let exclude_self = options
                .get("exclude_self")
                .and_then(|v| v.as_bool().ok())
                .unwrap_or(true);
            let include_sensors = options
                .get("include_sensors")
                .and_then(|v| v.as_bool().ok())
                .unwrap_or(false);
            do_raycast_3d(&h.0, origin, dir, max_dist, exclude_self, include_sensors)
        },
    );

    engine.register_fn(
        "raycast_2d",
        |h: &mut ScriptPhysicsHandle,
         ox: Dynamic,
         oy: Dynamic,
         dx: Dynamic,
         dy: Dynamic,
         max_distance: Dynamic|
         -> Result<Dynamic, Box<EvalAltResult>> {
            let origin = Vec2::new(
                dynamic_to_f32(&ox).ok_or_else(|| rhai_err("raycast_2d ox must be a number"))?,
                dynamic_to_f32(&oy).ok_or_else(|| rhai_err("raycast_2d oy must be a number"))?,
            );
            let dir = Vec2::new(
                dynamic_to_f32(&dx).ok_or_else(|| rhai_err("raycast_2d dx must be a number"))?,
                dynamic_to_f32(&dy).ok_or_else(|| rhai_err("raycast_2d dy must be a number"))?,
            );
            let max_dist = dynamic_to_f32(&max_distance)
                .ok_or_else(|| rhai_err("raycast_2d max_distance must be a number"))?;
            do_raycast_2d(&h.0, origin, dir, max_dist, true, false)
        },
    );
    engine.register_fn(
        "raycast_2d",
        |h: &mut ScriptPhysicsHandle, origin: Vec2, dir: Vec2, max_distance: Dynamic| -> Result<Dynamic, Box<EvalAltResult>> {
            let max_dist = dynamic_to_f32(&max_distance)
                .ok_or_else(|| rhai_err("raycast_2d max_distance must be a number"))?;
            do_raycast_2d(&h.0, origin, dir, max_dist, true, false)
        },
    );
    engine.register_fn(
        "raycast_2d_with_options",
        |h: &mut ScriptPhysicsHandle,
         origin: Vec2,
         dir: Vec2,
         max_distance: Dynamic,
         options: Map|
         -> Result<Dynamic, Box<EvalAltResult>> {
            let max_dist = dynamic_to_f32(&max_distance)
                .ok_or_else(|| rhai_err("raycast_2d max_distance must be a number"))?;
            let exclude_self = options
                .get("exclude_self")
                .and_then(|v| v.as_bool().ok())
                .unwrap_or(true);
            let include_sensors = options
                .get("include_sensors")
                .and_then(|v| v.as_bool().ok())
                .unwrap_or(false);
            do_raycast_2d(&h.0, origin, dir, max_dist, exclude_self, include_sensors)
        },
    );

    engine.register_fn(
        "apply_impulse_3d",
        |h: &mut ScriptPhysicsHandle, x: Dynamic, y: Dynamic, z: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let impulse = Vec3::new(
                dynamic_to_f32(&x).ok_or_else(|| rhai_err("impulse x must be a number"))?,
                dynamic_to_f32(&y).ok_or_else(|| rhai_err("impulse y must be a number"))?,
                dynamic_to_f32(&z).ok_or_else(|| rhai_err("impulse z must be a number"))?,
            );
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let entity = state
                .active_entity
                .ok_or_else(|| rhai_err("no active script entity"))?;
            state
                .commands
                .push(ScriptCommand::ApplyImpulse3D { entity, impulse });
            Ok(())
        },
    );
    engine.register_fn(
        "apply_impulse_3d",
        |h: &mut ScriptPhysicsHandle, impulse: Vec3| -> Result<(), Box<EvalAltResult>> {
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let entity = state
                .active_entity
                .ok_or_else(|| rhai_err("no active script entity"))?;
            state
                .commands
                .push(ScriptCommand::ApplyImpulse3D { entity, impulse });
            Ok(())
        },
    );
    engine.register_fn(
        "apply_impulse_3d_on",
        |h: &mut ScriptPhysicsHandle, entity_bits: i64, x: Dynamic, y: Dynamic, z: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let entity = parse_entity_bits(entity_bits)?;
            let impulse = Vec3::new(
                dynamic_to_f32(&x).ok_or_else(|| rhai_err("impulse x must be a number"))?,
                dynamic_to_f32(&y).ok_or_else(|| rhai_err("impulse y must be a number"))?,
                dynamic_to_f32(&z).ok_or_else(|| rhai_err("impulse z must be a number"))?,
            );
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .commands
                .push(ScriptCommand::ApplyImpulse3D { entity, impulse });
            Ok(())
        },
    );

    engine.register_fn(
        "apply_impulse_2d",
        |h: &mut ScriptPhysicsHandle, x: Dynamic, y: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let impulse = Vec2::new(
                dynamic_to_f32(&x).ok_or_else(|| rhai_err("impulse x must be a number"))?,
                dynamic_to_f32(&y).ok_or_else(|| rhai_err("impulse y must be a number"))?,
            );
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let entity = state
                .active_entity
                .ok_or_else(|| rhai_err("no active script entity"))?;
            state
                .commands
                .push(ScriptCommand::ApplyImpulse2D { entity, impulse });
            Ok(())
        },
    );
    engine.register_fn(
        "apply_impulse_2d",
        |h: &mut ScriptPhysicsHandle, impulse: Vec2| -> Result<(), Box<EvalAltResult>> {
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let entity = state
                .active_entity
                .ok_or_else(|| rhai_err("no active script entity"))?;
            state
                .commands
                .push(ScriptCommand::ApplyImpulse2D { entity, impulse });
            Ok(())
        },
    );
    engine.register_fn(
        "apply_impulse_2d_on",
        |h: &mut ScriptPhysicsHandle, entity_bits: i64, x: Dynamic, y: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let entity = parse_entity_bits(entity_bits)?;
            let impulse = Vec2::new(
                dynamic_to_f32(&x).ok_or_else(|| rhai_err("impulse x must be a number"))?,
                dynamic_to_f32(&y).ok_or_else(|| rhai_err("impulse y must be a number"))?,
            );
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .commands
                .push(ScriptCommand::ApplyImpulse2D { entity, impulse });
            Ok(())
        },
    );

    engine.register_fn(
        "move_character_3d",
        |h: &mut ScriptPhysicsHandle, vx: Dynamic, vy: Dynamic, vz: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let velocity = Vec3::new(
                dynamic_to_f32(&vx).ok_or_else(|| rhai_err("vx must be a number"))?,
                dynamic_to_f32(&vy).ok_or_else(|| rhai_err("vy must be a number"))?,
                dynamic_to_f32(&vz).ok_or_else(|| rhai_err("vz must be a number"))?,
            );
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let entity = state
                .active_entity
                .ok_or_else(|| rhai_err("no active script entity"))?;
            state
                .commands
                .push(ScriptCommand::MoveCharacter3D { entity, velocity });
            Ok(())
        },
    );
    engine.register_fn(
        "move_character_3d",
        |h: &mut ScriptPhysicsHandle, velocity: Vec3| -> Result<(), Box<EvalAltResult>> {
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let entity = state
                .active_entity
                .ok_or_else(|| rhai_err("no active script entity"))?;
            state
                .commands
                .push(ScriptCommand::MoveCharacter3D { entity, velocity });
            Ok(())
        },
    );
    engine.register_fn(
        "move_character_2d",
        |h: &mut ScriptPhysicsHandle, vx: Dynamic, vy: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let velocity = Vec2::new(
                dynamic_to_f32(&vx).ok_or_else(|| rhai_err("vx must be a number"))?,
                dynamic_to_f32(&vy).ok_or_else(|| rhai_err("vy must be a number"))?,
            );
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let entity = state
                .active_entity
                .ok_or_else(|| rhai_err("no active script entity"))?;
            state
                .commands
                .push(ScriptCommand::MoveCharacter2D { entity, velocity });
            Ok(())
        },
    );
    engine.register_fn(
        "move_character_2d",
        |h: &mut ScriptPhysicsHandle, velocity: Vec2| -> Result<(), Box<EvalAltResult>> {
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let entity = state
                .active_entity
                .ok_or_else(|| rhai_err("no active script entity"))?;
            state
                .commands
                .push(ScriptCommand::MoveCharacter2D { entity, velocity });
            Ok(())
        },
    );

    engine.register_fn("is_grounded_3d", |h: &mut ScriptPhysicsHandle| -> bool {
        let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (Some(world), Some(entity)) = (state.world.as_ref(), state.active_entity) else {
            return false;
        };
        world
            .get::<CharacterMotion3D>(entity)
            .is_some_and(|m| m.grounded)
    });
    engine.register_fn("is_grounded_2d", |h: &mut ScriptPhysicsHandle| -> bool {
        let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (Some(world), Some(entity)) = (state.world.as_ref(), state.active_entity) else {
            return false;
        };
        world
            .get::<CharacterMotion2D>(entity)
            .is_some_and(|m| m.grounded)
    });
    engine.register_fn("linear_velocity_3d", |h: &mut ScriptPhysicsHandle| -> Vec3 {
        let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (Some(world), Some(entity)) = (state.world.as_ref(), state.active_entity) else {
            return Vec3::ZERO;
        };
        world
            .get_resource::<PhysicsWorld3D>()
            .and_then(|p| p.linear_velocity(entity))
            .unwrap_or(Vec3::ZERO)
    });
    engine.register_fn("linear_velocity_2d", |h: &mut ScriptPhysicsHandle| -> Vec2 {
        let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (Some(world), Some(entity)) = (state.world.as_ref(), state.active_entity) else {
            return Vec2::ZERO;
        };
        world
            .get_resource::<PhysicsWorld2D>()
            .and_then(|p| p.linear_velocity(entity))
            .unwrap_or(Vec2::ZERO)
    });
}

fn register_audio_api(engine: &mut Engine) {
    engine.register_type_with_name::<ScriptAudioHandle>("AudioApi");

    engine.register_fn(
        "play",
        |h: &mut ScriptAudioHandle, clip: ImmutableString| -> Result<(), Box<EvalAltResult>> {
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let entity = state
                .active_entity
                .ok_or_else(|| rhai_err("no active script entity"))?;
            state.commands.push(ScriptCommand::PlayAudioClip {
                entity,
                clip: clip.into_owned(),
                bus: AudioBus::Sfx,
                gain: 1.0,
                pitch: 1.0,
                looping: false,
                streaming: false,
            });
            Ok(())
        },
    );
    engine.register_fn(
        "play_with_options",
        |h: &mut ScriptAudioHandle, clip: ImmutableString, options: Map| -> Result<(), Box<EvalAltResult>> {
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let entity = state
                .active_entity
                .ok_or_else(|| rhai_err("no active script entity"))?;
            let bus = options
                .get("bus")
                .and_then(|v| v.clone().into_string().ok())
                .map_or(AudioBus::Sfx, |s| parse_audio_bus(&s));
            let gain = options
                .get("gain")
                .and_then(dynamic_to_f32)
                .unwrap_or(1.0);
            let pitch = options
                .get("pitch")
                .and_then(dynamic_to_f32)
                .unwrap_or(1.0);
            let looping = options
                .get("looping")
                .and_then(|v| v.as_bool().ok())
                .unwrap_or(false);
            let streaming = options
                .get("streaming")
                .and_then(|v| v.as_bool().ok())
                .unwrap_or(false);
            state.commands.push(ScriptCommand::PlayAudioClip {
                entity,
                clip: clip.into_owned(),
                bus,
                gain,
                pitch,
                looping,
                streaming,
            });
            Ok(())
        },
    );
    engine.register_fn("play_emitter", |h: &mut ScriptAudioHandle| -> Result<(), Box<EvalAltResult>> {
        let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let entity = state
            .active_entity
            .ok_or_else(|| rhai_err("no active script entity"))?;
        state
            .commands
            .push(ScriptCommand::PlayAudioEmitter { entity });
        Ok(())
    });
    engine.register_fn(
        "play_emitter_on",
        |h: &mut ScriptAudioHandle, entity_bits: i64| -> Result<(), Box<EvalAltResult>> {
            let entity = parse_entity_bits(entity_bits)?;
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .commands
                .push(ScriptCommand::PlayAudioEmitter { entity });
            Ok(())
        },
    );
}

fn register_world_api(engine: &mut Engine) {
    engine.register_type_with_name::<ScriptWorldHandle>("WorldApi");

    engine.register_fn(
        "spawn_prefab",
        |h: &mut ScriptWorldHandle, path: ImmutableString, x: Dynamic, y: Dynamic, z: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let pos = Vec3::new(
                dynamic_to_f32(&x).ok_or_else(|| rhai_err("spawn_prefab x must be a number"))?,
                dynamic_to_f32(&y).ok_or_else(|| rhai_err("spawn_prefab y must be a number"))?,
                dynamic_to_f32(&z).ok_or_else(|| rhai_err("spawn_prefab z must be a number"))?,
            );
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.commands.push(ScriptCommand::SpawnPrefab {
                path: path.into_owned(),
                position: pos,
                parent: None,
            });
            Ok(())
        },
    );
    engine.register_fn(
        "spawn_prefab",
        |h: &mut ScriptWorldHandle, path: ImmutableString, position: Vec3| -> Result<(), Box<EvalAltResult>> {
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.commands.push(ScriptCommand::SpawnPrefab {
                path: path.into_owned(),
                position,
                parent: None,
            });
            Ok(())
        },
    );
    engine.register_fn("despawn_self", |h: &mut ScriptWorldHandle| -> Result<(), Box<EvalAltResult>> {
        let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let entity = state
            .active_entity
            .ok_or_else(|| rhai_err("no active script entity"))?;
        state.commands.push(ScriptCommand::DespawnEntity(entity));
        Ok(())
    });
    engine.register_fn(
        "despawn",
        |h: &mut ScriptWorldHandle, entity_bits: i64| -> Result<(), Box<EvalAltResult>> {
            let entity = parse_entity_bits(entity_bits)?;
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.commands.push(ScriptCommand::DespawnEntity(entity));
            Ok(())
        },
    );
    engine.register_fn(
        "find_by_name",
        |h: &mut ScriptWorldHandle, target_name: ImmutableString| -> Dynamic {
            let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(world) = state.world.as_ref() else {
                return Dynamic::UNIT;
            };
            let target = target_name.as_str();
            let mut matches: Vec<Entity> = world
                .archetypes()
                .iter()
                .flat_map(|arch| arch.entities())
                .map(|ae| ae.id())
                .filter(|&e| world.get::<Name>(e).is_some_and(|n| n.as_str() == target))
                .collect();
            matches.sort_unstable();
            matches
                .first()
                .map_or(Dynamic::UNIT, |e| Dynamic::from(e.to_bits() as i64))
        },
    );
    engine.register_fn(
        "find_all_by_name",
        |h: &mut ScriptWorldHandle, target_name: ImmutableString| -> Array {
            let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(world) = state.world.as_ref() else {
                return Array::new();
            };
            let target = target_name.as_str();
            let mut matches: Vec<Entity> = world
                .archetypes()
                .iter()
                .flat_map(|arch| arch.entities())
                .map(|ae| ae.id())
                .filter(|&e| world.get::<Name>(e).is_some_and(|n| n.as_str() == target))
                .collect();
            matches.sort_unstable();
            matches
                .into_iter()
                .map(|e| Dynamic::from(e.to_bits() as i64))
                .collect()
        },
    );
    engine.register_fn(
        "find_by_guid",
        |h: &mut ScriptWorldHandle, guid_str: ImmutableString| -> Dynamic {
            let Ok(guid) = EntityGuid::parse_str(guid_str.as_str()) else {
                return Dynamic::UNIT;
            };
            let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(world) = state.world.as_ref() else {
                return Dynamic::UNIT;
            };
            world
                .get_resource::<EntityGuidMap>()
                .and_then(|map| map.get_entity(guid))
                .map_or(Dynamic::UNIT, |e| Dynamic::from(e.to_bits() as i64))
        },
    );
    engine.register_fn(
        "entity_exists",
        |h: &mut ScriptWorldHandle, entity_bits: i64| -> bool {
            let Some(entity) = Entity::try_from_bits(entity_bits as u64) else {
                return false;
            };
            let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .world
                .as_ref()
                .is_some_and(|w| w.get_entity(entity).is_ok())
        },
    );
    engine.register_fn(
        "get_component",
        |h: &mut ScriptWorldHandle, entity_bits: i64, type_path: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            let entity = parse_entity_bits(entity_bits)?;
            read_component_for_entity(&h.0, Some(entity), type_path.as_str())
        },
    );
    engine.register_fn(
        "get_component_field",
        |h: &mut ScriptWorldHandle, entity_bits: i64, type_path: ImmutableString, field: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            let entity = parse_entity_bits(entity_bits)?;
            read_field_for_entity(&h.0, Some(entity), type_path.as_str(), field.as_str())
        },
    );
    engine.register_fn(
        "set_component_field",
        |h: &mut ScriptWorldHandle,
         entity_bits: i64,
         type_path: ImmutableString,
         field: ImmutableString,
         value: Dynamic|
         -> Result<(), Box<EvalAltResult>> {
            let entity = parse_entity_bits(entity_bits)?;
            queue_set_field_for_entity(&h.0, Some(entity), type_path.as_str(), field.as_str(), value)
        },
    );
}

fn register_events_api(engine: &mut Engine) {
    engine.register_type_with_name::<ScriptEventsHandle>("EventsApi");

    engine.register_fn(
        "emit",
        |h: &mut ScriptEventsHandle, name: ImmutableString, payload: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let source_entity = state
                .active_entity
                .ok_or_else(|| rhai_err("no active script entity"))?;
            let event = ScriptCustomEvent {
                source_entity,
                source_name: state.active_entity_name.clone(),
                source_guid: state.active_entity_guid,
                name: name.into_owned(),
                payload: dynamic_to_json(&payload),
            };
            state.commands.push(ScriptCommand::EmitCustomEvent(event));
            Ok(())
        },
    );
    engine.register_fn(
        "emit",
        |h: &mut ScriptEventsHandle, name: ImmutableString| -> Result<(), Box<EvalAltResult>> {
            let mut state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let source_entity = state
                .active_entity
                .ok_or_else(|| rhai_err("no active script entity"))?;
            let event = ScriptCustomEvent {
                source_entity,
                source_name: state.active_entity_name.clone(),
                source_guid: state.active_entity_guid,
                name: name.into_owned(),
                payload: Value::Null,
            };
            state.commands.push(ScriptCommand::EmitCustomEvent(event));
            Ok(())
        },
    );
    engine.register_fn("log", |h: &mut ScriptEventsHandle, msg: Dynamic| {
        let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let text = dynamic_to_display_string(&msg);
        tracing::info!(
            target: "engine_scripting",
            entity = ?state.active_entity,
            entity_name = state.active_entity_name.as_deref().unwrap_or("-"),
            "{text}"
        );
    });
    engine.register_fn("warn", |h: &mut ScriptEventsHandle, msg: Dynamic| {
        let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let text = dynamic_to_display_string(&msg);
        tracing::warn!(
            target: "engine_scripting",
            entity = ?state.active_entity,
            entity_name = state.active_entity_name.as_deref().unwrap_or("-"),
            "{text}"
        );
    });
    engine.register_fn("error", |h: &mut ScriptEventsHandle, msg: Dynamic| {
        let state = h.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let text = dynamic_to_display_string(&msg);
        tracing::error!(
            target: "engine_scripting",
            entity = ?state.active_entity,
            entity_name = state.active_entity_name.as_deref().unwrap_or("-"),
            "{text}"
        );
    });
}

fn register_global_helpers(engine: &mut Engine, state_arc: &Arc<Mutex<ScriptCallState>>) {
    let s = state_arc.clone();
    engine.register_fn("log", move |msg: Dynamic| {
        let state = s.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let text = dynamic_to_display_string(&msg);
        tracing::info!(
            target: "engine_scripting",
            entity = ?state.active_entity,
            entity_name = state.active_entity_name.as_deref().unwrap_or("-"),
            "{text}"
        );
    });

    let s = state_arc.clone();
    engine.register_fn("log_warn", move |msg: Dynamic| {
        let state = s.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let text = dynamic_to_display_string(&msg);
        tracing::warn!(
            target: "engine_scripting",
            entity = ?state.active_entity,
            entity_name = state.active_entity_name.as_deref().unwrap_or("-"),
            "{text}"
        );
    });

    let s = state_arc.clone();
    engine.register_fn("log_error", move |msg: Dynamic| {
        let state = s.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let text = dynamic_to_display_string(&msg);
        tracing::error!(
            target: "engine_scripting",
            entity = ?state.active_entity,
            entity_name = state.active_entity_name.as_deref().unwrap_or("-"),
            "{text}"
        );
    });

    let s = state_arc.clone();
    engine.register_fn(
        "get_component",
        move |type_path: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            read_component_for_entity(&s, None, type_path.as_str())
        },
    );

    let s = state_arc.clone();
    engine.register_fn(
        "set_component",
        move |type_path: ImmutableString, value: Dynamic| -> Result<(), Box<EvalAltResult>> {
            queue_set_component_for_entity(&s, None, type_path.as_str(), value)
        },
    );

    let s = state_arc.clone();
    engine.register_fn(
        "get_component_field",
        move |type_path: ImmutableString, field: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            read_field_for_entity(&s, None, type_path.as_str(), field.as_str())
        },
    );

    let s = state_arc.clone();
    engine.register_fn(
        "get_field",
        move |type_path: ImmutableString, field: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            read_field_for_entity(&s, None, type_path.as_str(), field.as_str())
        },
    );

    let s = state_arc.clone();
    engine.register_fn(
        "set_component_field",
        move |type_path: ImmutableString, field: ImmutableString, value: Dynamic| -> Result<(), Box<EvalAltResult>> {
            queue_set_field_for_entity(&s, None, type_path.as_str(), field.as_str(), value)
        },
    );

    let s = state_arc.clone();
    engine.register_fn(
        "set_field",
        move |type_path: ImmutableString, field: ImmutableString, value: Dynamic| -> Result<(), Box<EvalAltResult>> {
            queue_set_field_for_entity(&s, None, type_path.as_str(), field.as_str(), value)
        },
    );
}

fn resolve_audio_clip_path(world: &World, clip: &str) -> Result<PathBuf, String> {
    if let Ok(uuid) = AssetUuid::parse_str(clip) {
        let Some(registry) = world.get_resource::<AssetRegistry>() else {
            return Err(format!(
                "cannot resolve audio UUID `{uuid}` without AssetRegistry"
            ));
        };
        return resolve_clip(registry, uuid).map_err(|err| err.to_string());
    }

    if let Some(registry) = world.get_resource::<AssetRegistry>()
        && let Some(record) = registry.get_by_path(clip)
        && let Some(abs) = registry.resolve_abs_path(record.uuid)
    {
        return Ok(abs);
    }

    let direct = PathBuf::from(clip);
    if direct.is_absolute() {
        return Ok(direct);
    }

    if let Some(paths) = world.get_resource::<ProjectPaths>() {
        let under_assets = paths.assets_dir.join(clip);
        if under_assets.exists() {
            return Ok(under_assets);
        }
        let under_root = paths.root_dir.join(clip);
        if under_root.exists() {
            return Ok(under_root);
        }
        return Ok(under_assets);
    }

    if let Some(registry) = world.get_resource::<AssetRegistry>() {
        let candidate = registry.shared().assets_dir().join(clip);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    Ok(direct)
}

fn resolve_prefab_paths(world: &World, raw: &str) -> Result<(PathBuf, String), String> {
    if let Ok(uuid) = AssetUuid::parse_str(raw) {
        let Some(registry) = world.get_resource::<AssetRegistry>() else {
            return Err(format!(
                "cannot resolve prefab UUID `{uuid}` without AssetRegistry"
            ));
        };
        let abs = registry
            .resolve_abs_path(uuid)
            .ok_or_else(|| format!("prefab asset UUID `{uuid}` not found in AssetRegistry"))?;
        let key = registry
            .shared()
            .resolve_path(uuid)
            .unwrap_or_else(|| raw.to_owned());
        return Ok((abs, key));
    }

    if let Some(registry) = world.get_resource::<AssetRegistry>()
        && let Some(record) = registry.get_by_path(raw)
        && let Some(abs) = registry.resolve_abs_path(record.uuid)
    {
        return Ok((abs, record.relative_path));
    }

    let direct = PathBuf::from(raw);
    if direct.is_absolute() {
        return Ok((direct, raw.replace('\\', "/")));
    }

    if let Some(paths) = world.get_resource::<ProjectPaths>() {
        let under_assets = paths.assets_dir.join(raw);
        if under_assets.exists() {
            return Ok((under_assets, raw.replace('\\', "/")));
        }
        let under_root = paths.root_dir.join(raw);
        if under_root.exists() {
            return Ok((under_root, raw.replace('\\', "/")));
        }
    }

    if let Some(registry) = world.get_resource::<AssetRegistry>() {
        let candidate = registry.shared().assets_dir().join(raw);
        if candidate.exists() {
            return Ok((candidate, raw.replace('\\', "/")));
        }
    }

    Ok((direct, raw.replace('\\', "/")))
}

fn emit_runtime_command_diagnostic(
    world: &mut World,
    file: &str,
    entity_ctx: &DiagnosticEntityContext,
    hook: &str,
    message: String,
) {
    let diag = world
        .resource_mut::<ScriptDiagnostics>()
        .record(
            ScriptDiagnosticSeverity::Error,
            ScriptDiagnosticStage::Runtime,
            file,
            None,
            None,
            entity_ctx,
            Some(hook.to_owned()),
            message,
        );
    world.trigger(ScriptDiagnosticEvent(diag));
}

/// Active audio voices started by scripts so PIE Stop / runtime reset stops them cleanly.
#[derive(Resource, Default, Debug)]
pub struct ActiveScriptVoices {
    voices: Vec<VoiceId>,
}

impl ActiveScriptVoices {
    /// Tracks a newly started script audio [`VoiceId`].
    pub fn push(&mut self, voice: VoiceId) {
        self.voices.push(voice);
    }

    /// Returns the number of tracked voice IDs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.voices.len()
    }

    /// Returns `true` when no script audio voices are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.voices.is_empty()
    }

    /// Removes and returns all tracked voice IDs.
    pub fn drain(&mut self) -> Vec<VoiceId> {
        std::mem::take(&mut self.voices)
    }
}

/// Stops all tracked script-triggered audio voices on [`AudioEngine`] and clears [`ActiveScriptVoices`].
pub(crate) fn stop_active_script_voices(world: &mut World) {
    let voices = world
        .get_resource_mut::<ActiveScriptVoices>()
        .map(|mut active| active.drain())
        .unwrap_or_default();
    if voices.is_empty() {
        return;
    }
    if let Some(mut audio) = world.get_resource_mut::<AudioEngine>() {
        for voice in voices {
            audio.stop(voice);
        }
    }
}

/// Deferred physics impulses queued for newly spawned dynamic bodies not yet synchronized into Rapier.
#[derive(Resource, Default, Debug)]
pub struct PendingScriptImpulses {
    /// Queued 3D impulses `(entity, impulse)` to retry once `PhysicsWorld3D` creates the body.
    pub impulses_3d: Vec<(Entity, Vec3)>,
    /// Queued 2D impulses `(entity, impulse)` to retry once `PhysicsWorld2D` creates the body.
    pub impulses_2d: Vec<(Entity, Vec2)>,
}

impl PendingScriptImpulses {
    /// Clears all buffered pending impulses.
    pub fn clear(&mut self) {
        self.impulses_3d.clear();
        self.impulses_2d.clear();
    }
}

/// Applies any pending impulses whose target rigid bodies now exist in `PhysicsWorld3D` / `PhysicsWorld2D`.
pub(crate) fn flush_pending_impulses(world: &mut World) {
    let (imp3, imp2) = {
        let Some(mut pending) = world.get_resource_mut::<PendingScriptImpulses>() else {
            return;
        };
        if pending.impulses_3d.is_empty() && pending.impulses_2d.is_empty() {
            return;
        }
        (
            std::mem::take(&mut pending.impulses_3d),
            std::mem::take(&mut pending.impulses_2d),
        )
    };

    for (entity, impulse) in imp3 {
        if let Some(mut physics) = world.get_resource_mut::<PhysicsWorld3D>() {
            let _ = physics.apply_impulse(entity, impulse);
        }
    }
    for (entity, impulse) in imp2 {
        if let Some(mut physics) = world.get_resource_mut::<PhysicsWorld2D>() {
            let _ = physics.apply_impulse(entity, impulse);
        }
    }
}

/// Flushes a batch of deferred [`ScriptCommand`] mutations safely against `world`.
pub fn flush_script_commands(
    world: &mut World,
    commands: Vec<ScriptCommand>,
    source_file: &str,
    entity_ctx: &DiagnosticEntityContext,
    hook_name: &str,
) {
    for cmd in commands {
        match cmd {
            ScriptCommand::SetTransform { entity, transform } => {
                if let Some(mut tfm) = world.get_mut::<Transform>(entity) {
                    *tfm = transform;
                } else if world.get_entity(entity).is_ok() {
                    world.entity_mut(entity).insert(transform);
                }
            }
            ScriptCommand::SetComponent {
                entity,
                type_path,
                value,
            } => {
                if let Err(err) = write_component_dynamic(world, entity, &type_path, &value) {
                    emit_runtime_command_diagnostic(world, source_file, entity_ctx, hook_name, err);
                }
            }
            ScriptCommand::SetComponentField {
                entity,
                type_path,
                field,
                value,
            } => {
                if let Err(err) =
                    write_component_field_dynamic(world, entity, &type_path, &field, &value)
                {
                    emit_runtime_command_diagnostic(world, source_file, entity_ctx, hook_name, err);
                }
            }
            ScriptCommand::ApplyImpulse3D { entity, impulse } => {
                let applied = world
                    .get_resource_mut::<PhysicsWorld3D>()
                    .is_some_and(|mut physics| physics.apply_impulse(entity, impulse));
                if !applied
                    && world
                        .get::<RigidBody3D>(entity)
                        .is_some_and(|body| body.kind == BodyKind::Dynamic)
                    && let Some(mut pending) = world.get_resource_mut::<PendingScriptImpulses>()
                {
                    pending.impulses_3d.push((entity, impulse));
                }
            }
            ScriptCommand::ApplyImpulse2D { entity, impulse } => {
                let applied = world
                    .get_resource_mut::<PhysicsWorld2D>()
                    .is_some_and(|mut physics| physics.apply_impulse(entity, impulse));
                if !applied
                    && world
                        .get::<RigidBody2D>(entity)
                        .is_some_and(|body| body.kind == BodyKind::Dynamic)
                    && let Some(mut pending) = world.get_resource_mut::<PendingScriptImpulses>()
                {
                    pending.impulses_2d.push((entity, impulse));
                }
            }
            ScriptCommand::MoveCharacter3D { entity, velocity } => {
                if let Some(mut motion) = world.get_mut::<CharacterMotion3D>(entity) {
                    motion.velocity = velocity;
                } else if world.get_entity(entity).is_ok() {
                    world.entity_mut(entity).insert(CharacterMotion3D {
                        velocity,
                        ..Default::default()
                    });
                }
            }
            ScriptCommand::MoveCharacter2D { entity, velocity } => {
                if let Some(mut motion) = world.get_mut::<CharacterMotion2D>(entity) {
                    motion.velocity = velocity;
                } else if world.get_entity(entity).is_ok() {
                    world.entity_mut(entity).insert(CharacterMotion2D {
                        velocity,
                        ..Default::default()
                    });
                }
            }
            ScriptCommand::PlayAudioClip {
                entity: _,
                clip,
                bus,
                gain,
                pitch,
                looping,
                streaming,
            } => match resolve_audio_clip_path(world, &clip) {
                Ok(path) => {
                    let play_result = world
                        .get_resource_mut::<AudioEngine>()
                        .filter(|audio| audio.available())
                        .map(|mut audio| {
                            let sound = PlaySound {
                                path,
                                bus,
                                spatial: None,
                                gain,
                                pitch,
                                streaming,
                                looping,
                            };
                            audio.play(&sound)
                        });
                    match play_result {
                        Some(Ok(voice)) => {
                            if let Some(mut active) = world.get_resource_mut::<ActiveScriptVoices>()
                            {
                                active.push(voice);
                            }
                        }
                        Some(Err(err)) => {
                            emit_runtime_command_diagnostic(
                                world,
                                source_file,
                                entity_ctx,
                                hook_name,
                                format!("audio.play('{clip}') failed: {err}"),
                            );
                        }
                        None => {}
                    }
                }
                Err(err) => {
                    emit_runtime_command_diagnostic(world, source_file, entity_ctx, hook_name, err);
                }
            },
            ScriptCommand::PlayAudioEmitter { entity } => {
                let Some(emitter) = world.get::<AudioEmitter>(entity).cloned() else {
                    emit_runtime_command_diagnostic(
                        world,
                        source_file,
                        entity_ctx,
                        hook_name,
                        format!("entity {entity:?} has no AudioEmitter component"),
                    );
                    continue;
                };
                let Some(clip_uuid) = emitter.clip else {
                    emit_runtime_command_diagnostic(
                        world,
                        source_file,
                        entity_ctx,
                        hook_name,
                        format!("AudioEmitter on {entity:?} has no clip assigned"),
                    );
                    continue;
                };
                let position = world
                    .get::<GlobalTransform>(entity)
                    .map(GlobalTransform::translation)
                    .or_else(|| world.get::<Transform>(entity).map(|t| t.translation))
                    .unwrap_or(Vec3::ZERO);
                let resolved = world
                    .get_resource::<AssetRegistry>()
                    .ok_or_else(|| "AssetRegistry unavailable for play_emitter".to_owned())
                    .and_then(|reg| resolve_clip(reg, clip_uuid).map_err(|e| e.to_string()));
                match resolved {
                    Ok(path) => {
                        let play_result = world
                            .get_resource_mut::<AudioEngine>()
                            .filter(|audio| audio.available())
                            .map(|mut audio| {
                                let sound = PlaySound {
                                    path,
                                    bus: emitter.bus,
                                    spatial: (emitter.space != AudioSpace::Global).then_some(
                                        SpatialSound {
                                            space: emitter.space,
                                            position,
                                            min_distance: emitter.min_distance,
                                            max_distance: emitter.max_distance,
                                        },
                                    ),
                                    gain: emitter.gain,
                                    pitch: emitter.pitch,
                                    streaming: emitter.streaming,
                                    looping: emitter.looping,
                                };
                                audio.play(&sound)
                            });
                        match play_result {
                            Some(Ok(voice)) => {
                                if let Some(mut active) =
                                    world.get_resource_mut::<ActiveScriptVoices>()
                                {
                                    active.push(voice);
                                }
                            }
                            Some(Err(err)) => {
                                emit_runtime_command_diagnostic(
                                    world,
                                    source_file,
                                    entity_ctx,
                                    hook_name,
                                    format!("audio.play_emitter() failed: {err}"),
                                );
                            }
                            None => {}
                        }
                    }
                    Err(err) => {
                        emit_runtime_command_diagnostic(
                            world,
                            source_file,
                            entity_ctx,
                            hook_name,
                            err,
                        );
                    }
                }
            }
            ScriptCommand::SpawnPrefab {
                path,
                position,
                parent,
            } => match resolve_prefab_paths(world, &path) {
                Ok((abs_path, key)) => {
                    if let Err(err) =
                        spawn_prefab_from_file(world, &abs_path, &key, parent, Some(position))
                    {
                        emit_runtime_command_diagnostic(
                            world,
                            source_file,
                            entity_ctx,
                            hook_name,
                            format!("world.spawn_prefab('{path}') failed: {err}"),
                        );
                    }
                }
                Err(err) => {
                    emit_runtime_command_diagnostic(world, source_file, entity_ctx, hook_name, err);
                }
            },
            ScriptCommand::DespawnEntity(entity) => {
                if world.get_entity(entity).is_ok() {
                    world.despawn(entity);
                }
            }
            ScriptCommand::EmitCustomEvent(event) => {
                world.trigger(event);
            }
        }
    }
}
