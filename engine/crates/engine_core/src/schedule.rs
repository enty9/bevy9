//! Canonical system sets defining execution order across engine subsystems.

use bevy_app::{App, FixedUpdate, Update};
use bevy_ecs::schedule::{IntoScheduleConfigs, SystemSet};

/// Ordered system sets running in the variable-timestep [`Update`] schedule.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineSet {
    /// Action input sampling and context resolution.
    Input,
    /// Per-frame gameplay, cameras, and animation updates.
    Gameplay,
    /// Audio mixer updates and spatial emitter synchronization.
    Audio,
    /// Editor bridge state synchronization, picking, and gizmos.
    EditorSync,
}

/// Ordered system sets running in the fixed-timestep [`FixedUpdate`] schedule.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineFixedSet {
    /// Fixed-step buffered input state preparation.
    Input,
    /// Fixed-step script callbacks before physics simulation.
    ScriptFixed,
    /// Deterministic physics simulation step.
    Physics,
    /// Post-physics fixed-step gameplay reactions.
    Gameplay,
}

/// Configures the ordering chains for [`EngineSet`] and [`EngineFixedSet`].
pub fn configure_engine_schedules(app: &mut App) {
    app.configure_sets(
        Update,
        (
            EngineSet::Input,
            EngineSet::Gameplay,
            EngineSet::Audio,
            EngineSet::EditorSync,
        )
            .chain(),
    )
    .configure_sets(
        FixedUpdate,
        (
            EngineFixedSet::Input,
            EngineFixedSet::ScriptFixed,
            EngineFixedSet::Physics,
            EngineFixedSet::Gameplay,
        )
            .chain(),
    );
}
