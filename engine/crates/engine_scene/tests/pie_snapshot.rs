//! Deterministic Play-In-Editor (PIE) snapshot capture and multi-cycle restore tests.

use std::path::PathBuf;

use bevy_app::App;
use bevy_asset::{AssetApp, AssetPlugin};
use bevy_camera::{visibility::Visibility, Camera3d};
use bevy_color::Color;
use bevy_ecs::{
    component::Component,
    hierarchy::{ChildOf, Children},
    name::Name,
    query::With,
};
use bevy_light::DirectionalLight;
use bevy_math::{Quat, Vec2, Vec3};
use bevy_mesh::{Mesh, Mesh3d};
use bevy_pbr::StandardMaterial;
use bevy_sprite::Sprite;
use bevy_sprite_render::ColorMaterial;
use bevy_transform::components::Transform;
use engine_audio::{AudioBus, AudioEmitter, AudioSpace};
use engine_core::{AssetUuid, EngineCorePlugin, EntityGuid, EntityGuidMap, WorldMode};
use engine_physics::{
    BodyKind, Collider2D, Collider3D, PhysicsMaterial, PhysicsWorld2D, PhysicsWorld3D, RigidBody2D,
    RigidBody3D,
};
use engine_protocol::{EntityPresetKind, SceneTreeNode};
use engine_scene::prelude::*;

#[derive(Component, Debug)]
struct EditorInfraMarker;

fn setup_pie_scene_app() -> App {
    let mut app = App::new();
    app.insert_resource(WorldMode::Hybrid)
        .insert_resource(SceneCameraActivationPolicy {
            allow_scene_cameras: false,
        })
        .init_resource::<PhysicsWorld2D>()
        .init_resource::<PhysicsWorld3D>()
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

fn normalize_tree_for_comparison(nodes: &[SceneTreeNode]) -> Vec<SceneTreeNode> {
    nodes
        .iter()
        .map(|node| SceneTreeNode {
            guid: node.guid,
            entity_bits: 0,
            name: node.name.clone(),
            parent_guid: node.parent_guid,
            visible: node.visible,
            children: normalize_tree_for_comparison(&node.children),
            components: node.components.clone(),
            prefab_path: node.prefab_path.clone(),
            is_prefab_member: node.is_prefab_member,
        })
        .collect()
}

fn count_alive_entities(app: &App) -> usize {
    app.world()
        .archetypes()
        .iter()
        .map(|arch| arch.len() as usize)
        .sum()
}

#[test]
fn pie_snapshot_10x_cycles_restore_exact_state_with_zero_leaks() {
    let mut app = setup_pie_scene_app();

    // Non-scene editor infrastructure entity that must survive every PIE restore untouched.
    let infra_entity = app
        .world_mut()
        .spawn((EditorInfraMarker, Name::new("EditorCameraStub")))
        .id();

    let initial_group_tfm = Transform::from_xyz(1.0, 2.0, -3.0);
    let (group_entity, group_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Empty,
        Some("Actors".to_owned()),
        None,
        Some(initial_group_tfm),
    );

    let initial_hero_tfm = Transform {
        translation: Vec3::new(-2.5, 4.0, 1.5),
        rotation: Quat::from_rotation_y(0.75),
        scale: Vec3::new(1.0, 2.0, 1.0),
    };
    let (hero_entity, hero_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Cube3d,
        Some("Hero".to_owned()),
        Some(group_entity),
        Some(initial_hero_tfm),
    );
    let hero_mat = StandardMaterialDesc {
        base_color: Color::srgb(0.85, 0.25, 0.15),
        metallic: 0.4,
        perceptual_roughness: 0.3,
        ..Default::default()
    };
    let hero_rb = RigidBody3D {
        kind: BodyKind::Dynamic,
        velocity: Vec3::new(0.0, 2.0, 0.0),
        gravity_scale: 1.5,
        ccd: true,
    };
    let hero_col = Collider3D::Cuboid(Vec3::splat(0.5));
    let hero_audio = AudioEmitter {
        clip: Some(AssetUuid::new()),
        bus: AudioBus::Sfx,
        space: AudioSpace::Scene3D,
        autoplay: true,
        looping: true,
        gain: 0.75,
        ..Default::default()
    };
    app.world_mut().entity_mut(hero_entity).insert((
        hero_mat,
        hero_rb,
        hero_col.clone(),
        hero_audio.clone(),
    ));

    let initial_weapon_tfm = Transform::from_xyz(0.5, 0.25, -0.75);
    let (weapon_entity, weapon_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Sphere3d,
        Some("Weapon".to_owned()),
        Some(hero_entity),
        Some(initial_weapon_tfm),
    );
    app.world_mut()
        .entity_mut(weapon_entity)
        .insert(Visibility::Hidden);

    let (floor_entity, floor_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Plane3d,
        Some("Floor".to_owned()),
        None,
        Some(Transform::IDENTITY),
    );
    let floor_mat = PhysicsMaterial {
        friction: 0.9,
        restitution: 0.2,
        density: 1.0,
    };
    app.world_mut().entity_mut(floor_entity).insert((
        Collider3D::Cuboid(Vec3::new(5.0, 0.1, 5.0)),
        floor_mat,
    ));

    let (_, cam_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Camera3d,
        Some("MainCamera".to_owned()),
        None,
        Some(Transform::from_xyz(0.0, 5.0, 10.0)),
    );
    let (_, sun_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::DirectionalLight,
        Some("Sun".to_owned()),
        None,
        None,
    );
    let (sprite_entity, sprite_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Sprite2d,
        Some("HudBadge".to_owned()),
        None,
        Some(Transform::from_xyz(32.0, -16.0, 1.0)),
    );
    let sprite_desc = SpriteDesc {
        color: Color::srgb(0.1, 0.9, 0.4),
        custom_size: Some(Vec2::new(48.0, 48.0)),
        flip_x: true,
        ..Default::default()
    };
    app.world_mut().entity_mut(sprite_entity).insert((
        sprite_desc,
        RigidBody2D::default(),
        Collider2D::Ball(24.0),
    ));

    let expected_active_scene = ActiveScene {
        path: Some(PathBuf::from("scenes/pie_cycle_test.level.ron")),
        name: "PIE Cycle Scene".to_owned(),
        world_mode: WorldMode::Hybrid,
        dirty: true,
    };
    app.world_mut()
        .insert_resource(expected_active_scene.clone());

    // Hydrate all descriptors before capturing baseline counts.
    app.update();

    let baseline_total_entities = count_alive_entities(&app);
    let baseline_tree = normalize_tree_for_comparison(&build_scene_tree(app.world()));
    let snapshot = capture_pie_snapshot(app.world()).expect("PIE snapshot capture must succeed");
    assert_eq!(snapshot.entity_count, 7);
    assert_eq!(snapshot.entity_guids.len(), 7);
    assert_eq!(snapshot.active_scene, expected_active_scene);

    for cycle in 0..10 {
        let world = app.world_mut();

        // 1. Mutate existing entities: transform, name, material, visibility, reparenting.
        let hero_ent = world
            .resource::<EntityGuidMap>()
            .get_entity(hero_guid)
            .expect("Hero present at start of cycle");
        let weapon_ent = world
            .resource::<EntityGuidMap>()
            .get_entity(weapon_guid)
            .expect("Weapon present at start of cycle");
        let floor_ent = world
            .resource::<EntityGuidMap>()
            .get_entity(floor_guid)
            .expect("Floor present at start of cycle");
        let group_ent = world
            .resource::<EntityGuidMap>()
            .get_entity(group_guid)
            .expect("Group present at start of cycle");

        *world.get_mut::<Transform>(hero_ent).unwrap() =
            Transform::from_xyz(100.0 + cycle as f32, -50.0, 25.0);
        *world.get_mut::<Name>(hero_ent).unwrap() = Name::new(format!("MutatedHero_{cycle}"));
        world.get_mut::<StandardMaterialDesc>(hero_ent).unwrap().base_color =
            Color::srgb(0.0, 0.0, 1.0);
        *world.get_mut::<Visibility>(weapon_ent).unwrap() = Visibility::Visible;
        world.entity_mut(weapon_ent).remove::<ChildOf>();

        // 2. Delete an authored entity during PIE.
        despawn_subtree(world, floor_ent);

        // 3. Spawn new SceneEntities (root + parented).
        let (spawned_root, _) = spawn_preset(
            world,
            EntityPresetKind::Cube3d,
            Some(format!("TempCrate_{cycle}")),
            None,
            Some(Transform::from_xyz(cycle as f32, 1.0, 0.0)),
        );
        let _ = spawn_preset(
            world,
            EntityPresetKind::Sphere3d,
            Some(format!("TempChild_{cycle}")),
            Some(group_ent),
            None,
        );
        // Attach a non-SceneEntity child directly under a SceneEntity.
        world.spawn((Name::new("TransientMeshChild"), ChildOf(spawned_root)));

        // 4. Spawn transient PieRuntimeEntity subtree without SceneEntity.
        let runtime_root = world
            .spawn((PieRuntimeEntity, Name::new(format!("Projectile_{cycle}"))))
            .id();
        world.spawn((Name::new("ProjectileTrail"), ChildOf(runtime_root)));

        // 5. Spawn detached hydrated children to ensure orphan cleanup.
        let orphan_gltf = world.spawn(HydratedGltfModelRoot).id();
        world.spawn((Name::new("OrphanMesh"), ChildOf(orphan_gltf)));
        world.spawn(HydratedTilemapTile);

        // 6. Clobber ActiveScene metadata.
        world.insert_resource(ActiveScene {
            path: None,
            name: "Corrupted".to_owned(),
            world_mode: WorldMode::Mode2D,
            dirty: false,
        });

        app.update();
        assert!(
            count_alive_entities(&app) > baseline_total_entities,
            "cycle {cycle}: PIE mutations should have spawned extra entities"
        );

        // 7. Restore exact pre-PIE snapshot and re-hydrate.
        restore_pie_snapshot(app.world_mut(), &snapshot)
            .unwrap_or_else(|err| panic!("cycle {cycle}: restore_pie_snapshot failed: {err}"));
        app.update();

        // 8. Verify zero leaked entities and exact bit-for-bit scene restoration.
        let world = app.world();
        assert!(
            world.get_entity(infra_entity).is_ok(),
            "cycle {cycle}: non-scene infrastructure entity must survive restore"
        );
        assert_eq!(
            count_alive_entities(&app),
            baseline_total_entities,
            "cycle {cycle}: total entity count must match baseline with zero leaks"
        );

        let guid_map = world.resource::<EntityGuidMap>();
        assert_eq!(guid_map.len(), snapshot.entity_guids.len());
        let mut live_guids: Vec<EntityGuid> = guid_map.iter().map(|(g, _)| g).collect();
        live_guids.sort_unstable();
        assert_eq!(live_guids, snapshot.entity_guids);

        let restored_group = guid_map.get_entity(group_guid).expect("Group restored");
        let restored_hero = guid_map.get_entity(hero_guid).expect("Hero restored");
        let restored_weapon = guid_map.get_entity(weapon_guid).expect("Weapon restored");
        let restored_floor = guid_map.get_entity(floor_guid).expect("Floor restored");
        let restored_cam = guid_map.get_entity(cam_guid).expect("Camera restored");
        let restored_sun = guid_map.get_entity(sun_guid).expect("Sun restored");
        let restored_sprite = guid_map.get_entity(sprite_guid).expect("Sprite restored");

        assert_eq!(
            world.get::<Transform>(restored_group),
            Some(&initial_group_tfm)
        );
        assert_eq!(
            world.get::<Transform>(restored_hero),
            Some(&initial_hero_tfm)
        );
        assert_eq!(
            world.get::<Name>(restored_hero).map(Name::as_str),
            Some("Hero")
        );
        assert_eq!(
            world.get::<StandardMaterialDesc>(restored_hero),
            Some(&hero_mat)
        );
        assert_eq!(world.get::<RigidBody3D>(restored_hero), Some(&hero_rb));
        assert_eq!(world.get::<Collider3D>(restored_hero), Some(&hero_col));
        assert_eq!(world.get::<AudioEmitter>(restored_hero), Some(&hero_audio));
        assert!(world.entity(restored_hero).contains::<Mesh3d>());

        // Hierarchy ChildOf + Children links fully restored.
        assert_eq!(
            world.get::<ChildOf>(restored_hero).map(ChildOf::parent),
            Some(restored_group)
        );
        assert!(world
            .get::<Children>(restored_group)
            .is_some_and(|children| children.len() == 1 && children.contains(&restored_hero)));
        assert_eq!(
            world.get::<ChildOf>(restored_weapon).map(ChildOf::parent),
            Some(restored_hero)
        );
        assert!(world
            .get::<Children>(restored_hero)
            .is_some_and(|children| children.len() == 1 && children.contains(&restored_weapon)));
        assert_eq!(
            world.get::<Transform>(restored_weapon),
            Some(&initial_weapon_tfm)
        );
        assert_eq!(
            world.get::<Visibility>(restored_weapon),
            Some(&Visibility::Hidden)
        );

        assert_eq!(
            world.get::<PhysicsMaterial>(restored_floor),
            Some(&floor_mat)
        );
        assert!(world.entity(restored_cam).contains::<Camera3d>());
        assert!(world.entity(restored_sun).contains::<DirectionalLight>());
        assert_eq!(world.get::<SpriteDesc>(restored_sprite), Some(&sprite_desc));
        assert!(world.entity(restored_sprite).contains::<Sprite>());

        assert_eq!(
            world.resource::<ActiveScene>(),
            &expected_active_scene,
            "cycle {cycle}: ActiveScene metadata and dirty flag must be restored"
        );
        assert_eq!(
            *world.resource::<WorldMode>(),
            expected_active_scene.world_mode
        );

        let restored_tree = normalize_tree_for_comparison(&build_scene_tree(world));
        assert_eq!(
            restored_tree, baseline_tree,
            "cycle {cycle}: restored hierarchy tree must match baseline"
        );

        let world_mut = app.world_mut();
        assert_eq!(
            world_mut
                .query_filtered::<(), With<PieRuntimeEntity>>()
                .iter(world_mut)
                .count(),
            0
        );
        assert_eq!(
            world_mut
                .query_filtered::<(), With<HydratedGltfModelRoot>>()
                .iter(world_mut)
                .count(),
            0
        );
        assert_eq!(
            world_mut
                .query_filtered::<(), With<HydratedTilemapTile>>()
                .iter(world_mut)
                .count(),
            0
        );
    }
}
