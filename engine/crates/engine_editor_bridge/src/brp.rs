//! Bevy Remote Protocol (BRP) plugin builder and custom engine RPC methods.

use std::path::PathBuf;

use bevy_ecs::{name::Name, query::With, system::In, world::World};
use bevy_math::Vec3;
use bevy_remote::{
    builtin_methods::{parse, parse_some},
    error_codes,
    http::{Headers, RemoteHttpPlugin},
    BrpError, BrpResult, RemotePlugin,
};
use bevy_transform::components::Transform;
use engine_core::{EntityGuidMap, ProjectPaths, WorldMode};
use engine_protocol::{
    SceneLoadParams, SceneNewParams, SceneOperationResult, SceneSaveParams, SetViewportModeParams,
    SetViewportModeResult, SpawnPresetParams, SpawnPresetResult, ViewportCameraMode,
    WorldTreeResult, METHOD_ENTITY_SPAWN_PRESET, METHOD_SCENE_LOAD, METHOD_SCENE_NEW,
    METHOD_SCENE_SAVE, METHOD_VIEWPORT_SET_MODE, METHOD_WORLD_TREE,
};
use engine_render::ViewportConfig;
use engine_scene::{
    build_scene_tree, default_preset_name, load_level_from_file, reset_to_new_scene,
    save_level_to_file, spawn_preset, ActiveScene, LevelMetadata, SceneEntity,
};
use serde::Serialize;
use serde_json::Value;

use crate::{
    config::EditorBridgeConfig,
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
        .with_method_main(METHOD_VIEWPORT_SET_MODE, handle_viewport_set_mode)
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

fn resolve_project_path(world: &World, raw_path: &str) -> PathBuf {
    let candidate = PathBuf::from(raw_path);
    if candidate.is_relative()
        && let Some(paths) = world.get_resource::<ProjectPaths>()
    {
        return paths.root_dir.join(candidate);
    }
    candidate
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

    if let Some(mut active_scene) = world.get_resource_mut::<ActiveScene>() {
        active_scene.dirty = true;
    }
    if let Some(mut dirty) = world.get_resource_mut::<BridgeDirtyFlags>() {
        dirty.hierarchy_dirty = true;
    }

    serialize_result(SpawnPresetResult {
        guid,
        entity_bits: entity.to_bits(),
        name,
    })
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
