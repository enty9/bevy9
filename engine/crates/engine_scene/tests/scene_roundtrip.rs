//! Integration round-trip, hydration, and sample scene tests for `engine_scene`.

use std::path::PathBuf;

use bevy_app::App;
use bevy_asset::{AssetApp, AssetPlugin};
use bevy_camera::{visibility::Visibility, Camera, Camera2d, Camera3d};
use bevy_color::Color;
use bevy_ecs::{
    hierarchy::{ChildOf, Children},
    name::Name,
};
use bevy_light::{DirectionalLight, PointLight};
use bevy_math::{Vec2, Vec3};
use bevy_mesh::{Mesh, Mesh2d, Mesh3d};
use bevy_pbr::{MeshMaterial3d, StandardMaterial};
use bevy_sprite::Sprite;
use bevy_sprite_render::{ColorMaterial, MeshMaterial2d};
use bevy_transform::components::Transform;
use engine_core::{EngineCorePlugin, EntityGuid, EntityGuidMap, WorldMode};
use engine_protocol::EntityPresetKind;
use engine_scene::prelude::*;

fn setup_scene_app(mode: WorldMode, allow_scene_cameras: bool) -> App {
    let mut app = App::new();
    app.insert_resource(mode)
        .insert_resource(SceneCameraActivationPolicy {
            allow_scene_cameras,
        })
        .add_plugins((
            AssetPlugin {
                use_asset_processor_override: Some(false),
                ..Default::default()
            },
            EngineCorePlugin,
            EngineScenePlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<StandardMaterial>()
        .init_asset::<ColorMaterial>();
    app
}

#[test]
fn roundtrip_3d_scene_preserves_guids_hierarchy_and_descriptors() {
    let mut source_app = setup_scene_app(WorldMode::Mode3D, true);

    let (_, floor_guid) = spawn_preset(
        source_app.world_mut(),
        EntityPresetKind::Plane3d,
        Some("Floor".to_owned()),
        None,
        Some(Transform::IDENTITY),
    );
    let (_, dir_light_guid) = spawn_preset(
        source_app.world_mut(),
        EntityPresetKind::DirectionalLight,
        Some("Sun".to_owned()),
        None,
        None,
    );
    let (group_entity, group_guid) = spawn_preset(
        source_app.world_mut(),
        EntityPresetKind::Empty,
        Some("Props".to_owned()),
        None,
        Some(Transform::from_xyz(0.0, 0.5, 0.0)),
    );
    let (_, cube_guid) = spawn_preset(
        source_app.world_mut(),
        EntityPresetKind::Cube3d,
        Some("HeroCube".to_owned()),
        Some(group_entity),
        Some(Transform::from_xyz(-1.2, 0.5, 0.0)),
    );
    let (_, cam_guid) = spawn_preset(
        source_app.world_mut(),
        EntityPresetKind::Camera3d,
        Some("MainCamera3D".to_owned()),
        None,
        None,
    );

    let ron = save_level_to_string(
        source_app.world(),
        &LevelMetadata {
            name: "Test 3D".to_owned(),
            world_mode: WorldMode::Mode3D,
        },
    )
    .expect("3D level should serialize cleanly");

    let mut target_app = setup_scene_app(WorldMode::Hybrid, true);
    let loaded_meta =
        load_level_from_str(target_app.world_mut(), &ron).expect("3D level should deserialize");
    assert_eq!(loaded_meta.name, "Test 3D");
    assert_eq!(loaded_meta.world_mode, WorldMode::Mode3D);
    assert_eq!(
        *target_app.world().resource::<WorldMode>(),
        WorldMode::Mode3D
    );

    // Run one update to trigger hydration.
    target_app.update();

    let world = target_app.world();
    let guid_map = world.resource::<EntityGuidMap>();
    assert_eq!(guid_map.len(), 5);
    let loaded_floor = guid_map.get_entity(floor_guid).expect("Floor GUID present");
    let loaded_sun = guid_map
        .get_entity(dir_light_guid)
        .expect("Sun GUID present");
    let loaded_group = guid_map.get_entity(group_guid).expect("Props GUID present");
    let loaded_cube = guid_map
        .get_entity(cube_guid)
        .expect("HeroCube GUID present");
    let loaded_cam = guid_map.get_entity(cam_guid).expect("Camera GUID present");

    assert_eq!(
        world.entity(loaded_floor).get::<Name>().map(Name::as_str),
        Some("Floor")
    );
    assert!(world.entity(loaded_floor).contains::<Mesh3d>());
    assert!(world
        .entity(loaded_floor)
        .contains::<MeshMaterial3d<StandardMaterial>>());
    assert!(world.entity(loaded_sun).contains::<DirectionalLight>());
    assert!(world.entity(loaded_cam).contains::<Camera3d>());
    assert!(world
        .entity(loaded_cam)
        .get::<Camera>()
        .is_some_and(|c| c.is_active));

    // Verify Parent <-> Child relationship restoration.
    assert_eq!(
        world
            .entity(loaded_cube)
            .get::<ChildOf>()
            .map(ChildOf::parent),
        Some(loaded_group)
    );
    assert!(world
        .entity(loaded_group)
        .get::<Children>()
        .is_some_and(|children| children.contains(&loaded_cube)));
    assert_eq!(
        world
            .entity(loaded_cube)
            .get::<Transform>()
            .expect("Transform preserved")
            .translation,
        Vec3::new(-1.2, 0.5, 0.0)
    );
    assert!(world.entity(loaded_cube).contains::<MeshPrimitive3d>());
    assert!(world.entity(loaded_cube).contains::<StandardMaterialDesc>());

    // Verify scene tree structure.
    let tree = build_scene_tree(world);
    assert_eq!(tree.len(), 4);
    let props_node = tree
        .iter()
        .find(|n| n.guid == group_guid)
        .expect("Props root node present");
    assert_eq!(props_node.children.len(), 1);
    assert_eq!(props_node.children[0].guid, cube_guid);
}

#[test]
fn roundtrip_2d_scene_preserves_guids_sprites_and_editor_camera_policy() {
    let mut source_app = setup_scene_app(WorldMode::Mode2D, false);

    let (group_entity, group_guid) = spawn_preset(
        source_app.world_mut(),
        EntityPresetKind::Empty,
        Some("Actors2D".to_owned()),
        None,
        Some(Transform::IDENTITY),
    );
    let (sprite_entity, sprite_guid) = spawn_preset(
        source_app.world_mut(),
        EntityPresetKind::Sprite2d,
        Some("PlayerSprite".to_owned()),
        Some(group_entity),
        Some(Transform::from_xyz(120.0, -40.0, 1.0)),
    );
    source_app
        .world_mut()
        .entity_mut(sprite_entity)
        .insert(SpriteDesc {
            color: Color::srgb(0.2, 0.9, 0.4),
            custom_size: Some(Vec2::new(64.0, 96.0)),
            flip_x: true,
            flip_y: false,
            ..Default::default()
        });

    let (_, circle_guid) = spawn_preset(
        source_app.world_mut(),
        EntityPresetKind::Circle2d,
        Some("Orb".to_owned()),
        Some(group_entity),
        Some(Transform::from_xyz(-100.0, 50.0, 0.0)),
    );
    let (_, cam_guid) = spawn_preset(
        source_app.world_mut(),
        EntityPresetKind::Camera2d,
        Some("MainCamera2D".to_owned()),
        None,
        None,
    );

    let ron = save_level_to_string(
        source_app.world(),
        &LevelMetadata {
            name: "Test 2D".to_owned(),
            world_mode: WorldMode::Mode2D,
        },
    )
    .expect("2D level should serialize cleanly");

    let mut target_app = setup_scene_app(WorldMode::Mode3D, false);
    let loaded_meta =
        load_level_from_str(target_app.world_mut(), &ron).expect("2D level should deserialize");
    assert_eq!(loaded_meta.world_mode, WorldMode::Mode2D);

    target_app.update();

    let world = target_app.world();
    let guid_map = world.resource::<EntityGuidMap>();
    assert_eq!(guid_map.len(), 4);

    let loaded_sprite = guid_map
        .get_entity(sprite_guid)
        .expect("Sprite GUID present");
    let loaded_circle = guid_map
        .get_entity(circle_guid)
        .expect("Circle GUID present");
    let loaded_group = guid_map.get_entity(group_guid).expect("Group GUID present");
    let loaded_cam = guid_map
        .get_entity(cam_guid)
        .expect("Camera2D GUID present");

    assert!(world.entity(loaded_sprite).contains::<Sprite>());
    assert_eq!(
        world
            .entity(loaded_sprite)
            .get::<Sprite>()
            .expect("hydrated sprite")
            .custom_size,
        Some(Vec2::new(64.0, 96.0))
    );
    assert!(world.entity(loaded_circle).contains::<Mesh2d>());
    assert!(world
        .entity(loaded_circle)
        .contains::<MeshMaterial2d<ColorMaterial>>());
    assert!(world.entity(loaded_cam).contains::<Camera2d>());
    // Editor policy (`allow_scene_cameras = false`) must keep scene camera inactive.
    assert!(world
        .entity(loaded_cam)
        .get::<Camera>()
        .is_some_and(|c| !c.is_active));
    assert_eq!(
        world
            .entity(loaded_group)
            .get::<Children>()
            .map(|children| children.len()),
        Some(2)
    );
    assert!(world.entity(loaded_sprite).get::<Visibility>().is_some());
    assert!(world.entity(loaded_circle).get::<EntityGuid>().is_some());
}

#[test]
fn rejects_unsupported_level_schema_version() {
    let mut app = setup_scene_app(WorldMode::Mode3D, true);
    let invalid_ron = r#"
(
    schema_version: 99,
    name: "Future Level",
    world_mode: "3d",
    world: (
        resources: {},
        entities: {},
    ),
)
"#;

    let err = load_level_from_str(app.world_mut(), invalid_ron)
        .expect_err("unsupported schema version must fail");
    assert!(matches!(err, SceneError::UnsupportedVersion { found: 99 }));
}

#[test]
fn sample_sandbox_scenes_load_cleanly() {
    let samples_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    let scene_3d = samples_dir.join("sandbox_3d/scenes/main.level.ron");
    let scene_2d = samples_dir.join("sandbox_2d/scenes/main.level.ron");

    assert!(scene_3d.is_file(), "missing {}", scene_3d.display());
    assert!(scene_2d.is_file(), "missing {}", scene_2d.display());

    // 3D sample verification
    {
        let mut app = setup_scene_app(WorldMode::Mode3D, true);
        let meta = load_level_from_file(app.world_mut(), &scene_3d)
            .expect("sandbox_3d main.level.ron should load");
        assert_eq!(meta.name, "Sandbox 3D");
        assert_eq!(meta.world_mode, WorldMode::Mode3D);
        app.update();

        let tree = build_scene_tree(app.world());
        let showcase = tree
            .iter()
            .find(|node| node.name == "ShowcaseGroup")
            .expect("ShowcaseGroup present in sandbox_3d");
        assert_eq!(showcase.children.len(), 2);
        assert_eq!(app.world().resource::<EntityGuidMap>().len(), 7);

        let world = app.world_mut();
        assert_eq!(world.query::<&Mesh3d>().iter(world).count(), 3);
        assert_eq!(world.query::<&DirectionalLight>().iter(world).count(), 1);
        assert_eq!(world.query::<&PointLight>().iter(world).count(), 1);
        assert_eq!(world.query::<&Camera3d>().iter(world).count(), 1);
    }

    // 2D sample verification
    {
        let mut app = setup_scene_app(WorldMode::Mode2D, true);
        let meta = load_level_from_file(app.world_mut(), &scene_2d)
            .expect("sandbox_2d main.level.ron should load");
        assert_eq!(meta.name, "Sandbox 2D");
        assert_eq!(meta.world_mode, WorldMode::Mode2D);
        app.update();

        let tree = build_scene_tree(app.world());
        let hero_group = tree
            .iter()
            .find(|node| node.name == "HeroGroup")
            .expect("HeroGroup present in sandbox_2d");
        assert_eq!(hero_group.children.len(), 2);
        assert_eq!(app.world().resource::<EntityGuidMap>().len(), 5);

        let world = app.world_mut();
        assert_eq!(world.query::<&Mesh2d>().iter(world).count(), 2);
        assert_eq!(world.query::<&Sprite>().iter(world).count(), 1);
        assert_eq!(world.query::<&Camera2d>().iter(world).count(), 1);
    }
}
