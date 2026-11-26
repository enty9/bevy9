//! Integration smoke test for `engine.asset.*` BRP handlers, `AssetsChanged` IPC, and Viewport spawn Undo/Redo.

use core::time::Duration;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    thread,
    time::Instant,
};

use bevy_app::{App, TaskPoolPlugin};
use bevy_asset::{AssetApp, AssetPlugin};
use bevy_image::{Image, TextureAtlasLayout};
use bevy_input::mouse::{MouseMotion, MouseWheel};
use bevy_mesh::Mesh;
use bevy_pbr::StandardMaterial;
use bevy_sprite_render::ColorMaterial;
use bevy_time::TimePlugin;
use bevy_window::WindowCloseRequested;
use bevy_world_serialization::WorldAsset;
use engine_assets::{
    save_tilemap_to_file, EngineAssetsPlugin, TilemapAsset, TilemapLayer, CURRENT_TILEMAP_VERSION,
};
use engine_core::{AssetUuid, EngineCorePlugin, ProjectPaths, WorldMode};
use engine_editor_bridge::{EditorBridgeConfig, EditorBridgePlugin, EditorIpcServer};
use engine_protocol::{
    AssetCreateFolderResult, AssetDeleteResult, AssetImportResult, AssetKind, AssetListResult,
    AssetRecord, AssetSpawnInViewportResult, AssetThumbnailResult, AtlasGridSettings,
    ClientIpcMessage, HistoryStepResult, ImageImportSettings, ServerIpcMessage,
    TextureFilterMode, WorldTreeResult, METHOD_ASSET_CREATE_FOLDER, METHOD_ASSET_DELETE,
    METHOD_ASSET_IMPORT, METHOD_ASSET_LIST, METHOD_ASSET_MOVE, METHOD_ASSET_RESCAN,
    METHOD_ASSET_SPAWN_IN_VIEWPORT, METHOD_ASSET_THUMBNAIL, METHOD_ASSET_UPDATE_SETTINGS,
    METHOD_HISTORY_REDO, METHOD_HISTORY_UNDO, METHOD_WORLD_TREE, PROTOCOL_VERSION,
};
use engine_render::EngineRenderPlugin;
use engine_scene::{EngineScenePlugin, SceneGltfModel, SceneTilemap};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};

struct TempProject {
    root: PathBuf,
    external_dir: PathBuf,
}

impl TempProject {
    fn new() -> Self {
        let id = AssetUuid::new();
        let root = std::env::temp_dir().join(format!("bevy9_assets_smoke_{id}"));
        let external_dir = std::env::temp_dir().join(format!("bevy9_assets_ext_{id}"));
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::create_dir_all(root.join("scenes")).unwrap();
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::create_dir_all(&external_dir).unwrap();
        Self { root, external_dir }
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
        let _ = fs::remove_dir_all(&self.external_dir);
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

fn minimal_glb_bytes() -> Vec<u8> {
    let bin: [u8; 40] = [0; 40];
    let json = br#"{"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"mesh":0}],"meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],"buffers":[{"byteLength":40}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}]}"#;
    let mut padded = json.to_vec();
    while !padded.len().is_multiple_of(4) {
        padded.push(b' ');
    }
    let total = 12 + 8 + padded.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&0x4654_6C67u32.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(padded.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
    out.extend_from_slice(&padded);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x004E_4942u32.to_le_bytes());
    out.extend_from_slice(&bin);
    out
}

fn allocate_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}

fn call_brp_raw(app: &mut App, port: u16, method: &str, params: Option<Value>) -> Value {
    let body = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    }))
    .unwrap();

    let (tx, rx) = crossbeam_channel::bounded(1);
    let handle = thread::spawn(move || {
        let mut stream = None;
        for _ in 0..80 {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(_) => thread::sleep(Duration::from_millis(10)),
            }
        }
        let mut stream = stream.expect("BRP connection");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let req = format!(
            "POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let _ = tx.send(raw);
    });

    let deadline = Instant::now() + Duration::from_secs(5);
    let raw = loop {
        app.update();
        if let Ok(buf) = rx.try_recv() {
            break buf;
        }
        assert!(Instant::now() < deadline, "BRP '{method}' timed out");
        thread::sleep(Duration::from_millis(5));
    };
    handle.join().unwrap();

    let text = String::from_utf8(raw).unwrap();
    let (_hdrs, raw_body) = text.split_once("\r\n\r\n").unwrap();
    let parsed: Value = serde_json::from_str(raw_body.trim()).unwrap_or_else(|_| {
        // Handle chunked body if hyper streamed it.
        let mut decoded = String::new();
        let mut rest = raw_body;
        while let Some((len_hex, after)) = rest.split_once("\r\n") {
            let Ok(sz) = usize::from_str_radix(len_hex.trim(), 16) else {
                break;
            };
            if sz == 0 {
                break;
            }
            decoded.push_str(&after[..sz]);
            rest = &after[sz + 2..];
        }
        serde_json::from_str(&decoded).unwrap()
    });
    parsed
}

fn call_brp_ok<T: DeserializeOwned>(
    app: &mut App,
    port: u16,
    method: &str,
    params: Option<Value>,
) -> T {
    let parsed = call_brp_raw(app, port, method, params);
    if let Some(err) = parsed.get("error") {
        panic!("BRP '{method}' returned unexpected error: {err}");
    }
    serde_json::from_value(parsed["result"].clone()).unwrap()
}

fn wait_for_assets_changed_ipc(
    app: &mut App,
    ws: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    min_revision: u64,
) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        app.update();
        match ws.read() {
            Ok(Message::Text(txt)) => {
                if let Ok(ServerIpcMessage::AssetsChanged { revision }) =
                    serde_json::from_str::<ServerIpcMessage>(&txt)
                    && revision >= min_revision
                {
                    return revision;
                }
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(err))
                if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) => panic!("IPC read error: {err}"),
        }
        thread::sleep(Duration::from_millis(8));
    }
    panic!("timed out waiting for AssetsChanged IPC revision >= {min_revision}");
}

#[test]
fn asset_brp_handlers_ipc_safe_delete_and_viewport_spawn_undo_work_end_to_end() {
    let temp = TempProject::new();
    let paths = temp.paths();
    fs::write(paths.assets_dir.join("seeded_crate.png"), minimal_png_bytes()).unwrap();
    fs::write(paths.assets_dir.join("crystal.glb"), minimal_glb_bytes()).unwrap();

    let ext_png = temp.external_dir.join("hero_sheet.png");
    fs::write(&ext_png, minimal_png_bytes()).unwrap();

    let brp_port = allocate_free_port();
    let mut app = App::new();
    app.insert_resource(WorldMode::Mode3D)
        .insert_resource(paths.clone())
        .insert_resource(EditorBridgeConfig {
            brp_port,
            ipc_port: 0,
            enable_http_brp: true,
            enable_ws_ipc: true,
            allow_cors: true,
            ..Default::default()
        })
        .add_message::<MouseMotion>()
        .add_message::<MouseWheel>()
        .add_message::<WindowCloseRequested>()
        .add_plugins((
            TaskPoolPlugin::default(),
            TimePlugin,
            AssetPlugin {
                file_path: paths.assets_dir.to_string_lossy().into_owned(),
                use_asset_processor_override: Some(false),
                ..Default::default()
            },
            EngineCorePlugin,
            EngineAssetsPlugin,
            EngineScenePlugin,
            EngineRenderPlugin,
            EditorBridgePlugin,
        ))
        .init_asset::<Image>()
        .init_asset::<TextureAtlasLayout>()
        .init_asset::<Mesh>()
        .init_asset::<StandardMaterial>()
        .init_asset::<ColorMaterial>()
        .init_asset::<WorldAsset>();

    app.update();
    app.update();

    let ipc_port = app.world().resource::<EditorIpcServer>().bound_addr().port();
    let (mut ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{ipc_port}")).unwrap();
    if let MaybeTlsStream::Plain(tcp) = ws.get_mut() {
        tcp.set_nonblocking(true).unwrap();
    }
    let hello = ClientIpcMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        client_name: "assets_smoke".to_owned(),
    };
    ws.send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .unwrap();
    wait_for_assets_changed_ipc(&mut app, &mut ws, 0);

    // 1. List initial assets.
    let initial: AssetListResult = call_brp_ok(&mut app, brp_port, METHOD_ASSET_LIST, None);
    assert_eq!(initial.assets.len(), 2);
    let crystal_rec = initial
        .assets
        .iter()
        .find(|a| a.relative_path == "crystal.glb")
        .unwrap()
        .clone();

    // 2. Create folder & import external file into it.
    let folder: AssetCreateFolderResult = call_brp_ok(
        &mut app,
        brp_port,
        METHOD_ASSET_CREATE_FOLDER,
        Some(json!({ "relative_path": "textures/characters" })),
    );
    assert_eq!(folder.relative_path, "textures/characters");

    let imported: AssetImportResult = call_brp_ok(
        &mut app,
        brp_port,
        METHOD_ASSET_IMPORT,
        Some(json!({
            "source_paths": [ext_png.display().to_string()],
            "target_folder": "textures/characters",
        })),
    );
    assert_eq!(imported.imported.len(), 1);
    let sheet_rec = imported.imported[0].clone();
    assert_eq!(sheet_rec.kind, AssetKind::Image);
    assert_eq!(sheet_rec.relative_path, "textures/characters/hero_sheet.png");
    assert!(Path::new(&paths.assets_dir)
        .join("textures/characters/hero_sheet.png.b9meta")
        .exists());

    // 3. Update import settings & rename/move while preserving AssetUuid.
    let updated: AssetRecord = call_brp_ok(
        &mut app,
        brp_port,
        METHOD_ASSET_UPDATE_SETTINGS,
        Some(json!({
            "uuid": sheet_rec.uuid,
            "import_settings": {
                "kind": "image",
                "filter_mode": "nearest",
                "srgb": false,
                "atlas_grid": AtlasGridSettings {
                    tile_size: [16, 16],
                    columns: 2,
                    rows: 2,
                    padding: [0, 0],
                    offset: [0, 0],
                },
            },
            "tags": ["atlas", "hero"],
        })),
    );
    assert_eq!(updated.tags, vec!["atlas", "hero"]);
    assert!(matches!(
        updated.import_settings,
        engine_protocol::AssetImportSettings::Image(ImageImportSettings {
            filter_mode: TextureFilterMode::Nearest,
            srgb: false,
            atlas_grid: Some(_),
        })
    ));

    let moved: AssetRecord = call_brp_ok(
        &mut app,
        brp_port,
        METHOD_ASSET_MOVE,
        Some(json!({
            "uuid": sheet_rec.uuid,
            "new_relative_path": "textures/hero_atlas.png",
        })),
    );
    assert_eq!(moved.uuid, sheet_rec.uuid);
    assert_eq!(moved.relative_path, "textures/hero_atlas.png");

    // 4. Create a tilemap referencing `sheet_rec.uuid` and rescan.
    let tm_path = paths.assets_dir.join("maps/arena.tilemap.ron");
    save_tilemap_to_file(
        &tm_path,
        &TilemapAsset {
            schema_version: CURRENT_TILEMAP_VERSION,
            tileset_texture: Some(sheet_rec.uuid),
            tile_size: [16, 16],
            grid_columns: 2,
            grid_rows: 2,
            layers: vec![TilemapLayer {
                name: "floor".to_owned(),
                z_offset: 0.0,
                visible: true,
                width: 2,
                height: 2,
                tiles: vec![Some(0), Some(1), Some(0), Some(1)],
                metadata: Vec::new(),
            }],
        },
    )
    .unwrap();
    let rescanned: AssetListResult = call_brp_ok(&mut app, brp_port, METHOD_ASSET_RESCAN, None);
    let tm_rec = rescanned
        .assets
        .iter()
        .find(|a| a.relative_path == "maps/arena.tilemap.ron")
        .unwrap()
        .clone();

    // 5. Safe delete blocks when dependent exists, then succeeds with `force: true`.
    let blocked = call_brp_raw(
        &mut app,
        brp_port,
        METHOD_ASSET_DELETE,
        Some(json!({ "uuid": sheet_rec.uuid, "force": false })),
    );
    let deps_arr = blocked["error"]["data"]["blocked_by_dependents"]
        .as_array()
        .expect("blocked_by_dependents array expected");
    assert!(!deps_arr.is_empty());

    let forced: AssetDeleteResult = call_brp_ok(
        &mut app,
        brp_port,
        METHOD_ASSET_DELETE,
        Some(json!({ "uuid": sheet_rec.uuid, "force": true })),
    );
    assert_eq!(forced.deleted_uuid, sheet_rec.uuid);

    // 6. Thumbnail generation finishes & returns valid PNG data URL.
    let thumb_deadline = Instant::now() + Duration::from_secs(5);
    let mut ready_thumb = None;
    while Instant::now() < thumb_deadline {
        app.update();
        let res: AssetThumbnailResult = call_brp_ok(
            &mut app,
            brp_port,
            METHOD_ASSET_THUMBNAIL,
            Some(json!({ "uuid": crystal_rec.uuid })),
        );
        if res.ready {
            ready_thumb = res.data_url;
            break;
        }
        thread::sleep(Duration::from_millis(15));
    }
    assert!(ready_thumb
        .expect("thumbnail should become ready")
        .starts_with("data:image/png;base64,"));

    // 7. Spawn glTF and Tilemap in viewport + verify Undo / Redo.
    let spawned_gltf: AssetSpawnInViewportResult = call_brp_ok(
        &mut app,
        brp_port,
        METHOD_ASSET_SPAWN_IN_VIEWPORT,
        Some(json!({ "uuid": crystal_rec.uuid, "x": 320.0, "y": 180.0 })),
    );
    let spawned_tm: AssetSpawnInViewportResult = call_brp_ok(
        &mut app,
        brp_port,
        METHOD_ASSET_SPAWN_IN_VIEWPORT,
        Some(json!({ "uuid": tm_rec.uuid, "x": 400.0, "y": 240.0 })),
    );
    app.update();

    {
        let mut gltf_q = app.world_mut().query::<&SceneGltfModel>();
        assert_eq!(gltf_q.iter(app.world()).count(), 1);
        let mut tm_q = app.world_mut().query::<&SceneTilemap>();
        assert_eq!(tm_q.iter(app.world()).count(), 1);
    }

    let undo_res: HistoryStepResult = call_brp_ok(&mut app, brp_port, METHOD_HISTORY_UNDO, None);
    assert!(undo_res.applied);
    let tree_after_undo: WorldTreeResult = call_brp_ok(&mut app, brp_port, METHOD_WORLD_TREE, None);
    assert!(!tree_after_undo
        .roots
        .iter()
        .any(|n| n.guid == spawned_tm.guid));
    assert!(tree_after_undo
        .roots
        .iter()
        .any(|n| n.guid == spawned_gltf.guid));

    let redo_res: HistoryStepResult = call_brp_ok(&mut app, brp_port, METHOD_HISTORY_REDO, None);
    assert!(redo_res.applied);
    let tree_after_redo: WorldTreeResult = call_brp_ok(&mut app, brp_port, METHOD_WORLD_TREE, None);
    assert!(tree_after_redo
        .roots
        .iter()
        .any(|n| n.guid == spawned_tm.guid));

    // Confirm AssetsChanged IPC messages flowed over the WS channel.
    wait_for_assets_changed_ipc(&mut app, &mut ws, 1);
}
