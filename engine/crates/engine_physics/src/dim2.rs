//! XY shape conversion and platformer policies.

use bevy_camera::Camera2d;
use bevy_math::{EulerRot, Quat};
use engine_assets::{load_tilemap_from_file, AssetRegistry, TilemapAsset};
use rapier::control::{CharacterLength, KinematicCharacterController};
use rapier2d as rapier;

use crate::{
    backend::{backend, finite_or},
    *,
};

backend!(
    PhysicsWorld2D,
    RigidBody2D,
    Collider2D,
    TriggerVolume2D,
    PhysicsJoint2D,
    CharacterController2D,
    CharacterMotion2D,
    D2,
    Vec2,
    100.,
    Camera2d
);

fn vector(v: Vec2) -> Vector<f32> {
    Vector::new(v.x, v.y)
}
fn point(v: Vec2) -> Point<f32> {
    Point::new(v.x, v.y)
}
fn from_vector(v: Vector<f32>) -> Vec2 {
    Vec2::new(v.x, v.y)
}
fn debug_point(p: Point<f32>) -> Vec3 {
    Vec3::new(p.x, p.y, 0.)
}
fn draw_extra_collider_debug(_: &mut Gizmos<PhysicsGizmos>, _: &Collider, _: Color, _: Vec3) {}
fn dimensions(v: Vec3) -> Vec2 {
    v.truncate()
}
fn gravity_vector(g: &Gravity) -> Vec2 {
    g.xy
}
fn pose(t: Transform) -> Isometry<f32> {
    Isometry::new(
        vector(t.translation.truncate()),
        t.rotation.normalize().to_euler(EulerRot::ZYX).0,
    )
}
fn apply_pose(p: Isometry<f32>, mut t: Transform) -> Transform {
    t.translation = from_vector(p.translation.vector).extend(t.translation.z);
    t.rotation = Quat::from_rotation_z(p.rotation.angle());
    t
}

#[derive(Resource, Default)]
pub(crate) struct TilemapShapes(BTreeMap<AssetUuid, (String, Option<TilemapAsset>)>);

#[derive(bevy_ecs::system::SystemParam)]
struct Extras<'w, 's> {
    registry: Option<Res<'w, AssetRegistry>>,
    tilemaps: ResMut<'w, TilemapShapes>,
    authored: Query<'w, 's, &'static Collider2D>,
    platforms: Query<'w, 's, (), With<OneWayPlatform2D>>,
}

impl Extras<'_, '_> {
    fn prepare(&mut self) {
        let used: BTreeSet<_> = self
            .authored
            .iter()
            .filter_map(|shape| match shape {
                Collider2D::TilemapCollider { asset, .. } => Some(*asset),
                _ => None,
            })
            .collect();
        self.tilemaps.0.retain(|uuid, _| used.contains(uuid));
        let Some(registry) = &self.registry else {
            return;
        };
        for uuid in used {
            let Some(record) = registry.get(uuid) else {
                self.tilemaps.0.remove(&uuid);
                continue;
            };
            if self
                .tilemaps
                .0
                .get(&uuid)
                .is_some_and(|(hash, _)| *hash == record.source_hash)
            {
                continue;
            }
            let asset = registry
                .resolve_abs_path(uuid)
                .and_then(|path| load_tilemap_from_file(&path).ok());
            self.tilemaps.0.insert(uuid, (record.source_hash, asset));
        }
    }

    fn asset_version(&self, collider: &Collider2D) -> Option<(String, f32)> {
        let Collider2D::TilemapCollider { asset, .. } = collider else {
            return None;
        };
        let (hash, _) = self.tilemaps.0.get(asset)?;
        Some((hash.clone(), self.tile_scale(*asset)))
    }

    fn tile_scale(&self, asset: AssetUuid) -> f32 {
        self.registry
            .as_ref()
            .and_then(|reg| reg.shared().tilemap_settings(asset))
            .map_or(1., |config| config.pixels_per_unit.max(0.01))
    }

    fn character_filter(
        &self,
        controller: &CharacterController2D,
        own: &Collider,
        desired: Vec2,
        other: &Collider,
    ) -> bool {
        let platform = Entity::try_from_bits(other.user_data as u64)
            .is_some_and(|entity| self.platforms.contains(entity));
        if !platform {
            return true;
        }
        controller.platformer
            && desired.y <= 0.
            && own.compute_aabb().mins.y >= other.compute_aabb().maxs.y - 0.1
    }
}

fn shape_builder(
    shape: &Collider2D,
    scale: Vec2,
    extras: Option<&Extras<'_, '_>>,
) -> Option<ColliderBuilder> {
    let sizes = scale.abs();
    let size = |n: f32| (n > 0. && n.is_finite()).then_some(n);
    let points = |vertices: &[Vec2]| -> Option<Vec<_>> {
        vertices
            .iter()
            .map(|&v| {
                let v = v * scale;
                v.is_finite().then(|| point(v))
            })
            .collect()
    };
    match shape {
        Collider2D::Cuboid(half) => Some(ColliderBuilder::cuboid(
            size(half.x * sizes.x)?,
            size(half.y * sizes.y)?,
        )),
        Collider2D::Ball(r) => Some(ColliderBuilder::ball(size(r * sizes.max_element())?)),
        Collider2D::Capsule(h, r) => Some(ColliderBuilder::capsule_y(
            size(h * sizes.y)?,
            size(r * sizes.x)?,
        )),
        Collider2D::Segment(a, b) if a != b => {
            let points = points(&[*a, *b])?;
            (points[0] != points[1]).then(|| ColliderBuilder::segment(points[0], points[1]))
        }
        Collider2D::ConvexPolygon(vertices) if vertices.len() >= 3 => {
            ColliderBuilder::convex_hull(&points(vertices)?)
        }
        Collider2D::Polyline(vertices) if vertices.len() >= 2 => {
            Some(ColliderBuilder::polyline(points(vertices)?, None))
        }
        Collider2D::TilemapCollider { asset, tile_scale } => {
            let extras = extras?;
            let map = extras.tilemaps.0.get(asset)?.1.as_ref()?;
            let unit = (tile_scale * extras.tile_scale(*asset)).max(0.01);
            let tile = Vec2::new(
                map.tile_size[0].max(1) as f32,
                map.tile_size[1].max(1) as f32,
            ) * unit;
            let half = tile * sizes * 0.5;
            let cell = SharedShape::cuboid(size(half.x)?, size(half.y)?);
            let mut parts = Vec::new();
            for layer in &map.layers {
                if layer.width == 0 || layer.height == 0 {
                    continue;
                }
                let solid: BTreeSet<_> = layer
                    .metadata
                    .iter()
                    .filter(|m| m.solid)
                    .map(|m| m.tile_index)
                    .collect();
                let center = Vec2::new((layer.width - 1) as f32, (layer.height - 1) as f32) * 0.5;
                for (index, value) in layer.tiles.iter().enumerate() {
                    let col = index % layer.width as usize;
                    let row = index / layer.width as usize;
                    if row >= layer.height as usize {
                        break;
                    }
                    if value.as_ref().is_some_and(|i| solid.contains(i)) {
                        let offset =
                            Vec2::new(col as f32 - center.x, center.y - row as f32) * tile * scale;
                        parts.push((Isometry::new(vector(offset), 0.), cell.clone()));
                    }
                }
            }
            (!parts.is_empty()).then(|| ColliderBuilder::compound(parts))
        }
        _ => None,
    }
}

fn make_joint(joint: &PhysicsJoint2D) -> GenericJoint {
    let (a, b) = (
        point(safe_vector(joint.anchor)),
        point(safe_vector(joint.target_anchor)),
    );
    match joint.kind {
        JointKind::Fixed => FixedJointBuilder::new()
            .local_anchor1(a)
            .local_anchor2(b)
            .into(),
        JointKind::Revolute => RevoluteJointBuilder::new()
            .local_anchor1(a)
            .local_anchor2(b)
            .into(),
    }
}

fn make_controller(controller: &CharacterController2D) -> KinematicCharacterController {
    let angle = finite_or(controller.slope_degrees, 45.)
        .clamp(0., 89.9)
        .to_radians();
    KinematicCharacterController {
        max_slope_climb_angle: if controller.platformer {
            angle
        } else {
            std::f32::consts::PI
        },
        min_slope_slide_angle: angle,
        snap_to_ground: (controller.platformer
            && controller.ground_snap.is_finite()
            && controller.ground_snap > 0.)
            .then_some(CharacterLength::Absolute(controller.ground_snap)),
        ..Default::default()
    }
}

fn desired_motion(
    controller: &CharacterController2D,
    motion: &mut CharacterMotion2D,
    g: Vec2,
    dt: f32,
) -> Vec2 {
    let mut v = safe_vector(motion.velocity);
    if controller.platformer {
        motion.fall_speed = finite_or(motion.fall_speed, 0.) + g.y * dt;
        v.y += motion.fall_speed;
    } else {
        motion.fall_speed = 0.;
    }
    safe_vector(v * dt)
}
