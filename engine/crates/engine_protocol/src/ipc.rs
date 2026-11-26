//! WebSocket IPC message contracts for real-time editor <-> runtime communication.

use engine_core::{EntityGuid, WorldMode};
use serde::{Deserialize, Serialize};

use crate::viewport::{
    GizmoMode, GizmoSnapConfig, GizmoSpace, ViewportCameraMode, ViewportInputEvent, ViewportMode,
};

/// Severity levels for runtime log messages forwarded to the editor Console.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Fine-grained diagnostic output.
    Trace,
    /// Developer debugging message.
    Debug,
    /// Informational lifecycle message.
    Info,
    /// Non-fatal warning.
    Warn,
    /// Error condition.
    Error,
}

/// Structured log entry streamed over IPC to the editor Console panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorLogEntry {
    /// Log severity.
    pub level: LogLevel,
    /// Log source target/module path.
    pub target: String,
    /// Formatted log message text.
    pub message: String,
}

/// Periodic runtime performance telemetry published to the editor UI.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct FrameMetrics {
    /// Smoothed frames per second.
    pub fps: f32,
    /// Last frame delta time in milliseconds.
    pub frame_time_ms: f32,
    /// Total ECS entities in the main world.
    pub entity_count: usize,
    /// Number of entities belonging to the active scene.
    pub scene_entity_count: usize,
}

/// Undo/redo command stack availability state published to the editor UI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryStatePayload {
    /// Whether at least one undo entry is available.
    pub can_undo: bool,
    /// Whether at least one redo entry is available.
    pub can_redo: bool,
    /// Label of the next operation to undo, if any.
    pub undo_label: Option<String>,
    /// Label of the next operation to redo, if any.
    pub redo_label: Option<String>,
    /// Number of entries currently stored on the undo stack.
    pub undo_len: usize,
    /// Number of entries currently stored on the redo stack.
    pub redo_len: usize,
}

/// Hierarchical representation of a scene entity for the editor World Outliner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneTreeNode {
    /// Persistent [`EntityGuid`] assigned to the entity.
    pub guid: EntityGuid,
    /// Ephemeral ECS [`bevy_ecs::entity::Entity::to_bits`] value for direct BRP calls.
    pub entity_bits: u64,
    /// Display name from Bevy `Name` component (or fallback label).
    pub name: String,
    /// Parent entity's [`EntityGuid`], if the entity is a child.
    pub parent_guid: Option<EntityGuid>,
    /// Whether the entity is marked visible.
    pub visible: bool,
    /// Ordered child entity nodes.
    pub children: Vec<SceneTreeNode>,
    /// Fully-qualified reflected component type paths attached to this entity.
    pub components: Vec<String>,
    /// Normalized `.prefab.ron` asset path if this entity is a `PrefabInstance` root.
    #[serde(default)]
    pub prefab_path: Option<String>,
    /// Whether this entity belongs to a prefab instance subtree (root or child member).
    #[serde(default)]
    pub is_prefab_member: bool,
}

/// Messages sent from the editor UI/Tauri client to the Engine Host over WebSocket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientIpcMessage {
    /// Initial handshake sent after opening the WebSocket connection.
    Hello {
        /// Protocol version supported by the client.
        protocol_version: u32,
        /// Human-readable client label (e.g. `"editor-ui"`).
        client_name: String,
    },
    /// Forwards an input or resize event from the embedded `<canvas>`.
    ViewportInput {
        /// Encoded input/resize event.
        event: ViewportInputEvent,
    },
    /// Requests switching between [`ViewportMode::Offscreen`] and [`ViewportMode::NativeWindow`].
    SetViewportMode {
        /// Desired presentation mode.
        mode: ViewportMode,
    },
    /// Requests switching the editor camera between [`ViewportCameraMode::Mode3D`] and [`ViewportCameraMode::Mode2D`].
    SetCameraMode {
        /// Desired camera navigation mode.
        camera_mode: ViewportCameraMode,
    },
    /// Updates the current editor selection by persistent entity GUIDs.
    SelectEntities {
        /// Selected entity GUIDs.
        guids: Vec<EntityGuid>,
    },
    /// Updates active transform gizmo mode, coordinate space, and snap configuration.
    SetGizmoState {
        /// Active gizmo tool (`translate`, `rotate`, or `scale`).
        mode: GizmoMode,
        /// Active coordinate space (`world` or `local`).
        space: GizmoSpace,
        /// Snapping configuration.
        snap: GizmoSnapConfig,
    },
    /// Heartbeat ping with caller-supplied nonce.
    Ping {
        /// Timestamp or sequence token echoed back in [`ServerIpcMessage::Pong`].
        nonce: u64,
    },
}

/// Messages pushed from the Engine Host to connected WebSocket clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerIpcMessage {
    /// Handshake acknowledgment sent in response to [`ClientIpcMessage::Hello`].
    Welcome {
        /// Protocol version spoken by the runtime.
        protocol_version: u32,
        /// Active project name.
        project_name: String,
        /// Active project dimensional mode.
        world_mode: WorldMode,
        /// Currently loaded scene display name.
        scene_name: String,
        /// Current scene file path, if saved/loaded from disk.
        scene_path: Option<String>,
        /// Active viewport presentation mode.
        viewport_mode: ViewportMode,
        /// Active viewport camera mode.
        camera_mode: ViewportCameraMode,
    },
    /// Full hierarchy tree snapshot of the current scene.
    HierarchySnapshot {
        /// Root nodes of the scene tree.
        roots: Vec<SceneTreeNode>,
    },
    /// Notification that editor selection changed.
    SelectionChanged {
        /// Currently selected entity GUIDs.
        guids: Vec<EntityGuid>,
    },
    /// Notification that viewport transport or camera state changed.
    ViewportStateChanged {
        /// Current viewport presentation mode.
        viewport_mode: ViewportMode,
        /// Current viewport camera mode.
        camera_mode: ViewportCameraMode,
        /// Current render width in pixels.
        width: u32,
        /// Current render height in pixels.
        height: u32,
    },
    /// Notification that transform gizmo mode, space, or snap settings changed.
    GizmoStateChanged {
        /// Active gizmo tool (`translate`, `rotate`, or `scale`).
        mode: GizmoMode,
        /// Active coordinate space (`world` or `local`).
        space: GizmoSpace,
        /// Active snapping configuration.
        snap: GizmoSnapConfig,
    },
    /// Hints the UI Inspector to refresh component values for the listed entities.
    InspectorRefresh {
        /// Persistent GUIDs of entities whose components changed in the runtime.
        guids: Vec<EntityGuid>,
    },
    /// Notification that Undo/Redo stack availability or labels changed.
    HistoryStateChanged {
        /// Updated history stack summary.
        state: HistoryStatePayload,
    },
    /// Notification that the project `AssetRegistry` or cached thumbnails changed.
    AssetsChanged {
        /// Monotonic revision counter bumped after rescans, moves, imports, or thumbnail generation.
        revision: u64,
    },
    /// Log entry emitted by the runtime.
    Log {
        /// Structured log entry.
        entry: EditorLogEntry,
    },
    /// Periodic performance metrics snapshot.
    Metrics {
        /// Latest frame and entity metrics.
        metrics: FrameMetrics,
    },
    /// Heartbeat reply matching [`ClientIpcMessage::Ping`].
    Pong {
        /// Echoed nonce token.
        nonce: u64,
    },
    /// Error notification for a malformed or failed client IPC message.
    Error {
        /// Human-readable error explanation.
        message: String,
    },
}
