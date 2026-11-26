//! 3D orbit/fly and 2D pan/zoom editor cameras driven by native and forwarded input.

use std::collections::HashSet;

use bevy_camera::{
    Camera, Camera2d, Camera3d, OrthographicProjection, PerspectiveProjection, Projection,
};
use bevy_ecs::{
    component::Component,
    message::MessageReader,
    query::{With, Without},
    resource::Resource,
    system::{Commands, Query, Res, ResMut},
};
use bevy_input::{
    keyboard::KeyCode,
    mouse::{MouseButton, MouseMotion, MouseWheel},
    ButtonInput,
};
use bevy_math::{Quat, Vec2, Vec3};
use bevy_time::Time;
use bevy_transform::components::Transform;
use engine_protocol::{PointerButtonCode, ViewportCameraMode, ViewportInputEvent, ViewportMode};

use crate::{picking::ViewportFrameInput, viewport::ViewportConfig};

/// Marker component for the editor's 3D perspective camera.
#[derive(Component, Debug, Default, Clone, Copy)]
pub struct EditorCamera3d;

/// Marker component for the editor's 2D orthographic camera.
#[derive(Component, Debug, Default, Clone, Copy)]
pub struct EditorCamera2d;

/// Persistent state for both the 3D and 2D editor cameras.
///
/// Storing both states independently allows switching between 2D and 3D modes
/// without resetting the other camera's position or zoom.
#[derive(Resource, Debug, Clone)]
pub struct EditorCameraState {
    /// 3D orbit pivot target in world space.
    pub focus: Vec3,
    /// 3D yaw angle in radians.
    pub yaw: f32,
    /// 3D pitch angle in radians (clamped inside `[-1.5, 1.5]`).
    pub pitch: f32,
    /// Orbit distance from [`Self::focus`].
    pub distance: f32,
    /// WASD/QE fly speed in world units per second.
    pub fly_speed: f32,
    /// 2D camera center in world XY coordinates.
    pub center_2d: Vec2,
    /// 2D orthographic zoom scale (smaller = zoomed in closer).
    pub zoom_2d: f32,
    left_down: bool,
    right_down: bool,
    middle_down: bool,
    alt_down: bool,
    held_keys: HashSet<String>,
}

impl Default for EditorCameraState {
    fn default() -> Self {
        Self {
            focus: Vec3::new(0.0, 0.5, 0.0),
            yaw: 0.62,
            pitch: -0.42,
            distance: 7.5,
            fly_speed: 6.0,
            center_2d: Vec2::ZERO,
            zoom_2d: 1.0,
            left_down: false,
            right_down: false,
            middle_down: false,
            alt_down: false,
            held_keys: HashSet::new(),
        }
    }
}

impl EditorCameraState {
    /// Returns `true` when the right mouse button is held down for camera look/fly navigation.
    #[must_use]
    pub const fn is_right_down(&self) -> bool {
        self.right_down
    }

    /// Computes the world [`Transform`] for the 3D editor camera.
    #[must_use]
    pub fn compute_3d_transform(&self) -> Transform {
        let orientation = Quat::from_euler(bevy_math::EulerRot::YXZ, self.yaw, self.pitch, 0.0);
        let offset = orientation * Vec3::new(0.0, 0.0, self.distance.max(0.2));
        Transform::from_translation(self.focus + offset).looking_at(self.focus, Vec3::Y)
    }

    /// Computes the world [`Transform`] for the 2D editor camera.
    #[must_use]
    pub fn compute_2d_transform(&self) -> Transform {
        Transform::from_xyz(self.center_2d.x, self.center_2d.y, 0.0)
    }

    /// Applies forwarded [`ViewportInputEvent`] records from the embedded UI `<canvas>`.
    pub fn apply_forwarded_events(
        &mut self,
        events: &[ViewportInputEvent],
        camera_mode: ViewportCameraMode,
        dt: f32,
    ) {
        for event in events {
            match event {
                ViewportInputEvent::PointerButton {
                    button,
                    pressed,
                    modifiers,
                    ..
                } => {
                    self.alt_down = modifiers.alt;
                    match button {
                        PointerButtonCode::Left => self.left_down = *pressed,
                        PointerButtonCode::Right => self.right_down = *pressed,
                        PointerButtonCode::Middle => self.middle_down = *pressed,
                    }
                }
                ViewportInputEvent::PointerMove {
                    delta_x, delta_y, ..
                } => {
                    self.apply_pointer_delta(*delta_x, *delta_y, camera_mode);
                }
                ViewportInputEvent::Wheel { delta_y, .. } => {
                    self.apply_wheel_delta(*delta_y, camera_mode);
                }
                ViewportInputEvent::Key {
                    code,
                    pressed,
                    modifiers,
                } => {
                    self.alt_down = modifiers.alt;
                    if *pressed {
                        self.held_keys.insert(code.clone());
                    } else {
                        self.held_keys.remove(code);
                    }
                }
                ViewportInputEvent::FocusChanged { focused } => {
                    if !focused {
                        self.left_down = false;
                        self.right_down = false;
                        self.middle_down = false;
                        self.alt_down = false;
                        self.held_keys.clear();
                    }
                }
                ViewportInputEvent::Resize { .. } => {}
            }
        }

        if camera_mode == ViewportCameraMode::Mode3D && self.right_down {
            let mut move_dir = Vec3::ZERO;
            if self.held_keys.contains("KeyW") {
                move_dir.z -= 1.0;
            }
            if self.held_keys.contains("KeyS") {
                move_dir.z += 1.0;
            }
            if self.held_keys.contains("KeyA") {
                move_dir.x -= 1.0;
            }
            if self.held_keys.contains("KeyD") {
                move_dir.x += 1.0;
            }
            if self.held_keys.contains("KeyE") {
                move_dir.y += 1.0;
            }
            if self.held_keys.contains("KeyQ") {
                move_dir.y -= 1.0;
            }

            if move_dir != Vec3::ZERO {
                let speed_mul = if self.held_keys.contains("ShiftLeft")
                    || self.held_keys.contains("ShiftRight")
                {
                    2.5
                } else {
                    1.0
                };
                let orientation =
                    Quat::from_euler(bevy_math::EulerRot::YXZ, self.yaw, self.pitch, 0.0);
                let world_step =
                    orientation * move_dir.normalize() * (self.fly_speed * speed_mul * dt);
                self.focus += world_step;
            }
        }
    }

    fn apply_pointer_delta(&mut self, delta_x: f32, delta_y: f32, camera_mode: ViewportCameraMode) {
        match camera_mode {
            ViewportCameraMode::Mode3D => {
                if self.right_down {
                    self.yaw -= delta_x * 0.005;
                    self.pitch = (self.pitch - delta_y * 0.005).clamp(-1.5, 1.5);
                } else if self.middle_down || (self.alt_down && self.left_down) {
                    let orientation =
                        Quat::from_euler(bevy_math::EulerRot::YXZ, self.yaw, self.pitch, 0.0);
                    let right = orientation * Vec3::X;
                    let up = orientation * Vec3::Y;
                    let pan_scale = self.distance * 0.0018;
                    self.focus += (-right * delta_x + up * delta_y) * pan_scale;
                }
            }
            ViewportCameraMode::Mode2D => {
                if self.right_down || self.middle_down || (self.alt_down && self.left_down) {
                    self.center_2d.x -= delta_x * self.zoom_2d;
                    self.center_2d.y += delta_y * self.zoom_2d;
                }
            }
        }
    }

    fn apply_wheel_delta(&mut self, delta_y: f32, camera_mode: ViewportCameraMode) {
        if delta_y.abs() <= f32::EPSILON {
            return;
        }
        let factor = bevy_math::ops::exp(-delta_y * 0.1);
        match camera_mode {
            ViewportCameraMode::Mode3D => {
                self.distance = (self.distance * factor).clamp(0.4, 250.0);
            }
            ViewportCameraMode::Mode2D => {
                self.zoom_2d = (self.zoom_2d * factor).clamp(0.05, 20.0);
            }
        }
    }
}

pub(crate) fn spawn_editor_cameras(
    mut commands: Commands,
    config: Res<ViewportConfig>,
    state: Res<EditorCameraState>,
    existing_3d: Query<(), With<EditorCamera3d>>,
    existing_2d: Query<(), With<EditorCamera2d>>,
) {
    if existing_3d.is_empty() {
        commands.spawn((
            EditorCamera3d,
            Camera3d::default(),
            Camera {
                is_active: config.camera_mode == ViewportCameraMode::Mode3D,
                order: 10,
                ..Default::default()
            },
            state.compute_3d_transform(),
            Projection::Perspective(PerspectiveProjection {
                fov: 45.0_f32.to_radians(),
                ..Default::default()
            }),
        ));
    }

    if existing_2d.is_empty() {
        commands.spawn((
            EditorCamera2d,
            Camera2d,
            Camera {
                is_active: config.camera_mode == ViewportCameraMode::Mode2D,
                order: 10,
                ..Default::default()
            },
            state.compute_2d_transform(),
            Projection::Orthographic(OrthographicProjection {
                scale: state.zoom_2d,
                ..OrthographicProjection::default_2d()
            }),
        ));
    }
}

pub(crate) fn sync_editor_camera_activity(
    config: Res<ViewportConfig>,
    mut cam3d: Query<&mut Camera, (With<EditorCamera3d>, Without<EditorCamera2d>)>,
    mut cam2d: Query<&mut Camera, (With<EditorCamera2d>, Without<EditorCamera3d>)>,
) {
    let want_3d = config.camera_mode == ViewportCameraMode::Mode3D;
    for mut camera in &mut cam3d {
        if camera.is_active != want_3d {
            camera.is_active = want_3d;
        }
    }
    for mut camera in &mut cam2d {
        if camera.is_active == want_3d {
            camera.is_active = !want_3d;
        }
    }
}

type EditorCam3dFilter = (With<EditorCamera3d>, Without<EditorCamera2d>);
type EditorCam2dFilter = (With<EditorCamera2d>, Without<EditorCamera3d>);

#[expect(
    clippy::too_many_arguments,
    reason = "Combines forwarded IPC input, optional native OS input, gizmo drag gate, and 2D/3D camera sync"
)]
pub(crate) fn update_editor_cameras(
    config: Res<ViewportConfig>,
    time: Res<Time>,
    mut state: ResMut<EditorCameraState>,
    frame_input: Res<ViewportFrameInput>,
    gizmo_state: Option<Res<crate::gizmo::TransformGizmoState>>,
    mouse_buttons: Option<Res<ButtonInput<MouseButton>>>,
    key_codes: Option<Res<ButtonInput<KeyCode>>>,
    mut mouse_motion: MessageReader<MouseMotion>,
    mut mouse_wheel: MessageReader<MouseWheel>,
    mut cam3d_query: Query<&mut Transform, EditorCam3dFilter>,
    mut cam2d_query: Query<(&mut Transform, &mut Projection), EditorCam2dFilter>,
) {
    let dt = time.delta_secs();
    let gizmo_dragging = gizmo_state
        .as_deref()
        .is_some_and(crate::gizmo::TransformGizmoState::is_dragging);

    if !frame_input.events.is_empty() {
        if gizmo_dragging {
            let filtered: Vec<ViewportInputEvent> = frame_input
                .events
                .iter()
                .filter(|event| !matches!(event, ViewportInputEvent::PointerMove { .. }))
                .cloned()
                .collect();
            if !filtered.is_empty() {
                state.apply_forwarded_events(&filtered, config.camera_mode, dt);
            }
        } else {
            state.apply_forwarded_events(&frame_input.events, config.camera_mode, dt);
        }
    }

    if config.mode == ViewportMode::NativeWindow {
        if let Some(buttons) = mouse_buttons.as_deref() {
            state.left_down = buttons.pressed(MouseButton::Left);
            state.right_down = buttons.pressed(MouseButton::Right);
            state.middle_down = buttons.pressed(MouseButton::Middle);
        }
        if let Some(keys) = key_codes.as_deref() {
            state.alt_down = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
        }

        let mut delta = Vec2::ZERO;
        for motion in mouse_motion.read() {
            delta += motion.delta;
        }
        if !gizmo_dragging && delta != Vec2::ZERO {
            state.apply_pointer_delta(delta.x, delta.y, config.camera_mode);
        }

        let mut wheel_y = 0.0;
        for wheel in mouse_wheel.read() {
            wheel_y += wheel.y;
        }
        if !gizmo_dragging && wheel_y.abs() > f32::EPSILON {
            state.apply_wheel_delta(wheel_y, config.camera_mode);
        }

        if config.camera_mode == ViewportCameraMode::Mode3D
            && state.right_down
            && let Some(keys) = key_codes.as_deref()
        {
            let mut move_dir = Vec3::ZERO;
            if keys.pressed(KeyCode::KeyW) {
                move_dir.z -= 1.0;
            }
            if keys.pressed(KeyCode::KeyS) {
                move_dir.z += 1.0;
            }
            if keys.pressed(KeyCode::KeyA) {
                move_dir.x -= 1.0;
            }
            if keys.pressed(KeyCode::KeyD) {
                move_dir.x += 1.0;
            }
            if keys.pressed(KeyCode::KeyE) {
                move_dir.y += 1.0;
            }
            if keys.pressed(KeyCode::KeyQ) {
                move_dir.y -= 1.0;
            }
            if move_dir != Vec3::ZERO {
                let speed_mul =
                    if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
                        2.5
                    } else {
                        1.0
                    };
                let orientation =
                    Quat::from_euler(bevy_math::EulerRot::YXZ, state.yaw, state.pitch, 0.0);
                let step = orientation * move_dir.normalize() * (state.fly_speed * speed_mul * dt);
                state.focus += step;
            }
        }
    } else {
        mouse_motion.clear();
        mouse_wheel.clear();
    }

    let desired_3d = state.compute_3d_transform();
    for mut transform in &mut cam3d_query {
        *transform = desired_3d;
    }

    let desired_2d = state.compute_2d_transform();
    for (mut transform, mut projection) in &mut cam2d_query {
        *transform = desired_2d;
        if let Projection::Orthographic(ortho) = &mut *projection {
            ortho.scale = state.zoom_2d;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_protocol::InputModifiers;

    #[test]
    fn forwarded_input_orbits_and_zooms_3d_without_clobbering_2d() {
        let mut state = EditorCameraState::default();
        let initial_yaw = state.yaw;
        let initial_dist = state.distance;
        let initial_2d_center = state.center_2d;
        let initial_2d_zoom = state.zoom_2d;

        state.apply_forwarded_events(
            &[
                ViewportInputEvent::PointerButton {
                    button: PointerButtonCode::Right,
                    pressed: true,
                    x: 100.0,
                    y: 100.0,
                    modifiers: InputModifiers::default(),
                },
                ViewportInputEvent::PointerMove {
                    x: 140.0,
                    y: 110.0,
                    delta_x: 40.0,
                    delta_y: 10.0,
                },
                ViewportInputEvent::Wheel {
                    delta_x: 0.0,
                    delta_y: 2.0,
                    modifiers: InputModifiers::default(),
                },
                ViewportInputEvent::PointerButton {
                    button: PointerButtonCode::Right,
                    pressed: false,
                    x: 140.0,
                    y: 110.0,
                    modifiers: InputModifiers::default(),
                },
            ],
            ViewportCameraMode::Mode3D,
            1.0 / 60.0,
        );

        assert!(state.yaw < initial_yaw);
        assert!(state.distance < initial_dist);
        assert_eq!(state.center_2d, initial_2d_center);
        assert_eq!(state.zoom_2d, initial_2d_zoom);
    }

    #[test]
    fn forwarded_input_pans_and_zooms_2d_independently() {
        let mut state = EditorCameraState::default();
        let initial_focus = state.focus;

        state.apply_forwarded_events(
            &[
                ViewportInputEvent::PointerButton {
                    button: PointerButtonCode::Middle,
                    pressed: true,
                    x: 50.0,
                    y: 50.0,
                    modifiers: InputModifiers::default(),
                },
                ViewportInputEvent::PointerMove {
                    x: 70.0,
                    y: 30.0,
                    delta_x: 20.0,
                    delta_y: -20.0,
                },
                ViewportInputEvent::Wheel {
                    delta_x: 0.0,
                    delta_y: -1.5,
                    modifiers: InputModifiers::default(),
                },
            ],
            ViewportCameraMode::Mode2D,
            1.0 / 60.0,
        );

        assert_eq!(state.center_2d, Vec2::new(-20.0, -20.0));
        assert!(state.zoom_2d > 1.0);
        assert_eq!(state.focus, initial_focus);
    }
}
