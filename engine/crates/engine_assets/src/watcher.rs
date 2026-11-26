//! Debounced filesystem watcher (`notify`) syncing [`AssetRegistry`] and triggering live Bevy hot-reload.

use core::time::Duration;
use std::{
    path::Path,
    time::Instant,
};

use bevy_asset::AssetServer;
use bevy_ecs::{
    resource::Resource,
    system::{Res, ResMut},
};
use crossbeam_channel::{unbounded, Receiver};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::{meta::is_ignored_file, registry::AssetRegistry};

const RESCAN_DEBOUNCE: Duration = Duration::from_millis(150);

/// ECS resource holding a live OS file-watcher monitoring the project `assets/` and `scenes/` directories.
#[derive(Resource)]
pub struct AssetWatcherResource {
    _watcher: RecommendedWatcher,
    rx: Receiver<()>,
    pending_rescan_at: Option<Instant>,
}

impl AssetWatcherResource {
    /// Starts watching `assets_dir` (and optional `scenes_dir`) recursively for non-sidecar changes.
    pub fn start(assets_dir: &Path, scenes_dir: Option<&Path>) -> Result<Self, notify::Error> {
        let (tx, rx) = unbounded();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let Ok(event) = res else {
                return;
            };
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            let relevant = event.paths.iter().any(|p| !is_ignored_file(p));
            if relevant {
                let _ = tx.send(());
            }
        })?;

        if assets_dir.exists() {
            watcher.watch(assets_dir, RecursiveMode::Recursive)?;
        }
        if let Some(scenes) = scenes_dir
            && scenes.exists()
        {
            watcher.watch(scenes, RecursiveMode::Recursive)?;
        }

        Ok(Self {
            _watcher: watcher,
            rx,
            pending_rescan_at: None,
        })
    }
}

/// Polls filesystem notifications with debouncing, rescans [`AssetRegistry`], and reloads modified assets in [`AssetServer`].
pub fn poll_asset_watcher_system(
    mut watcher: Option<ResMut<AssetWatcherResource>>,
    mut registry: Option<ResMut<AssetRegistry>>,
    asset_server: Option<Res<AssetServer>>,
) {
    let (Some(watcher), Some(registry)) = (watcher.as_deref_mut(), registry.as_deref_mut()) else {
        return;
    };

    let mut received = false;
    while watcher.rx.try_recv().is_ok() {
        received = true;
    }
    if received {
        watcher.pending_rescan_at = Some(Instant::now() + RESCAN_DEBOUNCE);
        return;
    }

    let Some(deadline) = watcher.pending_rescan_at else {
        return;
    };
    if Instant::now() < deadline {
        return;
    }
    watcher.pending_rescan_at = None;

    match registry.scan_all() {
        Ok(report) => {
            if !report.modified_paths.is_empty()
                && let Some(server) = asset_server.as_deref()
            {
                for rel_path in &report.modified_paths {
                    server.reload(rel_path.clone());
                }
            }
            if report.changed {
                tracing::debug!(
                    assets = report.total_assets,
                    modified = report.modified_paths.len(),
                    "AssetRegistry rescan complete"
                );
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "AssetRegistry filesystem watcher rescan failed");
        }
    }
}
