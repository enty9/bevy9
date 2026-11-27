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
use engine_audio::{AudioEmitter, AudioListener, AudioSpace};
use engine_core::{AssetUuid, EngineCorePlugin, EntityGuid, EntityGuidMap, WorldMode};
use engine_physics::{
    BodyKind, CharacterController2D, CharacterController3D, Collider2D, Collider3D,
    CollisionLayers, PhysicsJoint2D, PhysicsJoint3D, PhysicsMaterial, RigidBody2D, RigidBody3D,
    TriggerVolume3D,
};
use engine_protocol::EntityPresetKind;
use engine_scene::prelude::*;
use serde_json::json;

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
        assert_eq!(app.world().resource::<EntityGuidMap>().len(), 8);

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
        assert_eq!(app.world().resource::<EntityGuidMap>().len(), 7);

        let world = app.world_mut();
        assert_eq!(world.query::<&Mesh2d>().iter(world).count(), 3);
        assert_eq!(
            world
                .query::<&MeshMaterial2d<ColorMaterial>>()
                .iter(world)
                .count(),
            3
        );
        assert_eq!(world.query::<&Sprite>().iter(world).count(), 1);
        assert_eq!(world.query::<&Camera2d>().iter(world).count(), 1);
    }
}

#[test]
fn stage_five_descriptors_round_trip_with_stable_scene_and_asset_references() {
    let mut app = setup_scene_app(WorldMode::Mode3D, true);
    let anchor = EntityGuid::new();
    let moving = EntityGuid::new();
    let clip = AssetUuid::new();
    let emitter = AudioEmitter {
        clip: Some(clip),
        space: AudioSpace::Scene3D,
        streaming: true,
        ..Default::default()
    };
    let world = app.world_mut();
    world.spawn((
        SceneEntity,
        anchor,
        Transform::IDENTITY,
        AudioListener::default(),
    ));
    world.spawn((
        SceneEntity,
        moving,
        RigidBody3D {
            kind: BodyKind::Kinematic,
            ..Default::default()
        },
        Collider3D::Ball(1.2),
        TriggerVolume3D,
        PhysicsJoint3D {
            target: Some(anchor),
            ..Default::default()
        },
        emitter.clone(),
    ));
    let meta = LevelMetadata {
        name: "Gameplay descriptors".into(),
        world_mode: WorldMode::Mode3D,
    };
    let serialized = save_level_to_string(world, &meta).unwrap();
    assert_eq!(load_level_from_str(world, &serialized).unwrap(), meta);
    let moving = world
        .resource::<EntityGuidMap>()
        .get_entity(moving)
        .unwrap();
    assert_eq!(world.get::<AudioEmitter>(moving), Some(&emitter));
    assert_eq!(
        world.get::<Collider3D>(moving),
        Some(&Collider3D::Ball(1.2))
    );
    assert_eq!(
        world.get::<PhysicsJoint3D>(moving).unwrap().target,
        Some(anchor)
    );
    assert!(world.get::<TriggerVolume3D>(moving).is_some());
}

#[test]
fn stage_five_inspector_default_json_payloads_deserialize_to_defaults() {
    let mut app = setup_scene_app(WorldMode::Hybrid, true);
    let entity = app.world_mut().spawn((SceneEntity, EntityGuid::new())).id();

    let payloads = [
        (
            "engine_audio::AudioEmitter",
            json!({
                "clip": null,
                "bus": "Sfx",
                "space": "Global",
                "autoplay": false,
                "looping": false,
                "streaming": false,
                "gain": 1.0,
                "gain_variation": 0.0,
                "pitch": 1.0,
                "pitch_variation": 0.0,
                "min_distance": 1.0,
                "max_distance": 100.0
            }),
        ),
        (
            "engine_audio::AudioListener",
            json!({
                "enabled": true,
                "priority": 0
            }),
        ),
        (
            "engine_physics::RigidBody2D",
            json!({
                "kind": "Dynamic",
                "velocity": [0.0, 0.0],
                "gravity_scale": 1.0,
                "ccd": false
            }),
        ),
        (
            "engine_physics::RigidBody3D",
            json!({
                "kind": "Dynamic",
                "velocity": [0.0, 0.0, 0.0],
                "gravity_scale": 1.0,
                "ccd": false
            }),
        ),
        (
            "engine_physics::Collider2D",
            json!({
                "Cuboid": [16.0, 16.0]
            }),
        ),
        (
            "engine_physics::Collider3D",
            json!({
                "Cuboid": [0.5, 0.5, 0.5]
            }),
        ),
        (
            "engine_physics::PhysicsMaterial",
            json!({
                "friction": 0.5,
                "restitution": 0.0,
                "density": 1.0
            }),
        ),
        (
            "engine_physics::CollisionLayers",
            json!({
                "memberships": 4_294_967_295_u32,
                "filter": 4_294_967_295_u32
            }),
        ),
        (
            "engine_physics::CharacterController2D",
            json!({
                "platformer": true,
                "slope_degrees": 45.0,
                "ground_snap": 2.0
            }),
        ),
        (
            "engine_physics::CharacterController3D",
            json!({
                "slope_degrees": 45.0,
                "step_height": 0.3,
                "ground_snap": 0.15
            }),
        ),
        (
            "engine_physics::PhysicsJoint2D",
            json!({
                "target": null,
                "kind": "Fixed",
                "anchor": [0.0, 0.0],
                "target_anchor": [0.0, 0.0]
            }),
        ),
        (
            "engine_physics::PhysicsJoint3D",
            json!({
                "target": null,
                "kind": "Fixed",
                "anchor": [0.0, 0.0, 0.0],
                "target_anchor": [0.0, 0.0, 0.0]
            }),
        ),
    ];

    for (type_path, value) in &payloads {
        write_reflected_component_json(app.world_mut(), entity, type_path, value)
            .unwrap_or_else(|err| panic!("failed to write default payload for {type_path}: {err}"));
    }

    let world = app.world();
    assert_eq!(
        world.get::<AudioEmitter>(entity),
        Some(&AudioEmitter::default())
    );
    let listener = world.get::<AudioListener>(entity).expect("AudioListener");
    let listener_def = AudioListener::default();
    assert_eq!(listener.enabled, listener_def.enabled);
    assert_eq!(listener.priority, listener_def.priority);

    assert_eq!(
        world.get::<RigidBody2D>(entity),
        Some(&RigidBody2D::default())
    );
    assert_eq!(
        world.get::<RigidBody3D>(entity),
        Some(&RigidBody3D::default())
    );
    assert_eq!(
        world.get::<Collider2D>(entity),
        Some(&Collider2D::default())
    );
    assert_eq!(
        world.get::<Collider3D>(entity),
        Some(&Collider3D::default())
    );
    assert_eq!(
        world.get::<PhysicsMaterial>(entity),
        Some(&PhysicsMaterial::default())
    );
    assert_eq!(
        world.get::<CollisionLayers>(entity),
        Some(&CollisionLayers::default())
    );

    let ctrl2 = world
        .get::<CharacterController2D>(entity)
        .expect("CharacterController2D");
    let ctrl2_def = CharacterController2D::default();
    assert_eq!(ctrl2.platformer, ctrl2_def.platformer);
    assert_eq!(ctrl2.slope_degrees.to_bits(), ctrl2_def.slope_degrees.to_bits());
    assert_eq!(ctrl2.ground_snap.to_bits(), ctrl2_def.ground_snap.to_bits());

    let ctrl3 = world
        .get::<CharacterController3D>(entity)
        .expect("CharacterController3D");
    let ctrl3_def = CharacterController3D::default();
    assert_eq!(ctrl3.slope_degrees.to_bits(), ctrl3_def.slope_degrees.to_bits());
    assert_eq!(ctrl3.step_height.to_bits(), ctrl3_def.step_height.to_bits());
    assert_eq!(ctrl3.ground_snap.to_bits(), ctrl3_def.ground_snap.to_bits());

    assert_eq!(
        world.get::<PhysicsJoint2D>(entity),
        Some(&PhysicsJoint2D::default())
    );
    assert_eq!(
        world.get::<PhysicsJoint3D>(entity),
        Some(&PhysicsJoint3D::default())
    );
}
