//! 2D/3D editor cameras, procedural grids, and dual Offscreen/NativeWindow `ViewportTransport`.

pub mod camera;
pub mod grid;
pub mod picking;
pub mod viewport;

/// Commonly used editor render types.
pub mod prelude {
    pub use crate::{
        camera::{EditorCamera2d, EditorCamera3d, EditorCameraState},
        picking::{
            EditorOverlayGizmoGroup, EditorPointerState, ViewportFrameInput,
            ViewportInputCaptureSet, OFFSCREEN_POINTER_ID,
        },
        viewport::{
            align_bytes_per_row, clamp_viewport_dimensions, create_offscreen_target_image,
            unpack_aligned_rgba8, DetachedViewportWindow, OffscreenRenderState, ViewportConfig,
            ViewportFrameChannel, ViewportFramePacket, ViewportInputQueue,
        },
        EngineRenderPlugin,
    };
}

use bevy_app::{App, Plugin, PreUpdate, Startup, Update};
use bevy_ecs::{
    message::Messages,
    schedule::{common_conditions::resource_exists, IntoScheduleConfigs},
};
use bevy_gizmos::{
    config::{GizmoConfig, GizmoConfigStore, GizmoLineConfig},
    AppGizmoBuilder,
};
use bevy_picking::{
    backend::ray::RayMap,
    pointer::{PointerInput, PointerMap},
    PickingSystems,
};
use engine_core::{schedule::EngineSet, WorldMode};
use engine_protocol::ViewportCameraMode;

pub use prelude::*;

/// Plugin setting up 2D/3D editor cameras, reference grids, picking input, and `ViewportTransport`.
#[derive(Debug, Default, Clone, Copy)]
pub struct EngineRenderPlugin;

impl Plugin for EngineRenderPlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<ViewportConfig>() {
            let camera_mode = match app.world().get_resource::<WorldMode>() {
                Some(WorldMode::Mode2D) => ViewportCameraMode::Mode2D,
                _ => ViewportCameraMode::Mode3D,
            };
            app.insert_resource(ViewportConfig {
                camera_mode,
                ..Default::default()
            });
        }

        if app.world().contains_resource::<GizmoConfigStore>() {
            app.insert_gizmo_config(
                EditorOverlayGizmoGroup,
                GizmoConfig {
                    depth_bias: -1.0,
                    line: GizmoLineConfig {
                        width: 2.5,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
        }

        app.init_resource::<EditorCameraState>()
            .init_resource::<ViewportFrameChannel>()
            .init_resource::<ViewportInputQueue>()
            .init_resource::<ViewportFrameInput>()
            .init_resource::<EditorPointerState>()
            .init_resource::<OffscreenRenderState>()
            .add_systems(
                Startup,
                (
                    camera::spawn_editor_cameras,
                    picking::spawn_offscreen_pointer.run_if(resource_exists::<PointerMap>),
                ),
            )
            .add_systems(
                PreUpdate,
                (
                    picking::capture_viewport_frame_input
                        .in_set(ViewportInputCaptureSet)
                        .before(PickingSystems::ProcessInput),
                    picking::forward_offscreen_pointer_inputs
                        .in_set(PickingSystems::ProcessInput)
                        .before(PointerInput::receive)
                        .run_if(resource_exists::<Messages<PointerInput>>),
                    picking::populate_editor_picking_rays
                        .in_set(PickingSystems::ProcessInput)
                        .after(RayMap::repopulate),
                ),
            )
            .add_systems(
                Update,
                (
                    viewport::process_viewport_resize_inputs,
                    camera::sync_editor_camera_activity,
                    camera::update_editor_cameras,
                    viewport::sync_viewport_transport,
                )
                    .chain()
                    .in_set(EngineSet::EditorSync),
            )
            .add_systems(
                Update,
                grid::draw_editor_grid
                    .run_if(resource_exists::<GizmoConfigStore>)
                    .in_set(EngineSet::EditorSync),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_asset::{AssetApp, AssetPlugin};
    use bevy_camera::RenderTarget;
    use bevy_image::Image;
    use bevy_input::mouse::{MouseMotion, MouseWheel};
    use bevy_time::TimePlugin;
    use bevy_window::{Window, WindowCloseRequested, WindowRef};
    use engine_core::EngineCorePlugin;
    use engine_protocol::{ViewportInputEvent, ViewportMode};

    fn setup_headless_render_app(mode: WorldMode) -> App {
        let mut app = App::new();
        app.insert_resource(mode)
            .add_message::<MouseMotion>()
            .add_message::<MouseWheel>()
            .add_message::<WindowCloseRequested>()
            .add_plugins((
                TimePlugin,
                AssetPlugin {
                    use_asset_processor_override: Some(false),
                    ..Default::default()
                },
                EngineCorePlugin,
                EngineRenderPlugin,
            ))
            .init_asset::<Image>();
        app
    }

    #[test]
    fn switching_viewport_transport_preserves_editor_camera_state() {
        let mut app = setup_headless_render_app(WorldMode::Mode3D);
        app.update();

        {
            let mut cam_state = app.world_mut().resource_mut::<EditorCameraState>();
            cam_state.focus = bevy_math::Vec3::new(3.0, 2.0, -1.0);
            cam_state.distance = 12.0;
        }

        // Offscreen mode should allocate target image, keep no detached OS window,
        // and attach RenderTarget::Image.
        {
            let state = app.world().resource::<OffscreenRenderState>();
            assert!(state.image_handle.is_some());
            assert!(state.readback_entity.is_some());
            assert!(state.detached_window.is_none());
            assert_eq!(state.allocated_size, bevy_math::UVec2::new(1280, 720));
        }
        {
            let world = app.world_mut();
            let mut q = world.query::<(&RenderTarget, &EditorCamera3d)>();
            let (target, _) = q.single(world).expect("EditorCamera3d present");
            assert!(matches!(target, RenderTarget::Image(_)));
        }

        // Switch to NativeWindow: Readback despawned, detached window spawned, RenderTarget points to detached window.
        app.world_mut().resource_mut::<ViewportConfig>().mode = ViewportMode::NativeWindow;
        app.update();

        let win_entity = {
            let state = app.world().resource::<OffscreenRenderState>();
            assert!(state.readback_entity.is_none());
            let win_entity = state
                .detached_window
                .expect("detached viewport window must be spawned");
            let cam_state = app.world().resource::<EditorCameraState>();
            assert_eq!(cam_state.focus, bevy_math::Vec3::new(3.0, 2.0, -1.0));
            assert_eq!(cam_state.distance, 12.0);
            win_entity
        };
        {
            let world = app.world();
            let window = world
                .get::<Window>(win_entity)
                .expect("detached window entity must have Window component");
            assert!(window.visible);
            assert!(!window.focused);
            assert!(world.get::<DetachedViewportWindow>(win_entity).is_some());
        }
        {
            let world = app.world_mut();
            let mut q = world.query::<(&RenderTarget, &EditorCamera3d)>();
            let (target, _) = q.single(world).expect("EditorCamera3d present");
            assert!(matches!(
                target,
                RenderTarget::Window(WindowRef::Entity(id)) if *id == win_entity
            ));
        }

        // Emitting WindowCloseRequested on the detached window switches back to Offscreen,
        // despawns the window entity, and restores RenderTarget::Image.
        app.world_mut()
            .write_message(WindowCloseRequested { window: win_entity });
        app.update();

        {
            let config = app.world().resource::<ViewportConfig>();
            assert_eq!(config.mode, ViewportMode::Offscreen);
            let state = app.world().resource::<OffscreenRenderState>();
            assert!(state.detached_window.is_none());
            assert!(state.readback_entity.is_some());
            assert!(app.world().get_entity(win_entity).is_err());
        }
        {
            let world = app.world_mut();
            let mut q = world.query::<(&RenderTarget, &EditorCamera3d)>();
            let (target, _) = q.single(world).expect("EditorCamera3d present");
            assert!(matches!(target, RenderTarget::Image(_)));
        }

        // Resize event in Offscreen reallocates target cleanly.
        app.world_mut()
            .resource_mut::<ViewportInputQueue>()
            .push(ViewportInputEvent::Resize {
                width: 800,
                height: 600,
                scale_factor: 1.0,
            });
        app.update();

        let state = app.world().resource::<OffscreenRenderState>();
        assert_eq!(state.allocated_size, bevy_math::UVec2::new(800, 600));
        assert!(state.readback_entity.is_some());
    }
}
