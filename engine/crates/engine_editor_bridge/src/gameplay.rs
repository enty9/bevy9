//! Editor endpoints for input overrides, physics visualization, and audio preview/mixing.

use bevy_asset::{AssetServer, Assets, LoadState};
use bevy_ecs::{
    system::{Command, In, Query, Res, ResMut},
    world::World,
};
use bevy_remote::{builtin_methods::parse_some, error_codes, BrpError, BrpResult, RemotePlugin};
use bevy_settings::SaveSettingsSync;
use bevy_transform::components::GlobalTransform;
use engine_assets::AssetRegistry;
use engine_audio::{resolve_clip, AudioBus, AudioEmitter, AudioEngine, AudioMixer, BusSettings};
use engine_core::{AssetUuid, EntityGuid, EntityGuidMap};
use engine_input::{
    ActionState, InputContexts, InputDomain, InputMappingAsset, InputOverrides, InputSource,
    ProjectInputMapping,
};
use engine_physics::PhysicsOptions;
use engine_render::camera::EditorCameraState;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::EditorSelection;

pub(crate) fn register(plugin: RemotePlugin) -> RemotePlugin {
    plugin
        .with_method_main("engine.input.bindings", list_bindings)
        .with_method_main("engine.input.rebind", rebind)
        .with_method_main("engine.physics.debug", debug)
        .with_method_main("engine.audio.mixer", mixer)
        .with_method_main("engine.audio.set_bus", set_bus)
        .with_method_main("engine.audio.preview", preview)
        .with_method_main("engine.audio.preview_emitter", preview_emitter)
}

fn unavailable() -> BrpError {
    BrpError::internal("gameplay subsystem unavailable")
}

fn invalid(message: impl Into<String>) -> BrpError {
    BrpError {
        code: error_codes::INVALID_PARAMS,
        message: message.into(),
        data: None,
    }
}

fn mapping(world: &World) -> Option<&InputMappingAsset> {
    let handle = world.get_resource::<ProjectInputMapping>()?;
    world
        .get_resource::<Assets<InputMappingAsset>>()?
        .get(&handle.0)
}

fn list_bindings(In(_): In<Option<Value>>, world: &mut World) -> BrpResult {
    let Some(mapping) = mapping(world) else {
        let load = world
            .get_resource::<ProjectInputMapping>()
            .and_then(|handle| {
                world
                    .get_resource::<AssetServer>()?
                    .get_load_state(handle.0.id())
            });
        return Ok(json!({"bindings": [], "status": match load {
            Some(LoadState::Failed(err)) => format!("Couldn't load the input mapping: {err}"),
            Some(_) => "Loading input mapping…".into(),
            None => "No project input mapping configured.".into(),
        }}));
    };
    let overrides = world
        .get_resource::<InputOverrides>()
        .ok_or_else(unavailable)?;
    let bindings = mapping
        .contexts
        .iter()
        .flat_map(|context| {
            context.bindings.iter().map(|binding| json!({
        "id": binding.id, "action": binding.action, "context": context.id, "domain": context.domain,
        "source": overrides.source(binding), "default_source": binding.source,
        "overridden": overrides.bindings.contains_key(&binding.id),
    }))
        })
        .collect::<Vec<_>>();
    Ok(json!({"bindings": bindings, "status": null}))
}

pub(crate) fn focus_selection(
    contexts: Option<Res<InputContexts>>,
    actions: Option<Res<ActionState>>,
    guids: Option<Res<EntityGuidMap>>,
    selection: Res<EditorSelection>,
    camera: Option<ResMut<EditorCameraState>>,
    transforms: Query<&GlobalTransform>,
) {
    let (Some(contexts), Some(actions), Some(guids), Some(mut camera)) =
        (contexts, actions, guids, camera)
    else {
        return;
    };
    if contexts.domain != InputDomain::Editor || !actions.get("editor_focus").just_pressed {
        return;
    }
    if let Some(target) = selection
        .guids()
        .last()
        .and_then(|&guid| guids.get_entity(guid))
        .and_then(|e| transforms.get(e).ok())
    {
        camera.focus = target.translation();
        camera.center_2d = target.translation().truncate();
    }
}

#[derive(Deserialize)]
struct Rebind {
    id: String,
    source: Option<InputSource>,
}

fn rebind(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: Rebind = parse_some(params)?;
    if !mapping(world).is_some_and(|mapping| {
        mapping
            .contexts
            .iter()
            .any(|c| c.bindings.iter().any(|b| b.id == params.id))
    }) {
        return Err(invalid("unknown input binding"));
    }
    world
        .get_resource_mut::<InputOverrides>()
        .ok_or_else(unavailable)?
        .set(params.id, params.source);
    SaveSettingsSync::IfChanged.apply(world);
    list_bindings(In(None), world)
}

#[derive(Deserialize)]
struct DebugArgs {
    enabled: bool,
}

fn debug(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let mut options = world
        .get_resource_mut::<PhysicsOptions>()
        .ok_or_else(unavailable)?;
    if let Some(params) = params {
        let params: DebugArgs = parse_some(Some(params))?;
        options.debug = params.enabled;
    }
    Ok(json!({"enabled": options.debug}))
}

fn mixer(In(_): In<Option<Value>>, world: &mut World) -> BrpResult {
    let mixer = world.get_resource::<AudioMixer>().ok_or_else(unavailable)?;
    let available = world
        .get_resource::<AudioEngine>()
        .is_some_and(AudioEngine::available);
    Ok(json!({"available": available, "buses": mixer}))
}

#[derive(Deserialize)]
struct BusArgs {
    bus: AudioBus,
    settings: BusSettings,
}

fn set_bus(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: BusArgs = parse_some(params)?;
    world
        .get_resource_mut::<AudioMixer>()
        .ok_or_else(unavailable)?
        .set(params.bus, params.settings);
    mixer(In(None), world)
}

#[derive(Deserialize)]
struct PreviewArgs {
    uuid: Option<AssetUuid>,
}

fn preview(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: PreviewArgs = parse_some(params)?;
    let path = params
        .uuid
        .map(|uuid| {
            let registry = world
                .get_resource::<AssetRegistry>()
                .ok_or_else(unavailable)?;
            resolve_clip(registry, uuid).map_err(|err| invalid(err.to_string()))
        })
        .transpose()?;
    let mut audio = world
        .get_resource_mut::<AudioEngine>()
        .ok_or_else(unavailable)?;
    if let Some(path) = path {
        audio.preview(path).map_err(BrpError::internal)?;
    } else {
        audio.stop_preview();
    }
    Ok(json!({}))
}

#[derive(Deserialize)]
struct EmitterArgs {
    guid: EntityGuid,
}

fn preview_emitter(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: EmitterArgs = parse_some(params)?;
    let entity = world
        .get_resource::<EntityGuidMap>()
        .and_then(|guids| guids.get_entity(params.guid))
        .ok_or_else(|| invalid("unknown entity"))?;
    let emitter = world
        .get::<AudioEmitter>(entity)
        .cloned()
        .ok_or_else(|| invalid("entity has no AudioEmitter"))?;
    let position = world
        .get::<GlobalTransform>(entity)
        .ok_or_else(|| invalid("emitter is missing a transform"))?
        .translation();
    let clip = emitter
        .clip
        .ok_or_else(|| invalid("assign a clip before previewing the emitter"))?;
    let registry = world
        .get_resource::<AssetRegistry>()
        .ok_or_else(unavailable)?;
    let path = resolve_clip(registry, clip).map_err(|err| invalid(err.to_string()))?;
    world
        .get_resource_mut::<AudioEngine>()
        .ok_or_else(unavailable)?
        .preview_emitter(entity, &emitter, path, position)
        .map_err(BrpError::internal)?;
    Ok(json!({}))
}
