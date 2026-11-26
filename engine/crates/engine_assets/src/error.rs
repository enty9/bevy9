//! Error types returned by `engine_assets` metadata, registry, importer, and DDC operations.

use std::path::PathBuf;

use engine_core::AssetUuid;
use thiserror::Error;

/// Errors returned by [`crate::AssetRegistry`], `.b9meta` sidecars, and asset importers.
#[derive(Debug, Error)]
pub enum AssetError {
    /// Filesystem I/O failure.
    #[error("asset I/O error at '{}': {source}", path.display())]
    Io {
        /// Path involved in the failing filesystem operation.
        path: PathBuf,
        /// Source OS error.
        #[source]
        source: std::io::Error,
    },
    /// RON parsing error for `.b9meta`, `.tilemap.ron`, or `.prefab.ron`.
    #[error("failed to parse asset RON '{}': {source}", path.display())]
    RonParse {
        /// File path being parsed.
        path: PathBuf,
        /// Source RON spanned error.
        #[source]
        source: Box<ron::error::SpannedError>,
    },
    /// RON serialization error.
    #[error("failed to serialize asset RON: {0}")]
    RonSerialize(#[from] ron::Error),
    /// Requested asset UUID was not found in the active registry.
    #[error("asset with UUID {0} was not found in AssetRegistry")]
    NotFound(AssetUuid),
    /// Invalid or path-escaping relative asset path supplied by caller.
    #[error("invalid relative asset path '{0}'")]
    InvalidRelativePath(String),
    /// Destination path already belongs to another indexed asset.
    #[error("destination asset path '{0}' already exists")]
    DestinationExists(String),
    /// Deletion blocked because other assets or scenes depend on this asset.
    #[error("asset {uuid} is still referenced by {} dependent(s)", dependents.len())]
    HasDependents {
        /// Target asset UUID requested for deletion.
        uuid: AssetUuid,
        /// Human-readable descriptions/UUIDs of referencing assets or scenes.
        dependents: Vec<String>,
    },
    /// Tilemap or asset schema version mismatch or invalid structure.
    #[error("invalid asset data: {0}")]
    InvalidData(String),
}
