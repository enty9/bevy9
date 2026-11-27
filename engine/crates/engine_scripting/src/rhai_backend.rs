//! Rhai [`ScriptBackend`] implementation with strict sandboxing, AST caching, and per-entity scopes.

use std::{collections::HashMap, sync::Arc, time::SystemTime};

use bevy_ecs::entity::Entity;
use rhai::{CallFnOptions, Dynamic, Engine, Map, Scope, AST};

use crate::{
    api::{
        contact_info_to_dynamic, register_script_api, ScriptAudioHandle, ScriptCallContext,
        ScriptEntityHandle, ScriptEventsHandle, ScriptInputHandle, ScriptPhysicsHandle,
        ScriptTransformHandle, ScriptWorldHandle,
    },
    backend::{
        ScriptBackend, ScriptCompiledInfo, ScriptError, ScriptHook, ScriptHookArgs, ScriptHookSet,
        ScriptSandboxConfig, ScriptSourceKey,
    },
    component::ScriptProperties,
    diagnostics::{eval_error_details, parse_error_details},
    reflect_bridge::{properties_to_rhai_map, sync_rhai_map_into_properties},
};

struct CompiledRhaiScript {
    ast: Arc<AST>,
    source_label: String,
    content_hash: String,
    mtime: Option<SystemTime>,
    revision: u64,
    hooks: ScriptHookSet,
    hook_lines: HashMap<ScriptHook, u32>,
    on_update_takes_dt: bool,
    on_fixed_update_takes_dt: bool,
}

impl CompiledRhaiScript {
    fn to_info(&self, key: &ScriptSourceKey) -> ScriptCompiledInfo {
        ScriptCompiledInfo {
            key: key.clone(),
            source_label: self.source_label.clone(),
            content_hash: self.content_hash.clone(),
            mtime: self.mtime,
            revision: self.revision,
            hooks: self.hooks,
        }
    }
}

struct EntityRhaiInstance {
    script_key: ScriptSourceKey,
    ast_revision: u64,
    scope: Scope<'static>,
    started: bool,
    last_synced_properties: ScriptProperties,
}

/// Rhai scripting runtime backend implementing [`ScriptBackend`].
///
/// Enforces strict [`ScriptSandboxConfig`] resource limits, caches compiled [`AST`]s by
/// [`ScriptSourceKey`] and content hash, and isolates each entity in its own [`Scope`].
pub struct RhaiBackend {
    engine: Engine,
    sandbox: ScriptSandboxConfig,
    call_context: ScriptCallContext,
    cache: HashMap<ScriptSourceKey, CompiledRhaiScript>,
    instances: HashMap<Entity, EntityRhaiInstance>,
    next_revision: u64,
}

impl Default for RhaiBackend {
    fn default() -> Self {
        Self::new(ScriptSandboxConfig::default(), ScriptCallContext::new())
    }
}

impl RhaiBackend {
    /// Creates a new [`RhaiBackend`] configured with `sandbox` and wired to `call_context`.
    #[must_use]
    pub fn new(sandbox: ScriptSandboxConfig, call_context: ScriptCallContext) -> Self {
        let mut engine = Engine::new();
        apply_sandbox(&mut engine, &sandbox);
        register_script_api(&mut engine, &call_context);

        Self {
            engine,
            sandbox,
            call_context,
            cache: HashMap::new(),
            instances: HashMap::new(),
            next_revision: 1,
        }
    }

    /// Returns a clone of the shared [`ScriptCallContext`] bound to this backend's Rhai engine.
    #[must_use]
    pub fn call_context(&self) -> ScriptCallContext {
        self.call_context.clone()
    }

    /// Returns mutable access to the underlying [`rhai::Engine`] for registering custom host APIs.
    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    /// Returns read-only access to the underlying [`rhai::Engine`].
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Returns the number of compiled scripts currently held in the AST cache.
    #[must_use]
    pub fn cached_script_count(&self) -> usize {
        self.cache.len()
    }

    /// Returns the number of active per-entity script scopes.
    #[must_use]
    pub fn active_instance_count(&self) -> usize {
        self.instances.len()
    }

    fn build_base_scope(&self, properties: &ScriptProperties) -> Scope<'static> {
        let inner = &self.call_context.inner;
        let mut scope = Scope::new();
        scope.push("transform", ScriptTransformHandle(inner.clone()));
        scope.push("entity", ScriptEntityHandle(inner.clone()));
        scope.push("input", ScriptInputHandle(inner.clone()));
        scope.push("physics", ScriptPhysicsHandle(inner.clone()));
        scope.push("audio", ScriptAudioHandle(inner.clone()));
        scope.push("world", ScriptWorldHandle(inner.clone()));
        scope.push("events", ScriptEventsHandle(inner.clone()));
        scope.push("props", Dynamic::from_map(properties_to_rhai_map(properties)));
        scope
    }
}

fn find_hook_lines(source_code: &str) -> HashMap<ScriptHook, u32> {
    let mut lines = HashMap::new();
    let hooks = [
        ScriptHook::OnStart,
        ScriptHook::OnUpdate,
        ScriptHook::OnFixedUpdate,
        ScriptHook::OnCollision,
        ScriptHook::OnTriggerEnter,
        ScriptHook::OnTriggerExit,
    ];
    for (idx, line_text) in source_code.lines().enumerate() {
        let trimmed = line_text.trim_start();
        if let Some(rest) = trimmed.strip_prefix("fn")
            && rest.chars().next().is_some_and(char::is_whitespace)
        {
            let rest = rest.trim_start();
            for hook in hooks {
                let name = hook.fn_name();
                if let Some(after) = rest.strip_prefix(name)
                    && after.trim_start().starts_with('(')
                    && let Ok(line_no) = u32::try_from(idx + 1)
                {
                    lines.entry(hook).or_insert(line_no);
                }
            }
        }
    }
    lines
}

fn apply_sandbox(engine: &mut Engine, config: &ScriptSandboxConfig) {
    engine.set_max_operations(config.max_operations);
    engine.set_max_expr_depths(config.max_expr_depth, config.max_function_expr_depth);
    engine.set_max_call_levels(config.max_call_levels);
    engine.set_max_string_size(config.max_string_size);
    engine.set_max_array_size(config.max_array_size);
    engine.set_max_map_size(config.max_map_size);
    engine.set_max_variables(config.max_variables);
}

fn inspect_hooks(ast: &AST) -> (ScriptHookSet, bool, bool) {
    let mut hooks = ScriptHookSet::default();
    let mut on_update_takes_dt = false;
    let mut on_fixed_update_takes_dt = false;

    for meta in ast.iter_functions() {
        let arity = meta.params.len();
        match (meta.name, arity) {
            ("on_start", 0) => hooks.on_start = true,
            ("on_update", 1) => {
                hooks.on_update = true;
                on_update_takes_dt = true;
            }
            ("on_update", 0) => hooks.on_update = true,
            ("on_fixed_update", 1) => {
                hooks.on_fixed_update = true;
                on_fixed_update_takes_dt = true;
            }
            ("on_fixed_update", 0) => hooks.on_fixed_update = true,
            ("on_collision", 1) => hooks.on_collision = true,
            ("on_trigger_enter", 1) => hooks.on_trigger_enter = true,
            ("on_trigger_exit", 1) => hooks.on_trigger_exit = true,
            _ => {}
        }
    }

    (hooks, on_update_takes_dt, on_fixed_update_takes_dt)
}

impl ScriptBackend for RhaiBackend {
    fn name(&self) -> &'static str {
        "rhai"
    }

    fn sandbox_config(&self) -> &ScriptSandboxConfig {
        &self.sandbox
    }

    fn set_sandbox_config(&mut self, config: ScriptSandboxConfig) {
        self.sandbox = config;
        apply_sandbox(&mut self.engine, &self.sandbox);
    }

    fn compile_source(
        &mut self,
        key: &ScriptSourceKey,
        source_label: &str,
        source_code: &str,
        mtime: Option<SystemTime>,
    ) -> Result<ScriptCompiledInfo, ScriptError> {
        let content_hash = blake3::hash(source_code.as_bytes()).to_hex().to_string();

        if let Some(existing) = self.cache.get_mut(key)
            && existing.content_hash == content_hash
        {
            existing.mtime = mtime;
            existing.source_label = source_label.to_owned();
            return Ok(existing.to_info(key));
        }

        let mut ast = self.engine.compile(source_code).map_err(|parse_err| {
            let (line, column, message) = parse_error_details(&parse_err);
            ScriptError::Compile {
                file: source_label.to_owned(),
                line,
                column,
                message,
            }
        })?;
        ast.set_source(source_label);

        let (hooks, on_update_takes_dt, on_fixed_update_takes_dt) = inspect_hooks(&ast);
        let hook_lines = find_hook_lines(source_code);
        let revision = self.next_revision;
        self.next_revision = self.next_revision.saturating_add(1);

        let entry = CompiledRhaiScript {
            ast: Arc::new(ast),
            source_label: source_label.to_owned(),
            content_hash,
            mtime,
            revision,
            hooks,
            hook_lines,
            on_update_takes_dt,
            on_fixed_update_takes_dt,
        };
        let info = entry.to_info(key);
        self.cache.insert(key.clone(), entry);
        Ok(info)
    }

    fn compiled_info(&self, key: &ScriptSourceKey) -> Option<ScriptCompiledInfo> {
        self.cache.get(key).map(|entry| entry.to_info(key))
    }

    fn invalidate(&mut self, key: &ScriptSourceKey) {
        self.cache.remove(key);
    }

    fn clear_cache(&mut self) {
        self.cache.clear();
    }

    fn has_entity_instance(&self, entity: Entity) -> bool {
        self.instances.contains_key(&entity)
    }

    fn entity_started(&self, entity: Entity) -> bool {
        self.instances.get(&entity).is_some_and(|inst| inst.started)
    }

    fn remove_entity_instance(&mut self, entity: Entity) {
        self.instances.remove(&entity);
    }

    fn reset_all_instances(&mut self) {
        self.instances.clear();
    }

    fn sync_entity_properties(&mut self, entity: Entity, properties: &ScriptProperties) {
        if let Some(instance) = self.instances.get_mut(&entity)
            && instance.last_synced_properties != *properties
        {
            instance.scope.set_or_push(
                "props",
                Dynamic::from_map(properties_to_rhai_map(properties)),
            );
            instance.last_synced_properties = properties.clone();
        }
    }

    fn invoke_hook(
        &mut self,
        entity: Entity,
        key: &ScriptSourceKey,
        properties: &mut ScriptProperties,
        hook: ScriptHook,
        args: ScriptHookArgs,
        _call_context: &ScriptCallContext,
    ) -> Result<bool, ScriptError> {
        let Some(compiled) = self.cache.get(key) else {
            return Err(ScriptError::NotFound(key.display_label()));
        };
        let ast = compiled.ast.clone();
        let source_label = compiled.source_label.clone();
        let ast_revision = compiled.revision;
        let hooks = compiled.hooks;
        let fallback_line = compiled.hook_lines.get(&hook).copied();
        let on_update_takes_dt = compiled.on_update_takes_dt;
        let on_fixed_update_takes_dt = compiled.on_fixed_update_takes_dt;

        let needs_init = match self.instances.get(&entity) {
            None => true,
            Some(inst) => inst.script_key != *key || inst.ast_revision != ast_revision,
        };

        if needs_init {
            let preserve_started = self
                .instances
                .get(&entity)
                .is_some_and(|inst| inst.script_key == *key && inst.started);
            let mut scope = self.build_base_scope(properties);

            // Evaluate top-level script statements once so per-entity state variables initialize.
            if let Err(err) = self.engine.run_ast_with_scope(&mut scope, &ast) {
                let (line, column, message, sandbox_limit) = eval_error_details(&err);
                return Err(ScriptError::Runtime {
                    file: source_label,
                    line,
                    column,
                    message,
                    sandbox_limit,
                });
            }

            if let Some(map) = scope.get_value::<Map>("props") {
                sync_rhai_map_into_properties(&map, properties);
            } else {
                scope.set_or_push(
                    "props",
                    Dynamic::from_map(properties_to_rhai_map(properties)),
                );
            }

            self.instances.insert(
                entity,
                EntityRhaiInstance {
                    script_key: key.clone(),
                    ast_revision,
                    scope,
                    started: preserve_started,
                    last_synced_properties: properties.clone(),
                },
            );
        }

        let Some(instance) = self.instances.get_mut(&entity) else {
            return Ok(false);
        };

        // Sync any external Inspector edits into `props` before invoking the hook.
        if instance.last_synced_properties != *properties {
            instance.scope.set_or_push(
                "props",
                Dynamic::from_map(properties_to_rhai_map(properties)),
            );
            instance.last_synced_properties = properties.clone();
        }

        if hook == ScriptHook::OnStart {
            instance.started = true;
        }

        if !hooks.contains(hook) {
            return Ok(false);
        }

        let options = CallFnOptions::new().eval_ast(false).rewind_scope(true);
        let call_result: Result<Dynamic, Box<rhai::EvalAltResult>> = match (hook, args) {
            (ScriptHook::OnStart, _) => self.engine.call_fn_with_options(
                options,
                &mut instance.scope,
                &ast,
                hook.fn_name(),
                (),
            ),
            (ScriptHook::OnUpdate, ScriptHookArgs::Delta(dt)) => {
                if on_update_takes_dt {
                    self.engine.call_fn_with_options(
                        options,
                        &mut instance.scope,
                        &ast,
                        hook.fn_name(),
                        (dt,),
                    )
                } else {
                    self.engine.call_fn_with_options(
                        options,
                        &mut instance.scope,
                        &ast,
                        hook.fn_name(),
                        (),
                    )
                }
            }
            (ScriptHook::OnFixedUpdate, ScriptHookArgs::Delta(dt)) => {
                if on_fixed_update_takes_dt {
                    self.engine.call_fn_with_options(
                        options,
                        &mut instance.scope,
                        &ast,
                        hook.fn_name(),
                        (dt,),
                    )
                } else {
                    self.engine.call_fn_with_options(
                        options,
                        &mut instance.scope,
                        &ast,
                        hook.fn_name(),
                        (),
                    )
                }
            }
            (
                ScriptHook::OnCollision | ScriptHook::OnTriggerEnter | ScriptHook::OnTriggerExit,
                ScriptHookArgs::Contact(contact),
            ) => {
                let other = contact_info_to_dynamic(&contact);
                self.engine.call_fn_with_options(
                    options,
                    &mut instance.scope,
                    &ast,
                    hook.fn_name(),
                    (other,),
                )
            }
            _ => self.engine.call_fn_with_options(
                options,
                &mut instance.scope,
                &ast,
                hook.fn_name(),
                (),
            ),
        };

        if let Err(err) = call_result {
            let (line, column, message, sandbox_limit) = eval_error_details(&err);
            return Err(ScriptError::Runtime {
                file: source_label,
                line: line.or(fallback_line),
                column,
                message,
                sandbox_limit,
            });
        }

        if let Some(map) = instance.scope.get_value::<Map>("props") {
            sync_rhai_map_into_properties(&map, properties);
            instance.last_synced_properties = properties.clone();
        }

        Ok(true)
    }
}
