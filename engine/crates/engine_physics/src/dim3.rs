//! XYZ shape conversion and stair/slope policy.

use bevy_camera::Camera3d;
use bevy_math::{Isometry3d, Quat};
use rapier::{
    control::{CharacterAutostep, CharacterLength, KinematicCharacterController},
    na::{DMatrix, Quaternion, Translation3, UnitQuaternion},
};
use rapier3d as rapier;

use crate::{
    backend::{backend, finite_or},
    *,
};

backend!(
    PhysicsWorld3D,
    RigidBody3D,
    Collider3D,
    TriggerVolume3D,
    PhysicsJoint3D,
    CharacterController3D,
    CharacterMotion3D,
    D3,
    Vec3,
    1.,
    Camera3d
);

fn vector(v: Vec3) -> Vector<f32> {
    Vector::new(v.x, v.y, v.z)
}
fn point(v: Vec3) -> Point<f32> {
    Point::new(v.x, v.y, v.z)
}
fn from_vector(v: Vector<f32>) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}
fn debug_point(p: Point<f32>) -> Vec3 {
    from_vector(p.coords)
}
fn draw_extra_collider_debug(
    gizmos: &mut Gizmos<PhysicsGizmos>,
    collider: &Collider,
    color: Color,
    cam_pos: Vec3,
) {
    if let TypedShape::Ball(ball) = collider.shape().as_typed_shape() {
        let center = from_vector(collider.position().translation.vector);
        let to_cam = cam_pos - center;
        let dist = to_cam.length();
        let r = ball.radius;
        if dist > r * 1.02 && r > 0.0 {
            let normal = to_cam / dist;
            let ratio = (r / dist).min(0.999);
            let horizon_center = center + normal * (r * ratio);
            let horizon_radius = r * (1.0 - ratio * ratio).sqrt();
            let rot = Quat::from_rotation_arc(Vec3::Z, normal);
            gizmos
                .circle(Isometry3d::new(horizon_center, rot), horizon_radius, color)
                .resolution(48);
        }
    }
}
fn dimensions(v: Vec3) -> Vec3 {
    v
}
fn gravity_vector(g: &Gravity) -> Vec3 {
    g.xyz
}
fn pose(t: Transform) -> Isometry<f32> {
    let q = t.rotation.normalize();
    Isometry::from_parts(
        Translation3::from(vector(t.translation)),
        UnitQuaternion::new_unchecked(Quaternion::new(q.w, q.x, q.y, q.z)),
    )
}
fn apply_pose(p: Isometry<f32>, mut t: Transform) -> Transform {
    let q = p.rotation.quaternion();
    t.translation = from_vector(p.translation.vector);
    t.rotation = Quat::from_xyzw(q.i, q.j, q.k, q.w);
    t
}

#[derive(bevy_ecs::system::SystemParam)]
struct Extras<'w, 's>(std::marker::PhantomData<(&'w (), &'s ())>);

impl Extras<'_, '_> {
    fn prepare(&mut self) {}
    fn asset_version(&self, _: &Collider3D) -> Option<(String, f32)> {
        None
    }
    fn character_filter(
        &self,
        _: &CharacterController3D,
        _: &Collider,
        _: Vec3,
        _: &Collider,
    ) -> bool {
        true
    }
}

fn shape_builder(
    shape: &Collider3D,
    scale: Vec3,
    _: Option<&Extras<'_, '_>>,
) -> Option<ColliderBuilder> {
    let sizes = scale.abs();
    let size = |n: f32| (n > 0. && n.is_finite()).then_some(n);
    let points = |vertices: &[Vec3]| -> Option<Vec<_>> {
        vertices
            .iter()
            .map(|&v| {
                let v = v * scale;
                v.is_finite().then(|| point(v))
            })
            .collect()
    };
    match shape {
        Collider3D::Cuboid(half) => Some(ColliderBuilder::cuboid(
            size(half.x * sizes.x)?,
            size(half.y * sizes.y)?,
            size(half.z * sizes.z)?,
        )),
        Collider3D::Ball(r) => Some(ColliderBuilder::ball(size(r * sizes.max_element())?)),
        Collider3D::Capsule(h, r) => Some(ColliderBuilder::capsule_y(
            size(h * sizes.y)?,
            size(r * sizes.x.max(sizes.z))?,
        )),
        Collider3D::Cylinder(h, r) => Some(ColliderBuilder::cylinder(
            size(h * sizes.y)?,
            size(r * sizes.x.max(sizes.z))?,
        )),
        Collider3D::ConvexHull(vertices) if vertices.len() >= 4 => {
            ColliderBuilder::convex_hull(&points(vertices)?)
        }
        Collider3D::TriMesh { vertices, indices }
            if !indices.is_empty()
                && indices
                    .iter()
                    .flatten()
                    .all(|&i| (i as usize) < vertices.len()) =>
        {
            ColliderBuilder::trimesh(points(vertices)?, indices.clone()).ok()
        }
        Collider3D::Heightfield {
            rows,
            columns,
            heights,
            scale: extent,
        } => {
            let (rows, columns) = (*rows as usize, *columns as usize);
            if rows < 2
                || columns < 2
                || rows.checked_mul(columns)? != heights.len()
                || !heights.iter().all(|v| v.is_finite())
            {
                return None;
            }
            let extent = *extent * sizes;
            Some(ColliderBuilder::heightfield(
                DMatrix::from_row_slice(rows, columns, heights),
                Vector::new(size(extent.x)?, size(extent.y)?, size(extent.z)?),
            ))
        }
        _ => None,
    }
}

fn make_joint(joint: &PhysicsJoint3D) -> GenericJoint {
    let (a, b) = (
        point(safe_vector(joint.anchor)),
        point(safe_vector(joint.target_anchor)),
    );
    match joint.kind {
        JointKind::Fixed => FixedJointBuilder::new()
            .local_anchor1(a)
            .local_anchor2(b)
            .into(),
        JointKind::Revolute => RevoluteJointBuilder::new(Vector::y_axis())
            .local_anchor1(a)
            .local_anchor2(b)
            .into(),
    }
}

fn make_controller(controller: &CharacterController3D) -> KinematicCharacterController {
    let angle = finite_or(controller.slope_degrees, 45.)
        .clamp(0., 89.9)
        .to_radians();
    KinematicCharacterController {
        max_slope_climb_angle: angle,
        min_slope_slide_angle: angle,
        autostep: (controller.step_height.is_finite() && controller.step_height > 0.).then_some(
            CharacterAutostep {
                max_height: CharacterLength::Absolute(controller.step_height),
                include_dynamic_bodies: false,
                ..Default::default()
            },
        ),
        snap_to_ground: (controller.ground_snap.is_finite() && controller.ground_snap > 0.)
            .then_some(CharacterLength::Absolute(controller.ground_snap)),
        ..Default::default()
    }
}

fn desired_motion(
    _: &CharacterController3D,
    motion: &mut CharacterMotion3D,
    _: Vec3,
    dt: f32,
) -> Vec3 {
    safe_vector(motion.velocity * dt)
}
