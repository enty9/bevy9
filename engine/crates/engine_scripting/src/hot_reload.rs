//! File-watcher and content-hash hot-reload for `.rhai` scripts on disk.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use bevy_ecs::{entity::Entity, event::Event, name::Name, resource::Resource, world::World};
use crossbeam_channel::{unbounded, Receiver};
use engine_assets::AssetRegistry;
use engine_core::{EntityGuid, ProjectPaths};
use notify::{Event as NotifyEvent, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::{
    backend::{ScriptBackend, ScriptError, ScriptSourceKey},
    component::ScriptComponent,
    diagnostics::{
        DiagnosticEntityContext, ScriptDiagnosticEvent, ScriptDiagnosticSeverity,
        ScriptDiagnosticStage, ScriptDiagnostics,
    },
    ScriptRuntime,
};

/// Configuration controlling `.rhai` script hot-reload polling and filesystem watching.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptHotReloadConfig {
    /// Whether automatic `.rhai` hot-reload is active.
    pub enabled: bool,
    /// Minimum interval between content-hash polling sweeps when no OS watcher event fired.
    pub poll_interval: Duration,
}

impl Default for ScriptHotReloadConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval: Duration::from_millis(100),
        }
    }
}

impl ScriptHotReloadConfig {
    /// Enables hot-reload with zero poll delay (useful for deterministic headless tests).
    #[must_use]
    pub const fn immediate() -> Self {
        Self {
            enabled: true,
            poll_interval: Duration::ZERO,
        }
    }

    /// Disables hot-reload completely.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            poll_interval: Duration::from_secs(1),
        }
    }
}

/// Bevy observer event emitted whenever a `.rhai` script successfully hot-reloads on disk.
#[derive(Event, Debug, Clone, PartialEq, Eq)]
pub struct ScriptReloadedEvent {
    /// Cache key of the reloaded script.
    pub key: ScriptSourceKey,
    /// Display path of the reloaded script.
    pub file: String,
    /// New compiled AST revision number.
    pub revision: u64,
    /// Hex-encoded BLAKE3 digest of the new source bytes.
    pub content_hash: String,
}

/// Live OS filesystem watcher monitoring directories containing `.rhai` scripts.
#[derive(Resource)]
pub struct ScriptHotReloadWatcher {
    watcher: Option<RecommendedWatcher>,
    rx: Receiver<PathBuf>,
    watched_dirs: HashSet<PathBuf>,
}

impl Default for ScriptHotReloadWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptHotReloadWatcher {
    /// Creates a live [`RecommendedWatcher`] filtering for `.rhai` file modifications.
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        let watcher = notify::recommended_watcher(move |res: Result<NotifyEvent, notify::Error>| {
            let Ok(event) = res else {
                return;
            };
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            for path in event.paths {
                let is_rhai = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("rhai"));
                if is_rhai {
                    let _ = tx.send(path);
                }
            }
        });

        let watcher = match watcher {
            Ok(w) => Some(w),
            Err(err) => {
                tracing::warn!(
                    target: "engine_scripting",
                    error = %err,
                    "failed to initialize OS script file watcher; falling back to polling"
                );
                None
            }
        };

        Self {
            watcher,
            rx,
            watched_dirs: HashSet::new(),
        }
    }

    /// Registers `dir` for recursive `.rhai` change notifications (idempotent).
    pub fn watch_dir(&mut self, dir: &Path) {
        if !dir.exists() || !dir.is_dir() {
            return;
        }
        let canonical = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        if self.watched_dirs.contains(&canonical) {
            return;
        }
        if let Some(watcher) = &mut self.watcher {
            if let Err(err) = watcher.watch(&canonical, RecursiveMode::Recursive) {
                tracing::warn!(
                    target: "engine_scripting",
                    path = %canonical.display(),
                    error = %err,
                    "failed to watch script directory"
                );
            } else {
                self.watched_dirs.insert(canonical);
            }
        }
    }

    /// Drains all `.rhai` file paths reported changed since the last call.
    #[must_use]
    pub fn drain_changed_paths(&self) -> HashSet<PathBuf> {
        let mut changed = HashSet::new();
        while let Ok(path) = self.rx.try_recv() {
            let canonical = fs::canonicalize(&path).unwrap_or(path);
            changed.insert(canonical);
        }
        changed
    }
}

/// Tracking state for a single disk-backed `.rhai` script file.
#[derive(Debug, Clone)]
pub struct TrackedScriptFile {
    /// Cache key in the backend.
    pub key: ScriptSourceKey,
    /// Absolute path on disk.
    pub abs_path: PathBuf,
    /// Human-readable file label for diagnostics.
    pub source_label: String,
    /// Last observed modification time.
    pub last_mtime: Option<SystemTime>,
    /// BLAKE3 digest of the last successfully compiled version.
    pub last_hash: Option<String>,
    /// BLAKE3 digest of the last failed compile (prevents per-frame duplicate diagnostics).
    pub last_failed_hash: Option<String>,
    /// `true` while the current on-disk source fails to compile.
    pub has_compile_error: bool,
}

/// Tracks disk-backed `.rhai` scripts for automatic hot-reload and error deduplication.
#[derive(Resource, Default, Debug)]
pub struct ScriptFileTracker {
    files: HashMap<ScriptSourceKey, TrackedScriptFile>,
    last_poll: Option<Instant>,
    reload_count: u64,
}

impl ScriptFileTracker {
    /// Returns the number of tracked disk-backed scripts.
    #[must_use]
    pub fn tracked_count(&self) -> usize {
        self.files.len()
    }

    /// Returns total number of successful hot-reloads performed.
    #[must_use]
    pub const fn reload_count(&self) -> u64 {
        self.reload_count
    }

    /// Returns `true` if `key` currently has an unresolved compile error on disk.
    #[must_use]
    pub fn is_errored(&self, key: &ScriptSourceKey) -> bool {
        self.files
            .get(key)
            .is_some_and(|entry| entry.has_compile_error)
    }

    /// Returns tracked metadata for `key`, if any.
    #[must_use]
    pub fn get(&self, key: &ScriptSourceKey) -> Option<&TrackedScriptFile> {
        self.files.get(key)
    }

    /// Clears all tracked script metadata.
    pub fn clear(&mut self) {
        self.files.clear();
        self.last_poll = None;
    }
}

/// Resolved script identity and optional filesystem path for a [`ScriptComponent`].
#[derive(Debug, Clone)]
pub struct ResolvedScript {
    /// Cache key identifying this script.
    pub key: ScriptSourceKey,
    /// Resolved path on disk (`None` for inline scripts or unresolved UUIDs).
    pub abs_path: Option<PathBuf>,
    /// Human-readable file label for diagnostics.
    pub source_label: String,
}

/// Resolves a [`ScriptComponent`] reference into a [`ResolvedScript`].
#[must_use]
pub fn resolve_script_source(
    world: &World,
    backend: &dyn ScriptBackend,
    comp: &ScriptComponent,
) -> Option<ResolvedScript> {
    if comp.is_unassigned() {
        return None;
    }

    // 1. Prefer persistent AssetUuid when set.
    if let Some(uuid) = comp.script {
        if let Some(registry) = world.get_resource::<AssetRegistry>()
            && let Some(abs_path) = registry.resolve_abs_path(uuid)
        {
            let source_label = registry
                .shared()
                .resolve_path(uuid)
                .unwrap_or_else(|| abs_path.to_string_lossy().replace('\\', "/"));
            return Some(ResolvedScript {
                key: ScriptSourceKey::Asset(uuid),
                abs_path: Some(abs_path),
                source_label,
            });
        }

        // Check if registered in-memory under this UUID already.
        let key = ScriptSourceKey::Asset(uuid);
        if let Some(info) = backend.compiled_info(&key) {
            return Some(ResolvedScript {
                key,
                abs_path: None,
                source_label: info.source_label,
            });
        }

        // Fallback to script_path if provided.
        if comp.script_path.trim().is_empty() {
            return Some(ResolvedScript {
                key,
                abs_path: None,
                source_label: format!("uuid://{uuid}"),
            });
        }
    }

    let raw = comp.script_path.trim();
    if raw.is_empty() {
        return None;
    }

    let normalized = raw.replace('\\', "/");

    // 2. Check inline script cache first (for tests and virtual scripts).
    let inline_key = ScriptSourceKey::Inline(normalized.clone());
    if let Some(info) = backend.compiled_info(&inline_key) {
        return Some(ResolvedScript {
            key: inline_key,
            abs_path: None,
            source_label: info.source_label,
        });
    }

    // 3. Check if already cached as a Path key without disk re-resolution.
    let path_key = ScriptSourceKey::Path(normalized.clone());
    let candidate = resolve_disk_script_path(world, raw);
    if candidate.exists() {
        let abs = fs::canonicalize(&candidate).unwrap_or(candidate);
        return Some(ResolvedScript {
            key: path_key,
            abs_path: Some(abs),
            source_label: normalized,
        });
    }

    if let Some(info) = backend.compiled_info(&path_key) {
        return Some(ResolvedScript {
            key: path_key,
            abs_path: None,
            source_label: info.source_label,
        });
    }

    Some(ResolvedScript {
        key: path_key,
        abs_path: Some(candidate),
        source_label: normalized,
    })
}

fn resolve_disk_script_path(world: &World, raw: &str) -> PathBuf {
    let direct = PathBuf::from(raw);
    if direct.is_absolute() {
        return direct;
    }

    if let Some(paths) = world.get_resource::<ProjectPaths>() {
        let under_scripts = paths.scripts_dir.join(raw);
        if under_scripts.exists() {
            return under_scripts;
        }
        let under_assets = paths.assets_dir.join(raw);
        if under_assets.exists() {
            return under_assets;
        }
        let under_root = paths.root_dir.join(raw);
        if under_root.exists() {
            return under_root;
        }
    }

    if let Some(registry) = world.get_resource::<AssetRegistry>() {
        if let Some(rec) = registry.get_by_path(raw)
            && let Some(abs) = registry.resolve_abs_path(rec.uuid)
        {
            return abs;
        }
        let under_assets = registry.shared().assets_dir().join(raw);
        if under_assets.exists() {
            return under_assets;
        }
    }

    direct
}

/// Ensures `resolved` is compiled in `backend` and registered with [`ScriptFileTracker`].
///
/// Returns `true` if the script is ready to execute, or `false` if loading/compilation failed
/// (diagnostics are recorded without crashing).
pub(crate) fn ensure_script_compiled(
    world: &mut World,
    backend: &mut dyn ScriptBackend,
    resolved: &ResolvedScript,
    entity_ctx: &DiagnosticEntityContext,
) -> bool {
    let tracker_errored = world
        .get_resource::<ScriptFileTracker>()
        .is_some_and(|t| t.is_errored(&resolved.key));
    if tracker_errored {
        return false;
    }

    if backend.compiled_info(&resolved.key).is_some() {
        // Ensure disk-backed scripts are tracked even if precompiled.
        if let Some(abs_path) = &resolved.abs_path {
            ensure_tracked_disk_file(world, resolved, abs_path, backend.compiled_info(&resolved.key).map(|i| i.content_hash));
        }
        return true;
    }

    let Some(abs_path) = &resolved.abs_path else {
        let diag = world.resource_mut::<ScriptDiagnostics>().record(
            ScriptDiagnosticSeverity::Error,
            ScriptDiagnosticStage::Load,
            resolved.source_label.clone(),
            None,
            None,
            entity_ctx,
            None,
            format!("script '{}' could not be resolved", resolved.source_label),
        );
        world.trigger(ScriptDiagnosticEvent(diag));
        return false;
    };

    if let Some(parent) = abs_path.parent()
        && let Some(mut watcher) = world.get_resource_mut::<ScriptHotReloadWatcher>()
    {
        watcher.watch_dir(parent);
    }

    let source_code = match fs::read_to_string(abs_path) {
        Ok(text) => text,
        Err(err) => {
            let hash_marker = format!("io-error:{err}");
            let already_reported = world
                .get_resource::<ScriptFileTracker>()
                .and_then(|t| t.get(&resolved.key))
                .and_then(|e| e.last_failed_hash.as_deref())
                == Some(hash_marker.as_str());

            let entry = TrackedScriptFile {
                key: resolved.key.clone(),
                abs_path: abs_path.clone(),
                source_label: resolved.source_label.clone(),
                last_mtime: None,
                last_hash: None,
                last_failed_hash: Some(hash_marker),
                has_compile_error: true,
            };
            world
                .resource_mut::<ScriptFileTracker>()
                .files
                .insert(resolved.key.clone(), entry);

            if !already_reported {
                let diag = world.resource_mut::<ScriptDiagnostics>().record(
                    ScriptDiagnosticSeverity::Error,
                    ScriptDiagnosticStage::Load,
                    resolved.source_label.clone(),
                    None,
                    None,
                    entity_ctx,
                    None,
                    format!("failed to read '{}': {err}", abs_path.display()),
                );
                world.trigger(ScriptDiagnosticEvent(diag));
            }
            return false;
        }
    };

    let mtime = fs::metadata(abs_path).ok().and_then(|m| m.modified().ok());
    let content_hash = blake3::hash(source_code.as_bytes()).to_hex().to_string();

    match backend.compile_source(&resolved.key, &resolved.source_label, &source_code, mtime) {
        Ok(info) => {
            world.resource_mut::<ScriptFileTracker>().files.insert(
                resolved.key.clone(),
                TrackedScriptFile {
                    key: resolved.key.clone(),
                    abs_path: abs_path.clone(),
                    source_label: resolved.source_label.clone(),
                    last_mtime: mtime,
                    last_hash: Some(info.content_hash),
                    last_failed_hash: None,
                    has_compile_error: false,
                },
            );
            true
        }
        Err(err) => {
            let (line, column, message) = match err {
                ScriptError::Compile {
                    line,
                    column,
                    message,
                    ..
                } => (line, column, message),
                other => (None, None, other.to_string()),
            };

            let already_reported = world
                .get_resource::<ScriptFileTracker>()
                .and_then(|t| t.get(&resolved.key))
                .and_then(|e| e.last_failed_hash.as_deref())
                == Some(content_hash.as_str());

            world.resource_mut::<ScriptFileTracker>().files.insert(
                resolved.key.clone(),
                TrackedScriptFile {
                    key: resolved.key.clone(),
                    abs_path: abs_path.clone(),
                    source_label: resolved.source_label.clone(),
                    last_mtime: mtime,
                    last_hash: None,
                    last_failed_hash: Some(content_hash),
                    has_compile_error: true,
                },
            );

            if !already_reported {
                let diag = world.resource_mut::<ScriptDiagnostics>().record(
                    ScriptDiagnosticSeverity::Error,
                    ScriptDiagnosticStage::Compile,
                    resolved.source_label.clone(),
                    line,
                    column,
                    entity_ctx,
                    None,
                    message,
                );
                world.trigger(ScriptDiagnosticEvent(diag));
            }
            false
        }
    }
}

fn ensure_tracked_disk_file(
    world: &mut World,
    resolved: &ResolvedScript,
    abs_path: &Path,
    content_hash: Option<String>,
) {
    let already_tracked = world
        .get_resource::<ScriptFileTracker>()
        .is_some_and(|t| t.files.contains_key(&resolved.key));
    if already_tracked {
        return;
    }
    if let Some(parent) = abs_path.parent()
        && let Some(mut watcher) = world.get_resource_mut::<ScriptHotReloadWatcher>()
    {
        watcher.watch_dir(parent);
    }
    let mtime = fs::metadata(abs_path).ok().and_then(|m| m.modified().ok());
    if let Some(mut tracker) = world.get_resource_mut::<ScriptFileTracker>() {
        tracker.files.insert(
            resolved.key.clone(),
            TrackedScriptFile {
                key: resolved.key.clone(),
                abs_path: abs_path.to_path_buf(),
                source_label: resolved.source_label.clone(),
                last_mtime: mtime,
                last_hash: content_hash,
                last_failed_hash: None,
                has_compile_error: false,
            },
        );
    }
}

/// Checks all tracked `.rhai` scripts (and any paths flagged by the OS file watcher) and
/// recompiles modified scripts on disk while preserving [`ScriptComponent::properties`].
///
/// Returns the number of scripts successfully recompiled during this sweep.
pub fn reload_modified_scripts(world: &mut World) -> usize {
    let config = world
        .get_resource::<ScriptHotReloadConfig>()
        .copied()
        .unwrap_or_default();
    if !config.enabled {
        return 0;
    }

    let watcher_paths = world
        .get_resource::<ScriptHotReloadWatcher>()
        .map(ScriptHotReloadWatcher::drain_changed_paths)
        .unwrap_or_default();

    let now = Instant::now();
    let should_poll = {
        let Some(tracker) = world.get_resource::<ScriptFileTracker>() else {
            return 0;
        };
        if tracker.files.is_empty() {
            return 0;
        }
        !watcher_paths.is_empty()
            || config.poll_interval.is_zero()
            || tracker
                .last_poll
                .is_none_or(|last| now.saturating_duration_since(last) >= config.poll_interval)
    };

    if !should_poll {
        return 0;
    }

    if let Some(mut tracker) = world.get_resource_mut::<ScriptFileTracker>() {
        tracker.last_poll = Some(now);
    }

    // Refresh relocated AssetUuid paths from AssetRegistry if files were moved.
    let tracked_snapshot: Vec<TrackedScriptFile> = {
        let registry = world.get_resource::<AssetRegistry>().cloned();
        let mut tracker = world.resource_mut::<ScriptFileTracker>();
        for entry in tracker.files.values_mut() {
            if let ScriptSourceKey::Asset(uuid) = entry.key
                && let Some(ref reg) = registry
                && let Some(new_abs) = reg.resolve_abs_path(uuid)
            {
                entry.abs_path = new_abs;
                if let Some(rel) = reg.shared().resolve_path(uuid) {
                    entry.source_label = rel;
                }
            }
        }
        tracker.files.values().cloned().collect()
    };

    let Some(mut runtime) = world.remove_resource::<ScriptRuntime>() else {
        return 0;
    };

    let mut reloaded_count = 0usize;

    for tracked in tracked_snapshot {
        let _ = &watcher_paths;
        let Ok(source_code) = fs::read_to_string(&tracked.abs_path) else {
            // File may be mid-save/atomic-replace; keep current state until readable.
            continue;
        };
        let mtime = fs::metadata(&tracked.abs_path)
            .ok()
            .and_then(|m| m.modified().ok());
        let hash = blake3::hash(source_code.as_bytes()).to_hex().to_string();

        if !tracked.has_compile_error && tracked.last_hash.as_deref() == Some(hash.as_str()) {
            continue;
        }
        if tracked.has_compile_error && tracked.last_failed_hash.as_deref() == Some(hash.as_str()) {
            continue;
        }

        match runtime.backend.compile_source(
            &tracked.key,
            &tracked.source_label,
            &source_code,
            mtime,
        ) {
            Ok(info) => {
                if let Some(mut tracker) = world.get_resource_mut::<ScriptFileTracker>() {
                    if let Some(entry) = tracker.files.get_mut(&tracked.key) {
                        entry.last_mtime = mtime;
                        entry.last_hash = Some(info.content_hash.clone());
                        entry.last_failed_hash = None;
                        entry.has_compile_error = false;
                    }
                    tracker.reload_count = tracker.reload_count.saturating_add(1);
                }

                tracing::info!(
                    target: "engine_scripting",
                    file = %tracked.source_label,
                    revision = info.revision,
                    "Hot-reloaded script"
                );
                world.trigger(ScriptReloadedEvent {
                    key: tracked.key.clone(),
                    file: tracked.source_label.clone(),
                    revision: info.revision,
                    content_hash: info.content_hash,
                });
                reloaded_count += 1;
            }
            Err(err) => {
                let (line, column, message) = match err {
                    ScriptError::Compile {
                        line,
                        column,
                        message,
                        ..
                    } => (line, column, message),
                    other => (None, None, other.to_string()),
                };

                if let Some(mut tracker) = world.get_resource_mut::<ScriptFileTracker>()
                    && let Some(entry) = tracker.files.get_mut(&tracked.key)
                {
                    entry.last_mtime = mtime;
                    entry.last_failed_hash = Some(hash);
                    entry.has_compile_error = true;
                }

                let entity_ctx = find_entity_context_for_script(world, runtime.backend.as_ref(), &tracked.key);
                let diag = world.resource_mut::<ScriptDiagnostics>().record(
                    ScriptDiagnosticSeverity::Error,
                    ScriptDiagnosticStage::Reload,
                    tracked.source_label.clone(),
                    line,
                    column,
                    &entity_ctx,
                    None,
                    message,
                );
                world.trigger(ScriptDiagnosticEvent(diag));
            }
        }
    }

    world.insert_resource(runtime);
    reloaded_count
}

fn find_entity_context_for_script(
    world: &World,
    backend: &dyn ScriptBackend,
    key: &ScriptSourceKey,
) -> DiagnosticEntityContext {
    let mut matches: Vec<Entity> = world
        .archetypes()
        .iter()
        .flat_map(|arch| arch.entities())
        .map(|ae| ae.id())
        .filter(|&entity| {
            world.get::<ScriptComponent>(entity).is_some_and(|comp| {
                resolve_script_source(world, backend, comp)
                    .is_some_and(|resolved| resolved.key == *key)
            })
        })
        .collect();
    matches.sort_unstable();
    let Some(entity) = matches.first().copied() else {
        return DiagnosticEntityContext::default();
    };
    DiagnosticEntityContext {
        entity: Some(entity),
        entity_name: world.get::<Name>(entity).map(|n| n.as_str().to_owned()),
        entity_guid: world.get::<EntityGuid>(entity).copied(),
    }
}

/// Exclusive Bevy system polling modified `.rhai` scripts for hot-reload.
pub fn poll_script_hot_reload_system(world: &mut World) {
    let _ = reload_modified_scripts(world);
}
