//! Bevy Remote Protocol (BRP) plugin builder and custom engine RPC methods.

use std::path::PathBuf;

use bevy_asset::AssetServer;
use bevy_camera::{visibility::Visibility, Camera};
use bevy_color::Color;
use bevy_ecs::{entity::Entity, hierarchy::ChildOf, name::Name, query::With, system::In, world::World};
use bevy_math::{Vec2, Vec3};
use bevy_remote::{
    builtin_methods::{parse, parse_some},
    error_codes,
    http::{Headers, RemoteHttpPlugin},
    BrpError, BrpResult, RemotePlugin,
};
use bevy_transform::components::{GlobalTransform, Transform};
use engine_assets::{AssetError, AssetRegistry, ThumbnailService};
use engine_core::{EntityGuid, EntityGuidMap, ProjectPaths, WorldMode};
use engine_protocol::{
    AddComponentParams, AssetCreateFolderParams, AssetDeleteParams, AssetDeleteResult,
    AssetImportParams, AssetImportResult, AssetKind, AssetListParams, AssetMoveParams,
    AssetSpawnInViewportParams, AssetSpawnInViewportResult, AssetThumbnailParams,
    AssetThumbnailResult, AssetUpdateSettingsParams, ComponentMutationResult, DeleteEntityParams,
    DeleteEntityResult, PrefabApplyResult, PrefabInstanceActionParams, PrefabListResult,
    PrefabRevertParams, PrefabRevertResult, PrefabSaveParams, PrefabSaveResult, PrefabSpawnParams,
    PrefabSpawnResult, RemoveComponentParams, ReparentEntityParams, ReparentEntityResult,
    SceneLoadParams, SceneNewParams, SceneOperationResult, SceneSaveParams, SetComponentParams,
    SetViewportModeParams, SetViewportModeResult, SpawnPresetParams, SpawnPresetResult,
    ViewportCameraMode, WorldTreeResult, METHOD_ASSET_CREATE_FOLDER, METHOD_ASSET_DELETE,
    METHOD_ASSET_IMPORT, METHOD_ASSET_LIST, METHOD_ASSET_MOVE, METHOD_ASSET_RESCAN,
    METHOD_ASSET_SPAWN_IN_VIEWPORT, METHOD_ASSET_THUMBNAIL, METHOD_ASSET_UPDATE_SETTINGS,
    METHOD_COMPONENT_ADD, METHOD_COMPONENT_REMOVE, METHOD_COMPONENT_SET, METHOD_ENTITY_DELETE,
    METHOD_ENTITY_REPARENT, METHOD_ENTITY_SPAWN_PRESET, METHOD_HISTORY_REDO, METHOD_HISTORY_STATE,
    METHOD_HISTORY_UNDO, METHOD_PREFAB_APPLY, METHOD_PREFAB_LIST, METHOD_PREFAB_REVERT,
    METHOD_PREFAB_SAVE, METHOD_PREFAB_SPAWN, METHOD_SCENE_LOAD, METHOD_SCENE_NEW, METHOD_SCENE_SAVE,
    METHOD_VIEWPORT_SET_MODE, METHOD_WORLD_TREE,
};
use engine_render::{
    camera::{EditorCamera2d, EditorCamera3d},
    ViewportConfig,
};
use engine_scene::{
    apply_prefab_instance, build_scene_tree, default_preset_name, despawn_subtree,
    ensure_prefab_extension, list_prefab_files, load_level_from_file, normalize_prefab_path,
    read_reflected_component_json, remove_reflected_component, reset_to_new_scene,
    resolve_prefab_instance_root, revert_prefab_instance, save_level_to_file, save_prefab_to_file,
    snapshot_subtree, spawn_prefab_from_file, spawn_preset, write_reflected_component_json,
    ActiveScene, LevelMetadata, MeshPrimitive3d, PrefabMember, SceneEntity, SceneGltfModel,
    SceneTilemap, SpriteDesc, StandardMaterialDesc,
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    config::EditorBridgeConfig,
    history::{
        apply_reparent, now_epoch_ms, prune_selection_guids, redo_last, resolve_project_path,
        short_type_name, sync_entity_prefab_overrides_if_member, undo_last, EditorCommand,
        EditorHistory,
    },
    selection::{BridgeDirtyFlags, EditorSelection},
};

/// Builds Bevy's [`RemotePlugin`] pre-configured with all custom `engine.*` RPC methods.
#[must_use]
pub fn build_remote_plugin() -> RemotePlugin {
    RemotePlugin::default()
        .with_method_main(METHOD_SCENE_NEW, handle_scene_new)
        .with_method_main(METHOD_SCENE_LOAD, handle_scene_load)
        .with_method_main(METHOD_SCENE_SAVE, handle_scene_save)
        .with_method_main(METHOD_WORLD_TREE, handle_world_tree)
        .with_method_main(METHOD_ENTITY_SPAWN_PRESET, handle_entity_spawn_preset)
        .with_method_main(METHOD_ENTITY_DELETE, handle_entity_delete)
        .with_method_main(METHOD_ENTITY_REPARENT, handle_entity_reparent)
        .with_method_main(METHOD_COMPONENT_SET, handle_component_set)
        .with_method_main(METHOD_COMPONENT_ADD, handle_component_add)
        .with_method_main(METHOD_COMPONENT_REMOVE, handle_component_remove)
        .with_method_main(METHOD_HISTORY_UNDO, handle_history_undo)
        .with_method_main(METHOD_HISTORY_REDO, handle_history_redo)
        .with_method_main(METHOD_HISTORY_STATE, handle_history_state)
        .with_method_main(METHOD_PREFAB_SAVE, handle_prefab_save)
        .with_method_main(METHOD_PREFAB_SPAWN, handle_prefab_spawn)
        .with_method_main(METHOD_PREFAB_APPLY, handle_prefab_apply)
        .with_method_main(METHOD_PREFAB_REVERT, handle_prefab_revert)
        .with_method_main(METHOD_PREFAB_LIST, handle_prefab_list)
        .with_method_main(METHOD_VIEWPORT_SET_MODE, handle_viewport_set_mode)
        .with_method_main(METHOD_ASSET_LIST, handle_asset_list)
        .with_method_main(METHOD_ASSET_IMPORT, handle_asset_import)
        .with_method_main(METHOD_ASSET_MOVE, handle_asset_move)
        .with_method_main(METHOD_ASSET_DELETE, handle_asset_delete)
        .with_method_main(METHOD_ASSET_CREATE_FOLDER, handle_asset_create_folder)
        .with_method_main(METHOD_ASSET_UPDATE_SETTINGS, handle_asset_update_settings)
        .with_method_main(METHOD_ASSET_THUMBNAIL, handle_asset_thumbnail)
        .with_method_main(METHOD_ASSET_SPAWN_IN_VIEWPORT, handle_asset_spawn_in_viewport)
        .with_method_main(METHOD_ASSET_RESCAN, handle_asset_rescan)
}

/// Builds Bevy's [`RemoteHttpPlugin`] configured from [`EditorBridgeConfig`].
#[must_use]
pub fn build_remote_http_plugin(config: &EditorBridgeConfig) -> RemoteHttpPlugin {
    let plugin = RemoteHttpPlugin::default()
        .with_address(config.host)
        .with_port(config.brp_port);

    if config.allow_cors {
        let headers = Headers::new()
            .insert("Access-Control-Allow-Origin", "*")
            .insert("Access-Control-Allow-Headers", "Content-Type")
            .insert("Access-Control-Allow-Methods", "POST, OPTIONS");
        plugin.with_headers(headers)
    } else {
        plugin
    }
}

fn serialize_result<T: Serialize>(value: T) -> BrpResult {
    serde_json::to_value(value).map_err(BrpError::internal)
}

fn sync_camera_mode_for_world_mode(world: &mut World, world_mode: WorldMode) {
    let target_camera_mode = match world_mode {
        WorldMode::Mode2D => ViewportCameraMode::Mode2D,
        WorldMode::Mode3D | WorldMode::Hybrid => ViewportCameraMode::Mode3D,
    };
    if let Some(mut config) = world.get_resource_mut::<ViewportConfig>() {
        config.camera_mode = target_camera_mode;
    }
}

fn mark_scene_reset_dirty(world: &mut World) {
    if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
        history.clear();
    }

    let selection_changed = world
        .get_resource_mut::<EditorSelection>()
        .is_some_and(|mut selection| selection.clear());

    if let Some(mut dirty) = world.get_resource_mut::<BridgeDirtyFlags>() {
        dirty.hierarchy_dirty = true;
        dirty.viewport_dirty = true;
        if selection_changed {
            dirty.selection_dirty = true;
        }
    }
}

fn lookup_entity_by_guid(world: &World, guid: EntityGuid) -> Result<Entity, BrpError> {
    let entity = world
        .get_resource::<EntityGuidMap>()
        .and_then(|map| map.get_entity(guid))
        .ok_or_else(|| BrpError {
            code: error_codes::ENTITY_NOT_FOUND,
            message: format!("Entity with GUID {guid} not found"),
            data: None,
        })?;
    if world.get_entity(entity).is_err() {
        return Err(BrpError {
            code: error_codes::ENTITY_NOT_FOUND,
            message: format!("Entity with GUID {guid} ({entity:?}) no longer exists"),
            data: None,
        });
    }
    Ok(entity)
}

fn is_protected_component(type_path: &str) -> bool {
    matches!(
        type_path,
        "engine_core::guid::EntityGuid"
            | "engine_scene::components::SceneEntity"
            | "engine_scene::prefab::PrefabInstance"
            | "engine_scene::prefab::PrefabMember"
    )
}

fn mark_mutation_dirty(world: &mut World, hierarchy_dirty: bool, refresh_guid: Option<EntityGuid>) {
    if let Some(mut active_scene) = world.get_resource_mut::<ActiveScene>() {
        active_scene.dirty = true;
    }
    if hierarchy_dirty && let Some(mut dirty) = world.get_resource_mut::<BridgeDirtyFlags>() {
        dirty.hierarchy_dirty = true;
    }
    if let Some(guid) = refresh_guid
        && let Some(mut history) = world.get_resource_mut::<EditorHistory>()
    {
        history.queue_inspector_refresh(&[guid]);
    }
}

fn count_scene_entities(world: &mut World) -> usize {
    world
        .query_filtered::<(), With<SceneEntity>>()
        .iter(world)
        .count()
}

fn handle_scene_new(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: SceneNewParams = parse_some(params)?;
    let name = if params.name.trim().is_empty() {
        "Untitled Scene".to_owned()
    } else {
        params.name
    };

    reset_to_new_scene(world, name.clone(), params.world_mode);
    sync_camera_mode_for_world_mode(world, params.world_mode);
    mark_scene_reset_dirty(world);

    serialize_result(SceneOperationResult {
        name,
        path: None,
        world_mode: params.world_mode,
        entity_count: 0,
    })
}

fn handle_scene_load(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: SceneLoadParams = parse_some(params)?;
    if params.path.trim().is_empty() {
        return Err(BrpError {
            code: error_codes::INVALID_PARAMS,
            message: "Scene path must not be empty".to_owned(),
            data: None,
        });
    }

    let resolved_path = resolve_project_path(world, &params.path);
    let metadata = load_level_from_file(world, &resolved_path).map_err(BrpError::internal)?;
    sync_camera_mode_for_world_mode(world, metadata.world_mode);
    mark_scene_reset_dirty(world);

    let entity_count = count_scene_entities(world);
    serialize_result(SceneOperationResult {
        name: metadata.name,
        path: Some(resolved_path.display().to_string()),
        world_mode: metadata.world_mode,
        entity_count,
    })
}

fn handle_scene_save(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: SceneSaveParams = match params {
        None | Some(Value::Null) => SceneSaveParams {
            path: None,
            name: None,
        },
        Some(value) => parse(value)?,
    };

    let target_path = match params.path {
        Some(raw) => {
            if raw.trim().is_empty() {
                return Err(BrpError {
                    code: error_codes::INVALID_PARAMS,
                    message: "Scene save path must not be empty".to_owned(),
                    data: None,
                });
            }
            resolve_project_path(world, &raw)
        }
        None => world
            .get_resource::<ActiveScene>()
            .and_then(|active| active.path.clone())
            .ok_or_else(|| BrpError {
                code: error_codes::INVALID_PARAMS,
                message: "No scene save path provided and ActiveScene.path is unset".to_owned(),
                data: None,
            })?,
    };

    let (active_name, active_world_mode) = world.get_resource::<ActiveScene>().map_or_else(
        || {
            (
                "Untitled Scene".to_owned(),
                world
                    .get_resource::<WorldMode>()
                    .copied()
                    .unwrap_or_default(),
            )
        },
        |active| (active.name.clone(), active.world_mode),
    );

    let resolved_name = params
        .name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(active_name);

    let metadata = LevelMetadata {
        name: resolved_name.clone(),
        world_mode: active_world_mode,
    };

    save_level_to_file(world, &metadata, &target_path).map_err(BrpError::internal)?;

    let entity_count = count_scene_entities(world);
    serialize_result(SceneOperationResult {
        name: resolved_name,
        path: Some(target_path.display().to_string()),
        world_mode: active_world_mode,
        entity_count,
    })
}

fn handle_world_tree(In(_params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let (scene_name, scene_path, world_mode) = world.get_resource::<ActiveScene>().map_or_else(
        || {
            (
                "Untitled Scene".to_owned(),
                None,
                world
                    .get_resource::<WorldMode>()
                    .copied()
                    .unwrap_or_default(),
            )
        },
        |active| {
            (
                active.name.clone(),
                active.path.as_ref().map(|p| p.display().to_string()),
                active.world_mode,
            )
        },
    );

    let roots = build_scene_tree(world);
    serialize_result(WorldTreeResult {
        scene_name,
        scene_path,
        world_mode,
        roots,
    })
}

fn handle_entity_spawn_preset(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: SpawnPresetParams = parse_some(params)?;

    let parent_entity = match params.parent_guid {
        Some(parent_guid) => {
            let entity = world
                .get_resource::<EntityGuidMap>()
                .and_then(|map| map.get_entity(parent_guid))
                .ok_or_else(|| BrpError {
                    code: error_codes::ENTITY_NOT_FOUND,
                    message: format!("Parent entity with GUID {parent_guid} not found"),
                    data: None,
                })?;
            Some(entity)
        }
        None => None,
    };

    let transform = params
        .position
        .map(|[x, y, z]| Transform::from_translation(Vec3::new(x, y, z)));

    let (entity, guid) = spawn_preset(world, params.preset, params.name, parent_entity, transform);

    let name = world
        .get::<Name>(entity)
        .map(|n| n.as_str().to_owned())
        .unwrap_or_else(|| default_preset_name(params.preset).to_owned());

    let snapshot = snapshot_subtree(world, entity).map_err(BrpError::internal)?;
    if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
        history.record(
            EditorCommand::CreateEntity {
                snapshot,
                label: format!("Create {name}"),
            },
            None,
            now_epoch_ms(),
        );
    }

    mark_mutation_dirty(world, true, None);

    serialize_result(SpawnPresetResult {
        guid,
        entity_bits: entity.to_bits(),
        name,
    })
}

fn handle_entity_delete(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: DeleteEntityParams = parse_some(params)?;
    let entity = lookup_entity_by_guid(world, params.guid)?;

    let name = world
        .get::<Name>(entity)
        .map_or_else(|| "Entity".to_owned(), |n| n.as_str().to_owned());
    let snapshot = snapshot_subtree(world, entity).map_err(BrpError::internal)?;
    let previous_selection = world
        .get_resource::<EditorSelection>()
        .map(|s| s.guids().to_vec())
        .unwrap_or_default();

    let deleted_guids = despawn_subtree(world, entity);
    let selection_changed = prune_selection_guids(world, &deleted_guids);

    if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
        history.record(
            EditorCommand::DeleteEntity {
                snapshot,
                previous_selection,
                label: format!("Delete {name}"),
            },
            None,
            now_epoch_ms(),
        );
    }

    if let Some(mut dirty) = world.get_resource_mut::<BridgeDirtyFlags>() {
        dirty.hierarchy_dirty = true;
        if selection_changed {
            dirty.selection_dirty = true;
        }
    }
    if let Some(mut active_scene) = world.get_resource_mut::<ActiveScene>() {
        active_scene.dirty = true;
    }

    serialize_result(DeleteEntityResult { deleted_guids })
}

fn handle_entity_reparent(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: ReparentEntityParams = parse_some(params)?;
    let entity = lookup_entity_by_guid(world, params.guid)?;
    let old_parent = world
        .get::<ChildOf>(entity)
        .and_then(|child_of| world.get::<EntityGuid>(child_of.parent()).copied());

    if old_parent != params.parent_guid {
        apply_reparent(world, params.guid, params.parent_guid).map_err(|message| BrpError {
            code: error_codes::INVALID_PARAMS,
            message,
            data: None,
        })?;

        if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
            history.record(
                EditorCommand::Reparent {
                    guid: params.guid,
                    old_parent,
                    new_parent: params.parent_guid,
                },
                None,
                now_epoch_ms(),
            );
        }
        mark_mutation_dirty(world, true, Some(params.guid));
    }

    serialize_result(ReparentEntityResult {
        guid: params.guid,
        parent_guid: params.parent_guid,
    })
}

fn handle_component_set(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: SetComponentParams = parse_some(params)?;
    if is_protected_component(&params.type_path) {
        return Err(BrpError {
            code: error_codes::INVALID_PARAMS,
            message: format!("Component `{}` is protected and read-only", params.type_path),
            data: None,
        });
    }

    let entity = lookup_entity_by_guid(world, params.guid)?;
    let before =
        read_reflected_component_json(world, entity, &params.type_path).map_err(BrpError::component_error)?;

    write_reflected_component_json(world, entity, &params.type_path, &params.value)
        .map_err(BrpError::component_error)?;

    if before.as_ref() != Some(&params.value)
        && let Some(mut history) = world.get_resource_mut::<EditorHistory>()
    {
        let label = params
            .label
            .unwrap_or_else(|| format!("Edit {}", short_type_name(&params.type_path)));
        let cmd = match before {
            Some(prev_val) => EditorCommand::SetComponent {
                guid: params.guid,
                type_path: params.type_path.clone(),
                before: prev_val,
                after: params.value,
                label,
            },
            None => EditorCommand::AddComponent {
                guid: params.guid,
                type_path: params.type_path.clone(),
                value: params.value,
            },
        };
        history.record(cmd, params.merge_key, now_epoch_ms());
    }

    let hierarchy_dirty = matches!(
        params.type_path.as_str(),
        "bevy_ecs::name::Name" | "bevy_camera::visibility::Visibility"
    );
    sync_entity_prefab_overrides_if_member(world, params.guid);
    mark_mutation_dirty(world, hierarchy_dirty, Some(params.guid));

    serialize_result(ComponentMutationResult {
        guid: params.guid,
        type_path: params.type_path,
    })
}

fn handle_component_add(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: AddComponentParams = parse_some(params)?;
    if is_protected_component(&params.type_path) {
        return Err(BrpError {
            code: error_codes::INVALID_PARAMS,
            message: format!("Component `{}` is protected and cannot be added", params.type_path),
            data: None,
        });
    }

    let entity = lookup_entity_by_guid(world, params.guid)?;
    let before =
        read_reflected_component_json(world, entity, &params.type_path).map_err(BrpError::component_error)?;

    write_reflected_component_json(world, entity, &params.type_path, &params.value)
        .map_err(BrpError::component_error)?;

    if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
        let cmd = match before {
            Some(prev_val) => EditorCommand::SetComponent {
                guid: params.guid,
                type_path: params.type_path.clone(),
                before: prev_val,
                after: params.value,
                label: format!("Replace {}", short_type_name(&params.type_path)),
            },
            None => EditorCommand::AddComponent {
                guid: params.guid,
                type_path: params.type_path.clone(),
                value: params.value,
            },
        };
        history.record(cmd, None, now_epoch_ms());
    }

    sync_entity_prefab_overrides_if_member(world, params.guid);
    mark_mutation_dirty(world, true, Some(params.guid));

    serialize_result(ComponentMutationResult {
        guid: params.guid,
        type_path: params.type_path,
    })
}

fn handle_component_remove(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: RemoveComponentParams = parse_some(params)?;
    if is_protected_component(&params.type_path) {
        return Err(BrpError {
            code: error_codes::INVALID_PARAMS,
            message: format!("Component `{}` is protected and cannot be removed", params.type_path),
            data: None,
        });
    }

    let entity = lookup_entity_by_guid(world, params.guid)?;
    let previous = read_reflected_component_json(world, entity, &params.type_path)
        .map_err(BrpError::component_error)?
        .ok_or_else(|| BrpError::component_not_present(&params.type_path, entity))?;

    remove_reflected_component(world, entity, &params.type_path).map_err(BrpError::component_error)?;

    if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
        history.record(
            EditorCommand::RemoveComponent {
                guid: params.guid,
                type_path: params.type_path.clone(),
                previous_value: previous,
            },
            None,
            now_epoch_ms(),
        );
    }

    sync_entity_prefab_overrides_if_member(world, params.guid);
    mark_mutation_dirty(world, true, Some(params.guid));

    serialize_result(ComponentMutationResult {
        guid: params.guid,
        type_path: params.type_path,
    })
}

fn handle_history_undo(In(_params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let res = undo_last(world).map_err(BrpError::internal)?;
    serialize_result(res)
}

fn handle_history_redo(In(_params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let res = redo_last(world).map_err(BrpError::internal)?;
    serialize_result(res)
}

fn handle_history_state(In(_params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let state = world
        .get_resource::<EditorHistory>()
        .map(EditorHistory::state)
        .unwrap_or_default();
    serialize_result(state)
}

fn handle_prefab_save(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: PrefabSaveParams = parse_some(params)?;
    if params.path.trim().is_empty() {
        return Err(BrpError {
            code: error_codes::INVALID_PARAMS,
            message: "Prefab save path must not be empty".to_owned(),
            data: None,
        });
    }
    let root_entity = lookup_entity_by_guid(world, params.root_guid)?;
    let ensured = ensure_prefab_extension(&params.path);
    let abs_path = resolve_project_path(world, &ensured.to_string_lossy());
    let project_root = world
        .get_resource::<ProjectPaths>()
        .map(|paths| paths.root_dir.clone());
    let prefab_path_key = normalize_prefab_path(&abs_path, project_root.as_deref());

    let (root_source_guid, entity_count) = save_prefab_to_file(
        world,
        root_entity,
        &abs_path,
        &prefab_path_key,
        params.name.as_deref(),
    )
    .map_err(BrpError::internal)?;

    mark_mutation_dirty(world, true, Some(params.root_guid));

    serialize_result(PrefabSaveResult {
        prefab_path: prefab_path_key,
        root_source_guid,
        entity_count,
    })
}

fn handle_prefab_spawn(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: PrefabSpawnParams = parse_some(params)?;
    if params.path.trim().is_empty() {
        return Err(BrpError {
            code: error_codes::INVALID_PARAMS,
            message: "Prefab path must not be empty".to_owned(),
            data: None,
        });
    }
    let parent_entity = match params.parent_guid {
        Some(parent_guid) => Some(lookup_entity_by_guid(world, parent_guid)?),
        None => None,
    };

    let abs_path = resolve_project_path(world, &params.path);
    let project_root = world
        .get_resource::<ProjectPaths>()
        .map(|paths| paths.root_dir.clone());
    let prefab_path_key = normalize_prefab_path(&abs_path, project_root.as_deref());

    let (spawned_root, guid) = spawn_prefab_from_file(
        world,
        &abs_path,
        &prefab_path_key,
        parent_entity,
        params.position.map(Vec3::from_array),
    )
    .map_err(BrpError::internal)?;

    let name = world
        .get::<Name>(spawned_root)
        .map_or_else(|| "Prefab".to_owned(), |n| n.as_str().to_owned());

    let snapshot = snapshot_subtree(world, spawned_root).map_err(BrpError::internal)?;
    if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
        history.record(
            EditorCommand::CreateEntity {
                snapshot,
                label: format!("Instantiate {name}"),
            },
            None,
            now_epoch_ms(),
        );
    }

    mark_mutation_dirty(world, true, None);

    serialize_result(PrefabSpawnResult {
        guid,
        entity_bits: spawned_root.to_bits(),
        name,
        prefab_path: prefab_path_key,
    })
}

fn handle_prefab_apply(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: PrefabInstanceActionParams = parse_some(params)?;
    let target_entity = lookup_entity_by_guid(world, params.guid)?;
    let Some((instance_root, prefab_path)) = resolve_prefab_instance_root(world, target_entity)
    else {
        return Err(BrpError {
            code: error_codes::INVALID_PARAMS,
            message: format!("Entity {} does not belong to a PrefabInstance", params.guid),
            data: None,
        });
    };

    let abs_path = resolve_project_path(world, &prefab_path);
    let updated_guids =
        apply_prefab_instance(world, instance_root, &abs_path).map_err(BrpError::internal)?;

    if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
        history.queue_inspector_refresh(&updated_guids);
    }
    mark_mutation_dirty(world, true, None);

    serialize_result(PrefabApplyResult {
        prefab_path,
        updated_guids,
    })
}

fn handle_prefab_revert(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: PrefabRevertParams = parse_some(params)?;
    let target_entity = lookup_entity_by_guid(world, params.guid)?;
    let Some((instance_root, prefab_path)) = resolve_prefab_instance_root(world, target_entity)
    else {
        return Err(BrpError {
            code: error_codes::INVALID_PARAMS,
            message: format!("Entity {} does not belong to a PrefabInstance", params.guid),
            data: None,
        });
    };

    let component_filter_source = match params.type_path.as_deref() {
        Some(tp) => {
            let member = world.get::<PrefabMember>(target_entity).copied().ok_or_else(|| {
                BrpError {
                    code: error_codes::INVALID_PARAMS,
                    message: format!("Entity {} has no PrefabMember component", params.guid),
                    data: None,
                }
            })?;
            Some((member.source_guid, tp.to_owned()))
        }
        None => None,
    };

    let abs_path = resolve_project_path(world, &prefab_path);
    let before = snapshot_subtree(world, instance_root).map_err(BrpError::internal)?;
    let filter_ref = component_filter_source
        .as_ref()
        .map(|(src_guid, tp)| (*src_guid, tp.as_str()));
    let reverted_guids = revert_prefab_instance(world, instance_root, &abs_path, filter_ref)
        .map_err(BrpError::internal)?;
    let after = snapshot_subtree(world, instance_root).map_err(BrpError::internal)?;

    let label = match component_filter_source {
        Some((_, ref tp)) => format!("Revert {}", short_type_name(tp)),
        None => "Revert Prefab Instance".to_owned(),
    };

    let root_guid = world.get::<EntityGuid>(instance_root).copied();
    if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
        history.record(
            EditorCommand::RestoreSubtree {
                before,
                after,
                label,
            },
            None,
            now_epoch_ms(),
        );
        history.queue_inspector_refresh(&reverted_guids);
        if let Some(r_guid) = root_guid {
            history.queue_inspector_refresh(&[r_guid]);
        }
    }
    mark_mutation_dirty(world, true, None);

    serialize_result(PrefabRevertResult {
        prefab_path,
        reverted_guids,
    })
}

fn handle_prefab_list(In(_params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let search_root = world
        .get_resource::<ProjectPaths>()
        .map(|paths| paths.root_dir.clone())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();

    let prefabs = list_prefab_files(&search_root);
    serialize_result(PrefabListResult { prefabs })
}

fn handle_viewport_set_mode(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: SetViewportModeParams = parse_some(params)?;
    {
        let Some(mut config) = world.get_resource_mut::<ViewportConfig>() else {
            return Err(BrpError::resource_not_present("ViewportConfig"));
        };
        config.mode = params.mode;
    }

    if let Some(mut dirty) = world.get_resource_mut::<BridgeDirtyFlags>() {
        dirty.viewport_dirty = true;
    }

    serialize_result(SetViewportModeResult { mode: params.mode })
}

fn asset_error_to_brp(err: AssetError) -> BrpError {
    match err {
        AssetError::HasDependents { uuid, dependents } => BrpError {
            code: error_codes::INVALID_PARAMS,
            message: format!(
                "Asset {uuid} is still referenced by {} dependent(s): {}",
                dependents.len(),
                dependents.join(", ")
            ),
            data: Some(json!({
                "uuid": uuid,
                "blocked_by_dependents": dependents,
            })),
        },
        AssetError::NotFound(_)
        | AssetError::InvalidRelativePath(_)
        | AssetError::DestinationExists(_)
        | AssetError::InvalidData(_) => BrpError {
            code: error_codes::INVALID_PARAMS,
            message: err.to_string(),
            data: None,
        },
        AssetError::Io { .. } | AssetError::RonParse { .. } | AssetError::RonSerialize(_) => {
            BrpError::internal(err)
        }
    }
}

fn queue_missing_asset_thumbnails(world: &mut World) {
    let Some(registry) = world.get_resource::<AssetRegistry>().cloned() else {
        return;
    };
    if let Some(mut service) = world.get_resource_mut::<ThumbnailService>() {
        service.request_all_missing(&registry);
    }
}

fn handle_asset_list(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let filter: AssetListParams = match params {
        Some(Value::Null) | None => AssetListParams::default(),
        Some(other) => parse(other)?,
    };
    let Some(registry) = world.get_resource::<AssetRegistry>() else {
        return Err(BrpError::resource_not_present("AssetRegistry"));
    };
    serialize_result(registry.snapshot(&filter))
}

fn handle_asset_import(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: AssetImportParams = parse_some(params)?;
    let source_paths: Vec<PathBuf> = params.source_paths.iter().map(PathBuf::from).collect();
    let imported = {
        let Some(mut registry) = world.get_resource_mut::<AssetRegistry>() else {
            return Err(BrpError::resource_not_present("AssetRegistry"));
        };
        registry
            .import_external_files(&source_paths, &params.target_folder)
            .map_err(asset_error_to_brp)?
    };
    queue_missing_asset_thumbnails(world);
    serialize_result(AssetImportResult { imported })
}

fn handle_asset_move(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: AssetMoveParams = parse_some(params)?;
    let moved = {
        let Some(mut registry) = world.get_resource_mut::<AssetRegistry>() else {
            return Err(BrpError::resource_not_present("AssetRegistry"));
        };
        registry
            .move_or_rename(params.uuid, &params.new_relative_path)
            .map_err(asset_error_to_brp)?
    };
    queue_missing_asset_thumbnails(world);
    serialize_result(moved)
}

fn handle_asset_delete(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: AssetDeleteParams = parse_some(params)?;
    let dependents = {
        let Some(mut registry) = world.get_resource_mut::<AssetRegistry>() else {
            return Err(BrpError::resource_not_present("AssetRegistry"));
        };
        registry
            .delete_asset(params.uuid, params.force)
            .map_err(asset_error_to_brp)?
    };
    serialize_result(AssetDeleteResult {
        deleted_uuid: params.uuid,
        dependents,
    })
}

fn handle_asset_create_folder(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: AssetCreateFolderParams = parse_some(params)?;
    let entry = {
        let Some(mut registry) = world.get_resource_mut::<AssetRegistry>() else {
            return Err(BrpError::resource_not_present("AssetRegistry"));
        };
        registry
            .create_folder(&params.relative_path)
            .map_err(asset_error_to_brp)?
    };
    serialize_result(entry)
}

fn handle_asset_update_settings(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: AssetUpdateSettingsParams = parse_some(params)?;
    let updated = {
        let Some(mut registry) = world.get_resource_mut::<AssetRegistry>() else {
            return Err(BrpError::resource_not_present("AssetRegistry"));
        };
        registry
            .update_import_settings(params.uuid, params.import_settings, params.tags)
            .map_err(asset_error_to_brp)?
    };
    if let Some(asset_server) = world.get_resource::<AssetServer>() {
        asset_server.reload(updated.relative_path.clone());
    }
    serialize_result(updated)
}

fn handle_asset_thumbnail(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: AssetThumbnailParams = parse_some(params)?;
    let (record, abs_path) = {
        let Some(registry) = world.get_resource::<AssetRegistry>() else {
            return Err(BrpError::resource_not_present("AssetRegistry"));
        };
        let shared = registry.shared();
        let rec = shared
            .get(params.uuid)
            .ok_or_else(|| asset_error_to_brp(AssetError::NotFound(params.uuid)))?;
        let abs = shared
            .resolve_abs_path(params.uuid)
            .ok_or_else(|| asset_error_to_brp(AssetError::NotFound(params.uuid)))?;
        (rec, abs)
    };

    let data_url = if let Some(mut service) = world.get_resource_mut::<ThumbnailService>() {
        service.request_for_record(&record, abs_path);
        service.get_data_url(params.uuid, &record.source_hash)
    } else {
        None
    };

    serialize_result(AssetThumbnailResult {
        uuid: params.uuid,
        ready: data_url.is_some(),
        data_url,
    })
}

fn resolve_viewport_drop_position(
    world: &mut World,
    screen_pos: Vec2,
) -> (Vec3, ViewportCameraMode) {
    let camera_mode = world
        .get_resource::<ViewportConfig>()
        .map_or(ViewportCameraMode::Mode3D, |cfg| cfg.camera_mode);

    let mut cam_q = world.query::<(
        &Camera,
        &GlobalTransform,
        Option<&EditorCamera3d>,
        Option<&EditorCamera2d>,
    )>();

    let mut chosen_ray = None;
    let mut fallback_ray = None;

    for (camera, tfm, cam3d, cam2d) in cam_q.iter(world) {
        if !camera.is_active {
            continue;
        }
        let Ok(ray) = camera.viewport_to_world(tfm, screen_pos) else {
            continue;
        };
        let matches_mode = match camera_mode {
            ViewportCameraMode::Mode3D => cam3d.is_some(),
            ViewportCameraMode::Mode2D => cam2d.is_some(),
        };
        if matches_mode {
            chosen_ray = Some(ray);
            break;
        }
        if fallback_ray.is_none() {
            fallback_ray = Some(ray);
        }
    }

    let Some(ray) = chosen_ray.or(fallback_ray) else {
        return (Vec3::ZERO, camera_mode);
    };

    let pos = match camera_mode {
        ViewportCameraMode::Mode3D => {
            if ray.direction.y.abs() > 1e-4 {
                let t = -ray.origin.y / ray.direction.y;
                if t > 0.0 && t < 120.0 {
                    ray.origin + ray.direction * t
                } else {
                    ray.origin + ray.direction * 6.0
                }
            } else {
                ray.origin + ray.direction * 6.0
            }
        }
        ViewportCameraMode::Mode2D => {
            if ray.direction.z.abs() > 1e-4 {
                let t = -ray.origin.z / ray.direction.z;
                let hit = ray.origin + ray.direction * t;
                Vec3::new(hit.x, hit.y, 0.0)
            } else {
                Vec3::new(ray.origin.x, ray.origin.y, 0.0)
            }
        }
    };

    (pos, camera_mode)
}

fn handle_asset_spawn_in_viewport(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: AssetSpawnInViewportParams = parse_some(params)?;
    let (record, abs_path) = {
        let Some(registry) = world.get_resource::<AssetRegistry>() else {
            return Err(BrpError::resource_not_present("AssetRegistry"));
        };
        let shared = registry.shared();
        let rec = shared
            .get(params.uuid)
            .ok_or_else(|| asset_error_to_brp(AssetError::NotFound(params.uuid)))?;
        let abs = shared
            .resolve_abs_path(params.uuid)
            .ok_or_else(|| asset_error_to_brp(AssetError::NotFound(params.uuid)))?;
        (rec, abs)
    };

    let (pos, camera_mode) =
        resolve_viewport_drop_position(world, Vec2::new(params.x, params.y));
    let stem = record
        .name
        .split('.')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&record.name)
        .to_owned();

    let (entity, guid, label) = match record.kind {
        AssetKind::GltfModel => {
            let guid = EntityGuid::new();
            let entity = world
                .spawn((
                    SceneEntity,
                    guid,
                    Name::new(stem.clone()),
                    Transform::from_translation(pos),
                    Visibility::default(),
                    SceneGltfModel {
                        asset: Some(params.uuid),
                        scene_index: 0,
                        scale_multiplier: 1.0,
                    },
                ))
                .id();
            (entity, guid, format!("Spawn glTF {stem}"))
        }
        AssetKind::Image => {
            let is_2d = world
                .get_resource::<ActiveScene>()
                .is_some_and(|s| s.world_mode == WorldMode::Mode2D)
                || camera_mode == ViewportCameraMode::Mode2D;
            let guid = EntityGuid::new();
            if is_2d {
                let entity = world
                    .spawn((
                        SceneEntity,
                        guid,
                        Name::new(stem.clone()),
                        Transform::from_translation(Vec3::new(pos.x, pos.y, 0.0)),
                        Visibility::default(),
                        SpriteDesc {
                            color: Color::WHITE,
                            texture: Some(params.uuid),
                            atlas_index: None,
                            custom_size: Some(Vec2::splat(64.0)),
                            flip_x: false,
                            flip_y: false,
                        },
                    ))
                    .id();
                (entity, guid, format!("Spawn Sprite {stem}"))
            } else {
                let entity = world
                    .spawn((
                        SceneEntity,
                        guid,
                        Name::new(stem.clone()),
                        Transform::from_translation(pos + Vec3::Y * 0.5),
                        Visibility::default(),
                        MeshPrimitive3d::Cube { size: Vec3::ONE },
                        StandardMaterialDesc {
                            base_color: Color::WHITE,
                            base_color_texture: Some(params.uuid),
                            ..Default::default()
                        },
                    ))
                    .id();
                (entity, guid, format!("Spawn Textured Cube {stem}"))
            }
        }
        AssetKind::Tilemap => {
            let guid = EntityGuid::new();
            let entity = world
                .spawn((
                    SceneEntity,
                    guid,
                    Name::new(stem.clone()),
                    Transform::from_translation(Vec3::new(pos.x, pos.y, 0.0)),
                    Visibility::default(),
                    SceneTilemap {
                        asset: Some(params.uuid),
                        tile_scale: 1.0,
                    },
                ))
                .id();
            (entity, guid, format!("Spawn Tilemap {stem}"))
        }
        AssetKind::Prefab => {
            let project_root = world
                .get_resource::<ProjectPaths>()
                .map(|p| p.root_dir.clone());
            let key = normalize_prefab_path(&abs_path, project_root.as_deref());
            let (entity, guid) =
                spawn_prefab_from_file(world, &abs_path, &key, None, Some(pos))
                    .map_err(BrpError::internal)?;
            (entity, guid, format!("Spawn Prefab {stem}"))
        }
        AssetKind::Scene | AssetKind::Script | AssetKind::Material | AssetKind::Unknown => {
            return Err(BrpError {
                code: error_codes::INVALID_PARAMS,
                message: format!(
                    "Asset '{}' ({:?}) cannot be spawned directly into Viewport",
                    record.relative_path, record.kind
                ),
                data: None,
            });
        }
    };

    let spawned_name = world
        .get::<Name>(entity)
        .map_or(stem, |n| n.as_str().to_owned());

    let snapshot = snapshot_subtree(world, entity).map_err(BrpError::internal)?;
    if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
        history.record(
            EditorCommand::CreateEntity { snapshot, label },
            None,
            now_epoch_ms(),
        );
    }

    let selection_changed = world
        .get_resource_mut::<EditorSelection>()
        .is_some_and(|mut selection| selection.set(vec![guid]));
    if selection_changed && let Some(mut dirty) = world.get_resource_mut::<BridgeDirtyFlags>() {
        dirty.selection_dirty = true;
    }
    mark_mutation_dirty(world, true, None);

    serialize_result(AssetSpawnInViewportResult {
        guid,
        entity_bits: entity.to_bits(),
        name: spawned_name,
    })
}

fn handle_asset_rescan(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let filter: AssetListParams = match params {
        Some(Value::Null) | None => AssetListParams::default(),
        Some(other) => parse(other)?,
    };
    let snapshot = {
        let Some(mut registry) = world.get_resource_mut::<AssetRegistry>() else {
            return Err(BrpError::resource_not_present("AssetRegistry"));
        };
        registry.scan_all().map_err(asset_error_to_brp)?;
        registry.snapshot(&filter)
    };
    queue_missing_asset_thumbnails(world);
    serialize_result(snapshot)
}
