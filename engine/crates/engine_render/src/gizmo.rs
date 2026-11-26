//! Interactive 3D and 2D transform gizmos (`Translate`, `Rotate`, `Scale`) with snapping
//! and `Local`/`World` coordinate spaces.

use bevy_camera::{Camera, OrthographicProjection, Projection};
use bevy_color::Color;
use bevy_ecs::{entity::Entity, resource::Resource};
use bevy_gizmos::gizmos::Gizmos;
use bevy_input::{keyboard::KeyCode, ButtonInput};
use bevy_math::{ops, EulerRot, Isometry2d, Isometry3d, Mat3, Quat, Rot2, Vec2, Vec3, Vec3Swizzles};
use bevy_shape::Ray3d;
use bevy_transform::components::{GlobalTransform, Transform};
use engine_core::EntityGuid;
use engine_protocol::{
    GizmoMode, GizmoSnapConfig, GizmoSpace, ViewportCameraMode, ViewportInputEvent,
};

use crate::picking::{EditorOverlayGizmoGroup, EditorPointerState};

/// Individual interactive handle of a 3D or 2D transform gizmo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GizmoHandle {
    /// X axis arrow / ring / scale arm.
    AxisX,
    /// Y axis arrow / ring / scale arm.
    AxisY,
    /// Z axis arrow / ring / scale arm (also 2D Z rotation ring).
    AxisZ,
    /// XY plane translation square (also free XY translation handle in 2D).
    PlaneXY,
    /// XZ plane translation square.
    PlaneXZ,
    /// YZ plane translation square.
    PlaneYZ,
    /// Center uniform scale cube / square handle.
    CenterUniform,
}

impl GizmoHandle {
    /// Axis index (`0 = X`, `1 = Y`, `2 = Z`) when this handle represents a single cardinal axis.
    #[must_use]
    pub const fn axis_index(self) -> Option<usize> {
        match self {
            Self::AxisX => Some(0),
            Self::AxisY => Some(1),
            Self::AxisZ => Some(2),
            Self::PlaneXY | Self::PlaneXZ | Self::PlaneYZ | Self::CenterUniform => None,
        }
    }

    /// Returns `(u_axis_index, v_axis_index, normal_axis_index)` for plane handles.
    #[must_use]
    pub const fn plane_indices(self) -> Option<(usize, usize, usize)> {
        match self {
            Self::PlaneXY => Some((0, 1, 2)),
            Self::PlaneXZ => Some((0, 2, 1)),
            Self::PlaneYZ => Some((1, 2, 0)),
            Self::AxisX | Self::AxisY | Self::AxisZ | Self::CenterUniform => None,
        }
    }
}

/// Snapshot of a selected entity's transform inputs supplied by the editor bridge.
#[derive(Debug, Clone, Copy)]
pub struct GizmoTargetSnapshot {
    /// Persistent entity GUID.
    pub guid: EntityGuid,
    /// ECS entity handle.
    pub entity: Entity,
    /// Entity's current local [`Transform`].
    pub local_transform: Transform,
    /// Entity's current computed [`GlobalTransform`].
    pub global_transform: GlobalTransform,
    /// Parent entity's [`GlobalTransform`], if parented.
    pub parent_global: Option<GlobalTransform>,
}

/// Completed gizmo drag transaction ready to be recorded in Undo/Redo history.
#[derive(Debug, Clone, PartialEq)]
pub struct GizmoTransformCommit {
    /// Human-readable operation description (e.g. `"Translate"`, `"Rotate"`, `"Scale"`).
    pub label: String,
    /// Per-entity `(guid, before_local, after_local)` transform diffs.
    pub entries: Vec<(EntityGuid, Transform, Transform)>,
}

#[derive(Debug, Clone)]
struct DragTargetStart {
    guid: EntityGuid,
    entity: Entity,
    initial_local: Transform,
    current_local: Transform,
    initial_world_pos: Vec3,
    initial_world_rot: Quat,
    parent_global: Option<GlobalTransform>,
}

#[derive(Debug, Clone)]
struct ActiveGizmoDrag {
    mode: GizmoMode,
    camera_mode: ViewportCameraMode,
    handle: GizmoHandle,
    pivot_start: Vec3,
    basis_rot: Quat,
    gizmo_scale: f32,
    initial_cursor_pos: Vec2,
    initial_axis_param: f32,
    initial_plane_point: Vec3,
    last_angle: f32,
    cumulative_angle: f32,
    targets: Vec<DragTargetStart>,
}

/// Resource driving 3D and 2D transform gizmo interaction, snapping, and rendering.
#[derive(Resource, Debug, Clone)]
pub struct TransformGizmoState {
    /// Active gizmo tool (`Translate`, `Rotate`, or `Scale`).
    pub mode: GizmoMode,
    /// Active coordinate basis (`World` vs `Local`).
    pub space: GizmoSpace,
    /// Grid/step snapping configuration.
    pub snap: GizmoSnapConfig,
    /// Handle currently hovered under the cursor (when not dragging).
    pub hovered_handle: Option<GizmoHandle>,
    /// Set `true` when the latest protocol/hotkey change needs broadcasting over IPC.
    pub state_dirty: bool,
    /// Set `true` during a drag press/move/release so click-to-select picking does not clear selection.
    pub consumed_pointer_click: bool,
    active_drag: Option<ActiveGizmoDrag>,
    /// Entity GUIDs whose Inspector transform fields should refresh after a completed drag.
    pub inspector_refresh_guids: Vec<EntityGuid>,
    /// Completed drag commits queued for the Undo/Redo history stack.
    pub completed_commits: Vec<GizmoTransformCommit>,
}

impl Default for TransformGizmoState {
    fn default() -> Self {
        Self {
            mode: GizmoMode::Translate,
            space: GizmoSpace::World,
            snap: GizmoSnapConfig::default(),
            hovered_handle: None,
            state_dirty: false,
            consumed_pointer_click: false,
            active_drag: None,
            inspector_refresh_guids: Vec::new(),
            completed_commits: Vec::new(),
        }
    }
}

impl TransformGizmoState {
    /// Returns `true` if a gizmo handle is currently being dragged.
    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        self.active_drag.is_some()
    }

    /// Returns the currently active handle being dragged, if any.
    #[must_use]
    pub fn active_handle(&self) -> Option<GizmoHandle> {
        self.active_drag.as_ref().map(|d| d.handle)
    }

    /// Drains all finished gizmo drag commits in chronological order.
    pub fn drain_commits(&mut self) -> Vec<GizmoTransformCommit> {
        core::mem::take(&mut self.completed_commits)
    }

    /// Updates [`Self::mode`] from `W` / `E` / `R` shortcuts while guarding against camera fly mode.
    pub fn apply_shortcuts(
        &mut self,
        forwarded_events: &[ViewportInputEvent],
        native_keys: Option<&ButtonInput<KeyCode>>,
        right_mouse_down: bool,
        allow_native_shortcuts: bool,
    ) {
        if right_mouse_down || self.is_dragging() {
            return;
        }

        let mut next_mode = None;
        for event in forwarded_events {
            if let ViewportInputEvent::Key {
                code,
                pressed: true,
                modifiers,
            } = event
            {
                if modifiers.ctrl || modifiers.alt || modifiers.meta {
                    continue;
                }
                match code.as_str() {
                    "KeyW" => next_mode = Some(GizmoMode::Translate),
                    "KeyE" => next_mode = Some(GizmoMode::Rotate),
                    "KeyR" => next_mode = Some(GizmoMode::Scale),
                    _ => {}
                }
            }
        }

        if allow_native_shortcuts && let Some(keys) = native_keys {
            let modifier_held = keys.pressed(KeyCode::ControlLeft)
                || keys.pressed(KeyCode::ControlRight)
                || keys.pressed(KeyCode::AltLeft)
                || keys.pressed(KeyCode::AltRight)
                || keys.pressed(KeyCode::SuperLeft)
                || keys.pressed(KeyCode::SuperRight);
            if !modifier_held {
                if keys.just_pressed(KeyCode::KeyW) {
                    next_mode = Some(GizmoMode::Translate);
                } else if keys.just_pressed(KeyCode::KeyE) {
                    next_mode = Some(GizmoMode::Rotate);
                } else if keys.just_pressed(KeyCode::KeyR) {
                    next_mode = Some(GizmoMode::Scale);
                }
            }
        }

        if let Some(mode) = next_mode
            && self.mode != mode
        {
            self.mode = mode;
            self.hovered_handle = None;
            self.state_dirty = true;
        }
    }

    /// Evaluates hover hit-testing, drag start, drag motion, and drag commit against `targets`.
    ///
    /// Returns updated `(Entity, Transform)` pairs whenever an active drag updates targets.
    pub fn update_interaction(
        &mut self,
        camera_mode: ViewportCameraMode,
        camera: Option<(&Camera, &Projection, &GlobalTransform)>,
        pointer: &EditorPointerState,
        targets: &[GizmoTargetSnapshot],
    ) -> Vec<(Entity, Transform)> {
        if targets.is_empty() {
            self.hovered_handle = None;
            if !pointer.left_down && !pointer.left_just_released {
                self.consumed_pointer_click = false;
            }
            if let Some(drag) = self.active_drag.take() {
                push_commit_if_changed(
                    &mut self.completed_commits,
                    &mut self.inspector_refresh_guids,
                    drag,
                );
            }
            return Vec::new();
        }

        let Some((_cam, projection, cam_global)) = camera else {
            return Vec::new();
        };

        let (pivot, basis_rot) = compute_gizmo_pivot_and_basis(targets, self.space, self.mode);
        let gizmo_scale = compute_gizmo_scale(camera_mode, projection, cam_global.translation(), pivot);

        // If already dragging, update or finalize drag.
        if let Some(drag) = self.active_drag.as_mut() {
            self.consumed_pointer_click = true;
            let snap_active = self.snap.enabled ^ pointer.ctrl_down;
            let updates = update_active_drag(
                drag,
                pointer.ray,
                pointer.position,
                self.snap,
                snap_active,
            );

            if (pointer.left_just_released || !pointer.left_down)
                && let Some(finished) = self.active_drag.take()
            {
                push_commit_if_changed(
                    &mut self.completed_commits,
                    &mut self.inspector_refresh_guids,
                    finished,
                );
            }
            return updates;
        }

        // Clear click consumption only after any release frame has passed.
        if !pointer.left_down && !pointer.left_just_released {
            self.consumed_pointer_click = false;
        }

        // Do not hover or grab gizmo handles while orbiting, panning, or holding Alt.
        if pointer.right_down || pointer.middle_down || pointer.alt_down {
            self.hovered_handle = None;
            return Vec::new();
        }

        let Some(ray) = pointer.ray else {
            self.hovered_handle = None;
            return Vec::new();
        };

        let hit_handle = hit_test_gizmo(ray, camera_mode, self.mode, pivot, basis_rot, gizmo_scale);
        self.hovered_handle = hit_handle;

        if pointer.left_just_pressed
            && let Some(handle) = hit_handle
        {
            self.consumed_pointer_click = true;
            if let Some(drag) = start_active_drag(
                self.mode,
                camera_mode,
                handle,
                pivot,
                basis_rot,
                gizmo_scale,
                ray,
                pointer.position.unwrap_or(Vec2::ZERO),
                targets,
            ) {
                if pointer.left_just_released || !pointer.left_down {
                    push_commit_if_changed(
                        &mut self.completed_commits,
                        &mut self.inspector_refresh_guids,
                        drag,
                    );
                } else {
                    self.active_drag = Some(drag);
                }
            }
        }

        Vec::new()
    }

    /// Renders the 3D or 2D gizmo handles onto `gizmos` for `targets`.
    pub fn draw(
        &self,
        gizmos: &mut Gizmos<EditorOverlayGizmoGroup>,
        camera_mode: ViewportCameraMode,
        projection: Option<&Projection>,
        cam_pos: Vec3,
        targets: &[GizmoTargetSnapshot],
    ) {
        if targets.is_empty() {
            return;
        }

        let (pivot, basis_rot) = compute_gizmo_pivot_and_basis(targets, self.space, self.mode);
        let fallback_proj = match camera_mode {
            ViewportCameraMode::Mode3D => Projection::Perspective(Default::default()),
            ViewportCameraMode::Mode2D => {
                Projection::Orthographic(OrthographicProjection::default_2d())
            }
        };
        let proj = projection.unwrap_or(&fallback_proj);
        let gizmo_scale = compute_gizmo_scale(camera_mode, proj, cam_pos, pivot);

        let highlight = self
            .active_drag
            .as_ref()
            .map(|d| d.handle)
            .or(self.hovered_handle);

        let color_x = Color::srgb(0.95, 0.25, 0.25);
        let color_y = Color::srgb(0.28, 0.90, 0.36);
        let color_z = Color::srgb(0.28, 0.56, 0.98);
        let color_center = Color::srgb(0.92, 0.94, 0.98);
        let color_active = Color::srgb(1.0, 0.90, 0.20);

        let pick_color = |handle: GizmoHandle, base: Color| -> Color {
            if highlight == Some(handle) {
                color_active
            } else {
                base
            }
        };

        let axis_x = basis_rot * Vec3::X;
        let axis_y = basis_rot * Vec3::Y;
        let axis_z = basis_rot * Vec3::Z;

        match camera_mode {
            ViewportCameraMode::Mode3D => match self.mode {
                GizmoMode::Translate => {
                    gizmos.arrow(pivot, pivot + axis_x * gizmo_scale, pick_color(GizmoHandle::AxisX, color_x));
                    gizmos.arrow(pivot, pivot + axis_y * gizmo_scale, pick_color(GizmoHandle::AxisY, color_y));
                    gizmos.arrow(pivot, pivot + axis_z * gizmo_scale, pick_color(GizmoHandle::AxisZ, color_z));

                    draw_plane_square_3d(
                        gizmos,
                        pivot,
                        axis_x,
                        axis_y,
                        gizmo_scale,
                        pick_color(GizmoHandle::PlaneXY, color_z),
                    );
                    draw_plane_square_3d(
                        gizmos,
                        pivot,
                        axis_x,
                        axis_z,
                        gizmo_scale,
                        pick_color(GizmoHandle::PlaneXZ, color_y),
                    );
                    draw_plane_square_3d(
                        gizmos,
                        pivot,
                        axis_y,
                        axis_z,
                        gizmo_scale,
                        pick_color(GizmoHandle::PlaneYZ, color_x),
                    );
                }
                GizmoMode::Rotate => {
                    let radius = gizmo_scale * 0.95;
                    // `gizmos.circle` lies in the XY plane of its Isometry3d (`normal = local +Z`).
                    let rot_x = basis_rot * Quat::from_rotation_y(core::f32::consts::FRAC_PI_2);
                    let rot_y = basis_rot * Quat::from_rotation_x(core::f32::consts::FRAC_PI_2);
                    let rot_z = basis_rot;

                    gizmos
                        .circle(Isometry3d::new(pivot, rot_x), radius, pick_color(GizmoHandle::AxisX, color_x))
                        .resolution(48);
                    gizmos
                        .circle(Isometry3d::new(pivot, rot_y), radius, pick_color(GizmoHandle::AxisY, color_y))
                        .resolution(48);
                    gizmos
                        .circle(Isometry3d::new(pivot, rot_z), radius, pick_color(GizmoHandle::AxisZ, color_z))
                        .resolution(48);
                }
                GizmoMode::Scale => {
                    let tip_half = gizmo_scale * 0.08;
                    for (handle, axis, color) in [
                        (GizmoHandle::AxisX, axis_x, color_x),
                        (GizmoHandle::AxisY, axis_y, color_y),
                        (GizmoHandle::AxisZ, axis_z, color_z),
                    ] {
                        let c = pick_color(handle, color);
                        let end = pivot + axis * gizmo_scale;
                        gizmos.line(pivot, end, c);
                        draw_wire_cube(gizmos, end, basis_rot, tip_half, c);
                    }
                    draw_wire_cube(
                        gizmos,
                        pivot,
                        basis_rot,
                        gizmo_scale * 0.11,
                        pick_color(GizmoHandle::CenterUniform, color_center),
                    );
                }
            },
            ViewportCameraMode::Mode2D => {
                let p2 = pivot.xy();
                let ax2 = axis_x.xy().normalize_or(Vec2::X);
                let ay2 = axis_y.xy().normalize_or(Vec2::Y);

                match self.mode {
                    GizmoMode::Translate => {
                        gizmos.arrow_2d(p2, p2 + ax2 * gizmo_scale, pick_color(GizmoHandle::AxisX, color_x));
                        gizmos.arrow_2d(p2, p2 + ay2 * gizmo_scale, pick_color(GizmoHandle::AxisY, color_y));

                        let a = gizmo_scale * 0.22;
                        let b = gizmo_scale * 0.48;
                        let c = pick_color(GizmoHandle::PlaneXY, color_center);
                        let q0 = p2 + ax2 * a + ay2 * a;
                        let q1 = p2 + ax2 * b + ay2 * a;
                        let q2 = p2 + ax2 * b + ay2 * b;
                        let q3 = p2 + ax2 * a + ay2 * b;
                        gizmos.linestrip_2d([q0, q1, q2, q3, q0], c);
                    }
                    GizmoMode::Rotate => {
                        let radius = gizmo_scale * 0.92;
                        let c = pick_color(GizmoHandle::AxisZ, color_z);
                        gizmos.circle_2d(Isometry2d::from_translation(p2), radius, c).resolution(48);
                        gizmos.line_2d(p2, p2 + ax2 * radius, c);
                    }
                    GizmoMode::Scale => {
                        let tip = Vec2::splat(gizmo_scale * 0.14);
                        let rot_2d = Rot2::radians(
                            ops::atan2(ay2.y, ay2.x) - core::f32::consts::FRAC_PI_2,
                        );
                        for (handle, dir, color) in [
                            (GizmoHandle::AxisX, ax2, color_x),
                            (GizmoHandle::AxisY, ay2, color_y),
                        ] {
                            let c = pick_color(handle, color);
                            let end = p2 + dir * gizmo_scale;
                            gizmos.line_2d(p2, end, c);
                            gizmos.rect_2d(Isometry2d::new(end, rot_2d), tip, c);
                        }
                        gizmos.rect_2d(
                            Isometry2d::new(p2, rot_2d),
                            Vec2::splat(gizmo_scale * 0.20),
                            pick_color(GizmoHandle::CenterUniform, color_center),
                        );
                    }
                }
            }
        }
    }
}

/// Computes the gizmo pivot position (mean world position) and basis rotation (`World` vs `Local`).
#[must_use]
pub fn compute_gizmo_pivot_and_basis(
    targets: &[GizmoTargetSnapshot],
    space: GizmoSpace,
    mode: GizmoMode,
) -> (Vec3, Quat) {
    if targets.is_empty() {
        return (Vec3::ZERO, Quat::IDENTITY);
    }

    let mut sum = Vec3::ZERO;
    for target in targets {
        sum += target.global_transform.translation();
    }
    let pivot = sum / targets.len() as f32;

    // Scaling always uses the primary entity's local axes; Translate/Rotate respect `space`.
    let basis_rot = if mode == GizmoMode::Scale || space == GizmoSpace::Local {
        let (_, rot, _) = targets[0].global_transform.to_scale_rotation_translation();
        rot.normalize()
    } else {
        Quat::IDENTITY
    };

    (pivot, basis_rot)
}

/// Computes scale-invariant gizmo handle size in world units.
#[must_use]
pub fn compute_gizmo_scale(
    camera_mode: ViewportCameraMode,
    projection: &Projection,
    cam_pos: Vec3,
    pivot: Vec3,
) -> f32 {
    match camera_mode {
        ViewportCameraMode::Mode3D => {
            let dist = cam_pos.distance(pivot).max(0.5);
            (dist * 0.14).clamp(0.35, 30.0)
        }
        ViewportCameraMode::Mode2D => match projection {
            Projection::Orthographic(ortho) => (75.0 * ortho.scale).clamp(12.0, 2000.0),
            _ => 75.0,
        },
    }
}

/// Snap helper quantizing `value` to nearest multiple of `step`.
#[must_use]
pub fn snap_value(value: f32, step: f32) -> f32 {
    if step <= f32::EPSILON {
        value
    } else {
        (value / step).round() * step
    }
}

/// Hit-tests `ray` against the rendered gizmo handles and returns the closest matched handle.
#[must_use]
pub fn hit_test_gizmo(
    ray: Ray3d,
    camera_mode: ViewportCameraMode,
    mode: GizmoMode,
    pivot: Vec3,
    basis_rot: Quat,
    gizmo_scale: f32,
) -> Option<GizmoHandle> {
    let axes = [
        basis_rot * Vec3::X,
        basis_rot * Vec3::Y,
        basis_rot * Vec3::Z,
    ];

    match camera_mode {
        ViewportCameraMode::Mode2D => {
            let dir_z = ray.direction.z;
            if dir_z.abs() <= 1e-5 {
                return None;
            }
            let t = (pivot.z - ray.origin.z) / dir_z;
            let hit = (ray.origin + *ray.direction * t).xy();
            let p2 = pivot.xy();
            let ax = axes[0].xy().normalize_or(Vec2::X);
            let ay = axes[1].xy().normalize_or(Vec2::Y);
            let thresh = gizmo_scale * 0.16;

            match mode {
                GizmoMode::Translate => {
                    let rel = hit - p2;
                    let u = rel.dot(ax);
                    let v = rel.dot(ay);
                    let min_p = gizmo_scale * 0.20;
                    let max_p = gizmo_scale * 0.50;
                    if (min_p..=max_p).contains(&u) && (min_p..=max_p).contains(&v) {
                        return Some(GizmoHandle::PlaneXY);
                    }
                    let d_x = distance_point_to_segment_2d(hit, p2, p2 + ax * gizmo_scale);
                    let d_y = distance_point_to_segment_2d(hit, p2, p2 + ay * gizmo_scale);
                    pick_closest_2d(
                        &[(GizmoHandle::AxisX, d_x), (GizmoHandle::AxisY, d_y)],
                        thresh,
                    )
                }
                GizmoMode::Rotate => {
                    let radius = gizmo_scale * 0.92;
                    let dist = (hit.distance(p2) - radius).abs();
                    (dist <= thresh).then_some(GizmoHandle::AxisZ)
                }
                GizmoMode::Scale => {
                    if hit.distance(p2) <= gizmo_scale * 0.16 {
                        return Some(GizmoHandle::CenterUniform);
                    }
                    let d_x = distance_point_to_segment_2d(hit, p2, p2 + ax * gizmo_scale);
                    let d_y = distance_point_to_segment_2d(hit, p2, p2 + ay * gizmo_scale);
                    pick_closest_2d(
                        &[(GizmoHandle::AxisX, d_x), (GizmoHandle::AxisY, d_y)],
                        thresh,
                    )
                }
            }
        }
        ViewportCameraMode::Mode3D => match mode {
            GizmoMode::Translate => {
                let mut best: Option<(f32, GizmoHandle)> = None;
                let plane_min = gizmo_scale * 0.20;
                let plane_max = gizmo_scale * 0.50;

                for handle in [GizmoHandle::PlaneXY, GizmoHandle::PlaneXZ, GizmoHandle::PlaneYZ] {
                    let (ui, vi, ni) = handle.plane_indices().expect("plane handle");
                    if let Some((t_ray, p)) = ray_plane_intersect(ray, pivot, axes[ni]) {
                        let rel = p - pivot;
                        let u = rel.dot(axes[ui]);
                        let v = rel.dot(axes[vi]);
                        if (plane_min..=plane_max).contains(&u)
                            && (plane_min..=plane_max).contains(&v)
                        {
                            best = pick_nearer(best, t_ray, handle);
                        }
                    }
                }
                if let Some((_, handle)) = best {
                    return Some(handle);
                }

                let thresh = gizmo_scale * 0.14;
                for (idx, handle) in [
                    (0, GizmoHandle::AxisX),
                    (1, GizmoHandle::AxisY),
                    (2, GizmoHandle::AxisZ),
                ] {
                    if let Some((dist, t_ray)) =
                        ray_segment_distance_3d(ray, pivot, pivot + axes[idx] * (gizmo_scale * 1.05))
                        && dist <= thresh
                    {
                        best = pick_nearer(best, t_ray, handle);
                    }
                }
                best.map(|(_, h)| h)
            }
            GizmoMode::Rotate => {
                let radius = gizmo_scale * 0.95;
                let ring_thresh = gizmo_scale * 0.14;
                let mut best: Option<(f32, GizmoHandle)> = None;
                for (idx, handle) in [
                    (0, GizmoHandle::AxisX),
                    (1, GizmoHandle::AxisY),
                    (2, GizmoHandle::AxisZ),
                ] {
                    if let Some((t_ray, hit_p)) = ray_plane_intersect(ray, pivot, axes[idx]) {
                        let radial_err = (hit_p.distance(pivot) - radius).abs();
                        if radial_err <= ring_thresh {
                            best = pick_nearer(best, t_ray, handle);
                        }
                    }
                }
                best.map(|(_, h)| h)
            }
            GizmoMode::Scale => {
                if let Some(t_center) =
                    ray_sphere_distance(ray, pivot, gizmo_scale * 0.16)
                {
                    let _ = t_center;
                    return Some(GizmoHandle::CenterUniform);
                }
                let thresh = gizmo_scale * 0.14;
                let mut best: Option<(f32, GizmoHandle)> = None;
                for (idx, handle) in [
                    (0, GizmoHandle::AxisX),
                    (1, GizmoHandle::AxisY),
                    (2, GizmoHandle::AxisZ),
                ] {
                    if let Some((dist, t_ray)) =
                        ray_segment_distance_3d(ray, pivot, pivot + axes[idx] * (gizmo_scale * 1.05))
                        && dist <= thresh
                    {
                        best = pick_nearer(best, t_ray, handle);
                    }
                }
                best.map(|(_, h)| h)
            }
        },
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Captures initial drag anchors across ray, pivot, basis, and targets"
)]
fn start_active_drag(
    mode: GizmoMode,
    camera_mode: ViewportCameraMode,
    handle: GizmoHandle,
    pivot: Vec3,
    basis_rot: Quat,
    gizmo_scale: f32,
    ray: Ray3d,
    cursor_pos: Vec2,
    targets: &[GizmoTargetSnapshot],
) -> Option<ActiveGizmoDrag> {
    let drag_targets: Vec<DragTargetStart> = targets
        .iter()
        .map(|t| {
            let (_, w_rot, w_pos) = t.global_transform.to_scale_rotation_translation();
            DragTargetStart {
                guid: t.guid,
                entity: t.entity,
                initial_local: t.local_transform,
                current_local: t.local_transform,
                initial_world_pos: w_pos,
                initial_world_rot: w_rot.normalize(),
                parent_global: t.parent_global,
            }
        })
        .collect();

    let axes = [
        basis_rot * Vec3::X,
        basis_rot * Vec3::Y,
        basis_rot * Vec3::Z,
    ];

    let mut initial_axis_param = 0.0;
    let mut initial_plane_point = pivot;
    let mut last_angle = 0.0;

    match mode {
        GizmoMode::Translate => {
            if let Some(axis_idx) = handle.axis_index() {
                initial_axis_param = closest_point_on_axis_ray(ray, pivot, axes[axis_idx])?;
            } else if let Some((_, _, normal_idx)) = handle.plane_indices() {
                let (_, hit_p) = ray_plane_intersect_unclamped(ray, pivot, axes[normal_idx])?;
                initial_plane_point = hit_p;
            }
        }
        GizmoMode::Rotate => {
            let axis_idx = if camera_mode == ViewportCameraMode::Mode2D {
                2
            } else {
                handle.axis_index().unwrap_or(2)
            };
            let axis = axes[axis_idx];
            if let Some((_, hit_p)) = ray_plane_intersect_unclamped(ray, pivot, axis)
                && (hit_p - pivot).length_squared() > 1e-6
            {
                let v = (hit_p - pivot).normalize();
                let (u_ref, w_ref) = orthonormal_basis_for_normal(axis);
                last_angle = ops::atan2(v.dot(w_ref), v.dot(u_ref));
            }
        }
        GizmoMode::Scale => {
            if let Some(axis_idx) = handle.axis_index() {
                initial_axis_param =
                    closest_point_on_axis_ray(ray, pivot, axes[axis_idx]).unwrap_or(gizmo_scale);
            }
        }
    }

    Some(ActiveGizmoDrag {
        mode,
        camera_mode,
        handle,
        pivot_start: pivot,
        basis_rot,
        gizmo_scale,
        initial_cursor_pos: cursor_pos,
        initial_axis_param,
        initial_plane_point,
        last_angle,
        cumulative_angle: 0.0,
        targets: drag_targets,
    })
}

fn update_active_drag(
    drag: &mut ActiveGizmoDrag,
    ray_opt: Option<Ray3d>,
    cursor_opt: Option<Vec2>,
    snap: GizmoSnapConfig,
    snap_active: bool,
) -> Vec<(Entity, Transform)> {
    let axes = [
        drag.basis_rot * Vec3::X,
        drag.basis_rot * Vec3::Y,
        drag.basis_rot * Vec3::Z,
    ];

    match drag.mode {
        GizmoMode::Translate => {
            let Some(ray) = ray_opt else {
                return Vec::new();
            };
            let translate_step = if drag.camera_mode == ViewportCameraMode::Mode2D {
                snap.translate_step_2d
            } else {
                snap.translate_step
            };
            let world_delta = if let Some(axis_idx) = drag.handle.axis_index() {
                let Some(curr_t) = closest_point_on_axis_ray(ray, drag.pivot_start, axes[axis_idx])
                else {
                    return Vec::new();
                };
                let mut raw = curr_t - drag.initial_axis_param;
                if snap_active {
                    raw = snap_value(raw, translate_step);
                }
                axes[axis_idx] * raw
            } else if let Some((ui, vi, ni)) = drag.handle.plane_indices() {
                let Some((_, hit_p)) =
                    ray_plane_intersect_unclamped(ray, drag.pivot_start, axes[ni])
                else {
                    return Vec::new();
                };
                let delta = hit_p - drag.initial_plane_point;
                let mut du = delta.dot(axes[ui]);
                let mut dv = delta.dot(axes[vi]);
                if snap_active {
                    du = snap_value(du, translate_step);
                    dv = snap_value(dv, translate_step);
                }
                axes[ui] * du + axes[vi] * dv
            } else {
                Vec3::ZERO
            };

            let mut out = Vec::with_capacity(drag.targets.len());
            for target in &mut drag.targets {
                let new_world_pos = target.initial_world_pos + world_delta;
                let local_pos = world_to_local_position(new_world_pos, target.parent_global);
                let mut next = target.initial_local;
                next.translation = local_pos;
                target.current_local = next;
                out.push((target.entity, next));
            }
            out
        }
        GizmoMode::Rotate => {
            let axis_idx = if drag.camera_mode == ViewportCameraMode::Mode2D {
                2
            } else {
                drag.handle.axis_index().unwrap_or(2)
            };
            let axis = axes[axis_idx];

            if let Some(ray) = ray_opt
                && let Some((_, hit_p)) = ray_plane_intersect_unclamped(ray, drag.pivot_start, axis)
                && (hit_p - drag.pivot_start).length_squared() > 1e-6
            {
                let v = (hit_p - drag.pivot_start).normalize();
                let (u_ref, w_ref) = orthonormal_basis_for_normal(axis);
                let current_angle = ops::atan2(v.dot(w_ref), v.dot(u_ref));
                let mut step_delta = current_angle - drag.last_angle;
                while step_delta > core::f32::consts::PI {
                    step_delta -= core::f32::consts::TAU;
                }
                while step_delta < -core::f32::consts::PI {
                    step_delta += core::f32::consts::TAU;
                }
                drag.cumulative_angle += step_delta;
                drag.last_angle = current_angle;
            } else if let Some(cursor_pos) = cursor_opt {
                drag.cumulative_angle = (cursor_pos.x - drag.initial_cursor_pos.x) * 0.015;
            } else {
                return Vec::new();
            }

            let mut applied_angle = drag.cumulative_angle;
            if snap_active {
                let step_rad = snap.rotate_step_degrees.max(0.1).to_radians();
                applied_angle = snap_value(applied_angle, step_rad);
            }
            let delta_rot = Quat::from_axis_angle(axis, applied_angle);

            let mut out = Vec::with_capacity(drag.targets.len());
            for target in &mut drag.targets {
                let new_world_pos =
                    drag.pivot_start + delta_rot * (target.initial_world_pos - drag.pivot_start);
                let new_world_rot = (delta_rot * target.initial_world_rot).normalize();
                let mut next = target.initial_local;
                next.translation = world_to_local_position(new_world_pos, target.parent_global);
                next.rotation = world_to_local_rotation(new_world_rot, target.parent_global);
                target.current_local = next;
                out.push((target.entity, next));
            }
            out
        }
        GizmoMode::Scale => {
            let mut factor_vec = Vec3::ONE;
            match drag.handle {
                GizmoHandle::CenterUniform => {
                    let Some(cursor) = cursor_opt else {
                        return Vec::new();
                    };
                    let delta = ((cursor.x - drag.initial_cursor_pos.x)
                        - (cursor.y - drag.initial_cursor_pos.y))
                        / 120.0;
                    let mut factor = (1.0 + delta).max(0.01);
                    if snap_active {
                        factor = snap_value(factor, snap.scale_step).max(0.01);
                    }
                    factor_vec = if drag.camera_mode == ViewportCameraMode::Mode2D {
                        Vec3::new(factor, factor, 1.0)
                    } else {
                        Vec3::splat(factor)
                    };
                }
                _ => {
                    let Some(ray) = ray_opt else {
                        return Vec::new();
                    };
                    let Some(axis_idx) = drag.handle.axis_index() else {
                        return Vec::new();
                    };
                    let Some(curr_t) =
                        closest_point_on_axis_ray(ray, drag.pivot_start, axes[axis_idx])
                    else {
                        return Vec::new();
                    };
                    let mut factor =
                        (1.0 + (curr_t - drag.initial_axis_param) / drag.gizmo_scale.max(1e-3))
                            .max(0.01);
                    if snap_active {
                        factor = snap_value(factor, snap.scale_step).max(0.01);
                    }
                    match axis_idx {
                        0 => factor_vec.x = factor,
                        1 => factor_vec.y = factor,
                        _ => factor_vec.z = factor,
                    }
                }
            }

            let mut out = Vec::with_capacity(drag.targets.len());
            for target in &mut drag.targets {
                let mut next = target.initial_local;
                next.scale = Vec3::new(
                    (target.initial_local.scale.x * factor_vec.x).max(0.01),
                    (target.initial_local.scale.y * factor_vec.y).max(0.01),
                    (target.initial_local.scale.z * factor_vec.z).max(0.01),
                );
                target.current_local = next;
                out.push((target.entity, next));
            }
            out
        }
    }
}

fn push_commit_if_changed(
    out: &mut Vec<GizmoTransformCommit>,
    refresh_guids: &mut Vec<EntityGuid>,
    drag: ActiveGizmoDrag,
) {
    let entries: Vec<(EntityGuid, Transform, Transform)> = drag
        .targets
        .into_iter()
        .filter(|t| t.initial_local != t.current_local)
        .map(|t| (t.guid, t.initial_local, t.current_local))
        .collect();

    if entries.is_empty() {
        return;
    }

    for (guid, _, _) in &entries {
        if !refresh_guids.contains(guid) {
            refresh_guids.push(*guid);
        }
    }

    let label = match drag.mode {
        GizmoMode::Translate => "Translate".to_owned(),
        GizmoMode::Rotate => "Rotate".to_owned(),
        GizmoMode::Scale => "Scale".to_owned(),
    };

    out.push(GizmoTransformCommit { label, entries });
}

fn world_to_local_position(world_pos: Vec3, parent_global: Option<GlobalTransform>) -> Vec3 {
    match parent_global {
        Some(parent) => parent.affine().inverse().transform_point3(world_pos),
        None => world_pos,
    }
}

fn world_to_local_rotation(world_rot: Quat, parent_global: Option<GlobalTransform>) -> Quat {
    match parent_global {
        Some(parent) => {
            let (_, parent_rot, _) = parent.to_scale_rotation_translation();
            (parent_rot.normalize().inverse() * world_rot).normalize()
        }
        None => world_rot.normalize(),
    }
}

fn closest_point_on_axis_ray(ray: Ray3d, line_origin: Vec3, line_dir: Vec3) -> Option<f32> {
    let u = *ray.direction;
    let v = line_dir.normalize_or_zero();
    if v == Vec3::ZERO {
        return None;
    }
    let w0 = ray.origin - line_origin;
    let b = u.dot(v);
    let d = u.dot(w0);
    let e = v.dot(w0);
    let denom = 1.0 - b * b;
    if denom.abs() < 1e-4 {
        return None;
    }
    Some((e - b * d) / denom)
}

fn ray_plane_intersect(ray: Ray3d, plane_point: Vec3, plane_normal: Vec3) -> Option<(f32, Vec3)> {
    let (t, point) = ray_plane_intersect_unclamped(ray, plane_point, plane_normal)?;
    (t >= 0.0).then_some((t, point))
}

fn ray_plane_intersect_unclamped(
    ray: Ray3d,
    plane_point: Vec3,
    plane_normal: Vec3,
) -> Option<(f32, Vec3)> {
    let denom = (*ray.direction).dot(plane_normal);
    if denom.abs() <= 1e-4 {
        return None;
    }
    let t = (plane_point - ray.origin).dot(plane_normal) / denom;
    Some((t, ray.origin + *ray.direction * t))
}

fn ray_sphere_distance(ray: Ray3d, center: Vec3, radius: f32) -> Option<f32> {
    let oc = ray.origin - center;
    let b = oc.dot(*ray.direction);
    let c = oc.length_squared() - radius * radius;
    if c <= 0.0 {
        return Some(0.0);
    }
    if b > 0.0 {
        return None;
    }
    let disc = b * b - c;
    if disc < 0.0 {
        return None;
    }
    Some((-b - disc.sqrt()).max(0.0))
}

fn ray_segment_distance_3d(ray: Ray3d, seg_a: Vec3, seg_b: Vec3) -> Option<(f32, f32)> {
    let u = *ray.direction;
    let v = seg_b - seg_a;
    let len_sq = v.length_squared();
    if len_sq <= 1e-8 {
        return None;
    }
    let w0 = ray.origin - seg_a;
    let a = u.dot(u);
    let b = u.dot(v);
    let c = len_sq;
    let d = u.dot(w0);
    let e = v.dot(w0);
    let denom = a * c - b * b;

    let (mut sc, mut tc) = if denom < 1e-6 {
        (0.0, (e / c).clamp(0.0, 1.0))
    } else {
        let s = ((b * e - c * d) / denom).max(0.0);
        let t = (a * e - b * d) / denom;
        if t < 0.0 {
            ((-d / a).max(0.0), 0.0)
        } else if t > 1.0 {
            (((b - d) / a).max(0.0), 1.0)
        } else {
            (s, t)
        }
    };

    if sc < 0.0 {
        sc = 0.0;
        tc = (e / c).clamp(0.0, 1.0);
    }

    let p_ray = ray.origin + u * sc;
    let p_seg = seg_a + v * tc;
    Some((p_ray.distance(p_seg), sc))
}

fn distance_point_to_segment_2d(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_squared();
    if len_sq <= 1e-6 {
        return p.distance(a);
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

fn pick_closest_2d(candidates: &[(GizmoHandle, f32)], max_dist: f32) -> Option<GizmoHandle> {
    candidates
        .iter()
        .copied()
        .filter(|(_, d)| *d <= max_dist)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(h, _)| h)
}

fn pick_nearer(
    current: Option<(f32, GizmoHandle)>,
    t_ray: f32,
    handle: GizmoHandle,
) -> Option<(f32, GizmoHandle)> {
    match current {
        Some((best_t, _)) if best_t <= t_ray => current,
        _ => Some((t_ray, handle)),
    }
}

fn orthonormal_basis_for_normal(normal: Vec3) -> (Vec3, Vec3) {
    let n = normal.normalize_or(Vec3::Z);
    let helper = if n.abs_diff_eq(Vec3::Y, 0.01) {
        Vec3::X
    } else {
        Vec3::Y
    };
    let u = n.cross(helper).normalize();
    let v = n.cross(u).normalize();
    (u, v)
}

/// Helper converting Euler rotation `(rx, ry, rz)` degrees safely in XYZ order.
#[must_use]
pub fn quat_from_euler_deg(x: f32, y: f32, z: f32) -> Quat {
    Quat::from_euler(EulerRot::XYZ, x.to_radians(), y.to_radians(), z.to_radians())
}

fn draw_plane_square_3d(
    gizmos: &mut Gizmos<EditorOverlayGizmoGroup>,
    pivot: Vec3,
    u_axis: Vec3,
    v_axis: Vec3,
    scale: f32,
    color: Color,
) {
    let a = scale * 0.22;
    let b = scale * 0.46;
    let p0 = pivot + u_axis * a + v_axis * a;
    let p1 = pivot + u_axis * b + v_axis * a;
    let p2 = pivot + u_axis * b + v_axis * b;
    let p3 = pivot + u_axis * a + v_axis * b;
    gizmos.line(p0, p1, color);
    gizmos.line(p1, p2, color);
    gizmos.line(p2, p3, color);
    gizmos.line(p3, p0, color);
}

fn draw_wire_cube(
    gizmos: &mut Gizmos<EditorOverlayGizmoGroup>,
    center: Vec3,
    rot: Quat,
    half: f32,
    color: Color,
) {
    let mat = Mat3::from_quat(rot);
    let offsets = [
        Vec3::new(-half, -half, -half),
        Vec3::new(half, -half, -half),
        Vec3::new(half, half, -half),
        Vec3::new(-half, half, -half),
        Vec3::new(-half, -half, half),
        Vec3::new(half, -half, half),
        Vec3::new(half, half, half),
        Vec3::new(-half, half, half),
    ];
    let corners: [Vec3; 8] = core::array::from_fn(|i| center + mat * offsets[i]);
    let edges = [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];
    for (i, j) in edges {
        gizmos.line(corners[i], corners[j], color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_camera::{OrthographicProjection, PerspectiveProjection, RenderTargetInfo};
    use engine_protocol::InputModifiers;

    fn make_3d_camera(cam_pos: Vec3, target: Vec3) -> (Camera, Projection, GlobalTransform) {
        let mut proj = Projection::Perspective(PerspectiveProjection {
            fov: 45.0_f32.to_radians(),
            ..Default::default()
        });
        proj.update(800.0, 600.0);
        let mut cam = Camera::default();
        cam.computed.target_info = Some(RenderTargetInfo {
            physical_size: bevy_math::UVec2::new(800, 600),
            scale_factor: 1.0,
        });
        cam.computed.clip_from_view = proj.get_clip_from_view();
        let cam_tfm = GlobalTransform::from(Transform::from_translation(cam_pos).looking_at(target, Vec3::Y));
        (cam, proj, cam_tfm)
    }

    fn make_2d_camera() -> (Camera, Projection, GlobalTransform) {
        let mut proj = Projection::Orthographic(OrthographicProjection::default_2d());
        proj.update(800.0, 600.0);
        let mut cam = Camera::default();
        cam.computed.target_info = Some(RenderTargetInfo {
            physical_size: bevy_math::UVec2::new(800, 600),
            scale_factor: 1.0,
        });
        cam.computed.clip_from_view = proj.get_clip_from_view();
        let cam_tfm = GlobalTransform::IDENTITY;
        (cam, proj, cam_tfm)
    }

    #[test]
    fn translate_3d_axis_plane_snap_local_and_parent_hierarchy() {
        let (cam, proj, cam_tfm) = make_3d_camera(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);

        // Parent rotated 90 degrees around Y: world +X maps to local +Z!
        let parent_global = GlobalTransform::from(
            Transform::IDENTITY.with_rotation(Quat::from_rotation_y(core::f32::consts::FRAC_PI_2)),
        );
        let child_local = Transform::from_xyz(0.0, 0.0, 0.0);
        let child_global = parent_global.mul_transform(child_local);
        let entity = Entity::from_raw_u32(7).unwrap();
        let guid = EntityGuid::new();

        let targets = [GizmoTargetSnapshot {
            guid,
            entity,
            local_transform: child_local,
            global_transform: child_global,
            parent_global: Some(parent_global),
        }];

        let mut gizmo = TransformGizmoState {
            mode: GizmoMode::Translate,
            space: GizmoSpace::World,
            snap: GizmoSnapConfig {
                enabled: true,
                translate_step: 0.5,
                ..Default::default()
            },
            ..Default::default()
        };

        let scale = compute_gizmo_scale(ViewportCameraMode::Mode3D, &proj, cam_tfm.translation(), Vec3::ZERO);
        let handle_world_pt = Vec3::new(scale * 0.7, 0.0, 0.0);
        let press_px = cam.world_to_viewport(&cam_tfm, handle_world_pt).unwrap();
        let press_ray = cam.viewport_to_world(&cam_tfm, press_px).unwrap();

        let press_ptr = EditorPointerState {
            position: Some(press_px),
            ray: Some(press_ray),
            left_down: true,
            left_just_pressed: true,
            ..Default::default()
        };
        gizmo.update_interaction(
            ViewportCameraMode::Mode3D,
            Some((&cam, &proj, &cam_tfm)),
            &press_ptr,
            &targets,
        );
        assert!(gizmo.is_dragging());
        assert!(gizmo.consumed_pointer_click);
        assert_eq!(gizmo.active_handle(), Some(GizmoHandle::AxisX));

        // Drag +1.6 in world +X -> snapped to +1.5 in world +X -> local +Z = -1.5 (wait: +Y rot 90 maps local +Z to +X).
        let drag_world_pt = handle_world_pt + Vec3::new(1.6, 0.0, 0.0);
        let drag_px = cam.world_to_viewport(&cam_tfm, drag_world_pt).unwrap();
        let drag_ray = cam.viewport_to_world(&cam_tfm, drag_px).unwrap();
        let drag_ptr = EditorPointerState {
            position: Some(drag_px),
            ray: Some(drag_ray),
            left_down: true,
            ..Default::default()
        };
        let updates = gizmo.update_interaction(
            ViewportCameraMode::Mode3D,
            Some((&cam, &proj, &cam_tfm)),
            &drag_ptr,
            &targets,
        );
        assert_eq!(updates.len(), 1);
        let updated_local = updates[0].1;
        let updated_world = parent_global.transform_point(updated_local.translation);
        assert!((updated_world.x - 1.5).abs() < 0.02, "world x = {}", updated_world.x);
        assert!(updated_world.y.abs() < 0.02);
        assert!(updated_world.z.abs() < 0.02);

        // Release finishes drag and queues one Undo commit.
        let release_ptr = EditorPointerState {
            position: Some(drag_px),
            ray: Some(drag_ray),
            left_down: false,
            left_just_released: true,
            ..Default::default()
        };
        gizmo.update_interaction(
            ViewportCameraMode::Mode3D,
            Some((&cam, &proj, &cam_tfm)),
            &release_ptr,
            &targets,
        );
        assert!(!gizmo.is_dragging());
        let commits = gizmo.drain_commits();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].label, "Translate");
        assert_eq!(commits[0].entries.len(), 1);
    }

    #[test]
    fn rotate_and_scale_3d_with_snap() {
        let (cam, proj, cam_tfm) = make_3d_camera(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);
        let entity = Entity::from_raw_u32(8).unwrap();
        let guid = EntityGuid::new();
        let targets = [GizmoTargetSnapshot {
            guid,
            entity,
            local_transform: Transform::IDENTITY,
            global_transform: GlobalTransform::IDENTITY,
            parent_global: None,
        }];

        // Rotate Z by ~90 deg with 15-deg snapping.
        let mut gizmo = TransformGizmoState {
            mode: GizmoMode::Rotate,
            space: GizmoSpace::World,
            snap: GizmoSnapConfig {
                enabled: true,
                rotate_step_degrees: 15.0,
                scale_step: 0.25,
                ..Default::default()
            },
            ..Default::default()
        };
        let scale = compute_gizmo_scale(ViewportCameraMode::Mode3D, &proj, cam_tfm.translation(), Vec3::ZERO);
        let ring_r = scale * 0.95;
        let press_px = cam.world_to_viewport(&cam_tfm, Vec3::new(ring_r, 0.0, 0.0)).unwrap();
        let drag_px = cam.world_to_viewport(&cam_tfm, Vec3::new(0.0, ring_r, 0.0)).unwrap();

        gizmo.update_interaction(
            ViewportCameraMode::Mode3D,
            Some((&cam, &proj, &cam_tfm)),
            &EditorPointerState {
                position: Some(press_px),
                ray: Some(cam.viewport_to_world(&cam_tfm, press_px).unwrap()),
                left_down: true,
                left_just_pressed: true,
                ..Default::default()
            },
            &targets,
        );
        assert_eq!(gizmo.active_handle(), Some(GizmoHandle::AxisZ));

        let updates = gizmo.update_interaction(
            ViewportCameraMode::Mode3D,
            Some((&cam, &proj, &cam_tfm)),
            &EditorPointerState {
                position: Some(drag_px),
                ray: Some(cam.viewport_to_world(&cam_tfm, drag_px).unwrap()),
                left_down: true,
                ..Default::default()
            },
            &targets,
        );
        let angle = updates[0].1.rotation.to_euler(EulerRot::XYZ).2.to_degrees();
        assert!((angle - 90.0).abs() < 1.0, "expected 90 deg rotation, got {angle}");

        // Scale uniform from center handle.
        let mut scale_gizmo = TransformGizmoState {
            mode: GizmoMode::Scale,
            snap: gizmo.snap,
            ..Default::default()
        };
        let center_px = cam.world_to_viewport(&cam_tfm, Vec3::ZERO).unwrap();
        scale_gizmo.update_interaction(
            ViewportCameraMode::Mode3D,
            Some((&cam, &proj, &cam_tfm)),
            &EditorPointerState {
                position: Some(center_px),
                ray: Some(cam.viewport_to_world(&cam_tfm, center_px).unwrap()),
                left_down: true,
                left_just_pressed: true,
                ..Default::default()
            },
            &targets,
        );
        assert_eq!(scale_gizmo.active_handle(), Some(GizmoHandle::CenterUniform));
        let updates = scale_gizmo.update_interaction(
            ViewportCameraMode::Mode3D,
            Some((&cam, &proj, &cam_tfm)),
            &EditorPointerState {
                position: Some(center_px + Vec2::new(60.0, 0.0)),
                ray: Some(cam.viewport_to_world(&cam_tfm, center_px + Vec2::new(60.0, 0.0)).unwrap()),
                left_down: true,
                ..Default::default()
            },
            &targets,
        );
        assert!((updates[0].1.scale.x - 1.5).abs() < 1e-3);
        assert_eq!(updates[0].1.scale, Vec3::splat(updates[0].1.scale.x));
    }

    #[test]
    fn gizmo_2d_translate_rotate_scale_and_shortcuts_respect_rmb() {
        let (cam, proj, cam_tfm) = make_2d_camera();
        let entity = Entity::from_raw_u32(9).unwrap();
        let guid = EntityGuid::new();
        let targets = [GizmoTargetSnapshot {
            guid,
            entity,
            local_transform: Transform::IDENTITY,
            global_transform: GlobalTransform::IDENTITY,
            parent_global: None,
        }];

        let mut gizmo = TransformGizmoState {
            snap: GizmoSnapConfig {
                enabled: true,
                translate_step_2d: 16.0,
                rotate_step_degrees: 45.0,
                scale_step: 0.5,
                ..Default::default()
            },
            ..Default::default()
        };

        // 1. KeyE while RMB is held MUST NOT change mode; without RMB it switches to Rotate!
        gizmo.apply_shortcuts(
            &[ViewportInputEvent::Key {
                code: "KeyE".to_owned(),
                pressed: true,
                modifiers: InputModifiers::default(),
            }],
            None,
            true,
            false,
        );
        assert_eq!(gizmo.mode, GizmoMode::Translate);

        // 2. 2D PlaneXY free translate + snap (16px).
        let scale = compute_gizmo_scale(ViewportCameraMode::Mode2D, &proj, Vec3::ZERO, Vec3::ZERO);
        let plane_pt = Vec3::new(scale * 0.35, scale * 0.35, 0.0);
        let press_px = cam.world_to_viewport(&cam_tfm, plane_pt).unwrap();
        let drag_px = cam
            .world_to_viewport(&cam_tfm, plane_pt + Vec3::new(35.0, -14.0, 0.0))
            .unwrap();

        gizmo.update_interaction(
            ViewportCameraMode::Mode2D,
            Some((&cam, &proj, &cam_tfm)),
            &EditorPointerState {
                position: Some(press_px),
                ray: Some(cam.viewport_to_world(&cam_tfm, press_px).unwrap()),
                left_down: true,
                left_just_pressed: true,
                ..Default::default()
            },
            &targets,
        );
        assert_eq!(gizmo.active_handle(), Some(GizmoHandle::PlaneXY));

        let updates = gizmo.update_interaction(
            ViewportCameraMode::Mode2D,
            Some((&cam, &proj, &cam_tfm)),
            &EditorPointerState {
                position: Some(drag_px),
                ray: Some(cam.viewport_to_world(&cam_tfm, drag_px).unwrap()),
                left_down: true,
                ..Default::default()
            },
            &targets,
        );
        assert!((updates[0].1.translation.x - 32.0).abs() < 0.1);
        assert!((updates[0].1.translation.y - (-16.0)).abs() < 0.1);

        // Release first, then switch mode with KeyE cleanly.
        gizmo.update_interaction(
            ViewportCameraMode::Mode2D,
            Some((&cam, &proj, &cam_tfm)),
            &EditorPointerState {
                position: Some(drag_px),
                ray: Some(cam.viewport_to_world(&cam_tfm, drag_px).unwrap()),
                left_down: false,
                left_just_released: true,
                ..Default::default()
            },
            &targets,
        );
        gizmo.apply_shortcuts(
            &[ViewportInputEvent::Key {
                code: "KeyE".to_owned(),
                pressed: true,
                modifiers: InputModifiers::default(),
            }],
            None,
            false,
            false,
        );
        assert_eq!(gizmo.mode, GizmoMode::Rotate);

        // 3. 2D Scale preserves Z scale == 1.0!
        gizmo.apply_shortcuts(
            &[ViewportInputEvent::Key {
                code: "KeyR".to_owned(),
                pressed: true,
                modifiers: InputModifiers::default(),
            }],
            None,
            false,
            false,
        );
        assert_eq!(gizmo.mode, GizmoMode::Scale);
        let center_px = cam.world_to_viewport(&cam_tfm, Vec3::ZERO).unwrap();
        gizmo.update_interaction(
            ViewportCameraMode::Mode2D,
            Some((&cam, &proj, &cam_tfm)),
            &EditorPointerState {
                position: Some(center_px),
                ray: Some(cam.viewport_to_world(&cam_tfm, center_px).unwrap()),
                left_down: true,
                left_just_pressed: true,
                ..Default::default()
            },
            &targets,
        );
        let updates = gizmo.update_interaction(
            ViewportCameraMode::Mode2D,
            Some((&cam, &proj, &cam_tfm)),
            &EditorPointerState {
                position: Some(center_px + Vec2::new(60.0, 0.0)),
                ray: Some(cam.viewport_to_world(&cam_tfm, center_px + Vec2::new(60.0, 0.0)).unwrap()),
                left_down: true,
                ..Default::default()
            },
            &targets,
        );
        assert!((updates[0].1.scale.x - 1.5).abs() < 1e-3);
        assert!((updates[0].1.scale.y - 1.5).abs() < 1e-3);
        assert!((updates[0].1.scale.z - 1.0).abs() < 1e-6);
    }
}
