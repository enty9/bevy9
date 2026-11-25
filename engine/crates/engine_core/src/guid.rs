//! Stable entity identifiers (`EntityGuid`) and bidirectional world lookup (`EntityGuidMap`).

use core::fmt;
use std::collections::HashMap;

use bevy_ecs::{
    component::Component, entity::Entity, reflect::ReflectComponent, resource::Resource,
    world::World,
};
use bevy_reflect::{std_traits::ReflectDefault, Reflect, ReflectDeserialize, ReflectSerialize};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable persistent identifier assigned to engine-managed entities.
///
/// Unlike ephemeral ECS [`Entity`] handles, an `EntityGuid` remains stable across
/// scene saves, loads, and Play-In-Editor snapshots.
#[derive(
    Component,
    Reflect,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[reflect(Component, Default, Serialize, Deserialize, PartialEq, Hash)]
pub struct EntityGuid(Uuid);

impl EntityGuid {
    /// Generates a new random v4 `EntityGuid`.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wraps an existing [`Uuid`] as an `EntityGuid`.
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Parses an `EntityGuid` from a canonical UUID string.
    pub fn parse_str(input: &str) -> Result<Self, uuid::Error> {
        Uuid::parse_str(input).map(Self)
    }

    /// Returns the underlying [`Uuid`] value.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for EntityGuid {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EntityGuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Uuid> for EntityGuid {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

impl From<EntityGuid> for Uuid {
    fn from(value: EntityGuid) -> Self {
        value.0
    }
}

/// Bidirectional lookup index between [`EntityGuid`] and active ECS [`Entity`] handles.
///
/// Automatically synchronized through component lifecycle hooks registered by
/// [`register_guid_hooks`].
#[derive(Resource, Default, Debug)]
pub struct EntityGuidMap {
    guid_to_entity: HashMap<EntityGuid, Entity>,
    entity_to_guid: HashMap<Entity, EntityGuid>,
}

impl EntityGuidMap {
    /// Returns the [`Entity`] currently associated with `guid`, if any.
    #[must_use]
    pub fn get_entity(&self, guid: EntityGuid) -> Option<Entity> {
        self.guid_to_entity.get(&guid).copied()
    }

    /// Returns the [`EntityGuid`] currently associated with `entity`, if any.
    #[must_use]
    pub fn get_guid(&self, entity: Entity) -> Option<EntityGuid> {
        self.entity_to_guid.get(&entity).copied()
    }

    /// Returns `true` if `guid` is currently indexed in the map.
    #[must_use]
    pub fn contains_guid(&self, guid: EntityGuid) -> bool {
        self.guid_to_entity.contains_key(&guid)
    }

    /// Returns the number of indexed `(EntityGuid, Entity)` pairs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.guid_to_entity.len()
    }

    /// Returns `true` when no entity GUIDs are currently indexed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.guid_to_entity.is_empty()
    }

    /// Iterates over all `(EntityGuid, Entity)` pairs stored in the map.
    pub fn iter(&self) -> impl Iterator<Item = (EntityGuid, Entity)> + '_ {
        self.guid_to_entity
            .iter()
            .map(|(&guid, &entity)| (guid, entity))
    }

    pub(crate) fn insert_mapping(&mut self, entity: Entity, guid: EntityGuid) {
        if let Some(previous_guid) = self.entity_to_guid.insert(entity, guid)
            && previous_guid != guid
            && self.guid_to_entity.get(&previous_guid) == Some(&entity)
        {
            self.guid_to_entity.remove(&previous_guid);
        }

        if let Some(previous_entity) = self.guid_to_entity.insert(guid, entity)
            && previous_entity != entity
        {
            tracing::warn!(
                guid = %guid,
                previous_entity = ?previous_entity,
                new_entity = ?entity,
                "Duplicate EntityGuid inserted; updating EntityGuidMap to the latest entity"
            );
            if self.entity_to_guid.get(&previous_entity) == Some(&guid) {
                self.entity_to_guid.remove(&previous_entity);
            }
        }
    }

    pub(crate) fn remove_mapping(&mut self, entity: Entity, guid: EntityGuid) {
        if self.entity_to_guid.get(&entity) == Some(&guid) {
            self.entity_to_guid.remove(&entity);
        }
        if self.guid_to_entity.get(&guid) == Some(&entity) {
            self.guid_to_entity.remove(&guid);
        }
    }
}

/// Registers `on_insert` and `on_discard` lifecycle hooks on [`EntityGuid`] to keep
/// [`EntityGuidMap`] synchronized automatically.
///
/// If [`EntityGuidMap`] is not present in the [`World`], the hooks exit safely.
pub fn register_guid_hooks(world: &mut World) {
    let hooks = world.register_component_hooks::<EntityGuid>();
    hooks.try_on_insert(|mut world, context| {
        let Some(guid) = world.get::<EntityGuid>(context.entity).copied() else {
            return;
        };
        if let Some(mut map) = world.get_resource_mut::<EntityGuidMap>() {
            map.insert_mapping(context.entity, guid);
        }
    });
    hooks.try_on_discard(|mut world, context| {
        let Some(guid) = world.get::<EntityGuid>(context.entity).copied() else {
            return;
        };
        if let Some(mut map) = world.get_resource_mut::<EntityGuidMap>() {
            map.remove_mapping(context.entity, guid);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_app::App;

    #[test]
    fn guid_generation_parse_display_and_json_roundtrip() {
        let first = EntityGuid::new();
        let second = EntityGuid::new();
        assert_ne!(first, second);

        let text = first.to_string();
        let parsed = EntityGuid::parse_str(&text).expect("valid UUID string must parse");
        assert_eq!(parsed, first);

        let json = serde_json::to_string(&first).expect("EntityGuid should serialize");
        let deserialized: EntityGuid =
            serde_json::from_str(&json).expect("EntityGuid should deserialize");
        assert_eq!(deserialized, first);
    }

    #[test]
    fn guid_map_syncs_with_spawn_overwrite_remove_and_despawn() {
        let mut app = App::new();
        app.init_resource::<EntityGuidMap>();
        register_guid_hooks(app.world_mut());

        let guid_a = EntityGuid::new();
        let entity = app.world_mut().spawn(guid_a).id();

        {
            let map = app.world().resource::<EntityGuidMap>();
            assert_eq!(map.len(), 1);
            assert_eq!(map.get_entity(guid_a), Some(entity));
            assert_eq!(map.get_guid(entity), Some(guid_a));
            assert!(map.contains_guid(guid_a));
        }

        // Overwrite with a new GUID on the same entity: old GUID must be cleaned up.
        let guid_b = EntityGuid::new();
        app.world_mut().entity_mut(entity).insert(guid_b);
        {
            let map = app.world().resource::<EntityGuidMap>();
            assert_eq!(map.len(), 1);
            assert_eq!(map.get_entity(guid_a), None);
            assert_eq!(map.get_entity(guid_b), Some(entity));
            assert_eq!(map.get_guid(entity), Some(guid_b));
        }

        // Remove component: entity must disappear from both maps.
        app.world_mut().entity_mut(entity).remove::<EntityGuid>();
        {
            let map = app.world().resource::<EntityGuidMap>();
            assert!(map.is_empty());
            assert_eq!(map.get_entity(guid_b), None);
            assert_eq!(map.get_guid(entity), None);
        }

        // Re-insert and despawn entity: must clean up on despawn.
        let guid_c = EntityGuid::new();
        app.world_mut().entity_mut(entity).insert(guid_c);
        assert_eq!(app.world().resource::<EntityGuidMap>().len(), 1);

        app.world_mut().despawn(entity);
        let map = app.world().resource::<EntityGuidMap>();
        assert!(map.is_empty());
        assert_eq!(map.get_entity(guid_c), None);
        assert_eq!(map.get_guid(entity), None);
    }

    #[test]
    fn duplicate_guid_handles_conflict_and_preserves_latest_entity_on_old_despawn() {
        let mut app = App::new();
        app.init_resource::<EntityGuidMap>();
        register_guid_hooks(app.world_mut());

        let shared_guid = EntityGuid::new();
        let first = app.world_mut().spawn(shared_guid).id();
        let second = app.world_mut().spawn(shared_guid).id();

        {
            let map = app.world().resource::<EntityGuidMap>();
            assert_eq!(map.len(), 1);
            assert_eq!(map.get_entity(shared_guid), Some(second));
            assert_eq!(map.get_guid(second), Some(shared_guid));
            assert_eq!(map.get_guid(first), None);
        }

        // Despawning the first entity must NOT evict `second` from `guid_to_entity`.
        app.world_mut().despawn(first);
        let map = app.world().resource::<EntityGuidMap>();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get_entity(shared_guid), Some(second));
        assert_eq!(map.get_guid(second), Some(shared_guid));
    }
}
