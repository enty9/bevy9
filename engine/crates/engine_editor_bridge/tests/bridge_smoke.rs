//! Integration smoke tests for `engine_editor_bridge` HTTP BRP and `WebSocket` IPC.

use core::time::Duration;
use std::{
    fs,
    io::{ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    thread,
    time::Instant,
};

use bevy_app::{App, TaskPoolPlugin};
use bevy_asset::{AssetApp, AssetPlugin};
use bevy_image::Image;
use bevy_input::mouse::{MouseMotion, MouseWheel};
use bevy_time::TimePlugin;
use bevy_window::WindowCloseRequested;
use engine_core::{EngineCorePlugin, EntityGuid, WorldMode};
use engine_editor_bridge::{
    EditorBridgeConfig, EditorBridgePlugin, EditorIpcServer, EditorSelection,
};
use engine_protocol::{
    decode_viewport_frame, ClientIpcMessage, EntityPresetKind, PixelFormat, SceneLoadParams,
    SceneNewParams, SceneOperationResult, SceneSaveParams, ServerIpcMessage, SetViewportModeParams,
    SetViewportModeResult, SpawnPresetParams, SpawnPresetResult, ViewportCameraMode,
    ViewportFrameHeader, ViewportMode, WorldTreeResult, METHOD_ENTITY_SPAWN_PRESET,
    METHOD_SCENE_LOAD, METHOD_SCENE_NEW, METHOD_SCENE_SAVE, METHOD_VIEWPORT_SET_MODE,
    METHOD_WORLD_TREE, PROTOCOL_VERSION,
};
use engine_render::{
    EngineRenderPlugin, ViewportConfig, ViewportFrameChannel, ViewportFramePacket,
};
use engine_scene::{load_level_from_file, EngineScenePlugin};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};

struct TempFileGuard {
    path: PathBuf,
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn allocate_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port should bind");
    listener
        .local_addr()
        .expect("listener local_addr should be available")
        .port()
}

fn setup_bridge_app(brp_port: u16) -> App {
    let mut app = App::new();
    app.insert_resource(WorldMode::Mode3D)
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
                use_asset_processor_override: Some(false),
                ..Default::default()
            },
            EngineCorePlugin,
            EngineScenePlugin,
            EngineRenderPlugin,
            EditorBridgePlugin,
        ))
        .init_asset::<Image>();

    let sample_scene = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/sandbox_3d/scenes/main.level.ron");
    load_level_from_file(app.world_mut(), &sample_scene)
        .expect("sandbox_3d scene should load cleanly");

    app.update();
    app.update();
    app
}

fn send_http_brp_request(port: u16, method: &str, params: Option<Value>) -> (String, Value) {
    let body = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    }))
    .expect("JSON-RPC payload should serialize");

    let mut stream = None;
    for _ in 0..80 {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
    let mut stream = stream.expect("BRP HTTP server should accept connection");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set_read_timeout should succeed");

    let request = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .expect("HTTP request write should succeed");

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .expect("HTTP response read should succeed");
    let text = String::from_utf8(raw).expect("HTTP response must be valid UTF-8");
    let (headers, raw_body) = text
        .split_once("\r\n\r\n")
        .expect("HTTP response must contain header/body separator");
    let json_body = if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        decode_chunked_body(raw_body)
    } else {
        raw_body.to_owned()
    };
    let parsed: Value = serde_json::from_str(&json_body)
        .unwrap_or_else(|err| panic!("BRP response must be valid JSON ({err}): {json_body:?}"));
    (headers.to_owned(), parsed)
}

fn decode_chunked_body(mut remaining: &str) -> String {
    let mut out = String::new();
    while let Some((size_line, after_size)) = remaining.split_once("\r\n") {
        let hex_size = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(hex_size, 16)
            .unwrap_or_else(|err| panic!("invalid chunk size '{hex_size}': {err}"));
        if size == 0 {
            break;
        }
        assert!(
            after_size.len() >= size,
            "truncated HTTP chunk: expected {size} bytes, got {}",
            after_size.len()
        );
        out.push_str(&after_size[..size]);
        remaining = after_size[size..]
            .strip_prefix("\r\n")
            .unwrap_or(&after_size[size..]);
    }
    out
}

fn call_brp_while_pumping<T: DeserializeOwned>(
    app: &mut App,
    brp_port: u16,
    method: &str,
    params: Option<Value>,
) -> (String, T) {
    let method_owned = method.to_owned();
    let handle = thread::spawn(move || send_http_brp_request(brp_port, &method_owned, params));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !handle.is_finished() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for BRP method '{method}'"
        );
        app.update();
        thread::sleep(Duration::from_millis(5));
    }

    let (headers, response) = handle.join().expect("HTTP client thread should not panic");
    assert!(
        response.get("error").is_none(),
        "unexpected BRP error for '{method}': {response}"
    );
    let result_val = response
        .get("result")
        .cloned()
        .expect("BRP response must include 'result'");
    let typed: T = serde_json::from_value(result_val).expect("BRP result should deserialize");
    (headers, typed)
}

fn recv_ws_message_while_pumping(
    app: &mut App,
    ws: &mut WebSocket<MaybeTlsStream<TcpStream>>,
) -> Message {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for WebSocket IPC message"
        );
        app.update();
        match ws.read() {
            Ok(Message::Ping(payload)) => {
                ws.send(Message::Pong(payload))
                    .expect("pong reply should send");
            }
            Ok(msg) => return msg,
            Err(tungstenite::Error::Io(ref err))
                if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            Err(err) => panic!("WebSocket IPC read failed: {err}"),
        }
    }
}

fn recv_server_json_while_pumping(
    app: &mut App,
    ws: &mut WebSocket<MaybeTlsStream<TcpStream>>,
) -> ServerIpcMessage {
    loop {
        match recv_ws_message_while_pumping(app, ws) {
            Message::Text(text) => {
                let parsed: ServerIpcMessage =
                    serde_json::from_str(text.as_str()).expect("server IPC JSON must deserialize");
                if !matches!(parsed, ServerIpcMessage::Metrics { .. }) {
                    return parsed;
                }
            }
            Message::Binary(_) | Message::Pong(_) => {}
            other => panic!("unexpected WebSocket frame while expecting JSON: {other:?}"),
        }
    }
}

#[test]
fn brp_custom_methods_and_ws_ipc_roundtrip() {
    let brp_port = allocate_free_port();
    let mut app = setup_bridge_app(brp_port);
    let ipc_addr = app.world().resource::<EditorIpcServer>().bound_addr();

    // 1. Verify built-in `registry.schema` + CORS headers.
    let (headers, schema): (String, Value) =
        call_brp_while_pumping(&mut app, brp_port, "registry.schema", None);
    let headers_lower = headers.to_ascii_lowercase();
    assert!(
        headers_lower.contains("access-control-allow-origin: *"),
        "expected CORS header in BRP response headers: {headers}"
    );
    assert!(
        schema
            .get("engine_scene::components::MeshPrimitive3d")
            .is_some(),
        "registry.schema must export MeshPrimitive3d"
    );

    // 2. Verify `engine.world.tree`.
    let (_, initial_tree): (String, WorldTreeResult) =
        call_brp_while_pumping(&mut app, brp_port, METHOD_WORLD_TREE, None);
    assert_eq!(initial_tree.scene_name, "Sandbox 3D");
    assert_eq!(initial_tree.world_mode, WorldMode::Mode3D);
    let showcase = initial_tree
        .roots
        .iter()
        .find(|node| node.name == "ShowcaseGroup")
        .expect("ShowcaseGroup root must exist");
    assert_eq!(showcase.children.len(), 2);

    // 3. Verify `engine.entity.spawn_preset` under ShowcaseGroup.
    let spawn_params = SpawnPresetParams {
        preset: EntityPresetKind::Cube3d,
        name: Some("RpcCube".to_owned()),
        parent_guid: Some(showcase.guid),
        position: Some([1.0, 2.0, 3.0]),
    };
    let (_, spawned): (String, SpawnPresetResult) = call_brp_while_pumping(
        &mut app,
        brp_port,
        METHOD_ENTITY_SPAWN_PRESET,
        Some(serde_json::to_value(spawn_params).unwrap()),
    );
    assert_eq!(spawned.name, "RpcCube");

    let (_, updated_tree): (String, WorldTreeResult) =
        call_brp_while_pumping(&mut app, brp_port, METHOD_WORLD_TREE, None);
    let updated_showcase = updated_tree
        .roots
        .iter()
        .find(|node| node.name == "ShowcaseGroup")
        .expect("ShowcaseGroup must still exist");
    assert!(updated_showcase
        .children
        .iter()
        .any(|child| child.guid == spawned.guid && child.name == "RpcCube"));

    // 4. Verify `engine.viewport.set_mode`.
    let (_, mode_res): (String, SetViewportModeResult) = call_brp_while_pumping(
        &mut app,
        brp_port,
        METHOD_VIEWPORT_SET_MODE,
        Some(
            serde_json::to_value(SetViewportModeParams {
                mode: ViewportMode::NativeWindow,
            })
            .unwrap(),
        ),
    );
    assert_eq!(mode_res.mode, ViewportMode::NativeWindow);
    assert_eq!(
        app.world().resource::<ViewportConfig>().mode,
        ViewportMode::NativeWindow
    );
    app.update();
    assert!(
        app.world()
            .resource::<engine_render::OffscreenRenderState>()
            .detached_window
            .is_some(),
        "detached window entity must be spawned in NativeWindow mode"
    );

    // Switch back to Offscreen before WebSocket tests.
    let (_, _): (String, SetViewportModeResult) = call_brp_while_pumping(
        &mut app,
        brp_port,
        METHOD_VIEWPORT_SET_MODE,
        Some(
            serde_json::to_value(SetViewportModeParams {
                mode: ViewportMode::Offscreen,
            })
            .unwrap(),
        ),
    );
    app.update();
    assert!(
        app.world()
            .resource::<engine_render::OffscreenRenderState>()
            .detached_window
            .is_none(),
        "detached window entity must be despawned when returning to Offscreen mode"
    );

    // 5. Verify `engine.scene.save` -> `engine.scene.new` -> `engine.scene.load`.
    let temp_scene = TempFileGuard {
        path: std::env::temp_dir().join(format!("bridge_smoke_{}.level.ron", EntityGuid::new())),
    };
    let (_, save_res): (String, SceneOperationResult) = call_brp_while_pumping(
        &mut app,
        brp_port,
        METHOD_SCENE_SAVE,
        Some(
            serde_json::to_value(SceneSaveParams {
                path: Some(temp_scene.path.display().to_string()),
                name: Some("Saved Bridge Scene".to_owned()),
            })
            .unwrap(),
        ),
    );
    assert_eq!(save_res.name, "Saved Bridge Scene");
    assert_eq!(save_res.entity_count, 8);

    let (_, new_res): (String, SceneOperationResult) = call_brp_while_pumping(
        &mut app,
        brp_port,
        METHOD_SCENE_NEW,
        Some(
            serde_json::to_value(SceneNewParams {
                name: "Fresh 2D".to_owned(),
                world_mode: WorldMode::Mode2D,
            })
            .unwrap(),
        ),
    );
    assert_eq!(new_res.entity_count, 0);
    assert_eq!(
        app.world().resource::<ViewportConfig>().camera_mode,
        ViewportCameraMode::Mode2D
    );

    let (_, load_res): (String, SceneOperationResult) = call_brp_while_pumping(
        &mut app,
        brp_port,
        METHOD_SCENE_LOAD,
        Some(
            serde_json::to_value(SceneLoadParams {
                path: temp_scene.path.display().to_string(),
            })
            .unwrap(),
        ),
    );
    assert_eq!(load_res.name, "Saved Bridge Scene");
    assert_eq!(load_res.entity_count, 8);
    assert_eq!(
        app.world().resource::<ViewportConfig>().camera_mode,
        ViewportCameraMode::Mode3D
    );

    // Flush change-detection notifications generated during `RemoteLast` before WebSocket handshake.
    app.update();

    // 6. Verify WebSocket IPC handshake, ping/pong, selection sync, and binary BV9F frame stream.
    let (mut ws, _) =
        tungstenite::connect(format!("ws://{ipc_addr}")).expect("WebSocket IPC must connect");
    if let MaybeTlsStream::Plain(stream) = ws.get_ref() {
        stream
            .set_read_timeout(Some(Duration::from_millis(10)))
            .expect("WS read timeout should configure");
    }

    let hello = ClientIpcMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        client_name: "bridge-smoke-test".to_owned(),
    };
    ws.send(Message::text(serde_json::to_string(&hello).unwrap()))
        .expect("Hello should send");

    let msg1 = recv_server_json_while_pumping(&mut app, &mut ws);
    assert!(matches!(
        msg1,
        ServerIpcMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            world_mode: WorldMode::Mode3D,
            viewport_mode: ViewportMode::Offscreen,
            camera_mode: ViewportCameraMode::Mode3D,
            ..
        }
    ));

    let msg2 = recv_server_json_while_pumping(&mut app, &mut ws);
    assert!(matches!(msg2, ServerIpcMessage::HierarchySnapshot { .. }));

    let msg3 = recv_server_json_while_pumping(&mut app, &mut ws);
    assert!(matches!(msg3, ServerIpcMessage::SelectionChanged { ref guids } if guids.is_empty()));

    let msg4 = recv_server_json_while_pumping(&mut app, &mut ws);
    assert!(matches!(
        msg4,
        ServerIpcMessage::ViewportStateChanged {
            viewport_mode: ViewportMode::Offscreen,
            camera_mode: ViewportCameraMode::Mode3D,
            ..
        }
    ));

    let msg5 = recv_server_json_while_pumping(&mut app, &mut ws);
    assert!(matches!(msg5, ServerIpcMessage::GizmoStateChanged { .. }));

    // Ping / Pong
    let ping = ClientIpcMessage::Ping { nonce: 42 };
    ws.send(Message::text(serde_json::to_string(&ping).unwrap()))
        .expect("Ping should send");
    let pong = recv_server_json_while_pumping(&mut app, &mut ws);
    assert_eq!(pong, ServerIpcMessage::Pong { nonce: 42 });

    // Selection synchronization
    let select = ClientIpcMessage::SelectEntities {
        guids: vec![spawned.guid],
    };
    ws.send(Message::text(serde_json::to_string(&select).unwrap()))
        .expect("SelectEntities should send");
    let sel_msg = recv_server_json_while_pumping(&mut app, &mut ws);
    assert_eq!(
        sel_msg,
        ServerIpcMessage::SelectionChanged {
            guids: vec![spawned.guid],
        }
    );
    assert_eq!(
        app.world().resource::<EditorSelection>().guids(),
        &[spawned.guid]
    );

    // Binary BV9F viewport frame broadcast
    let test_pixels: Vec<u8> = (0..(4 * 4 * 4))
        .map(|idx| ((idx * 13) % 251) as u8)
        .collect();
    let packet = ViewportFramePacket {
        header: ViewportFrameHeader {
            protocol_version: PROTOCOL_VERSION,
            frame_id: 7,
            width: 4,
            height: 4,
            format: PixelFormat::Rgba8UnormSrgb,
            payload_len: test_pixels.len() as u32,
        },
        pixels: test_pixels.clone(),
    };
    app.world()
        .resource::<ViewportFrameChannel>()
        .try_send_latest(packet);

    let frame_bytes = loop {
        match recv_ws_message_while_pumping(&mut app, &mut ws) {
            Message::Binary(bytes) => break bytes,
            Message::Text(_) | Message::Pong(_) => {}
            other => panic!("unexpected WS message while waiting for binary frame: {other:?}"),
        }
    };

    let (decoded_header, decoded_pixels) =
        decode_viewport_frame(&frame_bytes).expect("binary BV9F frame must decode cleanly");
    assert_eq!(decoded_header.frame_id, 7);
    assert_eq!(decoded_header.width, 4);
    assert_eq!(decoded_header.height, 4);
    assert_eq!(decoded_header.format, PixelFormat::Rgba8UnormSrgb);
    assert_eq!(decoded_pixels, test_pixels.as_slice());

    let _ = ws.close(None);
}
