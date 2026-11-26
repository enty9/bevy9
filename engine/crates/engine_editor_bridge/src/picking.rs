//! Viewport 2D/3D picking, light/camera helper icons, and selection outline rendering.

use bevy_camera::{
    visibility::{InheritedVisibility, Visibility},
    Camera, Projection,
};
use bevy_color::Color;
use bevy_ecs::{
    entity::Entity,
    hierarchy::ChildOf,
    message::MessageWriter,
    query::{Has, With},
    resource::Resource,
    system::{Query, Res, ResMut},
};
use bevy_gizmos::gizmos::Gizmos;
use bevy_math::{Isometry2d, Isometry3d, Quat, Vec2, Vec3, Vec3Swizzles};
use bevy_picking::{
    backend::{ray::RayMap, HitData, PointerHits},
    hover::HoverMap,
    pointer::PointerId,
    Pickable,
};
use bevy_transform::components::GlobalTransform;
use engine_core::{EntityGuid, EntityGuidMap};
use engine_protocol::ViewportCameraMode;
use engine_render::{
    picking::{EditorOverlayGizmoGroup, EditorPointerState, OFFSCREEN_POINTER_ID},
    ViewportConfig,
};
use engine_scene::{
    MeshPrimitive2d, MeshPrimitive3d, SceneCamera2d, SceneCamera3d, SceneDirectionalLight,
    SceneEntity, ScenePointLight, SpriteDesc,
};

use crate::selection::{BridgeDirtyFlags, EditorSelection};

/// Currently hovered [`SceneEntity`] GUID under the active viewport cursor.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorHoveredEntity {
    /// Persistent GUID of the hovered scene entity, if any.
    pub guid: Option<EntityGuid>,
}

/// Walks ancestor links from a hit entity up to the owning [`SceneEntity`] + [`EntityGuid`].
#[must_use]
pub fn resolve_scene_entity(
    start: Entity,
    scene_entities: &Query<&EntityGuid, With<SceneEntity>>,
    parents: &Query<&ChildOf>,
) -> Option<(Entity, EntityGuid)> {
    let mut current = start;
    for _ in 0..16 {
        if let Ok(&guid) = scene_entities.get(current) {
            return Some((current, guid));
        }
        current = parents.get(current).ok()?.parent();
    }
    None
}

fn is_entity_hierarchy_visible(
    start: Entity,
    visibility: Option<&Visibility>,
    inherited: Option<&InheritedVisibility>,
    vis_query: &Query<(&Visibility, Option<&ChildOf>)>,
) -> bool {
    if visibility.is_some_and(|v| *v == Visibility::Hidden) {
        return false;
    }
    if let Ok((vis, mut parent_opt)) = vis_query.get(start) {
        if *vis == Visibility::Hidden {
            return false;
        }
        let mut hops = 0usize;
        while let Some(child_of) = parent_opt {
            if hops >= 16 {
                break;
            }
            hops += 1;
            let parent = child_of.parent();
            let Ok((p_vis, next_parent)) = vis_query.get(parent) else {
                break;
            };
            if *p_vis == Visibility::Hidden {
                return false;
            }
            parent_opt = next_parent;
        }
    }
    // Only trust `InheritedVisibility` if `Visibility::Inherited` isn't contradicted by an un-propagated test frame.
    let _ = inherited;
    true
}

type HelperEntityItem<'a> = (
    Entity,
    &'a GlobalTransform,
    Option<&'a Visibility>,
    Option<&'a InheritedVisibility>,
    Option<&'a Pickable>,
    Has<SceneDirectionalLight>,
    Has<ScenePointLight>,
    Has<SceneCamera3d>,
    Has<SceneCamera2d>,
);

/// Picking backend for non-mesh editor helper icons (`DirectionalLight`, `PointLight`, `Camera3d`, `Camera2d`).
pub(crate) fn helper_icon_picking_backend(
    ray_map: Res<RayMap>,
    cameras: Query<(&Camera, &Projection, &GlobalTransform)>,
    helpers: Query<HelperEntityItem<'_>, With<SceneEntity>>,
    vis_query: Query<(&Visibility, Option<&ChildOf>)>,
    mut pointer_hits: MessageWriter<PointerHits>,
) {
    for (&ray_id, &ray) in ray_map.iter() {
        let Ok((camera, projection, cam_tfm)) = cameras.get(ray_id.camera) else {
            continue;
        };
        if !camera.is_active {
            continue;
        }

        let cam_pos = cam_tfm.translation();
        let mut picks: Vec<(Entity, HitData)> = Vec::new();

        for (
            entity,
            global_tfm,
            vis,
            inherited,
            pickable,
            has_dir_light,
            has_point_light,
            has_cam3d,
            has_cam2d,
        ) in &helpers
        {
            if !has_dir_light && !has_point_light && !has_cam3d && !has_cam2d {
                continue;
            }
            if pickable.is_some_and(|p| !p.is_hoverable) {
                continue;
            }
            if !is_entity_hierarchy_visible(entity, vis, inherited, &vis_query) {
                continue;
            }

            let center = global_tfm.translation();
            match projection {
                Projection::Perspective(_) => {
                    let dist = center.distance(cam_pos).max(0.5);
                    let radius = (dist * 0.045).clamp(0.18, 1.5);
                    if let Some(t) = ray_sphere_intersect(ray.origin, *ray.direction, center, radius)
                    {
                        let hit_pos = ray.origin + *ray.direction * t;
                        picks.push((
                            entity,
                            HitData::new(ray_id.camera, t, Some(hit_pos), Some(-*ray.direction)),
                        ));
                    }
                }
                Projection::Orthographic(ortho) => {
                    if !has_cam2d && !has_dir_light && !has_point_light {
                        continue;
                    }
                    let dir_z = ray.direction.z;
                    if dir_z.abs() <= 1e-5 {
                        continue;
                    }
                    let t = (center.z - ray.origin.z) / dir_z;
                    if t < 0.0 {
                        continue;
                    }
                    let hit_pos = ray.origin + *ray.direction * t;
                    let half = (20.0 * ortho.scale).clamp(6.0, 200.0);
                    if (hit_pos.x - center.x).abs() <= half && (hit_pos.y - center.y).abs() <= half {
                        picks.push((
                            entity,
                            HitData::new(ray_id.camera, t, Some(hit_pos), Some(Vec3::Z)),
                        ));
                    }
                }
                Projection::Custom(_) => {}
            }
        }

        if !picks.is_empty() {
            pointer_hits.write(PointerHits::new(
                ray_id.pointer,
                picks,
                camera.order as f32 + 0.05,
            ));
        }
    }
}

/// Resolves hovered scene entities from `HoverMap` and updates [`EditorSelection`] on clean clicks.
#[expect(
    clippy::too_many_arguments,
    reason = "Synchronizes HoverMap, pointer clicks, EntityGuidMap pruning, and dirty IPC flags"
)]
pub(crate) fn handle_viewport_selection_picking(
    guid_map: Option<Res<EntityGuidMap>>,
    pointer_state: Res<EditorPointerState>,
    hover_map: Option<Res<HoverMap>>,
    mut selection: ResMut<EditorSelection>,
    mut hovered: ResMut<EditorHoveredEntity>,
    mut dirty: ResMut<BridgeDirtyFlags>,
    scene_entities: Query<&EntityGuid, With<SceneEntity>>,
    parents: Query<&ChildOf>,
) {
    if let Some(map) = guid_map.as_deref() {
        let valid: Vec<EntityGuid> = selection
            .guids()
            .iter()
            .copied()
            .filter(|guid| map.contains_guid(*guid))
            .collect();
        if valid.len() != selection.guids().len() && selection.set(valid) {
            dirty.selection_dirty = true;
        }
    }

    let mut top_hovered: Option<(f32, EntityGuid)> = None;
    if let Some(hover_map_res) = hover_map.as_deref() {
        let candidates = [
            pointer_state.pointer_id,
            Some(OFFSCREEN_POINTER_ID),
            Some(PointerId::Mouse),
        ];
        for pointer_id in candidates.into_iter().flatten() {
            let Some(hits) = hover_map_res.get(&pointer_id) else {
                continue;
            };
            for (&hit_entity, hit_data) in hits {
                if let Some((_, guid)) = resolve_scene_entity(hit_entity, &scene_entities, &parents)
                {
                    let replace = top_hovered
                        .is_none_or(|(best_depth, _)| hit_data.depth < best_depth);
                    if replace {
                        top_hovered = Some((hit_data.depth, guid));
                    }
                }
            }
            if top_hovered.is_some() {
                break;
            }
        }
    }

    let hovered_guid = top_hovered.map(|(_, guid)| guid);
    hovered.guid = hovered_guid;

    let clean_left_click = pointer_state.left_just_released
        && !pointer_state.right_down
        && !pointer_state.middle_down
        && !pointer_state.alt_down
        && pointer_state.max_drag_distance <= 5.0;

    if !clean_left_click {
        return;
    }

    let multi_select = pointer_state.ctrl_down || pointer_state.shift_down;
    let changed = match (hovered_guid, multi_select) {
        (Some(guid), true) => {
            let mut next = selection.guids().to_vec();
            if let Some(existing_idx) = next.iter().position(|g| *g == guid) {
                next.remove(existing_idx);
            } else {
                next.push(guid);
            }
            selection.set(next)
        }
        (Some(guid), false) => selection.set(vec![guid]),
        (None, true) => false,
        (None, false) => selection.clear(),
    };

    if changed {
        dirty.selection_dirty = true;
    }
}

type SceneOverlayItem<'a> = (
    Entity,
    &'a EntityGuid,
    &'a GlobalTransform,
    Option<&'a Visibility>,
    Option<&'a MeshPrimitive3d>,
    Option<&'a MeshPrimitive2d>,
    Option<&'a SpriteDesc>,
    Option<&'a SceneDirectionalLight>,
    Option<&'a ScenePointLight>,
    Option<&'a SceneCamera3d>,
    Option<&'a SceneCamera2d>,
);

/// Draws 2D/3D helper icons (lights/cameras) and selection/hover wireframe outlines.
pub(crate) fn draw_editor_helper_icons_and_selection(
    config: Res<ViewportConfig>,
    selection: Res<EditorSelection>,
    hovered: Res<EditorHoveredEntity>,
    mut gizmos: Gizmos<EditorOverlayGizmoGroup>,
    entities: Query<SceneOverlayItem<'_>, With<SceneEntity>>,
    vis_query: Query<(&Visibility, Option<&ChildOf>)>,
    cam3d_query: Query<&GlobalTransform, With<engine_render::EditorCamera3d>>,
) {
    let selected_color = Color::srgb(1.0, 0.62, 0.12);
    let hover_color = Color::srgba(0.45, 0.85, 1.0, 0.85);
    let cam3d_pos = cam3d_query.iter().next().map(GlobalTransform::translation);

    for (
        entity,
        guid,
        global_tfm,
        vis,
        mesh3d,
        mesh2d,
        sprite_desc,
        dir_light,
        point_light,
        cam3d,
        cam2d,
    ) in &entities
    {
        if !is_entity_hierarchy_visible(entity, vis, None, &vis_query) {
            continue;
        }

        let is_selected = selection.guids().contains(guid);
        let is_hovered = !is_selected && hovered.guid == Some(*guid);
        let center = global_tfm.translation();
        let (_, rot, _) = global_tfm.to_scale_rotation_translation();

        let icon_scale_3d = cam3d_pos
            .map(|c| (center.distance(c) * 0.035).clamp(0.16, 1.2))
            .unwrap_or(0.35);

        match config.camera_mode {
            ViewportCameraMode::Mode3D => {
                if dir_light.is_some() {
                    let color = if is_selected {
                        selected_color
                    } else {
                        Color::srgb(1.0, 0.88, 0.28)
                    };
                    gizmos.circle(
                        Isometry3d::new(center, rot),
                        icon_scale_3d * 0.85,
                        color,
                    );
                    let forward = rot * Vec3::NEG_Z;
                    let right = rot * Vec3::X;
                    let up = rot * Vec3::Y;
                    gizmos.arrow(center, center + forward * (icon_scale_3d * 2.2), color);
                    for offset in [
                        right * icon_scale_3d * 0.6,
                        -right * icon_scale_3d * 0.6,
                        up * icon_scale_3d * 0.6,
                        -up * icon_scale_3d * 0.6,
                    ] {
                        gizmos.line(
                            center + offset,
                            center + offset + forward * (icon_scale_3d * 1.4),
                            color,
                        );
                    }
                } else if point_light.is_some() {
                    let color = if is_selected {
                        selected_color
                    } else {
                        Color::srgb(1.0, 0.78, 0.22)
                    };
                    let r = icon_scale_3d * 0.7;
                    gizmos.circle(Isometry3d::new(center, rot), r, color);
                    gizmos.circle(
                        Isometry3d::new(center, rot * Quat::from_rotation_x(core::f32::consts::FRAC_PI_2)),
                        r,
                        color,
                    );
                    gizmos.circle(
                        Isometry3d::new(center, rot * Quat::from_rotation_y(core::f32::consts::FRAC_PI_2)),
                        r,
                        color,
                    );
                } else if cam3d.is_some() {
                    let color = if is_selected {
                        selected_color
                    } else {
                        Color::srgb(0.45, 0.82, 1.0)
                    };
                    draw_camera_3d_frustum(&mut gizmos, center, rot, icon_scale_3d, color);
                } else if cam2d.is_some() {
                    let color = if is_selected {
                        selected_color
                    } else {
                        Color::srgb(0.45, 0.82, 1.0)
                    };
                    draw_camera_3d_frustum(&mut gizmos, center, rot, icon_scale_3d * 0.85, color);
                }

                if is_selected || is_hovered {
                    let outline_color = if is_selected { selected_color } else { hover_color };
                    let half = entity_local_half_extents_3d(
                        mesh3d,
                        mesh2d,
                        sprite_desc,
                        dir_light.is_some()
                            || point_light.is_some()
                            || cam3d.is_some()
                            || cam2d.is_some(),
                        icon_scale_3d,
                    );
                    draw_oriented_box_3d(&mut gizmos, global_tfm, half, outline_color);
                }
            }
            ViewportCameraMode::Mode2D => {
                if let Some(c2d) = cam2d {
                    let color = if is_selected {
                        selected_color
                    } else {
                        Color::srgb(0.45, 0.82, 1.0)
                    };
                    let angle = rot.to_euler(bevy_math::EulerRot::ZYX).0;
                    let frame_size = Vec2::new(config.width as f32, config.height as f32)
                        * c2d.scale.max(0.05);
                    gizmos.rect_2d(Isometry2d::new(center.xy(), bevy_math::Rot2::radians(angle)), frame_size, color);
                    gizmos.circle_2d(center.xy(), 14.0, color);
                } else if point_light.is_some() || dir_light.is_some() {
                    let color = if is_selected {
                        selected_color
                    } else {
                        Color::srgb(1.0, 0.84, 0.25)
                    };
                    gizmos.circle_2d(center.xy(), 16.0, color);
                    if dir_light.is_some() {
                        let dir = (rot * Vec3::NEG_Z).xy();
                        let len = dir.length();
                        let arrow_dir = if len > 1e-3 { dir / len } else { Vec2::X };
                        gizmos.arrow_2d(center.xy(), center.xy() + arrow_dir * 34.0, color);
                    }
                }

                if is_selected || is_hovered {
                    let outline_color = if is_selected { selected_color } else { hover_color };
                    let half = entity_local_half_extents_2d(
                        mesh2d,
                        sprite_desc,
                        cam2d.is_some() || dir_light.is_some() || point_light.is_some(),
                    );
                    draw_oriented_rect_2d(&mut gizmos, global_tfm, half, outline_color);
                }
            }
        }
    }
}

fn ray_sphere_intersect(origin: Vec3, dir: Vec3, center: Vec3, radius: f32) -> Option<f32> {
    let oc = origin - center;
    let b = oc.dot(dir);
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

fn entity_local_half_extents_3d(
    mesh3d: Option<&MeshPrimitive3d>,
    mesh2d: Option<&MeshPrimitive2d>,
    sprite_desc: Option<&SpriteDesc>,
    is_helper: bool,
    helper_scale: f32,
) -> Vec3 {
    if let Some(prim) = mesh3d {
        return match *prim {
            MeshPrimitive3d::Cube { size } => size * 0.5 + Vec3::splat(0.02),
            MeshPrimitive3d::Sphere { radius } => Vec3::splat(radius.max(0.01) + 0.02),
            MeshPrimitive3d::Plane { size } => Vec3::new(size.x * 0.5, 0.03, size.y * 0.5),
            MeshPrimitive3d::Cylinder { radius, height } => {
                Vec3::new(radius.max(0.01) + 0.02, height.max(0.01) * 0.5 + 0.02, radius.max(0.01) + 0.02)
            }
        };
    }
    if let Some(prim) = mesh2d {
        return match *prim {
            MeshPrimitive2d::Rectangle { size } => Vec3::new(size.x * 0.5, size.y * 0.5, 0.05),
            MeshPrimitive2d::Circle { radius } => Vec3::new(radius, radius, 0.05),
        };
    }
    if let Some(sprite) = sprite_desc {
        let size = sprite.custom_size.unwrap_or(Vec2::splat(64.0));
        return Vec3::new(size.x * 0.5, size.y * 0.5, 0.05);
    }
    if is_helper {
        return Vec3::splat(helper_scale * 1.1);
    }
    Vec3::splat(0.25)
}

fn entity_local_half_extents_2d(
    mesh2d: Option<&MeshPrimitive2d>,
    sprite_desc: Option<&SpriteDesc>,
    is_helper: bool,
) -> Vec2 {
    if let Some(prim) = mesh2d {
        return match *prim {
            MeshPrimitive2d::Rectangle { size } => size * 0.5 + Vec2::splat(3.0),
            MeshPrimitive2d::Circle { radius } => Vec2::splat(radius + 3.0),
        };
    }
    if let Some(sprite) = sprite_desc {
        let size = sprite.custom_size.unwrap_or(Vec2::splat(64.0));
        return size * 0.5 + Vec2::splat(3.0);
    }
    if is_helper {
        return Vec2::splat(22.0);
    }
    Vec2::splat(16.0)
}

fn draw_oriented_box_3d(
    gizmos: &mut Gizmos<EditorOverlayGizmoGroup>,
    tfm: &GlobalTransform,
    half: Vec3,
    color: Color,
) {
    let corners = [
        tfm.transform_point(Vec3::new(-half.x, -half.y, -half.z)),
        tfm.transform_point(Vec3::new(half.x, -half.y, -half.z)),
        tfm.transform_point(Vec3::new(half.x, half.y, -half.z)),
        tfm.transform_point(Vec3::new(-half.x, half.y, -half.z)),
        tfm.transform_point(Vec3::new(-half.x, -half.y, half.z)),
        tfm.transform_point(Vec3::new(half.x, -half.y, half.z)),
        tfm.transform_point(Vec3::new(half.x, half.y, half.z)),
        tfm.transform_point(Vec3::new(-half.x, half.y, half.z)),
    ];
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
    for (a, b) in edges {
        gizmos.line(corners[a], corners[b], color);
    }
}

fn draw_oriented_rect_2d(
    gizmos: &mut Gizmos<EditorOverlayGizmoGroup>,
    tfm: &GlobalTransform,
    half: Vec2,
    color: Color,
) {
    let p0 = tfm.transform_point(Vec3::new(-half.x, -half.y, 0.0)).xy();
    let p1 = tfm.transform_point(Vec3::new(half.x, -half.y, 0.0)).xy();
    let p2 = tfm.transform_point(Vec3::new(half.x, half.y, 0.0)).xy();
    let p3 = tfm.transform_point(Vec3::new(-half.x, half.y, 0.0)).xy();
    gizmos.linestrip_2d([p0, p1, p2, p3, p0], color);
}

fn draw_camera_3d_frustum(
    gizmos: &mut Gizmos<EditorOverlayGizmoGroup>,
    center: Vec3,
    rot: Quat,
    scale: f32,
    color: Color,
) {
    let forward = rot * Vec3::NEG_Z;
    let right = rot * Vec3::X;
    let up = rot * Vec3::Y;
    let far_center = center + forward * (scale * 2.2);
    let w = right * (scale * 1.15);
    let h = up * (scale * 0.72);
    let tl = far_center - w + h;
    let tr = far_center + w + h;
    let br = far_center + w - h;
    let bl = far_center - w - h;

    for corner in [tl, tr, br, bl] {
        gizmos.line(center, corner, color);
    }
    gizmos.line(tl, tr, color);
    gizmos.line(tr, br, color);
    gizmos.line(br, bl, color);
    gizmos.line(bl, tl, color);
    gizmos.line(tl + up * (scale * 0.25), far_center + h + up * (scale * 0.55), color);
    gizmos.line(far_center + h + up * (scale * 0.55), tr + up * (scale * 0.25), color);
}
