//! Pluggable [`ScriptBackend`] abstraction, sandbox limits, and lifecycle hook definitions.

use std::time::SystemTime;

use bevy_ecs::{entity::Entity, reflect::ReflectResource, resource::Resource};
use bevy_reflect::{std_traits::ReflectDefault, Reflect};
use engine_core::{AssetUuid, EntityGuid};
use engine_physics::{ContactPhase, PhysicsDimension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::component::ScriptProperties;

/// Sandbox safety limits protecting the host from infinite loops and runaway script allocations.
#[derive(Resource, Reflect, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Resource, Default)]
pub struct ScriptSandboxConfig {
    /// Maximum number of VM operations allowed per script evaluation/hook invocation.
    pub max_operations: u64,
    /// Maximum expression nesting depth at top-level.
    pub max_expr_depth: usize,
    /// Maximum expression nesting depth inside script functions.
    pub max_function_expr_depth: usize,
    /// Maximum function call stack depth.
    pub max_call_levels: usize,
    /// Maximum length (in bytes) of a single script string.
    pub max_string_size: usize,
    /// Maximum number of elements in a single script array.
    pub max_array_size: usize,
    /// Maximum number of entries in a single script map.
    pub max_map_size: usize,
    /// Maximum number of variables simultaneously in scope.
    pub max_variables: usize,
}

impl Default for ScriptSandboxConfig {
    fn default() -> Self {
        Self {
            max_operations: 100_000,
            max_expr_depth: 64,
            max_function_expr_depth: 32,
            max_call_levels: 32,
            max_string_size: 65_536,
            max_array_size: 4_096,
            max_map_size: 1_024,
            max_variables: 1_024,
        }
    }
}

/// Unique cache key identifying a compiled script source.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ScriptSourceKey {
    /// Script indexed by persistent [`AssetUuid`] in `AssetRegistry`.
    Asset(AssetUuid),
    /// Script addressed by normalized filesystem path.
    Path(String),
    /// In-memory virtual script registered directly with the runtime.
    Inline(String),
}

impl ScriptSourceKey {
    /// Returns a human-readable display label for diagnostics.
    #[must_use]
    pub fn display_label(&self) -> String {
        match self {
            Self::Asset(uuid) => format!("uuid://{uuid}"),
            Self::Path(path) => path.clone(),
            Self::Inline(name) => format!("inline://{name}"),
        }
    }
}

/// Supported entity script lifecycle hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScriptHook {
    /// Invoked once when an entity script starts executing (`on_start()`).
    OnStart,
    /// Invoked every variable-timestep frame (`on_update(dt)`).
    OnUpdate,
    /// Invoked every fixed-timestep physics tick before physics steps (`on_fixed_update(dt)`).
    OnFixedUpdate,
    /// Invoked when a solid collision starts (`on_collision(other)`).
    OnCollision,
    /// Invoked when a sensor trigger overlap begins (`on_trigger_enter(other)`).
    OnTriggerEnter,
    /// Invoked when a sensor trigger overlap ends (`on_trigger_exit(other)`).
    OnTriggerExit,
}

impl ScriptHook {
    /// Canonical script function name for this hook.
    #[must_use]
    pub const fn fn_name(self) -> &'static str {
        match self {
            Self::OnStart => "on_start",
            Self::OnUpdate => "on_update",
            Self::OnFixedUpdate => "on_fixed_update",
            Self::OnCollision => "on_collision",
            Self::OnTriggerEnter => "on_trigger_enter",
            Self::OnTriggerExit => "on_trigger_exit",
        }
    }

    /// Returns `true` when this hook runs inside the `FixedUpdate` schedule.
    #[must_use]
    pub const fn is_fixed_step(self) -> bool {
        matches!(
            self,
            Self::OnFixedUpdate | Self::OnCollision | Self::OnTriggerEnter | Self::OnTriggerExit
        )
    }
}

/// Summary of lifecycle hooks defined inside a compiled script.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScriptHookSet {
    /// Script defines `on_start()`.
    pub on_start: bool,
    /// Script defines `on_update(dt)` (or `on_update()`).
    pub on_update: bool,
    /// Script defines `on_fixed_update(dt)` (or `on_fixed_update()`).
    pub on_fixed_update: bool,
    /// Script defines `on_collision(other)`.
    pub on_collision: bool,
    /// Script defines `on_trigger_enter(other)`.
    pub on_trigger_enter: bool,
    /// Script defines `on_trigger_exit(other)`.
    pub on_trigger_exit: bool,
}

impl ScriptHookSet {
    /// Returns `true` if `hook` is defined in the compiled script.
    #[must_use]
    pub const fn contains(self, hook: ScriptHook) -> bool {
        match hook {
            ScriptHook::OnStart => self.on_start,
            ScriptHook::OnUpdate => self.on_update,
            ScriptHook::OnFixedUpdate => self.on_fixed_update,
            ScriptHook::OnCollision => self.on_collision,
            ScriptHook::OnTriggerEnter => self.on_trigger_enter,
            ScriptHook::OnTriggerExit => self.on_trigger_exit,
        }
    }
}

/// Contact metadata delivered as the `other` argument to collision and trigger hooks.
#[derive(Debug, Clone)]
pub struct ScriptContactInfo {
    /// The other entity involved in the contact or trigger overlap.
    pub other_entity: Entity,
    /// Name of the other entity, if it has a `Name` component.
    pub other_name: Option<String>,
    /// Persistent GUID of the other entity, if it has an [`EntityGuid`].
    pub other_guid: Option<EntityGuid>,
    /// Physics world dimension (`2d` or `3d`).
    pub dimension: PhysicsDimension,
    /// Contact phase (`started` or `stopped`).
    pub phase: ContactPhase,
}

/// Typed arguments passed to a [`ScriptHook`] invocation.
#[derive(Debug, Clone)]
pub enum ScriptHookArgs {
    /// No arguments (`on_start()`).
    None,
    /// Frame or fixed-step delta time in seconds (`on_update(dt)`, `on_fixed_update(dt)`).
    Delta(f64),
    /// Contact/trigger counterpart info (`on_collision(other)`, `on_trigger_enter(other)`, `on_trigger_exit(other)`).
    Contact(ScriptContactInfo),
}

/// Metadata for a compiled script cached inside a [`ScriptBackend`].
#[derive(Debug, Clone)]
pub struct ScriptCompiledInfo {
    /// Cache key identifying this script.
    pub key: ScriptSourceKey,
    /// Display path or identifier recorded in diagnostics.
    pub source_label: String,
    /// Hex-encoded BLAKE3 digest of the compiled source text.
    pub content_hash: String,
    /// Optional last-modified timestamp on disk when compiled.
    pub mtime: Option<SystemTime>,
    /// Monotonically increasing revision incremented on each recompile.
    pub revision: u64,
    /// Lifecycle hooks discovered in the compiled script.
    pub hooks: ScriptHookSet,
}

/// Errors produced by compiling, loading, or executing scripts.
#[derive(Debug, Error, Clone)]
pub enum ScriptError {
    /// Failed to read a script file from disk.
    #[error("failed to read script '{path}': {message}")]
    Io {
        /// Filesystem path or label.
        path: String,
        /// Underlying OS error description.
        message: String,
    },
    /// Script source could not be found in cache or on disk.
    #[error("script not found: {0}")]
    NotFound(String),
    /// Script syntax or compilation failure.
    #[error("script compile error in '{file}': {message}")]
    Compile {
        /// Script path or label.
        file: String,
        /// 1-based line number, if available.
        line: Option<u32>,
        /// 1-based column number, if available.
        column: Option<u32>,
        /// Compiler/parser error description.
        message: String,
    },
    /// Runtime error or sandbox violation while executing a script.
    #[error("script runtime error in '{file}': {message}")]
    Runtime {
        /// Script path or label.
        file: String,
        /// 1-based line number, if available.
        line: Option<u32>,
        /// 1-based column number, if available.
        column: Option<u32>,
        /// Runtime error description.
        message: String,
        /// `true` when execution was terminated by a sandbox guard (e.g. `max_operations`).
        sandbox_limit: bool,
    },
}

/// Pluggable script runtime backend abstraction (implemented by `RhaiBackend`).
pub trait ScriptBackend: Send + Sync + 'static {
    /// Short backend identifier (for example `"rhai"`).
    fn name(&self) -> &'static str;

    /// Returns the active sandbox configuration.
    fn sandbox_config(&self) -> &ScriptSandboxConfig;

    /// Updates sandbox limits for future compilations and executions.
    fn set_sandbox_config(&mut self, config: ScriptSandboxConfig);

    /// Compiles and caches `source_code` under `key`, returning its compiled metadata.
    fn compile_source(
        &mut self,
        key: &ScriptSourceKey,
        source_label: &str,
        source_code: &str,
        mtime: Option<SystemTime>,
    ) -> Result<ScriptCompiledInfo, ScriptError>;

    /// Returns metadata for a previously compiled script, if cached.
    fn compiled_info(&self, key: &ScriptSourceKey) -> Option<ScriptCompiledInfo>;

    /// Returns `true` if `key` is already compiled and matches `content_hash`.
    fn is_cached_fresh(&self, key: &ScriptSourceKey, content_hash: &str) -> bool {
        self.compiled_info(key)
            .is_some_and(|info| info.content_hash == content_hash)
    }

    /// Removes a single script from the compiled cache.
    fn invalidate(&mut self, key: &ScriptSourceKey);

    /// Removes all scripts from the compiled cache.
    fn clear_cache(&mut self);

    /// Returns `true` if `entity` has an active script execution instance.
    fn has_entity_instance(&self, entity: Entity) -> bool;

    /// Returns `true` if `on_start()` has already run for `entity`.
    fn entity_started(&self, entity: Entity) -> bool;

    /// Removes per-entity runtime state for `entity` (for example on despawn or disable).
    fn remove_entity_instance(&mut self, entity: Entity);

    /// Resets all per-entity runtime state (called on PIE Stop) while keeping compiled caches intact.
    fn reset_all_instances(&mut self);

    /// Pushes updated [`ScriptProperties`] into an existing entity instance's scope.
    fn sync_entity_properties(&mut self, entity: Entity, properties: &ScriptProperties);

    /// Invokes a lifecycle `hook` for `entity` against the compiled script at `key`.
    ///
    /// Returns `Ok(true)` if the hook existed and executed, `Ok(false)` if the script
    /// did not define `hook`, or `Err(ScriptError)` if execution failed.
    fn invoke_hook(
        &mut self,
        entity: Entity,
        key: &ScriptSourceKey,
        properties: &mut ScriptProperties,
        hook: ScriptHook,
        args: ScriptHookArgs,
        call_context: &crate::api::ScriptCallContext,
    ) -> Result<bool, ScriptError>;
}
