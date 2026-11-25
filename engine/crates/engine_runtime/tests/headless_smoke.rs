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
use engine_core::{ProjectManifest, ProjectPaths, WorldMode};
use engine_runtime::{build_app_with_options, RuntimeCli, RuntimeMode, RuntimeOptions};

fn sample_empty_project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/empty_project")
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

    let editor_cli =
        RuntimeCli::try_parse_from(["engine_runtime", "--mode", "editor", "--title", "Test Host"])
            .expect("editor CLI args should parse");
    assert_eq!(editor_cli.effective_mode(), RuntimeMode::Editor);
    assert_eq!(editor_cli.title.as_deref(), Some("Test Host"));
}

#[test]
fn headless_empty_project_runs_and_exits() {
    let project_dir = sample_empty_project_dir();
    let cli = RuntimeCli {
        project: Some(project_dir.clone()),
        mode: RuntimeMode::Window,
        headless: true,
        max_frames: Some(5),
        title: None,
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
