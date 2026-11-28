//! Integration tests for `.b9meta` sidecars, `AssetRegistry` dependencies, safe moves/deletes, `uuid://` source, and `notify` hot-reload.

use core::time::Duration;
use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use bevy_app::App;
use engine_assets::{AssetError, AssetRegistry, AssetWatcherResource, EngineAssetsPlugin};
use engine_core::{EngineCorePlugin, ProjectManifest, ProjectPaths, WorldMode};
use engine_protocol::{AssetImportSettings, AssetKind, AssetListParams, TextureFilterMode};

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

fn minimal_1x1_png_bytes() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 128, 255]));
    let mut bytes = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut bytes);
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .expect("PNG encode should succeed");
    bytes
}

#[test]
fn registry_sidecars_dependencies_safe_move_and_safe_delete() {
    let tmp = TempDirGuard::new("bevy9_assets_reg");
    let assets_dir = tmp.path.join("assets");
    let scenes_dir = tmp.path.join("scenes");
    fs::create_dir_all(&assets_dir).unwrap();
    fs::create_dir_all(&scenes_dir).unwrap();

    // Create a PNG in `assets/` and index it.
    let external_src = tmp.path.join("hero_tile.png");
    fs::write(&external_src, minimal_1x1_png_bytes()).unwrap();

    let mut reg = AssetRegistry::new(assets_dir.clone(), Some(scenes_dir.clone()));
    reg.create_folder("sprites/characters").unwrap();

    let imported = reg
        .import_external_files(&[external_src], "sprites/characters")
        .unwrap();
    assert_eq!(imported.len(), 1);
    let hero = &imported[0];
    assert_eq!(hero.kind, AssetKind::Image);
    assert_eq!(hero.relative_path, "sprites/characters/hero_tile.png");
    assert!(assets_dir.join("sprites/characters/hero_tile.png.b9meta").exists());

    // Create a scene in `scenes/` referencing the image UUID, plus a prefab inside `assets/` referencing it.
    let level_file = scenes_dir.join("main.level.ron");
    fs::write(&level_file, format!("(texture: Some(\"{}\"))", hero.uuid)).unwrap();
    let prefab_file = assets_dir.join("hero.prefab.ron");
    fs::write(&prefab_file, format!("(sprite_tex: \"{}\")", hero.uuid)).unwrap();

    reg.scan_all().unwrap();
    let prefab_rec = reg.get_by_path("hero.prefab.ron").unwrap();
    assert_eq!(reg.dependencies_of(prefab_rec.uuid), vec![hero.uuid]);
    assert_eq!(reg.dependents_of(hero.uuid), vec![prefab_rec.uuid]);

    // Safe delete refuses while prefab and scene still depend on `hero.uuid`.
    let err = reg.delete_asset(hero.uuid, false).unwrap_err();
    assert!(matches!(err, AssetError::HasDependents { .. }));

    // Safe move/rename preserves the exact same `AssetUuid` and updates `.b9meta` path.
    let moved = reg
        .move_or_rename(hero.uuid, "textures/renamed_hero.png")
        .unwrap();
    assert_eq!(moved.uuid, hero.uuid);
    assert_eq!(moved.relative_path, "textures/renamed_hero.png");
    assert!(!assets_dir.join("sprites/characters/hero_tile.png").exists());
    assert!(!assets_dir.join("sprites/characters/hero_tile.png.b9meta").exists());
    assert!(assets_dir.join("textures/renamed_hero.png.b9meta").exists());
    assert_eq!(
        reg.resolve_path(hero.uuid).as_deref(),
        Some("textures/renamed_hero.png")
    );

    // Updating import settings persists in `.b9meta` and survives full re-scan.
    if let AssetImportSettings::Image(mut img_settings) = moved.import_settings.clone() {
        img_settings.filter_mode = TextureFilterMode::Nearest;
        reg.update_import_settings(
            hero.uuid,
            AssetImportSettings::Image(img_settings),
            Some(vec!["pixel_art".to_owned()]),
        )
        .unwrap();
    } else {
        panic!("expected Image import settings");
    }

    let mut fresh_reg = AssetRegistry::new(assets_dir.clone(), Some(scenes_dir));
    fresh_reg.scan_all().unwrap();
    let reloaded = fresh_reg.get(hero.uuid).unwrap();
    assert_eq!(reloaded.tags, vec!["pixel_art"]);
    assert!(matches!(
        reloaded.import_settings,
        AssetImportSettings::Image(s) if s.filter_mode == TextureFilterMode::Nearest
    ));

    let filtered = fresh_reg.snapshot(&AssetListParams {
        kind: Some(AssetKind::Image),
        search: Some("renamed".to_owned()),
        tag: Some("pixel_art".to_owned()),
        ..Default::default()
    });
    assert_eq!(filtered.assets.len(), 1);

    // Forced delete removes asset & `.b9meta`.
    fresh_reg.delete_asset(hero.uuid, true).unwrap();
    assert!(fresh_reg.get(hero.uuid).is_none());
    assert!(!assets_dir.join("textures/renamed_hero.png").exists());
    assert!(!assets_dir.join("textures/renamed_hero.png.b9meta").exists());
}

#[test]
fn file_watcher_detects_modified_asset_and_updates_hash() {
    let tmp = TempDirGuard::new("bevy9_assets_watch");
    let assets_dir = tmp.path.join("assets");
    let scenes_dir = tmp.path.join("scenes");
    fs::create_dir_all(&assets_dir).unwrap();
    fs::create_dir_all(&scenes_dir).unwrap();

    let png_path = assets_dir.join("swatch.png");
    fs::write(&png_path, minimal_1x1_png_bytes()).unwrap();

    let mut app = App::new();
    app.insert_resource(ProjectManifest::new("WatchTest", WorldMode::Mode2D))
        .insert_resource(ProjectPaths {
            root_dir: tmp.path.clone(),
            manifest_path: tmp.path.join("project.toml"),
            assets_dir: assets_dir.clone(),
            scenes_dir,
            scripts_dir: tmp.path.join("scripts"),
        })
        .add_plugins((EngineCorePlugin, EngineAssetsPlugin));

    assert!(app.world().contains_resource::<AssetWatcherResource>());
    let before = app
        .world()
        .resource::<AssetRegistry>()
        .get_by_path("swatch.png")
        .expect("swatch.png should be indexed");

    // Overwrite file with different pixel color to trigger notify event & BLAKE3 hash update.
    let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 255, 0, 255]));
    let mut new_bytes = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut new_bytes);
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .unwrap();
    fs::write(&png_path, new_bytes).unwrap();

    let deadline = Instant::now() + Duration::from_secs(4);
    let mut updated_hash = before.source_hash.clone();
    while Instant::now() < deadline {
        app.update();
        if let Some(now) = app
            .world()
            .resource::<AssetRegistry>()
            .get(before.uuid)
        {
            updated_hash = now.source_hash;
            if updated_hash != before.source_hash {
                break;
            }
        }
        thread::sleep(Duration::from_millis(25));
    }

    assert_ne!(
        updated_hash, before.source_hash,
        "watcher must re-scan modified file and update source_hash"
    );
}

#[test]
fn rhai_scripts_index_as_script_assets_with_sidecars_and_dependencies() {
    let tmp = TempDirGuard::new("bevy9_assets_rhai");
    let assets_dir = tmp.path.join("assets");
    let scenes_dir = tmp.path.join("scenes");
    fs::create_dir_all(assets_dir.join("scripts")).unwrap();
    fs::create_dir_all(assets_dir.join("audio")).unwrap();
    fs::create_dir_all(&scenes_dir).unwrap();

    // Seed an audio clip first so we can verify UUID dependency scanning from `.rhai`.
    let clip_path = assets_dir.join("audio/jump.wav");
    fs::write(&clip_path, b"RIFF0000WAVEfmt ").unwrap();

    let mut reg = AssetRegistry::new(assets_dir.clone(), Some(scenes_dir.clone()));
    reg.scan_all().unwrap();
    let clip_rec = reg
        .get_by_path("audio/jump.wav")
        .expect("jump.wav should be indexed");
    assert_eq!(clip_rec.kind, AssetKind::Audio);

    // Author a `.rhai` script referencing the audio clip UUID.
    let script_path = assets_dir.join("scripts/player.rhai");
    fs::write(
        &script_path,
        format!(
            "fn on_start() {{\n    audio.play(\"{}\");\n}}\n",
            clip_rec.uuid
        ),
    )
    .unwrap();

    let report = reg.scan_all().unwrap();
    assert!(report.changed);
    assert!(
        assets_dir.join("scripts/player.rhai.b9meta").exists(),
        ".b9meta sidecar must be created for .rhai scripts"
    );

    let script_rec = reg
        .get_by_path("scripts/player.rhai")
        .expect("player.rhai must be indexed");
    assert_eq!(script_rec.kind, AssetKind::Script);
    assert_eq!(script_rec.import_settings, AssetImportSettings::Generic);
    assert_eq!(reg.dependencies_of(script_rec.uuid), vec![clip_rec.uuid]);
    assert_eq!(reg.dependents_of(clip_rec.uuid), vec![script_rec.uuid]);

    // Filter snapshot specifically for `AssetKind::Script`.
    let filtered = reg.snapshot(&AssetListParams {
        kind: Some(AssetKind::Script),
        ..Default::default()
    });
    assert_eq!(filtered.assets.len(), 1);
    assert_eq!(filtered.assets[0].uuid, script_rec.uuid);

    // Renaming a `.rhai` script preserves its persistent `AssetUuid`.
    let moved = reg
        .move_or_rename(script_rec.uuid, "scripts/hero_controller.rhai")
        .unwrap();
    assert_eq!(moved.uuid, script_rec.uuid);
    assert_eq!(moved.kind, AssetKind::Script);
    assert!(assets_dir
        .join("scripts/hero_controller.rhai.b9meta")
        .exists());
    assert!(!assets_dir.join("scripts/player.rhai.b9meta").exists());

    // Subsequent scan with no disk changes reports unchanged.
    let rescan = reg.scan_all().unwrap();
    assert!(!rescan.changed);
    assert!(rescan.modified_paths.is_empty());
}
