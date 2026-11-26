//! Custom Bevy Remote Protocol (BRP) method names and request/response payloads.

use engine_core::{EntityGuid, WorldMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ipc::{HistoryStatePayload, SceneTreeNode},
    viewport::ViewportMode,
};

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

/// BRP method name for undoing the latest editor transaction.
pub const METHOD_HISTORY_UNDO: &str = "engine.history.undo";

/// BRP method name for redoing the most recently undone editor transaction.
pub const METHOD_HISTORY_REDO: &str = "engine.history.redo";

/// BRP method name for querying the current Undo/Redo stack status.
pub const METHOD_HISTORY_STATE: &str = "engine.history.state";

/// BRP method name for transactionally deleting an entity and its descendant subtree.
pub const METHOD_ENTITY_DELETE: &str = "engine.entity.delete";

/// BRP method name for transactionally reparenting an entity under a new parent or scene root.
pub const METHOD_ENTITY_REPARENT: &str = "engine.entity.reparent";

/// BRP method name for transactionally setting a component value on an entity (with optional coalescing).
pub const METHOD_COMPONENT_SET: &str = "engine.component.set";

/// BRP method name for transactionally adding a component to an entity.
pub const METHOD_COMPONENT_ADD: &str = "engine.component.add";

/// BRP method name for transactionally removing a component from an entity.
pub const METHOD_COMPONENT_REMOVE: &str = "engine.component.remove";

/// BRP method name for saving a scene subtree as a reusable `.prefab.ron` asset.
pub const METHOD_PREFAB_SAVE: &str = "engine.prefab.save";

/// BRP method name for instantiating a `.prefab.ron` asset into the active scene.
pub const METHOD_PREFAB_SPAWN: &str = "engine.prefab.spawn";

/// BRP method name for applying local changes on a prefab instance back to its `.prefab.ron` asset and sibling instances.
pub const METHOD_PREFAB_APPLY: &str = "engine.prefab.apply";

/// BRP method name for reverting overrides on a prefab instance back to the `.prefab.ron` template.
pub const METHOD_PREFAB_REVERT: &str = "engine.prefab.revert";

/// BRP method name for listing `.prefab.ron` files discovered inside the active project.
pub const METHOD_PREFAB_LIST: &str = "engine.prefab.list";

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

/// Response returned by [`METHOD_HISTORY_UNDO`] and [`METHOD_HISTORY_REDO`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryStepResult {
    /// Whether an undo or redo command was executed.
    pub applied: bool,
    /// Human-readable label of the executed command, if [`Self::applied`] is `true`.
    pub label: Option<String>,
    /// Resulting Undo/Redo stack summary after the step.
    pub state: HistoryStatePayload,
}

/// Parameters for [`METHOD_ENTITY_DELETE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteEntityParams {
    /// Persistent [`EntityGuid`] of the entity subtree root to delete.
    pub guid: EntityGuid,
}

/// Response returned by [`METHOD_ENTITY_DELETE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteEntityResult {
    /// Persistent [`EntityGuid`]s of the deleted root and all despawned descendants.
    pub deleted_guids: Vec<EntityGuid>,
}

/// Parameters for [`METHOD_ENTITY_REPARENT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReparentEntityParams {
    /// Persistent [`EntityGuid`] of the child entity to reparent.
    pub guid: EntityGuid,
    /// New parent [`EntityGuid`], or `None` to move to the scene root.
    pub parent_guid: Option<EntityGuid>,
}

/// Response returned by [`METHOD_ENTITY_REPARENT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReparentEntityResult {
    /// Target entity [`EntityGuid`].
    pub guid: EntityGuid,
    /// Resulting parent [`EntityGuid`], if any.
    pub parent_guid: Option<EntityGuid>,
}

/// Parameters for [`METHOD_COMPONENT_SET`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetComponentParams {
    /// Target entity persistent [`EntityGuid`].
    pub guid: EntityGuid,
    /// Fully-qualified reflected component type path.
    pub type_path: String,
    /// Serialized component JSON value matching BRP format.
    pub value: Value,
    /// Optional coalescing key to merge continuous edits within the merge window.
    #[serde(default)]
    pub merge_key: Option<String>,
    /// Optional human-readable command label override for the Undo history stack.
    #[serde(default)]
    pub label: Option<String>,
}

/// Parameters for [`METHOD_COMPONENT_ADD`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddComponentParams {
    /// Target entity persistent [`EntityGuid`].
    pub guid: EntityGuid,
    /// Fully-qualified reflected component type path to insert.
    pub type_path: String,
    /// Initial serialized component JSON value matching BRP format.
    pub value: Value,
}

/// Parameters for [`METHOD_COMPONENT_REMOVE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveComponentParams {
    /// Target entity persistent [`EntityGuid`].
    pub guid: EntityGuid,
    /// Fully-qualified reflected component type path to remove.
    pub type_path: String,
}

/// Response returned by [`METHOD_COMPONENT_SET`], [`METHOD_COMPONENT_ADD`], and [`METHOD_COMPONENT_REMOVE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentMutationResult {
    /// Target entity persistent [`EntityGuid`].
    pub guid: EntityGuid,
    /// Component type path that was mutated.
    pub type_path: String,
}

/// Parameters for [`METHOD_PREFAB_SAVE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefabSaveParams {
    /// Persistent [`EntityGuid`] of the scene entity subtree root to export.
    pub root_guid: EntityGuid,
    /// Target `.prefab.ron` path (relative to the project root or absolute).
    pub path: String,
    /// Optional human-readable name header stored in the `.prefab.ron` file.
    #[serde(default)]
    pub name: Option<String>,
}

/// Response returned by [`METHOD_PREFAB_SAVE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefabSaveResult {
    /// Normalized prefab path assigned on the linked `PrefabInstance` component.
    pub prefab_path: String,
    /// Template root [`EntityGuid`] recorded inside the `.prefab.ron` asset.
    pub root_source_guid: EntityGuid,
    /// Total number of entities serialized into the prefab subtree.
    pub entity_count: usize,
}

/// Parameters for [`METHOD_PREFAB_SPAWN`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrefabSpawnParams {
    /// Path to the `.prefab.ron` file (relative to project root or absolute).
    pub path: String,
    /// Optional parent [`EntityGuid`] to attach the spawned prefab instance under.
    #[serde(default)]
    pub parent_guid: Option<EntityGuid>,
    /// Optional world/local spawn position `[x, y, z]`.
    #[serde(default)]
    pub position: Option<[f32; 3]>,
}

/// Response returned by [`METHOD_PREFAB_SPAWN`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefabSpawnResult {
    /// Persistent [`EntityGuid`] assigned to the newly spawned prefab instance root.
    pub guid: EntityGuid,
    /// Ephemeral ECS [`bevy_ecs::entity::Entity::to_bits`] handle of the spawned root.
    pub entity_bits: u64,
    /// Display name of the spawned prefab root.
    pub name: String,
    /// Normalized `.prefab.ron` path stored in `PrefabInstance`.
    pub prefab_path: String,
}

/// Parameters for [`METHOD_PREFAB_APPLY`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefabInstanceActionParams {
    /// Persistent [`EntityGuid`] of the prefab root or any member entity in that instance.
    pub guid: EntityGuid,
}

/// Response returned by [`METHOD_PREFAB_APPLY`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefabApplyResult {
    /// Normalized `.prefab.ron` path updated on disk.
    pub prefab_path: String,
    /// Persistent [`EntityGuid`]s across all scene entities updated by apply/propagation.
    pub updated_guids: Vec<EntityGuid>,
}

/// Parameters for [`METHOD_PREFAB_REVERT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefabRevertParams {
    /// Persistent [`EntityGuid`] of the prefab root or any member entity in that instance.
    pub guid: EntityGuid,
    /// Optional reflected component type path to revert exclusively on `guid`; if omitted, reverts the whole instance.
    #[serde(default)]
    pub type_path: Option<String>,
}

/// Response returned by [`METHOD_PREFAB_REVERT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefabRevertResult {
    /// Normalized `.prefab.ron` path referenced by the reverted instance.
    pub prefab_path: String,
    /// Persistent [`EntityGuid`]s of entities whose components were reverted.
    pub reverted_guids: Vec<EntityGuid>,
}

/// Metadata entry describing a `.prefab.ron` file discovered in the active project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefabFileEntry {
    /// Display filename stem without `.prefab.ron`.
    pub name: String,
    /// Path relative to the project root with `/` separators.
    pub relative_path: String,
    /// Resolved absolute path on disk.
    pub absolute_path: String,
}

/// Response returned by [`METHOD_PREFAB_LIST`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefabListResult {
    /// Sorted list of discovered project `.prefab.ron` assets.
    pub prefabs: Vec<PrefabFileEntry>,
}
