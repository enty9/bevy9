//! Integration smoke tests for `engine_runtime`.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use bevy::{
    app::{AppExit, Last},
    diagnostic::FrameCount,
    ecs::schedule::IntoScheduleConfigs,
    ecs::system::Res,
};
use clap::Parser;
use engine_core::{EntityGuidMap, ProjectManifest, ProjectPaths, WorldMode};
use engine_editor_bridge::{DEFAULT_BRP_PORT, DEFAULT_IPC_PORT};
use engine_runtime::{build_app_with_options, RuntimeCli, RuntimeMode, RuntimeOptions};
use engine_scene::ActiveScene;

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
        title: None,
        world_mode: None,
        brp_port: DEFAULT_BRP_PORT,
        ipc_port: DEFAULT_IPC_PORT,
    };

    let mut app = build_app_with_options(
        &cli,
        RuntimeOptions {
            enable_logging: false,
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
    for (dir_name, expected_scene_name, expected_mode, expected_entities) in [
        ("sandbox_3d", "Sandbox 3D", WorldMode::Mode3D, 7),
        ("sandbox_2d", "Sandbox 2D", WorldMode::Mode2D, 5),
    ] {
        let project_dir = sample_project_dir(dir_name);
        let cli = RuntimeCli {
            project: Some(project_dir),
            scene: None,
            mode: RuntimeMode::Headless,
            headless: true,
            max_frames: Some(5),
            title: None,
            world_mode: None,
            brp_port: DEFAULT_BRP_PORT,
            ipc_port: DEFAULT_IPC_PORT,
        };

        let mut app = build_app_with_options(
            &cli,
            RuntimeOptions {
                enable_logging: false,
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

        let exit = app.run();
        assert_eq!(exit, AppExit::Success);
    }
}
