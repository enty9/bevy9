//! Serialization, override tracking, Apply sibling propagation, Revert, and level roundtrip tests for `.prefab.ron`.

use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use bevy_app::App;
use bevy_color::Color;
use bevy_ecs::{entity::Entity, hierarchy::ChildOf, name::Name};
use bevy_math::Vec3;
use bevy_transform::components::Transform;
use engine_core::{EngineCorePlugin, EntityGuid, EntityGuidMap, WorldMode};
use engine_scene::{
    apply_prefab_instance, list_prefab_files, load_level_from_str,
    prefab_field_override_key, reset_to_new_scene, revert_prefab_instance, save_level_to_string,
    save_prefab_to_file, spawn_prefab_from_file, sync_prefab_instance_overrides, EngineScenePlugin,
    LevelMetadata, MeshPrimitive3d, PrefabInstance, PrefabMember, SceneEntity,
    StandardMaterialDesc,
};

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!("{prefix}_{}_{nanos}", std::process::id()));
        fs::create_dir_all(&path).expect("temp dir should create");
        Self { path }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn setup_scene_app() -> App {
    let mut app = App::new();
    app.insert_resource(WorldMode::Mode3D)
        .add_plugins((EngineCorePlugin, EngineScenePlugin));
    reset_to_new_scene(app.world_mut(), "PrefabTest", WorldMode::Mode3D);
    app
}

#[test]
fn prefab_save_spawn_overrides_apply_propagation_revert_and_level_roundtrip() {
    let tmp = TempDirGuard::new("bevy9_prefab_roundtrip");
    let prefab_rel = "prefabs/crate.prefab.ron";
    let prefab_abs = tmp.path.join(prefab_rel);

    let mut app = setup_scene_app();

    // 1. Spawn a parent+child subtree and save it as `.prefab.ron`.
    let parent_guid = EntityGuid::new();
    let child_guid = EntityGuid::new();
    let parent_ent = app
        .world_mut()
        .spawn((
            SceneEntity,
            parent_guid,
            Name::new("Crate"),
            Transform::from_xyz(0.0, 1.0, 0.0),
            MeshPrimitive3d::default(),
            StandardMaterialDesc::default(),
        ))
        .id();
    let child_ent = app
        .world_mut()
        .spawn((
            SceneEntity,
            child_guid,
            Name::new("Badge"),
            Transform::from_xyz(0.5, 0.0, 0.0),
            StandardMaterialDesc {
                metallic: 0.25,
                ..Default::default()
            },
            ChildOf(parent_ent),
        ))
        .id();

    let (root_src_guid, count) = save_prefab_to_file(
        app.world_mut(),
        parent_ent,
        &prefab_abs,
        prefab_rel,
        Some("CratePrefab"),
    )
    .expect("saving prefab should succeed");
    assert_eq!(count, 2);
    assert_eq!(root_src_guid, parent_guid);
    assert!(prefab_abs.exists());

    let inst_orig = app.world().get::<PrefabInstance>(parent_ent).unwrap();
    assert_eq!(inst_orig.prefab_path, prefab_rel);
    assert!(inst_orig.overrides.is_empty());
    assert_eq!(
        app.world().get::<PrefabMember>(child_ent).unwrap().instance_root_guid,
        parent_guid
    );

    let listed = list_prefab_files(&tmp.path);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].relative_path, prefab_rel);

    // 2. Spawn sibling Instance A (default position) and Instance B at [5, 0, 0].
    let (inst_a, guid_a) = spawn_prefab_from_file(
        app.world_mut(),
        &prefab_abs,
        prefab_rel,
        None,
        None,
    )
    .expect("spawn instance A should succeed");
    let (inst_b, guid_b) = spawn_prefab_from_file(
        app.world_mut(),
        &prefab_abs,
        prefab_rel,
        None,
        Some(Vec3::new(5.0, 0.0, 0.0)),
    )
    .expect("spawn instance B should succeed");

    // Distinct live GUIDs across original + A + B (6 entities total).
    let all_live_guids: HashSet<EntityGuid> = app
        .world()
        .resource::<EntityGuidMap>()
        .iter()
        .map(|(g, _)| g)
        .collect();
    assert_eq!(all_live_guids.len(), 6);
    assert_ne!(guid_a, guid_b);

    // Spawn position [5, 0, 0] != template [0, 1, 0], so Instance B immediately records Transform::translation override.
    let tfm_override_key = prefab_field_override_key(
        root_src_guid,
        "bevy_transform::components::transform::Transform",
        "translation",
    );
    assert!(
        app.world()
            .get::<PrefabInstance>(inst_b)
            .unwrap()
            .overrides
            .contains_key(&tfm_override_key)
    );

    // 3. Override metallic = 0.9 on Instance B and resync overrides.
    app.world_mut()
        .get_mut::<StandardMaterialDesc>(inst_b)
        .unwrap()
        .metallic = 0.9;
    assert!(sync_prefab_instance_overrides(app.world_mut(), inst_b, &prefab_abs).unwrap());
    let mat_override_key = prefab_field_override_key(
        root_src_guid,
        "engine_scene::components::StandardMaterialDesc",
        "metallic",
    );
    assert!(
        app.world()
            .get::<PrefabInstance>(inst_b)
            .unwrap()
            .overrides
            .contains_key(&mat_override_key)
    );

    // 4. Modify non-overridden fields on Instance A (base_color + child Name) and Apply to Prefab!
    let updated_color = Color::srgb(0.2, 0.8, 0.4);
    app.world_mut()
        .get_mut::<StandardMaterialDesc>(inst_a)
        .unwrap()
        .base_color = updated_color;

    let child_a: Entity = app
        .world()
        .resource::<EntityGuidMap>()
        .iter()
        .find_map(|(_, e)| {
            app.world()
                .get::<PrefabMember>(e)
                .filter(|m| m.instance_root_guid == guid_a && m.source_guid == child_guid)
                .map(|_| e)
        })
        .unwrap();
    app.world_mut()
        .get_mut::<Name>(child_a)
        .unwrap()
        .set("BadgeApplied");

    let updated = apply_prefab_instance(app.world_mut(), inst_a, &prefab_abs)
        .expect("apply should succeed");
    assert!(!updated.is_empty());

    // Instance B inherits updated base_color & child Name, while KEEPING local translation=[5,0,0] and metallic=0.9!
    let mat_b = app.world().get::<StandardMaterialDesc>(inst_b).unwrap();
    assert_eq!(mat_b.base_color, updated_color);
    assert!((mat_b.metallic - 0.9).abs() < 1e-5);
    assert_eq!(
        app.world().get::<Transform>(inst_b).unwrap().translation,
        Vec3::new(5.0, 0.0, 0.0)
    );

    let child_b: Entity = app
        .world()
        .resource::<EntityGuidMap>()
        .iter()
        .find_map(|(_, e)| {
            app.world()
                .get::<PrefabMember>(e)
                .filter(|m| m.instance_root_guid == guid_b && m.source_guid == child_guid)
                .map(|_| e)
        })
        .unwrap();
    assert_eq!(app.world().get::<Name>(child_b).unwrap().as_str(), "BadgeApplied");

    // Original parent entity also inherits changes (cleanly keeping nothing overridden).
    assert_eq!(
        app.world().get::<StandardMaterialDesc>(parent_ent).unwrap().base_color,
        updated_color
    );

    // 5. `.level.ron` save -> clear -> load roundtrip preserves PrefabInstance, PrefabMember, and overrides on B.
    let saved_level = save_level_to_string(
        app.world(),
        &LevelMetadata {
            name: "PrefabLevel".to_owned(),
            world_mode: WorldMode::Mode3D,
        },
    )
    .expect("level save with prefabs should succeed");

    load_level_from_str(app.world_mut(), &saved_level).expect("level reload should succeed");
    let reloaded_b = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(guid_b)
        .expect("instance B GUID should survive level roundtrip");
    let reloaded_inst_b = app
        .world()
        .get::<PrefabInstance>(reloaded_b)
        .expect("PrefabInstance should survive level roundtrip");
    assert_eq!(reloaded_inst_b.prefab_path, prefab_rel);
    assert!(reloaded_inst_b.overrides.contains_key(&tfm_override_key));
    assert!(reloaded_inst_b.overrides.contains_key(&mat_override_key));

    // 6. Single-component revert (`StandardMaterialDesc`) on B resets metallic to 0.0 while keeping Transform override.
    revert_prefab_instance(
        app.world_mut(),
        reloaded_b,
        &prefab_abs,
        Some((
            root_src_guid,
            "engine_scene::components::StandardMaterialDesc",
        )),
    )
    .expect("single-component revert should succeed");
    assert!((app.world().get::<StandardMaterialDesc>(reloaded_b).unwrap().metallic - 0.0).abs() < 1e-5);
    assert_eq!(
        app.world().get::<Transform>(reloaded_b).unwrap().translation,
        Vec3::new(5.0, 0.0, 0.0)
    );

    // 7. Full revert on B resets Transform to template [0, 1, 0] and empties overrides.
    revert_prefab_instance(app.world_mut(), reloaded_b, &prefab_abs, None)
        .expect("full revert should succeed");
    assert_eq!(
        app.world().get::<Transform>(reloaded_b).unwrap().translation,
        Vec3::new(0.0, 1.0, 0.0)
    );
    assert!(
        app.world()
            .get::<PrefabInstance>(reloaded_b)
            .unwrap()
            .overrides
            .is_empty()
    );
}
