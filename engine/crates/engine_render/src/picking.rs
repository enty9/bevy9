//! Editor pointer tracking, virtual offscreen pointer forwarding, and picking ray population.

use bevy_camera::{Camera, ImageRenderTarget, NormalizedRenderTarget, RenderTarget};
use bevy_ecs::{
    entity::Entity,
    message::MessageWriter,
    query::{Or, With},
    resource::Resource,
    schedule::SystemSet,
    system::{Commands, Query, Res, ResMut},
};
use bevy_gizmos::config::GizmoConfigGroup;
use bevy_input::{keyboard::KeyCode, mouse::MouseButton, ButtonInput};
use bevy_math::Vec2;
use bevy_picking::{
    backend::ray::{RayId, RayMap},
    pointer::{Location, PointerAction, PointerButton, PointerId, PointerInput, PointerLocation, PointerMap},
};
use bevy_reflect::{std_traits::ReflectDefault, Reflect};
use bevy_shape::Ray3d;
use bevy_transform::components::GlobalTransform;
use bevy_window::{PrimaryWindow, Window};
use engine_protocol::{PointerButtonCode, ViewportInputEvent, ViewportMode};
use uuid::Uuid;

use crate::{
    camera::{EditorCamera2d, EditorCamera3d},
    viewport::{DetachedViewportWindow, OffscreenRenderState, ViewportConfig, ViewportInputQueue},
};

/// System set running once per frame in `PreUpdate` before [`bevy_picking::PickingSystems::ProcessInput`]
/// to snapshot forwarded `<canvas>` events into [`ViewportFrameInput`].
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ViewportInputCaptureSet;

/// Gizmo configuration group used for editor selection outlines, helper icons, and transform gizmos.
#[derive(Default, Reflect, GizmoConfigGroup)]
#[reflect(Default)]
pub struct EditorOverlayGizmoGroup;

/// Stable [`PointerId`] used by the embedded offscreen `<canvas>` viewport.
pub const OFFSCREEN_POINTER_ID: PointerId =
    PointerId::Custom(Uuid::from_u128(0xBEE9_0001_0000_4000_8000_0000_0000_0001));

/// Snapshot of [`ViewportInputQueue`] events captured once per frame in `PreUpdate`
/// so picking, gizmos, and cameras share the exact same input stream.
#[derive(Resource, Default, Debug, Clone)]
pub struct ViewportFrameInput {
    /// Ordered viewport input and resize events for the current frame.
    pub events: Vec<ViewportInputEvent>,
}

/// Unified pointer + modifier state shared across picking, gizmos, and camera systems
/// in both `Offscreen` and `NativeWindow` modes.
#[derive(Resource, Default, Debug, Clone)]
pub struct EditorPointerState {
    /// Current pointer coordinates in viewport pixel space.
    pub position: Option<Vec2>,
    /// Pointer movement delta accumulated during the current frame.
    pub delta: Vec2,
    /// World-space camera ray through [`Self::position`], if a viewport camera is active.
    pub ray: Option<Ray3d>,
    /// Active editor camera entity casting [`Self::ray`].
    pub active_camera: Option<Entity>,
    /// Active [`PointerId`] driving the viewport (`OFFSCREEN_POINTER_ID` or `PointerId::Mouse`).
    pub pointer_id: Option<PointerId>,
    /// Left mouse button is currently held down.
    pub left_down: bool,
    /// Left mouse button transitioned to pressed during this frame.
    pub left_just_pressed: bool,
    /// Left mouse button transitioned to released during this frame.
    pub left_just_released: bool,
    /// Right mouse button is currently held down.
    pub right_down: bool,
    /// Middle mouse button is currently held down.
    pub middle_down: bool,
    /// Shift modifier is held.
    pub shift_down: bool,
    /// Control (or Command) modifier is held.
    pub ctrl_down: bool,
    /// Alt / Option modifier is held.
    pub alt_down: bool,
    /// Pointer position where the current primary press started.
    pub press_origin: Option<Vec2>,
    /// Maximum cursor distance moved while the primary button has been held.
    pub max_drag_distance: f32,
}

impl EditorPointerState {
    fn begin_frame(&mut self) {
        if self.left_just_released && !self.left_down {
            self.press_origin = None;
            self.max_drag_distance = 0.0;
        }
        self.delta = Vec2::ZERO;
        self.left_just_pressed = false;
        self.left_just_released = false;
        self.ray = None;
        self.active_camera = None;
    }

    fn record_drag_distance(&mut self) {
        if (self.left_down || self.left_just_released)
            && let (Some(origin), Some(pos)) = (self.press_origin, self.position)
        {
            self.max_drag_distance = self.max_drag_distance.max(origin.distance(pos));
        }
    }
}

/// Spawns the persistent virtual picking pointer used for offscreen `<canvas>` input.
pub(crate) fn spawn_offscreen_pointer(mut commands: Commands, pointer_map: Res<PointerMap>) {
    if pointer_map.get_entity(OFFSCREEN_POINTER_ID).is_none() {
        commands.spawn(OFFSCREEN_POINTER_ID);
    }
}

/// Captures queued forwarded `<canvas>` events into [`ViewportFrameInput`] once per frame
/// and updates [`EditorPointerState`] for offscreen mode.
pub(crate) fn capture_viewport_frame_input(
    config: Res<ViewportConfig>,
    mut queue: ResMut<ViewportInputQueue>,
    mut frame_input: ResMut<ViewportFrameInput>,
    mut pointer_state: ResMut<EditorPointerState>,
) {
    frame_input.events = queue.drain_all();
    pointer_state.begin_frame();

    if config.mode != ViewportMode::Offscreen {
        return;
    }

    pointer_state.pointer_id = Some(OFFSCREEN_POINTER_ID);

    for event in &frame_input.events {
        match event {
            ViewportInputEvent::PointerMove {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                pointer_state.position = Some(Vec2::new(*x, *y));
                pointer_state.delta += Vec2::new(*delta_x, *delta_y);
                pointer_state.record_drag_distance();
            }
            ViewportInputEvent::PointerButton {
                button,
                pressed,
                x,
                y,
                modifiers,
            } => {
                pointer_state.position = Some(Vec2::new(*x, *y));
                pointer_state.shift_down = modifiers.shift;
                pointer_state.ctrl_down = modifiers.ctrl || modifiers.meta;
                pointer_state.alt_down = modifiers.alt;

                match button {
                    PointerButtonCode::Left => {
                        if *pressed {
                            if !pointer_state.left_down {
                                pointer_state.left_just_pressed = true;
                                pointer_state.press_origin = Some(Vec2::new(*x, *y));
                                pointer_state.max_drag_distance = 0.0;
                            }
                            pointer_state.left_down = true;
                        } else {
                            pointer_state.record_drag_distance();
                            pointer_state.left_down = false;
                            pointer_state.left_just_released = true;
                        }
                    }
                    PointerButtonCode::Right => pointer_state.right_down = *pressed,
                    PointerButtonCode::Middle => pointer_state.middle_down = *pressed,
                }
            }
            ViewportInputEvent::Key { modifiers, .. }
            | ViewportInputEvent::Wheel { modifiers, .. } => {
                pointer_state.shift_down = modifiers.shift;
                pointer_state.ctrl_down = modifiers.ctrl || modifiers.meta;
                pointer_state.alt_down = modifiers.alt;
            }
            ViewportInputEvent::FocusChanged { focused: false } => {
                pointer_state.left_down = false;
                pointer_state.right_down = false;
                pointer_state.middle_down = false;
                pointer_state.shift_down = false;
                pointer_state.ctrl_down = false;
                pointer_state.alt_down = false;
                pointer_state.press_origin = None;
                pointer_state.max_drag_distance = 0.0;
            }
            ViewportInputEvent::FocusChanged { focused: true }
            | ViewportInputEvent::Resize { .. } => {}
        }
    }
}

/// Converts [`ViewportFrameInput`] pointer events into Bevy [`PointerInput`] messages
/// targeting the active offscreen render target image.
pub(crate) fn forward_offscreen_pointer_inputs(
    config: Res<ViewportConfig>,
    offscreen_state: Res<OffscreenRenderState>,
    frame_input: Res<ViewportFrameInput>,
    mut pointer_inputs: MessageWriter<PointerInput>,
) {
    if config.mode != ViewportMode::Offscreen {
        return;
    }

    let Some(image_handle) = offscreen_state.image_handle.as_ref() else {
        return;
    };

    let target = NormalizedRenderTarget::Image(ImageRenderTarget {
        handle: image_handle.clone(),
        scale_factor: 1.0,
    });

    for event in &frame_input.events {
        match event {
            ViewportInputEvent::PointerMove {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                pointer_inputs.write(PointerInput::new(
                    OFFSCREEN_POINTER_ID,
                    Location {
                        target: target.clone(),
                        position: Vec2::new(*x, *y),
                    },
                    PointerAction::Move {
                        delta: Vec2::new(*delta_x, *delta_y),
                    },
                ));
            }
            ViewportInputEvent::PointerButton {
                button,
                pressed,
                x,
                y,
                ..
            } => {
                let position = Vec2::new(*x, *y);
                let location = Location {
                    target: target.clone(),
                    position,
                };
                // `PointerInput::receive` only updates `PointerLocation` on `Move` actions,
                // so emit a zero-delta move right before press/release to ensure exact hit coords.
                pointer_inputs.write(PointerInput::new(
                    OFFSCREEN_POINTER_ID,
                    location.clone(),
                    PointerAction::Move { delta: Vec2::ZERO },
                ));
                let action = if *pressed {
                    PointerAction::Press(map_pointer_button(*button))
                } else {
                    PointerAction::Release(map_pointer_button(*button))
                };
                pointer_inputs.write(PointerInput::new(OFFSCREEN_POINTER_ID, location, action));
            }
            ViewportInputEvent::FocusChanged { focused: false } => {
                pointer_inputs.write(PointerInput::new(
                    OFFSCREEN_POINTER_ID,
                    Location {
                        target: target.clone(),
                        position: Vec2::ZERO,
                    },
                    PointerAction::Cancel,
                ));
            }
            _ => {}
        }
    }
}

type EditorCamFilter = Or<(With<EditorCamera3d>, With<EditorCamera2d>)>;

/// Populates [`RayMap`] for active editor cameras (fixing Bevy's built-in `Location::is_in_viewport`
/// returning `false` when no `PrimaryWindow` exists) and updates [`EditorPointerState`].
#[expect(
    clippy::too_many_arguments,
    reason = "Reads active editor cameras, picking pointers, and optional native window input"
)]
pub(crate) fn populate_editor_picking_rays(
    config: Res<ViewportConfig>,
    mut pointer_state: ResMut<EditorPointerState>,
    mut ray_map: Option<ResMut<RayMap>>,
    primary_window: Query<Entity, With<PrimaryWindow>>,
    detached_windows: Query<&Window, With<DetachedViewportWindow>>,
    cameras: Query<(Entity, &Camera, &RenderTarget, &GlobalTransform), EditorCamFilter>,
    pointers: Query<(&PointerId, &PointerLocation)>,
    mouse_buttons: Option<Res<ButtonInput<MouseButton>>>,
    key_codes: Option<Res<ButtonInput<KeyCode>>>,
) {
    if config.mode == ViewportMode::NativeWindow {
        pointer_state.pointer_id = Some(PointerId::Mouse);
        if let Some(window) = detached_windows.iter().next() {
            let next_pos = window.cursor_position();
            if let (Some(prev), Some(next)) = (pointer_state.position, next_pos) {
                pointer_state.delta = next - prev;
            }
            pointer_state.position = next_pos;
        }
        if let Some(buttons) = mouse_buttons.as_deref() {
            pointer_state.left_just_pressed = buttons.just_pressed(MouseButton::Left);
            pointer_state.left_just_released = buttons.just_released(MouseButton::Left);
            pointer_state.left_down = buttons.pressed(MouseButton::Left);
            pointer_state.right_down = buttons.pressed(MouseButton::Right);
            pointer_state.middle_down = buttons.pressed(MouseButton::Middle);

            if pointer_state.left_just_pressed {
                pointer_state.press_origin = pointer_state.position;
                pointer_state.max_drag_distance = 0.0;
            }
            pointer_state.record_drag_distance();
        }
        if let Some(keys) = key_codes.as_deref() {
            pointer_state.shift_down =
                keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
            pointer_state.ctrl_down = keys.pressed(KeyCode::ControlLeft)
                || keys.pressed(KeyCode::ControlRight)
                || keys.pressed(KeyCode::SuperLeft)
                || keys.pressed(KeyCode::SuperRight);
            pointer_state.alt_down =
                keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
        }
    }

    let primary_win = primary_window.single().ok();
    let expected_pointer = pointer_state.pointer_id;

    for (camera_entity, camera, render_target, camera_tfm) in &cameras {
        if !camera.is_active {
            continue;
        }
        let Some(normalized_cam_target) = render_target.normalize(primary_win) else {
            continue;
        };

        let mut matched_from_pointer = false;
        if let Some(ray_map_res) = ray_map.as_deref_mut() {
            for (&pointer_id, pointer_loc) in &pointers {
                let Some(loc) = pointer_loc.location() else {
                    continue;
                };
                if loc.target != normalized_cam_target {
                    continue;
                }
                let in_viewport = camera
                    .logical_viewport_rect()
                    .is_some_and(|rect| rect.contains(loc.position));
                if !in_viewport {
                    continue;
                }
                if let Ok(ray) = camera.viewport_to_world(camera_tfm, loc.position) {
                    ray_map_res
                        .map
                        .insert(RayId::new(camera_entity, pointer_id), ray);
                    if expected_pointer == Some(pointer_id) || expected_pointer.is_none() {
                        pointer_state.ray = Some(ray);
                        pointer_state.active_camera = Some(camera_entity);
                        matched_from_pointer = true;
                    }
                }
            }
        }

        if !matched_from_pointer
            && let Some(pos) = pointer_state.position
            && camera
                .logical_viewport_rect()
                .is_some_and(|rect| rect.contains(pos))
            && let Ok(ray) = camera.viewport_to_world(camera_tfm, pos)
        {
            pointer_state.ray = Some(ray);
            pointer_state.active_camera = Some(camera_entity);
        }
    }
}

const fn map_pointer_button(button: PointerButtonCode) -> PointerButton {
    match button {
        PointerButtonCode::Left => PointerButton::Primary,
        PointerButtonCode::Right => PointerButton::Secondary,
        PointerButtonCode::Middle => PointerButton::Middle,
    }
}
