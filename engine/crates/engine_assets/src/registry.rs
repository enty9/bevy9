//! Project asset index (`AssetRegistry`) with UUID resolution, dependency graphs, and safe refactoring.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, RwLock},
    time::UNIX_EPOCH,
};

use bevy_ecs::resource::Resource;
use engine_core::AssetUuid;
use engine_protocol::{
    AssetFolderEntry, AssetImportSettings, AssetKind, AssetListParams, AssetRecord,
    AssetRegistrySnapshot, GltfImportSettings, ImageImportSettings, TilemapImportSettings,
};
use walkdir::WalkDir;

use crate::{
    error::AssetError,
    meta::{
        extract_uuid_references_from_text_asset, inspect_gltf_sub_assets, is_ignored_file,
        load_or_create_meta, meta_path_for, save_meta_file, AssetMetaFile, CURRENT_META_VERSION,
    },
};

#[derive(Debug, Default)]
struct AssetRegistryInner {
    assets_dir: PathBuf,
    scenes_dir: Option<PathBuf>,
    folders: BTreeSet<String>,
    by_uuid: HashMap<AssetUuid, AssetRecord>,
    by_path: HashMap<String, AssetUuid>,
    dependents_by_uuid: HashMap<AssetUuid, BTreeSet<AssetUuid>>,
    external_dependents_by_uuid: HashMap<AssetUuid, Vec<String>>,
    cached_thumbnails: HashSet<AssetUuid>,
}

/// Thread-safe handle to the indexed project [`AssetRegistry`] state (used by `uuid://` reader & settings closures).
#[derive(Resource, Clone, Default, Debug)]
pub struct SharedAssetRegistry {
    inner: Arc<RwLock<AssetRegistryInner>>,
}

impl SharedAssetRegistry {
    /// Creates a new shared registry pointing at `assets_dir` and optional `scenes_dir`.
    #[must_use]
    pub fn new(assets_dir: PathBuf, scenes_dir: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(AssetRegistryInner {
                assets_dir,
                scenes_dir,
                ..Default::default()
            })),
        }
    }

    /// Returns the configured project `assets/` directory.
    #[must_use]
    pub fn assets_dir(&self) -> PathBuf {
        self.inner
            .read()
            .map_or_else(|_| PathBuf::from("assets"), |g| g.assets_dir.clone())
    }

    /// Resolves `uuid` to its normalized relative path inside `assets/`, if indexed.
    #[must_use]
    pub fn resolve_path(&self, uuid: AssetUuid) -> Option<String> {
        let guard = self.inner.read().ok()?;
        guard.by_uuid.get(&uuid).map(|r| r.relative_path.clone())
    }

    /// Resolves `uuid` to an absolute filesystem path on disk, if indexed.
    #[must_use]
    pub fn resolve_abs_path(&self, uuid: AssetUuid) -> Option<PathBuf> {
        let guard = self.inner.read().ok()?;
        let rec = guard.by_uuid.get(&uuid)?;
        Some(guard.assets_dir.join(Path::new(&rec.relative_path)))
    }

    /// Looks up a cloned [`AssetRecord`] by [`AssetUuid`].
    #[must_use]
    pub fn get(&self, uuid: AssetUuid) -> Option<AssetRecord> {
        self.inner.read().ok()?.by_uuid.get(&uuid).cloned()
    }

    /// Looks up a cloned [`AssetRecord`] by normalized relative path inside `assets/`.
    #[must_use]
    pub fn get_by_path(&self, relative_path: &str) -> Option<AssetRecord> {
        let normalized = relative_path.replace('\\', "/");
        let guard = self.inner.read().ok()?;
        let &uuid = guard.by_path.get(&normalized)?;
        guard.by_uuid.get(&uuid).cloned()
    }

    /// Returns image import settings stored for `uuid`, if it is an [`AssetKind::Image`].
    #[must_use]
    pub fn image_settings(&self, uuid: AssetUuid) -> Option<ImageImportSettings> {
        let rec = self.get(uuid)?;
        match rec.import_settings {
            AssetImportSettings::Image(settings) => Some(settings),
            _ => None,
        }
    }

    /// Returns glTF import settings stored for `uuid`, if it is an [`AssetKind::GltfModel`].
    #[must_use]
    pub fn gltf_settings(&self, uuid: AssetUuid) -> Option<GltfImportSettings> {
        let rec = self.get(uuid)?;
        match rec.import_settings {
            AssetImportSettings::Gltf(settings) => Some(settings),
            _ => None,
        }
    }

    /// Returns tilemap import settings stored for `uuid`, if it is an [`AssetKind::Tilemap`].
    #[must_use]
    pub fn tilemap_settings(&self, uuid: AssetUuid) -> Option<TilemapImportSettings> {
        let rec = self.get(uuid)?;
        match rec.import_settings {
            AssetImportSettings::Tilemap(settings) => Some(settings),
            _ => None,
        }
    }

    /// Marks `uuid` as having a cached thumbnail on disk.
    pub fn set_has_thumbnail(&self, uuid: AssetUuid, has_thumbnail: bool) {
        if let Ok(mut guard) = self.inner.write() {
            if has_thumbnail {
                guard.cached_thumbnails.insert(uuid);
            } else {
                guard.cached_thumbnails.remove(&uuid);
            }
            if let Some(record) = guard.by_uuid.get_mut(&uuid) {
                record.has_thumbnail = has_thumbnail;
            }
        }
    }

    /// Returns reverse dependents (asset UUIDs + scene descriptions) referencing `uuid`.
    #[must_use]
    pub fn describe_dependents(&self, uuid: AssetUuid) -> Vec<String> {
        let Ok(guard) = self.inner.read() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(asset_deps) = guard.dependents_by_uuid.get(&uuid) {
            for dep_uuid in asset_deps {
                if let Some(rec) = guard.by_uuid.get(dep_uuid) {
                    out.push(format!("{} ({})", rec.relative_path, dep_uuid));
                } else {
                    out.push(dep_uuid.to_string());
                }
            }
        }
        if let Some(ext) = guard.external_dependents_by_uuid.get(&uuid) {
            out.extend(ext.iter().cloned());
        }
        out
    }
}

/// Result summary produced when scanning the workspace asset folder.
#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    /// Number of indexed assets after scan.
    pub total_assets: usize,
    /// Relative paths of existing assets whose content hash changed since the previous scan.
    pub modified_paths: Vec<String>,
    /// Whether any asset was added, removed, or modified compared to the previous index state.
    pub changed: bool,
}

/// ECS resource maintaining the active project asset index, sidecars, dependencies, and change notifications.
#[derive(Resource, Clone, Debug)]
pub struct AssetRegistry {
    shared: SharedAssetRegistry,
    /// Monotonically incremented whenever the asset index or thumbnail readiness changes.
    pub revision: u64,
    /// Set `true` when connected UI clients need an `AssetsChanged` IPC notification.
    pub dirty: bool,
    /// Assets moved or renamed since last scene re-hydration flush.
    pub relocated_uuids: Vec<AssetUuid>,
    /// Assets whose source bytes or import settings changed and should re-hydrate live components.
    pub reimport_uuids: Vec<AssetUuid>,
}

impl Default for AssetRegistry {
    fn default() -> Self {
        Self::from_shared(SharedAssetRegistry::new(PathBuf::from("assets"), None))
    }
}

impl AssetRegistry {
    /// Creates an [`AssetRegistry`] rooted at `assets_dir` with optional `scenes_dir` dependency scanning.
    #[must_use]
    pub fn new(assets_dir: PathBuf, scenes_dir: Option<PathBuf>) -> Self {
        Self::from_shared(SharedAssetRegistry::new(assets_dir, scenes_dir))
    }

    /// Wraps an already created [`SharedAssetRegistry`] handle.
    #[must_use]
    pub fn from_shared(shared: SharedAssetRegistry) -> Self {
        Self {
            shared,
            revision: 0,
            dirty: false,
            relocated_uuids: Vec::new(),
            reimport_uuids: Vec::new(),
        }
    }

    /// Returns a thread-safe cloneable handle to the underlying shared registry store.
    #[must_use]
    pub fn shared(&self) -> SharedAssetRegistry {
        self.shared.clone()
    }

    /// Returns the root `assets/` path monitored by this registry.
    #[must_use]
    pub fn assets_dir(&self) -> PathBuf {
        self.shared.assets_dir()
    }

    /// Resolves `uuid` to a relative asset path inside `assets/`.
    #[must_use]
    pub fn resolve_path(&self, uuid: AssetUuid) -> Option<String> {
        self.shared.resolve_path(uuid)
    }

    /// Resolves `uuid` to an absolute path on disk.
    #[must_use]
    pub fn resolve_abs_path(&self, uuid: AssetUuid) -> Option<PathBuf> {
        self.shared.resolve_abs_path(uuid)
    }

    /// Looks up an indexed asset record by [`AssetUuid`].
    #[must_use]
    pub fn get(&self, uuid: AssetUuid) -> Option<AssetRecord> {
        self.shared.get(uuid)
    }

    /// Looks up an indexed asset record by relative path inside `assets/`.
    #[must_use]
    pub fn get_by_path(&self, relative_path: &str) -> Option<AssetRecord> {
        self.shared.get_by_path(relative_path)
    }

    /// Returns image import settings for `uuid`, if available.
    #[must_use]
    pub fn image_settings(&self, uuid: AssetUuid) -> Option<ImageImportSettings> {
        self.shared.image_settings(uuid)
    }

    /// Returns glTF import settings for `uuid`, if available.
    #[must_use]
    pub fn gltf_settings(&self, uuid: AssetUuid) -> Option<GltfImportSettings> {
        self.shared.gltf_settings(uuid)
    }

    /// Returns outgoing UUID dependencies for `uuid`.
    #[must_use]
    pub fn dependencies_of(&self, uuid: AssetUuid) -> Vec<AssetUuid> {
        self.shared
            .get(uuid)
            .map(|rec| rec.dependencies)
            .unwrap_or_default()
    }

    /// Returns reverse UUID dependents inside `assets/` for `uuid`.
    #[must_use]
    pub fn dependents_of(&self, uuid: AssetUuid) -> Vec<AssetUuid> {
        self.shared
            .get(uuid)
            .map(|rec| rec.dependents)
            .unwrap_or_default()
    }

    /// Marks thumbnail availability for `uuid` and bumps registry revision when newly ready.
    pub fn mark_thumbnail_ready(&mut self, uuid: AssetUuid) {
        let already = self
            .shared
            .get(uuid)
            .is_some_and(|rec| rec.has_thumbnail);
        self.shared.set_has_thumbnail(uuid, true);
        if !already {
            self.bump_dirty();
        }
    }

    /// Performs a full recursive scan of `assets/` (and optional `scenes_dir`), syncing `.b9meta` sidecars and dependency graphs.
    pub fn scan_all(&mut self) -> Result<ScanReport, AssetError> {
        let (assets_dir, scenes_dir, previous_hashes, previous_keys) = {
            let guard = self
                .shared
                .inner
                .read()
                .map_err(|_| AssetError::InvalidData("AssetRegistry lock poisoned".to_owned()))?;
            let hashes: HashMap<AssetUuid, String> = guard
                .by_uuid
                .iter()
                .map(|(&u, r)| (u, r.source_hash.clone()))
                .collect();
            let keys: HashMap<AssetUuid, String> = guard
                .by_uuid
                .iter()
                .map(|(&u, r)| (u, r.relative_path.clone()))
                .collect();
            (
                guard.assets_dir.clone(),
                guard.scenes_dir.clone(),
                hashes,
                keys,
            )
        };

        fs::create_dir_all(&assets_dir).map_err(|source| AssetError::Io {
            path: assets_dir.clone(),
            source,
        })?;

        let mut folders = BTreeSet::new();
        let mut by_uuid = HashMap::new();
        let mut by_path = HashMap::new();
        let mut modified_paths = Vec::new();
        let mut modified_uuids = Vec::new();

        let existing_thumbnails: HashSet<AssetUuid> = self
            .shared
            .inner
            .read()
            .map(|g| g.cached_thumbnails.clone())
            .unwrap_or_default();

        for entry in WalkDir::new(&assets_dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !should_skip_walk_entry(e.path(), &assets_dir))
            .flatten()
        {
            let path = entry.path();
            if path == assets_dir {
                continue;
            }
            let Ok(rel) = path.strip_prefix(&assets_dir) else {
                continue;
            };
            let rel_norm = normalize_rel_path(rel);
            if rel_norm.is_empty() {
                continue;
            }

            if entry.file_type().is_dir() {
                folders.insert(rel_norm);
                continue;
            }
            if !entry.file_type().is_file() || is_ignored_file(path) {
                continue;
            }

            let (meta, _meta_written) = load_or_create_meta(path)?;
            let fs_meta = fs::metadata(path).map_err(|source| AssetError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            let modified_ms = fs_meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as u64);

            let sub_assets = if meta.kind == AssetKind::GltfModel {
                inspect_gltf_sub_assets(path)
            } else {
                Vec::new()
            };

            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| rel_norm.clone());

            let hash_changed = previous_hashes
                .get(&meta.uuid)
                .is_some_and(|prev| prev != &meta.source_hash);
            if hash_changed {
                modified_paths.push(rel_norm.clone());
                modified_uuids.push(meta.uuid);
            }

            let has_thumbnail = existing_thumbnails.contains(&meta.uuid) && !hash_changed;

            let record = AssetRecord {
                uuid: meta.uuid,
                relative_path: rel_norm.clone(),
                name,
                kind: meta.kind,
                file_size: fs_meta.len(),
                modified_ms,
                source_hash: meta.source_hash,
                tags: meta.tags,
                dependencies: meta.dependencies,
                dependents: Vec::new(),
                sub_assets,
                import_settings: meta.import_settings,
                has_thumbnail,
            };

            by_path.insert(rel_norm, meta.uuid);
            by_uuid.insert(meta.uuid, record);
        }

        // Compute outgoing + reverse dependencies across assets_dir and external scenes_dir.
        let known_uuids: HashSet<AssetUuid> = by_uuid.keys().copied().collect();
        let mut dependents_by_uuid: HashMap<AssetUuid, BTreeSet<AssetUuid>> = HashMap::new();

        let asset_uuids: Vec<AssetUuid> = by_uuid.keys().copied().collect();
        for asset_uuid in asset_uuids {
            let rel_path = by_uuid[&asset_uuid].relative_path.clone();
            let abs_path = assets_dir.join(Path::new(&rel_path));
            let mut deps: BTreeSet<AssetUuid> = by_uuid[&asset_uuid]
                .dependencies
                .iter()
                .copied()
                .filter(|d| *d != asset_uuid && known_uuids.contains(d))
                .collect();
            for scanned_dep in extract_uuid_references_from_text_asset(&abs_path, &known_uuids) {
                if scanned_dep != asset_uuid {
                    deps.insert(scanned_dep);
                }
            }
            let dep_vec: Vec<AssetUuid> = deps.iter().copied().collect();
            for dep in &dep_vec {
                dependents_by_uuid
                    .entry(*dep)
                    .or_default()
                    .insert(asset_uuid);
            }
            if let Some(rec) = by_uuid.get_mut(&asset_uuid) {
                rec.dependencies = dep_vec;
            }
        }

        let mut external_dependents_by_uuid: HashMap<AssetUuid, Vec<String>> = HashMap::new();
        if let Some(scenes_root) = scenes_dir.as_ref()
            && scenes_root.exists()
        {
            for entry in WalkDir::new(scenes_root)
                .follow_links(false)
                .into_iter()
                .flatten()
            {
                let path = entry.path();
                if !entry.file_type().is_file() || is_ignored_file(path) {
                    continue;
                }
                let refs = extract_uuid_references_from_text_asset(path, &known_uuids);
                if refs.is_empty() {
                    continue;
                }
                let label = path
                    .strip_prefix(scenes_root)
                    .map(normalize_rel_path)
                    .unwrap_or_else(|_| path.display().to_string());
                let scene_label = format!("scene:{label}");
                for ref_uuid in refs {
                    external_dependents_by_uuid
                        .entry(ref_uuid)
                        .or_default()
                        .push(scene_label.clone());
                }
            }
        }

        for (&uuid, deps) in &dependents_by_uuid {
            if let Some(rec) = by_uuid.get_mut(&uuid) {
                rec.dependents = deps.iter().copied().collect();
            }
        }

        let current_keys: HashMap<AssetUuid, String> = by_uuid
            .iter()
            .map(|(&u, r)| (u, r.relative_path.clone()))
            .collect();
        let current_hashes: HashMap<AssetUuid, String> = by_uuid
            .iter()
            .map(|(&u, r)| (u, r.source_hash.clone()))
            .collect();
        let changed = previous_keys != current_keys || previous_hashes != current_hashes;
        let total_assets = by_uuid.len();

        {
            let mut guard = self
                .shared
                .inner
                .write()
                .map_err(|_| AssetError::InvalidData("AssetRegistry lock poisoned".to_owned()))?;
            guard.folders = folders;
            guard.by_uuid = by_uuid;
            guard.by_path = by_path;
            guard.dependents_by_uuid = dependents_by_uuid;
            guard.external_dependents_by_uuid = external_dependents_by_uuid;
            for uuid in &modified_uuids {
                guard.cached_thumbnails.remove(uuid);
            }
        }

        for uuid in modified_uuids {
            if !self.reimport_uuids.contains(&uuid) {
                self.reimport_uuids.push(uuid);
            }
        }
        if changed {
            self.bump_dirty();
        }

        Ok(ScanReport {
            total_assets,
            modified_paths,
            changed,
        })
    }

    /// Returns a filtered directory & asset snapshot for the editor Content Browser.
    #[must_use]
    pub fn snapshot(&self, params: &AssetListParams) -> AssetRegistrySnapshot {
        let Ok(guard) = self.shared.inner.read() else {
            return AssetRegistrySnapshot {
                root_dir: "assets".to_owned(),
                folders: Vec::new(),
                assets: Vec::new(),
            };
        };

        let folder_filter = params
            .folder
            .as_deref()
            .map(trim_rel_slashes)
            .filter(|s| !s.is_empty());
        let search_needle = params
            .search
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase);
        let tag_filter = params
            .tag
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let folders: Vec<AssetFolderEntry> = guard
            .folders
            .iter()
            .map(|rel| {
                let name = rel.rsplit('/').next().unwrap_or(rel).to_owned();
                AssetFolderEntry {
                    relative_path: rel.clone(),
                    name,
                }
            })
            .collect();

        let mut assets: Vec<AssetRecord> = guard
            .by_uuid
            .values()
            .filter(|rec| {
                if let Some(kind) = params.kind
                    && rec.kind != kind
                {
                    return false;
                }
                if let Some(prefix) = folder_filter.as_deref() {
                    let prefix_slash = format!("{prefix}/");
                    if !rec.relative_path.starts_with(&prefix_slash) {
                        return false;
                    }
                }
                if let Some(tag) = tag_filter
                    && !rec.tags.iter().any(|t| t == tag)
                {
                    return false;
                }
                if let Some(needle) = search_needle.as_deref() {
                    let matched_name = rec.name.to_ascii_lowercase().contains(needle);
                    let matched_path = rec.relative_path.to_ascii_lowercase().contains(needle);
                    let matched_tag = rec
                        .tags
                        .iter()
                        .any(|t| t.to_ascii_lowercase().contains(needle));
                    if !matched_name && !matched_path && !matched_tag {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();

        assets.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

        AssetRegistrySnapshot {
            root_dir: guard.assets_dir.display().to_string(),
            folders,
            assets,
        }
    }

    /// Creates a directory inside `assets/` at `relative_path`.
    pub fn create_folder(&mut self, relative_path: &str) -> Result<AssetFolderEntry, AssetError> {
        let rel = sanitize_relative_path(relative_path)?;
        let assets_dir = self.assets_dir();
        let abs = assets_dir.join(Path::new(&rel));
        fs::create_dir_all(&abs).map_err(|source| AssetError::Io {
            path: abs,
            source,
        })?;
        self.scan_all()?;
        let name = rel.rsplit('/').next().unwrap_or(&rel).to_owned();
        Ok(AssetFolderEntry {
            relative_path: rel,
            name,
        })
    }

    /// Copies external source files into `assets/<target_folder>`, generates `.b9meta` sidecars, and indexes them.
    pub fn import_external_files(
        &mut self,
        sources: &[PathBuf],
        target_folder: &str,
    ) -> Result<Vec<AssetRecord>, AssetError> {
        let assets_dir = self.assets_dir();
        let clean_folder = trim_rel_slashes(target_folder);
        let dest_dir = if clean_folder.is_empty() {
            assets_dir.clone()
        } else {
            let safe_rel = sanitize_relative_path(&clean_folder)?;
            assets_dir.join(Path::new(&safe_rel))
        };
        fs::create_dir_all(&dest_dir).map_err(|source| AssetError::Io {
            path: dest_dir.clone(),
            source,
        })?;

        let mut imported_rels = Vec::new();
        for src in sources {
            if !src.is_file() {
                return Err(AssetError::InvalidData(format!(
                    "Import source '{}' does not exist or is not a file",
                    src.display()
                )));
            }
            let file_name = src
                .file_name()
                .ok_or_else(|| AssetError::InvalidData("Import source has no filename".to_owned()))?;
            let dest_abs = unique_destination_path(&dest_dir, &file_name.to_string_lossy());
            fs::copy(src, &dest_abs).map_err(|source| AssetError::Io {
                path: dest_abs.clone(),
                source,
            })?;
            load_or_create_meta(&dest_abs)?;
            let rel = normalize_rel_path(
                dest_abs
                    .strip_prefix(&assets_dir)
                    .unwrap_or(dest_abs.as_path()),
            );
            imported_rels.push(rel);
        }

        self.scan_all()?;
        let mut records = Vec::with_capacity(imported_rels.len());
        for rel in imported_rels {
            if let Some(rec) = self.get_by_path(&rel) {
                records.push(rec);
            }
        }
        Ok(records)
    }

    /// Moves or renames an asset and its `*.b9meta` sidecar while preserving its [`AssetUuid`].
    pub fn move_or_rename(
        &mut self,
        uuid: AssetUuid,
        new_relative_path: &str,
    ) -> Result<AssetRecord, AssetError> {
        let clean_new_rel = sanitize_relative_path(new_relative_path)?;
        let assets_dir = self.assets_dir();
        let existing = self.get(uuid).ok_or(AssetError::NotFound(uuid))?;

        if existing.relative_path == clean_new_rel {
            return Ok(existing);
        }
        if self.get_by_path(&clean_new_rel).is_some() {
            return Err(AssetError::DestinationExists(clean_new_rel));
        }

        let old_abs = assets_dir.join(Path::new(&existing.relative_path));
        let new_abs = assets_dir.join(Path::new(&clean_new_rel));
        if new_abs.exists() {
            return Err(AssetError::DestinationExists(clean_new_rel));
        }

        if let Some(parent) = new_abs.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|source| AssetError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        move_file_with_fallback(&old_abs, &new_abs)?;

        let old_b9meta = meta_path_for(&old_abs);
        let new_b9meta = meta_path_for(&new_abs);
        if old_b9meta.exists() {
            move_file_with_fallback(&old_b9meta, &new_b9meta)?;
        }

        // Remove stale Bevy processor `.meta` at the old location so AssetProcessor re-emits cleanly.
        let mut old_bevy_meta_os = old_abs.as_os_str().to_os_string();
        old_bevy_meta_os.push(".meta");
        let old_bevy_meta = PathBuf::from(old_bevy_meta_os);
        if old_bevy_meta.exists() {
            let _ = fs::remove_file(old_bevy_meta);
        }

        self.scan_all()?;
        if !self.relocated_uuids.contains(&uuid) {
            self.relocated_uuids.push(uuid);
        }
        self.bump_dirty();

        self.get(uuid).ok_or(AssetError::NotFound(uuid))
    }

    /// Updates importer settings and optional tags stored inside the `*.b9meta` sidecar of `uuid`.
    pub fn update_import_settings(
        &mut self,
        uuid: AssetUuid,
        import_settings: AssetImportSettings,
        tags: Option<Vec<String>>,
    ) -> Result<AssetRecord, AssetError> {
        let existing = self.get(uuid).ok_or(AssetError::NotFound(uuid))?;
        let abs_path = self.assets_dir().join(Path::new(&existing.relative_path));
        let meta = AssetMetaFile {
            schema_version: CURRENT_META_VERSION,
            uuid,
            kind: existing.kind,
            source_hash: existing.source_hash.clone(),
            tags: tags.unwrap_or(existing.tags),
            dependencies: existing.dependencies,
            import_settings,
        };
        save_meta_file(&abs_path, &meta)?;
        self.scan_all()?;
        if !self.reimport_uuids.contains(&uuid) {
            self.reimport_uuids.push(uuid);
        }
        self.bump_dirty();
        self.get(uuid).ok_or(AssetError::NotFound(uuid))
    }

    /// Deletes an indexed asset and its `*.b9meta` sidecar, checking reverse dependents unless `force` is `true`.
    pub fn delete_asset(
        &mut self,
        uuid: AssetUuid,
        force: bool,
    ) -> Result<Vec<AssetUuid>, AssetError> {
        let existing = self.get(uuid).ok_or(AssetError::NotFound(uuid))?;
        let desc_dependents = self.shared.describe_dependents(uuid);
        let asset_dependents = existing.dependents.clone();

        if !force && !desc_dependents.is_empty() {
            return Err(AssetError::HasDependents {
                uuid,
                dependents: desc_dependents,
            });
        }

        let abs_path = self.assets_dir().join(Path::new(&existing.relative_path));
        if abs_path.exists() {
            fs::remove_file(&abs_path).map_err(|source| AssetError::Io {
                path: abs_path.clone(),
                source,
            })?;
        }
        let b9meta = meta_path_for(&abs_path);
        if b9meta.exists() {
            let _ = fs::remove_file(&b9meta);
        }

        self.shared.set_has_thumbnail(uuid, false);
        self.scan_all()?;
        self.bump_dirty();
        Ok(asset_dependents)
    }

    fn bump_dirty(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.dirty = true;
    }
}

fn trim_rel_slashes(raw: &str) -> String {
    raw.replace('\\', "/").trim_matches('/').to_owned()
}

fn normalize_rel_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").trim_matches('/').to_owned()
}

fn sanitize_relative_path(raw: &str) -> Result<String, AssetError> {
    let replaced = raw.replace('\\', "/");
    let trimmed = replaced.trim().trim_start_matches('/');
    if trimmed.is_empty() {
        return Err(AssetError::InvalidRelativePath(raw.to_owned()));
    }
    let path = Path::new(trimmed);
    let mut parts = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Normal(seg) => {
                let s = seg.to_string_lossy();
                if s.is_empty() {
                    continue;
                }
                parts.push(s.into_owned());
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(AssetError::InvalidRelativePath(raw.to_owned()));
            }
        }
    }
    if parts.is_empty() {
        return Err(AssetError::InvalidRelativePath(raw.to_owned()));
    }
    Ok(parts.join("/"))
}

fn should_skip_walk_entry(path: &Path, root: &Path) -> bool {
    if path == root {
        return false;
    }
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| name == ".cache" || name == "imported_assets" || name == "target" || name.starts_with('.'))
}

fn unique_destination_path(dest_dir: &Path, filename: &str) -> PathBuf {
    let candidate = dest_dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("asset");
    let ext = path.extension().and_then(|s| s.to_str());

    let mut counter = 1u32;
    loop {
        let next_name = match ext {
            Some(e) => format!("{stem}_{counter}.{e}"),
            None => format!("{stem}_{counter}"),
        };
        let next = dest_dir.join(next_name);
        if !next.exists() {
            return next;
        }
        counter += 1;
    }
}

fn move_file_with_fallback(from: &Path, to: &Path) -> Result<(), AssetError> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to).map_err(|source| AssetError::Io {
        path: to.to_path_buf(),
        source,
    })?;
    fs::remove_file(from).map_err(|source| AssetError::Io {
        path: from.to_path_buf(),
        source,
    })
}
