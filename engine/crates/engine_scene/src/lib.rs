//! Versioned `.level.ron` scene serialization, 2D/3D descriptors, hydration, and presets.

pub mod components;
pub mod hydration;
pub mod presets;
pub mod serialization;

/// Commonly used scene types and functions.
pub mod prelude {
    pub use crate::{
        components::{
            ActiveScene, ColorMaterialDesc, MeshPrimitive2d, MeshPrimitive3d, SceneCamera2d,
            SceneCamera3d, SceneCameraActivationPolicy, SceneDirectionalLight, SceneEntity,
            ScenePointLight, SpriteDesc, StandardMaterialDesc,
        },
        presets::{default_preset_name, spawn_preset},
        serialization::{
            build_scene_tree, clear_scene_entities, collect_subtree_entities, despawn_subtree,
            load_level_from_file, load_level_from_str, read_reflected_component_json,
            remove_reflected_component, reset_to_new_scene, respawn_subtree_from_snapshot,
            save_level_to_file, save_level_to_string, snapshot_subtree,
            write_reflected_component_json, LevelMetadata, SceneError, SceneSerializationFilter,
            SubtreeSnapshot, CURRENT_LEVEL_VERSION,
        },
        EngineScenePlugin,
    };
}

use bevy_app::{App, Plugin, PostStartup, Update};
use bevy_camera::visibility::Visibility;
use bevy_color::{Color, LinearRgba};
use bevy_ecs::{
    hierarchy::ChildOf,
    name::Name,
    reflect::AppTypeRegistry,
    schedule::{IntoScheduleConfigs, SystemSet},
};
use bevy_transform::components::Transform;
use engine_core::{schedule::EngineSet, EntityGuid, WorldMode};

pub use prelude::*;

/// System set for scene descriptor hydration systems.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SceneHydrationSystems;

/// Plugin registering scene descriptor types, serialization filters, and hydration systems.
#[derive(Debug, Default, Clone, Copy)]
pub struct EngineScenePlugin;

impl Plugin for EngineScenePlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<AppTypeRegistry>() {
            app.init_resource::<AppTypeRegistry>();
        }

        app.register_type::<EntityGuid>()
            .register_type::<WorldMode>()
            .register_type::<Name>()
            .register_type::<Transform>()
            .register_type::<Visibility>()
            .register_type::<ChildOf>()
            .register_type::<Color>()
            .register_type::<LinearRgba>()
            .register_type::<SceneEntity>()
            .register_type::<MeshPrimitive3d>()
            .register_type::<StandardMaterialDesc>()
            .register_type::<MeshPrimitive2d>()
            .register_type::<ColorMaterialDesc>()
            .register_type::<SpriteDesc>()
            .register_type::<SceneCamera3d>()
            .register_type::<SceneCamera2d>()
            .register_type::<SceneDirectionalLight>()
            .register_type::<ScenePointLight>()
            .register_type::<ActiveScene>()
            .register_type::<SceneCameraActivationPolicy>();

        if !app.world().contains_resource::<ActiveScene>() {
            let initial_mode = app
                .world()
                .get_resource::<WorldMode>()
                .copied()
                .unwrap_or_default();
            app.insert_resource(ActiveScene {
                world_mode: initial_mode,
                ..Default::default()
            });
        }

        if !app
            .world()
            .contains_resource::<SceneCameraActivationPolicy>()
        {
            app.init_resource::<SceneCameraActivationPolicy>();
        }

        let mut filter = app
            .world_mut()
            .get_resource_or_init::<SceneSerializationFilter>()
            .clone();
        filter
            .allow::<EntityGuid>()
            .allow::<Name>()
            .allow::<Transform>()
            .allow::<Visibility>()
            .allow::<ChildOf>()
            .allow::<SceneEntity>()
            .allow::<MeshPrimitive3d>()
            .allow::<StandardMaterialDesc>()
            .allow::<MeshPrimitive2d>()
            .allow::<ColorMaterialDesc>()
            .allow::<SpriteDesc>()
            .allow::<SceneCamera3d>()
            .allow::<SceneCamera2d>()
            .allow::<SceneDirectionalLight>()
            .allow::<ScenePointLight>();
        app.insert_resource(filter);

        app.add_systems(
            PostStartup,
            (
                hydration::hydrate_3d_meshes_and_materials,
                hydration::hydrate_2d_meshes_and_materials,
                hydration::hydrate_sprites,
                hydration::hydrate_lights,
                hydration::hydrate_cameras,
            )
                .in_set(SceneHydrationSystems),
        )
        .add_systems(
            Update,
            (
                hydration::hydrate_3d_meshes_and_materials,
                hydration::hydrate_2d_meshes_and_materials,
                hydration::hydrate_sprites,
                hydration::hydrate_lights,
                hydration::hydrate_cameras,
            )
                .in_set(SceneHydrationSystems)
                .in_set(EngineSet::Gameplay),
        );
    }
}
