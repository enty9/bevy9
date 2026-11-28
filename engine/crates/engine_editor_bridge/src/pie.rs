//! Play-In-Editor (PIE) session lifecycle controller, camera/viewport handoff, and BRP handlers.

use core::{fmt, time::Duration};
use std::error::Error;

use bevy_app::{FixedMain, FixedUpdate, PostUpdate};
use bevy_camera::{Camera, Projection, RenderTarget};
use bevy_ecs::{
    entity::Entity,
    event::Event,
    query::{Or, With, Without},
    resource::Resource,
    system::{In, Res, ResMut},
    world::World,
};
use bevy_input::{keyboard::KeyCode, ButtonInput};
use bevy_remote::{builtin_methods::parse, error_codes, BrpError, BrpResult, RemotePlugin};
use bevy_time::{Fixed, Time};
use bevy_transform::components::Transform;
use bevy_window::WindowRef;
use engine_audio::{AudioEngine, AudioOptions};
use engine_core::{EntityGuid, EntityGuidMap, WorldMode};
use engine_input::{ForwardedInput, InputContexts, InputDomain};
use engine_physics::{
    PhysicsDebugSelection, PhysicsOptions, PhysicsTimestep, PhysicsWorld2D, PhysicsWorld3D,
};
use engine_protocol::{
    PieState, PieStatePayload, PieStepParams, ViewportCameraMode, ViewportInputEvent, ViewportMode,
    METHOD_PIE_PAUSE, METHOD_PIE_PLAY, METHOD_PIE_STATE, METHOD_PIE_STEP, METHOD_PIE_STOP,
};
use engine_render::{
    camera::{EditorCamera2d, EditorCamera3d, EditorCameraState},
    gizmo::TransformGizmoState,
    viewport::{OffscreenRenderState, ViewportConfig},
};
use engine_scene::{
    capture_pie_snapshot, restore_pie_snapshot, ActiveScene, PieSnapshot, SceneCamera2d,
    SceneCamera3d, SceneCameraActivationPolicy, SceneEntity, SceneError,
};
use engine_scripting::{reset_runtime_state, run_update_scripts_system, ScriptExecutionOptions};
use serde_json::{json, Value};

use crate::{
    history::EditorHistory,
    picking::EditorHoveredEntity,
    selection::{BridgeDirtyFlags, EditorSelection},
};

/// Errors produced by Play-In-Editor lifecycle transitions.
#[derive(Debug)]
pub enum PieError {
    /// Requested an operation (such as `pause`) that requires an active PIE session.
    NotActive,
    /// Requested an invalid fixed-step count (such as `0`).
    InvalidStepCount,
    /// Scene snapshot capture or rollback restoration failed.
    Snapshot(SceneError),
}

impl fmt::Display for PieError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotActive => {
                f.write_str("Play-In-Editor is not active; start PIE before pausing")
            }
            Self::InvalidStepCount => f.write_str("PIE step count must be at least 1"),
            Self::Snapshot(err) => write!(f, "PIE snapshot error: {err}"),
        }
    }
}

impl Error for PieError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Snapshot(err) => Some(err),
            Self::NotActive | Self::InvalidStepCount => None,
        }
    }
}

impl From<SceneError> for PieError {
    fn from(value: SceneError) -> Self {
        Self::Snapshot(value)
    }
}

/// Observer event triggered whenever the Play-In-Editor lifecycle state changes.
#[derive(Event, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PieStateTransition {
    /// Lifecycle state prior to the transition.
    pub previous: PieState,
    /// Lifecycle state after the transition.
    pub current: PieState,
}

#[derive(Debug, Clone)]
struct SavedEditorState {
    snapshot: PieSnapshot,
    editor_camera: Option<EditorCameraState>,
    viewport_camera_mode: Option<ViewportCameraMode>,
    viewport_show_grid: Option<bool>,
    selection: Vec<EntityGuid>,
}

/// ECS resource holding Play-In-Editor session state, pre-PIE snapshot, and telemetry.
#[derive(Resource, Debug, Default)]
pub struct PieSessionState {
    state: PieState,
    fixed_steps: u64,
    frame_count: u64,
    elapsed_secs: f64,
    using_scene_camera: bool,
    stepping: bool,
    /// Set `true` whenever PIE state or active camera mode changed and needs an IPC broadcast.
    pub dirty: bool,
    saved_session: Option<SavedEditorState>,
}

impl PieSessionState {
    /// Returns the current [`PieState`].
    #[must_use]
    pub const fn state(&self) -> PieState {
        self.state
    }

    /// Returns `true` when in normal scene editing mode (`PieState::Editing`).
    #[must_use]
    pub const fn is_editing(&self) -> bool {
        self.state.is_editing()
    }

    /// Returns `true` when PIE is active (`PieState::Playing` or `PieState::Paused`).
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.state.is_active()
    }

    /// Returns `true` when PIE is actively playing (`PieState::Playing`).
    #[must_use]
    pub const fn is_playing(&self) -> bool {
        self.state.is_playing()
    }

    /// Returns `true` when PIE is paused (`PieState::Paused`).
    #[must_use]
    pub const fn is_paused(&self) -> bool {
        self.state.is_paused()
    }

    /// Returns `true` when gameplay fixed ticks should advance (`Playing` or inside a `Step` tick).
    #[must_use]
    pub const fn is_simulating(&self) -> bool {
        self.state.is_playing() || self.stepping
    }

    /// Returns the number of fixed simulation steps executed during the current PIE session.
    #[must_use]
    pub const fn fixed_steps(&self) -> u64 {
        self.fixed_steps
    }

    /// Returns the number of update frames executed during the current PIE session.
    #[must_use]
    pub const fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Returns the accumulated simulated gameplay time in seconds for the current PIE session.
    #[must_use]
    pub const fn elapsed_secs(&self) -> f64 {
        self.elapsed_secs
    }

    /// Returns `true` if a scene camera (`SceneCamera3d`/`SceneCamera2d`) currently owns viewport rendering.
    #[must_use]
    pub const fn using_scene_camera(&self) -> bool {
        self.using_scene_camera
    }

    /// Returns the captured pre-PIE [`PieSnapshot`] while a PIE session is active.
    #[must_use]
    pub fn snapshot(&self) -> Option<&PieSnapshot> {
        self.saved_session.as_ref().map(|saved| &saved.snapshot)
    }

    /// Builds a serializable [`PieStatePayload`] describing the current session status.
    #[must_use]
    pub const fn payload(&self) -> PieStatePayload {
        PieStatePayload {
            state: self.state,
            fixed_steps: self.fixed_steps,
            frame_count: self.frame_count,
            elapsed_secs: self.elapsed_secs,
            using_scene_camera: self.using_scene_camera,
        }
    }

    fn saved_editor_camera(&self) -> Option<&EditorCameraState> {
        self.saved_session
            .as_ref()
            .and_then(|saved| saved.editor_camera.as_ref())
    }
}

/// Run condition returning `true` only when the editor is in [`PieState::Editing`].
#[must_use]
pub fn is_editing(pie: Option<Res<PieSessionState>>) -> bool {
    pie.is_none_or(|session| session.is_editing())
}

/// Returns `true` when `world` has an active PIE session (`Playing` or `Paused`).
#[must_use]
pub fn is_pie_active(world: &World) -> bool {
    world
        .get_resource::<PieSessionState>()
        .is_some_and(PieSessionState::is_active)
}

/// Play-In-Editor lifecycle state-machine operations executed against an ECS [`World`].
#[derive(Debug, Default, Clone, Copy)]
pub struct PieController;

impl PieController {
    /// Returns the current [`PieStatePayload`] for `world`.
    #[must_use]
    pub fn state(world: &World) -> PieStatePayload {
        world
            .get_resource::<PieSessionState>()
            .map(PieSessionState::payload)
            .unwrap_or_default()
    }

    /// Transitions `Edit -> Play` (capturing a full scene snapshot) or resumes `Pause -> Play`.
    ///
    /// Calling `play` while already [`PieState::Playing`] is an idempotent no-op.
    pub fn play(world: &mut World) -> Result<PieStatePayload, PieError> {
        ensure_pie_resource(world);
        let current = world.resource::<PieSessionState>().state;
        match current {
            PieState::Playing => Ok(Self::state(world)),
            PieState::Editing => {
                enter_pie(world, PieState::Playing)?;
                Ok(Self::state(world))
            }
            PieState::Paused => {
                discard_fixed_overstep(world);
                if let Some(mut physics) = world.get_resource_mut::<PhysicsOptions>() {
                    physics.simulate = true;
                }
                if let Some(mut scripts) = world.get_resource_mut::<ScriptExecutionOptions>() {
                    scripts.enabled = true;
                }
                if let Some(mut audio) = world.get_resource_mut::<AudioOptions>() {
                    audio.gameplay = true;
                }
                if let Some(mut contexts) = world.get_resource_mut::<InputContexts>() {
                    contexts.domain = InputDomain::Game;
                }
                if let Some(mut policy) = world.get_resource_mut::<SceneCameraActivationPolicy>() {
                    policy.allow_scene_cameras = true;
                }
                {
                    let mut pie = world.resource_mut::<PieSessionState>();
                    pie.state = PieState::Playing;
                    pie.dirty = true;
                }
                sync_pie_cameras_world(world);
                world.trigger(PieStateTransition {
                    previous: PieState::Paused,
                    current: PieState::Playing,
                });
                Ok(Self::state(world))
            }
        }
    }

    /// Transitions `Play -> Pause` while preserving the live PIE world and physics state.
    ///
    /// Calling `pause` while already [`PieState::Paused`] is an idempotent no-op.
    /// Calling `pause` while [`PieState::Editing`] returns [`PieError::NotActive`].
    pub fn pause(world: &mut World) -> Result<PieStatePayload, PieError> {
        ensure_pie_resource(world);
        let current = world.resource::<PieSessionState>().state;
        match current {
            PieState::Editing => Err(PieError::NotActive),
            PieState::Paused => Ok(Self::state(world)),
            PieState::Playing => {
                if let Some(mut physics) = world.get_resource_mut::<PhysicsOptions>() {
                    physics.simulate = false;
                }
                if let Some(mut scripts) = world.get_resource_mut::<ScriptExecutionOptions>() {
                    scripts.enabled = false;
                }
                {
                    let mut pie = world.resource_mut::<PieSessionState>();
                    pie.state = PieState::Paused;
                    pie.dirty = true;
                }
                world.trigger(PieStateTransition {
                    previous: PieState::Playing,
                    current: PieState::Paused,
                });
                Ok(Self::state(world))
            }
        }
    }

    /// Advances the PIE simulation by `steps` deterministic fixed-update ticks and returns to
    /// [`PieState::Paused`].
    ///
    /// If called while [`PieState::Editing`], automatically enters PIE (capturing the pre-PIE
    /// snapshot) in [`PieState::Paused`] before executing `steps`.
    pub fn step(world: &mut World, steps: u32) -> Result<PieStatePayload, PieError> {
        if steps == 0 {
            return Err(PieError::InvalidStepCount);
        }
        ensure_pie_resource(world);

        let current = world.resource::<PieSessionState>().state;
        match current {
            PieState::Editing => {
                enter_pie(world, PieState::Paused)?;
            }
            PieState::Playing => {
                if let Some(mut physics) = world.get_resource_mut::<PhysicsOptions>() {
                    physics.simulate = false;
                }
                if let Some(mut scripts) = world.get_resource_mut::<ScriptExecutionOptions>() {
                    scripts.enabled = false;
                }
                {
                    let mut pie = world.resource_mut::<PieSessionState>();
                    pie.state = PieState::Paused;
                    pie.dirty = true;
                }
                world.trigger(PieStateTransition {
                    previous: PieState::Playing,
                    current: PieState::Paused,
                });
            }
            PieState::Paused => {}
        }

        // Honor any recently modified `PhysicsTimestep` before executing fixed ticks.
        if let Some(hz) = world
            .get_resource::<PhysicsTimestep>()
            .map(|ts| ts.hz)
            .filter(|hz| (1.0..=240.0).contains(hz))
            && let Some(mut fixed_time) = world.get_resource_mut::<Time<Fixed>>()
        {
            fixed_time.set_timestep_hz(hz);
        }

        let timestep = world
            .get_resource::<Time<Fixed>>()
            .map_or(Duration::from_secs_f64(1.0 / 60.0), Time::timestep);
        let prev_generic_time = world.get_resource::<Time>().copied();

        if let Some(mut physics) = world.get_resource_mut::<PhysicsOptions>() {
            physics.simulate = true;
        }
        if let Some(mut scripts) = world.get_resource_mut::<ScriptExecutionOptions>() {
            scripts.enabled = true;
        }
        world.resource_mut::<PieSessionState>().stepping = true;

        for _ in 0..steps {
            let steps_before = world.resource::<PieSessionState>().fixed_steps;
            if let Some(mut fixed_time) = world.get_resource_mut::<Time<Fixed>>() {
                let leftover = fixed_time.overstep();
                fixed_time.discard_overstep(leftover);
                fixed_time.advance_by(timestep);
                let generic_fixed = fixed_time.as_generic();
                if let Some(mut generic) = world.get_resource_mut::<Time>() {
                    *generic = generic_fixed;
                }
            }

            if world.try_run_schedule(FixedMain).is_err() {
                let _ = world.try_run_schedule(FixedUpdate);
            }
            run_update_scripts_system(world);

            let mut pie = world.resource_mut::<PieSessionState>();
            if pie.fixed_steps == steps_before {
                pie.fixed_steps += 1;
                pie.elapsed_secs += timestep.as_secs_f64();
            }
        }

        if let Some(prev_time) = prev_generic_time
            && let Some(mut generic) = world.get_resource_mut::<Time>()
        {
            *generic = prev_time;
        }

        // Run PostUpdate with `overstep == timestep` (interpolation alpha = 1.0) so physics
        // `interpolate` writes the exact post-step pose to `Transform` and `GlobalTransform`.
        if let Some(mut fixed_time) = world.get_resource_mut::<Time<Fixed>>() {
            let leftover = fixed_time.overstep();
            fixed_time.discard_overstep(leftover);
            fixed_time.accumulate_overstep(timestep);
        }
        let _ = world.try_run_schedule(PostUpdate);
        discard_fixed_overstep(world);

        if let Some(mut physics) = world.get_resource_mut::<PhysicsOptions>() {
            physics.simulate = false;
        }
        if let Some(mut scripts) = world.get_resource_mut::<ScriptExecutionOptions>() {
            scripts.enabled = false;
        }
        {
            let mut pie = world.resource_mut::<PieSessionState>();
            pie.stepping = false;
            pie.state = PieState::Paused;
            pie.frame_count += 1;
            pie.dirty = true;
        }

        sync_pie_cameras_world(world);

        if let Some(mut dirty) = world.get_resource_mut::<BridgeDirtyFlags>() {
            dirty.hierarchy_dirty = true;
        }
        let selected = world
            .get_resource::<EditorSelection>()
            .map(|sel| sel.guids().to_vec())
            .unwrap_or_default();
        if !selected.is_empty()
            && let Some(mut history) = world.get_resource_mut::<EditorHistory>()
        {
            history.queue_inspector_refresh(&selected);
        }

        Ok(Self::state(world))
    }

    /// Stops PIE (`Playing` or `Paused`), shuts down simulation/gameplay audio, restores the
    /// exact pre-PIE scene snapshot and editor state, and returns to [`PieState::Editing`].
    ///
    /// Calling `stop` while already [`PieState::Editing`] is an idempotent no-op.
    pub fn stop(world: &mut World) -> Result<PieStatePayload, PieError> {
        ensure_pie_resource(world);
        let previous = world.resource::<PieSessionState>().state;
        if previous.is_editing() {
            return Ok(Self::state(world));
        }

        if let Some(mut physics) = world.get_resource_mut::<PhysicsOptions>() {
            physics.simulate = false;
        }
        if let Some(mut scripts) = world.get_resource_mut::<ScriptExecutionOptions>() {
            scripts.enabled = false;
        }
        reset_runtime_state(world);
        if let Some(mut audio_opts) = world.get_resource_mut::<AudioOptions>() {
            audio_opts.gameplay = false;
        }
        if let Some(mut audio_engine) = world.get_resource_mut::<AudioEngine>() {
            audio_engine.stop_preview();
        }
        if let Some(mut contexts) = world.get_resource_mut::<InputContexts>() {
            contexts.domain = InputDomain::Editor;
        }
        if let Some(mut policy) = world.get_resource_mut::<SceneCameraActivationPolicy>() {
            policy.allow_scene_cameras = false;
        }
        if let Some(mut forwarded) = world.get_resource_mut::<ForwardedInput>() {
            *forwarded = ForwardedInput::default();
        }
        discard_fixed_overstep(world);

        let saved_snapshot = world
            .resource::<PieSessionState>()
            .saved_session
            .as_ref()
            .map(|saved| saved.snapshot.clone());
        if let Some(snapshot) = saved_snapshot {
            restore_pie_snapshot(world, &snapshot)?;
        } else {
            if let Some(mut physics_2d) = world.get_resource_mut::<PhysicsWorld2D>() {
                *physics_2d = PhysicsWorld2D::default();
            }
            if let Some(mut physics_3d) = world.get_resource_mut::<PhysicsWorld3D>() {
                *physics_3d = PhysicsWorld3D::default();
            }
        }
        if let Some(mut scripts) = world.get_resource_mut::<ScriptExecutionOptions>() {
            scripts.enabled = false;
        }
        reset_runtime_state(world);

        let saved_session = world.resource_mut::<PieSessionState>().saved_session.take();
        let mut restored_selection = Vec::new();
        if let Some(saved) = saved_session {
            if let Some(mut cfg) = world.get_resource_mut::<ViewportConfig>() {
                if let Some(cam_mode) = saved.viewport_camera_mode {
                    cfg.camera_mode = cam_mode;
                }
                if let Some(show_grid) = saved.viewport_show_grid {
                    cfg.show_grid = show_grid;
                }
            }
            if let Some(mut saved_cam) = saved.editor_camera {
                let cam_mode = saved.viewport_camera_mode.unwrap_or_default();
                clear_editor_camera_held_inputs(&mut saved_cam, cam_mode);
                let desired_3d = saved_cam.compute_3d_transform();
                let desired_2d = saved_cam.compute_2d_transform();
                let zoom_2d = saved_cam.zoom_2d;
                if let Some(mut cam_state) = world.get_resource_mut::<EditorCameraState>() {
                    *cam_state = saved_cam;
                }
                apply_editor_camera_transforms(world, desired_3d, desired_2d, zoom_2d);
            }

            restored_selection = if let Some(guid_map) = world.get_resource::<EntityGuidMap>() {
                saved
                    .selection
                    .into_iter()
                    .filter(|guid| guid_map.contains_guid(*guid))
                    .collect()
            } else {
                saved.selection
            };
            if let Some(mut selection) = world.get_resource_mut::<EditorSelection>() {
                selection.set(restored_selection.clone());
            }
        }

        if let Some(mut hovered) = world.get_resource_mut::<EditorHoveredEntity>() {
            hovered.guid = None;
        }
        reset_gizmo_transient_state(world);

        {
            let mut pie = world.resource_mut::<PieSessionState>();
            pie.state = PieState::Editing;
            pie.fixed_steps = 0;
            pie.frame_count = 0;
            pie.elapsed_secs = 0.0;
            pie.using_scene_camera = false;
            pie.stepping = false;
            pie.dirty = true;
        }

        sync_pie_cameras_world(world);

        if let Some(mut dirty) = world.get_resource_mut::<BridgeDirtyFlags>() {
            dirty.hierarchy_dirty = true;
            dirty.viewport_dirty = true;
            dirty.selection_dirty = true;
        }
        if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
            history.dirty = true;
            history.queue_inspector_refresh(&restored_selection);
        }

        world.trigger(PieStateTransition {
            previous,
            current: PieState::Editing,
        });

        Ok(Self::state(world))
    }
}

fn ensure_pie_resource(world: &mut World) {
    if !world.contains_resource::<PieSessionState>() {
        world.init_resource::<PieSessionState>();
    }
}

fn discard_fixed_overstep(world: &mut World) {
    if let Some(mut fixed_time) = world.get_resource_mut::<Time<Fixed>>() {
        let leftover = fixed_time.overstep();
        fixed_time.discard_overstep(leftover);
    }
}

fn clear_editor_camera_held_inputs(
    state: &mut EditorCameraState,
    camera_mode: ViewportCameraMode,
) {
    state.apply_forwarded_events(
        &[ViewportInputEvent::FocusChanged { focused: false }],
        camera_mode,
        0.0,
    );
}

fn apply_editor_camera_transforms(
    world: &mut World,
    desired_3d: Transform,
    desired_2d: Transform,
    zoom_2d: f32,
) {
    let mut cam3d = world.query_filtered::<&mut Transform, (With<EditorCamera3d>, Without<EditorCamera2d>)>();
    for mut tfm in cam3d.iter_mut(world) {
        *tfm = desired_3d;
    }
    let mut cam2d = world.query_filtered::<(&mut Transform, &mut Projection), (With<EditorCamera2d>, Without<EditorCamera3d>)>();
    for (mut tfm, mut proj) in cam2d.iter_mut(world) {
        *tfm = desired_2d;
        if let Projection::Orthographic(ortho) = &mut *proj {
            ortho.scale = zoom_2d;
        }
    }
}

fn reset_gizmo_transient_state(world: &mut World) {
    if let Some(mut gizmo) = world.get_resource_mut::<TransformGizmoState>() {
        let mut fresh = TransformGizmoState::default();
        fresh.mode = gizmo.mode;
        fresh.space = gizmo.space;
        fresh.snap = gizmo.snap;
        *gizmo = fresh;
    }
}

fn enter_pie(world: &mut World, target_state: PieState) -> Result<(), PieError> {
    // Flush any completed gizmo transforms before taking the pre-PIE snapshot.
    crate::history::capture_gizmo_commits_system(world);
    reset_gizmo_transient_state(world);

    let snapshot = capture_pie_snapshot(world)?;

    let viewport_camera_mode = world
        .get_resource::<ViewportConfig>()
        .map(|cfg| cfg.camera_mode);
    let viewport_show_grid = world
        .get_resource::<ViewportConfig>()
        .map(|cfg| cfg.show_grid);
    let editor_camera = world.get_resource::<EditorCameraState>().cloned().map(|mut cam| {
        clear_editor_camera_held_inputs(&mut cam, viewport_camera_mode.unwrap_or_default());
        cam
    });
    let selection = world
        .get_resource::<EditorSelection>()
        .map(|sel| sel.guids().to_vec())
        .unwrap_or_default();

    if let Some(mut audio_engine) = world.get_resource_mut::<AudioEngine>() {
        audio_engine.stop_preview();
    }
    if let Some(mut physics_2d) = world.get_resource_mut::<PhysicsWorld2D>() {
        *physics_2d = PhysicsWorld2D::default();
    }
    if let Some(mut physics_3d) = world.get_resource_mut::<PhysicsWorld3D>() {
        *physics_3d = PhysicsWorld3D::default();
    }
    reset_runtime_state(world);
    discard_fixed_overstep(world);

    if let Some(mut physics) = world.get_resource_mut::<PhysicsOptions>() {
        physics.simulate = target_state.is_playing();
    }
    if let Some(mut scripts) = world.get_resource_mut::<ScriptExecutionOptions>() {
        scripts.enabled = target_state.is_playing();
    }
    if let Some(mut audio_opts) = world.get_resource_mut::<AudioOptions>() {
        audio_opts.gameplay = true;
    }
    if let Some(mut contexts) = world.get_resource_mut::<InputContexts>() {
        contexts.domain = InputDomain::Game;
    }
    if let Some(mut policy) = world.get_resource_mut::<SceneCameraActivationPolicy>() {
        policy.allow_scene_cameras = true;
    }
    if let Some(mut cfg) = world.get_resource_mut::<ViewportConfig>() {
        cfg.show_grid = false;
    }
    if let Some(mut hovered) = world.get_resource_mut::<EditorHoveredEntity>() {
        hovered.guid = None;
    }
    if let Some(mut debug_sel) = world.get_resource_mut::<PhysicsDebugSelection>() {
        debug_sel.0.clear();
    }
    if let Some(mut forwarded) = world.get_resource_mut::<ForwardedInput>() {
        *forwarded = ForwardedInput::default();
    }
    if let Some(mut cam_state) = world.get_resource_mut::<EditorCameraState>() {
        clear_editor_camera_held_inputs(&mut cam_state, viewport_camera_mode.unwrap_or_default());
    }

    {
        let mut pie = world.resource_mut::<PieSessionState>();
        pie.state = target_state;
        pie.fixed_steps = 0;
        pie.frame_count = 0;
        pie.elapsed_secs = 0.0;
        pie.stepping = false;
        pie.dirty = true;
        pie.saved_session = Some(SavedEditorState {
            snapshot,
            editor_camera,
            viewport_camera_mode,
            viewport_show_grid,
            selection,
        });
    }

    sync_pie_cameras_world(world);

    if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
        history.dirty = true;
    }

    world.trigger(PieStateTransition {
        previous: PieState::Editing,
        current: target_state,
    });

    Ok(())
}

fn render_targets_match(existing: &RenderTarget, desired: &RenderTarget) -> bool {
    match (existing, desired) {
        (RenderTarget::Image(a), RenderTarget::Image(b)) => a.handle == b.handle,
        (RenderTarget::Window(WindowRef::Primary), RenderTarget::Window(WindowRef::Primary)) => {
            true
        }
        (
            RenderTarget::Window(WindowRef::Entity(a)),
            RenderTarget::Window(WindowRef::Entity(b)),
        ) => a == b,
        _ => false,
    }
}

fn active_viewport_render_target(world: &World) -> Option<RenderTarget> {
    let config = world.get_resource::<ViewportConfig>()?;
    let render_state = world.get_resource::<OffscreenRenderState>()?;
    match config.mode {
        ViewportMode::Offscreen => render_state
            .image_handle
            .as_ref()
            .map(|handle| RenderTarget::from(handle.clone())),
        ViewportMode::NativeWindow => render_state
            .detached_window
            .map(|win| RenderTarget::Window(WindowRef::Entity(win))),
    }
}

fn preferred_camera_mode(world: &World) -> ViewportCameraMode {
    let world_mode = world
        .get_resource::<ActiveScene>()
        .map(|scene| scene.world_mode)
        .or_else(|| world.get_resource::<WorldMode>().copied());
    match world_mode {
        Some(WorldMode::Mode2D) => ViewportCameraMode::Mode2D,
        Some(WorldMode::Mode3D) => ViewportCameraMode::Mode3D,
        Some(WorldMode::Hybrid) | None => world
            .get_resource::<ViewportConfig>()
            .map_or(ViewportCameraMode::Mode3D, |cfg| cfg.camera_mode),
    }
}

/// Synchronizes scene camera activation, viewport `RenderTarget` binding, and editor camera
/// fallback immediately against `world`.
pub(crate) fn sync_pie_cameras_world(world: &mut World) {
    let pie_active = is_pie_active(world);
    let pie_playing = world
        .get_resource::<PieSessionState>()
        .is_some_and(PieSessionState::is_playing);
    let editor_cam_mode = world
        .get_resource::<ViewportConfig>()
        .map_or(ViewportCameraMode::Mode3D, |cfg| cfg.camera_mode);

    if !pie_active {
        let mut scene_cams = world.query_filtered::<
            &mut Camera,
            (With<SceneEntity>, Or<(With<SceneCamera3d>, With<SceneCamera2d>)>),
        >();
        for mut cam in scene_cams.iter_mut(world) {
            if cam.is_active {
                cam.is_active = false;
            }
        }

        let want_3d = editor_cam_mode == ViewportCameraMode::Mode3D;
        let mut ed3 = world
            .query_filtered::<&mut Camera, (With<EditorCamera3d>, Without<EditorCamera2d>)>();
        for mut cam in ed3.iter_mut(world) {
            if cam.is_active != want_3d {
                cam.is_active = want_3d;
            }
        }
        let mut ed2 = world
            .query_filtered::<&mut Camera, (With<EditorCamera2d>, Without<EditorCamera3d>)>();
        for mut cam in ed2.iter_mut(world) {
            if cam.is_active == want_3d {
                cam.is_active = !want_3d;
            }
        }

        if let Some(mut pie) = world.get_resource_mut::<PieSessionState>()
            && pie.using_scene_camera
        {
            pie.using_scene_camera = false;
            pie.dirty = true;
        }
        return;
    }

    // While actively playing, keep the editor camera frozen at its pre-PIE pose so gameplay
    // WASD/mouse input never drifts the editor camera (or jittery fallback view).
    if pie_playing
        && let Some(saved_cam) = world
            .get_resource::<PieSessionState>()
            .and_then(PieSessionState::saved_editor_camera)
            .cloned()
    {
        let desired_3d = saved_cam.compute_3d_transform();
        let desired_2d = saved_cam.compute_2d_transform();
        let zoom_2d = saved_cam.zoom_2d;
        if let Some(mut cam_state) = world.get_resource_mut::<EditorCameraState>() {
            *cam_state = saved_cam;
        }
        apply_editor_camera_transforms(world, desired_3d, desired_2d, zoom_2d);
    }

    if let Some(mut debug_sel) = world.get_resource_mut::<PhysicsDebugSelection>()
        && !debug_sel.0.is_empty()
    {
        debug_sel.0.clear();
    }

    let pref_mode = preferred_camera_mode(world);
    let mut candidates: Vec<(bool, Option<EntityGuid>, Entity)> = Vec::new();
    {
        let mut q = world.query_filtered::<
            (
                Entity,
                Option<&EntityGuid>,
                Option<&SceneCamera3d>,
                Option<&SceneCamera2d>,
            ),
            (With<SceneEntity>, Or<(With<SceneCamera3d>, With<SceneCamera2d>)>),
        >();
        for (entity, guid, cam3, cam2) in q.iter(world) {
            let active_3d = cam3.is_some_and(|c| c.active);
            let active_2d = cam2.is_some_and(|c| c.active);
            if !active_3d && !active_2d {
                continue;
            }
            let matches_pref = match pref_mode {
                ViewportCameraMode::Mode3D => active_3d,
                ViewportCameraMode::Mode2D => active_2d,
            };
            candidates.push((!matches_pref, guid.copied(), entity));
        }
    }
    candidates.sort_unstable();
    let winner = candidates.first().map(|(_, _, entity)| *entity);
    let viewport_target = active_viewport_render_target(world);

    let mut scene_cam_entities: Vec<Entity> = Vec::new();
    {
        let mut q = world.query_filtered::<
            Entity,
            (With<SceneEntity>, Or<(With<SceneCamera3d>, With<SceneCamera2d>)>),
        >();
        for entity in q.iter(world) {
            scene_cam_entities.push(entity);
        }
    }

    for entity in scene_cam_entities {
        let is_winner = winner == Some(entity);
        if let Some(mut cam) = world.get_mut::<Camera>(entity)
            && cam.is_active != is_winner
        {
            cam.is_active = is_winner;
        }
        if is_winner && let Some(ref target) = viewport_target {
            let needs_update = world
                .get::<RenderTarget>(entity)
                .is_none_or(|existing| !render_targets_match(existing, target));
            if needs_update {
                world.entity_mut(entity).insert(target.clone());
                if let Some(mut projection) = world.get_mut::<Projection>(entity) {
                    bevy_ecs::change_detection::DetectChangesMut::set_changed(&mut projection);
                }
            }
        }
    }

    let using_scene_camera = winner.is_some();
    if using_scene_camera {
        let mut ed_cams = world
            .query_filtered::<&mut Camera, Or<(With<EditorCamera3d>, With<EditorCamera2d>)>>();
        for mut cam in ed_cams.iter_mut(world) {
            if cam.is_active {
                cam.is_active = false;
            }
        }
    } else {
        // Clean fallback to editor camera when no active scene camera exists.
        let want_3d = editor_cam_mode == ViewportCameraMode::Mode3D;
        let mut ed3 = world
            .query_filtered::<&mut Camera, (With<EditorCamera3d>, Without<EditorCamera2d>)>();
        for mut cam in ed3.iter_mut(world) {
            if cam.is_active != want_3d {
                cam.is_active = want_3d;
            }
        }
        let mut ed2 = world
            .query_filtered::<&mut Camera, (With<EditorCamera2d>, Without<EditorCamera3d>)>();
        for mut cam in ed2.iter_mut(world) {
            if cam.is_active == want_3d {
                cam.is_active = !want_3d;
            }
        }
    }

    if let Some(mut pie) = world.get_resource_mut::<PieSessionState>()
        && pie.using_scene_camera != using_scene_camera
    {
        pie.using_scene_camera = using_scene_camera;
        pie.dirty = true;
    }
}

pub(crate) fn track_pie_fixed_step(
    fixed_time: Option<Res<Time<Fixed>>>,
    physics_timestep: Option<Res<PhysicsTimestep>>,
    mut pie: ResMut<PieSessionState>,
) {
    if !pie.is_simulating() {
        return;
    }
    let dt = fixed_time.map_or_else(
        || {
            let hz = physics_timestep
                .map(|ts| ts.hz)
                .filter(|hz| (1.0..=240.0).contains(hz))
                .unwrap_or(60.0);
            1.0 / hz
        },
        |t| t.timestep().as_secs_f64(),
    );
    pie.fixed_steps += 1;
    pie.elapsed_secs += dt;
}

pub(crate) fn track_pie_update_frame(mut pie: ResMut<PieSessionState>) {
    if pie.is_playing() {
        pie.frame_count += 1;
    }
}

pub(crate) fn handle_native_pie_shortcuts_system(world: &mut World) {
    if !is_pie_active(world) {
        return;
    }
    let is_native_window = world
        .get_resource::<ViewportConfig>()
        .is_some_and(|cfg| cfg.mode == ViewportMode::NativeWindow);
    if !is_native_window {
        return;
    }
    let escape_pressed = world
        .get_resource::<ButtonInput<KeyCode>>()
        .is_some_and(|keys| keys.just_pressed(KeyCode::Escape));
    if !escape_pressed {
        return;
    }
    if let Err(err) = PieController::stop(world) {
        tracing::warn!(error = %err, "native Escape PIE stop failed");
    }
}

pub(crate) fn sync_pie_cameras_and_viewport(world: &mut World) {
    sync_pie_cameras_world(world);
}

pub(crate) fn register(plugin: RemotePlugin) -> RemotePlugin {
    plugin
        .with_method_main(METHOD_PIE_PLAY, handle_pie_play)
        .with_method_main(METHOD_PIE_PAUSE, handle_pie_pause)
        .with_method_main(METHOD_PIE_STEP, handle_pie_step)
        .with_method_main(METHOD_PIE_STOP, handle_pie_stop)
        .with_method_main(METHOD_PIE_STATE, handle_pie_state)
}

fn pie_error_to_brp(err: PieError, state: PieState) -> BrpError {
    match err {
        PieError::NotActive => BrpError {
            code: error_codes::INVALID_REQUEST,
            message: err.to_string(),
            data: Some(json!({ "pie_state": state })),
        },
        PieError::InvalidStepCount => BrpError {
            code: error_codes::INVALID_PARAMS,
            message: err.to_string(),
            data: None,
        },
        PieError::Snapshot(scene_err) => BrpError::internal(scene_err),
    }
}

fn serialize_pie_payload(payload: PieStatePayload) -> BrpResult {
    serde_json::to_value(payload).map_err(BrpError::internal)
}

fn handle_pie_play(In(_params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let state = PieController::state(world).state;
    let payload = PieController::play(world).map_err(|err| pie_error_to_brp(err, state))?;
    serialize_pie_payload(payload)
}

fn handle_pie_pause(In(_params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let state = PieController::state(world).state;
    let payload = PieController::pause(world).map_err(|err| pie_error_to_brp(err, state))?;
    serialize_pie_payload(payload)
}

fn handle_pie_step(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: PieStepParams = match params {
        None | Some(Value::Null) => PieStepParams::default(),
        Some(value) => parse(value)?,
    };
    let state = PieController::state(world).state;
    let payload =
        PieController::step(world, params.steps).map_err(|err| pie_error_to_brp(err, state))?;
    serialize_pie_payload(payload)
}

fn handle_pie_stop(In(_params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let state = PieController::state(world).state;
    let payload = PieController::stop(world).map_err(|err| pie_error_to_brp(err, state))?;
    serialize_pie_payload(payload)
}

fn handle_pie_state(In(_params): In<Option<Value>>, world: &mut World) -> BrpResult {
    serialize_pie_payload(PieController::state(world))
}

/// Returns a BRP error when a destructive authoring operation is attempted while PIE is active.
pub(crate) fn reject_during_pie(world: &World, action: &str) -> Result<(), BrpError> {
    let payload = PieController::state(world);
    if payload.state.is_active() {
        return Err(BrpError {
            code: error_codes::INVALID_REQUEST,
            message: format!(
                "Cannot {action} while Play-In-Editor is active; stop PIE first"
            ),
            data: Some(json!({ "pie_state": payload.state })),
        });
    }
    Ok(())
}
