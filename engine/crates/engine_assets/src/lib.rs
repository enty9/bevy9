//! Asset management crate: UUID `.b9meta` sidecars, [`AssetRegistry`], `uuid://` source, importers, DDC, thumbnails, and file watcher.

pub mod error;
pub mod meta;
pub mod registry;
pub mod source;
pub mod watcher;

/// Commonly used types and helpers exported by `engine_assets`.
pub mod prelude {
    pub use crate::{
        error::AssetError,
        meta::{
            compute_file_blake3, extract_uuid_references_from_text_asset, infer_asset_kind,
            inspect_gltf_sub_assets, load_or_create_meta, meta_path_for, save_meta_file,
            AssetMetaFile, CURRENT_META_VERSION, SIDECAR_EXTENSION,
        },
        registry::{AssetRegistry, ScanReport, SharedAssetRegistry},
        source::{register_uuid_asset_source, UuidAssetReader, UUID_ASSET_SOURCE},
        watcher::{poll_asset_watcher_system, AssetWatcherResource},
        EngineAssetsPlugin,
    };
}

use bevy_app::{App, Plugin, Update};
use bevy_ecs::schedule::IntoScheduleConfigs;
use engine_core::{schedule::EngineSet, ProjectPaths};

pub use prelude::*;

/// Plugin initializing [`AssetRegistry`], starting the filesystem watcher, and scheduling hot-reload polling.
#[derive(Debug, Default, Clone, Copy)]
pub struct EngineAssetsPlugin;

impl Plugin for EngineAssetsPlugin {
    fn build(&self, app: &mut App) {
        let mut registry = if let Some(existing) = app.world().get_resource::<AssetRegistry>() {
            existing.clone()
        } else if let Some(shared) = app.world().get_resource::<SharedAssetRegistry>() {
            AssetRegistry::from_shared(shared.clone())
        } else if let Some(paths) = app.world().get_resource::<ProjectPaths>() {
            AssetRegistry::new(paths.assets_dir.clone(), Some(paths.scenes_dir.clone()))
        } else {
            AssetRegistry::default()
        };

        if let Err(err) = registry.scan_all() {
            tracing::warn!(error = %err, "initial AssetRegistry scan failed");
        }
        let shared = registry.shared();
        app.insert_resource(shared);
        app.insert_resource(registry);

        let (assets_dir, scenes_dir) = app
            .world()
            .get_resource::<ProjectPaths>()
            .map_or_else(
                || (std::path::PathBuf::from("assets"), None),
                |p| (p.assets_dir.clone(), Some(p.scenes_dir.clone())),
            );

        if assets_dir.exists() {
            match AssetWatcherResource::start(&assets_dir, scenes_dir.as_deref()) {
                Ok(watcher) => {
                    app.insert_resource(watcher);
                }
                Err(err) => {
                    tracing::warn!(
                        path = %assets_dir.display(),
                        error = %err,
                        "failed to start AssetWatcherResource"
                    );
                }
            }
        }

        app.add_systems(
            Update,
            poll_asset_watcher_system.in_set(EngineSet::EditorSync),
        );
    }
}
