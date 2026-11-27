//! Rapier's 2D and 3D APIs are isomorphic. Instantiate the same ECS lifecycle for both.

macro_rules! backend {
    ($world:ident, $body:ident, $collider:ident, $trigger:ident, $joint:ident, $controller:ident, $motion:ident, $dim:ident, $vec:ty, $length:expr) => {
        use std::collections::{BTreeMap, BTreeSet};

        use bevy_color::Color;
        use bevy_gizmos::prelude::Gizmos;
        use bevy_transform::{
            components::{GlobalTransform, Transform},
            helper::TransformHelper,
        };
        use engine_core::EntityGuidMap;
        use rapier::math::{Isometry, Point, Vector};
        use rapier::parry::{
            partitioning::{Bvh, BvhBuildStrategy},
            query::ShapeCastOptions,
        };
        use rapier::prelude::*;

        struct ColliderConfig<D> {
            descriptor: D,
            material: PhysicsMaterial,
            layers: CollisionLayers,
            sensor: bool,
            scale: $vec,
            asset_version: Option<(String, f32)>,
        }

        struct BodyEntry {
            handle: RigidBodyHandle,
            body: $body,
            collider: Option<ColliderHandle>,
            config: Option<ColliderConfig<$collider>>,
            last_local: Transform,
            last_target: Isometry<f32>,
            previous: Isometry<f32>,
            current: Isometry<f32>,
        }

        #[derive(Resource, Default)]
        /// Rapier simulation and collision queries for this dimension.
        pub struct $world {
            pipeline: PhysicsPipeline,
            bodies: RigidBodySet,
            colliders: ColliderSet,
            islands: IslandManager,
            broad: BroadPhaseBvh,
            narrow: NarrowPhase,
            joints: ImpulseJointSet,
            multibody: MultibodyJointSet,
            ccd: CCDSolver,
            entries: BTreeMap<Entity, BodyEntry>,
            joint_entries: BTreeMap<Entity, (Entity, $joint, ImpulseJointHandle)>,
            contacts: BTreeSet<[Entity; 2]>,
            overlaps: BTreeSet<[Entity; 2]>,
            query_needs_rebuild: bool,
            debug: DebugRenderPipeline,
        }

        fn groups(layers: CollisionLayers) -> InteractionGroups {
            InteractionGroups::new(
                Group::from_bits_retain(layers.memberships),
                Group::from_bits_retain(layers.filter),
            )
        }

        fn mobility(kind: BodyKind) -> RigidBodyType {
            match kind {
                BodyKind::Dynamic => RigidBodyType::Dynamic,
                BodyKind::Static => RigidBodyType::Fixed,
                BodyKind::Kinematic => RigidBodyType::KinematicPositionBased,
            }
        }

        impl $world {
            fn query<'a>(&'a self, filter: QueryFilter<'a>) -> QueryPipeline<'a> {
                self.broad.as_query_pipeline(
                    self.narrow.query_dispatcher(),
                    &self.bodies,
                    &self.colliders,
                    filter,
                )
            }

            fn filter(&self, filter: CastFilter) -> QueryFilter<'static> {
                QueryFilter {
                    flags: if filter.include_sensors {
                        QueryFilterFlags::empty()
                    } else {
                        QueryFilterFlags::EXCLUDE_SENSORS
                    },
                    groups: filter.layers.map(groups),
                    exclude_rigid_body: filter
                        .exclude
                        .and_then(|e| self.entries.get(&e).map(|entry| entry.handle)),
                    ..Default::default()
                }
            }

            fn entity_for(&self, handle: ColliderHandle) -> Option<Entity> {
                Entity::try_from_bits(self.colliders.get(handle)?.user_data as u64)
            }

            /// Casts a ray in a direction (normalized internally) after the most recent simulation step.
            pub fn raycast(
                &self,
                origin: $vec,
                direction: $vec,
                max_distance: f32,
                filter: CastFilter,
            ) -> Option<CastHit<$vec>> {
                if !origin.is_finite() || !max_distance.is_finite() || max_distance < 0. {
                    return None;
                }
                let ray = Ray::new(point(origin), vector(direction.try_normalize()?));
                let (handle, hit) = self.query(self.filter(filter)).cast_ray_and_get_normal(
                    &ray,
                    max_distance,
                    true,
                )?;
                Some(CastHit {
                    entity: self.entity_for(handle)?,
                    distance: hit.time_of_impact,
                    normal: from_vector(hit.normal),
                })
            }

            /// Sweeps a collider descriptor from a world transform. Asset-backed shapes are not accepted here.
            pub fn shapecast(
                &self,
                shape: &$collider,
                origin: Transform,
                direction: $vec,
                max_distance: f32,
                filter: CastFilter,
            ) -> Option<CastHit<$vec>> {
                if !valid_transform(&origin) || !max_distance.is_finite() || max_distance < 0. {
                    return None;
                }
                let shape = shape_builder(shape, dimensions(origin.scale), None)?.build();
                let options = ShapeCastOptions {
                    max_time_of_impact: max_distance,
                    ..Default::default()
                };
                let (handle, hit) = self.query(self.filter(filter)).cast_shape(
                    &pose(origin),
                    &vector(direction.try_normalize()?),
                    shape.shape(),
                    options,
                )?;
                Some(CastHit {
                    entity: self.entity_for(handle)?,
                    distance: hit.time_of_impact,
                    normal: from_vector(*hit.normal1),
                })
            }

            /// Reads simulated linear velocity, independently of render interpolation.
            pub fn linear_velocity(&self, entity: Entity) -> Option<$vec> {
                Some(from_vector(
                    *self.bodies.get(self.entries.get(&entity)?.handle)?.linvel(),
                ))
            }

            /// Applies a world-space impulse to a dynamic body. Returns false if it does not exist.
            pub fn apply_impulse(&mut self, entity: Entity, impulse: $vec) -> bool {
                if !impulse.is_finite() {
                    return false;
                }
                let Some(body) = self
                    .entries
                    .get(&entity)
                    .and_then(|entry| self.bodies.get_mut(entry.handle))
                else {
                    return false;
                };
                if !body.is_dynamic() {
                    return false;
                }
                body.apply_impulse(vector(impulse), true);
                true
            }

            fn remove(&mut self, entity: Entity) {
                if let Some(entry) = self.entries.remove(&entity) {
                    self.query_needs_rebuild = true;
                    self.bodies.remove(
                        entry.handle,
                        &mut self.islands,
                        &mut self.colliders,
                        &mut self.joints,
                        &mut self.multibody,
                        true,
                    );
                }
            }

            fn pairs(&self) -> (BTreeSet<[Entity; 2]>, BTreeSet<[Entity; 2]>) {
                let entity_pair = |a, b| {
                    let mut pair = [self.entity_for(a)?, self.entity_for(b)?];
                    pair.sort_unstable();
                    Some(pair)
                };
                let solid = self
                    .narrow
                    .contact_pairs()
                    .filter(|p| p.has_any_active_contact)
                    .filter_map(|p| entity_pair(p.collider1, p.collider2))
                    .collect();
                let sensors = self
                    .narrow
                    .intersection_pairs()
                    .filter(|(_, _, active)| *active)
                    .filter_map(|(a, b, _)| entity_pair(a, b))
                    .collect();
                (solid, sensors)
            }
        }

        fn valid_transform(t: &Transform) -> bool {
            t.translation.is_finite()
                && t.rotation.is_finite()
                && t.rotation.length_squared() > 1e-10
                && t.scale.is_finite()
        }

        type Descriptors = (
            Entity,
            &'static Transform,
            Option<&'static ChildOf>,
            Option<&'static $body>,
            Option<Ref<'static, $collider>>,
            Has<$trigger>,
            Option<&'static CollisionLayers>,
            Option<&'static PhysicsMaterial>,
            Option<&'static $joint>,
        );

        #[derive(bevy_ecs::system::SystemParam)]
        pub(crate) struct SceneBodies<'w, 's> {
            descriptors: Query<'w, 's, Descriptors, Or<(With<$body>, With<$collider>)>>,
            characters: Query<'w, 's, (Entity, &'static $controller, &'static mut $motion)>,
            transforms: TransformHelper<'w, 's>,
            guids: Option<Res<'w, EntityGuidMap>>,
            extras: Extras<'w, 's>,
        }

        pub(crate) fn step(
            world: ResMut<$world>,
            mut scene: SceneBodies,
            gravity: Res<Gravity>,
            options: Res<PhysicsOptions>,
            time: Res<Time<Fixed>>,
            mut commands: Commands,
        ) {
            let world = world.into_inner();
            if !options.simulate && !options.debug {
                if !world.entries.is_empty() {
                    *world = $world::default();
                }
                return;
            }
            scene.extras.prepare();

            let mut seen = BTreeSet::new();
            let mut joints = BTreeMap::new();
            let mut descriptors: Vec<_> = scene.descriptors.iter().collect();
            descriptors.sort_unstable_by_key(|row| row.0);
            for (entity, &local, parent, body, collider, sensor, layers, material, joint) in
                descriptors
            {
                let transform = match parent {
                    Some(parent) => {
                        let Ok(global) = scene.transforms.compute_global_transform(parent.parent())
                        else {
                            continue;
                        };
                        global.mul_transform(local).compute_transform()
                    }
                    None => local,
                };
                if !valid_transform(&transform) {
                    continue;
                }
                seen.insert(entity);
                let body = body.copied().unwrap_or($body {
                    kind: BodyKind::Static,
                    ..Default::default()
                });
                let transform_pose = pose(transform);
                let entry = world.entries.entry(entity).or_insert_with(|| {
                    let handle = world.bodies.insert(
                        RigidBodyBuilder::new(mobility(body.kind))
                            .pose(transform_pose)
                            .linvel(vector(safe_vector(body.velocity)))
                            .gravity_scale(finite_or(body.gravity_scale, 1.))
                            .ccd_enabled(body.ccd),
                    );
                    BodyEntry {
                        handle,
                        body,
                        collider: None,
                        config: None,
                        last_local: local,
                        last_target: transform_pose,
                        previous: transform_pose,
                        current: transform_pose,
                    }
                });
                let rigid = &mut world.bodies[entry.handle];
                if entry.body != body {
                    if entry.body.kind != body.kind {
                        rigid.set_body_type(mobility(body.kind), true);
                    }
                    if entry.body.velocity != body.velocity || entry.body.kind != body.kind {
                        rigid.set_linvel(vector(safe_vector(body.velocity)), true);
                    }
                    rigid.set_gravity_scale(finite_or(body.gravity_scale, 1.), true);
                    rigid.enable_ccd(body.ccd);
                    entry.body = body;
                }

                let local_changed = local != entry.last_local;
                let target_changed = entry.last_target != transform_pose;
                if !options.simulate
                    || (body.kind == BodyKind::Static && target_changed)
                    || (body.kind == BodyKind::Dynamic && local_changed)
                    || (body.kind == BodyKind::Kinematic
                        && local_changed
                        && scene.characters.get(entity).is_ok())
                {
                    if rigid.position() != &transform_pose {
                        rigid.set_position(transform_pose, true);
                        world.query_needs_rebuild = true;
                    }
                    entry.previous = transform_pose;
                    entry.current = transform_pose;
                } else if body.kind == BodyKind::Kinematic
                    && (local_changed
                        || (target_changed
                            && parent.is_some()
                            && scene.characters.get(entity).is_err()))
                {
                    rigid.set_next_kinematic_position(transform_pose);
                }
                entry.last_local = local;
                entry.last_target = transform_pose;

                let config = collider.as_ref().map(|descriptor| ColliderConfig {
                    descriptor: &**descriptor,
                    material: material.copied().unwrap_or_default(),
                    layers: layers.copied().unwrap_or_default(),
                    sensor,
                    scale: dimensions(transform.scale),
                    asset_version: scene.extras.asset_version(descriptor),
                });
                let geometry_changed = match (&config, &entry.config) {
                    (None, None) => false,
                    (Some(next), Some(old)) => {
                        (collider.as_ref().is_some_and(|shape| shape.is_changed())
                            && next.descriptor != &old.descriptor)
                            || next.material != old.material
                            || next.layers != old.layers
                            || next.sensor != old.sensor
                            || !next.scale.abs_diff_eq(old.scale, 1e-5)
                            || next.asset_version != old.asset_version
                    }
                    _ => true,
                };
                if geometry_changed {
                    world.query_needs_rebuild = true;
                    if let Some(handle) = entry.collider.take() {
                        world
                            .colliders
                            .remove(handle, &mut world.islands, &mut world.bodies, true);
                    }
                    entry.collider = config.as_ref().and_then(|c| {
                        let builder = shape_builder(c.descriptor, c.scale, Some(&scene.extras))?;
                        Some(
                            world.colliders.insert_with_parent(
                                builder
                                    .friction(finite_or(c.material.friction, 0.5).max(0.))
                                    .restitution(
                                        finite_or(c.material.restitution, 0.).clamp(0., 1.),
                                    )
                                    .density(finite_or(c.material.density, 1.).max(0.001))
                                    .collision_groups(groups(c.layers))
                                    .sensor(c.sensor)
                                    .active_collision_types(if c.sensor {
                                        ActiveCollisionTypes::all()
                                    } else {
                                        ActiveCollisionTypes::default()
                                    })
                                    .user_data(entity.to_bits().into()),
                                entry.handle,
                                &mut world.bodies,
                            ),
                        )
                    });
                    entry.config = config.map(|c| ColliderConfig {
                        descriptor: c.descriptor.clone(),
                        material: c.material,
                        layers: c.layers,
                        sensor: c.sensor,
                        scale: c.scale,
                        asset_version: c.asset_version,
                    });
                }
                if let Some(joint) = joint
                    && let Some(target) = joint
                        .target
                        .and_then(|guid| scene.guids.as_ref()?.get_entity(guid))
                    && target != entity
                {
                    joints.insert(entity, (target, *joint));
                }
            }

            let removed: Vec<_> = world
                .entries
                .keys()
                .filter(|entity| !seen.contains(entity))
                .copied()
                .collect();
            for entity in removed {
                world.remove(entity);
            }
            world
                .joint_entries
                .retain(|entity, (target, joint, handle)| {
                    if joints.get(entity) == Some(&(*target, *joint))
                        && world.joints.get(*handle).is_some()
                    {
                        return true;
                    }
                    world.joints.remove(*handle, true);
                    false
                });
            for (entity, (target, joint)) in joints {
                if let (Some(a), Some(b)) = (world.entries.get(&entity), world.entries.get(&target))
                {
                    world.joint_entries.entry(entity).or_insert_with(|| {
                        (
                            target,
                            joint,
                            world
                                .joints
                                .insert(a.handle, b.handle, make_joint(&joint), true),
                        )
                    });
                }
            }

            world
                .bodies
                .propagate_modified_body_positions_to_colliders(&mut world.colliders);
            if !options.simulate {
                world.contacts.clear();
                world.overlaps.clear();
                return;
            }

            let dt = time.timestep().as_secs_f32();
            let g = safe_vector(gravity_vector(&gravity));
            let mut characters: Vec<_> = scene.characters.iter_mut().collect();
            characters.sort_unstable_by_key(|character| character.0);
            // The simulation broad-phase belongs to the last step. Refresh the character queries
            // when geometry is spawned, removed or teleported before the next simulation step.
            let current_bvh = (world.query_needs_rebuild && !characters.is_empty()).then(|| {
                Bvh::from_iter(
                    BvhBuildStrategy::Binned,
                    world.colliders.iter().map(|(handle, collider)| {
                        (handle.into_raw_parts().0 as usize, collider.compute_aabb())
                    }),
                )
            });
            for (entity, controller, mut motion) in characters {
                let Some(entry) = world
                    .entries
                    .get(&entity)
                    .filter(|e| e.body.kind == BodyKind::Kinematic)
                else {
                    continue;
                };
                let Some(collider) = entry
                    .collider
                    .and_then(|handle| world.colliders.get(handle))
                else {
                    continue;
                };
                if collider.is_sensor() {
                    continue;
                }
                let mut next = *world.bodies[entry.handle].position();
                let desired = desired_motion(controller, &mut motion, g, dt);
                let predicate = |_: ColliderHandle, other: &Collider| {
                    scene
                        .extras
                        .character_filter(controller, collider, desired, other)
                };
                let layer_filter = entry.config.as_ref().map(|c| c.layers);
                let filter = QueryFilter {
                    predicate: Some(&predicate),
                    ..world.filter(CastFilter {
                        exclude: Some(entity),
                        layers: layer_filter,
                        ..Default::default()
                    })
                };
                let mut query = world.query(filter);
                if let Some(bvh) = &current_bvh {
                    query.bvh = bvh;
                }
                let movement = make_controller(controller).move_shape(
                    dt,
                    &query,
                    collider.shape(),
                    &next,
                    vector(desired),
                    |_| {},
                );
                next.translation.vector += movement.translation;
                motion.grounded = movement.grounded;
                if motion.grounded {
                    motion.fall_speed = 0.;
                }
                world.bodies[entry.handle].set_next_kinematic_position(next);
            }
            world.pipeline.step(
                &vector(g),
                &IntegrationParameters {
                    dt,
                    length_unit: $length,
                    ..Default::default()
                },
                &mut world.islands,
                &mut world.broad,
                &mut world.narrow,
                &mut world.bodies,
                &mut world.colliders,
                &mut world.joints,
                &mut world.multibody,
                &mut world.ccd,
                &(),
                &(),
            );
            world.query_needs_rebuild = false;

            for entry in world.entries.values_mut() {
                entry.previous = entry.current;
                entry.current = *world.bodies[entry.handle].position();
            }
            let (contacts, overlaps) = world.pairs();
            for (&entities, phase) in transitions(&world.contacts, &contacts) {
                commands.trigger(crate::CollisionEvent {
                    entities,
                    phase,
                    dimension: PhysicsDimension::$dim,
                });
            }
            for (&entities, phase) in transitions(&world.overlaps, &overlaps) {
                commands.trigger(TriggerEvent {
                    entities,
                    phase,
                    dimension: PhysicsDimension::$dim,
                });
            }
            world.contacts = contacts;
            world.overlaps = overlaps;
        }

        fn safe_vector(v: $vec) -> $vec {
            if v.is_finite() {
                v
            } else {
                <$vec>::ZERO
            }
        }

        fn transitions<'a>(
            old: &'a BTreeSet<[Entity; 2]>,
            next: &'a BTreeSet<[Entity; 2]>,
        ) -> impl Iterator<Item = (&'a [Entity; 2], ContactPhase)> + 'a {
            old.difference(next)
                .map(|p| (p, ContactPhase::Stopped))
                .chain(next.difference(old).map(|p| (p, ContactPhase::Started)))
        }

        pub(crate) fn interpolate(
            mut world: ResMut<$world>,
            options: Res<PhysicsOptions>,
            time: Res<Time<Fixed>>,
            mut transforms: Query<(&mut Transform, Option<&ChildOf>)>,
            parents: Query<&GlobalTransform>,
        ) {
            if !options.simulate {
                return;
            }
            let alpha = time.overstep_fraction().clamp(0., 1.);
            for (&entity, entry) in &mut world.entries {
                if entry.body.kind == BodyKind::Static {
                    continue;
                }
                let Ok((mut local, parent)) = transforms.get_mut(entity) else {
                    continue;
                };
                if *local != entry.last_local {
                    continue;
                }
                let parent = parent.and_then(|parent| parents.get(parent.parent()).ok());
                let origin = parent.map_or(*local, |p| p.mul_transform(*local).compute_transform());
                let mut transform =
                    apply_pose(entry.previous.lerp_slerp(&entry.current, alpha), origin);
                if let Some(parent) = parent {
                    transform = GlobalTransform::from(transform).reparented_to(parent);
                }
                transform.scale = local.scale;
                if valid_transform(&transform) {
                    local.set_if_neq(transform);
                    entry.last_local = transform;
                }
            }
        }

        struct Lines<'a, 'w, 's>(&'a mut Gizmos<'w, 's, PhysicsGizmos>);

        impl DebugRenderBackend for Lines<'_, '_, '_> {
            fn draw_line(
                &mut self,
                _: DebugRenderObject<'_>,
                a: Point<f32>,
                b: Point<f32>,
                color: DebugColor,
            ) {
                self.0.line(
                    debug_point(a),
                    debug_point(b),
                    Color::hsla(color[0], color[1], color[2], color[3]),
                );
            }
        }

        pub(crate) fn draw_debug(
            world: ResMut<$world>,
            options: Res<PhysicsOptions>,
            mut gizmos: Gizmos<PhysicsGizmos>,
        ) {
            if !options.debug {
                return;
            }
            let world = world.into_inner();
            world.debug.mode = DebugRenderMode::COLLIDER_SHAPES | DebugRenderMode::JOINTS;
            world.debug.render(
                &mut Lines(&mut gizmos),
                &world.bodies,
                &world.colliders,
                &world.joints,
                &world.multibody,
                &world.narrow,
            );
        }
    };
}

pub(crate) use backend;

pub(crate) fn finite_or(value: f32, default: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        default
    }
}
