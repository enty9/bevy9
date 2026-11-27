use bevy_app::PostUpdate;
use bevy_time::TimePlugin;
use bevy_transform::TransformPlugin;
use engine_core::{EngineCorePlugin, EntityGuid};

use super::*;

#[derive(Resource, Default)]
struct Observed {
    contacts: Vec<CollisionEvent>,
    triggers: Vec<TriggerEvent>,
}

fn harness() -> App {
    let mut app = App::new();
    app.add_plugins((
        TimePlugin,
        TransformPlugin,
        EngineCorePlugin,
        EnginePhysicsPlugin,
    ));
    app.init_resource::<Observed>();
    app.add_observer(
        |event: On<CollisionEvent>, mut observed: ResMut<Observed>| {
            observed.contacts.push(*event.event());
        },
    );
    app.add_observer(|event: On<TriggerEvent>, mut observed: ResMut<Observed>| {
        observed.triggers.push(*event.event());
    });
    app
}

fn step(app: &mut App, count: usize) {
    for _ in 0..count {
        app.world_mut().run_schedule(FixedUpdate);
        app.world_mut().run_schedule(PostUpdate);
    }
}

#[test]
fn xy_falls_bounces_collides_and_casts_through_sensors() {
    let mut app = harness();
    let world = app.world_mut();
    let bounce = PhysicsMaterial {
        restitution: 0.8,
        ..Default::default()
    };
    let floor = world
        .spawn((
            Transform::from_xyz(0., -5., 0.),
            Collider2D::Cuboid(Vec2::new(200., 5.)),
            bounce,
        ))
        .id();
    let trigger = world
        .spawn((
            Transform::from_xyz(0., 60., 0.),
            Collider2D::Cuboid(Vec2::new(20., 10.)),
            TriggerVolume2D,
        ))
        .id();
    let ball = world
        .spawn((
            Transform::from_xyz(0., 120., 3.),
            Collider2D::Ball(10.),
            RigidBody2D {
                ccd: true,
                ..Default::default()
            },
            bounce,
        ))
        .id();
    step(&mut app, 15);
    assert!(
        app.world()
            .resource::<PhysicsWorld2D>()
            .linear_velocity(ball)
            .unwrap()
            .y
            < -100.
    );
    let mut rebounded = false;
    for _ in 0..60 {
        step(&mut app, 1);
        rebounded |= app
            .world()
            .resource::<PhysicsWorld2D>()
            .linear_velocity(ball)
            .unwrap()
            .y
            > 50.;
    }
    assert!(rebounded, "the 2D ball must bounce");
    assert_eq!(
        app.world().get::<Transform>(ball).unwrap().translation.z,
        3.
    );
    let events = app.world().resource::<Observed>();
    assert!(events.contacts.iter().any(|e| e.entities.contains(&floor)
        && e.dimension == PhysicsDimension::D2
        && e.phase == ContactPhase::Started));
    for phase in [ContactPhase::Started, ContactPhase::Stopped] {
        assert!(events.triggers.iter().any(|e| e.entities.contains(&trigger)
            && e.entities.contains(&ball)
            && e.phase == phase));
    }
    let physics = app.world().resource::<PhysicsWorld2D>();
    let origin = Vec2::new(0., 100.);
    let filter = CastFilter {
        exclude: Some(ball),
        ..Default::default()
    };
    assert_eq!(
        physics
            .raycast(origin, -Vec2::Y * 2., 200., filter)
            .unwrap()
            .entity,
        floor
    );
    let sensor_hit = physics
        .raycast(
            origin,
            -Vec2::Y,
            200.,
            CastFilter {
                include_sensors: true,
                ..filter
            },
        )
        .unwrap();
    assert_eq!(sensor_hit.entity, trigger);
    assert!((sensor_hit.distance - 30.).abs() < 0.001);
    let swept = physics
        .shapecast(
            &Collider2D::Ball(2.),
            Transform::from_translation(origin.extend(0.)),
            -Vec2::Y,
            200.,
            filter,
        )
        .unwrap();
    assert_eq!(swept.entity, floor);
    assert!((swept.distance - 98.).abs() < 0.01);
}

#[test]
fn xyz_contacts_triggers_queries_and_joint_use_independent_world() {
    let mut app = harness();
    let world = app.world_mut();
    let bounce = PhysicsMaterial {
        restitution: 0.8,
        ..Default::default()
    };
    let floor = world
        .spawn((
            Transform::from_xyz(0., -0.5, 0.),
            Collider3D::Cuboid(Vec3::new(10., 0.5, 10.)),
            bounce,
        ))
        .id();
    let sensor = world
        .spawn((
            Transform::from_xyz(0., 2., 0.),
            Collider3D::Cuboid(Vec3::new(1., 0.35, 1.)),
            TriggerVolume3D,
        ))
        .id();
    let ball = world
        .spawn((
            Transform::from_xyz(0., 4., 0.),
            RigidBody3D {
                ccd: true,
                ..Default::default()
            },
            Collider3D::Ball(0.25),
            bounce,
        ))
        .id();
    let anchor = EntityGuid::new();
    world.spawn((
        anchor,
        Transform::from_xyz(3., 4., 0.),
        RigidBody3D {
            kind: BodyKind::Static,
            ..Default::default()
        },
    ));
    let joint = world
        .spawn((
            Transform::from_xyz(3., 4., 0.),
            RigidBody3D::default(),
            Collider3D::Ball(0.25),
            PhysicsJoint3D {
                target: Some(anchor),
                ..Default::default()
            },
        ))
        .id();
    step(&mut app, 20);
    assert!(
        app.world()
            .resource::<PhysicsWorld3D>()
            .linear_velocity(ball)
            .unwrap()
            .y
            < -2.
    );
    let mut rebound = false;
    for _ in 0..100 {
        step(&mut app, 1);
        rebound |= app
            .world()
            .resource::<PhysicsWorld3D>()
            .linear_velocity(ball)
            .unwrap()
            .y
            > 1.;
    }
    assert!(rebound);
    assert!((app.world().get::<Transform>(joint).unwrap().translation.y - 4.).abs() < 0.03);
    let events = app.world().resource::<Observed>();
    assert!(events.contacts.iter().any(|e| e.entities.contains(&floor)
        && e.dimension == PhysicsDimension::D3
        && e.phase == ContactPhase::Started));
    assert!(events.triggers.iter().any(|e| e.entities.contains(&sensor)
        && e.entities.contains(&ball)
        && e.phase == ContactPhase::Started));
    let filter = CastFilter {
        exclude: Some(ball),
        ..Default::default()
    };
    let physics = app.world().resource::<PhysicsWorld3D>();
    assert_eq!(
        physics
            .raycast(Vec3::Y * 5., -Vec3::Y, 10., filter)
            .unwrap()
            .entity,
        floor
    );
    assert_eq!(
        physics
            .shapecast(
                &Collider3D::Ball(0.2),
                Transform::from_translation(Vec3::Y * 5.),
                -Vec3::Y,
                10.,
                filter
            )
            .unwrap()
            .entity,
        floor
    );
    assert!(app
        .world()
        .resource::<PhysicsWorld2D>()
        .raycast(Vec2::Y * 5., -Vec2::Y, 10., filter)
        .is_none());
}

#[test]
fn character_hits_3d_wall_and_can_jump_through_2d_platform() {
    let mut app = harness();
    let world = app.world_mut();
    world.spawn((
        Transform::from_xyz(2., 0., 0.),
        Collider3D::Cuboid(Vec3::new(0.2, 5., 5.)),
    ));
    world.spawn((
        Transform::IDENTITY,
        Collider2D::Cuboid(Vec2::new(100., 3.)),
        OneWayPlatform2D,
    ));
    let character3d = world
        .spawn((
            RigidBody3D {
                kind: BodyKind::Kinematic,
                ..Default::default()
            },
            Collider3D::Ball(0.3),
            CharacterController3D {
                step_height: 0.,
                ..Default::default()
            },
        ))
        .id();
    let character2d = world
        .spawn((
            Transform::from_xyz(0., -30., 0.),
            RigidBody2D {
                kind: BodyKind::Kinematic,
                ..Default::default()
            },
            Collider2D::Ball(5.),
            CharacterController2D::default(),
        ))
        .id();
    app.world_mut()
        .get_mut::<CharacterMotion3D>(character3d)
        .unwrap()
        .velocity = Vec3::X * 5.;
    app.world_mut()
        .get_mut::<CharacterMotion2D>(character2d)
        .unwrap()
        .fall_speed = 420.;
    step(&mut app, 60);
    let world = app.world();
    let x = world.get::<Transform>(character3d).unwrap().translation.x;
    assert!((1.2..1.55).contains(&x), "character ended at x={x}");
    let y = world.get::<Transform>(character2d).unwrap().translation.y;
    assert!((7.9..9.).contains(&y), "character ended at y={y}");
    assert!(
        world
            .get::<CharacterMotion2D>(character2d)
            .unwrap()
            .grounded
    );
}

#[test]
fn editor_debug_sync_does_not_advance_authored_transforms() {
    let mut app = harness();
    app.insert_resource(PhysicsOptions {
        simulate: false,
        debug: true,
    });
    let e = app
        .world_mut()
        .spawn((
            Transform::from_xyz(0., 5., 0.),
            Collider3D::Ball(0.5),
            RigidBody3D::default(),
        ))
        .id();
    step(&mut app, 60);
    assert_eq!(app.world().get::<Transform>(e).unwrap().translation.y, 5.);
    assert_eq!(
        app.world().resource::<PhysicsWorld3D>().linear_velocity(e),
        Some(Vec3::ZERO)
    );
}

#[test]
fn editor_debug_sync_updates_collider_on_same_post_update_and_clears_when_disabled() {
    let mut app = harness();
    app.insert_resource(PhysicsOptions {
        simulate: false,
        debug: true,
    });
    let e = app
        .world_mut()
        .spawn((Transform::from_xyz(0., 2., 0.), Collider3D::Ball(0.5)))
        .id();

    app.world_mut().run_schedule(PostUpdate);
    assert_eq!(
        app.world().resource::<PhysicsWorld3D>().linear_velocity(e),
        Some(Vec3::ZERO)
    );

    {
        let mut entity_mut = app.world_mut().entity_mut(e);
        entity_mut.get_mut::<Transform>().unwrap().translation.y = 4.;
        *entity_mut.get_mut::<Collider3D>().unwrap() = Collider3D::Cuboid(Vec3::ONE);
    }
    app.world_mut().run_schedule(PostUpdate);
    assert_eq!(app.world().get::<Transform>(e).unwrap().translation.y, 4.);
    assert_eq!(
        app.world().resource::<PhysicsWorld3D>().linear_velocity(e),
        Some(Vec3::ZERO)
    );

    app.world_mut().resource_mut::<PhysicsOptions>().debug = false;
    app.world_mut().run_schedule(PostUpdate);
    assert_eq!(
        app.world().resource::<PhysicsWorld3D>().linear_velocity(e),
        None
    );
}

#[test]
fn editor_debug_sync_builds_ball_shape_and_switches_variants() {
    use rapier3d::prelude::TypedShape;

    let mut app = harness();
    app.insert_resource(PhysicsOptions {
        simulate: false,
        debug: true,
    });
    let e = app
        .world_mut()
        .spawn((
            Transform::from_xyz(1.1, 2.6, 0.2),
            Collider3D::Ball(0.65),
            RigidBody3D::default(),
        ))
        .id();
    app.world_mut().run_schedule(PostUpdate);

    let ball_radius = |app: &App| match app
        .world()
        .resource::<PhysicsWorld3D>()
        .collider_for(e)
        .expect("collider must exist")
        .shape()
        .as_typed_shape()
    {
        TypedShape::Ball(ball) => Some(ball.radius),
        _ => None,
    };
    let radius =
        ball_radius(&app).expect("Collider3D::Ball must build a Rapier ball, not a cuboid");
    assert!((radius - 0.65).abs() < 1e-5);

    *app.world_mut().get_mut::<Collider3D>(e).unwrap() = Collider3D::Cuboid(Vec3::splat(0.6));
    app.world_mut().run_schedule(PostUpdate);
    assert!(matches!(
        app.world()
            .resource::<PhysicsWorld3D>()
            .collider_for(e)
            .unwrap()
            .shape()
            .as_typed_shape(),
        TypedShape::Cuboid(_)
    ));

    *app.world_mut().get_mut::<Collider3D>(e).unwrap() = Collider3D::Ball(0.4);
    app.world_mut().run_schedule(PostUpdate);
    let radius = ball_radius(&app).expect("switching back to Ball must rebuild a ball");
    assert!((radius - 0.4).abs() < 1e-5);
}

#[test]
fn selected_colliders_are_queryable_and_follow_edits_without_simulation() {
    let mut app = harness();
    let world = app.world_mut();
    world.insert_resource(PhysicsOptions {
        simulate: false,
        debug: false,
    });
    let entity = world
        .spawn((
            Transform::from_xyz(0., 2., 0.),
            Collider2D::Ball(1.),
            Collider3D::Ball(1.),
        ))
        .id();
    world
        .resource_mut::<PhysicsDebugSelection>()
        .0
        .insert(entity);
    let check = |app: &mut App, expected: f32| {
        app.world_mut().run_schedule(PostUpdate);
        let physics_2d = app.world().resource::<PhysicsWorld2D>();
        assert!(physics_2d.collider_for(entity).is_some());
        let hit = physics_2d
            .raycast(Vec2::ZERO, Vec2::Y, 10., CastFilter::default())
            .unwrap();
        assert_eq!(hit.entity, entity);
        assert!((hit.distance - expected).abs() < 1e-5);
        let hit = app
            .world()
            .resource::<PhysicsWorld3D>()
            .raycast(Vec3::ZERO, Vec3::Y, 10., CastFilter::default())
            .unwrap();
        assert_eq!(hit.entity, entity);
        assert!((hit.distance - expected).abs() < 1e-5);
    };
    check(&mut app, 1.);
    app.world_mut()
        .get_mut::<Transform>(entity)
        .unwrap()
        .translation
        .y = 4.;
    *app.world_mut().get_mut::<Collider2D>(entity).unwrap() = Collider2D::Ball(0.5);
    *app.world_mut().get_mut::<Collider3D>(entity).unwrap() = Collider3D::Ball(0.5);
    check(&mut app, 3.5);
    app.world_mut().despawn(entity);
    app.world_mut().run_schedule(PostUpdate);
    assert!(app
        .world()
        .resource::<PhysicsWorld2D>()
        .raycast(Vec2::ZERO, Vec2::Y, 10., CastFilter::default())
        .is_none());
    assert!(app
        .world()
        .resource::<PhysicsWorld3D>()
        .raycast(Vec3::ZERO, Vec3::Y, 10., CastFilter::default())
        .is_none());
}

#[test]
fn interpolation_uses_the_current_frame_parent_transform() {
    use bevy_transform::components::GlobalTransform;

    let mut app = harness();
    let world = app.world_mut();
    let parent = world.spawn(Transform::IDENTITY).id();
    let child = world
        .spawn((
            ChildOf(parent),
            Transform::from_xyz(3., 0., 0.),
            Collider3D::Ball(0.5),
            RigidBody3D {
                gravity_scale: 0.,
                ..Default::default()
            },
        ))
        .id();
    step(&mut app, 1);
    // Authored parent moves in Update after the fixed simulation step. The
    // child's world-space simulation pose should not inherit that displacement.
    app.world_mut()
        .get_mut::<Transform>(parent)
        .unwrap()
        .translation
        .x = 5.;
    app.world_mut().run_schedule(PostUpdate);
    let child_global = app.world().get::<GlobalTransform>(child).unwrap();
    assert!(child_global
        .translation()
        .abs_diff_eq(Vec3::new(3., 0., 0.), 1e-5));
    assert_eq!(
        app.world().get::<Transform>(child).unwrap().translation.x,
        -2.
    );
}
