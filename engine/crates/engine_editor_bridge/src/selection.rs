//! Shared editor selection state and IPC synchronization flags.

use std::collections::HashSet;

use bevy_ecs::resource::Resource;
use engine_core::EntityGuid;

/// Active entity selection synchronized between the Outliner, Viewport, and Engine Host.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct EditorSelection {
    guids: Vec<EntityGuid>,
}

impl EditorSelection {
    /// Replaces the current selection with `guids`, preserving order and removing duplicates.
    ///
    /// Returns `true` if the resulting selection changed.
    pub fn set(&mut self, guids: Vec<EntityGuid>) -> bool {
        let mut seen = HashSet::with_capacity(guids.len());
        let mut deduped = Vec::with_capacity(guids.len());
        for guid in guids {
            if seen.insert(guid) {
                deduped.push(guid);
            }
        }
        if self.guids == deduped {
            false
        } else {
            self.guids = deduped;
            true
        }
    }

    /// Clears the active selection, returning `true` if any entities were selected previously.
    pub fn clear(&mut self) -> bool {
        if self.guids.is_empty() {
            false
        } else {
            self.guids.clear();
            true
        }
    }

    /// Returns the currently selected [`EntityGuid`] slice.
    #[must_use]
    pub fn guids(&self) -> &[EntityGuid] {
        &self.guids
    }
}

/// Dirty flags indicating which editor snapshots should be broadcast to connected `WebSocket` clients.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct BridgeDirtyFlags {
    /// Triggers a `HierarchySnapshot` broadcast on the next sync pass.
    pub hierarchy_dirty: bool,
    /// Triggers a `ViewportStateChanged` broadcast on the next sync pass.
    pub viewport_dirty: bool,
    /// Triggers a `SelectionChanged` broadcast on the next sync pass.
    pub selection_dirty: bool,
}
