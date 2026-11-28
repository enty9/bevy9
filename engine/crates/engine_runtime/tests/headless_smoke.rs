//! Integration smoke tests for `engine_runtime`.

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use bevy::{
    app::{AppExit, Last},
    asset::Assets,
    diagnostic::FrameCount,
    ecs::{reflect::AppTypeRegistry, schedule::IntoScheduleConfigs, system::Res},
    input::keyboard::KeyCode,
    math::{Quat, Vec3},
    time::TimeUpdateStrategy,
    transform::components::Transform,
};
use clap::Parser;
use engine_audio::{AudioBackend, AudioBus, AudioEngine, AudioError, BusSettings, PlaySound, VoiceId};
use engine_core::{EntityGuid, EntityGuidMap, ProjectManifest, ProjectPaths, WorldMode};
use engine_editor_bridge::{EditorIpcServer, DEFAULT_BRP_PORT, DEFAULT_IPC_PORT};
use engine_input::{ForwardedInput, InputMappingAsset, ProjectInputMapping};
use engine_physics::{CharacterMotion2D, PhysicsOptions};
use engine_runtime::{build_app_with_options, RuntimeCli, RuntimeMode, RuntimeOptions};
use engine_scene::{ActiveScene, SpriteDesc};
use engine_scripting::{
    RhaiBackend, ScriptBackend, ScriptComponent, ScriptDiagnostics, ScriptExecutionOptions,
    ScriptSourceKey,
};

fn sample_project_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../samples/{name}"))
}

#[test]
fn cli_parses_headless_and_editor_flags() {
    let cli = RuntimeCli::try_parse_from([
        "engine_runtime",
        "--headless",
        "--project",
        "samples/empty_project",
        "--max-frames",
        "5",
    ])
    .expect("headless CLI args should parse");
    assert_eq!(cli.effective_mode(), RuntimeMode::Headless);
    assert_eq!(cli.max_frames, Some(5));
    assert_eq!(cli.project, Some(PathBuf::from("samples/empty_project")));
    assert_eq!(cli.brp_port, DEFAULT_BRP_PORT);
    assert_eq!(cli.ipc_port, DEFAULT_IPC_PORT);

    let editor_cli = RuntimeCli::try_parse_from([
        "engine_runtime",
        "--mode",
        "editor",
        "--title",
        "Test Host",
        "--world-mode",
        "2d",
        "--scene",
        "scenes/main.level.ron",
        "--brp-port",
        "16702",
        "--ipc-port",
        "16704",
    ])
    .expect("editor CLI args should parse");
    assert_eq!(editor_cli.effective_mode(), RuntimeMode::Editor);
    assert_eq!(editor_cli.title.as_deref(), Some("Test Host"));
    assert_eq!(editor_cli.world_mode, Some(WorldMode::Mode2D));
    assert_eq!(
        editor_cli.scene,
        Some(PathBuf::from("scenes/main.level.ron"))
    );
    assert_eq!(editor_cli.brp_port, 16702);
    assert_eq!(editor_cli.ipc_port, 16704);
}

#[test]
fn headless_empty_project_runs_and_exits() {
    let project_dir = sample_project_dir("empty_project");
    let cli = RuntimeCli {
        project: Some(project_dir.clone()),
        scene: None,
        mode: RuntimeMode::Window,
        headless: true,
        max_frames: Some(5),
        physics_debug: false,
        title: None,
        world_mode: None,
        brp_port: DEFAULT_BRP_PORT,
        ipc_port: DEFAULT_IPC_PORT,
    };

    let mut app = build_app_with_options(
        &cli,
        RuntimeOptions {
            enable_logging: false,
            ..Default::default()
        },
    )
    .expect("headless app should build for empty_project");

    // Verify resources before `App::run()`, because `App::run()` moves the inner World
    // into the runner and leaves an empty placeholder in `app`.
    {
        let world = app.world();
        let manifest = world.resource::<ProjectManifest>();
        assert_eq!(manifest.project.name, "Empty Project");
        assert_eq!(*world.resource::<WorldMode>(), WorldMode::Hybrid);

        let paths = world.resource::<ProjectPaths>();
        assert_eq!(paths.root_dir, project_dir);
    }

    let observed_frames = Arc::new(AtomicU32::new(0));
    let observed_frames_system = Arc::clone(&observed_frames);
    app.add_systems(
        Last,
        (move |frame_count: Res<FrameCount>| {
            observed_frames_system.store(frame_count.0, Ordering::Relaxed);
        })
        .after(bevy::diagnostic::update_frame_count),
    );

    let exit = app.run();
    assert_eq!(exit, AppExit::Success);
    assert!(
        observed_frames.load(Ordering::Relaxed) >= 5,
        "expected at least 5 frames to execute, observed {}",
        observed_frames.load(Ordering::Relaxed)
    );
}

#[test]
fn headless_sandbox_3d_and_2d_load_startup_scenes() {
    for (dir_name, expected_scene_name, expected_mode, expected_entities, ball_guid) in [
        (
            "sandbox_3d",
            "Sandbox 3D",
            WorldMode::Mode3D,
            8,
            "10000000-0000-4000-8000-000000000006",
        ),
        (
            "sandbox_2d",
            "Sandbox 2D",
            WorldMode::Mode2D,
            7,
            "20000000-0000-4000-8000-000000000004",
        ),
    ] {
        let project_dir = sample_project_dir(dir_name);
        let cli = RuntimeCli {
            project: Some(project_dir),
            scene: None,
            mode: RuntimeMode::Headless,
            headless: true,
            max_frames: Some(20),
            physics_debug: false,
            title: None,
            world_mode: None,
            brp_port: DEFAULT_BRP_PORT,
            ipc_port: DEFAULT_IPC_PORT,
        };

        let mut app = build_app_with_options(
            &cli,
            RuntimeOptions {
                enable_logging: false,
                ..Default::default()
            },
        )
        .expect("headless sandbox app should build");

        {
            let world = app.world();
            let active_scene = world.resource::<ActiveScene>();
            assert_eq!(active_scene.name, expected_scene_name);
            assert_eq!(active_scene.world_mode, expected_mode);
            assert_eq!(world.resource::<EntityGuidMap>().len(), expected_entities);
        }

        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            1. / 60.,
        )));
        let ball = app
            .world()
            .resource::<EntityGuidMap>()
            .get_entity(EntityGuid::parse_str(ball_guid).unwrap())
            .unwrap();
        let initial_y = app.world().get::<Transform>(ball).unwrap().translation.y;
        for _ in 0..12 {
            app.update();
        }
        assert!(app.world().get::<Transform>(ball).unwrap().translation.y < initial_y);
        let exit = app.run();
        assert_eq!(exit, AppExit::Success);
    }
}

#[derive(Clone, Default)]
struct RecordingAudioBackend {
    played: Arc<Mutex<Vec<PathBuf>>>,
}

impl AudioBackend for RecordingAudioBackend {
    fn play(&mut self, sound: &PlaySound) -> Result<VoiceId, AudioError> {
        let mut played = self.played.lock().unwrap();
        played.push(sound.path.clone());
        Ok(VoiceId(played.len() as u64))
    }

    fn stop(&mut self, _voice: VoiceId) {}
    fn set_bus(&mut self, _bus: AudioBus, _settings: BusSettings) {}
    fn set_listener(&mut self, _position: Vec3, _orientation: Quat) {}
    fn set_position(&mut self, _voice: VoiceId, _position: Vec3) {}
    fn collect_finished(&mut self) {}
}

fn wait_for_input_mapping(app: &mut bevy::app::App) {
    let handle = app
        .world()
        .get_resource::<ProjectInputMapping>()
        .map(|m| m.0.clone())
        .expect("sample project must configure input_mapping");
    let deadline = Instant::now() + Duration::from_secs(5);
    while app
        .world()
        .resource::<Assets<InputMappingAsset>>()
        .get(&handle)
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for gameplay.input.ron asset to finish loading"
        );
        app.update();
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn sandbox_3d_and_2d_rhai_scripts_compile_and_simulate_without_errors() {
    // 1. Direct RhaiBackend compilation check for every `.rhai` script in both samples.
    let mut backend = RhaiBackend::default();
    let sample_scripts = [
        ("sandbox_3d", "assets/scripts/player_3d.rhai"),
        ("sandbox_3d", "assets/scripts/collectible_trigger_3d.rhai"),
        ("sandbox_3d", "assets/scripts/camera_follow_3d.rhai"),
        ("sandbox_2d", "assets/scripts/player_2d.rhai"),
        ("sandbox_2d", "assets/scripts/collectible_trigger_2d.rhai"),
    ];
    for (sample, rel) in sample_scripts {
        let path = sample_project_dir(sample).join(rel);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed reading {}: {err}", path.display()));
        let key = ScriptSourceKey::Path(format!("{sample}/{rel}"));
        let info = backend
            .compile_source(&key, &path.display().to_string(), &source, None)
            .unwrap_or_else(|err| panic!("Rhai compile failed for {}: {err}", path.display()));
        assert!(
            info.hooks.on_start
                || info.hooks.on_update
                || info.hooks.on_fixed_update
                || info.hooks.on_trigger_enter,
            "sample script {} should define at least one lifecycle hook",
            path.display()
        );
    }

    // 2. Headless simulation of `sandbox_3d`: physics, trigger, WASD + Space jump, audio, camera follow.
    {
        let cli = RuntimeCli {
            project: Some(sample_project_dir("sandbox_3d")),
            scene: None,
            mode: RuntimeMode::Headless,
            headless: true,
            max_frames: None,
            physics_debug: false,
            title: None,
            world_mode: None,
            brp_port: DEFAULT_BRP_PORT,
            ipc_port: DEFAULT_IPC_PORT,
        };
        let mut app = build_app_with_options(
            &cli,
            RuntimeOptions {
                enable_logging: false,
                ..Default::default()
            },
        )
        .expect("sandbox_3d headless app should build");
        assert!(app.world().resource::<ScriptExecutionOptions>().enabled);

        let audio_backend = RecordingAudioBackend::default();
        let played_3d = Arc::clone(&audio_backend.played);
        app.insert_resource(AudioEngine::with_backend(audio_backend));
        app.init_resource::<ForwardedInput>();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            1.0 / 60.0,
        )));

        wait_for_input_mapping(&mut app);

        let hero_ent = app
            .world()
            .resource::<EntityGuidMap>()
            .get_entity(EntityGuid::parse_str("10000000-0000-4000-8000-000000000005").unwrap())
            .expect("HeroCube must exist");
        let trigger_ent = app
            .world()
            .resource::<EntityGuidMap>()
            .get_entity(EntityGuid::parse_str("10000000-0000-4000-8000-000000000008").unwrap())
            .expect("Trigger zone must exist");
        let cam_ent = app
            .world()
            .resource::<EntityGuidMap>()
            .get_entity(EntityGuid::parse_str("10000000-0000-4000-8000-000000000007").unwrap())
            .expect("MainCamera3D must exist");

        let initial_cam_pos = app.world().get::<Transform>(cam_ent).unwrap().translation;

        // Let HeroCube land on the floor and AccentSphere fall through Trigger zone.
        for _ in 0..45 {
            app.update();
        }
        let x_before_move = app.world().get::<Transform>(hero_ent).unwrap().translation.x;

        // Press D to move +X and Space to jump!
        {
            let mut forwarded = app.world_mut().resource_mut::<ForwardedInput>();
            forwarded.keys.press(KeyCode::KeyD);
            forwarded.keys.press(KeyCode::Space);
        }
        app.update();
        app.world_mut()
            .resource_mut::<ForwardedInput>()
            .keys
            .release(KeyCode::Space);
        for _ in 0..20 {
            app.update();
        }
        app.world_mut()
            .resource_mut::<ForwardedInput>()
            .keys
            .release(KeyCode::KeyD);
        for _ in 0..5 {
            app.update();
        }

        let diags = app.world().resource::<ScriptDiagnostics>().entries();
        assert!(
            diags.is_empty(),
            "sandbox_3d must run with zero script diagnostics, got: {diags:#?}"
        );

        let x_after_move = app.world().get::<Transform>(hero_ent).unwrap().translation.x;
        assert!(
            x_after_move > x_before_move + 0.1,
            "HeroCube must move +X when KeyD is held (before={x_before_move}, after={x_after_move})"
        );
        let hero_jumps = app
            .world()
            .get::<ScriptComponent>(hero_ent)
            .unwrap()
            .properties
            .get_int("jumps")
            .unwrap_or(0);
        assert!(hero_jumps >= 1, "HeroCube must jump when Space is pressed");

        let trigger_props = &app
            .world()
            .get::<ScriptComponent>(trigger_ent)
            .unwrap()
            .properties;
        assert!(
            trigger_props.get_int("trigger_count").unwrap_or(0) >= 1,
            "sandbox_3d Trigger zone must fire on_trigger_enter"
        );
        assert!(
            trigger_props.get_int("total_score").unwrap_or(0) >= 10,
            "sandbox_3d Trigger zone must increment total_score"
        );
        assert_ne!(
            app.world().get::<Transform>(cam_ent).unwrap().translation,
            initial_cam_pos,
            "MainCamera3D follow script must update camera position"
        );

        let played_paths = played_3d.lock().unwrap();
        assert!(
            played_paths.iter().any(|p| p.ends_with("jump.wav")),
            "sandbox_3d must play jump.wav, played: {played_paths:?}"
        );
        assert!(
            played_paths.iter().any(|p| p.ends_with("pickup.wav")),
            "sandbox_3d must play pickup.wav, played: {played_paths:?}"
        );
    }

    // 3. Headless simulation of `sandbox_2d`: 2D character controller, flip_x, jump, trigger, audio.
    {
        let cli = RuntimeCli {
            project: Some(sample_project_dir("sandbox_2d")),
            scene: None,
            mode: RuntimeMode::Headless,
            headless: true,
            max_frames: None,
            physics_debug: false,
            title: None,
            world_mode: None,
            brp_port: DEFAULT_BRP_PORT,
            ipc_port: DEFAULT_IPC_PORT,
        };
        let mut app = build_app_with_options(
            &cli,
            RuntimeOptions {
                enable_logging: false,
                ..Default::default()
            },
        )
        .expect("sandbox_2d headless app should build");
        assert!(app.world().resource::<ScriptExecutionOptions>().enabled);

        let audio_backend = RecordingAudioBackend::default();
        let played_2d = Arc::clone(&audio_backend.played);
        app.insert_resource(AudioEngine::with_backend(audio_backend));
        app.init_resource::<ForwardedInput>();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            1.0 / 60.0,
        )));

        wait_for_input_mapping(&mut app);

        let player_ent = app
            .world()
            .resource::<EntityGuidMap>()
            .get_entity(EntityGuid::parse_str("20000000-0000-4000-8000-000000000003").unwrap())
            .expect("PlayerSprite must exist");
        let trigger_ent = app
            .world()
            .resource::<EntityGuidMap>()
            .get_entity(EntityGuid::parse_str("20000000-0000-4000-8000-000000000007").unwrap())
            .expect("2D Trigger zone must exist");

        // Let PlayerSprite drop onto Physics floor and AccentCircle pass through Trigger zone.
        for _ in 0..50 {
            app.update();
        }
        assert!(
            app.world()
                .get::<CharacterMotion2D>(player_ent)
                .is_some_and(|m| m.grounded),
            "PlayerSprite must land on Physics floor"
        );

        let x_before_left = app
            .world()
            .get::<Transform>(player_ent)
            .unwrap()
            .translation
            .x;
        let y_grounded = app
            .world()
            .get::<Transform>(player_ent)
            .unwrap()
            .translation
            .y;

        // Move left (KeyA) and jump (Space) to test CharacterController2D + SpriteDesc.flip_x + jump audio!
        {
            let mut forwarded = app.world_mut().resource_mut::<ForwardedInput>();
            forwarded.keys.press(KeyCode::KeyA);
            forwarded.keys.press(KeyCode::Space);
        }
        app.update();
        app.world_mut()
            .resource_mut::<ForwardedInput>()
            .keys
            .release(KeyCode::Space);
        for _ in 0..10 {
            app.update();
        }
        let y_mid_jump = app
            .world()
            .get::<Transform>(player_ent)
            .unwrap()
            .translation
            .y;
        let x_after_left = app
            .world()
            .get::<Transform>(player_ent)
            .unwrap()
            .translation
            .x;
        app.world_mut()
            .resource_mut::<ForwardedInput>()
            .keys
            .release(KeyCode::KeyA);
        for _ in 0..5 {
            app.update();
        }

        let diags = app.world().resource::<ScriptDiagnostics>().entries();
        assert!(
            diags.is_empty(),
            "sandbox_2d must run with zero script diagnostics, got: {diags:#?}"
        );

        assert!(
            x_after_left < x_before_left - 10.0,
            "PlayerSprite must move -X when KeyA is held (before={x_before_left}, after={x_after_left})"
        );
        assert!(
            y_mid_jump > y_grounded + 10.0,
            "PlayerSprite must jump upward when Space is pressed (ground={y_grounded}, air={y_mid_jump})"
        );
        assert!(
            app.world().get::<SpriteDesc>(player_ent).unwrap().flip_x,
            "PlayerSprite must flip horizontally via Reflect bridge when moving left"
        );
        let player_jumps = app
            .world()
            .get::<ScriptComponent>(player_ent)
            .unwrap()
            .properties
            .get_int("jumps")
            .unwrap_or(0);
        assert!(player_jumps >= 1, "PlayerSprite jumps counter must increment");

        let trigger_props = &app
            .world()
            .get::<ScriptComponent>(trigger_ent)
            .unwrap()
            .properties;
        assert!(
            trigger_props.get_int("trigger_count").unwrap_or(0) >= 1,
            "sandbox_2d Trigger zone must fire on_trigger_enter"
        );
        assert!(
            trigger_props.get_int("total_score").unwrap_or(0) >= 25,
            "sandbox_2d Trigger zone must increment total_score"
        );

        let played_paths = played_2d.lock().unwrap();
        assert!(
            played_paths.iter().any(|p| p.ends_with("jump.wav")),
            "sandbox_2d must play jump.wav, played: {played_paths:?}"
        );
        assert!(
            played_paths.iter().any(|p| p.ends_with("pickup.wav")),
            "sandbox_2d must play pickup.wav, played: {played_paths:?}"
        );
    }
}

fn allocate_ephemeral_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("ephemeral port should bind");
    listener
        .local_addr()
        .expect("listener local_addr should be available")
        .port()
}

fn probe_brp_world_tree_http(port: u16) -> String {
    let body = r#"{"jsonrpc":"2.0","id":0,"method":"engine.world.tree"}"#;
    let req = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );

    let mut stream = None;
    for _ in 0..100 {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
    let mut stream = stream.expect("Editor Host BRP HTTP server should accept connections");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set_read_timeout should succeed");
    stream
        .write_all(req.as_bytes())
        .expect("BRP HTTP request write should succeed");

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .expect("BRP HTTP response read should succeed");
    String::from_utf8(raw).expect("BRP HTTP response should be valid UTF-8")
}

#[test]
fn editor_mode_boots_and_passes_brp_readiness_for_both_sandboxes() {
    for (dir_name, expected_scene_name, expected_mode, expected_entities, scripted_guid) in [
        (
            "sandbox_3d",
            "Sandbox 3D",
            WorldMode::Mode3D,
            8usize,
            "10000000-0000-4000-8000-000000000005",
        ),
        (
            "sandbox_2d",
            "Sandbox 2D",
            WorldMode::Mode2D,
            7usize,
            "20000000-0000-4000-8000-000000000003",
        ),
    ] {
        let brp_port = allocate_ephemeral_port();
        let ipc_port = allocate_ephemeral_port();
        let cli = RuntimeCli {
            project: Some(sample_project_dir(dir_name)),
            scene: None,
            mode: RuntimeMode::Editor,
            headless: false,
            max_frames: None,
            physics_debug: false,
            title: None,
            world_mode: None,
            brp_port,
            ipc_port,
        };

        let mut app = build_app_with_options(
            &cli,
            RuntimeOptions {
                enable_logging: false,
                headless_graphics: true,
            },
        )
        .unwrap_or_else(|err| panic!("Editor Host app should build for {dir_name}: {err:#}"));

        {
            let world = app.world();
            let active_scene = world.resource::<ActiveScene>();
            assert_eq!(active_scene.name, expected_scene_name);
            assert_eq!(active_scene.world_mode, expected_mode);
            assert_eq!(world.resource::<EntityGuidMap>().len(), expected_entities);
            assert!(
                !world.resource::<ScriptExecutionOptions>().enabled,
                "Editor mode must start with scripts paused until PIE Play"
            );
            assert!(
                !world.resource::<PhysicsOptions>().simulate,
                "Editor mode must start with physics paused until PIE Play"
            );
            assert!(
                world.contains_resource::<EditorIpcServer>(),
                "Editor mode must bind WebSocket IPC server"
            );
            let registry = world.resource::<AppTypeRegistry>().read();
            assert!(
                registry
                    .get_with_type_path("engine_scripting::component::ScriptComponent")
                    .is_some(),
                "ScriptComponent must be registered in Editor mode AppTypeRegistry"
            );
            let scripted_entity = world
                .resource::<EntityGuidMap>()
                .get_entity(EntityGuid::parse_str(scripted_guid).unwrap())
                .expect("scripted entity must exist in loaded editor scene");
            assert!(
                world.get::<ScriptComponent>(scripted_entity).is_some(),
                "scripted entity must retain ScriptComponent when loaded in Editor mode"
            );
        }

        // Pump startup and initial frames, then issue the exact HTTP BRP readiness probe
        // that `engine/editor/src-tauri/src/supervisor.rs` sends.
        app.update();
        app.update();

        let probe_handle = thread::spawn(move || probe_brp_world_tree_http(brp_port));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !probe_handle.is_finished() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for Editor Host BRP readiness probe on {dir_name}"
            );
            app.update();
            thread::sleep(Duration::from_millis(5));
        }

        let response = probe_handle
            .join()
            .expect("BRP probe client thread should not panic");
        assert!(
            response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200"),
            "Editor Host BRP readiness probe for {dir_name} must return HTTP 200, got:\n{response}"
        );
        assert!(
            response.contains(scripted_guid),
            "BRP engine.world.tree response for {dir_name} must include scripted entity {scripted_guid}, got:\n{response}"
        );
    }
}
