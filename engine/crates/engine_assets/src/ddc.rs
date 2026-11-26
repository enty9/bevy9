//! Project-local Derived Data Cache (`.cache/ddc`) and Bevy `AssetProcessor` configuration.

use std::{
    fs,
    path::{Path, PathBuf},
};

use bevy_app::App;
use bevy_asset::{
    processor::{AssetProcessor, FileTransactionLogFactory},
    AssetMode, AssetPlugin,
};
use bevy_ecs::resource::Resource;
use engine_core::ProjectPaths;

use crate::error::AssetError;

/// Project-relative directory housing processed assets, processor transaction logs, and thumbnail PNGs.
pub const DEFAULT_DDC_SUBDIR: &str = ".cache/ddc";

/// Filesystem paths for a project's local Derived Data Cache (`.cache/ddc`).
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct DdcConfig {
    /// Cache root (`<project_root>/.cache/ddc`).
    pub root_dir: PathBuf,
    /// Processed assets directory used when Bevy runs with `AssetMode::Processed`.
    pub processed_dir: PathBuf,
    /// Cached 128×128 PNG thumbnails directory (`<root_dir>/thumbnails`).
    pub thumbnails_dir: PathBuf,
    /// `AssetProcessor` write-ahead transaction log file (`<root_dir>/processor.log`).
    pub processor_log_file: PathBuf,
}

impl DdcConfig {
    /// Derives canonical/absolute `.cache/ddc` paths anchored under `project_root`.
    #[must_use]
    pub fn for_project_root(project_root: &Path) -> Self {
        let abs_root = if project_root.is_absolute() {
            project_root.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(project_root)
        };
        let root_dir = abs_root.join(DEFAULT_DDC_SUBDIR);
        Self {
            processed_dir: root_dir.join("processed"),
            thumbnails_dir: root_dir.join("thumbnails"),
            processor_log_file: root_dir.join("processor.log"),
            root_dir,
        }
    }

    /// Creates all `.cache/ddc` directories on disk if they do not exist yet.
    pub fn ensure_dirs(&self) -> Result<(), AssetError> {
        for dir in [&self.root_dir, &self.processed_dir, &self.thumbnails_dir] {
            fs::create_dir_all(dir).map_err(|source| AssetError::Io {
                path: dir.clone(),
                source,
            })?;
        }
        Ok(())
    }
}

/// Builds a configured Bevy [`AssetPlugin`] and [`DdcConfig`] for `paths`.
///
/// When `enable_processor` is `true`, Bevy runs in [`AssetMode::Processed`] storing compiled assets
/// inside `<project_root>/.cache/ddc/processed` (avoiding `imported_assets/` in `assets/`).
#[must_use]
pub fn configure_asset_plugin_for_project(
    paths: &ProjectPaths,
    enable_processor: bool,
) -> (AssetPlugin, DdcConfig) {
    let ddc = DdcConfig::for_project_root(&paths.root_dir);
    if let Err(err) = ddc.ensure_dirs() {
        tracing::warn!(error = %err, "failed to create .cache/ddc directories");
    }

    let abs_assets = if paths.assets_dir.is_absolute() {
        paths.assets_dir.clone()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(&paths.assets_dir)
    };

    let plugin = if enable_processor {
        AssetPlugin {
            file_path: abs_assets.to_string_lossy().into_owned(),
            processed_file_path: ddc.processed_dir.to_string_lossy().into_owned(),
            mode: AssetMode::Processed,
            use_asset_processor_override: Some(true),
            watch_for_changes_override: Some(true),
            ..Default::default()
        }
    } else {
        AssetPlugin {
            file_path: abs_assets.to_string_lossy().into_owned(),
            mode: AssetMode::Unprocessed,
            use_asset_processor_override: Some(false),
            watch_for_changes_override: Some(false),
            ..Default::default()
        }
    };

    (plugin, ddc)
}

/// Redirects Bevy's [`AssetProcessor`] transaction log file to `<project_root>/.cache/ddc/processor.log`.
///
/// Must be called after [`AssetPlugin`] / `DefaultPlugins` is added and before the first `app.update()`
/// (`Startup`) begins processing assets.
pub fn configure_asset_processor_ddc_log(app: &mut App, ddc: &DdcConfig) {
    if let Err(err) = ddc.ensure_dirs() {
        tracing::warn!(error = %err, "failed to prepare .cache/ddc before configuring processor log");
    }
    if let Some(processor) = app.world().get_resource::<AssetProcessor>()
        && let Err(err) = processor.data().set_log_factory(Box::new(FileTransactionLogFactory {
            file_path: ddc.processor_log_file.clone(),
        }))
    {
        tracing::warn!(
            path = %ddc.processor_log_file.display(),
            error = ?err,
            "failed to override AssetProcessor transaction log path"
        );
    }
}
