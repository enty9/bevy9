//! Core engine types, lifecycle schedules, and project manifest support.

pub mod guid;
pub mod manifest;
pub mod schedule;
pub mod world_mode;

/// Commonly used core engine types.
pub mod prelude {
    pub use crate::{
        guid::{EntityGuid, EntityGuidMap},
        manifest::{ProjectManifest, ProjectManifestError, ProjectMetadata, ProjectPaths},
        schedule::{EngineFixedSet, EngineSet},
        world_mode::WorldMode,
        EngineCorePlugin,
    };
}

use bevy_app::{App, Plugin};
use bevy_ecs::reflect::AppTypeRegistry;

pub use crate::{
    guid::{register_guid_hooks, EntityGuid, EntityGuidMap},
    manifest::{
        ProjectManifest, ProjectManifestError, ProjectMetadata, ProjectPaths, WindowConfig,
        CURRENT_MANIFEST_VERSION, MANIFEST_FILE_NAME,
    },
    schedule::{configure_engine_schedules, EngineFixedSet, EngineSet},
    world_mode::{ParseWorldModeError, WorldMode},
};

/// Core plugin registering foundational engine types, GUID hooks, and execution schedules.
#[derive(Debug, Default, Clone, Copy)]
pub struct EngineCorePlugin;

impl Plugin for EngineCorePlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<AppTypeRegistry>() {
            app.init_resource::<AppTypeRegistry>();
        }

        app.register_type::<EntityGuid>()
            .register_type::<WorldMode>()
            .init_resource::<EntityGuidMap>();

        if !app.world().contains_resource::<WorldMode>() {
            let default_mode = app
                .world()
                .get_resource::<ProjectManifest>()
                .map_or_else(WorldMode::default, |manifest| manifest.project.default_mode);
            app.insert_resource(default_mode);
        }

        register_guid_hooks(app.world_mut());
        configure_engine_schedules(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_app::{FixedUpdate, Update};
    use bevy_ecs::{resource::Resource, schedule::IntoScheduleConfigs, system::ResMut};
    use core::any::TypeId;

    #[derive(Resource, Default)]
    struct ExecutionTrace(Vec<&'static str>);

    #[test]
    fn plugin_registers_reflection_and_uses_manifest_world_mode() {
        let mut app = App::new();
        app.insert_resource(ProjectManifest::new("2D Sample", WorldMode::Mode2D));
        app.add_plugins(EngineCorePlugin);

        assert_eq!(*app.world().resource::<WorldMode>(), WorldMode::Mode2D);

        {
            let type_registry = app.world().resource::<AppTypeRegistry>().read();
            assert!(type_registry.contains(TypeId::of::<EntityGuid>()));
            assert!(type_registry.contains(TypeId::of::<WorldMode>()));
        }

        app.update();
    }

    #[test]
    fn plugin_preserves_preinserted_world_mode_and_orders_engine_sets() {
        let mut app = App::new();
        app.insert_resource(ProjectManifest::new("Hybrid Sample", WorldMode::Mode3D));
        app.insert_resource(WorldMode::Hybrid);
        app.init_resource::<ExecutionTrace>();
        app.add_plugins(EngineCorePlugin);

        // Pre-inserted WorldMode takes precedence over manifest default_mode.
        assert_eq!(*app.world().resource::<WorldMode>(), WorldMode::Hybrid);

        app.add_systems(
            Update,
            (
                (|mut t: ResMut<ExecutionTrace>| t.0.push("editor_sync"))
                    .in_set(EngineSet::EditorSync),
                (|mut t: ResMut<ExecutionTrace>| t.0.push("audio")).in_set(EngineSet::Audio),
                (|mut t: ResMut<ExecutionTrace>| t.0.push("gameplay")).in_set(EngineSet::Gameplay),
                (|mut t: ResMut<ExecutionTrace>| t.0.push("input")).in_set(EngineSet::Input),
            ),
        );
        app.add_systems(
            FixedUpdate,
            (
                (|mut t: ResMut<ExecutionTrace>| t.0.push("fixed_gameplay"))
                    .in_set(EngineFixedSet::Gameplay),
                (|mut t: ResMut<ExecutionTrace>| t.0.push("fixed_physics"))
                    .in_set(EngineFixedSet::Physics),
                (|mut t: ResMut<ExecutionTrace>| t.0.push("fixed_script"))
                    .in_set(EngineFixedSet::ScriptFixed),
                (|mut t: ResMut<ExecutionTrace>| t.0.push("fixed_input"))
                    .in_set(EngineFixedSet::Input),
            ),
        );

        app.world_mut().run_schedule(Update);
        app.world_mut().run_schedule(FixedUpdate);

        let trace = &app.world().resource::<ExecutionTrace>().0;
        assert_eq!(
            trace,
            &[
                "input",
                "gameplay",
                "audio",
                "editor_sync",
                "fixed_input",
                "fixed_script",
                "fixed_physics",
                "fixed_gameplay",
            ]
        );
    }
}
