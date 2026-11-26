//! Headless picking integration tests for Offscreen virtual pointer 2D/3D/helper picking.

use bevy_app::{App, PostUpdate, TaskPoolPlugin};
use bevy_asset::{AssetApp, AssetPlugin, Assets};
use bevy_camera::{
    primitives::{Aabb, MeshAabb},
    visibility::ViewVisibility,
    Camera, CameraPlugin, Projection, RenderTargetInfo,
};
use bevy_ecs::system::Query;
use bevy_image::{Image, TextureAtlasLayout};
use bevy_input::mouse::{MouseMotion, MouseWheel};
use bevy_math::{UVec2, Vec3};
use bevy_mesh::{Mesh, Mesh2d, Mesh3d, MeshPlugin};
use bevy_pbr::StandardMaterial;
use bevy_picking::{
    mesh_picking::MeshPickingPlugin,
    pointer::{PointerId, PointerMap},
    InteractionPlugin, PickingPlugin,
};
use bevy_sprite::{SpritePickingMode, SpritePickingPlugin, SpritePickingSettings};
use bevy_sprite_render::ColorMaterial;
use bevy_time::TimePlugin;
use bevy_transform::{components::Transform, TransformPlugin};
use bevy_window::WindowCloseRequested;
use engine_core::{EngineCorePlugin, WorldMode};
use engine_editor_bridge::{EditorBridgeConfig, EditorBridgePlugin, EditorSelection};
use engine_protocol::{
    EntityPresetKind, InputModifiers, PointerButtonCode, ViewportCameraMode, ViewportInputEvent,
};
use engine_render::{
    camera::EditorCameraState,
    picking::OFFSCREEN_POINTER_ID,
    EngineRenderPlugin, ViewportConfig, ViewportInputQueue,
};
use engine_scene::{spawn_preset, EngineScenePlugin};

fn sync_headless_camera_targets_and_visibility(
    config: bevy_ecs::system::Res<ViewportConfig>,
    meshes: bevy_ecs::system::Res<Assets<Mesh>>,
    mut cameras: Query<(&mut Camera, &mut Projection)>,
    mut vis_entities: Query<(&mut ViewVisibility,)>,
    mesh2d_missing_aabb: Query<(
        bevy_ecs::entity::Entity,
        &Mesh2d,
        Option<&Aabb>,
    )>,
    mut commands: bevy_ecs::system::Commands,
) {
    let size = UVec2::new(config.width, config.height);
    for (mut cam, mut proj) in &mut cameras {
        if !cam.is_active {
            continue;
        }
        cam.computed.target_info = Some(RenderTargetInfo {
            physical_size: size,
            scale_factor: 1.0,
        });
        proj.update(size.x as f32, size.y as f32);
        cam.computed.clip_from_view = proj.get_clip_from_view();
    }

    for (mut vis,) in &mut vis_entities {
        *vis = ViewVisibility::VISIBLE;
    }

    for (entity, mesh2d, existing_aabb) in &mesh2d_missing_aabb {
        if existing_aabb.is_none()
            && let Some(mesh) = meshes.get(&mesh2d.0)
            && let Some(aabb) = mesh.get_aabb()
        {
            commands.entity(entity).insert(aabb);
        }
    }
}

fn setup_headless_picking_app(mode: WorldMode, camera_mode: ViewportCameraMode) -> App {
    let mut app = App::new();
    app.insert_resource(mode)
        .insert_resource(ViewportConfig {
            camera_mode,
            width: 800,
            height: 600,
            ..Default::default()
        })
        .insert_resource(EditorBridgeConfig {
            enable_http_brp: false,
            enable_ws_ipc: false,
            ..Default::default()
        })
        .insert_resource(SpritePickingSettings {
            picking_mode: SpritePickingMode::BoundingBox,
            ..Default::default()
        })
        .add_message::<MouseMotion>()
        .add_message::<MouseWheel>()
        .add_message::<WindowCloseRequested>()
        .add_plugins((
            TaskPoolPlugin::default(),
            TimePlugin,
            TransformPlugin,
            AssetPlugin {
                use_asset_processor_override: Some(false),
                ..Default::default()
            },
            MeshPlugin,
            CameraPlugin,
            PickingPlugin,
            InteractionPlugin,
            MeshPickingPlugin,
            SpritePickingPlugin,
            EngineCorePlugin,
            EngineScenePlugin,
            EngineRenderPlugin,
            EditorBridgePlugin,
        ))
        .init_asset::<Image>()
        .init_asset::<TextureAtlasLayout>()
        .init_asset::<StandardMaterial>()
        .init_asset::<ColorMaterial>()
        .add_systems(PostUpdate, sync_headless_camera_targets_and_visibility);

    // Pump 2 frames so startup cameras, offscreen RenderTarget::Image, hydrated meshes/sprites,
    // and headless camera target_info settle.
    app.update();
    app.update();
    app
}

fn send_offscreen_click(
    app: &mut App,
    x: f32,
    y: f32,
    modifiers: InputModifiers,
) {
    {
        let mut queue = app.world_mut().resource_mut::<ViewportInputQueue>();
        queue.push(ViewportInputEvent::PointerMove {
            x,
            y,
            delta_x: 0.0,
            delta_y: 0.0,
        });
        queue.push(ViewportInputEvent::PointerButton {
            button: PointerButtonCode::Left,
            pressed: true,
            x,
            y,
            modifiers,
        });
    }
    app.update();

    {
        let mut queue = app.world_mut().resource_mut::<ViewportInputQueue>();
        queue.push(ViewportInputEvent::PointerButton {
            button: PointerButtonCode::Left,
            pressed: false,
            x,
            y,
            modifiers,
        });
    }
    app.update();
}

#[test]
fn offscreen_virtual_pointer_picks_3d_meshes_helpers_multiselect_and_clears() {
    let mut app = setup_headless_picking_app(WorldMode::Mode3D, ViewportCameraMode::Mode3D);

    // Ensure windowless mode truly has no PrimaryWindow and OFFSCREEN_POINTER_ID is spawned.
    assert!(app
        .world()
        .resource::<PointerMap>()
        .get_entity(OFFSCREEN_POINTER_ID)
        .is_some());
    assert!(app
        .world()
        .resource::<PointerMap>()
        .get_entity(PointerId::Mouse)
        .is_none());

    // Look directly at origin from +Z so projecting center/offset coords is exact.
    {
        let mut cam_state = app.world_mut().resource_mut::<EditorCameraState>();
        cam_state.focus = Vec3::ZERO;
        cam_state.yaw = 0.0;
        cam_state.pitch = 0.0;
        cam_state.distance = 6.0;
    }

    let (_, cube_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Cube3d,
        Some("CenterCube".to_owned()),
        None,
        Some(Transform::IDENTITY),
    );
    let (_, light_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::PointLight,
        Some("HelperLight".to_owned()),
        None,
        Some(Transform::from_xyz(2.0, 0.0, 0.0)),
    );

    // Pump frames for hydration, Aabb computation, and ViewVisibility.
    app.update();
    app.update();

    // Verify we actually hydrated Mesh3d (picked by MeshPickingPlugin).
    assert_eq!(
        app.world_mut()
            .query::<&Mesh3d>()
            .iter(app.world())
            .count(),
        1
    );

    // 1. Click center of 800x600 viewport -> picks CenterCube.
    send_offscreen_click(&mut app, 400.0, 300.0, InputModifiers::default());
    assert_eq!(
        app.world().resource::<EditorSelection>().guids(),
        &[cube_guid]
    );

    // Compute exact screen coordinates of HelperLight for Ctrl+Click multi-selection.
    let light_viewport_pos = {
        let world = app.world_mut();
        let mut cam_q = world.query_filtered::<(
            &Camera,
            &bevy_transform::components::GlobalTransform,
        ), bevy_ecs::query::With<engine_render::EditorCamera3d>>();
        let (cam, cam_tfm) = cam_q.single(world).expect("EditorCamera3d present");
        cam.world_to_viewport(cam_tfm, Vec3::new(2.0, 0.0, 0.0))
            .expect("light should project into viewport")
    };

    // 2. Ctrl+Click HelperLight -> both CenterCube and HelperLight selected.
    send_offscreen_click(
        &mut app,
        light_viewport_pos.x,
        light_viewport_pos.y,
        InputModifiers {
            ctrl: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.world().resource::<EditorSelection>().guids(),
        &[cube_guid, light_guid]
    );

    // 3. Ctrl+Click CenterCube again -> toggles CenterCube off, leaving HelperLight.
    send_offscreen_click(
        &mut app,
        400.0,
        300.0,
        InputModifiers {
            ctrl: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.world().resource::<EditorSelection>().guids(),
        &[light_guid]
    );

    // 4. Plain click on empty corner -> clears selection.
    send_offscreen_click(&mut app, 20.0, 20.0, InputModifiers::default());
    assert!(app.world().resource::<EditorSelection>().guids().is_empty());
}

#[test]
fn offscreen_virtual_pointer_picks_2d_sprites_meshes_and_cameras() {
    let mut app = setup_headless_picking_app(WorldMode::Mode2D, ViewportCameraMode::Mode2D);

    let (_, sprite_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Sprite2d,
        Some("TestSprite".to_owned()),
        None,
        Some(Transform::from_xyz(0.0, 0.0, 1.0)),
    );
    let (_, circle_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Circle2d,
        Some("TestCircle".to_owned()),
        None,
        Some(Transform::from_xyz(160.0, 0.0, 0.5)),
    );
    let (_, cam2d_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Camera2d,
        Some("SceneCam2D".to_owned()),
        None,
        Some(Transform::from_xyz(-180.0, 0.0, 0.0)),
    );

    app.update();
    app.update();

    // 1. Click center (400, 300) -> picks Sprite at world (0, 0).
    send_offscreen_click(&mut app, 400.0, 300.0, InputModifiers::default());
    assert_eq!(
        app.world().resource::<EditorSelection>().guids(),
        &[sprite_guid]
    );

    // 2. Click circle at world (160, 0) => viewport (400 + 160 = 560, 300).
    send_offscreen_click(&mut app, 560.0, 300.0, InputModifiers::default());
    assert_eq!(
        app.world().resource::<EditorSelection>().guids(),
        &[circle_guid]
    );

    // 3. Click 2D camera helper at world (-180, 0) => viewport (400 - 180 = 220, 300).
    send_offscreen_click(&mut app, 220.0, 300.0, InputModifiers::default());
    assert_eq!(
        app.world().resource::<EditorSelection>().guids(),
        &[cam2d_guid]
    );

    // 4. Click empty top-left corner -> clears selection.
    send_offscreen_click(&mut app, 15.0, 15.0, InputModifiers::default());
    assert!(app.world().resource::<EditorSelection>().guids().is_empty());
}
