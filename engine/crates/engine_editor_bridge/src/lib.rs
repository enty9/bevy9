//! Editor Host bridge exposing BRP JSON-RPC and a `WebSocket` IPC + frame server.

pub mod brp;
pub mod config;
mod gameplay;
pub mod history;
pub mod ipc_server;
pub mod picking;
pub mod pie;
pub mod selection;

/// Commonly used editor bridge types and constants.
pub mod prelude {
    pub use crate::{
        config::{EditorBridgeConfig, DEFAULT_BRP_PORT, DEFAULT_IPC_PORT},
        history::EditorHistory,
        ipc_server::EditorIpcServer,
        picking::EditorHoveredEntity,
        pie::{PieController, PieError, PieSessionState, PieStateTransition},
        selection::EditorSelection,
        EditorBridgePlugin,
    };
}

use bevy_app::{App, FixedUpdate, Plugin, PostUpdate, PreUpdate, Update};
use bevy_ecs::schedule::{common_conditions::resource_exists, IntoScheduleConfigs};
use bevy_gizmos::gizmos::GizmoStorage;
use bevy_picking::{backend::ray::RayMap, PickingSystems};
use engine_core::schedule::{EngineFixedSet, EngineSet};
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
            .init_resource::<PieSessionState>()
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

        // Prevent `engine_physics` debug preview sync from resetting live physics worlds while PIE is paused.
        app.configure_sets(
            PostUpdate,
            engine_physics::PhysicsDebugSyncSet.run_if(pie::is_editing),
        );

        app.add_systems(
            PreUpdate,
            (
                ipc_server::process_ipc_inbound
                    .run_if(resource_exists::<EditorIpcServer>)
                    .before(engine_input::InputSamplingSet)
                    .before(engine_render::picking::ViewportInputCaptureSet),
                picking::helper_icon_picking_backend
                    .run_if(pie::is_editing)
                    .run_if(resource_exists::<RayMap>)
                    .in_set(PickingSystems::Backend),
            ),
        )
        .add_systems(
            FixedUpdate,
            pie::track_pie_fixed_step.in_set(EngineFixedSet::Gameplay),
        )
        .add_systems(
            Update,
            (
                (
                    gameplay::focus_selection.run_if(pie::is_editing),
                    picking::update_transform_gizmos_system.run_if(pie::is_editing),
                )
                    .chain()
                    .before(engine_render::EditorCameraSystems)
                    .in_set(EngineSet::EditorSync),
                (
                    pie::track_pie_update_frame,
                    pie::sync_pie_cameras_and_viewport,
                    history::capture_gizmo_commits_system.run_if(pie::is_editing),
                    history::handle_native_history_shortcuts_system.run_if(pie::is_editing),
                    picking::handle_viewport_selection_picking.run_if(pie::is_editing),
                    gameplay::sync_physics_selection
                        .run_if(pie::is_editing)
                        .run_if(resource_exists::<engine_physics::PhysicsDebugSelection>),
                    picking::draw_editor_helper_icons_and_selection
                        .run_if(pie::is_editing)
                        .run_if(resource_exists::<GizmoStorage<EditorOverlayGizmoGroup, ()>>),
                    picking::draw_transform_gizmos_system
                        .run_if(pie::is_editing)
                        .run_if(resource_exists::<GizmoStorage<EditorOverlayGizmoGroup, ()>>),
                    ipc_server::detect_scene_hierarchy_changes,
                    ipc_server::publish_ipc_updates.run_if(resource_exists::<EditorIpcServer>),
                )
                    .chain()
                    .after(engine_render::EditorCameraSystems)
                    .in_set(EngineSet::EditorSync),
            ),
        );
    }
}
