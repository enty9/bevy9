//! Layered 2D tilemap asset format (`.tilemap.ron`) and Bevy [`AssetLoader`].

use std::{fs, path::Path};

use bevy_asset::{io::Reader, Asset, AssetLoader, LoadContext};
use bevy_reflect::{Reflect, TypePath};
use engine_core::AssetUuid;
use ron::ser::PrettyConfig;
use serde::{Deserialize, Serialize};

use crate::error::AssetError;

/// Current `.tilemap.ron` schema version.
pub const CURRENT_TILEMAP_VERSION: u32 = 1;

const fn default_tilemap_version() -> u32 {
    CURRENT_TILEMAP_VERSION
}

const fn default_tile_size() -> [u32; 2] {
    [16, 16]
}

const fn default_grid_dim() -> u32 {
    1
}

const fn default_visible() -> bool {
    true
}

/// Per-tile collision / gameplay metadata stored inside a [`TilemapLayer`].
#[derive(Reflect, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileMetadata {
    /// Zero-based index inside the tileset grid corresponding to this metadata entry.
    pub tile_index: u32,
    /// Whether this tile blocks movement / generates solid collision in gameplay systems.
    pub solid: bool,
    /// Optional gameplay tag (e.g. `"water"`, `"hazard"`, `"ladder"`).
    #[serde(default)]
    pub tag: String,
}

/// Single named 2D tile grid layer rendered at a deterministic `z_offset`.
#[derive(Reflect, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TilemapLayer {
    /// Human-readable layer name (e.g. `"ground"`, `"walls"`, `"decor"`).
    pub name: String,
    /// Local Z depth offset applied to tiles on this layer.
    #[serde(default)]
    pub z_offset: f32,
    /// Whether this layer is rendered during hydration and gameplay.
    #[serde(default = "default_visible")]
    pub visible: bool,
    /// Number of columns across the tilemap grid.
    pub width: u32,
    /// Number of rows down the tilemap grid.
    pub height: u32,
    /// Row-major grid cells storing `Some(tile_index)` or `None` for empty cells.
    pub tiles: Vec<Option<u32>>,
    /// Optional tile metadata rules keyed by tileset index.
    #[serde(default)]
    pub metadata: Vec<TileMetadata>,
}

/// Authored 2D tilemap asset loaded from `.tilemap.ron` files.
#[derive(Asset, Reflect, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TilemapAsset {
    /// File schema version checked when parsing `.tilemap.ron`.
    #[serde(default = "default_tilemap_version")]
    pub schema_version: u32,
    /// Optional UUID of the tileset texture asset in [`crate::AssetRegistry`].
    #[serde(default)]
    pub tileset_texture: Option<AssetUuid>,
    /// Width and height of each tile in world/pixel units `[width, height]`.
    #[serde(default = "default_tile_size")]
    pub tile_size: [u32; 2],
    /// Number of columns in the tileset texture atlas grid.
    #[serde(default = "default_grid_dim")]
    pub grid_columns: u32,
    /// Number of rows in the tileset texture atlas grid.
    #[serde(default = "default_grid_dim")]
    pub grid_rows: u32,
    /// Ordered list of tilemap layers rendered bottom-to-top.
    #[serde(default)]
    pub layers: Vec<TilemapLayer>,
}

impl Default for TilemapAsset {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_TILEMAP_VERSION,
            tileset_texture: None,
            tile_size: default_tile_size(),
            grid_columns: 1,
            grid_rows: 1,
            layers: Vec::new(),
        }
    }
}

/// Loads and validates a [`TilemapAsset`] from a `.tilemap.ron` file on disk.
pub fn load_tilemap_from_file(path: &Path) -> Result<TilemapAsset, AssetError> {
    let raw = fs::read_to_string(path).map_err(|source| AssetError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let parsed: TilemapAsset = ron::de::from_str(&raw).map_err(|source| AssetError::RonParse {
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;
    if parsed.schema_version != CURRENT_TILEMAP_VERSION {
        return Err(AssetError::InvalidData(format!(
            "unsupported tilemap schema_version {} in '{}' (expected {})",
            parsed.schema_version,
            path.display(),
            CURRENT_TILEMAP_VERSION
        )));
    }
    Ok(parsed)
}

/// Serializes and writes a [`TilemapAsset`] cleanly as pretty RON to `path`.
pub fn save_tilemap_to_file(path: &Path, asset: &TilemapAsset) -> Result<(), AssetError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| AssetError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let encoded = ron::ser::to_string_pretty(asset, PrettyConfig::default())?;
    fs::write(path, encoded).map_err(|source| AssetError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Bevy [`AssetLoader`] for `.tilemap.ron` files.
#[derive(Default, TypePath)]
pub struct TilemapAssetLoader;

impl AssetLoader for TilemapAssetLoader {
    type Asset = TilemapAsset;
    type Settings = ();
    type Error = AssetError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|source| AssetError::Io {
                path: load_context.path().path().to_path_buf(),
                source,
            })?;
        let parsed: TilemapAsset =
            ron::de::from_bytes(&bytes).map_err(|source| AssetError::RonParse {
                path: load_context.path().path().to_path_buf(),
                source: Box::new(source),
            })?;
        if parsed.schema_version != CURRENT_TILEMAP_VERSION {
            return Err(AssetError::InvalidData(format!(
                "unsupported tilemap schema_version {} in '{}' (expected {})",
                parsed.schema_version,
                load_context.path(),
                CURRENT_TILEMAP_VERSION
            )));
        }
        Ok(parsed)
    }

    fn extensions(&self) -> &[&str] {
        &["tilemap.ron"]
    }
}
