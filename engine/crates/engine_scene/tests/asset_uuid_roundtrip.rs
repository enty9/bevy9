//! Integration tests verifying `AssetUuid` roundtrip, prefab UUID tracking, relocation updates, and tilemap hydration.

use std::{
    fs,
    path::{Path, PathBuf},
};

use bevy_app::App;
use bevy_asset::{AssetApp, AssetPlugin};
use bevy_camera::visibility::Visibility;
use bevy_ecs::{entity::Entity, hierarchy::ChildOf, name::Name};
use bevy_image::{Image, TextureAtlasLayout};
use bevy_math::Vec3;
use bevy_mesh::Mesh;
use bevy_pbr::StandardMaterial;
use bevy_sprite::Sprite;
use bevy_sprite_render::ColorMaterial;
use bevy_transform::components::Transform;
use engine_assets::{
    save_tilemap_to_file, AssetRegistry, EngineAssetsPlugin, TileMetadata, TilemapAsset,
    TilemapLayer, CURRENT_TILEMAP_VERSION,
};
use engine_core::{AssetUuid, EngineCorePlugin, EntityGuid, ProjectPaths, WorldMode};
use engine_scene::prelude::*;

struct TempProjectDir {
    root: PathBuf,
}

impl TempProjectDir {
    fn new(prefix: &str) -> Self {
        let root = std::env::temp_dir().join(format!("bevy9_{prefix}_{}", AssetUuid::new()));
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::create_dir_all(root.join("scenes")).unwrap();
        fs::create_dir_all(root.join("scripts")).unwrap();
        Self { root }
    }

    fn paths(&self) -> ProjectPaths {
        ProjectPaths {
            root_dir: self.root.clone(),
            manifest_path: self.root.join("project.toml"),
            assets_dir: self.root.join("assets"),
            scenes_dir: self.root.join("scenes"),
            scripts_dir: self.root.join("scripts"),
        }
    }
}

impl Drop for TempProjectDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn minimal_png_bytes() -> Vec<u8> {
    vec![
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, b'I', b'D', b'A', b'T', 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x50, 0x0F, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xA9, 0x8C, 0x21, 0x00, 0x00,
        0x00, 0x00, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82,
    ]
}

fn setup_asset_scene_app(paths: &ProjectPaths, mode: WorldMode) -> App {
    let mut app = App::new();
    app.insert_resource(mode)
        .insert_resource(paths.clone())
        .insert_resource(SceneCameraActivationPolicy {
            allow_scene_cameras: false,
        })
        .add_plugins((
            bevy_app::TaskPoolPlugin::default(),
            AssetPlugin {
                file_path: paths.assets_dir.to_string_lossy().into_owned(),
                use_asset_processor_override: Some(false),
                ..Default::default()
            },
            EngineCorePlugin,
            EngineAssetsPlugin,
            EngineScenePlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<TextureAtlasLayout>()
        .init_asset::<StandardMaterial>()
        .init_asset::<ColorMaterial>()
        .init_asset::<bevy_world_serialization::WorldAsset>();
    app
}

fn spawn_sample_prefab_file(app: &mut App, prefab_abs: &Path) -> (Entity, AssetUuid) {
    let prefab_source_entity = app
        .world_mut()
        .spawn((
            SceneEntity,
            EntityGuid::new(),
            Name::new("CrateRoot"),
            Transform::IDENTITY,
            Visibility::default(),
            MeshPrimitive3d::default(),
            StandardMaterialDesc::default(),
        ))
        .id();
    let project_root = app
        .world()
        .resource::<ProjectPaths>()
        .root_dir
        .clone();
    let key = normalize_prefab_path(prefab_abs, Some(&project_root));
    save_prefab_to_file(
        app.world_mut(),
        prefab_source_entity,
        prefab_abs,
        &key,
        Some("CratePrefab"),
    )
    .unwrap();
    app.world_mut().resource_mut::<AssetRegistry>().scan_all().unwrap();
    let rel = normalize_prefab_path(prefab_abs, Some(&app.world().resource::<ProjectPaths>().assets_dir));
    let uuid = app
        .world()
        .resource::<AssetRegistry>()
        .shared()
        .get_by_path(&rel)
        .unwrap()
        .uuid;
    (prefab_source_entity, uuid)
}

#[test]
fn level_roundtrip_preserves_asset_uuids_and_updates_moved_prefab_path() {
    let temp = TempProjectDir::new("asset_uuid_roundtrip");
    let paths = temp.paths();
    fs::write(paths.assets_dir.join("albedo.png"), minimal_png_bytes()).unwrap();
    fs::write(paths.assets_dir.join("sheet.png"), minimal_png_bytes()).unwrap();

    let mut registry = AssetRegistry::new(paths.assets_dir.clone(), Some(paths.scenes_dir.clone()));
    registry.scan_all().unwrap();
    let tex_uuid = registry.shared().get_by_path("albedo.png").unwrap().uuid;
    let sheet_uuid = registry.shared().get_by_path("sheet.png").unwrap().uuid;

    let gltf_uuid = AssetUuid::new();
    let tilemap_uuid = AssetUuid::new();

    let mut app = setup_asset_scene_app(&paths, WorldMode::Hybrid);
    let prefab_abs = paths.assets_dir.join("prefabs/crate.prefab.ron");
    let (prefab_entity, prefab_uuid) = spawn_sample_prefab_file(&mut app, &prefab_abs);

    // Ensure the prefab root received its persistent AssetUuid.
    {
        let inst = app.world().get::<PrefabInstance>(prefab_entity).unwrap();
        assert_eq!(inst.prefab_uuid, Some(prefab_uuid));
    }

    let mat_guid = EntityGuid::new();
    app.world_mut().spawn((
        SceneEntity,
        mat_guid,
        Name::new("TexturedMesh"),
        Transform::IDENTITY,
        Visibility::default(),
        MeshPrimitive3d::default(),
        StandardMaterialDesc {
            base_color_texture: Some(tex_uuid),
            ..Default::default()
        },
    ));

    let sprite_guid = EntityGuid::new();
    app.world_mut().spawn((
        SceneEntity,
        sprite_guid,
        Name::new("AtlasSprite"),
        Transform::IDENTITY,
        Visibility::default(),
        SpriteDesc {
            texture: Some(sheet_uuid),
            atlas_index: Some(3),
            ..Default::default()
        },
    ));

    let gltf_guid = EntityGuid::new();
    app.world_mut().spawn((
        SceneEntity,
        gltf_guid,
        Name::new("ModelNode"),
        Transform::IDENTITY,
        Visibility::default(),
        SceneGltfModel {
            asset: Some(gltf_uuid),
            scene_index: 1,
            scale_multiplier: 2.5,
        },
    ));

    let tilemap_guid = EntityGuid::new();
    app.world_mut().spawn((
        SceneEntity,
        tilemap_guid,
        Name::new("TileLayer"),
        Transform::IDENTITY,
        Visibility::default(),
        SceneTilemap {
            asset: Some(tilemap_uuid),
            tile_scale: 1.5,
        },
    ));

    let scene_path = paths.scenes_dir.join("roundtrip.level.ron");
    let metadata = LevelMetadata {
        name: "Roundtrip".to_owned(),
        world_mode: WorldMode::Hybrid,
    };
    save_level_to_file(app.world_mut(), &metadata, &scene_path).unwrap();

    // Reload into a fresh app and verify all AssetUuid fields survive serialization intact.
    let mut reloaded_app = setup_asset_scene_app(&paths, WorldMode::Hybrid);
    load_level_from_file(reloaded_app.world_mut(), &scene_path).unwrap();
    reloaded_app.update();

    let mat_desc = *reloaded_app
        .world()
        .entity(
            reloaded_app
                .world()
                .resource::<engine_core::EntityGuidMap>()
                .get_entity(mat_guid)
                .unwrap(),
        )
        .get::<StandardMaterialDesc>()
        .unwrap();
    assert_eq!(mat_desc.base_color_texture, Some(tex_uuid));

    let sprite_desc = *reloaded_app
        .world()
        .entity(
            reloaded_app
                .world()
                .resource::<engine_core::EntityGuidMap>()
                .get_entity(sprite_guid)
                .unwrap(),
        )
        .get::<SpriteDesc>()
        .unwrap();
    assert_eq!(sprite_desc.texture, Some(sheet_uuid));
    assert_eq!(sprite_desc.atlas_index, Some(3));

    let gltf_desc = *reloaded_app
        .world()
        .entity(
            reloaded_app
                .world()
                .resource::<engine_core::EntityGuidMap>()
                .get_entity(gltf_guid)
                .unwrap(),
        )
        .get::<SceneGltfModel>()
        .unwrap();
    assert_eq!(gltf_desc.asset, Some(gltf_uuid));
    assert_eq!(gltf_desc.scene_index, 1);
    assert!((gltf_desc.scale_multiplier - 2.5).abs() < 1e-5);

    let tilemap_desc = *reloaded_app
        .world()
        .entity(
            reloaded_app
                .world()
                .resource::<engine_core::EntityGuidMap>()
                .get_entity(tilemap_guid)
                .unwrap(),
        )
        .get::<SceneTilemap>()
        .unwrap();
    assert_eq!(tilemap_desc.asset, Some(tilemap_uuid));
    assert!((tilemap_desc.tile_scale - 1.5).abs() < 1e-5);

    // Move the prefab file in AssetRegistry and confirm flush_asset_rehydration_signals updates PrefabInstance::prefab_path.
    let moved_rec = reloaded_app
        .world_mut()
        .resource_mut::<AssetRegistry>()
        .move_or_rename(prefab_uuid, "moved_prefabs/renamed_crate.prefab.ron")
        .unwrap();
    assert_eq!(moved_rec.uuid, prefab_uuid);
    reloaded_app.update();

    let mut inst_query = reloaded_app.world_mut().query::<&PrefabInstance>();
    let instances: Vec<&PrefabInstance> = inst_query.iter(reloaded_app.world()).collect();
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].prefab_uuid, Some(prefab_uuid));
    assert_eq!(
        instances[0].prefab_path,
        "assets/moved_prefabs/renamed_crate.prefab.ron"
    );

    let shared = reloaded_app
        .world()
        .resource::<AssetRegistry>()
        .shared();
    let resolved_abs = resolve_prefab_abs_path(instances[0], Some(&paths.root_dir), Some(&shared)).unwrap();
    assert_eq!(
        resolved_abs,
        paths.assets_dir.join("moved_prefabs/renamed_crate.prefab.ron")
    );
}

#[test]
fn tilemap_hydration_spawns_visible_non_empty_tile_sprites() {
    let temp = TempProjectDir::new("tilemap_hydration");
    let paths = temp.paths();
    let tilemap_path = paths.assets_dir.join("level.tilemap.ron");
    let tilemap = TilemapAsset {
        schema_version: CURRENT_TILEMAP_VERSION,
        tileset_texture: None,
        tile_size: [16, 16],
        grid_columns: 2,
        grid_rows: 2,
        layers: vec![
            TilemapLayer {
                name: "ground".to_owned(),
                z_offset: 0.0,
                visible: true,
                width: 2,
                height: 2,
                tiles: vec![Some(0), Some(1), None, Some(2)],
                metadata: vec![TileMetadata {
                    tile_index: 1,
                    solid: true,
                    tag: "wall".to_owned(),
                }],
            },
            TilemapLayer {
                name: "hidden".to_owned(),
                z_offset: 1.0,
                visible: false,
                width: 2,
                height: 1,
                tiles: vec![Some(0), Some(1)],
                metadata: Vec::new(),
            },
        ],
    };
    save_tilemap_to_file(&tilemap_path, &tilemap).unwrap();

    let mut app = setup_asset_scene_app(&paths, WorldMode::Mode2D);
    app.world_mut()
        .resource_mut::<AssetRegistry>()
        .scan_all()
        .unwrap();
    let tilemap_uuid = app
        .world()
        .resource::<AssetRegistry>()
        .shared()
        .get_by_path("level.tilemap.ron")
        .unwrap()
        .uuid;

    let host_entity = app
        .world_mut()
        .spawn((
            SceneEntity,
            EntityGuid::new(),
            Name::new("LevelTiles"),
            Transform::from_translation(Vec3::ZERO),
            Visibility::default(),
            SceneTilemap {
                asset: Some(tilemap_uuid),
                tile_scale: 2.0,
            },
        ))
        .id();

    app.update();

    let mut tile_query = app
        .world_mut()
        .query::<(&Sprite, &Transform, &ChildOf, &HydratedTilemapTile)>();
    let spawned_tiles: Vec<_> = tile_query.iter(app.world()).collect();
    assert_eq!(spawned_tiles.len(), 3);
    for (sprite, _tfm, child_of, _) in spawned_tiles {
        assert_eq!(child_of.parent(), host_entity);
        assert_eq!(sprite.custom_size, Some(bevy_math::Vec2::splat(32.0)));
    }
}
