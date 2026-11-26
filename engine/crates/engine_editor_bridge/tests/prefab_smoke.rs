//! Integration tests for `.prefab.ron` BRP operations (`save`, `list`, `spawn`, auto-overrides, `apply`, `revert`, Undo/Redo).

use core::time::Duration;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use bevy_app::{App, TaskPoolPlugin};
use bevy_asset::{AssetApp, AssetPlugin};
use bevy_image::Image;
use bevy_input::mouse::{MouseMotion, MouseWheel};
use bevy_math::Vec3;
use bevy_time::TimePlugin;
use bevy_transform::components::Transform;
use bevy_window::WindowCloseRequested;
use engine_core::{EngineCorePlugin, EntityGuidMap, ProjectPaths, WorldMode};
use engine_editor_bridge::{EditorBridgeConfig, EditorBridgePlugin};
use engine_protocol::{
    ComponentMutationResult, EntityPresetKind, HistoryStepResult, PrefabApplyResult,
    PrefabListResult, PrefabRevertParams, PrefabRevertResult, PrefabSaveParams, PrefabSaveResult,
    PrefabSpawnParams, PrefabSpawnResult, SetComponentParams, SpawnPresetParams, SpawnPresetResult,
    WorldTreeResult, METHOD_COMPONENT_SET, METHOD_ENTITY_SPAWN_PRESET, METHOD_HISTORY_UNDO,
    METHOD_PREFAB_APPLY, METHOD_PREFAB_LIST, METHOD_PREFAB_REVERT, METHOD_PREFAB_SAVE,
    METHOD_PREFAB_SPAWN, METHOD_WORLD_TREE,
};
use engine_render::EngineRenderPlugin;
use engine_scene::{
    prefab_field_override_key, reset_to_new_scene, EngineScenePlugin, PrefabInstance,
    StandardMaterialDesc,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

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

fn allocate_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port should bind");
    listener.local_addr().unwrap().port()
}

fn setup_app(brp_port: u16, root_dir: PathBuf) -> App {
    let mut app = App::new();
    app.insert_resource(WorldMode::Mode3D)
        .insert_resource(ProjectPaths {
            root_dir: root_dir.clone(),
            manifest_path: root_dir.join("project.toml"),
            assets_dir: root_dir.join("assets"),
            scenes_dir: root_dir.join("scenes"),
            scripts_dir: root_dir.join("scripts"),
        })
        .insert_resource(EditorBridgeConfig {
            brp_port,
            ipc_port: 0,
            enable_http_brp: true,
            enable_ws_ipc: false,
            allow_cors: false,
            ..Default::default()
        })
        .add_message::<MouseMotion>()
        .add_message::<MouseWheel>()
        .add_message::<WindowCloseRequested>()
        .add_plugins((
            TaskPoolPlugin::default(),
            TimePlugin,
            AssetPlugin {
                use_asset_processor_override: Some(false),
                ..Default::default()
            },
            EngineCorePlugin,
            EngineScenePlugin,
            EngineRenderPlugin,
            EditorBridgePlugin,
        ))
        .init_asset::<Image>();

    reset_to_new_scene(app.world_mut(), "PrefabBrpTest", WorldMode::Mode3D);
    app.update();
    app
}

fn send_brp_raw(port: u16, method: &str, params: Option<Value>) -> Value {
    let body = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    }))
    .unwrap();

    let mut stream = None;
    for _ in 0..80 {
        if let Ok(connected) = TcpStream::connect(("127.0.0.1", port)) {
            stream = Some(connected);
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let mut stream = stream.expect("BRP HTTP server must connect");
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
    let text = String::from_utf8(raw).unwrap();
    let (headers, raw_body) = text.split_once("\r\n\r\n").unwrap();
    let json_body = if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut rem = raw_body;
        let mut out = String::new();
        while let Some((sz_line, rest)) = rem.split_once("\r\n") {
            let sz = usize::from_str_radix(sz_line.split(';').next().unwrap_or("").trim(), 16)
                .unwrap();
            if sz == 0 {
                break;
            }
            out.push_str(&rest[..sz]);
            rem = rest[sz..].strip_prefix("\r\n").unwrap_or(&rest[sz..]);
        }
        out
    } else {
        raw_body.to_owned()
    };

    serde_json::from_str(&json_body).unwrap()
}

fn call_brp<T: DeserializeOwned>(
    app: &mut App,
    port: u16,
    method: &str,
    params: Option<Value>,
) -> T {
    let method_owned = method.to_owned();
    let handle = thread::spawn(move || send_brp_raw(port, &method_owned, params));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "timeout calling BRP '{method}'");
        app.update();
        thread::sleep(Duration::from_millis(5));
    }
    let resp = handle.join().unwrap();
    assert!(
        resp.get("error").is_none(),
        "unexpected BRP error calling '{method}': {resp}"
    );
    serde_json::from_value(resp.get("result").cloned().expect("result field")).unwrap()
}

#[test]
fn prefab_brp_save_spawn_auto_override_apply_revert_and_undo() {
    let tmp = TempDirGuard::new("bevy9_prefab_brp");
    let brp_port = allocate_free_port();
    let mut app = setup_app(brp_port, tmp.path.clone());

    // 1. Spawn cube A and save as prefab.
    let spawn_a: SpawnPresetResult = call_brp(
        &mut app,
        brp_port,
        METHOD_ENTITY_SPAWN_PRESET,
        Some(
            serde_json::to_value(SpawnPresetParams {
                preset: EntityPresetKind::Cube3d,
                name: Some("PropA".to_owned()),
                parent_guid: None,
                position: Some([0.0, 0.0, 0.0]),
            })
            .unwrap(),
        ),
    );

    let save_res: PrefabSaveResult = call_brp(
        &mut app,
        brp_port,
        METHOD_PREFAB_SAVE,
        Some(
            serde_json::to_value(PrefabSaveParams {
                root_guid: spawn_a.guid,
                path: "prefabs/prop.prefab.ron".to_owned(),
                name: Some("PropPrefab".to_owned()),
            })
            .unwrap(),
        ),
    );
    assert_eq!(save_res.prefab_path, "prefabs/prop.prefab.ron");
    assert_eq!(save_res.entity_count, 1);

    let list_res: PrefabListResult = call_brp(&mut app, brp_port, METHOD_PREFAB_LIST, None);
    assert_eq!(list_res.prefabs.len(), 1);
    assert_eq!(list_res.prefabs[0].relative_path, "prefabs/prop.prefab.ron");

    // 2. Spawn sibling B at [4, 0, 0] via engine.prefab.spawn.
    let spawn_b: PrefabSpawnResult = call_brp(
        &mut app,
        brp_port,
        METHOD_PREFAB_SPAWN,
        Some(
            serde_json::to_value(PrefabSpawnParams {
                path: "prefabs/prop.prefab.ron".to_owned(),
                parent_guid: None,
                position: Some([4.0, 0.0, 0.0]),
            })
            .unwrap(),
        ),
    );
    assert_eq!(spawn_b.prefab_path, "prefabs/prop.prefab.ron");

    let tree: WorldTreeResult = call_brp(&mut app, brp_port, METHOD_WORLD_TREE, None);
    assert_eq!(tree.roots.len(), 2);
    assert!(tree.roots.iter().all(|n| n.is_prefab_member));
    assert!(tree
        .roots
        .iter()
        .all(|n| n.prefab_path.as_deref() == Some("prefabs/prop.prefab.ron")));

    // 3. Edit B's StandardMaterialDesc.metallic via engine.component.set (auto-syncs PrefabInstance.overrides).
    let _: ComponentMutationResult = call_brp(
        &mut app,
        brp_port,
        METHOD_COMPONENT_SET,
        Some(
            serde_json::to_value(SetComponentParams {
                guid: spawn_b.guid,
                type_path: "engine_scene::components::StandardMaterialDesc".to_owned(),
                value: json!({
                    "base_color": { "Srgba": { "red": 0.8, "green": 0.82, "blue": 0.86, "alpha": 1.0 } },
                    "metallic": 0.85,
                    "perceptual_roughness": 0.5,
                    "emissive": { "red": 0.0, "green": 0.0, "blue": 0.0, "alpha": 1.0 }
                }),
                merge_key: None,
                label: Some("Edit Material".to_owned()),
            })
            .unwrap(),
        ),
    );

    let ent_b = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(spawn_b.guid)
        .unwrap();
    let metallic_key = prefab_field_override_key(
        save_res.root_source_guid,
        "engine_scene::components::StandardMaterialDesc",
        "metallic",
    );
    assert!(app
        .world()
        .get::<PrefabInstance>(ent_b)
        .unwrap()
        .overrides
        .contains_key(&metallic_key));

    // 4. Edit A's roughness -> Apply A -> B receives roughness=0.15 while keeping metallic=0.85 and translation=[4,0,0].
    let _: ComponentMutationResult = call_brp(
        &mut app,
        brp_port,
        METHOD_COMPONENT_SET,
        Some(
            serde_json::to_value(SetComponentParams {
                guid: spawn_a.guid,
                type_path: "engine_scene::components::StandardMaterialDesc".to_owned(),
                value: json!({
                    "base_color": { "Srgba": { "red": 0.8, "green": 0.82, "blue": 0.86, "alpha": 1.0 } },
                    "metallic": 0.0,
                    "perceptual_roughness": 0.15,
                    "emissive": { "red": 0.0, "green": 0.0, "blue": 0.0, "alpha": 1.0 }
                }),
                merge_key: None,
                label: Some("Edit Roughness".to_owned()),
            })
            .unwrap(),
        ),
    );
    let apply_res: PrefabApplyResult = call_brp(
        &mut app,
        brp_port,
        METHOD_PREFAB_APPLY,
        Some(json!({ "guid": spawn_a.guid })),
    );
    assert!(apply_res.updated_guids.contains(&spawn_b.guid));

    let ent_b = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(spawn_b.guid)
        .unwrap();
    let mat_b = app.world().get::<StandardMaterialDesc>(ent_b).unwrap();
    assert!((mat_b.perceptual_roughness - 0.15).abs() < 1e-5);
    assert!((mat_b.metallic - 0.85).abs() < 1e-5);
    assert_eq!(
        app.world().get::<Transform>(ent_b).unwrap().translation,
        Vec3::new(4.0, 0.0, 0.0)
    );

    // 5. Revert single component on B -> metallic back to 0.0; Undo -> metallic back to 0.85!
    let revert_single: PrefabRevertResult = call_brp(
        &mut app,
        brp_port,
        METHOD_PREFAB_REVERT,
        Some(
            serde_json::to_value(PrefabRevertParams {
                guid: spawn_b.guid,
                type_path: Some("engine_scene::components::StandardMaterialDesc".to_owned()),
            })
            .unwrap(),
        ),
    );
    assert_eq!(revert_single.reverted_guids, vec![spawn_b.guid]);
    let ent_b = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(spawn_b.guid)
        .unwrap();
    assert!(
        (app.world().get::<StandardMaterialDesc>(ent_b).unwrap().metallic - 0.0).abs() < 1e-5
    );
    assert_eq!(
        app.world().get::<Transform>(ent_b).unwrap().translation,
        Vec3::new(4.0, 0.0, 0.0)
    );

    let undo_step: HistoryStepResult = call_brp(&mut app, brp_port, METHOD_HISTORY_UNDO, None);
    assert!(undo_step.applied);
    let ent_b = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(spawn_b.guid)
        .unwrap();
    assert!(
        (app.world().get::<StandardMaterialDesc>(ent_b).unwrap().metallic - 0.85).abs() < 1e-5
    );

    // 6. Full revert on B -> translation returns to [0, 0, 0] and overrides empty.
    let _: PrefabRevertResult = call_brp(
        &mut app,
        brp_port,
        METHOD_PREFAB_REVERT,
        Some(
            serde_json::to_value(PrefabRevertParams {
                guid: spawn_b.guid,
                type_path: None,
            })
            .unwrap(),
        ),
    );
    let ent_b = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(spawn_b.guid)
        .unwrap();
    assert_eq!(
        app.world().get::<Transform>(ent_b).unwrap().translation,
        Vec3::ZERO
    );
    assert!(app
        .world()
        .get::<PrefabInstance>(ent_b)
        .unwrap()
        .overrides
        .is_empty());
}
