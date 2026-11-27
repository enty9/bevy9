//! Deterministic Play-In-Editor (PIE) scene snapshot capture and full rollback restore.

use std::collections::HashSet;

use bevy_ecs::{
    archetype::ArchetypeEntity,
    component::Component,
    entity::Entity,
    reflect::ReflectComponent,
    resource::Resource,
    world::World,
};
use bevy_reflect::{std_traits::ReflectDefault, Reflect};
use engine_core::{EntityGuid, WorldMode};
use engine_physics::{PhysicsWorld2D, PhysicsWorld3D};
use serde::{Deserialize, Serialize};

use crate::{
    components::{ActiveScene, SceneEntity},
    hydration::{HydratedGltfModelRoot, HydratedTilemapTile},
    prefab::{PrefabInstance, PrefabMember},
    serialization::{
        collect_subtree_entities, load_level_from_str, save_level_to_string, LevelMetadata,
        SceneError,
    },
};

/// Marker component for transient runtime-only entities spawned during Play-In-Editor (PIE)
/// that do not carry [`SceneEntity`] (such as projectiles, VFX, or script-spawned actors).
///
/// Every entity carrying `PieRuntimeEntity` (along with its entire descendant subtree) is
/// guaranteed to be despawned by [`clear_pie_scene_entities`] and [`restore_pie_snapshot`].
#[derive(
    Component, Reflect, Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
#[reflect(Component, Default)]
pub struct PieRuntimeEntity;

/// Complete pre-PIE scene snapshot used to roll back a Play-In-Editor session deterministically.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct PieSnapshot {
    /// Serialized `.level.ron` payload capturing every [`SceneEntity`], its [`EntityGuid`],
    /// `ChildOf` hierarchy, and all components allowed by [`crate::SceneSerializationFilter`].
    pub level_ron: String,
    /// Exact [`ActiveScene`] metadata (including disk path, world mode, and unsaved `dirty` flag).
    pub active_scene: ActiveScene,
    /// Sorted list of persistent [`EntityGuid`]s belonging to all captured [`SceneEntity`] instances.
    pub entity_guids: Vec<EntityGuid>,
    /// Total number of [`SceneEntity`] instances captured in this snapshot.
    pub entity_count: usize,
}

/// Despawns all scene entities, hydrated scene children, prefab nodes, and [`PieRuntimeEntity`]
/// subtrees from `world`, leaving non-scene editor/runtime infrastructure entities intact.
///
/// Returns the total number of entities despawned.
pub fn clear_pie_scene_entities(world: &mut World) -> usize {
    let roots: Vec<Entity> = world
        .archetypes()
        .iter()
        .flat_map(|archetype| archetype.entities())
        .map(ArchetypeEntity::id)
        .filter(|&entity| {
            let entity_ref = world.entity(entity);
            entity_ref.contains::<SceneEntity>()
                || entity_ref.contains::<EntityGuid>()
                || entity_ref.contains::<PieRuntimeEntity>()
                || entity_ref.contains::<HydratedGltfModelRoot>()
                || entity_ref.contains::<HydratedTilemapTile>()
                || entity_ref.contains::<PrefabInstance>()
                || entity_ref.contains::<PrefabMember>()
        })
        .collect();

    let mut to_despawn = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        for entity in collect_subtree_entities(world, root) {
            if seen.insert(entity) {
                to_despawn.push(entity);
            }
        }
    }

    let mut despawned = 0usize;
    for entity in to_despawn {
        if world.get_entity(entity).is_ok() {
            world.despawn(entity);
            despawned += 1;
        }
    }
    despawned
}

/// Captures a deterministic [`PieSnapshot`] of all [`SceneEntity`] instances and active scene
/// metadata in `world` prior to entering Play-In-Editor.
pub fn capture_pie_snapshot(world: &World) -> Result<PieSnapshot, SceneError> {
    let active_scene = world
        .get_resource::<ActiveScene>()
        .cloned()
        .unwrap_or_else(|| ActiveScene {
            world_mode: world
                .get_resource::<WorldMode>()
                .copied()
                .unwrap_or_default(),
            ..Default::default()
        });

    let mut entity_guids = Vec::new();
    let mut entity_count = 0usize;
    for archetype in world.archetypes().iter() {
        for arch_entity in archetype.entities() {
            let entity = arch_entity.id();
            let entity_ref = world.entity(entity);
            if entity_ref.contains::<SceneEntity>() {
                entity_count += 1;
                if let Some(&guid) = entity_ref.get::<EntityGuid>() {
                    entity_guids.push(guid);
                }
            }
        }
    }
    entity_guids.sort_unstable();

    let metadata = LevelMetadata {
        name: active_scene.name.clone(),
        world_mode: active_scene.world_mode,
    };
    let level_ron = save_level_to_string(world, &metadata)?;

    Ok(PieSnapshot {
        level_ron,
        active_scene,
        entity_guids,
        entity_count,
    })
}

/// Rolls `world` back to the exact pre-PIE scene state stored in `snapshot`.
///
/// Purges all current scene/runtime subtrees (preventing orphaned or leaked entities),
/// resets 2D/3D Rapier physics worlds if present, respawns all serialized scene entities
/// with their original [`EntityGuid`]s, components, and `ChildOf`/`Children` links, and
/// restores exact [`ActiveScene`] metadata (including `path` and `dirty`).
pub fn restore_pie_snapshot(world: &mut World, snapshot: &PieSnapshot) -> Result<(), SceneError> {
    clear_pie_scene_entities(world);

    if let Some(mut physics_2d) = world.get_resource_mut::<PhysicsWorld2D>() {
        *physics_2d = PhysicsWorld2D::default();
    }
    if let Some(mut physics_3d) = world.get_resource_mut::<PhysicsWorld3D>() {
        *physics_3d = PhysicsWorld3D::default();
    }

    load_level_from_str(world, &snapshot.level_ron)?;

    world.insert_resource(snapshot.active_scene.world_mode);
    world.insert_resource(snapshot.active_scene.clone());

    Ok(())
}
