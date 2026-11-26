//! Sidecar `*.b9meta` persistence, BLAKE3 content hashing, sub-asset inspection, and UUID reference scanning.

use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use engine_core::AssetUuid;
use engine_protocol::{AssetImportSettings, AssetKind, SubAssetInfo};
use ron::ser::PrettyConfig;
use serde::{Deserialize, Serialize};

use crate::error::AssetError;

/// File extension appended to assets for engine metadata sidecars (`foo.png` -> `foo.png.b9meta`).
///
/// Kept separate from `.meta` so Bevy `AssetProcessor` metadata files never conflict.
pub const SIDECAR_EXTENSION: &str = "b9meta";

/// Supported `*.b9meta` schema format version.
pub const CURRENT_META_VERSION: u32 = 1;

/// On-disk sidecar representation written alongside every indexed project asset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetMetaFile {
    /// Schema format version ([`CURRENT_META_VERSION`]).
    pub schema_version: u32,
    /// Persistent [`AssetUuid`] preserved across file moves and renames.
    pub uuid: AssetUuid,
    /// Classified asset kind.
    pub kind: AssetKind,
    /// BLAKE3 digest of the asset bytes on disk at last scan.
    pub source_hash: String,
    /// Optional user-defined search tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Outgoing asset UUID dependencies discovered or declared for this asset.
    #[serde(default)]
    pub dependencies: Vec<AssetUuid>,
    /// Importer settings specific to this asset type.
    #[serde(default)]
    pub import_settings: AssetImportSettings,
}

/// Computes the `*.b9meta` sidecar path corresponding to `asset_abs` (`a/b.png` -> `a/b.png.b9meta`).
#[must_use]
pub fn meta_path_for(asset_abs: &Path) -> PathBuf {
    let mut os = asset_abs.as_os_str().to_os_string();
    os.push(".");
    os.push(SIDECAR_EXTENSION);
    PathBuf::from(os)
}

/// Returns `true` for metadata files, placeholder files, hidden files, or cache directories.
#[must_use]
pub fn is_ignored_file(path: &Path) -> bool {
    for component in path.components() {
        let name = component.as_os_str().to_string_lossy();
        if name == ".cache" || name == "imported_assets" || name == "target" {
            return true;
        }
    }

    let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
        return true;
    };

    if file_name.starts_with('.') {
        return true;
    }

    let lower = file_name.to_ascii_lowercase();
    lower.ends_with(".b9meta")
        || lower.ends_with(".meta")
        || lower == ".gitkeep"
        || lower == "thumbs.db"
}

/// Determines the [`AssetKind`] from multi-extension or single-extension suffixes.
#[must_use]
pub fn infer_asset_kind(path: &Path) -> AssetKind {
    let lower = path
        .file_name()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    if lower.ends_with(".prefab.ron") {
        AssetKind::Prefab
    } else if lower.ends_with(".level.ron") {
        AssetKind::Scene
    } else if lower.ends_with(".tilemap.ron") {
        AssetKind::Tilemap
    } else if lower.ends_with(".mat.ron") {
        AssetKind::Material
    } else if lower.ends_with(".rhai") {
        AssetKind::Script
    } else if lower.ends_with(".glb") || lower.ends_with(".gltf") {
        AssetKind::GltfModel
    } else if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".webp")
        || lower.ends_with(".bmp")
        || lower.ends_with(".tga")
    {
        AssetKind::Image
    } else {
        AssetKind::Unknown
    }
}

/// Computes the hex-encoded BLAKE3 digest of a file on disk.
pub fn compute_file_blake3(path: &Path) -> Result<String, AssetError> {
    let mut file = fs::File::open(path).map_err(|source| AssetError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let read = file.read(&mut buf).map_err(|source| AssetError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Loads `*.b9meta` alongside `asset_abs`, or creates one with a fresh [`AssetUuid`].
///
/// Preserves existing [`AssetUuid`], `tags`, and compatible `import_settings` if the source
/// file hash changed, rewriting the sidecar automatically when needed. Returns `(meta, changed)`.
pub fn load_or_create_meta(asset_abs: &Path) -> Result<(AssetMetaFile, bool), AssetError> {
    let kind = infer_asset_kind(asset_abs);
    let source_hash = compute_file_blake3(asset_abs)?;
    let meta_path = meta_path_for(asset_abs);

    if meta_path.exists() {
        let raw = fs::read_to_string(&meta_path).map_err(|source| AssetError::Io {
            path: meta_path.clone(),
            source,
        })?;
        match ron::de::from_str::<AssetMetaFile>(&raw) {
            Ok(mut meta) => {
                let mut dirty = false;
                if meta.schema_version != CURRENT_META_VERSION {
                    meta.schema_version = CURRENT_META_VERSION;
                    dirty = true;
                }
                if meta.kind != kind {
                    meta.kind = kind;
                    meta.import_settings = AssetImportSettings::default_for_kind(kind);
                    dirty = true;
                }
                if meta.source_hash != source_hash {
                    meta.source_hash = source_hash;
                    dirty = true;
                }
                if dirty {
                    save_meta_file(asset_abs, &meta)?;
                }
                return Ok((meta, dirty));
            }
            Err(err) => {
                tracing::warn!(
                    path = %meta_path.display(),
                    error = %err,
                    "failed to parse existing .b9meta sidecar; regenerating fresh metadata"
                );
            }
        }
    }

    let meta = AssetMetaFile {
        schema_version: CURRENT_META_VERSION,
        uuid: AssetUuid::new(),
        kind,
        source_hash,
        tags: Vec::new(),
        dependencies: Vec::new(),
        import_settings: AssetImportSettings::default_for_kind(kind),
    };
    save_meta_file(asset_abs, &meta)?;
    Ok((meta, true))
}

/// Serializes and writes `meta` to the `*.b9meta` sidecar beside `asset_abs`.
pub fn save_meta_file(asset_abs: &Path, meta: &AssetMetaFile) -> Result<(), AssetError> {
    let meta_path = meta_path_for(asset_abs);
    let encoded = ron::ser::to_string_pretty(meta, PrettyConfig::default())?;
    fs::write(&meta_path, encoded).map_err(|source| AssetError::Io {
        path: meta_path,
        source,
    })
}

/// Inspects a `.glb` or `.gltf` file header/document and lists its scene, mesh, material, and animation labels.
#[must_use]
pub fn inspect_gltf_sub_assets(abs_path: &Path) -> Vec<SubAssetInfo> {
    let Ok(bytes) = fs::read(abs_path) else {
        return Vec::new();
    };
    let Ok(gltf) = gltf::Gltf::from_slice(&bytes) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for scene in gltf.scenes() {
        let idx = scene.index();
        out.push(SubAssetInfo {
            label: format!("Scene{idx}"),
            kind: "scene".to_owned(),
            name: scene
                .name()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("Scene {idx}")),
        });
    }
    for mesh in gltf.meshes() {
        let idx = mesh.index();
        out.push(SubAssetInfo {
            label: format!("Mesh{idx}"),
            kind: "mesh".to_owned(),
            name: mesh
                .name()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("Mesh {idx}")),
        });
    }
    for material in gltf.materials() {
        if let Some(idx) = material.index() {
            out.push(SubAssetInfo {
                label: format!("Material{idx}"),
                kind: "material".to_owned(),
                name: material
                    .name()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| format!("Material {idx}")),
            });
        }
    }
    for anim in gltf.animations() {
        let idx = anim.index();
        out.push(SubAssetInfo {
            label: format!("Animation{idx}"),
            kind: "animation".to_owned(),
            name: anim
                .name()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("Animation {idx}")),
        });
    }
    out
}

/// Scans text-based engine files (`.level.ron`, `.prefab.ron`, `.tilemap.ron`, `.mat.ron`, `.rhai`)
/// for canonical hyphenated UUID strings present in `known_uuids`.
#[must_use]
pub fn extract_uuid_references_from_text_asset(
    path: &Path,
    known_uuids: &HashSet<AssetUuid>,
) -> Vec<AssetUuid> {
    if known_uuids.is_empty() {
        return Vec::new();
    }
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !matches!(ext.as_str(), "ron" | "toml" | "json" | "rhai") {
        return Vec::new();
    }
    let Ok(meta) = fs::metadata(path) else {
        return Vec::new();
    };
    if meta.len() > 4 * 1024 * 1024 {
        return Vec::new();
    }
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let bytes = text.as_bytes();
    if bytes.len() < 36 {
        return found;
    }

    for i in 0..=bytes.len() - 36 {
        let slice = &bytes[i..i + 36];
        if slice[8] != b'-' || slice[13] != b'-' || slice[18] != b'-' || slice[23] != b'-' {
            continue;
        }
        if !slice
            .iter()
            .enumerate()
            .all(|(idx, &b)| matches!(idx, 8 | 13 | 18 | 23) || b.is_ascii_hexdigit())
        {
            continue;
        }
        if let Ok(candidate) = core::str::from_utf8(slice)
            && let Ok(uuid) = AssetUuid::parse_str(candidate)
            && known_uuids.contains(&uuid)
            && seen.insert(uuid)
        {
            found.push(uuid);
        }
    }

    found
}
