//! Integration tests for transactional Undo/Redo history, coalescing, gizmo commits, and BRP mutations.

use core::time::Duration;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::Instant,
};

use bevy_app::{App, TaskPoolPlugin};
use bevy_asset::{AssetApp, AssetPlugin};
use bevy_ecs::name::Name;
use bevy_image::Image;
use bevy_input::mouse::{MouseMotion, MouseWheel};
use bevy_math::Vec3;
use bevy_time::TimePlugin;
use bevy_transform::components::Transform;
use bevy_window::WindowCloseRequested;
use engine_core::{EngineCorePlugin, EntityGuidMap, WorldMode};
use engine_editor_bridge::{EditorBridgeConfig, EditorBridgePlugin, EditorSelection};
use engine_protocol::{
    AddComponentParams, ComponentMutationResult, DeleteEntityParams, DeleteEntityResult,
    EntityPresetKind, HistoryStatePayload, HistoryStepResult, RemoveComponentParams,
    ReparentEntityParams, ReparentEntityResult, SceneNewParams, SetComponentParams,
    SpawnPresetParams, SpawnPresetResult, WorldTreeResult, METHOD_COMPONENT_ADD,
    METHOD_COMPONENT_REMOVE, METHOD_COMPONENT_SET, METHOD_ENTITY_DELETE, METHOD_ENTITY_REPARENT,
    METHOD_ENTITY_SPAWN_PRESET, METHOD_HISTORY_REDO, METHOD_HISTORY_STATE, METHOD_HISTORY_UNDO,
    METHOD_SCENE_NEW, METHOD_WORLD_TREE,
};
use engine_render::{
    gizmo::{GizmoTransformCommit, TransformGizmoState},
    EngineRenderPlugin,
};
use engine_scene::{reset_to_new_scene, EngineScenePlugin, MeshPrimitive3d};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

fn allocate_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port should bind");
    listener
        .local_addr()
        .expect("listener local_addr should exist")
        .port()
}

fn setup_app(brp_port: u16) -> App {
    let mut app = App::new();
    app.insert_resource(WorldMode::Mode3D)
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

    reset_to_new_scene(app.world_mut(), "HistoryTest", WorldMode::Mode3D);
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

    serde_json::from_str(&json_body).expect("BRP response must be valid JSON")
}

fn call_brp_raw(app: &mut App, port: u16, method: &str, params: Option<Value>) -> Value {
    let method_owned = method.to_owned();
    let handle = thread::spawn(move || send_brp_raw(port, &method_owned, params));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "timeout calling BRP '{method}'");
        app.update();
        thread::sleep(Duration::from_millis(5));
    }
    handle.join().unwrap()
}

fn call_brp<T: DeserializeOwned>(
    app: &mut App,
    port: u16,
    method: &str,
    params: Option<Value>,
) -> T {
    let resp = call_brp_raw(app, port, method, params);
    assert!(
        resp.get("error").is_none(),
        "unexpected BRP error calling '{method}': {resp}"
    );
    serde_json::from_value(resp.get("result").cloned().expect("result field present"))
        .expect("result should deserialize")
}

#[test]
fn transactional_undo_redo_lifecycle_coalescing_gizmo_and_subtree_restore() {
    let brp_port = allocate_free_port();
    let mut app = setup_app(brp_port);

    // 1. Create parent A and child B.
    let parent_spawn: SpawnPresetResult = call_brp(
        &mut app,
        brp_port,
        METHOD_ENTITY_SPAWN_PRESET,
        Some(
            serde_json::to_value(SpawnPresetParams {
                preset: EntityPresetKind::Empty,
                name: Some("Alpha".to_owned()),
                parent_guid: None,
                position: Some([1.0, 2.0, 3.0]),
            })
            .unwrap(),
        ),
    );
    let child_spawn: SpawnPresetResult = call_brp(
        &mut app,
        brp_port,
        METHOD_ENTITY_SPAWN_PRESET,
        Some(
            serde_json::to_value(SpawnPresetParams {
                preset: EntityPresetKind::Empty,
                name: Some("Beta".to_owned()),
                parent_guid: Some(parent_spawn.guid),
                position: None,
            })
            .unwrap(),
        ),
    );
    assert_eq!(
        call_brp::<HistoryStatePayload>(&mut app, brp_port, METHOD_HISTORY_STATE, None).undo_len,
        2
    );

    // 2. Two rapid continuous edits with identical merge_key must coalesce into 1 entry.
    let merge_key = Some(format!("{}:bevy_ecs::name::Name", parent_spawn.guid));
    let _: ComponentMutationResult = call_brp(
        &mut app,
        brp_port,
        METHOD_COMPONENT_SET,
        Some(
            serde_json::to_value(SetComponentParams {
                guid: parent_spawn.guid,
                type_path: "bevy_ecs::name::Name".to_owned(),
                value: json!("AlphaMid"),
                merge_key: merge_key.clone(),
                label: Some("Edit Name".to_owned()),
            })
            .unwrap(),
        ),
    );
    let _: ComponentMutationResult = call_brp(
        &mut app,
        brp_port,
        METHOD_COMPONENT_SET,
        Some(
            serde_json::to_value(SetComponentParams {
                guid: parent_spawn.guid,
                type_path: "bevy_ecs::name::Name".to_owned(),
                value: json!("AlphaFinal"),
                merge_key,
                label: Some("Edit Name".to_owned()),
            })
            .unwrap(),
        ),
    );
    assert_eq!(
        call_brp::<HistoryStatePayload>(&mut app, brp_port, METHOD_HISTORY_STATE, None).undo_len,
        3
    );

    // 3. Gizmo drag commit should drain cleanly into a single undo entry.
    {
        let e = app
            .world()
            .resource::<EntityGuidMap>()
            .get_entity(parent_spawn.guid)
            .unwrap();
        let before_tfm = *app.world().get::<Transform>(e).unwrap();
        let mut after_tfm = before_tfm;
        after_tfm.translation = Vec3::new(10.0, 2.0, -4.0);
        *app.world_mut().get_mut::<Transform>(e).unwrap() = after_tfm;
        app.world_mut()
            .resource_mut::<TransformGizmoState>()
            .completed_commits
            .push(GizmoTransformCommit {
                label: "Translate".to_owned(),
                entries: vec![(parent_spawn.guid, before_tfm, after_tfm)],
            });
    }
    app.update();
    assert_eq!(
        call_brp::<HistoryStatePayload>(&mut app, brp_port, METHOD_HISTORY_STATE, None).undo_len,
        4
    );

    // 4. Reject invalid cycle reparenting (parent beneath own child), then add & remove component, reparent, and delete subtree.
    let cycle_err = call_brp_raw(
        &mut app,
        brp_port,
        METHOD_ENTITY_REPARENT,
        Some(
            serde_json::to_value(ReparentEntityParams {
                guid: parent_spawn.guid,
                parent_guid: Some(child_spawn.guid),
            })
            .unwrap(),
        ),
    );
    assert!(cycle_err.get("error").is_some());

    let _: ComponentMutationResult = call_brp(
        &mut app,
        brp_port,
        METHOD_COMPONENT_ADD,
        Some(
            serde_json::to_value(AddComponentParams {
                guid: child_spawn.guid,
                type_path: "engine_scene::components::MeshPrimitive3d".to_owned(),
                value: json!({ "Cube": { "size": [1.0, 1.0, 1.0] } }),
            })
            .unwrap(),
        ),
    );
    let _: ComponentMutationResult = call_brp(
        &mut app,
        brp_port,
        METHOD_COMPONENT_REMOVE,
        Some(
            serde_json::to_value(RemoveComponentParams {
                guid: child_spawn.guid,
                type_path: "engine_scene::components::MeshPrimitive3d".to_owned(),
            })
            .unwrap(),
        ),
    );
    let _: ReparentEntityResult = call_brp(
        &mut app,
        brp_port,
        METHOD_ENTITY_REPARENT,
        Some(
            serde_json::to_value(ReparentEntityParams {
                guid: child_spawn.guid,
                parent_guid: None,
            })
            .unwrap(),
        ),
    );
    // Reparent back under Alpha before deleting Alpha subtree so both get captured & restored!
    let _: ReparentEntityResult = call_brp(
        &mut app,
        brp_port,
        METHOD_ENTITY_REPARENT,
        Some(
            serde_json::to_value(ReparentEntityParams {
                guid: child_spawn.guid,
                parent_guid: Some(parent_spawn.guid),
            })
            .unwrap(),
        ),
    );

    app.world_mut()
        .resource_mut::<EditorSelection>()
        .set(vec![parent_spawn.guid]);

    let del_res: DeleteEntityResult = call_brp(
        &mut app,
        brp_port,
        METHOD_ENTITY_DELETE,
        Some(
            serde_json::to_value(DeleteEntityParams {
                guid: parent_spawn.guid,
            })
            .unwrap(),
        ),
    );
    assert_eq!(del_res.deleted_guids.len(), 2);
    assert!(app.world().resource::<EditorSelection>().guids().is_empty());

    let state_full: HistoryStatePayload = call_brp(&mut app, brp_port, METHOD_HISTORY_STATE, None);
    assert_eq!(state_full.undo_len, 9);

    // 5. Undo Delete -> subtree Alpha + Beta + selection restored!
    let step: HistoryStepResult = call_brp(&mut app, brp_port, METHOD_HISTORY_UNDO, None);
    assert!(step.applied);
    assert_eq!(
        app.world().resource::<EditorSelection>().guids(),
        &[parent_spawn.guid]
    );
    let tree: WorldTreeResult = call_brp(&mut app, brp_port, METHOD_WORLD_TREE, None);
    assert_eq!(tree.roots.len(), 1);
    assert_eq!(tree.roots[0].guid, parent_spawn.guid);
    assert_eq!(tree.roots[0].name, "AlphaFinal");
    assert_eq!(tree.roots[0].children.len(), 1);
    assert_eq!(tree.roots[0].children[0].guid, child_spawn.guid);

    // 6. Undo Reparent-back (now Beta is at root), Undo Reparent-to-root (back under Alpha).
    assert!(call_brp::<HistoryStepResult>(&mut app, brp_port, METHOD_HISTORY_UNDO, None).applied);
    assert_eq!(
        call_brp::<WorldTreeResult>(&mut app, brp_port, METHOD_WORLD_TREE, None)
            .roots
            .len(),
        2
    );
    assert!(call_brp::<HistoryStepResult>(&mut app, brp_port, METHOD_HISTORY_UNDO, None).applied);
    assert_eq!(
        call_brp::<WorldTreeResult>(&mut app, brp_port, METHOD_WORLD_TREE, None)
            .roots
            .len(),
        1
    );

    // 7. Undo RemoveComponent -> MeshPrimitive3d is back on Beta! Undo AddComponent -> gone again!
    assert!(call_brp::<HistoryStepResult>(&mut app, brp_port, METHOD_HISTORY_UNDO, None).applied);
    let beta_ent = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(child_spawn.guid)
        .unwrap();
    assert!(app.world().get::<MeshPrimitive3d>(beta_ent).is_some());

    assert!(call_brp::<HistoryStepResult>(&mut app, brp_port, METHOD_HISTORY_UNDO, None).applied);
    let beta_ent = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(child_spawn.guid)
        .unwrap();
    assert!(app.world().get::<MeshPrimitive3d>(beta_ent).is_none());

    // 8. Undo Gizmo Move -> translation returns to [1, 2, 3].
    let step_gizmo: HistoryStepResult = call_brp(&mut app, brp_port, METHOD_HISTORY_UNDO, None);
    assert_eq!(step_gizmo.label.as_deref(), Some("Move Entity"));
    let alpha_ent = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(parent_spawn.guid)
        .unwrap();
    assert_eq!(
        app.world().get::<Transform>(alpha_ent).unwrap().translation,
        Vec3::new(1.0, 2.0, 3.0)
    );

    // 9. Undo coalesced SetComponent -> single step returns directly to "Alpha".
    assert!(call_brp::<HistoryStepResult>(&mut app, brp_port, METHOD_HISTORY_UNDO, None).applied);
    let alpha_ent = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(parent_spawn.guid)
        .unwrap();
    assert_eq!(app.world().get::<Name>(alpha_ent).unwrap().as_str(), "Alpha");

    // 10. Undo two creates -> scene empty!
    assert!(call_brp::<HistoryStepResult>(&mut app, brp_port, METHOD_HISTORY_UNDO, None).applied);
    assert!(call_brp::<HistoryStepResult>(&mut app, brp_port, METHOD_HISTORY_UNDO, None).applied);
    assert_eq!(
        call_brp::<WorldTreeResult>(&mut app, brp_port, METHOD_WORLD_TREE, None)
            .roots
            .len(),
        0
    );
    let empty_state: HistoryStatePayload = call_brp(&mut app, brp_port, METHOD_HISTORY_STATE, None);
    assert!(!empty_state.can_undo);
    assert_eq!(empty_state.redo_len, 9);

    // 11. Redo all 9 steps -> scene ends back in deleted state (0 roots, can_undo == true).
    for _ in 0..9 {
        assert!(call_brp::<HistoryStepResult>(&mut app, brp_port, METHOD_HISTORY_REDO, None).applied);
    }
    assert_eq!(
        call_brp::<WorldTreeResult>(&mut app, brp_port, METHOD_WORLD_TREE, None)
            .roots
            .len(),
        0
    );

    // One undo after full redo brings AlphaFinal + Beta back intact.
    assert!(call_brp::<HistoryStepResult>(&mut app, brp_port, METHOD_HISTORY_UNDO, None).applied);
    let final_tree: WorldTreeResult = call_brp(&mut app, brp_port, METHOD_WORLD_TREE, None);
    assert_eq!(final_tree.roots.len(), 1);
    assert_eq!(final_tree.roots[0].name, "AlphaFinal");
    assert_eq!(final_tree.roots[0].children.len(), 1);
    assert_eq!(final_tree.roots[0].children[0].guid, child_spawn.guid);

    // 12. engine.scene.new clears history stacks.
    let _: Value = call_brp(
        &mut app,
        brp_port,
        METHOD_SCENE_NEW,
        Some(
            serde_json::to_value(SceneNewParams {
                name: "Clean".to_owned(),
                world_mode: WorldMode::Mode3D,
            })
            .unwrap(),
        ),
    );
    let cleared: HistoryStatePayload = call_brp(&mut app, brp_port, METHOD_HISTORY_STATE, None);
    assert!(!cleared.can_undo);
    assert!(!cleared.can_redo);
}
