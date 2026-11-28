//! Tests for `.cache/ddc` (`DdcConfig`), `AssetProcessor` log redirection, and async PNG `ThumbnailService`.

use std::{
    fs,
    io::Cursor,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use bevy_app::{App, TaskPoolPlugin};
use bevy_asset::{processor::AssetProcessor, AssetMode};
use engine_assets::{
    configure_asset_plugin_for_project, configure_asset_processor_ddc_log, load_tilemap_from_file,
    save_tilemap_to_file, AssetRegistry, DdcConfig, ThumbnailService, TileMetadata, TilemapAsset,
    TilemapLayer, CURRENT_TILEMAP_VERSION, THUMBNAIL_SIZE,
};
use engine_core::{AssetUuid, ProjectPaths};
use engine_protocol::AssetKind;
use image::{ImageFormat, Rgba, RgbaImage};

struct TempProject {
    root: PathBuf,
}

impl TempProject {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("bevy9_{name}_{}", AssetUuid::new()));
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

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn write_test_png(path: &std::path::Path, r: u8, g: u8, b: u8) {
    let mut img = RgbaImage::new(16, 16);
    for y in 0..16 {
        for x in 0..16 {
            let shade = if (x + y) % 2 == 0 { 1.0 } else { 0.65 };
            img.put_pixel(
                x,
                y,
                Rgba([
                    (f32::from(r) * shade) as u8,
                    (f32::from(g) * shade) as u8,
                    (f32::from(b) * shade) as u8,
                    255,
                ]),
            );
        }
    }
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .unwrap();
    fs::write(path, bytes).unwrap();
}

/// Builds a valid binary glTF 2.0 (`.glb`) containing an indexed double-pyramid octahedron mesh.
fn build_octahedron_glb() -> Vec<u8> {
    let positions: [[f32; 3]; 6] = [
        [0.0, 0.85, 0.0],
        [0.65, 0.0, 0.0],
        [0.0, 0.0, 0.65],
        [-0.65, 0.0, 0.0],
        [0.0, 0.0, -0.65],
        [0.0, -0.85, 0.0],
    ];
    let indices: [u16; 24] = [
        0, 1, 2, 0, 2, 3, 0, 3, 4, 0, 4, 1, 5, 2, 1, 5, 3, 2, 5, 4, 3, 5, 1, 4,
    ];

    let mut bin = Vec::new();
    for v in positions {
        for f in v {
            bin.extend_from_slice(&f.to_le_bytes());
        }
    }
    let pos_byte_len = bin.len();
    for idx in indices {
        bin.extend_from_slice(&idx.to_le_bytes());
    }
    let idx_byte_len = bin.len() - pos_byte_len;
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }

    let json = format!(
        r#"{{"asset":{{"version":"2.0","generator":"bevy9-test"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0,"name":"Crystal"}}],"meshes":[{{"name":"Octahedron","primitives":[{{"attributes":{{"POSITION":0}},"indices":1}}]}}],"buffers":[{{"byteLength":{}}}],"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{},"target":34962}},{{"buffer":0,"byteOffset":{},"byteLength":{},"target":34963}}],"accessors":[{{"bufferView":0,"componentType":5126,"count":6,"type":"VEC3","max":[0.65,0.85,0.65],"min":[-0.65,-0.85,-0.65]}},{{"bufferView":1,"componentType":5123,"count":24,"type":"SCALAR"}}]}}"#,
        bin.len(),
        pos_byte_len,
        pos_byte_len,
        idx_byte_len,
    );
    let mut json_bytes = json.into_bytes();
    while !json_bytes.len().is_multiple_of(4) {
        json_bytes.push(b' ');
    }

    let total_len = 12 + 8 + json_bytes.len() + 8 + bin.len();
    let mut glb = Vec::with_capacity(total_len);
    glb.extend_from_slice(&0x4654_6C67u32.to_le_bytes()); // "glTF"
    glb.extend_from_slice(&2u32.to_le_bytes()); // version 2
    glb.extend_from_slice(&(total_len as u32).to_le_bytes());

    glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes()); // "JSON"
    glb.extend_from_slice(&json_bytes);

    glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x004E_4942u32.to_le_bytes()); // "BIN\0"
    glb.extend_from_slice(&bin);
    glb
}

#[test]
fn ddc_config_and_asset_processor_use_project_cache_dir() {
    let temp = TempProject::new("ddc_config");
    let paths = temp.paths();
    let (plugin, ddc) = configure_asset_plugin_for_project(&paths, true);

    assert!(matches!(plugin.mode, AssetMode::Processed));
    assert!(plugin.processed_file_path.ends_with(".cache/ddc/processed"));
    assert!(ddc.processed_dir.exists());
    assert!(ddc.thumbnails_dir.exists());
    assert_eq!(
        ddc.processor_log_file,
        ddc.root_dir.join("processor.log")
    );

    let mut app = App::new();
    app.add_plugins((TaskPoolPlugin::default(), plugin));
    assert!(app.world().contains_resource::<AssetProcessor>());
    configure_asset_processor_ddc_log(&mut app, &ddc);
    // Ensure imported_assets was never created inside temp project root or CWD.
    assert!(!temp.root.join("imported_assets").exists());

    let (headless_plugin, _) = configure_asset_plugin_for_project(&paths, false);
    assert!(matches!(headless_plugin.mode, AssetMode::Unprocessed));
    assert_eq!(headless_plugin.use_asset_processor_override, Some(false));
}

#[test]
fn thumbnail_service_generates_pngs_and_invalidates_on_hash_change() {
    let temp = TempProject::new("thumbnails");
    let paths = temp.paths();
    let ddc = DdcConfig::for_project_root(&paths.root_dir);
    ddc.ensure_dirs().unwrap();

    let png_path = paths.assets_dir.join("crate.png");
    write_test_png(&png_path, 220, 120, 60);

    let glb_path = paths.assets_dir.join("crystal.glb");
    fs::write(&glb_path, build_octahedron_glb()).unwrap();

    let tilemap_path = paths.assets_dir.join("stage.tilemap.ron");
    save_tilemap_to_file(
        &tilemap_path,
        &TilemapAsset {
            schema_version: CURRENT_TILEMAP_VERSION,
            tileset_texture: None,
            tile_size: [16, 16],
            grid_columns: 2,
            grid_rows: 2,
            layers: vec![TilemapLayer {
                name: "ground".to_owned(),
                z_offset: 0.0,
                visible: true,
                width: 3,
                height: 2,
                tiles: vec![Some(0), Some(1), Some(2), Some(3), None, Some(0)],
                metadata: vec![TileMetadata {
                    tile_index: 1,
                    solid: true,
                    tag: "stone".to_owned(),
                }],
            }],
        },
    )
    .unwrap();

    let mut registry = AssetRegistry::new(paths.assets_dir.clone(), Some(paths.scenes_dir.clone()));
    registry.scan_all().unwrap();
    let shared = registry.shared();

    let png_rec = shared.get_by_path("crate.png").unwrap();
    let glb_rec = shared.get_by_path("crystal.glb").unwrap();
    let tm_rec = shared.get_by_path("stage.tilemap.ron").unwrap();

    let mut service = ThumbnailService::new(ddc.thumbnails_dir.clone(), Some(shared.clone()));
    service.request_all_missing(&registry);

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        service.poll_completed(&mut registry);
        let ready_all = [png_rec.uuid, glb_rec.uuid, tm_rec.uuid]
            .into_iter()
            .all(|uuid| shared.get(uuid).is_some_and(|r| r.has_thumbnail));
        if ready_all {
            break;
        }
        thread::sleep(Duration::from_millis(15));
    }

    for rec in [&png_rec, &glb_rec, &tm_rec] {
        let thumb_file = service.thumbnail_file_path(rec.uuid, &rec.source_hash);
        assert!(
            thumb_file.exists(),
            "missing thumbnail file '{}'",
            thumb_file.display()
        );
        let img = image::open(&thumb_file).unwrap();
        assert_eq!(img.width(), THUMBNAIL_SIZE);
        assert_eq!(img.height(), THUMBNAIL_SIZE);

        let url = service.get_data_url(rec.uuid, &rec.source_hash).unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
    }

    // Mutate crate.png and rescan -> old hash misses, new hash generates a distinct cached PNG.
    write_test_png(&png_path, 40, 180, 240);
    registry.scan_all().unwrap();
    let updated_rec = shared.get(png_rec.uuid).unwrap();
    assert_ne!(updated_rec.source_hash, png_rec.source_hash);
    assert!(service
        .get_data_url(updated_rec.uuid, &updated_rec.source_hash)
        .is_none());

    service.request_all_missing(&registry);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        service.poll_completed(&mut registry);
        if shared
            .get(updated_rec.uuid)
            .is_some_and(|r| r.has_thumbnail)
        {
            break;
        }
        thread::sleep(Duration::from_millis(15));
    }
    let updated_file = service.thumbnail_file_path(updated_rec.uuid, &updated_rec.source_hash);
    assert!(updated_file.exists());
    assert!(service
        .get_data_url(updated_rec.uuid, &updated_rec.source_hash)
        .is_some());
}

#[test]
fn sample_projects_assets_and_sidecars_load_cleanly() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let samples_dir = manifest_dir.parent().unwrap().parent().unwrap().join("samples");

    let sandbox3d_assets = samples_dir.join("sandbox_3d/assets");
    let sandbox2d_assets = samples_dir.join("sandbox_2d/assets");

    let checker_uuid = AssetUuid::parse_str("11111111-2222-4333-8444-555555555301").unwrap();
    let crystal_uuid = AssetUuid::parse_str("11111111-2222-4333-8444-555555555302").unwrap();
    let player3d_script_uuid =
        AssetUuid::parse_str("11111111-2222-4333-8444-555555555311").unwrap();
    let trigger3d_script_uuid =
        AssetUuid::parse_str("11111111-2222-4333-8444-555555555312").unwrap();
    let camera3d_script_uuid =
        AssetUuid::parse_str("11111111-2222-4333-8444-555555555313").unwrap();
    let tiles_uuid = AssetUuid::parse_str("11111111-2222-4333-8444-555555555201").unwrap();
    let tilemap_uuid = AssetUuid::parse_str("11111111-2222-4333-8444-555555555202").unwrap();
    let player2d_script_uuid =
        AssetUuid::parse_str("11111111-2222-4333-8444-555555555211").unwrap();
    let trigger2d_script_uuid =
        AssetUuid::parse_str("11111111-2222-4333-8444-555555555212").unwrap();

    // Read-only verification on loaded sample registries:
    let mut reg3d = AssetRegistry::new(sandbox3d_assets, Some(samples_dir.join("sandbox_3d/scenes")));
    let rep3d_first = reg3d.scan_all().unwrap();
    assert!(rep3d_first.modified_paths.is_empty());
    let rep3d_second = reg3d.scan_all().unwrap();
    assert!(!rep3d_second.changed);
    assert_eq!(reg3d.shared().get_by_path("checker_crate.png").unwrap().uuid, checker_uuid);
    assert_eq!(reg3d.shared().get_by_path("crystal.glb").unwrap().uuid, crystal_uuid);
    for (rel, expected_uuid) in [
        ("scripts/player_3d.rhai", player3d_script_uuid),
        ("scripts/collectible_trigger_3d.rhai", trigger3d_script_uuid),
        ("scripts/camera_follow_3d.rhai", camera3d_script_uuid),
    ] {
        let rec = reg3d
            .shared()
            .get_by_path(rel)
            .unwrap_or_else(|| panic!("sandbox_3d script '{rel}' must be indexed"));
        assert_eq!(rec.uuid, expected_uuid);
        assert_eq!(rec.kind, AssetKind::Script);
    }
    assert_eq!(
        reg3d.shared().get_by_path("audio/jump.wav").unwrap().kind,
        AssetKind::Audio
    );
    assert_eq!(
        reg3d.shared().get_by_path("audio/pickup.wav").unwrap().kind,
        AssetKind::Audio
    );

    let tilemap_path = sandbox2d_assets.join("demo.tilemap.ron");
    let mut reg2d = AssetRegistry::new(sandbox2d_assets, Some(samples_dir.join("sandbox_2d/scenes")));
    let rep2d_first = reg2d.scan_all().unwrap();
    assert!(rep2d_first.modified_paths.is_empty());
    let rep2d_second = reg2d.scan_all().unwrap();
    assert!(!rep2d_second.changed);

    let loaded_tm = load_tilemap_from_file(&tilemap_path).unwrap();
    assert_eq!(loaded_tm.tileset_texture, Some(tiles_uuid));
    assert_eq!(reg2d.shared().get_by_path("tiles_2x2.png").unwrap().uuid, tiles_uuid);
    let tilemap_rec = reg2d.shared().get_by_path("demo.tilemap.ron").unwrap();
    assert_eq!(tilemap_rec.uuid, tilemap_uuid);
    assert!(tilemap_rec.dependencies.contains(&tiles_uuid));
    for (rel, expected_uuid) in [
        ("scripts/player_2d.rhai", player2d_script_uuid),
        ("scripts/collectible_trigger_2d.rhai", trigger2d_script_uuid),
    ] {
        let rec = reg2d
            .shared()
            .get_by_path(rel)
            .unwrap_or_else(|| panic!("sandbox_2d script '{rel}' must be indexed"));
        assert_eq!(rec.uuid, expected_uuid);
        assert_eq!(rec.kind, AssetKind::Script);
    }
    assert_eq!(
        reg2d.shared().get_by_path("audio/jump.wav").unwrap().kind,
        AssetKind::Audio
    );
    assert_eq!(
        reg2d.shared().get_by_path("audio/pickup.wav").unwrap().kind,
        AssetKind::Audio
    );
}
