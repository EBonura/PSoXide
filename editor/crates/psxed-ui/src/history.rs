//! Snapshot-based undo / redo for the editor workspace.
//!
//! Each entry is a full [`ProjectDocument`] clone -- for
//! hand-authored level data the snapshots are cheap and avoid
//! the command-pattern bookkeeping that operation-based undo
//! demands. Capacity is bounded so a long edit session can't
//! grow the history without limit.

use std::collections::VecDeque;

use psxed_project::ProjectDocument;

/// Maximum number of snapshots retained on either the undo or
/// the redo stack. Hitting the cap drops the oldest entry --
/// matches IDE-style "you can always undo a few steps but the
/// stack stays bounded" behaviour.
pub const UNDO_CAPACITY: usize = 64;

/// Snapshot-based undo / redo timeline. The editor pushes a
/// `ProjectDocument` clone onto [`UndoStack::record`] before
/// every mutating action; [`UndoStack::undo`] /
/// [`UndoStack::redo`] swap the current document with the
/// adjacent entry in one chronological queue.
#[derive(Default)]
pub(crate) struct UndoStack {
    timeline: VecDeque<ProjectDocument>,
    cursor: usize,
}

impl UndoStack {
    /// Push the *pre-mutation* `snapshot` onto the history timeline
    /// and clear future entries -- any new edit forks history.
    pub(crate) fn record(&mut self, snapshot: ProjectDocument) -> bool {
        if self.cursor < self.timeline.len() {
            self.timeline.truncate(self.cursor);
        }
        if self.timeline.back() == Some(&snapshot) {
            return false;
        }
        if self.timeline.len() == UNDO_CAPACITY {
            self.timeline.pop_front();
            self.cursor = self.cursor.saturating_sub(1);
        }
        self.timeline.push_back(snapshot);
        self.cursor = self.timeline.len();
        true
    }

    /// Drop all undo / redo entries. Used after filesystem-backed
    /// operations because snapshots only capture project metadata,
    /// not file moves that have already happened on disk.
    pub(crate) fn clear(&mut self) {
        self.timeline.clear();
        self.cursor = 0;
    }

    /// Move the cursor one visible step backward. The live
    /// `current` document is inserted where the restored state came
    /// from, so redo remains the exact inverse operation.
    pub(crate) fn undo(&mut self, mut current: ProjectDocument) -> Option<ProjectDocument> {
        while self.cursor > 0 {
            let index = self.cursor - 1;
            let prev = self.timeline.remove(index)?;
            self.timeline.insert(index, current.clone());
            self.cursor = index;
            if prev != current {
                return Some(prev);
            }
            current = prev;
        }
        None
    }

    /// Inverse of [`Self::undo`]: move one visible step forward in
    /// history if the user previously undid something.
    pub(crate) fn redo(&mut self, mut current: ProjectDocument) -> Option<ProjectDocument> {
        while self.cursor < self.timeline.len() {
            let index = self.cursor;
            let next = self.timeline.remove(index)?;
            self.timeline.insert(index, current.clone());
            self.cursor = index + 1;
            if next != current {
                return Some(next);
            }
            current = next;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psxed_project::ProjectDocument;

    fn doc(name: &str) -> ProjectDocument {
        ProjectDocument::new(name)
    }

    #[test]
    fn undo_returns_previous_snapshot_and_pushes_current_to_redo() {
        let mut stack = UndoStack::default();
        assert!(stack.record(doc("v1")));
        let restored = stack.undo(doc("v2")).expect("undo entry exists");
        assert_eq!(restored.name, "v1");
        // Live state moved onto the future side of the timeline --
        // redo should hand it back.
        let redone = stack.redo(doc("v1")).expect("redo entry exists");
        assert_eq!(redone.name, "v2");
    }

    #[test]
    fn record_clears_redo_history() {
        let mut stack = UndoStack::default();
        stack.record(doc("v1"));
        let _ = stack.undo(doc("v2"));
        // A new edit *after* an undo forks history -- redo
        // should yield nothing.
        stack.record(doc("v2'"));
        assert!(stack.redo(doc("live")).is_none());
    }

    #[test]
    fn capacity_drops_oldest_undo_entry() {
        let mut stack = UndoStack::default();
        for i in 0..(UNDO_CAPACITY + 5) {
            stack.record(doc(&format!("v{i}")));
        }
        // Drain the stack -- the first entry should be
        // `v5` (oldest 5 dropped).
        let mut last = None;
        while let Some(prev) = stack.undo(doc("live")) {
            last = Some(prev.name);
        }
        assert_eq!(last.as_deref(), Some("v5"));
    }

    #[test]
    fn redo_advances_only_one_visible_state() {
        let mut stack = UndoStack::default();
        stack.record(doc("v1"));
        stack.record(doc("v2"));
        let restored = stack.undo(doc("v3")).expect("first undo");
        assert_eq!(restored.name, "v2");
        let restored = stack.undo(doc("v2")).expect("second undo");
        assert_eq!(restored.name, "v1");

        let redone = stack.redo(doc("v1")).expect("first redo");
        assert_eq!(redone.name, "v2");
        let redone = stack.redo(doc("v2")).expect("second redo");
        assert_eq!(redone.name, "v3");
    }

    #[test]
    fn duplicate_snapshots_are_not_recorded() {
        let mut stack = UndoStack::default();
        assert!(stack.record(doc("v1")));
        assert!(!stack.record(doc("v1")));
        assert!(stack.undo(doc("v2")).is_some());
        assert!(stack.undo(doc("v1")).is_none());
    }

    #[test]
    fn noop_entries_do_not_become_visible_undo_steps() {
        let mut stack = UndoStack::default();
        stack.record(doc("v1"));
        stack.record(doc("v2"));

        let restored = stack.undo(doc("v2")).expect("visible undo");
        assert_eq!(restored.name, "v1");
        assert!(stack.undo(doc("v1")).is_none());

        let redone = stack.redo(doc("v1")).expect("visible redo");
        assert_eq!(redone.name, "v2");
        assert!(stack.redo(doc("v2")).is_none());
    }
}
