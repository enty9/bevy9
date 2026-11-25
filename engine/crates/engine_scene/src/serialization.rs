//! Versioned `.level.ron` serialization, deserialization, and scene tree building.

use core::{any::TypeId, fmt, str::FromStr};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use bevy_asset::{AssetPath, AssetServer, LoadFromPath, UntypedHandle};
use bevy_camera::visibility::Visibility;
use bevy_ecs::{
    archetype::ArchetypeEntity,
    component::Component,
    entity::{Entity, EntityHashMap},
    hierarchy::{ChildOf, Children},
    name::Name,
    reflect::AppTypeRegistry,
    resource::Resource,
    world::World,
};
use bevy_reflect::TypeRegistry;
use bevy_world_serialization::{
    serde::{DynamicWorldSerializer, WorldDeserializer},
    DynamicWorld, DynamicWorldBuilder, WorldFilter, WorldInstanceSpawnError,
};
use engine_core::{EntityGuid, EntityGuidMap, WorldMode};
use engine_protocol::SceneTreeNode;
use ron::ser::PrettyConfig;
use serde::{
    de::{DeserializeSeed, MapAccess, SeqAccess, Visitor},
    ser::SerializeStruct,
    Deserialize, Deserializer, Serialize, Serializer,
};
use thiserror::Error;

use crate::components::{ActiveScene, SceneEntity};

/// Supported `.level.ron` file format version.
pub const CURRENT_LEVEL_VERSION: u32 = 1;

/// Metadata stored alongside the serialized world inside a `.level.ron` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelMetadata {
    /// Human-readable scene name.
    pub name: String,
    /// Dimensional mode (`2d`, `3d`, or `hybrid`) associated with the scene.
    pub world_mode: WorldMode,
}

/// Allow-list of component types included when saving scenes to `.level.ron`.
///
/// Restricting serialization to authored scene descriptors prevents runtime-only
/// handles (`Mesh3d`, `GlobalTransform`, `Children`, `RenderTarget`) from leaking
/// into `.level.ron` files.
#[derive(Resource, Debug, Clone, Default)]
pub struct SceneSerializationFilter {
    allowed_components: HashSet<TypeId>,
}

impl SceneSerializationFilter {
    /// Registers component `T` as serializable in `.level.ron` scene files.
    pub fn allow<T: Component>(&mut self) -> &mut Self {
        self.allowed_components.insert(TypeId::of::<T>());
        self
    }

    /// Registers a component by its [`TypeId`] as serializable in `.level.ron` scene files.
    pub fn allow_by_id(&mut self, type_id: TypeId) -> &mut Self {
        self.allowed_components.insert(type_id);
        self
    }

    /// Returns `true` if component `T` is currently in the scene allow-list.
    #[must_use]
    pub fn contains<T: Component>(&self) -> bool {
        self.allowed_components.contains(&TypeId::of::<T>())
    }

    /// Converts this allow-list into a [`WorldFilter`] for [`DynamicWorldBuilder`].
    #[must_use]
    pub fn to_world_filter(&self) -> WorldFilter {
        let mut filter = WorldFilter::deny_all();
        for &type_id in &self.allowed_components {
            filter = filter.allow_by_id(type_id);
        }
        filter
    }
}

/// Errors returned when saving or loading `.level.ron` scene files.
#[derive(Debug, Error)]
pub enum SceneError {
    /// Filesystem I/O failure.
    #[error("failed to access level file '{}': {source}", path.display())]
    Io {
        /// File path involved in the operation.
        path: PathBuf,
        /// Source I/O error.
        #[source]
        source: std::io::Error,
    },
    /// RON parsing error with source span details.
    #[error("failed to parse level RON: {0}")]
    RonParse(#[from] ron::error::SpannedError),
    /// RON serialization error.
    #[error("failed to serialize level RON: {0}")]
    RonSerialize(#[from] ron::Error),
    /// Level file specifies an unsupported `schema_version`.
    #[error("unsupported level schema_version {found} (expected {CURRENT_LEVEL_VERSION})")]
    UnsupportedVersion {
        /// Version integer found in the file.
        found: u32,
    },
    /// Level file has invalid metadata such as an unknown `world_mode`.
    #[error("invalid level metadata: {0}")]
    InvalidMetadata(String),
    /// Failed to instantiate a deserialized [`DynamicWorld`] into the ECS [`World`].
    #[error("failed to instantiate scene world: {0}")]
    Spawn(#[from] WorldInstanceSpawnError),
}

/// Removes all entities marked with [`SceneEntity`] from `world`.
pub fn clear_scene_entities(world: &mut World) {
    let scene_entities: Vec<Entity> = world
        .archetypes()
        .iter()
        .flat_map(|archetype| archetype.entities())
        .map(ArchetypeEntity::id)
        .filter(|&entity| world.entity(entity).contains::<SceneEntity>())
        .collect();

    for entity in scene_entities {
        if world.get_entity(entity).is_ok() {
            world.despawn(entity);
        }
    }
}

/// Creates a brand new empty scene, clearing existing [`SceneEntity`] instances and updating [`ActiveScene`].
pub fn reset_to_new_scene(world: &mut World, name: impl Into<String>, world_mode: WorldMode) {
    clear_scene_entities(world);
    let name = name.into();
    world.insert_resource(world_mode);
    world.insert_resource(ActiveScene {
        path: None,
        name,
        world_mode,
        dirty: false,
    });
}

/// Serializes all [`SceneEntity`] instances in `world` to a formatted `.level.ron` string.
pub fn save_level_to_string(world: &World, metadata: &LevelMetadata) -> Result<String, SceneError> {
    let type_registry = world.resource::<AppTypeRegistry>().read();
    let filter = world
        .get_resource::<SceneSerializationFilter>()
        .map(SceneSerializationFilter::to_world_filter)
        .unwrap_or_else(WorldFilter::allow_all);

    let scene_entities: Vec<Entity> = world
        .archetypes()
        .iter()
        .flat_map(|archetype| archetype.entities())
        .map(ArchetypeEntity::id)
        .filter(|&entity| world.entity(entity).contains::<SceneEntity>())
        .collect();

    let dynamic_world = DynamicWorldBuilder::from_world(world, &type_registry)
        .with_component_filter(filter)
        .with_resource_filter(WorldFilter::deny_all())
        .extract_entities(scene_entities.into_iter())
        .build();

    let serializer = LevelFileSerializer {
        schema_version: CURRENT_LEVEL_VERSION,
        name: &metadata.name,
        world_mode: metadata.world_mode,
        world: DynamicWorldSerializer::new(&dynamic_world, &type_registry),
    };

    Ok(ron::ser::to_string_pretty(
        &serializer,
        PrettyConfig::default(),
    )?)
}

/// Serializes all [`SceneEntity`] instances in `world` and writes them to `path`.
pub fn save_level_to_file(
    world: &mut World,
    metadata: &LevelMetadata,
    path: impl AsRef<Path>,
) -> Result<(), SceneError> {
    let path = path.as_ref();
    let encoded = save_level_to_string(world, metadata)?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|source| SceneError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(path, encoded).map_err(|source| SceneError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    world.insert_resource(metadata.world_mode);
    world.insert_resource(ActiveScene {
        path: Some(path.to_path_buf()),
        name: metadata.name.clone(),
        world_mode: metadata.world_mode,
        dirty: false,
    });
    Ok(())
}

/// Deserializes a `.level.ron` string and replaces the current scene in `world`.
pub fn load_level_from_str(world: &mut World, ron_str: &str) -> Result<LevelMetadata, SceneError> {
    let type_registry = world.resource::<AppTypeRegistry>().clone();
    let asset_server = world.get_resource::<AssetServer>().cloned();

    let parsed = {
        let registry_guard = type_registry.read();
        let mut noop_loader = NoopLoadFromPath;
        let mut active_server = asset_server;
        let load_from_path: &mut dyn LoadFromPath = match active_server.as_mut() {
            Some(server) => server,
            None => &mut noop_loader,
        };

        let mut deserializer = ron::de::Deserializer::from_str(ron_str)?;
        let level_deserializer = LevelFileDeserializer {
            type_registry: &registry_guard,
            load_from_path,
        };
        level_deserializer
            .deserialize(&mut deserializer)
            .map_err(|err| deserializer.span_error(err))?
    };

    if parsed.schema_version != CURRENT_LEVEL_VERSION {
        return Err(SceneError::UnsupportedVersion {
            found: parsed.schema_version,
        });
    }

    let world_mode = WorldMode::from_str(&parsed.world_mode).map_err(|_| {
        SceneError::InvalidMetadata(format!("invalid world_mode '{}'", parsed.world_mode))
    })?;

    let metadata = LevelMetadata {
        name: parsed.name,
        world_mode,
    };

    clear_scene_entities(world);

    let mut entity_map = EntityHashMap::default();
    {
        let registry_guard = type_registry.read();
        parsed
            .dynamic_world
            .write_to_world_with(world, &mut entity_map, &registry_guard)?;
    }

    let spawned_entities: Vec<Entity> = entity_map.values().copied().collect();
    restore_hierarchy_children(world, &spawned_entities);
    ensure_scene_guids(world, &spawned_entities);

    world.insert_resource(metadata.world_mode);
    world.insert_resource(ActiveScene {
        path: None,
        name: metadata.name.clone(),
        world_mode: metadata.world_mode,
        dirty: false,
    });

    Ok(metadata)
}

/// Reads and loads a `.level.ron` file from `path` into `world`.
pub fn load_level_from_file(
    world: &mut World,
    path: impl AsRef<Path>,
) -> Result<LevelMetadata, SceneError> {
    let path = path.as_ref();
    let ron_str = fs::read_to_string(path).map_err(|source| SceneError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = load_level_from_str(world, &ron_str)?;
    if let Some(mut active_scene) = world.get_resource_mut::<ActiveScene>() {
        active_scene.path = Some(path.to_path_buf());
    }
    Ok(metadata)
}

fn restore_hierarchy_children(world: &mut World, spawned_entities: &[Entity]) {
    let child_links: Vec<(Entity, Entity)> = spawned_entities
        .iter()
        .filter_map(|&entity| {
            let parent = world.get::<ChildOf>(entity)?.parent();
            let already_linked = world
                .get::<Children>(parent)
                .is_some_and(|children| children.contains(&entity));
            if already_linked {
                None
            } else {
                Some((entity, parent))
            }
        })
        .collect();

    for (entity, parent) in child_links {
        if world.get_entity(parent).is_ok() {
            // `DynamicWorld::write_to_world_with` uses `RelationshipHookMode::Skip`,
            // so removing and reinserting `ChildOf` triggers Bevy's relationship hook
            // to populate `Children` on the parent entity.
            world.entity_mut(entity).remove::<ChildOf>();
            world.entity_mut(entity).insert(ChildOf(parent));
        }
    }
}

fn ensure_scene_guids(world: &mut World, spawned_entities: &[Entity]) {
    let missing_guid_entities: Vec<Entity> = spawned_entities
        .iter()
        .copied()
        .filter(|&entity| {
            world.entity(entity).contains::<SceneEntity>()
                && !world.entity(entity).contains::<EntityGuid>()
        })
        .collect();

    for entity in missing_guid_entities {
        world.entity_mut(entity).insert(EntityGuid::new());
    }

    // Ensure `EntityGuidMap` indexes every entity even if deserialized without an initialized map.
    if !world.contains_resource::<EntityGuidMap>() {
        world.init_resource::<EntityGuidMap>();
    }
}

/// Builds a deterministic hierarchical snapshot of all [`SceneEntity`] instances in `world`.
#[must_use]
pub fn build_scene_tree(world: &World) -> Vec<SceneTreeNode> {
    let type_registry = world.get_resource::<AppTypeRegistry>().map(|r| r.read());

    struct RawNode {
        guid: EntityGuid,
        entity_bits: u64,
        name: String,
        parent_guid: Option<EntityGuid>,
        visible: bool,
        components: Vec<String>,
    }

    let mut nodes_by_guid: HashMap<EntityGuid, RawNode> = HashMap::new();
    let mut children_map: HashMap<EntityGuid, Vec<EntityGuid>> = HashMap::new();
    let mut all_guids: HashSet<EntityGuid> = HashSet::new();

    for archetype in world.archetypes().iter() {
        for arch_entity in archetype.entities() {
            let entity = arch_entity.id();
            let entity_ref = world.entity(entity);
            if !entity_ref.contains::<SceneEntity>() {
                continue;
            }
            let Some(&guid) = entity_ref.get::<EntityGuid>() else {
                continue;
            };

            let name = entity_ref
                .get::<Name>()
                .map(|n| n.as_str().to_owned())
                .unwrap_or_else(|| format!("Entity {}", entity.index()));

            let visible = entity_ref
                .get::<Visibility>()
                .is_none_or(|vis| *vis != Visibility::Hidden);

            let parent_guid = entity_ref
                .get::<ChildOf>()
                .and_then(|child_of| world.get::<EntityGuid>(child_of.parent()).copied());

            let mut components = Vec::new();
            for &component_id in archetype.components() {
                let Some(info) = world.components().get_info(component_id) else {
                    continue;
                };
                if let Some(type_id) = info.type_id()
                    && let Some(registry) = type_registry.as_deref()
                    && let Some(registration) = registry.get(type_id)
                {
                    components.push(registration.type_info().type_path().to_owned());
                }
            }
            components.sort();

            all_guids.insert(guid);
            if let Some(p_guid) = parent_guid {
                children_map.entry(p_guid).or_default().push(guid);
            }
            nodes_by_guid.insert(
                guid,
                RawNode {
                    guid,
                    entity_bits: entity.to_bits(),
                    name,
                    parent_guid,
                    visible,
                    components,
                },
            );
        }
    }

    fn assemble_node(
        guid: EntityGuid,
        nodes_by_guid: &HashMap<EntityGuid, RawNode>,
        children_map: &HashMap<EntityGuid, Vec<EntityGuid>>,
    ) -> SceneTreeNode {
        let raw = &nodes_by_guid[&guid];
        let mut child_guids = children_map.get(&guid).cloned().unwrap_or_default();
        child_guids.sort_by(|a, b| {
            let left = &nodes_by_guid[a];
            let right = &nodes_by_guid[b];
            left.name.cmp(&right.name).then_with(|| a.cmp(b))
        });

        let children = child_guids
            .into_iter()
            .map(|child_guid| assemble_node(child_guid, nodes_by_guid, children_map))
            .collect();

        SceneTreeNode {
            guid: raw.guid,
            entity_bits: raw.entity_bits,
            name: raw.name.clone(),
            parent_guid: raw.parent_guid,
            visible: raw.visible,
            children,
            components: raw.components.clone(),
        }
    }

    let mut root_guids: Vec<EntityGuid> = nodes_by_guid
        .values()
        .filter(|node| {
            node.parent_guid
                .is_none_or(|parent_guid| !all_guids.contains(&parent_guid))
        })
        .map(|node| node.guid)
        .collect();

    root_guids.sort_by(|a, b| {
        let left = &nodes_by_guid[a];
        let right = &nodes_by_guid[b];
        left.name.cmp(&right.name).then_with(|| a.cmp(b))
    });

    root_guids
        .into_iter()
        .map(|guid| assemble_node(guid, &nodes_by_guid, &children_map))
        .collect()
}

struct NoopLoadFromPath;

impl LoadFromPath for NoopLoadFromPath {
    fn load_from_path_erased(
        &mut self,
        type_id: TypeId,
        _path: AssetPath<'static>,
    ) -> UntypedHandle {
        UntypedHandle::default_for_type(type_id)
    }
}

struct LevelFileSerializer<'a> {
    schema_version: u32,
    name: &'a str,
    world_mode: WorldMode,
    world: DynamicWorldSerializer<'a>,
}

impl Serialize for LevelFileSerializer<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("LevelFile", 4)?;
        state.serialize_field("schema_version", &self.schema_version)?;
        state.serialize_field("name", self.name)?;
        state.serialize_field("world_mode", self.world_mode.as_str())?;
        state.serialize_field("world", &self.world)?;
        state.end()
    }
}

struct ParsedLevelFile {
    schema_version: u32,
    name: String,
    world_mode: String,
    dynamic_world: DynamicWorld,
}

struct LevelFileDeserializer<'a> {
    type_registry: &'a TypeRegistry,
    load_from_path: &'a mut dyn LoadFromPath,
}

impl<'de> DeserializeSeed<'de> for LevelFileDeserializer<'_> {
    type Value = ParsedLevelFile;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_struct(
            "LevelFile",
            &["schema_version", "name", "world_mode", "world"],
            LevelFileVisitor {
                type_registry: self.type_registry,
                load_from_path: self.load_from_path,
            },
        )
    }
}

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum LevelField {
    SchemaVersion,
    Name,
    WorldMode,
    World,
}

struct LevelFileVisitor<'a> {
    type_registry: &'a TypeRegistry,
    load_from_path: &'a mut dyn LoadFromPath,
}

impl<'de> Visitor<'de> for LevelFileVisitor<'_> {
    type Value = ParsedLevelFile;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Bevy9 `.level.ron` struct")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let schema_version: u32 = seq
            .next_element()?
            .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;
        let name: String = seq
            .next_element()?
            .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;
        let world_mode: String = seq
            .next_element()?
            .ok_or_else(|| serde::de::Error::invalid_length(2, &self))?;
        let dynamic_world = seq
            .next_element_seed(WorldDeserializer {
                type_registry: self.type_registry,
                load_from_path: self.load_from_path,
            })?
            .ok_or_else(|| serde::de::Error::invalid_length(3, &self))?;

        Ok(ParsedLevelFile {
            schema_version,
            name,
            world_mode,
            dynamic_world,
        })
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut schema_version = None;
        let mut name = None;
        let mut world_mode = None;
        let mut dynamic_world = None;

        while let Some(key) = map.next_key::<LevelField>()? {
            match key {
                LevelField::SchemaVersion => {
                    if schema_version.is_some() {
                        return Err(serde::de::Error::duplicate_field("schema_version"));
                    }
                    schema_version = Some(map.next_value()?);
                }
                LevelField::Name => {
                    if name.is_some() {
                        return Err(serde::de::Error::duplicate_field("name"));
                    }
                    name = Some(map.next_value()?);
                }
                LevelField::WorldMode => {
                    if world_mode.is_some() {
                        return Err(serde::de::Error::duplicate_field("world_mode"));
                    }
                    world_mode = Some(map.next_value()?);
                }
                LevelField::World => {
                    if dynamic_world.is_some() {
                        return Err(serde::de::Error::duplicate_field("world"));
                    }
                    dynamic_world = Some(map.next_value_seed(WorldDeserializer {
                        type_registry: self.type_registry,
                        load_from_path: self.load_from_path,
                    })?);
                }
            }
        }

        Ok(ParsedLevelFile {
            schema_version: schema_version
                .ok_or_else(|| serde::de::Error::missing_field("schema_version"))?,
            name: name.ok_or_else(|| serde::de::Error::missing_field("name"))?,
            world_mode: world_mode.ok_or_else(|| serde::de::Error::missing_field("world_mode"))?,
            dynamic_world: dynamic_world.ok_or_else(|| serde::de::Error::missing_field("world"))?,
        })
    }
}
