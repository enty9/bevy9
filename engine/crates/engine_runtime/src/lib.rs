//! Runtime executable bootstrapper for the engine.
//!
//! Supports running a project manifest in headless simulation mode, standalone windowed
//! mode, or as an Editor Host process.

pub mod bootstrap_preview;

use std::{path::PathBuf, time::Duration};

use bevy::{
    app::{AppExit, ScheduleRunnerPlugin},
    asset::AssetPlugin,
    diagnostic::FrameCount,
    log::LogPlugin,
    prelude::*,
    state::app::StatesPlugin,
    window::{ExitCondition, PresentMode, WindowPlugin, WindowResolution},
    winit::{UpdateMode, WinitSettings},
};
use bootstrap_preview::BootstrapPreviewPlugin;
use clap::{Parser, ValueEnum};
use engine_assets::{
    configure_asset_plugin_for_project, configure_asset_processor_ddc_log,
    register_uuid_asset_source, EngineAssetsPlugin, SharedAssetRegistry,
};
use engine_audio::{AudioOptions, EngineAudioPlugin};
use engine_core::{EngineCorePlugin, ProjectManifest, ProjectPaths, WorldMode};
use engine_editor_bridge::{
    EditorBridgeConfig, EditorBridgePlugin, EditorIpcServer, DEFAULT_BRP_PORT, DEFAULT_IPC_PORT,
};
use engine_input::{EngineInputPlugin, InputContexts, InputDomain, ProjectInputMapping};
use engine_physics::{EnginePhysicsPlugin, PhysicsDebugPlugin, PhysicsOptions};
use engine_protocol::ViewportCameraMode;
use engine_render::{EngineRenderPlugin, ViewportConfig};
use engine_scene::{load_level_from_file, EngineScenePlugin, SceneCameraActivationPolicy};

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

    /// Optional `.level.ron` scene path overriding the project's configured `startup_scene`.
    #[arg(long)]
    pub scene: Option<PathBuf>,

    /// Runtime execution mode (`headless`, `window`, or `editor`).
    #[arg(long, value_enum, default_value_t = RuntimeMode::Window)]
    pub mode: RuntimeMode,

    /// Shorthand flag that forces `headless` mode regardless of `--mode`.
    #[arg(long, default_value_t = false)]
    pub headless: bool,

    /// Optional frame limit after which the runtime exits cleanly.
    #[arg(long)]
    pub max_frames: Option<u32>,

    /// Draw physics collider and joint wireframes when rendering a viewport.
    #[arg(long)]
    pub physics_debug: bool,

    /// Optional override for the window title in windowed or editor mode.
    #[arg(long)]
    pub title: Option<String>,

    /// Optional override for the project's default dimensional mode (`2d`, `3d`, or `hybrid`).
    #[arg(long, value_name = "2d|3d|hybrid")]
    pub world_mode: Option<WorldMode>,

    /// TCP port bound by the Bevy Remote Protocol HTTP server in `editor` mode.
    #[arg(long, default_value_t = DEFAULT_BRP_PORT)]
    pub brp_port: u16,

    /// TCP port bound by the `WebSocket` IPC + frame server in `editor` mode.
    #[arg(long, default_value_t = DEFAULT_IPC_PORT)]
    pub ipc_port: u16,
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

    let startup_scene_path =
        resolve_startup_scene_path(cli.scene.as_ref(), paths.as_ref(), &manifest)?;

    let mode = cli.effective_mode();
    let world_mode = cli.world_mode.unwrap_or(manifest.project.default_mode);
    let enable_processor = mode != RuntimeMode::Headless;
    let shared_registry = paths.as_ref().map_or_else(
        SharedAssetRegistry::default,
        |project_paths| {
            SharedAssetRegistry::new(
                project_paths.assets_dir.clone(),
                Some(project_paths.scenes_dir.clone()),
            )
        },
    );

    let (asset_plugin, ddc_config) = match &paths {
        Some(project_paths) => {
            let (plugin, ddc) = configure_asset_plugin_for_project(project_paths, enable_processor);
            (plugin, Some(ddc))
        }
        None => (
            AssetPlugin {
                file_path: "assets".to_owned(),
                use_asset_processor_override: Some(false),
                ..Default::default()
            },
            None,
        ),
    };

    let mut app = App::new();
    app.insert_resource(mode)
        .insert_resource(world_mode)
        .insert_resource(manifest.clone())
        .insert_resource(shared_registry.clone());

    if let Some(ddc) = &ddc_config {
        app.insert_resource(ddc.clone());
    }

    if let Some(project_paths) = paths {
        app.insert_resource(project_paths);
    }

    register_uuid_asset_source(&mut app, shared_registry);

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
            let window_plugin = if mode == RuntimeMode::Editor {
                // Start windowless in Offscreen mode so Wayland/X11 never maps a blank window;
                // detached native windows are spawned dynamically on `ViewportMode::NativeWindow`.
                WindowPlugin {
                    primary_window: None,
                    exit_condition: ExitCondition::DontExit,
                    close_when_requested: false,
                    ..Default::default()
                }
            } else {
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
                    resolution: WindowResolution::new(
                        manifest.window.width,
                        manifest.window.height,
                    ),
                    resizable: manifest.window.resizable,
                    present_mode,
                    ..Default::default()
                };

                WindowPlugin {
                    primary_window: Some(primary_window),
                    ..Default::default()
                }
            };

            let plugins = DefaultPlugins.set(window_plugin).set(asset_plugin);

            if options.enable_logging {
                app.add_plugins(plugins);
            } else {
                app.add_plugins(plugins.disable::<LogPlugin>());
            }

            if let Some(ddc) = &ddc_config {
                configure_asset_processor_ddc_log(&mut app, ddc);
            }

            if mode == RuntimeMode::Editor {
                // Keep the main loop ticking at 60 Hz when 0 native OS windows exist.
                app.insert_resource(WinitSettings {
                    focused_mode: UpdateMode::Continuous,
                    unfocused_mode: UpdateMode::reactive_low_power(Duration::from_secs_f64(
                        1.0 / 60.0,
                    )),
                });
            }

            app.add_plugins(EngineCorePlugin);
        }
    }

    if mode == RuntimeMode::Editor {
        app.insert_resource(SceneCameraActivationPolicy {
            allow_scene_cameras: false,
        });
    }

    app.insert_resource(InputContexts {
        domain: if mode == RuntimeMode::Editor {
            InputDomain::Editor
        } else {
            InputDomain::Game
        },
        ..Default::default()
    });
    app.insert_resource(PhysicsOptions {
        simulate: mode != RuntimeMode::Editor,
        debug: cli.physics_debug && mode != RuntimeMode::Headless,
    });
    app.insert_resource(AudioOptions {
        device: mode != RuntimeMode::Headless,
        gameplay: mode == RuntimeMode::Window,
    });
    app.add_plugins((
        EngineAssetsPlugin,
        EngineScenePlugin,
        EngineInputPlugin,
        EnginePhysicsPlugin,
        EngineAudioPlugin,
    ));
    if mode != RuntimeMode::Headless {
        app.add_plugins(PhysicsDebugPlugin);
    }
    if let Some(mapping) = &manifest.project.input_mapping {
        let handle = app.world().resource::<AssetServer>().load(mapping.clone());
        app.insert_resource(ProjectInputMapping(handle));
    }
    if cli.project.is_some() && mode != RuntimeMode::Headless {
        app.add_plugins(bevy_settings::SettingsPlugin::new(&format!(
            "bevy9-{}", manifest.project.id
        )));
    }

    if let Some(scene_path) = startup_scene_path {
        load_level_from_file(app.world_mut(), &scene_path)?;
        if let Some(override_mode) = cli.world_mode {
            app.insert_resource(override_mode);
        }
    }

    match mode {
        RuntimeMode::Headless => {}
        RuntimeMode::Window => {
            app.insert_resource(ClearColor(Color::srgb(0.08, 0.10, 0.14)));
            app.add_plugins(BootstrapPreviewPlugin);
        }
        RuntimeMode::Editor => {
            let active_world_mode = app.world().resource::<WorldMode>();
            let initial_camera_mode = match active_world_mode {
                WorldMode::Mode2D => ViewportCameraMode::Mode2D,
                WorldMode::Mode3D | WorldMode::Hybrid => ViewportCameraMode::Mode3D,
            };
            let viewport_title = cli
                .title
                .clone()
                .unwrap_or_else(|| format!("{} — Bevy9 Viewport", manifest.project.name));

            app.insert_resource(ViewportConfig {
                camera_mode: initial_camera_mode,
                width: manifest.window.width,
                height: manifest.window.height,
                window_title: viewport_title,
                ..Default::default()
            });
            app.insert_resource(EditorBridgeConfig {
                brp_port: cli.brp_port,
                ipc_port: cli.ipc_port,
                ..Default::default()
            });
            app.insert_resource(SpritePickingSettings {
                picking_mode: SpritePickingMode::BoundingBox,
                ..Default::default()
            });
            app.add_plugins((
                MeshPickingPlugin,
                EngineRenderPlugin,
                EditorBridgePlugin,
            ));
            app.add_systems(Startup, log_editor_host_boot);
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

fn resolve_startup_scene_path(
    cli_scene: Option<&PathBuf>,
    paths: Option<&ProjectPaths>,
    manifest: &ProjectManifest,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(explicit) = cli_scene {
        let resolved = if explicit.is_relative()
            && let Some(project_paths) = paths
            && project_paths.root_dir.join(explicit).exists()
        {
            project_paths.root_dir.join(explicit)
        } else {
            explicit.clone()
        };
        if !resolved.exists() {
            anyhow::bail!("startup scene file not found: {}", resolved.display());
        }
        return Ok(Some(resolved));
    }

    if let Some(project_paths) = paths {
        let candidate = project_paths.root_dir.join(&manifest.project.startup_scene);
        if candidate.exists() {
            return Ok(Some(candidate));
        }
    }

    Ok(None)
}

fn log_editor_host_boot(
    manifest: Res<ProjectManifest>,
    world_mode: Res<WorldMode>,
    bridge_config: Res<EditorBridgeConfig>,
    ipc_server: Option<Res<EditorIpcServer>>,
) {
    let brp_url = format!("http://{}:{}", bridge_config.host, bridge_config.brp_port);
    let ipc_url = ipc_server
        .as_deref()
        .map(|server| format!("ws://{}", server.bound_addr()))
        .unwrap_or_else(|| "disabled".to_owned());

    tracing::info!(
        project = %manifest.project.name,
        world_mode = %*world_mode,
        brp_url = %brp_url,
        ipc_url = %ipc_url,
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
