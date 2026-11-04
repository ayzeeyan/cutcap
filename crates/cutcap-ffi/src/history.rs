//! Bounded clip-edit history owned by the project command owner.
//!
//! Each entry stores the exact clip value and its position in the project's
//! clip order, so undo/redo restore identity, timing and order exactly. Only
//! successful clip edits are recorded; selection, playback, probing and failed
//! commands never are. A new edit discards the redo branch.
//!
//! Document revisions: an edit moves the document from `before` to `after`.
//! Undo returns to `before` only if the document is still exactly at `after`
//! (nothing else changed since); otherwise it takes a fresh revision. So
//! Save → Insert → Undo is clean again, while Save → Insert → Add track → Undo
//! stays dirty. Revisions are unique per session (`next` only grows), so a
//! restored revision can never be confused with a different state.
use cutcap_contracts::{ClipId, HistoryStatus, SelectionState};
use cutcap_core::{Clip, Project, ProjectError};
use std::collections::VecDeque;

/// Each entry holds one clip (a few hundred bytes), so the bound is small and fixed.
pub const MAX_HISTORY: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Insert,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub kind: EditKind,
    /// Position of the clip in the project's clip order when it exists.
    pub index: usize,
    pub clip: Clip,
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
        let result = match edit.kind {
            EditKind::Insert => remove(project, edit.clip.id, selection),
            EditKind::Remove => restore(project, &edit),
        };
        match result {
            Ok(selection) => {
                if *revision == edit.after {
                    *revision = edit.before;
                } else {
                    bump(next, revision);
                }
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
        let result = match edit.kind {
            EditKind::Insert => restore(project, &edit),
            EditKind::Remove => remove(project, edit.clip.id, selection),
        };
        match result {
            Ok(selection) => {
                if *revision == edit.before {
                    *revision = edit.after;
                } else {
                    bump(next, revision);
                }
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

fn remove(
    project: &mut Project,
    id: ClipId,
    selection: SelectionState,
) -> Result<SelectionState, ProjectError> {
    project.remove_clip(id)?;
    Ok(if selection == SelectionState::Clip(id) {
        SelectionState::None
    } else {
        selection
    })
}

fn restore(project: &mut Project, edit: &Edit) -> Result<SelectionState, ProjectError> {
    project.insert_clip_at(edit.index, edit.clip.clone())?;
    Ok(SelectionState::Clip(edit.clip.id))
}
