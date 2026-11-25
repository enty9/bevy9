//! Runtime executable bootstrapper for the engine.
//!
//! Supports running a project manifest in headless simulation mode, standalone windowed
//! mode, or as an Editor Host process.

use std::{path::PathBuf, time::Duration};

use bevy::{
    app::{AppExit, ScheduleRunnerPlugin},
    asset::AssetPlugin,
    diagnostic::FrameCount,
    log::LogPlugin,
    prelude::*,
    state::app::StatesPlugin,
    window::{PresentMode, WindowPlugin, WindowResolution},
};
use clap::{Parser, ValueEnum};
use engine_core::{EngineCorePlugin, ProjectManifest, WorldMode};

/// Execution mode for `engine_runtime`.
#[derive(ValueEnum, Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuntimeMode {
    /// Runs schedules without opening an OS window or initializing GPU rendering.
    Headless,
    /// Opens a standalone native window for playing or previewing a project.
    #[default]
    Window,
    /// Boots the runtime as an Editor Host process controlled by the Tauri shell.
    Editor,
}

/// Command-line options parsed by `engine_runtime`.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "engine_runtime",
    version,
    about = "Bevy9 Engine runtime host (headless, window, and editor modes)"
)]
pub struct RuntimeCli {
    /// Path to a project directory containing `project.toml` or directly to a manifest file.
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Runtime execution mode (`headless`, `window`, or `editor`).
    #[arg(long, value_enum, default_value_t = RuntimeMode::Window)]
    pub mode: RuntimeMode,

    /// Shorthand flag that forces `headless` mode regardless of `--mode`.
    #[arg(long, default_value_t = false)]
    pub headless: bool,

    /// Optional frame limit after which the runtime exits cleanly.
    #[arg(long)]
    pub max_frames: Option<u32>,

    /// Optional override for the window title in windowed or editor mode.
    #[arg(long)]
    pub title: Option<String>,
}

impl RuntimeCli {
    /// Returns the resolved [`RuntimeMode`], honoring `--headless` if set.
    #[must_use]
    pub const fn effective_mode(&self) -> RuntimeMode {
        if self.headless {
            RuntimeMode::Headless
        } else {
            self.mode
        }
    }
}

/// Non-CLI options controlling runtime initialization (primarily for tests).
#[derive(Debug, Clone, Copy)]
pub struct RuntimeOptions {
    /// Whether to install Bevy's [`LogPlugin`] global tracing subscriber.
    pub enable_logging: bool,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            enable_logging: true,
        }
    }
}

/// Resource storing the maximum frame count before automatic exit.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxFrames(pub u32);

/// Builds a configured Bevy [`App`] from parsed [`RuntimeCli`] arguments using default options.
pub fn build_app(cli: &RuntimeCli) -> anyhow::Result<App> {
    build_app_with_options(cli, RuntimeOptions::default())
}

/// Builds a configured Bevy [`App`] from [`RuntimeCli`] and [`RuntimeOptions`].
pub fn build_app_with_options(cli: &RuntimeCli, options: RuntimeOptions) -> anyhow::Result<App> {
    let (manifest, paths) = match &cli.project {
        Some(project_path) => {
            let (manifest, paths) = ProjectManifest::load_from_path(project_path)?;
            (manifest, Some(paths))
        }
        None => (
            ProjectManifest::new("Untitled Project", WorldMode::Hybrid),
            None,
        ),
    };

    let mode = cli.effective_mode();
    let world_mode = manifest.project.default_mode;
    let asset_file_path = match &paths {
        Some(project_paths) => {
            let resolved = std::fs::canonicalize(&project_paths.assets_dir)
                .or_else(|_| std::path::absolute(&project_paths.assets_dir))?;
            resolved.to_string_lossy().into_owned()
        }
        None => "assets".to_owned(),
    };

    let asset_plugin = AssetPlugin {
        file_path: asset_file_path,
        // Keep AssetProcessor disabled until the DDC stage to avoid generating
        // `imported_assets/` directories in sample projects or test working directories.
        use_asset_processor_override: Some(false),
        ..Default::default()
    };

    let mut app = App::new();
    app.insert_resource(mode)
        .insert_resource(world_mode)
        .insert_resource(manifest.clone());

    if let Some(project_paths) = paths {
        app.insert_resource(project_paths);
    }

    match mode {
        RuntimeMode::Headless => {
            let wait_duration = if cli.max_frames.is_some() {
                Duration::from_millis(1)
            } else {
                Duration::from_secs_f64(1.0 / 60.0)
            };

            app.add_plugins(MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(wait_duration)));

            if options.enable_logging {
                app.add_plugins(LogPlugin::default());
            }

            app.add_plugins((
                asset_plugin,
                TransformPlugin,
                StatesPlugin,
                EngineCorePlugin,
            ));
        }
        RuntimeMode::Window | RuntimeMode::Editor => {
            let window_title = cli
                .title
                .as_deref()
                .unwrap_or(&manifest.window.title)
                .to_owned();
            let present_mode = if manifest.window.vsync {
                PresentMode::AutoVsync
            } else {
                PresentMode::AutoNoVsync
            };

            let primary_window = Window {
                title: window_title,
                resolution: WindowResolution::new(manifest.window.width, manifest.window.height),
                resizable: manifest.window.resizable,
                present_mode,
                ..Default::default()
            };

            let plugins = DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(primary_window),
                    ..Default::default()
                })
                .set(asset_plugin);

            if options.enable_logging {
                app.add_plugins(plugins);
            } else {
                app.add_plugins(plugins.disable::<LogPlugin>());
            }

            app.add_plugins(EngineCorePlugin);

            if mode == RuntimeMode::Editor {
                app.add_systems(Startup, log_editor_host_boot);
            }
        }
    }

    if let Some(limit) = cli.max_frames {
        app.insert_resource(MaxFrames(limit));
        app.add_systems(
            Last,
            check_max_frames.after(bevy::diagnostic::update_frame_count),
        );
    }

    Ok(app)
}

/// Builds and runs the engine application from [`RuntimeCli`].
pub fn run_from_cli(cli: RuntimeCli) -> anyhow::Result<AppExit> {
    let mut app = build_app(&cli)?;
    Ok(app.run())
}

fn log_editor_host_boot(manifest: Res<ProjectManifest>, world_mode: Res<WorldMode>) {
    tracing::info!(
        project = %manifest.project.name,
        world_mode = %*world_mode,
        "engine_runtime booted in Editor Host mode"
    );
}

fn check_max_frames(
    frame_count: Res<FrameCount>,
    max_frames: Res<MaxFrames>,
    mut exit: MessageWriter<AppExit>,
) {
    if frame_count.0 >= max_frames.0 {
        exit.write(AppExit::Success);
    }
}
