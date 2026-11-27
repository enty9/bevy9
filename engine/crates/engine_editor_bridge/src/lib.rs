//! Editor Host bridge exposing BRP JSON-RPC and a `WebSocket` IPC + frame server.

pub mod brp;
pub mod config;
pub mod history;
pub mod ipc_server;
pub mod picking;
pub mod selection;
mod gameplay;

/// Commonly used editor bridge types and constants.
pub mod prelude {
    pub use crate::{
        config::{EditorBridgeConfig, DEFAULT_BRP_PORT, DEFAULT_IPC_PORT},
        history::EditorHistory,
        ipc_server::EditorIpcServer,
        picking::EditorHoveredEntity,
        selection::EditorSelection,
        EditorBridgePlugin,
    };
}

use bevy_app::{App, Plugin, PreUpdate, Update};
use bevy_ecs::schedule::{common_conditions::resource_exists, IntoScheduleConfigs};
use bevy_gizmos::gizmos::GizmoStorage;
use bevy_picking::{backend::ray::RayMap, PickingSystems};
use engine_core::schedule::EngineSet;
use engine_render::picking::EditorOverlayGizmoGroup;

pub use prelude::*;

/// Plugin attaching HTTP BRP (with custom `engine.*` methods) and `WebSocket` IPC to an Editor Host [`App`].
#[derive(Debug, Default, Clone, Copy)]
pub struct EditorBridgePlugin;

impl Plugin for EditorBridgePlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<EditorBridgeConfig>() {
            app.init_resource::<EditorBridgeConfig>();
        }
        let config = app.world().resource::<EditorBridgeConfig>().clone();

        app.init_resource::<EditorSelection>()
            .init_resource::<engine_input::ForwardedInput>()
            .init_resource::<EditorHoveredEntity>()
            .init_resource::<EditorHistory>()
            .init_resource::<selection::BridgeDirtyFlags>()
            .init_resource::<ipc_server::LastBroadcastViewport>()
            .init_resource::<ipc_server::MetricsPublishState>();

        app.add_plugins(brp::build_remote_plugin());
        if config.enable_http_brp {
            app.add_plugins(brp::build_remote_http_plugin(&config));
        }

        if config.enable_ws_ipc {
            match EditorIpcServer::bind(config.host, config.ipc_port) {
                Ok(server) => {
                    tracing::info!(addr = %server.bound_addr(), "Editor WebSocket IPC server listening");
                    app.insert_resource(server);
                }
                Err(err) => {
                    tracing::error!(
                        host = %config.host,
                        port = config.ipc_port,
                        error = %err,
                        "failed to bind Editor WebSocket IPC listener"
                    );
                }
            }
        }

        app.add_systems(
            PreUpdate,
            (
                ipc_server::process_ipc_inbound
                    .run_if(resource_exists::<EditorIpcServer>)
                    .before(engine_input::InputSamplingSet)
                    .before(engine_render::picking::ViewportInputCaptureSet),
                picking::helper_icon_picking_backend
                    .run_if(resource_exists::<RayMap>)
                    .in_set(PickingSystems::Backend),
            ),
        )
        .add_systems(
            Update,
            (
                gameplay::focus_selection.before(engine_render::EditorCameraSystems),
                picking::update_transform_gizmos_system
                    .before(engine_render::EditorCameraSystems),
                history::capture_gizmo_commits_system,
                history::handle_native_history_shortcuts_system,
                picking::handle_viewport_selection_picking,
                picking::draw_editor_helper_icons_and_selection
                    .run_if(resource_exists::<GizmoStorage<EditorOverlayGizmoGroup, ()>>),
                picking::draw_transform_gizmos_system
                    .run_if(resource_exists::<GizmoStorage<EditorOverlayGizmoGroup, ()>>),
                ipc_server::detect_scene_hierarchy_changes,
                ipc_server::publish_ipc_updates.run_if(resource_exists::<EditorIpcServer>),
            )
                .chain()
                .in_set(EngineSet::EditorSync),
        );
    }
}
