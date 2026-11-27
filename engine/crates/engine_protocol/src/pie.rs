//! Play-In-Editor (PIE) lifecycle states and status telemetry contracts.

use serde::{Deserialize, Serialize};

/// Execution state of a Play-In-Editor (PIE) session shared across runtime, BRP, and UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PieState {
    /// Normal scene authoring mode (gameplay simulation stopped, editor tools active).
    #[default]
    Editing,
    /// Live Play-In-Editor simulation running.
    Playing,
    /// Play-In-Editor simulation paused with the live PIE world preserved.
    Paused,
}

impl PieState {
    /// Returns `true` when the editor is in normal authoring mode (`Editing`).
    #[must_use]
    pub const fn is_editing(self) -> bool {
        matches!(self, Self::Editing)
    }

    /// Returns `true` when a PIE session is active (`Playing` or `Paused`).
    #[must_use]
    pub const fn is_active(self) -> bool {
        !matches!(self, Self::Editing)
    }

    /// Returns `true` when PIE simulation is actively running (`Playing`).
    #[must_use]
    pub const fn is_playing(self) -> bool {
        matches!(self, Self::Playing)
    }

    /// Returns `true` when PIE simulation is paused (`Paused`).
    #[must_use]
    pub const fn is_paused(self) -> bool {
        matches!(self, Self::Paused)
    }
}

/// Play-In-Editor lifecycle status and session telemetry returned by BRP and pushed over IPC.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct PieStatePayload {
    /// Current PIE lifecycle state (`editing`, `playing`, or `paused`).
    pub state: PieState,
    /// Number of fixed simulation ticks executed during the active PIE session (`0` while editing).
    pub fixed_steps: u64,
    /// Number of update frames executed during the active PIE session (`0` while editing).
    pub frame_count: u64,
    /// Total simulated gameplay time in seconds during the active PIE session (`0.0` while editing).
    pub elapsed_secs: f64,
    /// Whether an active scene camera (`SceneCamera3d` / `SceneCamera2d`) currently drives the viewport.
    pub using_scene_camera: bool,
}
