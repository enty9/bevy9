//! Fixed-step 2D and 3D Rapier physics.

use std::collections::BTreeSet;

use bevy_app::{App, FixedUpdate, Plugin, PostUpdate, PreUpdate};
use bevy_ecs::{
    prelude::*,
    reflect::{ReflectComponent, ReflectResource},
};
use bevy_math::{Vec2, Vec3};
use bevy_reflect::{prelude::ReflectDefault, Reflect};
use bevy_time::{Fixed, Time};
use bevy_transform::{components::Transform, TransformSystems};
use engine_core::{AssetUuid, EngineFixedSet, EntityGuid};

mod backend;
mod dim2;
mod dim3;
pub use dim2::PhysicsWorld2D;
pub use dim3::PhysicsWorld3D;

/// Gravity vectors for the independent simulation worlds (in scene units/s²).
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct Gravity {
    /// XY gravity. In the engine's sprite scenes, one unit is one pixel by default.
    pub xy: Vec2,
    /// XYZ gravity.
    pub xyz: Vec3,
}

impl Default for Gravity {
    fn default() -> Self {
        Self {
            xy: Vec2::new(0., -980.),
            xyz: Vec3::new(0., -9.81, 0.),
        }
    }
}

/// Requested rate of Bevy's fixed schedule and the Rapier step.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct PhysicsTimestep {
    /// Steps per second; accepted values are 1–240 Hz.
    pub hz: f64,
}

impl Default for PhysicsTimestep {
    fn default() -> Self {
        Self { hz: 60. }
    }
}

/// Enables simulation and/or collider debug drawing. Editors can show colliders without simulating.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct PhysicsOptions {
    /// Run simulation in `FixedUpdate`.
    pub simulate: bool,
    /// Draw collider wireframes when [`PhysicsDebugPlugin`] is installed.
    pub debug: bool,
}

impl Default for PhysicsOptions {
    fn default() -> Self {
        Self {
            simulate: true,
            debug: false,
        }
    }
}

/// Bidirectional collision mask: both bodies must accept each other's memberships.
#[derive(Component, Debug, Clone, Copy, PartialEq, Reflect)]
#[reflect(Component, Default)]
pub struct CollisionLayers {
    /// Bits identifying this object's layers.
    pub memberships: u32,
    /// Bits of layers this object accepts.
    pub filter: u32,
}

impl Default for CollisionLayers {
    fn default() -> Self {
        Self {
            memberships: u32::MAX,
            filter: u32::MAX,
        }
    }
}

/// Material of a collider.
#[derive(Component, Debug, Clone, Copy, PartialEq, Reflect)]
#[reflect(Component, Default)]
pub struct PhysicsMaterial {
    /// Contact friction (nonnegative).
    pub friction: f32,
    /// Bounciness, between zero and one.
    pub restitution: f32,
    /// Mass density.
    pub density: f32,
}

impl Default for PhysicsMaterial {
    fn default() -> Self {
        Self {
            friction: 0.5,
            restitution: 0.,
            density: 1.,
        }
    }
}

/// Body mobility.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Reflect)]
pub enum BodyKind {
    /// Simulated by forces and contacts.
    #[default]
    Dynamic,
    /// Immovable geometry.
    Static,
    /// Driven by its transform or by a character controller.
    Kinematic,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Reflect)]
/// Joint constraint type.
pub enum JointKind {
    /// Holds the two body frames together.
    #[default]
    Fixed,
    /// Allows rotation about a single axis.
    Revolute,
}

macro_rules! dimension_components {
    ($body:ident, $trigger:ident, $joint:ident, $vector:ty) => {
        /// Authored rigid body descriptor. A collider without a body is static.
        #[derive(Component, Debug, Clone, Copy, PartialEq, Reflect)]
        #[reflect(Component, Default)]
        #[require(Transform)]
        pub struct $body {
            /// Mobility of this body.
            pub kind: BodyKind,
            /// Initial linear velocity, in scene units per second.
            pub velocity: $vector,
            /// Multiplier applied to world gravity.
            pub gravity_scale: f32,
            /// Enable continuous collision detection for fast-moving bodies.
            pub ccd: bool,
        }

        impl Default for $body {
            fn default() -> Self {
                Self {
                    kind: BodyKind::Dynamic,
                    velocity: <$vector>::ZERO,
                    gravity_scale: 1.,
                    ccd: false,
                }
            }
        }

        /// Makes the same entity's collider a sensor instead of a solid.
        #[derive(Component, Debug, Default, Clone, Copy, Reflect)]
        #[reflect(Component, Default)]
        pub struct $trigger;

        /// Joint to another authored body, resolved by stable scene GUID.
        #[derive(Component, Debug, Default, Clone, Copy, PartialEq, Reflect)]
        #[reflect(Component, Default)]
        pub struct $joint {
            /// The target body. An empty target disables the joint.
            pub target: Option<EntityGuid>,
            /// Constraint applied to the two bodies.
            pub kind: JointKind,
            /// Attachment point relative to this body.
            pub anchor: $vector,
            /// Attachment point relative to the target body.
            pub target_anchor: $vector,
        }
    };
}

dimension_components!(RigidBody2D, TriggerVolume2D, PhysicsJoint2D, Vec2);
dimension_components!(RigidBody3D, TriggerVolume3D, PhysicsJoint3D, Vec3);

/// Local 2D collision geometry.
#[derive(Component, Debug, Clone, PartialEq, Reflect)]
#[reflect(Component, Default)]
#[require(Transform)]
pub enum Collider2D {
    /// Rectangle defined by half extents.
    Cuboid(Vec2),
    /// Circle radius.
    Ball(f32),
    /// Y-aligned capsule with (half segment length, radius).
    Capsule(f32, f32),
    /// Segment endpoints.
    Segment(Vec2, Vec2),
    /// Convex hull of the supplied vertices.
    ConvexPolygon(Vec<Vec2>),
    /// Consecutive segments connecting these vertices.
    Polyline(Vec<Vec2>),
    /// Solid cells in the tilemap asset.
    TilemapCollider {
        /// UUID of the `.tilemap.ron` asset.
        asset: AssetUuid,
        /// Scale matching `SceneTilemap.tile_scale`.
        tile_scale: f32,
    },
}

impl Default for Collider2D {
    fn default() -> Self {
        Self::Cuboid(Vec2::splat(16.))
    }
}

/// Local 3D collision geometry.
#[derive(Component, Debug, Clone, PartialEq, Reflect)]
#[reflect(Component, Default)]
#[require(Transform)]
pub enum Collider3D {
    /// Box defined by half extents.
    Cuboid(Vec3),
    /// Sphere radius.
    Ball(f32),
    /// Y-aligned capsule with (half segment length, radius).
    Capsule(f32, f32),
    /// Y-aligned cylinder with (half height, radius).
    Cylinder(f32, f32),
    /// Convex hull of supplied vertices.
    ConvexHull(Vec<Vec3>),
    /// Indexed triangles (typically used on static bodies).
    TriMesh {
        /// Local mesh vertices.
        vertices: Vec<Vec3>,
        /// Indices of each triangle's three vertices.
        indices: Vec<[u32; 3]>,
    },
    /// Row-major rectangular height grid (typically used on static bodies).
    Heightfield {
        /// Number of rows along Z.
        rows: u32,
        /// Number of columns along X.
        columns: u32,
        /// Grid samples in row-major order.
        heights: Vec<f32>,
        /// Physical extent in X/Z and amplitude in Y.
        scale: Vec3,
    },
}

impl Default for Collider3D {
    fn default() -> Self {
        Self::Cuboid(Vec3::splat(0.5))
    }
}

/// Platform only blocks platformer character casts from above (world +Y).
#[derive(Component, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Component, Default)]
pub struct OneWayPlatform2D;

/// Tuning for position-based 2D character movement. Add a kinematic body and a convex collider.
#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component, Default)]
#[require(Transform, CharacterMotion2D)]
pub struct CharacterController2D {
    /// Enables platformer gravity and one-way platform handling. Otherwise use top-down control.
    pub platformer: bool,
    /// Maximum climbable slope, in degrees.
    pub slope_degrees: f32,
    /// Snap-to-ground distance. Zero disables snapping.
    pub ground_snap: f32,
}

impl Default for CharacterController2D {
    fn default() -> Self {
        Self {
            platformer: true,
            slope_degrees: 45.,
            ground_snap: 2.,
        }
    }
}

/// Tuning for position-based 3D character movement. Add a kinematic body and a convex collider.
#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component, Default)]
#[require(Transform, CharacterMotion3D)]
pub struct CharacterController3D {
    /// Maximum climbable slope, in degrees.
    pub slope_degrees: f32,
    /// Maximum automatic step height. Zero disables stepping.
    pub step_height: f32,
    /// Snap-to-ground distance. Zero disables snapping.
    pub ground_snap: f32,
}

impl Default for CharacterController3D {
    fn default() -> Self {
        Self {
            slope_degrees: 45.,
            step_height: 0.3,
            ground_snap: 0.15,
        }
    }
}

macro_rules! motion {
    ($name:ident, $vector:ty) => {
        /// Runtime control and feedback for a character. Insert this component to drive movement.
        #[derive(Component, Debug, Default)]
        pub struct $name {
            /// Desired velocity; the caller supplies jumping and any vertical velocity in 3D.
            pub velocity: $vector,
            /// True when the last move ended on a walkable surface.
            pub grounded: bool,
            /// Accumulated platformer gravity, zeroed on landing. Unused by 3D and top-down motion.
            pub fall_speed: f32,
        }
    };
}

motion!(CharacterMotion2D, Vec2);
motion!(CharacterMotion3D, Vec3);

/// Dimension of a physics observer event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicsDimension {
    /// XY world.
    D2,
    /// XYZ world.
    D3,
}

/// Lifecycle phase of a body pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactPhase {
    /// Entered contact/overlap this step.
    Started,
    /// Ended contact/overlap this step, including through entity removal.
    Stopped,
}

macro_rules! pair_event {
    ($event:ident, $docs:literal) => {
        #[doc = $docs]
        #[derive(Event, Debug, Clone, Copy)]
        pub struct $event {
            /// Pair of entities in stable order for this world.
            pub entities: [Entity; 2],
            /// Simulation dimension.
            pub dimension: PhysicsDimension,
            /// Lifecycle phase.
            pub phase: ContactPhase,
        }
    };
}

pair_event!(
    CollisionEvent,
    "Observer event for solid contact transitions."
);
pair_event!(
    TriggerEvent,
    "Observer event for sensor overlap transitions."
);

/// Filters for shape and ray casts, shared between dimensions.
#[derive(Debug, Clone, Copy, Default)]
pub struct CastFilter {
    /// Restrict hits to colliders compatible with these groups, if set.
    pub layers: Option<CollisionLayers>,
    /// Exclude all shapes of one body.
    pub exclude: Option<Entity>,
    /// Include sensor volumes (ignored by default).
    pub include_sensors: bool,
}

/// Result of a cast into a physics world, parameterized by its vector type.
#[derive(Debug, Clone, Copy)]
pub struct CastHit<V> {
    /// ECS entity owning the impacted collider.
    pub entity: Entity,
    /// Distance travelled along the normalized cast direction.
    pub distance: f32,
    /// World-space surface normal.
    pub normal: V,
}

/// System set synchronizing authored descriptors into physics worlds for editor debug previews.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhysicsDebugSyncSet;

/// Entities whose colliders should be shown even when global debugging is off.
#[derive(Resource, Default, PartialEq, Eq)]
pub struct PhysicsDebugSelection(pub BTreeSet<Entity>);

/// Installs both fixed-step physics worlds without requiring a renderer.
pub struct EnginePhysicsPlugin;

impl Plugin for EnginePhysicsPlugin {
    fn build(&self, app: &mut App) {
        register_components(app);
        app.init_resource::<Gravity>()
            .init_resource::<PhysicsTimestep>()
            .init_resource::<PhysicsOptions>()
            .init_resource::<PhysicsDebugSelection>()
            .init_resource::<PhysicsWorld2D>()
            .init_resource::<PhysicsWorld3D>()
            .init_resource::<dim2::TilemapShapes>()
            .register_type::<Gravity>()
            .register_type::<PhysicsTimestep>()
            .register_type::<PhysicsOptions>()
            .add_systems(PreUpdate, update_timestep)
            .add_systems(
                FixedUpdate,
                (dim2::step, dim3::step)
                    .chain()
                    .in_set(EngineFixedSet::Physics),
            )
            .add_systems(
                PostUpdate,
                (
                    (dim2::interpolate, dim3::interpolate)
                        .chain()
                        .before(TransformSystems::Propagate),
                    (dim2::sync_debug, dim3::sync_debug)
                        .chain()
                        .in_set(PhysicsDebugSyncSet)
                        .after(TransformSystems::Propagate),
                ),
            );
    }
}

/// Registers authored components for serialization and authoring tools without starting simulation.
pub fn register_components(app: &mut App) {
    app.register_type::<CollisionLayers>()
        .register_type::<PhysicsMaterial>()
        .register_type::<RigidBody2D>()
        .register_type::<RigidBody3D>()
        .register_type::<Collider2D>()
        .register_type::<Collider3D>()
        .register_type::<TriggerVolume2D>()
        .register_type::<TriggerVolume3D>()
        .register_type::<PhysicsJoint2D>()
        .register_type::<PhysicsJoint3D>()
        .register_type::<OneWayPlatform2D>()
        .register_type::<CharacterController2D>()
        .register_type::<CharacterController3D>();
}

fn update_timestep(timestep: Res<PhysicsTimestep>, mut fixed: ResMut<Time<Fixed>>) {
    if timestep.is_changed() && (1. ..=240.).contains(&timestep.hz) {
        fixed.set_timestep_hz(timestep.hz);
    }
}

/// Connects Rapier collider wireframes to Bevy gizmos. Requires `GizmoPlugin`.
pub struct PhysicsDebugPlugin;

impl Plugin for PhysicsDebugPlugin {
    fn build(&self, app: &mut App) {
        use bevy_gizmos::{
            config::{GizmoConfig, GizmoLineConfig},
            AppGizmoBuilder,
        };
        app.insert_gizmo_config(
            PhysicsGizmos,
            GizmoConfig {
                depth_bias: -0.04,
                line: GizmoLineConfig {
                    width: 2.0,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .add_systems(
            PostUpdate,
            (dim2::draw_debug, dim3::draw_debug)
                .chain()
                .after(PhysicsDebugSyncSet)
                .after(TransformSystems::Propagate),
        );
    }
}

/// Gizmo storage group for physics wireframes.
#[derive(Default, Reflect, bevy_gizmos::config::GizmoConfigGroup)]
pub struct PhysicsGizmos;

#[cfg(test)]
mod tests;
