//! Transactional Undo/Redo command history, coalescing, and native shortcut handling.

use std::{
    collections::{HashSet, VecDeque},
    time::{SystemTime, UNIX_EPOCH},
};

use bevy_ecs::{entity::Entity, hierarchy::ChildOf, resource::Resource, world::World};
use bevy_input::{keyboard::KeyCode, ButtonInput};
use bevy_transform::components::Transform;
use engine_core::{EntityGuid, EntityGuidMap};
use engine_protocol::{HistoryStatePayload, HistoryStepResult, ViewportMode};
use engine_render::{gizmo::TransformGizmoState, ViewportConfig};
use engine_scene::{
    despawn_subtree, remove_reflected_component, respawn_subtree_from_snapshot,
    write_reflected_component_json, ActiveScene, SubtreeSnapshot,
};
use serde_json::Value;

use crate::selection::{BridgeDirtyFlags, EditorSelection};

/// Maximum number of undo transactions retained in memory per scene session.
pub const MAX_HISTORY_ENTRIES: usize = 200;

/// Maximum elapsed milliseconds within which rapid component edits sharing a `merge_key` coalesce.
pub const MERGE_WINDOW_MS: u128 = 1200;

/// Local transform diff entry captured for a single entity in a gizmo transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct TransformChangeEntry {
    /// Persistent [`EntityGuid`] of the transformed entity.
    pub guid: EntityGuid,
    /// Local [`Transform`] prior to the drag gesture.
    pub before: Transform,
    /// Local [`Transform`] after the drag gesture.
    pub after: Transform,
}

/// Reversible editor command recorded in [`EditorHistory`].
#[derive(Debug, Clone, PartialEq)]
pub enum EditorCommand {
    /// Reversible entity/subtree creation (spawning a preset or prefab).
    CreateEntity {
        /// Serialized snapshot of the created subtree.
        snapshot: SubtreeSnapshot,
        /// Human-readable action description.
        label: String,
    },
    /// Reversible entity/subtree deletion preserving full subtree state and selection.
    DeleteEntity {
        /// Serialized snapshot of the deleted subtree.
        snapshot: SubtreeSnapshot,
        /// Editor selection before deletion (restored on Undo).
        previous_selection: Vec<EntityGuid>,
        /// Human-readable action description.
        label: String,
    },
    /// Reversible hierarchy reparenting.
    Reparent {
        /// Target entity GUID.
        guid: EntityGuid,
        /// Parent GUID prior to reparenting (`None` = scene root).
        old_parent: Option<EntityGuid>,
        /// Parent GUID after reparenting (`None` = scene root).
        new_parent: Option<EntityGuid>,
    },
    /// Reversible component update (eligible for `merge_key` coalescing).
    SetComponent {
        /// Target entity GUID.
        guid: EntityGuid,
        /// Fully-qualified reflected component type path.
        type_path: String,
        /// JSON value before mutation.
        before: Value,
        /// JSON value after mutation.
        after: Value,
        /// Human-readable action description.
        label: String,
    },
    /// Reversible component insertion.
    AddComponent {
        /// Target entity GUID.
        guid: EntityGuid,
        /// Fully-qualified reflected component type path.
        type_path: String,
        /// Inserted JSON component payload.
        value: Value,
    },
    /// Reversible component removal.
    RemoveComponent {
        /// Target entity GUID.
        guid: EntityGuid,
        /// Fully-qualified reflected component type path.
        type_path: String,
        /// Component JSON payload captured prior to removal.
        previous_value: Value,
    },
    /// Reversible batch transform commit produced by one gizmo drag gesture.
    BatchSetTransforms {
        /// All entity transform diffs affected by the drag gesture.
        entries: Vec<TransformChangeEntry>,
        /// Human-readable action description (e.g. `"Move Entity"`).
        label: String,
    },
    /// Reversible full-subtree replacement (used for prefab revert).
    RestoreSubtree {
        /// Subtree snapshot before the operation.
        before: SubtreeSnapshot,
        /// Subtree snapshot after the operation.
        after: SubtreeSnapshot,
        /// Human-readable action description.
        label: String,
    },
}

impl EditorCommand {
    /// Returns a concise human-readable label for UI tooltips.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::CreateEntity { label, .. }
            | Self::DeleteEntity { label, .. }
            | Self::SetComponent { label, .. }
            | Self::BatchSetTransforms { label, .. }
            | Self::RestoreSubtree { label, .. } => label.clone(),
            Self::Reparent { .. } => "Reparent Entity".to_owned(),
            Self::AddComponent { type_path, .. } => {
                format!("Add {}", short_type_name(type_path))
            }
            Self::RemoveComponent { type_path, .. } => {
                format!("Remove {}", short_type_name(type_path))
            }
        }
    }
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    command: EditorCommand,
    merge_key: Option<String>,
    timestamp_ms: u128,
}

/// ECS resource storing the bounded transactional Undo/Redo command stacks.
#[derive(Resource, Debug, Default)]
pub struct EditorHistory {
    undo_stack: VecDeque<HistoryEntry>,
    redo_stack: VecDeque<HistoryEntry>,
    /// Set `true` whenever undo/redo stacks change and clients need a `HistoryStateChanged` push.
    pub dirty: bool,
    /// Entity GUIDs whose inspector values changed due to Undo/Redo or history operations.
    pub pending_inspector_refresh: Vec<EntityGuid>,
}

impl EditorHistory {
    /// Records a newly executed command, clearing redo history and coalescing matching continuous edits.
    pub fn record(
        &mut self,
        command: EditorCommand,
        merge_key: Option<String>,
        now_ms: u128,
    ) {
        self.redo_stack.clear();

        if let Some(ref key) = merge_key
            && let Some(last) = self.undo_stack.back_mut()
            && last.merge_key.as_deref() == Some(key.as_str())
            && now_ms.saturating_sub(last.timestamp_ms) <= MERGE_WINDOW_MS
            && let (
                EditorCommand::SetComponent {
                    guid: prev_guid,
                    type_path: prev_path,
                    after: prev_after,
                    label: prev_label,
                    ..
                },
                EditorCommand::SetComponent {
                    guid: next_guid,
                    type_path: next_path,
                    after: next_after,
                    label: next_label,
                    ..
                },
            ) = (&mut last.command, &command)
            && *prev_guid == *next_guid
            && *prev_path == *next_path
        {
            *prev_after = next_after.clone();
            *prev_label = next_label.clone();
            last.timestamp_ms = now_ms;
            self.dirty = true;
            return;
        }

        self.undo_stack.push_back(HistoryEntry {
            command,
            merge_key,
            timestamp_ms: now_ms,
        });
        while self.undo_stack.len() > MAX_HISTORY_ENTRIES {
            self.undo_stack.pop_front();
        }
        self.dirty = true;
    }

    /// Clears both undo and redo stacks (typically when loading or resetting a scene).
    pub fn clear(&mut self) {
        let had_entries = !self.undo_stack.is_empty() || !self.redo_stack.is_empty();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.pending_inspector_refresh.clear();
        if had_entries {
            self.dirty = true;
        }
    }

    /// Queues `guids` for an upcoming `InspectorRefresh` broadcast (deduplicated).
    pub fn queue_inspector_refresh(&mut self, guids: &[EntityGuid]) {
        for &guid in guids {
            if !self.pending_inspector_refresh.contains(&guid) {
                self.pending_inspector_refresh.push(guid);
            }
        }
    }

    /// Returns a serializable summary of the current Undo/Redo stack state.
    #[must_use]
    pub fn state(&self) -> HistoryStatePayload {
        HistoryStatePayload {
            can_undo: !self.undo_stack.is_empty(),
            can_redo: !self.redo_stack.is_empty(),
            undo_label: self.undo_stack.back().map(|entry| entry.command.label()),
            redo_label: self.redo_stack.back().map(|entry| entry.command.label()),
            undo_len: self.undo_stack.len(),
            redo_len: self.redo_stack.len(),
        }
    }
}

/// Returns current UNIX epoch milliseconds for merge-window timestamping.
#[must_use]
pub fn now_epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// Returns the short type name portion after the final `::`.
#[must_use]
pub fn short_type_name(type_path: &str) -> &str {
    type_path.rsplit("::").next().unwrap_or(type_path)
}

/// Removes any `removed` GUIDs from [`EditorSelection`], returning `true` if selection changed.
pub(crate) fn prune_selection_guids(world: &mut World, removed: &[EntityGuid]) -> bool {
    if removed.is_empty() {
        return false;
    }
    let removed_set: HashSet<EntityGuid> = removed.iter().copied().collect();
    let Some(mut selection) = world.get_resource_mut::<EditorSelection>() else {
        return false;
    };
    let next: Vec<EntityGuid> = selection
        .guids()
        .iter()
        .copied()
        .filter(|guid| !removed_set.contains(guid))
        .collect();
    selection.set(next)
}

/// Validates and applies a reparenting operation by persistent [`EntityGuid`].
pub(crate) fn apply_reparent(
    world: &mut World,
    guid: EntityGuid,
    parent_guid: Option<EntityGuid>,
) -> Result<(), String> {
    let entity = resolve_entity_by_guid(world, guid)?;
    match parent_guid {
        Some(target_parent_guid) => {
            if target_parent_guid == guid {
                return Err("Cannot parent an entity to itself".to_owned());
            }
            let parent_entity = resolve_entity_by_guid(world, target_parent_guid)?;

            let mut cursor = Some(parent_entity);
            let mut visited = HashSet::new();
            while let Some(ancestor) = cursor {
                if ancestor == entity {
                    return Err("Cannot reparent an entity beneath its own descendant".to_owned());
                }
                if !visited.insert(ancestor) {
                    break;
                }
                cursor = world.get::<ChildOf>(ancestor).map(ChildOf::parent);
            }

            world.entity_mut(entity).insert(ChildOf(parent_entity));
        }
        None => {
            world.entity_mut(entity).remove::<ChildOf>();
        }
    }
    Ok(())
}

fn resolve_entity_by_guid(world: &World, guid: EntityGuid) -> Result<Entity, String> {
    let entity = world
        .get_resource::<EntityGuidMap>()
        .and_then(|map| map.get_entity(guid))
        .ok_or_else(|| format!("Entity with GUID {guid} not found"))?;
    if world.get_entity(entity).is_err() {
        return Err(format!("Entity {guid} ({entity:?}) is no longer alive"));
    }
    Ok(entity)
}

struct CommandEffect {
    hierarchy_dirty: bool,
    selection_dirty: bool,
    refresh_guids: Vec<EntityGuid>,
}

/// Undoes the most recent command in [`EditorHistory`].
pub fn undo_last(world: &mut World) -> Result<HistoryStepResult, String> {
    let entry = {
        let Some(mut history) = world.get_resource_mut::<EditorHistory>() else {
            return Err("EditorHistory resource is not initialized".to_owned());
        };
        let Some(entry) = history.undo_stack.pop_back() else {
            return Ok(HistoryStepResult {
                applied: false,
                label: None,
                state: history.state(),
            });
        };
        entry
    };

    let label = entry.command.label();
    let effect = match execute_command(world, &entry.command, true) {
        Ok(effect) => effect,
        Err(err) => {
            if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
                history.undo_stack.push_back(entry);
            }
            return Err(err);
        }
    };

    let state = finalize_history_step(world, Some(entry), true, effect);
    Ok(HistoryStepResult {
        applied: true,
        label: Some(label),
        state,
    })
}

/// Redoes the most recently undone command in [`EditorHistory`].
pub fn redo_last(world: &mut World) -> Result<HistoryStepResult, String> {
    let entry = {
        let Some(mut history) = world.get_resource_mut::<EditorHistory>() else {
            return Err("EditorHistory resource is not initialized".to_owned());
        };
        let Some(entry) = history.redo_stack.pop_back() else {
            return Ok(HistoryStepResult {
                applied: false,
                label: None,
                state: history.state(),
            });
        };
        entry
    };

    let label = entry.command.label();
    let effect = match execute_command(world, &entry.command, false) {
        Ok(effect) => effect,
        Err(err) => {
            if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
                history.redo_stack.push_back(entry);
            }
            return Err(err);
        }
    };

    let state = finalize_history_step(world, Some(entry), false, effect);
    Ok(HistoryStepResult {
        applied: true,
        label: Some(label),
        state,
    })
}

fn finalize_history_step(
    world: &mut World,
    moved_entry: Option<HistoryEntry>,
    push_to_redo: bool,
    effect: CommandEffect,
) -> HistoryStatePayload {
    if let Some(mut active_scene) = world.get_resource_mut::<ActiveScene>() {
        active_scene.dirty = true;
    }

    if let Some(mut dirty) = world.get_resource_mut::<BridgeDirtyFlags>() {
        if effect.hierarchy_dirty {
            dirty.hierarchy_dirty = true;
        }
        if effect.selection_dirty {
            dirty.selection_dirty = true;
        }
    }

    if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
        if let Some(entry) = moved_entry {
            if push_to_redo {
                history.redo_stack.push_back(entry);
            } else {
                history.undo_stack.push_back(entry);
            }
        }
        history.dirty = true;
        history.queue_inspector_refresh(&effect.refresh_guids);
        history.state()
    } else {
        HistoryStatePayload::default()
    }
}

fn execute_command(
    world: &mut World,
    command: &EditorCommand,
    undo: bool,
) -> Result<CommandEffect, String> {
    match command {
        EditorCommand::CreateEntity { snapshot, .. } => {
            if undo {
                let mut selection_dirty = false;
                if let Some(entity) = world
                    .get_resource::<EntityGuidMap>()
                    .and_then(|map| map.get_entity(snapshot.root_guid))
                    && world.get_entity(entity).is_ok()
                {
                    let despawned = despawn_subtree(world, entity);
                    selection_dirty = prune_selection_guids(world, &despawned);
                }
                Ok(CommandEffect {
                    hierarchy_dirty: true,
                    selection_dirty,
                    refresh_guids: Vec::new(),
                })
            } else {
                respawn_subtree_from_snapshot(world, snapshot).map_err(|e| e.to_string())?;
                let selection_dirty = world
                    .get_resource_mut::<EditorSelection>()
                    .is_some_and(|mut selection| selection.set(vec![snapshot.root_guid]));
                Ok(CommandEffect {
                    hierarchy_dirty: true,
                    selection_dirty,
                    refresh_guids: vec![snapshot.root_guid],
                })
            }
        }
        EditorCommand::DeleteEntity {
            snapshot,
            previous_selection,
            ..
        } => {
            if undo {
                respawn_subtree_from_snapshot(world, snapshot).map_err(|e| e.to_string())?;
                let selection_dirty = world
                    .get_resource_mut::<EditorSelection>()
                    .is_some_and(|mut selection| selection.set(previous_selection.clone()));
                Ok(CommandEffect {
                    hierarchy_dirty: true,
                    selection_dirty,
                    refresh_guids: vec![snapshot.root_guid],
                })
            } else {
                let mut selection_dirty = false;
                if let Some(entity) = world
                    .get_resource::<EntityGuidMap>()
                    .and_then(|map| map.get_entity(snapshot.root_guid))
                    && world.get_entity(entity).is_ok()
                {
                    let despawned = despawn_subtree(world, entity);
                    selection_dirty = prune_selection_guids(world, &despawned);
                }
                Ok(CommandEffect {
                    hierarchy_dirty: true,
                    selection_dirty,
                    refresh_guids: Vec::new(),
                })
            }
        }
        EditorCommand::Reparent {
            guid,
            old_parent,
            new_parent,
        } => {
            let target_parent = if undo { *old_parent } else { *new_parent };
            apply_reparent(world, *guid, target_parent)?;
            Ok(CommandEffect {
                hierarchy_dirty: true,
                selection_dirty: false,
                refresh_guids: vec![*guid],
            })
        }
        EditorCommand::SetComponent {
            guid,
            type_path,
            before,
            after,
            ..
        } => {
            let entity = resolve_entity_by_guid(world, *guid)?;
            let target_val = if undo { before } else { after };
            write_reflected_component_json(world, entity, type_path, target_val)
                .map_err(|e| e.to_string())?;
            Ok(CommandEffect {
                hierarchy_dirty: component_affects_tree(type_path),
                selection_dirty: false,
                refresh_guids: vec![*guid],
            })
        }
        EditorCommand::AddComponent {
            guid,
            type_path,
            value,
        } => {
            let entity = resolve_entity_by_guid(world, *guid)?;
            if undo {
                remove_reflected_component(world, entity, type_path).map_err(|e| e.to_string())?;
            } else {
                write_reflected_component_json(world, entity, type_path, value)
                    .map_err(|e| e.to_string())?;
            }
            Ok(CommandEffect {
                hierarchy_dirty: true,
                selection_dirty: false,
                refresh_guids: vec![*guid],
            })
        }
        EditorCommand::RemoveComponent {
            guid,
            type_path,
            previous_value,
        } => {
            let entity = resolve_entity_by_guid(world, *guid)?;
            if undo {
                write_reflected_component_json(world, entity, type_path, previous_value)
                    .map_err(|e| e.to_string())?;
            } else {
                remove_reflected_component(world, entity, type_path).map_err(|e| e.to_string())?;
            }
            Ok(CommandEffect {
                hierarchy_dirty: true,
                selection_dirty: false,
                refresh_guids: vec![*guid],
            })
        }
        EditorCommand::BatchSetTransforms { entries, .. } => {
            let mut refresh_guids = Vec::with_capacity(entries.len());
            for entry in entries {
                let entity = resolve_entity_by_guid(world, entry.guid)?;
                let target_tfm = if undo { entry.before } else { entry.after };
                let Some(mut tfm) = world.get_mut::<Transform>(entity) else {
                    return Err(format!("Entity {} has no Transform component", entry.guid));
                };
                *tfm = target_tfm;
                refresh_guids.push(entry.guid);
            }
            Ok(CommandEffect {
                hierarchy_dirty: false,
                selection_dirty: false,
                refresh_guids,
            })
        }
        EditorCommand::RestoreSubtree { before, after, .. } => {
            let target_snapshot = if undo { before } else { after };
            if let Some(existing) = world
                .get_resource::<EntityGuidMap>()
                .and_then(|map| map.get_entity(target_snapshot.root_guid))
                && world.get_entity(existing).is_ok()
            {
                let _ = despawn_subtree(world, existing);
            }
            respawn_subtree_from_snapshot(world, target_snapshot).map_err(|e| e.to_string())?;
            Ok(CommandEffect {
                hierarchy_dirty: true,
                selection_dirty: false,
                refresh_guids: vec![target_snapshot.root_guid],
            })
        }
    }
}

fn component_affects_tree(type_path: &str) -> bool {
    matches!(
        type_path,
        "bevy_ecs::name::Name" | "bevy_camera::visibility::Visibility"
    )
}

/// Drains completed gizmo drag transactions into [`EditorHistory`] as single undoable commands.
pub(crate) fn capture_gizmo_commits_system(world: &mut World) {
    let commits = world
        .get_resource_mut::<TransformGizmoState>()
        .map(|mut gizmo| gizmo.drain_commits())
        .unwrap_or_default();
    if commits.is_empty() {
        return;
    }

    let now_ms = now_epoch_ms();
    let mut any_changed = false;
    for commit in commits {
        if commit.entries.is_empty() {
            continue;
        }
        any_changed = true;
        let count = commit.entries.len();
        let verb = match commit.label.as_str() {
            "Translate" => "Move",
            "Rotate" => "Rotate",
            "Scale" => "Scale",
            other => other,
        };
        let label = if count == 1 {
            format!("{verb} Entity")
        } else {
            format!("{verb} {count} Entities")
        };

        let entries = commit
            .entries
            .into_iter()
            .map(|(guid, before, after)| TransformChangeEntry { guid, before, after })
            .collect();

        if let Some(mut history) = world.get_resource_mut::<EditorHistory>() {
            history.record(
                EditorCommand::BatchSetTransforms { entries, label },
                None,
                now_ms,
            );
        }
    }

    if any_changed && let Some(mut active_scene) = world.get_resource_mut::<ActiveScene>() {
        active_scene.dirty = true;
    }
}

/// Handles `Ctrl+Z` / `Ctrl+Shift+Z` / `Ctrl+Y` inside a detached native Bevy OS window.
pub(crate) fn handle_native_history_shortcuts_system(world: &mut World) {
    let is_native_window = world
        .get_resource::<ViewportConfig>()
        .is_some_and(|cfg| cfg.mode == ViewportMode::NativeWindow);
    if !is_native_window {
        return;
    }

    let is_dragging = world
        .get_resource::<TransformGizmoState>()
        .is_some_and(TransformGizmoState::is_dragging);
    if is_dragging {
        return;
    }

    let action = world.get_resource::<ButtonInput<KeyCode>>().and_then(|keys| {
        let ctrl_or_meta = keys.pressed(KeyCode::ControlLeft)
            || keys.pressed(KeyCode::ControlRight)
            || keys.pressed(KeyCode::SuperLeft)
            || keys.pressed(KeyCode::SuperRight);
        if !ctrl_or_meta {
            return None;
        }
        let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
        if keys.just_pressed(KeyCode::KeyZ) {
            Some(if shift { "redo" } else { "undo" })
        } else if keys.just_pressed(KeyCode::KeyY) {
            Some("redo")
        } else {
            None
        }
    });

    match action {
        Some("undo") => {
            if let Err(err) = undo_last(world) {
                tracing::warn!(error = %err, "native shortcut Undo failed");
            }
        }
        Some("redo") => {
            if let Err(err) = redo_last(world) {
                tracing::warn!(error = %err, "native shortcut Redo failed");
            }
        }
        _ => {}
    }
}
