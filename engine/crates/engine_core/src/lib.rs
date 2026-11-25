//! Core engine types, lifecycle schedules, and project manifest support.

use bevy_app::{App, Plugin};

/// Core plugin registering foundational engine types and schedules.
#[derive(Debug, Default, Clone, Copy)]
pub struct EngineCorePlugin;

impl Plugin for EngineCorePlugin {
    fn build(&self, _app: &mut App) {}
}
