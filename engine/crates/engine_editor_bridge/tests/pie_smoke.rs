//! End-to-end integration tests for Stage 6.1 Play-In-Editor (PIE) lifecycle,
//! BRP methods, WebSocket IPC transitions, physics rollback, audio shutdown,
//! camera handoff/fallback, mutation guards, and 10x leak-free cycles.

use core::time::Duration;
use std::{
    collections::HashSet,
    fs,
    io::{ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};

use bevy_app::{App, TaskPoolPlugin};
use bevy_asset::{AssetApp, AssetPlugin};
use bevy_camera::{Camera, RenderTarget};
use bevy_ecs::{
    hierarchy::{ChildOf, Children},
    name::Name,
    query::With,
};
use bevy_image::Image;
use bevy_input::mouse::{MouseMotion, MouseWheel};
use bevy_math::{Quat, Vec3};
use bevy_mesh::Mesh;
use bevy_pbr::StandardMaterial;
use bevy_sprite_render::ColorMaterial;
use bevy_time::TimePlugin;
use bevy_transform::{components::Transform, TransformPlugin};
use bevy_window::WindowCloseRequested;
use engine_assets::{AssetRegistry, EngineAssetsPlugin, SharedAssetRegistry};
use engine_audio::{
    AudioBackend, AudioBus, AudioEmitter, AudioEngine, AudioError, AudioOptions, AudioSpace,
    BusSettings, EngineAudioPlugin, PlaySound, VoiceId,
};
use engine_core::{
    EngineCorePlugin, EntityGuid, EntityGuidMap, ProjectManifest, WorldMode,
};
use engine_editor_bridge::{
    EditorBridgeConfig, EditorBridgePlugin, EditorIpcServer, EditorSelection, PieController,
    PieSessionState, PieStateTransition,
};
use engine_input::{EngineInputPlugin, InputContexts, InputDomain};
use engine_physics::{
    BodyKind, Collider2D, Collider3D, EnginePhysicsPlugin, PhysicsOptions, PhysicsWorld3D,
    RigidBody2D, RigidBody3D,
};
use engine_protocol::{
    ClientIpcMessage, DeleteEntityParams, EntityPresetKind, GizmoMode, HistoryStatePayload,
    HistoryStepResult, InputModifiers, PieState, PieStatePayload, PieStepParams,
    PointerButtonCode, PrefabSaveParams, SceneLoadParams, SceneNewParams, SceneSaveParams,
    ServerIpcMessage, SetComponentParams, SpawnPresetParams, SpawnPresetResult,
    ViewportCameraMode, ViewportInputEvent, METHOD_COMPONENT_SET, METHOD_ENTITY_DELETE,
    METHOD_ENTITY_SPAWN_PRESET, METHOD_HISTORY_REDO, METHOD_HISTORY_STATE, METHOD_HISTORY_UNDO,
    METHOD_PIE_PAUSE, METHOD_PIE_PLAY, METHOD_PIE_STATE, METHOD_PIE_STEP, METHOD_PIE_STOP,
    METHOD_PREFAB_SAVE, METHOD_SCENE_LOAD, METHOD_SCENE_NEW, METHOD_SCENE_SAVE, PROTOCOL_VERSION,
};
use engine_render::{
    camera::{EditorCamera2d, EditorCamera3d, EditorCameraState},
    gizmo::TransformGizmoState,
    EngineRenderPlugin, OffscreenRenderState, ViewportConfig, ViewportInputQueue,
};
use engine_scene::{
    reset_to_new_scene, spawn_preset, ActiveScene, EngineScenePlugin, PieRuntimeEntity,
    SceneCamera3d, SceneCameraActivationPolicy, SceneEntity,
};
use engine_scripting::{
    ActiveScriptVoices, EngineScriptingPlugin, PendingScriptContacts, PendingScriptImpulses,
    ScriptComponent, ScriptExecutionOptions, ScriptProperties, ScriptRuntime,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{prefix}_{}", EntityGuid::new()));
        fs::create_dir_all(path.join("assets")).expect("temp assets dir should create");
        fs::create_dir_all(path.join("scenes")).expect("temp scenes dir should create");
        Self { path }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Default)]
struct MockAudioState {
    next_id: u64,
    played_total: usize,
    active_voices: HashSet<VoiceId>,
}

#[derive(Clone, Default)]
struct MockAudioBackend {
    state: Arc<Mutex<MockAudioState>>,
}

impl AudioBackend for MockAudioBackend {
    fn play(&mut self, _sound: &PlaySound) -> Result<VoiceId, AudioError> {
        let mut guard = self.state.lock().unwrap();
        guard.next_id += 1;
        let id = VoiceId(guard.next_id);
        guard.played_total += 1;
        guard.active_voices.insert(id);
        Ok(id)
    }

    fn stop(&mut self, voice: VoiceId) {
        self.state.lock().unwrap().active_voices.remove(&voice);
    }

    fn set_bus(&mut self, _bus: AudioBus, _settings: BusSettings) {}
    fn set_listener(&mut self, _position: Vec3, _orientation: Quat) {}
    fn set_position(&mut self, _voice: VoiceId, _position: Vec3) {}
    fn collect_finished(&mut self) {}
}

fn allocate_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port should bind");
    listener
        .local_addr()
        .expect("listener local_addr should exist")
        .port()
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

fn recv_pie_ipc_state(
    app: &mut App,
    ws: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    expected: PieState,
) -> PieStatePayload {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for PieStateChanged({expected:?}) over WebSocket IPC"
        );
        app.update();
        match ws.read() {
            Ok(Message::Text(text)) => {
                if let Ok(ServerIpcMessage::PieStateChanged { state }) =
                    serde_json::from_str::<ServerIpcMessage>(text.as_str())
                    && state.state == expected
                {
                    return state;
                }
            }
            Ok(Message::Ping(payload)) => {
                ws.send(Message::Pong(payload)).unwrap();
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref err))
                if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            Err(err) => panic!("WebSocket IPC read failed: {err}"),
        }
    }
}

fn count_alive_entities(app: &App) -> usize {
    app.world()
        .archetypes()
        .iter()
        .map(|arch| arch.len() as usize)
        .sum()
}

fn setup_full_pie_app(
    brp_port: Option<u16>,
    enable_ws_ipc: bool,
    project_root: Option<&PathBuf>,
    audio_backend: Option<MockAudioBackend>,
) -> App {
    let mut app = App::new();
    app.insert_resource(WorldMode::Mode3D)
        .insert_resource(SceneCameraActivationPolicy {
            allow_scene_cameras: false,
        })
        .insert_resource(PhysicsOptions {
            simulate: false,
            debug: false,
        })
        .insert_resource(AudioOptions {
            device: false,
            gameplay: false,
        })
        .insert_resource(InputContexts {
            domain: InputDomain::Editor,
            ..Default::default()
        })
        .insert_resource(ScriptExecutionOptions::editor())
        .insert_resource(EditorBridgeConfig {
            brp_port: brp_port.unwrap_or(0),
            ipc_port: 0,
            enable_http_brp: brp_port.is_some(),
            enable_ws_ipc,
            allow_cors: false,
            ..Default::default()
        })
        .add_message::<MouseMotion>()
        .add_message::<MouseWheel>()
        .add_message::<WindowCloseRequested>();

    if let Some(root) = project_root {
        let paths = ProjectManifest::new("PIE Smoke", WorldMode::Mode3D).resolve_paths(root);
        let shared =
            SharedAssetRegistry::new(paths.assets_dir.clone(), Some(paths.scenes_dir.clone()));
        app.insert_resource(paths).insert_resource(shared);
    }

    app.add_plugins((
        TaskPoolPlugin::default(),
        TimePlugin,
        TransformPlugin,
        AssetPlugin {
            use_asset_processor_override: Some(false),
            ..Default::default()
        },
        EngineCorePlugin,
        EngineAssetsPlugin,
        EngineScenePlugin,
        EngineInputPlugin,
        EnginePhysicsPlugin,
        EngineAudioPlugin,
        EngineScriptingPlugin,
        EngineRenderPlugin,
        EditorBridgePlugin,
    ))
    .init_asset::<Image>()
    .init_asset::<Mesh>()
    .init_asset::<StandardMaterial>()
    .init_asset::<ColorMaterial>();

    if let Some(backend) = audio_backend {
        app.insert_resource(AudioEngine::with_backend(backend));
    }

    reset_to_new_scene(app.world_mut(), "PIE Test Scene", WorldMode::Mode3D);
    app.update();
    app
}

#[test]
fn pie_state_machine_physics_step_cameras_audio_ipc_and_guards() {
    let temp_project = TempDirGuard::new("pie_smoke_proj");
    let audio_file = temp_project.path.join("assets").join("theme.wav");
    fs::write(&audio_file, b"RIFF0000WAVEfmt ").expect("dummy wav file should write");

    let brp_port = allocate_free_port();
    let mock_audio = MockAudioBackend::default();
    let audio_state = Arc::clone(&mock_audio.state);
    let mut app = setup_full_pie_app(
        Some(brp_port),
        true,
        Some(&temp_project.path),
        Some(mock_audio),
    );
    let ipc_addr = app.world().resource::<EditorIpcServer>().bound_addr();

    // Index the dummy audio clip so AudioEmitter autoplay triggers cleanly during PIE.
    let audio_uuid = {
        let mut reg = app.world_mut().resource_mut::<AssetRegistry>();
        reg.scan_all().expect("asset scan should succeed");
        reg.shared()
            .get_by_path("theme.wav")
            .expect("theme.wav must be indexed")
            .uuid
    };

    // Populate scene: Floor, FallingBall, MainCamera, MusicEmitter.
    let (floor_ent, _) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Plane3d,
        Some("Floor".to_owned()),
        None,
        Some(Transform::from_xyz(0.0, 0.0, 0.0)),
    );
    app.world_mut()
        .entity_mut(floor_ent)
        .insert(Collider3D::Cuboid(Vec3::new(10.0, 0.1, 10.0)));

    let initial_ball_tfm = Transform::from_xyz(0.0, 10.0, 0.0);
    let (ball_ent, ball_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Sphere3d,
        Some("FallingBall".to_owned()),
        None,
        Some(initial_ball_tfm),
    );
    app.world_mut().entity_mut(ball_ent).insert((
        RigidBody3D {
            kind: BodyKind::Dynamic,
            ..Default::default()
        },
        Collider3D::Ball(0.5),
    ));

    let (_, cam_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Camera3d,
        Some("MainCamera".to_owned()),
        None,
        Some(Transform::from_xyz(0.0, 4.0, 12.0)),
    );

    let (audio_ent, _) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Empty,
        Some("MusicEmitter".to_owned()),
        None,
        Some(Transform::IDENTITY),
    );
    app.world_mut().entity_mut(audio_ent).insert(AudioEmitter {
        clip: Some(audio_uuid),
        bus: AudioBus::Music,
        space: AudioSpace::Global,
        autoplay: true,
        looping: true,
        ..Default::default()
    });

    // Spawn one preset via BRP so EditorHistory has a real pre-PIE undo entry.
    let extra_spawn: SpawnPresetResult = call_brp(
        &mut app,
        brp_port,
        METHOD_ENTITY_SPAWN_PRESET,
        Some(
            serde_json::to_value(SpawnPresetParams {
                preset: EntityPresetKind::Cube3d,
                name: Some("PrePieCube".to_owned()),
                parent_guid: None,
                position: Some([3.0, 1.0, 0.0]),
            })
            .unwrap(),
        ),
    );

    // Customize editor camera, selection, and ActiveScene metadata before Play.
    let saved_focus = Vec3::new(4.0, 3.0, 2.0);
    let saved_distance = 11.5;
    {
        let mut cam_state = app.world_mut().resource_mut::<EditorCameraState>();
        cam_state.focus = saved_focus;
        cam_state.distance = saved_distance;
    }
    app.world_mut()
        .resource_mut::<EditorSelection>()
        .set(vec![ball_guid]);
    let saved_scene_path = temp_project.path.join("scenes").join("main.level.ron");
    {
        let mut active = app.world_mut().resource_mut::<ActiveScene>();
        active.path = Some(saved_scene_path.clone());
        active.dirty = true;
    }
    app.update();

    // Connect WebSocket IPC client and verify initial PieStateChanged(Editing).
    let (mut ws, _) =
        tungstenite::connect(format!("ws://{ipc_addr}")).expect("WS IPC must connect");
    if let MaybeTlsStream::Plain(stream) = ws.get_ref() {
        stream
            .set_read_timeout(Some(Duration::from_millis(10)))
            .unwrap();
    }
    ws.send(Message::text(
        serde_json::to_string(&ClientIpcMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "pie-smoke-test".to_owned(),
        })
        .unwrap(),
    ))
    .unwrap();
    let initial_ipc_pie = recv_pie_ipc_state(&mut app, &mut ws, PieState::Editing);
    assert_eq!(initial_ipc_pie.state, PieState::Editing);
    assert!(!initial_ipc_pie.using_scene_camera);

    // 1. Edit -> Pause must fail cleanly; Step with steps=0 must fail cleanly.
    let pause_err = call_brp_raw(&mut app, brp_port, METHOD_PIE_PAUSE, None);
    assert!(
        pause_err.get("error").is_some(),
        "pausing while Editing must return an error"
    );
    let zero_step_err = call_brp_raw(
        &mut app,
        brp_port,
        METHOD_PIE_STEP,
        Some(serde_json::to_value(PieStepParams { steps: 0 }).unwrap()),
    );
    assert!(
        zero_step_err.get("error").is_some(),
        "stepping with steps=0 must return an INVALID_PARAMS error"
    );

    // 2. Edit -> Play via BRP.
    let play_res: PieStatePayload = call_brp(&mut app, brp_port, METHOD_PIE_PLAY, None);
    assert_eq!(play_res.state, PieState::Playing);
    assert!(play_res.using_scene_camera);

    let ipc_playing = recv_pie_ipc_state(&mut app, &mut ws, PieState::Playing);
    assert_eq!(ipc_playing.state, PieState::Playing);
    assert!(ipc_playing.using_scene_camera);

    // Verify subsystems activated cleanly.
    assert!(app.world().resource::<PhysicsOptions>().simulate);
    assert!(app.world().resource::<AudioOptions>().gameplay);
    assert_eq!(
        app.world().resource::<InputContexts>().domain,
        InputDomain::Game
    );
    assert!(
        app.world()
            .resource::<SceneCameraActivationPolicy>()
            .allow_scene_cameras
    );
    assert!(!app.world().resource::<ViewportConfig>().show_grid);

    // Verify scene camera took over rendering and targets the offscreen image while editor camera deactivated.
    {
        let expected_img = app
            .world()
            .resource::<OffscreenRenderState>()
            .image_handle
            .clone()
            .expect("offscreen render target must exist");
        let cam_ent = app
            .world()
            .resource::<EntityGuidMap>()
            .get_entity(cam_guid)
            .expect("MainCamera present");
        assert!(app.world().get::<Camera>(cam_ent).unwrap().is_active);
        assert!(matches!(
            app.world().get::<RenderTarget>(cam_ent),
            Some(RenderTarget::Image(img)) if img.handle == expected_img
        ));
        let world = app.world_mut();
        let mut ed_q = world.query_filtered::<&Camera, With<EditorCamera3d>>();
        assert!(!ed_q.single(world).unwrap().is_active);
    }

    // Verify gizmo shortcuts (W/E/R) are disabled while Playing.
    assert_eq!(
        app.world().resource::<TransformGizmoState>().mode,
        GizmoMode::Translate
    );
    app.world_mut()
        .resource_mut::<ViewportInputQueue>()
        .push(ViewportInputEvent::Key {
            code: "KeyR".to_owned(),
            pressed: true,
            modifiers: InputModifiers::default(),
        });

    // Forward viewport camera orbit input during Play and pump frames until physics advances.
    app.world_mut()
        .resource_mut::<ViewportInputQueue>()
        .push(ViewportInputEvent::PointerButton {
            button: PointerButtonCode::Right,
            pressed: true,
            x: 100.0,
            y: 100.0,
            modifiers: InputModifiers::default(),
        });
    app.world_mut()
        .resource_mut::<ViewportInputQueue>()
        .push(ViewportInputEvent::PointerMove {
            x: 180.0,
            y: 140.0,
            delta_x: 80.0,
            delta_y: 40.0,
        });

    let deadline = Instant::now() + Duration::from_secs(3);
    while app.world().resource::<PieSessionState>().fixed_steps() < 5 {
        assert!(Instant::now() < deadline, "physics fixed steps did not advance during PIE Play");
        thread::sleep(Duration::from_millis(12));
        app.update();
    }

    // Editor camera remained frozen and gizmo shortcuts remained disabled during Play.
    assert_eq!(
        app.world().resource::<TransformGizmoState>().mode,
        GizmoMode::Translate
    );
    assert_eq!(app.world().resource::<EditorCameraState>().focus, saved_focus);
    assert_eq!(
        app.world().resource::<EditorCameraState>().distance,
        saved_distance
    );

    // Verify audio autoplay voice started during PIE.
    assert!(
        !audio_state.lock().unwrap().active_voices.is_empty(),
        "autoplay AudioEmitter must start a voice during PIE Play"
    );

    // Verify FallingBall fell under gravity.
    let ball_ent_live = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(ball_guid)
        .unwrap();
    let y_during_play = app
        .world()
        .get::<Transform>(ball_ent_live)
        .unwrap()
        .translation
        .y;
    assert!(
        y_during_play < initial_ball_tfm.translation.y - 0.01,
        "FallingBall must fall under gravity during Play (y={y_during_play})"
    );

    // 3. Verify destructive scene/prefab saves and undo/redo are rejected during PIE.
    assert!(call_brp_raw(
        &mut app,
        brp_port,
        METHOD_SCENE_SAVE,
        Some(serde_json::to_value(SceneSaveParams { path: None, name: None }).unwrap())
    )
    .get("error")
    .is_some());
    assert!(call_brp_raw(
        &mut app,
        brp_port,
        METHOD_SCENE_NEW,
        Some(
            serde_json::to_value(SceneNewParams {
                name: "Blocked".to_owned(),
                world_mode: WorldMode::Mode3D,
            })
            .unwrap()
        )
    )
    .get("error")
    .is_some());
    assert!(call_brp_raw(
        &mut app,
        brp_port,
        METHOD_SCENE_LOAD,
        Some(
            serde_json::to_value(SceneLoadParams {
                path: saved_scene_path.display().to_string(),
            })
            .unwrap()
        )
    )
    .get("error")
    .is_some());
    assert!(call_brp_raw(&mut app, brp_port, METHOD_HISTORY_UNDO, None)
        .get("error")
        .is_some());
    assert!(call_brp_raw(&mut app, brp_port, METHOD_HISTORY_REDO, None)
        .get("error")
        .is_some());
    assert!(call_brp_raw(
        &mut app,
        brp_port,
        METHOD_PREFAB_SAVE,
        Some(
            serde_json::to_value(PrefabSaveParams {
                root_guid: ball_guid,
                path: "prefabs/blocked.prefab.ron".to_owned(),
                name: None,
            })
            .unwrap()
        )
    )
    .get("error")
    .is_some());
    let masked_hist: HistoryStatePayload =
        call_brp(&mut app, brp_port, METHOD_HISTORY_STATE, None);
    assert!(!masked_hist.can_undo && !masked_hist.can_redo);

    // 4. Play -> Pause via BRP.
    let pause_res: PieStatePayload = call_brp(&mut app, brp_port, METHOD_PIE_PAUSE, None);
    assert_eq!(pause_res.state, PieState::Paused);
    assert!(!app.world().resource::<PhysicsOptions>().simulate);
    let _ = recv_pie_ipc_state(&mut app, &mut ws, PieState::Paused);

    let y_paused = app
        .world()
        .get::<Transform>(ball_ent_live)
        .unwrap()
        .translation
        .y;
    let steps_paused = app.world().resource::<PieSessionState>().fixed_steps();
    let vel_paused = app
        .world()
        .resource::<PhysicsWorld3D>()
        .linear_velocity(ball_ent_live)
        .expect("ball must remain in PhysicsWorld3D while Paused");
    assert!(vel_paused.y < -0.1, "ball must retain downward velocity while Paused");

    // Pump several frames while Paused: ball and physics velocity must remain frozen and intact.
    for _ in 0..5 {
        thread::sleep(Duration::from_millis(12));
        app.update();
    }
    assert_eq!(
        app.world().resource::<PieSessionState>().fixed_steps(),
        steps_paused
    );
    assert_eq!(
        app.world()
            .get::<Transform>(ball_ent_live)
            .unwrap()
            .translation
            .y,
        y_paused
    );
    assert_eq!(
        app.world()
            .resource::<PhysicsWorld3D>()
            .linear_velocity(ball_ent_live),
        Some(vel_paused),
        "PhysicsWorld3D must NOT be reset while Paused"
    );

    // 5. Pause -> Step (exactly 1 fixed tick) via BRP.
    let step_res: PieStatePayload = call_brp(&mut app, brp_port, METHOD_PIE_STEP, None);
    assert_eq!(step_res.state, PieState::Paused);
    assert_eq!(step_res.fixed_steps, steps_paused + 1);
    assert!(!app.world().resource::<PhysicsOptions>().simulate);

    let y_after_step = app
        .world()
        .get::<Transform>(ball_ent_live)
        .unwrap()
        .translation
        .y;
    assert!(
        y_after_step < y_paused,
        "Step must advance physics by 1 fixed tick (before={y_paused}, after={y_after_step})"
    );

    // Pump again while still Paused -> stays frozen at stepped pose.
    for _ in 0..4 {
        thread::sleep(Duration::from_millis(10));
        app.update();
    }
    assert_eq!(
        app.world().resource::<PieSessionState>().fixed_steps(),
        steps_paused + 1
    );
    assert_eq!(
        app.world()
            .get::<Transform>(ball_ent_live)
            .unwrap()
            .translation
            .y,
        y_after_step
    );

    // 6. Pause -> Play (Resume) via BRP.
    let resume_res: PieStatePayload = call_brp(&mut app, brp_port, METHOD_PIE_PLAY, None);
    assert_eq!(resume_res.state, PieState::Playing);
    assert!(app.world().resource::<PhysicsOptions>().simulate);

    let deadline = Instant::now() + Duration::from_secs(3);
    while app.world().resource::<PieSessionState>().fixed_steps() < steps_paused + 5 {
        assert!(Instant::now() < deadline, "resumed PIE Play did not advance");
        thread::sleep(Duration::from_millis(12));
        app.update();
    }
    let y_after_resume = app
        .world()
        .get::<Transform>(ball_ent_live)
        .unwrap()
        .translation
        .y;
    assert!(y_after_resume < y_after_step);

    // 7. Play -> Stop via BRP.
    let stop_res: PieStatePayload = call_brp(&mut app, brp_port, METHOD_PIE_STOP, None);
    assert_eq!(stop_res.state, PieState::Editing);
    assert_eq!(stop_res.fixed_steps, 0);
    assert_eq!(stop_res.frame_count, 0);
    assert_eq!(stop_res.elapsed_secs, 0.0);
    assert!(!stop_res.using_scene_camera);

    let state_query: PieStatePayload = call_brp(&mut app, brp_port, METHOD_PIE_STATE, None);
    assert_eq!(state_query, stop_res);

    let _ = recv_pie_ipc_state(&mut app, &mut ws, PieState::Editing);
    app.update();

    // Verify all subsystems restored to Editor mode.
    assert!(!app.world().resource::<PhysicsOptions>().simulate);
    assert!(!app.world().resource::<AudioOptions>().gameplay);
    assert_eq!(
        app.world().resource::<InputContexts>().domain,
        InputDomain::Editor
    );
    assert!(
        !app.world()
            .resource::<SceneCameraActivationPolicy>()
            .allow_scene_cameras
    );
    assert!(app.world().resource::<ViewportConfig>().show_grid);
    assert!(
        audio_state.lock().unwrap().active_voices.is_empty(),
        "all active gameplay audio voices must stop cleanly on PIE Stop"
    );

    // Verify scene rollback: FallingBall back at exact y=10.0, editor camera & selection & dirty restored!
    let restored_ball = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(ball_guid)
        .expect("FallingBall GUID restored");
    assert_eq!(
        app.world().get::<Transform>(restored_ball),
        Some(&initial_ball_tfm)
    );
    assert_eq!(
        app.world().resource::<EditorSelection>().guids(),
        &[ball_guid]
    );
    assert_eq!(app.world().resource::<EditorCameraState>().focus, saved_focus);
    assert_eq!(
        app.world().resource::<EditorCameraState>().distance,
        saved_distance
    );
    assert_eq!(
        app.world().resource::<ActiveScene>().path.as_ref(),
        Some(&saved_scene_path)
    );
    assert!(app.world().resource::<ActiveScene>().dirty);

    // Verify camera handoff back to EditorCamera3d.
    {
        let cam_ent = app
            .world()
            .resource::<EntityGuidMap>()
            .get_entity(cam_guid)
            .expect("MainCamera restored");
        assert!(!app.world().get::<Camera>(cam_ent).unwrap().is_active);
        let world = app.world_mut();
        let mut ed_q = world.query_filtered::<&Camera, With<EditorCamera3d>>();
        assert!(ed_q.single(world).unwrap().is_active);
    }

    // Verify pre-PIE EditorHistory survived untouched and Undo works again!
    let restored_hist: HistoryStatePayload =
        call_brp(&mut app, brp_port, METHOD_HISTORY_STATE, None);
    assert!(restored_hist.can_undo);
    assert_eq!(restored_hist.undo_len, 1);

    let undo_res: HistoryStepResult = call_brp(&mut app, brp_port, METHOD_HISTORY_UNDO, None);
    assert!(undo_res.applied);
    assert!(
        !app.world()
            .resource::<EntityGuidMap>()
            .contains_guid(extra_spawn.guid),
        "pre-PIE undo must still work cleanly after PIE Stop"
    );

    // 8. Edit -> Step (2 fixed steps) directly from Editing, then Stop rollback.
    let edit_step: PieStatePayload = call_brp(
        &mut app,
        brp_port,
        METHOD_PIE_STEP,
        Some(serde_json::to_value(PieStepParams { steps: 2 }).unwrap()),
    );
    assert_eq!(edit_step.state, PieState::Paused);
    assert_eq!(edit_step.fixed_steps, 2);
    let ball_ent_stepped = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(ball_guid)
        .unwrap();
    assert!(
        app.world()
            .get::<Transform>(ball_ent_stepped)
            .unwrap()
            .translation
            .y
            < initial_ball_tfm.translation.y
    );

    let stop_after_step: PieStatePayload = call_brp(&mut app, brp_port, METHOD_PIE_STOP, None);
    assert_eq!(stop_after_step.state, PieState::Editing);
    let restored_ball = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(ball_guid)
        .unwrap();
    assert_eq!(
        app.world().get::<Transform>(restored_ball),
        Some(&initial_ball_tfm)
    );

    let _ = ws.close(None);
}

#[test]
fn pie_camera_fallback_to_editor_camera_when_no_active_scene_camera() {
    let mut app = setup_full_pie_app(None, false, None, None);

    // Spawn only an inactive scene camera (no active scene cameras in the scene).
    let (inactive_cam, _) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Camera3d,
        Some("InactiveCam".to_owned()),
        None,
        None,
    );
    app.world_mut()
        .get_mut::<SceneCamera3d>(inactive_cam)
        .unwrap()
        .active = false;
    app.update();

    let play = PieController::play(app.world_mut()).expect("Play must succeed");
    assert_eq!(play.state, PieState::Playing);
    assert!(
        !play.using_scene_camera,
        "PIE must fall back to editor camera when no active scene camera exists"
    );
    app.update();

    {
        let world = app.world_mut();
        let mut ed_q = world.query_filtered::<&Camera, With<EditorCamera3d>>();
        assert!(
            ed_q.single(world).unwrap().is_active,
            "EditorCamera3d must remain active during fallback"
        );
    }

    // Spawning an active SceneCamera3d mid-PIE switches cleanly to the scene camera.
    let (live_cam, _) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Camera3d,
        Some("RuntimeCam".to_owned()),
        None,
        None,
    );
    app.update();

    assert!(PieController::state(app.world()).using_scene_camera);
    assert!(app.world().get::<Camera>(live_cam).unwrap().is_active);
    {
        let world = app.world_mut();
        let mut ed_q = world.query_filtered::<&Camera, With<EditorCamera3d>>();
        assert!(!ed_q.single(world).unwrap().is_active);
    }

    // Stop rolls back the spawned camera and restores EditorCamera3d.
    let stopped = PieController::stop(app.world_mut()).expect("Stop must succeed");
    assert_eq!(stopped.state, PieState::Editing);
    assert!(!stopped.using_scene_camera);
    app.update();

    {
        let world = app.world_mut();
        let mut ed_q = world.query_filtered::<&Camera, With<EditorCamera3d>>();
        assert!(ed_q.single(world).unwrap().is_active);
    }
}

#[test]
fn pie_10x_cycles_bridge_with_physics_spawn_mutate_delete_zero_leaks() {
    let brp_port = allocate_free_port();
    let mut app = setup_full_pie_app(Some(brp_port), false, None, None);

    let (group_ent, group_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Empty,
        Some("DynamicGroup".to_owned()),
        None,
        Some(Transform::from_xyz(0.0, 1.0, 0.0)),
    );
    let initial_box_tfm = Transform::from_xyz(0.0, 8.0, 0.0);
    let (box_ent, box_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Cube3d,
        Some("PhysicsBox".to_owned()),
        Some(group_ent),
        Some(initial_box_tfm),
    );
    app.world_mut().entity_mut(box_ent).insert((
        RigidBody3D {
            kind: BodyKind::Dynamic,
            ..Default::default()
        },
        Collider3D::Cuboid(Vec3::splat(0.5)),
    ));

    let (victim_ent, victim_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Sphere3d,
        Some("Destructible".to_owned()),
        Some(group_ent),
        Some(Transform::from_xyz(2.0, 0.0, 0.0)),
    );
    let _ = victim_ent;

    app.update();
    app.update();

    let baseline_entities = count_alive_entities(&app);
    let mut baseline_guids: Vec<EntityGuid> = app
        .world()
        .resource::<EntityGuidMap>()
        .iter()
        .map(|(g, _)| g)
        .collect();
    baseline_guids.sort_unstable();
    assert_eq!(baseline_guids.len(), 3);

    let mut first_cycle_stepped_y: Option<f32> = None;

    for cycle in 0..10 {
        // Enter PIE via Step (5 deterministic fixed ticks) so physics runs deterministically!
        let stepped: PieStatePayload = call_brp(
            &mut app,
            brp_port,
            METHOD_PIE_STEP,
            Some(serde_json::to_value(PieStepParams { steps: 5 }).unwrap()),
        );
        assert_eq!(stepped.state, PieState::Paused);
        assert_eq!(stepped.fixed_steps, 5);

        let box_live = app
            .world()
            .resource::<EntityGuidMap>()
            .get_entity(box_guid)
            .expect("PhysicsBox present in PIE");
        let y_after_5 = app.world().get::<Transform>(box_live).unwrap().translation.y;
        assert!(y_after_5 < initial_box_tfm.translation.y);
        if let Some(expected_y) = first_cycle_stepped_y {
            assert!(
                (y_after_5 - expected_y).abs() < 1e-5,
                "cycle {cycle}: physics rollback must be deterministic (expected y={expected_y}, got {y_after_5})"
            );
        } else {
            first_cycle_stepped_y = Some(y_after_5);
        }

        // Spawn a new SceneEntity via BRP during PIE (must NOT pollute EditorHistory).
        let _: SpawnPresetResult = call_brp(
            &mut app,
            brp_port,
            METHOD_ENTITY_SPAWN_PRESET,
            Some(
                serde_json::to_value(SpawnPresetParams {
                    preset: EntityPresetKind::Cube3d,
                    name: Some(format!("RuntimeSpawn_{cycle}")),
                    parent_guid: Some(group_guid),
                    position: Some([1.0, 2.0, 3.0]),
                })
                .unwrap(),
            ),
        );

        // Mutate an existing entity's Name via BRP during PIE.
        let _: Value = call_brp(
            &mut app,
            brp_port,
            METHOD_COMPONENT_SET,
            Some(
                serde_json::to_value(SetComponentParams {
                    guid: box_guid,
                    type_path: "bevy_ecs::name::Name".to_owned(),
                    value: json!(format!("RenamedInPie_{cycle}")),
                    merge_key: None,
                    label: None,
                })
                .unwrap(),
            ),
        );

        // Delete an authored entity via BRP during PIE.
        let _: Value = call_brp(
            &mut app,
            brp_port,
            METHOD_ENTITY_DELETE,
            Some(serde_json::to_value(DeleteEntityParams { guid: victim_guid }).unwrap()),
        );

        // Also spawn a transient PieRuntimeEntity + child without SceneEntity.
        let rt_root = app
            .world_mut()
            .spawn((PieRuntimeEntity, Name::new(format!("Vfx_{cycle}"))))
            .id();
        app.world_mut()
            .spawn((Name::new("VfxParticle"), ChildOf(rt_root)));
        app.update();

        // Stop PIE and verify zero leaked entities + exact restoration!
        let stopped: PieStatePayload = call_brp(&mut app, brp_port, METHOD_PIE_STOP, None);
        assert_eq!(stopped.state, PieState::Editing);
        app.update();

        assert_eq!(
            count_alive_entities(&app),
            baseline_entities,
            "cycle {cycle}: entity count leaked after PIE Stop"
        );
        let world = app.world();
        let guid_map = world.resource::<EntityGuidMap>();
        let mut restored_guids: Vec<EntityGuid> = guid_map.iter().map(|(g, _)| g).collect();
        restored_guids.sort_unstable();
        assert_eq!(restored_guids, baseline_guids);

        let restored_group = guid_map.get_entity(group_guid).unwrap();
        let restored_box = guid_map.get_entity(box_guid).unwrap();
        let restored_victim = guid_map.get_entity(victim_guid).unwrap();

        assert_eq!(
            world.get::<Transform>(restored_box),
            Some(&initial_box_tfm)
        );
        assert_eq!(
            world.get::<Name>(restored_box).map(Name::as_str),
            Some("PhysicsBox")
        );
        assert_eq!(
            world.get::<ChildOf>(restored_box).map(ChildOf::parent),
            Some(restored_group)
        );
        assert_eq!(
            world.get::<ChildOf>(restored_victim).map(ChildOf::parent),
            Some(restored_group)
        );
        assert_eq!(
            world.get::<Children>(restored_group).map(|c| c.len()),
            Some(2)
        );

        // EditorHistory must remain untouched (0 entries) across all 10 cycles.
        let hist: HistoryStatePayload = call_brp(&mut app, brp_port, METHOD_HISTORY_STATE, None);
        assert_eq!(hist.undo_len, 0);
        assert_eq!(hist.redo_len, 0);

        let scene_count = app
            .world_mut()
            .query_filtered::<(), With<SceneEntity>>()
            .iter(app.world())
            .count();
        assert_eq!(scene_count, 3);
    }
}

#[test]
fn pie_2d_scene_camera_physics_and_idempotent_transitions() {
    #[derive(bevy_ecs::resource::Resource, Default)]
    struct RecordedTransitions(Vec<(PieState, PieState)>);

    let mut app = setup_full_pie_app(None, false, None, None);
    app.init_resource::<RecordedTransitions>();
    app.add_observer(
        |event: bevy_ecs::observer::On<PieStateTransition>,
         mut log: bevy_ecs::system::ResMut<RecordedTransitions>| {
            log.0.push((event.previous, event.current));
        },
    );

    reset_to_new_scene(app.world_mut(), "2D PIE Scene", WorldMode::Mode2D);
    app.world_mut().resource_mut::<ViewportConfig>().camera_mode = ViewportCameraMode::Mode2D;

    let initial_sprite_tfm = Transform::from_xyz(0.0, 200.0, 0.0);
    let (sprite_ent, sprite_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Sprite2d,
        Some("FallingSprite".to_owned()),
        None,
        Some(initial_sprite_tfm),
    );
    app.world_mut().entity_mut(sprite_ent).insert((
        RigidBody2D {
            kind: BodyKind::Dynamic,
            ..Default::default()
        },
        Collider2D::Ball(16.0),
    ));

    let (_, cam2d_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Camera2d,
        Some("MainCamera2D".to_owned()),
        None,
        None,
    );
    app.update();

    // Idempotent Stop while Editing does not emit transitions.
    assert_eq!(
        PieController::stop(app.world_mut()).unwrap().state,
        PieState::Editing
    );
    assert!(app.world().resource::<RecordedTransitions>().0.is_empty());

    // Play activates SceneCamera2d and deactivates EditorCamera2d.
    let play = PieController::play(app.world_mut()).unwrap();
    assert_eq!(play.state, PieState::Playing);
    assert!(play.using_scene_camera);
    app.update();

    let cam2d_ent = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(cam2d_guid)
        .unwrap();
    assert!(app.world().get::<Camera>(cam2d_ent).unwrap().is_active);
    {
        let world = app.world_mut();
        let mut ed2 = world.query_filtered::<&Camera, With<EditorCamera2d>>();
        assert!(!ed2.single(world).unwrap().is_active);
    }

    // Step 3 deterministic 2D physics ticks and verify double-Play does not clobber pre-PIE snapshot.
    let stepped = PieController::step(app.world_mut(), 3).unwrap();
    assert_eq!(stepped.state, PieState::Paused);
    assert_eq!(stepped.fixed_steps, 3);
    // Double-Pause is an idempotent no-op.
    assert_eq!(
        PieController::pause(app.world_mut()).unwrap().state,
        PieState::Paused
    );

    let sprite_live = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(sprite_guid)
        .unwrap();
    assert!(
        app.world()
            .get::<Transform>(sprite_live)
            .unwrap()
            .translation
            .y
            < initial_sprite_tfm.translation.y
    );

    // Resume and double-Play (must not overwrite pre-PIE snapshot!).
    assert_eq!(
        PieController::play(app.world_mut()).unwrap().state,
        PieState::Playing
    );
    assert_eq!(
        PieController::play(app.world_mut()).unwrap().state,
        PieState::Playing
    );

    // Stop rolls back 2D sprite to exact initial y=200.0 and reactivates EditorCamera2d.
    assert_eq!(
        PieController::stop(app.world_mut()).unwrap().state,
        PieState::Editing
    );
    app.update();

    let sprite_restored = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(sprite_guid)
        .unwrap();
    assert_eq!(
        app.world().get::<Transform>(sprite_restored),
        Some(&initial_sprite_tfm)
    );
    let cam2d_restored = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(cam2d_guid)
        .unwrap();
    assert!(!app.world().get::<Camera>(cam2d_restored).unwrap().is_active);
    {
        let world = app.world_mut();
        let mut ed2 = world.query_filtered::<&Camera, With<EditorCamera2d>>();
        assert!(ed2.single(world).unwrap().is_active);
    }

    assert_eq!(
        app.world().resource::<RecordedTransitions>().0,
        vec![
            (PieState::Editing, PieState::Playing),
            (PieState::Playing, PieState::Paused),
            (PieState::Paused, PieState::Playing),
            (PieState::Playing, PieState::Editing),
        ]
    );
}

#[test]
fn pie_scripting_play_pause_step_stop_and_zero_leaks() {
    let temp_project = TempDirGuard::new("pie_script_smoke");
    let clip_path = temp_project.path.join("assets").join("beep.wav");
    fs::write(&clip_path, b"RIFF0000WAVEfmt ").expect("dummy wav should write");
    let scripts_dir = temp_project.path.join("assets").join("scripts");
    fs::create_dir_all(&scripts_dir).expect("scripts dir should create");
    fs::write(
        scripts_dir.join("mover.rhai"),
        r#"
        let local_starts = 0;
        let local_updates = 0;
        let local_fixed = 0;

        fn on_start() {
            local_starts += 1;
            props.starts = local_starts;
            audio.play_with_options("beep.wav", #{ bus: "sfx", looping: true });
        }

        fn on_update(dt) {
            local_updates += 1;
            props.updates = local_updates;
            transform.translate(props.speed * dt, 0.0, 0.0);
        }

        fn on_fixed_update(dt) {
            local_fixed += 1;
            props.fixed_ticks = local_fixed;
        }
        "#,
    )
    .expect("mover.rhai should write");

    let mock_audio = MockAudioBackend::default();
    let audio_state = Arc::clone(&mock_audio.state);
    let mut app = setup_full_pie_app(None, false, Some(&temp_project.path), Some(mock_audio));

    let script_uuid = {
        let mut reg = app.world_mut().resource_mut::<AssetRegistry>();
        reg.scan_all().expect("asset scan should succeed");
        let rec = reg
            .get_by_path("scripts/mover.rhai")
            .expect("mover.rhai must be indexed");
        assert_eq!(rec.kind, engine_protocol::AssetKind::Script);
        rec.uuid
    };

    let initial_tfm = Transform::from_xyz(1.0, 2.0, 3.0);
    let initial_props = ScriptProperties::new()
        .with("speed", 6.0_f64)
        .with("starts", 0_i64)
        .with("updates", 0_i64)
        .with("fixed_ticks", 0_i64);
    let mut initial_script = ScriptComponent::from_uuid(script_uuid).with_properties(initial_props);
    initial_script.script_path = "scripts/mover.rhai".to_owned();

    let (actor_ent, actor_guid) = spawn_preset(
        app.world_mut(),
        EntityPresetKind::Cube3d,
        Some("ScriptedActor".to_owned()),
        None,
        Some(initial_tfm),
    );
    app.world_mut()
        .entity_mut(actor_ent)
        .insert(initial_script.clone());

    // 1. In Editing mode, scripts remain disabled and untouched.
    assert!(!app.world().resource::<ScriptExecutionOptions>().enabled);
    for _ in 0..4 {
        thread::sleep(Duration::from_millis(10));
        app.update();
    }
    assert_eq!(app.world().get::<Transform>(actor_ent), Some(&initial_tfm));
    assert_eq!(
        app.world()
            .get::<ScriptComponent>(actor_ent)
            .unwrap()
            .properties
            .get_int("starts"),
        Some(0)
    );
    assert!(audio_state.lock().unwrap().active_voices.is_empty());

    // 2. Edit -> Play: scripts execute, transform advances, audio voice starts.
    assert_eq!(
        PieController::play(app.world_mut()).unwrap().state,
        PieState::Playing
    );
    assert!(app.world().resource::<ScriptExecutionOptions>().enabled);

    let deadline = Instant::now() + Duration::from_secs(3);
    while app.world().resource::<PieSessionState>().fixed_steps() < 3 {
        assert!(Instant::now() < deadline, "PIE fixed steps must advance");
        thread::sleep(Duration::from_millis(12));
        app.update();
    }

    let live_actor = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(actor_guid)
        .unwrap();
    let live_comp = app
        .world()
        .get::<ScriptComponent>(live_actor)
        .unwrap()
        .clone();
    assert_eq!(live_comp.properties.get_int("starts"), Some(1));
    assert!(live_comp.properties.get_int("updates").unwrap_or(0) >= 1);
    assert!(live_comp.properties.get_int("fixed_ticks").unwrap_or(0) >= 3);
    let x_during_play = app
        .world()
        .get::<Transform>(live_actor)
        .unwrap()
        .translation
        .x;
    assert!(
        x_during_play > initial_tfm.translation.x + 0.01,
        "script must advance transform during Play (x={x_during_play})"
    );
    assert!(
        !audio_state.lock().unwrap().active_voices.is_empty(),
        "script audio voice must be active during Play"
    );

    // 3. Play -> Pause: scripting freezes while preserving live scope and state.
    assert_eq!(
        PieController::pause(app.world_mut()).unwrap().state,
        PieState::Paused
    );
    assert!(!app.world().resource::<ScriptExecutionOptions>().enabled);
    assert!(
        app.world()
            .resource::<ScriptRuntime>()
            .backend()
            .has_entity_instance(live_actor),
        "Pause must preserve live script scope"
    );

    let paused_x = app
        .world()
        .get::<Transform>(live_actor)
        .unwrap()
        .translation
        .x;
    let paused_updates = app
        .world()
        .get::<ScriptComponent>(live_actor)
        .unwrap()
        .properties
        .get_int("updates")
        .unwrap();
    let paused_fixed = app
        .world()
        .get::<ScriptComponent>(live_actor)
        .unwrap()
        .properties
        .get_int("fixed_ticks")
        .unwrap();

    for _ in 0..5 {
        thread::sleep(Duration::from_millis(10));
        app.update();
    }
    assert_eq!(
        app.world()
            .get::<Transform>(live_actor)
            .unwrap()
            .translation
            .x,
        paused_x
    );
    assert_eq!(
        app.world()
            .get::<ScriptComponent>(live_actor)
            .unwrap()
            .properties
            .get_int("updates"),
        Some(paused_updates)
    );
    assert_eq!(
        app.world()
            .get::<ScriptComponent>(live_actor)
            .unwrap()
            .properties
            .get_int("fixed_ticks"),
        Some(paused_fixed)
    );

    // 4. Pause -> Step(2): deterministically advances fixed + update hooks by 2 steps, then re-pauses.
    let stepped = PieController::step(app.world_mut(), 2).unwrap();
    assert_eq!(stepped.state, PieState::Paused);
    assert!(!app.world().resource::<ScriptExecutionOptions>().enabled);

    let stepped_comp = app
        .world()
        .get::<ScriptComponent>(live_actor)
        .unwrap()
        .clone();
    assert_eq!(
        stepped_comp.properties.get_int("starts"),
        Some(1),
        "Step must NOT re-trigger on_start"
    );
    assert_eq!(
        stepped_comp.properties.get_int("fixed_ticks"),
        Some(paused_fixed + 2)
    );
    assert_eq!(
        stepped_comp.properties.get_int("updates"),
        Some(paused_updates + 2)
    );
    let stepped_x = app
        .world()
        .get::<Transform>(live_actor)
        .unwrap()
        .translation
        .x;
    assert!(
        stepped_x > paused_x + 0.05,
        "Step must advance script translation (before={paused_x}, after={stepped_x})"
    );

    // Remains frozen after Step.
    for _ in 0..3 {
        thread::sleep(Duration::from_millis(10));
        app.update();
    }
    assert_eq!(
        app.world()
            .get::<ScriptComponent>(live_actor)
            .unwrap()
            .properties
            .get_int("updates"),
        Some(paused_updates + 2)
    );

    // 5. Pause -> Play (Resume): continues from preserved scope without restarting on_start.
    assert_eq!(
        PieController::play(app.world_mut()).unwrap().state,
        PieState::Playing
    );
    assert!(app.world().resource::<ScriptExecutionOptions>().enabled);
    let target_fixed = stepped.fixed_steps + 2;
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.world().resource::<PieSessionState>().fixed_steps() < target_fixed {
        assert!(Instant::now() < deadline, "resumed PIE must advance");
        thread::sleep(Duration::from_millis(12));
        app.update();
    }
    assert_eq!(
        app.world()
            .get::<ScriptComponent>(live_actor)
            .unwrap()
            .properties
            .get_int("starts"),
        Some(1)
    );
    assert!(
        app.world()
            .get::<ScriptComponent>(live_actor)
            .unwrap()
            .properties
            .get_int("updates")
            .unwrap()
            > paused_updates + 2
    );

    // 6. Play -> Stop: resets scripting runtime, stops script audio voices, restores exact snapshot!
    assert_eq!(
        PieController::stop(app.world_mut()).unwrap().state,
        PieState::Editing
    );
    app.update();

    assert!(!app.world().resource::<ScriptExecutionOptions>().enabled);
    assert!(
        audio_state.lock().unwrap().active_voices.is_empty(),
        "script-triggered audio voices must be stopped on PIE Stop"
    );
    assert!(app.world().resource::<ActiveScriptVoices>().is_empty());
    assert!(app.world().resource::<PendingScriptContacts>().is_empty());
    assert!(app
        .world()
        .resource::<PendingScriptImpulses>()
        .impulses_3d
        .is_empty());
    assert!(app
        .world()
        .resource::<PendingScriptImpulses>()
        .impulses_2d
        .is_empty());

    let restored_actor = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(actor_guid)
        .expect("ScriptedActor restored after Stop");
    assert!(!app
        .world()
        .resource::<ScriptRuntime>()
        .backend()
        .has_entity_instance(restored_actor));
    assert_eq!(
        app.world().get::<Transform>(restored_actor),
        Some(&initial_tfm)
    );
    assert_eq!(
        app.world().get::<ScriptComponent>(restored_actor),
        Some(&initial_script)
    );

    // 7. Second Play -> Stop cycle runs on_start cleanly from 0 -> 1 with zero leaked state.
    assert_eq!(
        PieController::play(app.world_mut()).unwrap().state,
        PieState::Playing
    );
    let step_second = PieController::step(app.world_mut(), 1).unwrap();
    assert_eq!(step_second.fixed_steps, 1);
    let second_actor = app
        .world()
        .resource::<EntityGuidMap>()
        .get_entity(actor_guid)
        .unwrap();
    let second_props = &app
        .world()
        .get::<ScriptComponent>(second_actor)
        .unwrap()
        .properties;
    assert_eq!(second_props.get_int("starts"), Some(1));
    assert_eq!(second_props.get_int("fixed_ticks"), Some(1));
    assert_eq!(second_props.get_int("updates"), Some(1));

    assert_eq!(
        PieController::stop(app.world_mut()).unwrap().state,
        PieState::Editing
    );
    assert!(audio_state.lock().unwrap().active_voices.is_empty());
    assert!(app.world().resource::<ActiveScriptVoices>().is_empty());
}
