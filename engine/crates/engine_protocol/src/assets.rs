//! Shared asset registry metadata, import settings, and Content Browser contracts.

use engine_core::AssetUuid;
use serde::{Deserialize, Serialize};

/// Classification of an indexed project asset inside `AssetRegistry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    /// Raster 2D image / texture (`.png`, `.jpg`, `.webp`, `.bmp`, `.tga`).
    Image,
    /// 3D `glTF` / binary `.glb` scene asset.
    GltfModel,
    /// Reusable entity subtree prefab (`.prefab.ron`).
    Prefab,
    /// Authored level scene (`.level.ron`).
    Scene,
    /// Layered 2D tilemap asset (`.tilemap.ron`).
    Tilemap,
    /// Rhai game logic script (`.rhai`).
    Script,
    /// Saved material descriptor asset (`.mat.ron`).
    Material,
    /// Audio clip (`.wav`, `.ogg`, `.mp3`, `.flac`).
    Audio,
    /// Unrecognized or generic project file.
    #[default]
    Unknown,
}

/// Texture sampling filter mode applied when importing 2D images.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextureFilterMode {
    /// Smooth bilinear filtering.
    #[default]
    Linear,
    /// Crisp nearest-neighbor filtering (ideal for pixel art and sprite sheets).
    Nearest,
}

/// Sprite sheet grid slicing configuration attached to imported images.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtlasGridSettings {
    /// Width and height of each grid cell in pixels `[width, height]`.
    pub tile_size: [u32; 2],
    /// Number of columns across the sheet.
    pub columns: u32,
    /// Number of rows down the sheet.
    pub rows: u32,
    /// Horizontal and vertical spacing between tiles in pixels.
    #[serde(default)]
    pub padding: [u32; 2],
    /// Top-left grid margin offset in pixels.
    #[serde(default)]
    pub offset: [u32; 2],
}

/// Per-asset import settings for [`AssetKind::Image`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageImportSettings {
    /// Texture sampling filter mode (`linear` vs `nearest`).
    pub filter_mode: TextureFilterMode,
    /// Whether the image pixels use sRGB gamma encoding (enabled for color sprites/albedo).
    pub srgb: bool,
    /// Optional sprite-sheet grid definition used to generate `TextureAtlasLayout`.
    #[serde(default)]
    pub atlas_grid: Option<AtlasGridSettings>,
}

impl Default for ImageImportSettings {
    fn default() -> Self {
        Self {
            filter_mode: TextureFilterMode::Linear,
            srgb: true,
            atlas_grid: None,
        }
    }
}

/// Per-asset import settings for [`AssetKind::GltfModel`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GltfImportSettings {
    /// Uniform scale multiplier applied when spawning instances in scene hydration.
    pub scale: f32,
    /// Whether missing vertex normals/tangents should be considered for mesh preprocessing.
    pub generate_normals_and_tangents: bool,
    /// Import meshes stored inside the `glTF` container.
    pub load_meshes: bool,
    /// Import PBR materials stored inside the `glTF` container.
    pub load_materials: bool,
    /// Import embedded cameras inside the `glTF` scene.
    pub load_cameras: bool,
    /// Import embedded punctual lights (`KHR_lights_punctual`).
    pub load_lights: bool,
    /// Import skeletal/node animations inside the `glTF` file.
    pub load_animations: bool,
}

impl Default for GltfImportSettings {
    fn default() -> Self {
        Self {
            scale: 1.0,
            generate_normals_and_tangents: true,
            load_meshes: true,
            load_materials: true,
            load_cameras: false,
            load_lights: true,
            load_animations: true,
        }
    }
}

/// Per-asset import settings for [`AssetKind::Tilemap`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TilemapImportSettings {
    /// World units per tile pixel scaling multiplier.
    pub pixels_per_unit: f32,
}

impl Default for TilemapImportSettings {
    fn default() -> Self {
        Self {
            pixels_per_unit: 1.0,
        }
    }
}

/// Tagged union of importer-specific settings persisted inside `*.b9meta` sidecars.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssetImportSettings {
    /// Image texture / sprite sheet settings.
    Image(ImageImportSettings),
    /// 3D `glTF` / GLB settings.
    Gltf(GltfImportSettings),
    /// 2D tilemap settings.
    Tilemap(TilemapImportSettings),
    /// Generic asset with no specialized importer settings.
    #[default]
    Generic,
}

impl AssetImportSettings {
    /// Constructs default settings appropriate for `kind`.
    #[must_use]
    pub fn default_for_kind(kind: AssetKind) -> Self {
        match kind {
            AssetKind::Image => Self::Image(ImageImportSettings::default()),
            AssetKind::GltfModel => Self::Gltf(GltfImportSettings::default()),
            AssetKind::Tilemap => Self::Tilemap(TilemapImportSettings::default()),
            AssetKind::Prefab
            | AssetKind::Scene
            | AssetKind::Script
            | AssetKind::Material
            | AssetKind::Audio
            | AssetKind::Unknown => Self::Generic,
        }
    }
}

/// Extracted sub-asset summary inside a compound container such as `.glb`/`.gltf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubAssetInfo {
    /// Sub-asset label (`"Scene0"`, `"Mesh0"`, `"Material0"`, `"Animation0"`, etc.).
    pub label: String,
    /// Kind category (`"scene"`, `"mesh"`, `"material"`, `"animation"`).
    pub kind: String,
    /// Human-readable sub-asset display name.
    pub name: String,
}

/// Indexed record describing a single project asset in the Content Browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetRecord {
    /// Persistent [`AssetUuid`] independent of relative file location.
    pub uuid: AssetUuid,
    /// Path relative to the project `assets/` root using `/` separators.
    pub relative_path: String,
    /// Display filename without directory components.
    pub name: String,
    /// Asset kind classification.
    pub kind: AssetKind,
    /// Source file byte length on disk.
    pub file_size: u64,
    /// Source modification timestamp in UNIX epoch milliseconds.
    pub modified_ms: u64,
    /// BLAKE3 hex digest of the source file contents.
    pub source_hash: String,
    /// User-assigned searchable tags stored in the `.b9meta` sidecar.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Outgoing UUID dependencies referenced by this asset.
    #[serde(default)]
    pub dependencies: Vec<AssetUuid>,
    /// Reverse UUID dependencies pointing to this asset from assets or scenes.
    #[serde(default)]
    pub dependents: Vec<AssetUuid>,
    /// Extracted child sub-assets (e.g. `glTF` meshes/materials/animations).
    #[serde(default)]
    pub sub_assets: Vec<SubAssetInfo>,
    /// Importer configuration persisted inside `*.b9meta`.
    #[serde(default)]
    pub import_settings: AssetImportSettings,
    /// Whether a cached PNG thumbnail is already available on disk or in memory.
    #[serde(default)]
    pub has_thumbnail: bool,
}

/// Directory entry inside the project's `assets/` tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetFolderEntry {
    /// Folder path relative to `assets/` using `/` separators.
    pub relative_path: String,
    /// Base directory name.
    pub name: String,
}

/// Filtered directory & asset listing returned to the editor Content Browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetRegistrySnapshot {
    /// Absolute or display root path of the project's `assets/` directory.
    pub root_dir: String,
    /// All known folders inside `assets/`, sorted deterministically.
    pub folders: Vec<AssetFolderEntry>,
    /// Matching indexed asset records, sorted deterministically by relative path.
    pub assets: Vec<AssetRecord>,
}
