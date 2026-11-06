//! Bounded clip-edit history owned by the project command owner.
//!
//! Each entry is one complete user gesture — an insertion, removal, move,
//! trim or split — stored as an exact [`ClipChange`]: every clip value it
//! removed and inserted, with their positions in the project's clip order. So
//! one undo restores identity, timing, source mapping and order exactly, and
//! one redo reproduces the same identities. Only successful edits are
//! recorded; selection, playback, probing and failed commands never are. A new
//! edit discards the redo branch.
//!
//! Document revisions: an edit moves the document from `before` to `after`.
//! Undo returns to `before` only if the document is still exactly at `after`
//! (nothing else changed since); otherwise it takes a fresh revision. So
//! Save → Insert → Undo is clean again, while Save → Insert → Add track → Undo
//! stays dirty. Revisions are unique per session (`next` only grows), so a
//! restored revision can never be confused with a different state.
use cutcap_contracts::{HistoryStatus, SelectionState};
use cutcap_core::{ClipChange, Project, ProjectError};
use std::collections::VecDeque;

/// Entries hold a few clip values each (a split holds three), so the bound is
/// small and fixed regardless of project size.
pub const MAX_HISTORY: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub change: ClipChange,
    pub before: u64,
    pub after: u64,
    pub label: String,
}

#[derive(Debug, Default)]
pub struct History {
    undo: VecDeque<Edit>,
    redo: Vec<Edit>,
}

/// Allocates a fresh, never-before-used document revision.
pub fn bump(next: &mut u64, revision: &mut u64) -> u64 {
    *next = next.saturating_add(1).max(revision.saturating_add(1));
    *revision = *next;
    *revision
}

impl History {
    pub fn record(&mut self, edit: Edit) {
        self.redo.clear();
        if self.undo.len() == MAX_HISTORY {
            self.undo.pop_front();
        }
        self.undo.push_back(edit);
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    pub fn status(&self) -> HistoryStatus {
        HistoryStatus {
            can_undo: !self.undo.is_empty(),
            can_redo: !self.redo.is_empty(),
            undo: self.undo.back().map(|edit| edit.label.clone()),
            redo: self.redo.last().map(|edit| edit.label.clone()),
            depth: self.undo.len(),
        }
    }

    /// Reverses the newest edit. Returns the selection that should follow, or
    /// `None` when there is nothing to undo. A failed inverse keeps the entry.
    pub fn undo(
        &mut self,
        project: &mut Project,
        revision: &mut u64,
        next: &mut u64,
        selection: SelectionState,
    ) -> Result<Option<SelectionState>, ProjectError> {
        let Some(edit) = self.undo.pop_back() else {
            return Ok(None);
        };
        match project.revert_change(&edit.change) {
            Ok(()) => {
                if *revision == edit.after {
                    *revision = edit.before;
                } else {
                    bump(next, revision);
                }
                let selection = follow(selection, &edit.change, true);
                self.redo.push(edit);
                Ok(Some(selection))
            }
            Err(error) => {
                self.undo.push_back(edit);
                Err(error)
            }
        }
    }

    /// Re-applies the most recently undone edit.
    pub fn redo(
        &mut self,
        project: &mut Project,
        revision: &mut u64,
        next: &mut u64,
        selection: SelectionState,
    ) -> Result<Option<SelectionState>, ProjectError> {
        let Some(edit) = self.redo.pop() else {
            return Ok(None);
        };
        match project.apply_change(&edit.change) {
            Ok(()) => {
                if *revision == edit.before {
                    *revision = edit.after;
                } else {
                    bump(next, revision);
                }
                let selection = follow(selection, &edit.change, false);
                if self.undo.len() == MAX_HISTORY {
                    self.undo.pop_front();
                }
                self.undo.push_back(edit);
                Ok(Some(selection))
            }
            Err(error) => {
                self.redo.push(edit);
                Err(error)
            }
        }
    }
}

/// Selection after undo (`reverting`) or redo: the first clip that reappears
/// is selected; otherwise the selection is kept unless it named a clip that
/// just disappeared.
fn follow(selection: SelectionState, change: &ClipChange, reverting: bool) -> SelectionState {
    let (gone, back) = if reverting {
        (&change.inserted, &change.removed)
    } else {
        (&change.removed, &change.inserted)
    };
    if let Some((_, clip)) = back.first() {
        return SelectionState::Clip(clip.id);
    }
    match selection {
        SelectionState::Clip(id) if gone.iter().any(|(_, c)| c.id == id) => SelectionState::None,
        other => other,
    }
}
