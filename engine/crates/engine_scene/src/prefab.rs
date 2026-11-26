//! Reusable `.prefab.ron` assets, `PrefabInstance` override tracking, Apply, and Revert.

use core::{any::TypeId, fmt};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use bevy_asset::{AssetServer, LoadFromPath};
use bevy_ecs::{
    component::Component,
    entity::{Entity, EntityHashMap},
    hierarchy::ChildOf,
    reflect::{AppTypeRegistry, ReflectComponent},
    world::World,
};
use bevy_math::Vec3;
use bevy_reflect::{std_traits::ReflectDefault, FromReflect, Reflect, ReflectDeserialize, ReflectSerialize, TypeRegistry};
use bevy_transform::components::Transform;
use bevy_world_serialization::{
    serde::{DynamicWorldSerializer, WorldDeserializer},
    DynamicWorld, DynamicWorldBuilder, WorldFilter,
};
use engine_assets::{load_or_create_meta, SharedAssetRegistry};
use engine_core::{AssetUuid, EntityGuid, EntityGuidMap};
use engine_protocol::PrefabFileEntry;
use ron::ser::PrettyConfig;
use serde::{
    de::{DeserializeSeed, MapAccess, SeqAccess, Visitor},
    ser::SerializeStruct,
    Deserialize, Deserializer, Serialize, Serializer,
};
use serde_json::Value;

use crate::{
    components::SceneEntity,
    serialization::{
        collect_subtree_entities, ensure_scene_guids, read_reflected_component_json,
        remove_reflected_component, restore_hierarchy_children, write_reflected_component_json,
        NoopLoadFromPath, SceneError, SceneSerializationFilter,
    },
};

/// Supported `.prefab.ron` file format version.
pub const CURRENT_PREFAB_VERSION: u32 = 1;

/// Sentinel override JSON string marking a template component that was removed on a prefab instance.
pub const PREFAB_COMPONENT_REMOVED: &str = "__prefab_removed__";

/// Marks the root entity of an instantiated `.prefab.ron` subtree and tracks local overrides.
#[derive(
    Component, Reflect, Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
#[reflect(Component, Default, Serialize, Deserialize)]
pub struct PrefabInstance {
    /// Persistent [`AssetUuid`] of the linked `.prefab.ron` asset when indexed inside `assets/`.
    #[serde(default)]
    pub prefab_uuid: Option<AssetUuid>,
    /// Normalized path to the linked `.prefab.ron` asset (relative to the project root when inside it).
    pub prefab_path: String,
    /// Template root [`EntityGuid`] inside the referenced `.prefab.ron` file.
    pub root_source_guid: EntityGuid,
    /// Map of override keys (`"<source_guid>::<type_path>[::<field>]"`) to JSON values or [`PREFAB_COMPONENT_REMOVED`].
    pub overrides: BTreeMap<String, String>,
}

/// Links every entity inside an instantiated prefab subtree back to its instance root and template GUID.
#[derive(
    Component, Reflect, Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
#[reflect(Component, Default, Serialize, Deserialize)]
pub struct PrefabMember {
    /// Live [`EntityGuid`] of the owning [`PrefabInstance`] root entity.
    pub instance_root_guid: EntityGuid,
    /// Corresponding entity [`EntityGuid`] inside the `.prefab.ron` asset template.
    pub source_guid: EntityGuid,
}

/// Formats the whole-component override map key for `(source_guid, type_path)`.
#[must_use]
pub fn prefab_component_override_key(source_guid: EntityGuid, type_path: &str) -> String {
    format!("{source_guid}::{type_path}")
}

/// Formats a per-field override map key for `(source_guid, type_path, field)`.
#[must_use]
pub fn prefab_field_override_key(
    source_guid: EntityGuid,
    type_path: &str,
    field: &str,
) -> String {
    format!("{source_guid}::{type_path}::{field}")
}

/// Normalizes a prefab path relative to `project_root` (when under it) using `/` separators.
#[must_use]
pub fn normalize_prefab_path(raw: &Path, project_root: Option<&Path>) -> String {
    let rel = if let Some(root) = project_root
        && let Ok(stripped) = raw.strip_prefix(root)
    {
        stripped
    } else {
        raw
    };
    rel.to_string_lossy().replace('\\', "/")
}

#[derive(Debug, Clone)]
struct PrefabTemplateNode {
    source_guid: EntityGuid,
    parent_source_guid: Option<EntityGuid>,
    components: BTreeMap<String, Value>,
}

#[derive(Debug, Clone)]
struct PrefabTemplate {
    ordered_nodes: Vec<PrefabTemplateNode>,
    by_source_guid: HashMap<EntityGuid, usize>,
}

impl PrefabTemplate {
    fn get_node(&self, source_guid: EntityGuid) -> Option<&PrefabTemplateNode> {
        self.by_source_guid
            .get(&source_guid)
            .and_then(|&idx| self.ordered_nodes.get(idx))
    }
}

fn is_structural_prefab_component(type_id: TypeId) -> bool {
    type_id == TypeId::of::<EntityGuid>()
        || type_id == TypeId::of::<SceneEntity>()
        || type_id == TypeId::of::<ChildOf>()
        || type_id == TypeId::of::<PrefabInstance>()
        || type_id == TypeId::of::<PrefabMember>()
}

fn collect_entity_authored_components_json(
    world: &World,
    entity: Entity,
    filter: Option<&SceneSerializationFilter>,
) -> Result<BTreeMap<String, Value>, SceneError> {
    let entity_ref = world
        .get_entity(entity)
        .map_err(|_| SceneError::Reflection(format!("Entity {entity:?} does not exist")))?;

    let type_paths: Vec<String> = {
        let type_registry = world.resource::<AppTypeRegistry>().read();
        entity_ref
            .archetype()
            .components()
            .iter()
            .filter_map(|&component_id| {
                let info = world.components().get_info(component_id)?;
                let type_id = info.type_id()?;
                if is_structural_prefab_component(type_id) {
                    return None;
                }
                if filter.is_some_and(|f| !f.contains_id(type_id)) {
                    return None;
                }
                let registration = type_registry.get(type_id)?;
                registration.data::<ReflectComponent>()?;
                Some(registration.type_info().type_path().to_owned())
            })
            .collect()
    };

    let mut out = BTreeMap::new();
    for type_path in type_paths {
        if let Some(val) = read_reflected_component_json(world, entity, &type_path)? {
            out.insert(type_path, val);
        }
    }
    Ok(out)
}

fn json_values_approx_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(fx), Some(fy)) => (fx - fy).abs() <= 1e-5,
            _ => x == y,
        },
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(xs), Value::Array(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys.iter())
                    .all(|(lx, ly)| json_values_approx_eq(lx, ly))
        }
        (Value::Object(xm), Value::Object(ym)) => {
            xm.len() == ym.len()
                && xm
                    .iter()
                    .all(|(k, v)| ym.get(k).is_some_and(|rv| json_values_approx_eq(v, rv)))
        }
        _ => false,
    }
}

fn parse_prefab_file(world: &World, abs_path: &Path) -> Result<ParsedPrefabFile, SceneError> {
    let ron_str = fs::read_to_string(abs_path).map_err(|source| SceneError::Io {
        path: abs_path.to_path_buf(),
        source,
    })?;
    let type_registry = world.resource::<AppTypeRegistry>().read();
    let mut noop_loader = NoopLoadFromPath;
    let mut active_server = world.get_resource::<AssetServer>().cloned();
    let load_from_path: &mut dyn LoadFromPath = match active_server.as_mut() {
        Some(server) => server,
        None => &mut noop_loader,
    };

    let mut deserializer = ron::de::Deserializer::from_str(&ron_str)?;
    let prefab_deserializer = PrefabFileDeserializer {
        type_registry: &type_registry,
        load_from_path,
    };
    let parsed = prefab_deserializer
        .deserialize(&mut deserializer)
        .map_err(|err| deserializer.span_error(err))?;

    if parsed.schema_version != CURRENT_PREFAB_VERSION {
        return Err(SceneError::UnsupportedVersion {
            found: parsed.schema_version,
        });
    }
    Ok(parsed)
}

fn load_prefab_template(world: &World, abs_path: &Path) -> Result<PrefabTemplate, SceneError> {
    let parsed = parse_prefab_file(world, abs_path)?;
    let type_registry = world.resource::<AppTypeRegistry>().clone();
    let filter = world.get_resource::<SceneSerializationFilter>().cloned();

    let mut scratch = World::new();
    scratch.insert_resource(type_registry.clone());
    if let Some(f) = filter.as_ref() {
        scratch.insert_resource(f.clone());
    }

    let mut entity_map = EntityHashMap::default();
    {
        let guard = type_registry.read();
        parsed
            .dynamic_world
            .write_to_world_with(&mut scratch, &mut entity_map, &guard)?;
    }

    let spawned_entities: Vec<Entity> = entity_map.values().copied().collect();
    restore_hierarchy_children(&mut scratch, &spawned_entities);
    ensure_scene_guids(&mut scratch, &spawned_entities);

    let root_entity = spawned_entities
        .iter()
        .copied()
        .find(|&e| scratch.get::<EntityGuid>(e) == Some(&parsed.root_source_guid))
        .ok_or_else(|| {
            SceneError::InvalidMetadata(format!(
                "Prefab '{}' is missing root entity GUID {}",
                abs_path.display(),
                parsed.root_source_guid
            ))
        })?;

    let ordered_entities = collect_subtree_entities(&scratch, root_entity);
    let mut ordered_nodes = Vec::with_capacity(ordered_entities.len());
    let mut by_source_guid = HashMap::with_capacity(ordered_entities.len());

    for (idx, entity) in ordered_entities.into_iter().enumerate() {
        let source_guid = scratch
            .get::<EntityGuid>(entity)
            .copied()
            .ok_or_else(|| SceneError::Reflection("Prefab template entity lacked EntityGuid".to_owned()))?;
        let parent_source_guid = if entity == root_entity {
            None
        } else {
            scratch
                .get::<ChildOf>(entity)
                .and_then(|c| scratch.get::<EntityGuid>(c.parent()).copied())
        };
        let components = collect_entity_authored_components_json(&scratch, entity, filter.as_ref())?;
        by_source_guid.insert(source_guid, idx);
        ordered_nodes.push(PrefabTemplateNode {
            source_guid,
            parent_source_guid,
            components,
        });
    }

    Ok(PrefabTemplate {
        ordered_nodes,
        by_source_guid,
    })
}

/// Saves the entity subtree rooted at `root` to a `.prefab.ron` file at `abs_path` and links `root` as a [`PrefabInstance`].
///
/// Returns `(root_source_guid, entity_count)`.
pub fn save_prefab_to_file(
    world: &mut World,
    root: Entity,
    abs_path: &Path,
    prefab_path_key: &str,
    name: Option<&str>,
) -> Result<(EntityGuid, usize), SceneError> {
    let root_ref = world
        .get_entity(root)
        .map_err(|_| SceneError::Reflection(format!("Entity {root:?} does not exist")))?;
    let root_live_guid = root_ref.get::<EntityGuid>().copied().ok_or_else(|| {
        SceneError::Reflection(format!("Entity {root:?} has no EntityGuid component"))
    })?;

    let is_existing_instance = root_ref.contains::<PrefabInstance>();
    let resolved_name = name
        .filter(|n| !n.trim().is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| root_ref.get::<bevy_ecs::name::Name>().map(|n| n.as_str().to_owned()))
        .unwrap_or_else(|| "Prefab".to_owned());

    let subtree = collect_subtree_entities(world, root);
    let entity_count = subtree.len();

    let mut source_guid_by_entity = HashMap::with_capacity(entity_count);
    for &entity in &subtree {
        let live_guid = world
            .get::<EntityGuid>(entity)
            .copied()
            .ok_or_else(|| SceneError::Reflection(format!("Entity {entity:?} has no EntityGuid")))?;
        let source_guid = if is_existing_instance
            && let Some(member) = world.get::<PrefabMember>(entity)
            && member.instance_root_guid == root_live_guid
        {
            member.source_guid
        } else {
            live_guid
        };
        source_guid_by_entity.insert(entity, source_guid);
    }

    let root_source_guid = source_guid_by_entity[&root];

    let encoded = {
        let type_registry = world.resource::<AppTypeRegistry>().read();
        let filter = world
            .get_resource::<SceneSerializationFilter>()
            .map(SceneSerializationFilter::to_world_filter)
            .unwrap_or_else(WorldFilter::allow_all);

        let mut dynamic_world = DynamicWorldBuilder::from_world(world, &type_registry)
            .with_component_filter(filter)
            .with_resource_filter(WorldFilter::deny_all())
            .extract_entities(subtree.iter().copied())
            .build();

        for de in &mut dynamic_world.entities {
            de.components.retain(|comp| {
                let Some(ti) = comp.get_represented_type_info() else {
                    return true;
                };
                let tid = ti.type_id();
                if tid == TypeId::of::<PrefabInstance>() || tid == TypeId::of::<PrefabMember>() {
                    return false;
                }
                if de.entity == root && tid == TypeId::of::<ChildOf>() {
                    return false;
                }
                true
            });

            if let Some(&source_guid) = source_guid_by_entity.get(&de.entity) {
                let mut replaced = false;
                for comp in &mut de.components {
                    if comp
                        .get_represented_type_info()
                        .is_some_and(|ti| ti.type_id() == TypeId::of::<EntityGuid>())
                    {
                        *comp = Box::new(source_guid);
                        replaced = true;
                        break;
                    }
                }
                if !replaced {
                    de.components.push(Box::new(source_guid));
                }
            }
        }

        let serializer = PrefabFileSerializer {
            schema_version: CURRENT_PREFAB_VERSION,
            name: &resolved_name,
            root_source_guid,
            world: DynamicWorldSerializer::new(&dynamic_world, &type_registry),
        };
        ron::ser::to_string_pretty(&serializer, PrettyConfig::default())?
    };

    if let Some(parent) = abs_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|source| SceneError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(abs_path, encoded).map_err(|source| SceneError::Io {
        path: abs_path.to_path_buf(),
        source,
    })?;

    for &entity in &subtree {
        let source_guid = source_guid_by_entity[&entity];
        world.entity_mut(entity).insert(PrefabMember {
            instance_root_guid: root_live_guid,
            source_guid,
        });
    }
    let prefab_uuid = resolve_or_create_prefab_uuid(world, abs_path, prefab_path_key);
    world.entity_mut(root).insert(PrefabInstance {
        prefab_uuid,
        prefab_path: prefab_path_key.to_owned(),
        root_source_guid,
        overrides: BTreeMap::new(),
    });

    Ok((root_source_guid, entity_count))
}

/// Instantiates a `.prefab.ron` asset from `abs_path` into `world`, assigning fresh [`EntityGuid`]s and attaching [`PrefabInstance`].
pub fn spawn_prefab_from_file(
    world: &mut World,
    abs_path: &Path,
    prefab_path_key: &str,
    parent: Option<Entity>,
    position: Option<Vec3>,
) -> Result<(Entity, EntityGuid), SceneError> {
    let mut parsed = parse_prefab_file(world, abs_path)?;
    let mut source_guid_by_template_entity = HashMap::new();
    let mut template_root_entity = None;

    for de in &mut parsed.dynamic_world.entities {
        let mut found_source_guid = None;
        for comp in &mut de.components {
            if comp
                .get_represented_type_info()
                .is_some_and(|ti| ti.type_id() == TypeId::of::<EntityGuid>())
            {
                found_source_guid = EntityGuid::from_reflect(comp.as_partial_reflect());
                *comp = Box::new(EntityGuid::new());
                break;
            }
        }
        let source_guid = found_source_guid.unwrap_or_else(EntityGuid::new);
        if source_guid == parsed.root_source_guid {
            template_root_entity = Some(de.entity);
        }
        source_guid_by_template_entity.insert(de.entity, source_guid);
    }

    let type_registry = world.resource::<AppTypeRegistry>().clone();
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

    let spawned_root = template_root_entity
        .and_then(|tmpl_e| entity_map.get(&tmpl_e).copied())
        .or_else(|| spawned_entities.first().copied())
        .ok_or_else(|| {
            SceneError::InvalidMetadata(format!("Prefab '{}' contained no entities", abs_path.display()))
        })?;

    if let Some(parent_entity) = parent
        && world.get_entity(parent_entity).is_ok()
    {
        world.entity_mut(spawned_root).insert(ChildOf(parent_entity));
    }

    if let Some(pos) = position
        && let Some(mut tfm) = world.get_mut::<Transform>(spawned_root)
    {
        tfm.translation = pos;
    }

    let root_live_guid = world
        .get::<EntityGuid>(spawned_root)
        .copied()
        .unwrap_or_else(EntityGuid::new);

    for (tmpl_entity, &live_entity) in &entity_map {
        let source_guid = source_guid_by_template_entity
            .get(tmpl_entity)
            .copied()
            .unwrap_or_else(EntityGuid::new);
        world.entity_mut(live_entity).insert(PrefabMember {
            instance_root_guid: root_live_guid,
            source_guid,
        });
    }

    let prefab_uuid = resolve_or_create_prefab_uuid(world, abs_path, prefab_path_key);
    world.entity_mut(spawned_root).insert(PrefabInstance {
        prefab_uuid,
        prefab_path: prefab_path_key.to_owned(),
        root_source_guid: parsed.root_source_guid,
        overrides: BTreeMap::new(),
    });

    sync_prefab_instance_overrides(world, spawned_root, abs_path)?;
    Ok((spawned_root, root_live_guid))
}

fn collect_instance_members_by_source(
    world: &World,
    instance_root_guid: EntityGuid,
) -> HashMap<EntityGuid, (Entity, EntityGuid)> {
    let mut members = HashMap::new();
    for archetype in world.archetypes().iter() {
        for arch_entity in archetype.entities() {
            let entity = arch_entity.id();
            if let Some(member) = world.get::<PrefabMember>(entity)
                && member.instance_root_guid == instance_root_guid
                && let Some(&live_guid) = world.get::<EntityGuid>(entity)
            {
                members.insert(member.source_guid, (entity, live_guid));
            }
        }
    }
    members
}

/// Recomputes [`PrefabInstance::overrides`] for `instance_root` by comparing live member components against `abs_path`.
///
/// Returns `true` if the `overrides` map changed.
pub fn sync_prefab_instance_overrides(
    world: &mut World,
    instance_root: Entity,
    abs_path: &Path,
) -> Result<bool, SceneError> {
    let instance_root_guid = world
        .get::<EntityGuid>(instance_root)
        .copied()
        .ok_or_else(|| SceneError::Reflection("Prefab instance root has no EntityGuid".to_owned()))?;

    let template = load_prefab_template(world, abs_path)?;
    let filter = world.get_resource::<SceneSerializationFilter>().cloned();
    let members = collect_instance_members_by_source(world, instance_root_guid);

    let mut next_overrides = BTreeMap::new();
    for tmpl_node in &template.ordered_nodes {
        let Some(&(live_entity, _)) = members.get(&tmpl_node.source_guid) else {
            continue;
        };
        let live_components =
            collect_entity_authored_components_json(world, live_entity, filter.as_ref())?;

        for (type_path, tmpl_val) in &tmpl_node.components {
            let Some(live_val) = live_components.get(type_path) else {
                next_overrides.insert(
                    prefab_component_override_key(tmpl_node.source_guid, type_path),
                    PREFAB_COMPONENT_REMOVED.to_owned(),
                );
                continue;
            };

            if let (Value::Object(live_obj), Value::Object(tmpl_obj)) = (live_val, tmpl_val) {
                let mut field_keys: HashSet<&str> =
                    live_obj.keys().map(String::as_str).collect();
                field_keys.extend(tmpl_obj.keys().map(String::as_str));

                let mut matched_all_fields = true;
                let mut per_field_diffs = Vec::new();
                for field in field_keys {
                    match (live_obj.get(field), tmpl_obj.get(field)) {
                        (Some(lv), Some(tv)) => {
                            if !json_values_approx_eq(lv, tv) {
                                per_field_diffs.push((field, lv));
                            }
                        }
                        _ => {
                            matched_all_fields = false;
                            break;
                        }
                    }
                }

                if matched_all_fields {
                    for (field, lv) in per_field_diffs {
                        let encoded = serde_json::to_string(lv)
                            .map_err(|e| SceneError::Reflection(e.to_string()))?;
                        next_overrides.insert(
                            prefab_field_override_key(tmpl_node.source_guid, type_path, field),
                            encoded,
                        );
                    }
                } else {
                    let encoded = serde_json::to_string(live_val)
                        .map_err(|e| SceneError::Reflection(e.to_string()))?;
                    next_overrides.insert(
                        prefab_component_override_key(tmpl_node.source_guid, type_path),
                        encoded,
                    );
                }
            } else if !json_values_approx_eq(live_val, tmpl_val) {
                let encoded = serde_json::to_string(live_val)
                    .map_err(|e| SceneError::Reflection(e.to_string()))?;
                next_overrides.insert(
                    prefab_component_override_key(tmpl_node.source_guid, type_path),
                    encoded,
                );
            }
        }

        for (type_path, live_val) in &live_components {
            if !tmpl_node.components.contains_key(type_path) {
                let encoded = serde_json::to_string(live_val)
                    .map_err(|e| SceneError::Reflection(e.to_string()))?;
                next_overrides.insert(
                    prefab_component_override_key(tmpl_node.source_guid, type_path),
                    encoded,
                );
            }
        }
    }

    let Some(mut inst) = world.get_mut::<PrefabInstance>(instance_root) else {
        return Ok(false);
    };
    if inst.overrides == next_overrides {
        return Ok(false);
    }
    inst.overrides = next_overrides;
    Ok(true)
}

/// Applies `instance_root` changes back to `abs_path` and propagates non-overridden fields to all sibling instances.
pub fn apply_prefab_instance(
    world: &mut World,
    instance_root: Entity,
    abs_path: &Path,
) -> Result<Vec<EntityGuid>, SceneError> {
    let instance_data = world
        .get::<PrefabInstance>(instance_root)
        .cloned()
        .ok_or_else(|| SceneError::Reflection("Target entity has no PrefabInstance component".to_owned()))?;

    save_prefab_to_file(
        world,
        instance_root,
        abs_path,
        &instance_data.prefab_path,
        None,
    )?;

    let mut updated_guids: Vec<EntityGuid> = collect_subtree_entities(world, instance_root)
        .into_iter()
        .filter_map(|e| world.get::<EntityGuid>(e).copied())
        .collect();

    let template = load_prefab_template(world, abs_path)?;
    let filter = world.get_resource::<SceneSerializationFilter>().cloned();

    let sibling_roots: Vec<Entity> = world
        .archetypes()
        .iter()
        .flat_map(|arch| arch.entities())
        .map(|ae| ae.id())
        .filter(|&e| {
            e != instance_root
                && world
                    .get::<PrefabInstance>(e)
                    .is_some_and(|inst| inst.prefab_path == instance_data.prefab_path)
        })
        .collect();

    for sib_root in sibling_roots {
        let Some(sib_root_guid) = world.get::<EntityGuid>(sib_root).copied() else {
            continue;
        };
        let overrides = world
            .get::<PrefabInstance>(sib_root)
            .map(|inst| inst.overrides.clone())
            .unwrap_or_default();
        let mut members = collect_instance_members_by_source(world, sib_root_guid);

        for tmpl_node in &template.ordered_nodes {
            if let Some(&(sib_entity, sib_guid)) = members.get(&tmpl_node.source_guid) {
                let current_live = collect_entity_authored_components_json(
                    world,
                    sib_entity,
                    filter.as_ref(),
                )?;

                for (type_path, tmpl_val) in &tmpl_node.components {
                    let comp_key = prefab_component_override_key(tmpl_node.source_guid, type_path);
                    if let Some(raw_override) = overrides.get(&comp_key) {
                        if raw_override == PREFAB_COMPONENT_REMOVED {
                            continue;
                        }
                        if let Ok(parsed_override) = serde_json::from_str::<Value>(raw_override) {
                            write_reflected_component_json(
                                world,
                                sib_entity,
                                type_path,
                                &parsed_override,
                            )?;
                            continue;
                        }
                    }

                    let mut target_val = tmpl_val.clone();
                    if let Value::Object(ref mut merged_obj) = target_val {
                        let field_prefix = format!("{comp_key}::");
                        for (ov_key, ov_raw) in &overrides {
                            if let Some(field) = ov_key.strip_prefix(&field_prefix)
                                && let Ok(parsed_field_val) = serde_json::from_str::<Value>(ov_raw)
                            {
                                merged_obj.insert(field.to_owned(), parsed_field_val);
                            }
                        }
                    }

                    write_reflected_component_json(world, sib_entity, type_path, &target_val)?;
                }

                for type_path in current_live.keys() {
                    if !tmpl_node.components.contains_key(type_path) {
                        let comp_key =
                            prefab_component_override_key(tmpl_node.source_guid, type_path);
                        if !overrides.contains_key(&comp_key) {
                            remove_reflected_component(world, sib_entity, type_path)?;
                        }
                    }
                }

                if !updated_guids.contains(&sib_guid) {
                    updated_guids.push(sib_guid);
                }
            } else if let Some(parent_src) = tmpl_node.parent_source_guid
                && let Some(&(parent_live_entity, _)) = members.get(&parent_src)
            {
                let new_guid = EntityGuid::new();
                let new_entity = world
                    .spawn((
                        SceneEntity,
                        new_guid,
                        PrefabMember {
                            instance_root_guid: sib_root_guid,
                            source_guid: tmpl_node.source_guid,
                        },
                        ChildOf(parent_live_entity),
                    ))
                    .id();
                for (type_path, tmpl_val) in &tmpl_node.components {
                    write_reflected_component_json(world, new_entity, type_path, tmpl_val)?;
                }
                members.insert(tmpl_node.source_guid, (new_entity, new_guid));
                updated_guids.push(new_guid);
            }
        }

        sync_prefab_instance_overrides(world, sib_root, abs_path)?;
    }

    Ok(updated_guids)
}

/// Reverts either a single `(source_guid, type_path)` or the entire `instance_root` subtree back to `abs_path`.
pub fn revert_prefab_instance(
    world: &mut World,
    instance_root: Entity,
    abs_path: &Path,
    component_filter: Option<(EntityGuid, &str)>,
) -> Result<Vec<EntityGuid>, SceneError> {
    let root_guid = world
        .get::<EntityGuid>(instance_root)
        .copied()
        .ok_or_else(|| SceneError::Reflection("Prefab instance root has no EntityGuid".to_owned()))?;
    let template = load_prefab_template(world, abs_path)?;
    let filter = world.get_resource::<SceneSerializationFilter>().cloned();
    let mut members = collect_instance_members_by_source(world, root_guid);
    let mut reverted_guids = Vec::new();

    if let Some((target_source_guid, target_type_path)) = component_filter {
        let Some(&(target_entity, target_live_guid)) = members.get(&target_source_guid) else {
            return Err(SceneError::Reflection(format!(
                "Prefab member source GUID {target_source_guid} not found on instance"
            )));
        };
        let tmpl_node = template.get_node(target_source_guid).ok_or_else(|| {
            SceneError::Reflection(format!(
                "Prefab source node {target_source_guid} no longer exists in template"
            ))
        })?;
        match tmpl_node.components.get(target_type_path) {
            Some(tmpl_val) => {
                write_reflected_component_json(world, target_entity, target_type_path, tmpl_val)?;
            }
            None => {
                remove_reflected_component(world, target_entity, target_type_path)?;
            }
        }
        reverted_guids.push(target_live_guid);
        sync_prefab_instance_overrides(world, instance_root, abs_path)?;
        return Ok(reverted_guids);
    }

    // Revert whole instance: despawn local nodes dropped from template, revert matching nodes, respawn missing ones.
    let stale_entities: Vec<Entity> = members
        .iter()
        .filter(|(src_guid, _)| template.get_node(**src_guid).is_none())
        .map(|(_, (e, _))| *e)
        .collect();
    for stale in stale_entities {
        if world.get_entity(stale).is_ok() {
            world.despawn(stale);
        }
    }
    members.retain(|src_guid, _| template.get_node(*src_guid).is_some());

    for tmpl_node in &template.ordered_nodes {
        if let Some(&(live_entity, live_guid)) = members.get(&tmpl_node.source_guid) {
            let current_live =
                collect_entity_authored_components_json(world, live_entity, filter.as_ref())?;
            for type_path in current_live.keys() {
                if !tmpl_node.components.contains_key(type_path) {
                    remove_reflected_component(world, live_entity, type_path)?;
                }
            }
            for (type_path, tmpl_val) in &tmpl_node.components {
                write_reflected_component_json(world, live_entity, type_path, tmpl_val)?;
            }
            reverted_guids.push(live_guid);
        } else if let Some(parent_src) = tmpl_node.parent_source_guid
            && let Some(&(parent_live_entity, _)) = members.get(&parent_src)
        {
            let new_guid = EntityGuid::new();
            let new_entity = world
                .spawn((
                    SceneEntity,
                    new_guid,
                    PrefabMember {
                        instance_root_guid: root_guid,
                        source_guid: tmpl_node.source_guid,
                    },
                    ChildOf(parent_live_entity),
                ))
                .id();
            for (type_path, tmpl_val) in &tmpl_node.components {
                write_reflected_component_json(world, new_entity, type_path, tmpl_val)?;
            }
            members.insert(tmpl_node.source_guid, (new_entity, new_guid));
            reverted_guids.push(new_guid);
        }
    }

    if let Some(mut inst) = world.get_mut::<PrefabInstance>(instance_root) {
        inst.overrides.clear();
    }
    sync_prefab_instance_overrides(world, instance_root, abs_path)?;
    Ok(reverted_guids)
}

/// Recursively discovers all `.prefab.ron` files under `project_root`, sorted by relative path.
#[must_use]
pub fn list_prefab_files(project_root: &Path) -> Vec<PrefabFileEntry> {
    let mut entries = Vec::new();
    collect_prefab_files_recursive(project_root, project_root, &mut entries);
    entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    entries
}

fn collect_prefab_files_recursive(
    root: &Path,
    current: &Path,
    out: &mut Vec<PrefabFileEntry>,
) {
    let Ok(read_dir) = fs::read_dir(current) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skip = path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|name| name == "target" || name == "node_modules" || name.starts_with('.'));
            if !skip {
                collect_prefab_files_recursive(root, &path, out);
            }
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Some(stem) = file_name.strip_suffix(".prefab.ron") {
            let relative_path = normalize_prefab_path(&path, Some(root));
            out.push(PrefabFileEntry {
                name: stem.to_owned(),
                relative_path,
                absolute_path: path.display().to_string(),
            });
        }
    }
}

/// Resolves the filesystem path to `inst`'s `.prefab.ron` asset, preferring [`PrefabInstance::prefab_uuid`]
/// in `registry` before falling back to [`PrefabInstance::prefab_path`].
#[must_use]
pub fn resolve_prefab_abs_path(
    inst: &PrefabInstance,
    project_root: Option<&Path>,
    registry: Option<&SharedAssetRegistry>,
) -> Option<PathBuf> {
    if let Some(uuid) = inst.prefab_uuid
        && let Some(reg) = registry
        && let Some(abs) = reg.resolve_abs_path(uuid)
    {
        return Some(abs);
    }
    if inst.prefab_path.is_empty() {
        return None;
    }
    let candidate = Path::new(&inst.prefab_path);
    if candidate.is_absolute() {
        Some(candidate.to_path_buf())
    } else if let Some(root) = project_root {
        Some(root.join(candidate))
    } else {
        Some(candidate.to_path_buf())
    }
}

fn resolve_or_create_prefab_uuid(
    world: &World,
    abs_path: &Path,
    prefab_path_key: &str,
) -> Option<AssetUuid> {
    if let Some(shared) = world.get_resource::<SharedAssetRegistry>() {
        let key_norm = prefab_path_key.replace('\\', "/");
        if let Some(record) = shared.get_by_path(&key_norm) {
            return Some(record.uuid);
        }
        if let Some(stripped) = key_norm.strip_prefix("assets/")
            && let Some(record) = shared.get_by_path(stripped)
        {
            return Some(record.uuid);
        }

        let assets_dir = shared.assets_dir();
        let inside_assets = abs_path.starts_with(&assets_dir)
            || (abs_path.canonicalize().ok().is_some_and(|canon_file| {
                assets_dir
                    .canonicalize()
                    .ok()
                    .is_some_and(|canon_dir| canon_file.starts_with(&canon_dir))
            }));
        if inside_assets
            && let Ok((meta, _)) = load_or_create_meta(abs_path)
        {
            return Some(meta.uuid);
        }
    }
    None
}

/// Resolves the [`PrefabInstance`] root `(entity, prefab_path)` starting from `entity` or its [`PrefabMember`].
pub fn resolve_prefab_instance_root(
    world: &World,
    entity: Entity,
) -> Option<(Entity, String)> {
    if let Some(inst) = world.get::<PrefabInstance>(entity) {
        return Some((entity, inst.prefab_path.clone()));
    }
    let member = world.get::<PrefabMember>(entity)?;
    let root_entity = world
        .get_resource::<EntityGuidMap>()?
        .get_entity(member.instance_root_guid)?;
    let inst = world.get::<PrefabInstance>(root_entity)?;
    Some((root_entity, inst.prefab_path.clone()))
}

struct PrefabFileSerializer<'a> {
    schema_version: u32,
    name: &'a str,
    root_source_guid: EntityGuid,
    world: DynamicWorldSerializer<'a>,
}

impl Serialize for PrefabFileSerializer<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("PrefabFile", 4)?;
        state.serialize_field("schema_version", &self.schema_version)?;
        state.serialize_field("name", self.name)?;
        state.serialize_field("root_source_guid", &self.root_source_guid)?;
        state.serialize_field("world", &self.world)?;
        state.end()
    }
}

struct ParsedPrefabFile {
    schema_version: u32,
    _name: String,
    root_source_guid: EntityGuid,
    dynamic_world: DynamicWorld,
}

struct PrefabFileDeserializer<'a> {
    type_registry: &'a TypeRegistry,
    load_from_path: &'a mut dyn LoadFromPath,
}

impl<'de> DeserializeSeed<'de> for PrefabFileDeserializer<'_> {
    type Value = ParsedPrefabFile;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_struct(
            "PrefabFile",
            &["schema_version", "name", "root_source_guid", "world"],
            PrefabFileVisitor {
                type_registry: self.type_registry,
                load_from_path: self.load_from_path,
            },
        )
    }
}

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum PrefabField {
    SchemaVersion,
    Name,
    RootSourceGuid,
    World,
}

struct PrefabFileVisitor<'a> {
    type_registry: &'a TypeRegistry,
    load_from_path: &'a mut dyn LoadFromPath,
}

impl<'de> Visitor<'de> for PrefabFileVisitor<'_> {
    type Value = ParsedPrefabFile;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Bevy9 `.prefab.ron` struct")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let schema_version: u32 = seq
            .next_element()?
            .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;
        let name: String = seq
            .next_element()?
            .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;
        let root_source_guid: EntityGuid = seq
            .next_element()?
            .ok_or_else(|| serde::de::Error::invalid_length(2, &self))?;
        let dynamic_world = seq
            .next_element_seed(WorldDeserializer {
                type_registry: self.type_registry,
                load_from_path: self.load_from_path,
            })?
            .ok_or_else(|| serde::de::Error::invalid_length(3, &self))?;

        Ok(ParsedPrefabFile {
            schema_version,
            _name: name,
            root_source_guid,
            dynamic_world,
        })
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut schema_version = None;
        let mut name = None;
        let mut root_source_guid = None;
        let mut dynamic_world = None;

        while let Some(key) = map.next_key::<PrefabField>()? {
            match key {
                PrefabField::SchemaVersion => {
                    if schema_version.is_some() {
                        return Err(serde::de::Error::duplicate_field("schema_version"));
                    }
                    schema_version = Some(map.next_value()?);
                }
                PrefabField::Name => {
                    if name.is_some() {
                        return Err(serde::de::Error::duplicate_field("name"));
                    }
                    name = Some(map.next_value()?);
                }
                PrefabField::RootSourceGuid => {
                    if root_source_guid.is_some() {
                        return Err(serde::de::Error::duplicate_field("root_source_guid"));
                    }
                    root_source_guid = Some(map.next_value()?);
                }
                PrefabField::World => {
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

        Ok(ParsedPrefabFile {
            schema_version: schema_version
                .ok_or_else(|| serde::de::Error::missing_field("schema_version"))?,
            _name: name.ok_or_else(|| serde::de::Error::missing_field("name"))?,
            root_source_guid: root_source_guid
                .ok_or_else(|| serde::de::Error::missing_field("root_source_guid"))?,
            dynamic_world: dynamic_world.ok_or_else(|| serde::de::Error::missing_field("world"))?,
        })
    }
}

/// Ensures `path` ends with `.prefab.ron`.
#[must_use]
pub fn ensure_prefab_extension(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed.ends_with(".prefab.ron") {
        PathBuf::from(trimmed)
    } else if let Some(prefix) = trimmed.strip_suffix(".ron") {
        PathBuf::from(format!("{prefix}.prefab.ron"))
    } else {
        PathBuf::from(format!("{trimmed}.prefab.ron"))
    }
}
