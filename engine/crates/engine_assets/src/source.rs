//! Custom `uuid://` Bevy [`AssetSourceId`](bevy_asset::io::AssetSourceId) reader resolving [`AssetUuid`]s dynamically.

use std::{fs, path::Path};

use bevy_app::App;
use bevy_asset::{
    io::{AssetReader, AssetReaderError, AssetSourceBuilder, PathStream, Reader, VecReader},
    AssetApp,
};
use engine_core::AssetUuid;
use futures_lite::stream;

use crate::registry::SharedAssetRegistry;

/// Scheme name used to reference indexed assets independently of disk path (`uuid://<uuid>`).
pub const UUID_ASSET_SOURCE: &str = "uuid";

/// Custom [`AssetReader`] backed by [`SharedAssetRegistry`] resolving `uuid://<uuid>[.ext]`.
#[derive(Clone, Debug)]
pub struct UuidAssetReader {
    registry: SharedAssetRegistry,
}

impl UuidAssetReader {
    /// Constructs a new reader backed by `registry`.
    #[must_use]
    pub const fn new(registry: SharedAssetRegistry) -> Self {
        Self { registry }
    }

    fn resolve_path(&self, path: &Path) -> Result<std::path::PathBuf, AssetReaderError> {
        let raw = path.to_string_lossy();
        let trimmed = raw.trim_start_matches('/');
        let uuid_token = trimmed
            .split(['/', '.'])
            .next()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AssetReaderError::NotFound(path.to_path_buf()))?;

        let uuid = AssetUuid::parse_str(uuid_token)
            .map_err(|_| AssetReaderError::NotFound(path.to_path_buf()))?;
        self.registry
            .resolve_abs_path(uuid)
            .ok_or_else(|| AssetReaderError::NotFound(path.to_path_buf()))
    }
}

impl AssetReader for UuidAssetReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        let abs = self.resolve_path(path)?;
        let bytes = fs::read(&abs).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                AssetReaderError::NotFound(abs)
            } else {
                AssetReaderError::from(err)
            }
        })?;
        Ok(VecReader::new(bytes))
    }

    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        let abs = self.resolve_path(path)?;
        let mut meta_os = abs.as_os_str().to_os_string();
        meta_os.push(".meta");
        let meta_path = std::path::PathBuf::from(meta_os);
        let bytes = fs::read(&meta_path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                AssetReaderError::NotFound(meta_path)
            } else {
                AssetReaderError::from(err)
            }
        })?;
        Ok(VecReader::new(bytes))
    }

    async fn read_directory<'a>(
        &'a self,
        _path: &'a Path,
    ) -> Result<Box<PathStream>, AssetReaderError> {
        let empty: Box<PathStream> = Box::new(stream::empty());
        Ok(empty)
    }

    async fn is_directory<'a>(&'a self, _path: &'a Path) -> Result<bool, AssetReaderError> {
        Ok(false)
    }
}

/// Registers the `uuid://` asset source on `app` backed by `registry`.
///
/// Must be invoked before `AssetPlugin` / `DefaultPlugins` is added to `app`.
pub fn register_uuid_asset_source(app: &mut App, registry: SharedAssetRegistry) {
    let reader_factory = move || -> Box<dyn bevy_asset::io::ErasedAssetReader> {
        Box::new(UuidAssetReader::new(registry.clone()))
    };
    let processed_factory = reader_factory.clone();
    app.register_asset_source(
        UUID_ASSET_SOURCE,
        AssetSourceBuilder::new(reader_factory).with_processed_reader(processed_factory),
    );
}
