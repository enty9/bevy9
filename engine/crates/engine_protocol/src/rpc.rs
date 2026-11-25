//! Custom Bevy Remote Protocol (BRP) method names and request/response payloads.

use engine_core::{EntityGuid, WorldMode};
use serde::{Deserialize, Serialize};

use crate::{ipc::SceneTreeNode, viewport::ViewportMode};

/// BRP method name for creating a new empty scene.
pub const METHOD_SCENE_NEW: &str = "engine.scene.new";

/// BRP method name for loading a `.level.ron` scene from disk.
pub const METHOD_SCENE_LOAD: &str = "engine.scene.load";

/// BRP method name for saving the active scene to a `.level.ron` file.
pub const METHOD_SCENE_SAVE: &str = "engine.scene.save";

/// BRP method name for querying the hierarchical scene tree.
pub const METHOD_WORLD_TREE: &str = "engine.world.tree";

/// BRP method name for spawning a built-in 2D/3D entity preset.
pub const METHOD_ENTITY_SPAWN_PRESET: &str = "engine.entity.spawn_preset";

/// BRP method name for switching viewport mode (`offscreen` / `native_window`).
pub const METHOD_VIEWPORT_SET_MODE: &str = "engine.viewport.set_mode";

/// Parameters for [`METHOD_SCENE_NEW`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneNewParams {
    /// Human-readable name for the new scene.
    pub name: String,
    /// Dimensional mode (`2d`, `3d`, or `hybrid`) for the scene.
    pub world_mode: WorldMode,
}

/// Parameters for [`METHOD_SCENE_LOAD`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneLoadParams {
    /// Path to the `.level.ron` file (absolute or relative to the active project root).
    pub path: String,
}

/// Parameters for [`METHOD_SCENE_SAVE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneSaveParams {
    /// Optional target `.level.ron` path; if omitted, uses the active scene path.
    pub path: Option<String>,
    /// Optional scene name override written into the saved header.
    pub name: Option<String>,
}

/// Response returned by [`METHOD_SCENE_NEW`], [`METHOD_SCENE_LOAD`], and [`METHOD_SCENE_SAVE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneOperationResult {
    /// Active scene name after the operation.
    pub name: String,
    /// Resolved scene path on disk, if any.
    pub path: Option<String>,
    /// Active [`WorldMode`] of the scene.
    pub world_mode: WorldMode,
    /// Number of scene entities present after the operation.
    pub entity_count: usize,
}

/// Response returned by [`METHOD_WORLD_TREE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldTreeResult {
    /// Active scene name.
    pub scene_name: String,
    /// Active scene file path, if any.
    pub scene_path: Option<String>,
    /// Active [`WorldMode`].
    pub world_mode: WorldMode,
    /// Root scene entities with recursive children.
    pub roots: Vec<SceneTreeNode>,
}

/// Built-in entity presets spawnable from the editor Outliner or BRP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityPresetKind {
    /// Empty transform-only entity.
    Empty,
    /// 3D unit cube mesh with standard PBR material.
    Cube3d,
    /// 3D UV sphere mesh with standard PBR material.
    Sphere3d,
    /// 3D ground plane mesh with standard PBR material.
    Plane3d,
    /// 3D directional light source.
    DirectionalLight,
    /// 3D point light source.
    PointLight,
    /// 3D perspective camera entity.
    Camera3d,
    /// 2D sprite entity.
    Sprite2d,
    /// 2D colored rectangle mesh.
    Quad2d,
    /// 2D colored circle mesh.
    Circle2d,
    /// 2D orthographic camera entity.
    Camera2d,
}

/// Parameters for [`METHOD_ENTITY_SPAWN_PRESET`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpawnPresetParams {
    /// Preset type to spawn.
    pub preset: EntityPresetKind,
    /// Optional display name override.
    pub name: Option<String>,
    /// Optional parent [`EntityGuid`] to attach under.
    pub parent_guid: Option<EntityGuid>,
    /// Optional initial world/local translation `[x, y, z]`.
    pub position: Option<[f32; 3]>,
}

/// Response returned by [`METHOD_ENTITY_SPAWN_PRESET`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnPresetResult {
    /// Newly assigned persistent [`EntityGuid`].
    pub guid: EntityGuid,
    /// Ephemeral ECS [`bevy_ecs::entity::Entity::to_bits`] identifier.
    pub entity_bits: u64,
    /// Resolved display name of the spawned entity.
    pub name: String,
}

/// Parameters for [`METHOD_VIEWPORT_SET_MODE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetViewportModeParams {
    /// Desired viewport presentation mode.
    pub mode: ViewportMode,
}

/// Response returned by [`METHOD_VIEWPORT_SET_MODE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetViewportModeResult {
    /// Resulting active viewport presentation mode.
    pub mode: ViewportMode,
}
