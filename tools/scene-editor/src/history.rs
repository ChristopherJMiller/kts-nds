//! Undo/redo (#43). A **snapshot** history over the editable document (the
//! manifest `level` + per-zone `contents`), with per-gesture coalescing.
//!
//! Rather than a command object per mutation — invasive given edits happen in
//! many places (canvas drags, panel `DragValue`s, colour pickers, add/remove
//! buttons) — the editor keeps a `baseline` clone of the last *settled*
//! document. Every frame [`EditorApp::commit_if_settled`] checks: if the live
//! document differs from the baseline **and** no interaction is in flight
//! (pointer up, no widget focused), the baseline is pushed onto the undo stack
//! and refreshed. A whole drag or number scrub is therefore one undo step,
//! since the document only "settles" once the pointer releases.
//!
//! Selection (`sel`/`active`) rides along in each snapshot so undoing a delete
//! restores what was selected, but it is **not** part of the diff — clicking a
//! different instance must never create an undo step.

use std::collections::BTreeMap;

use eframe::egui;
use scene2bin::{Level, Zone};

use crate::app::{EditorApp, Sel};

/// A full editable-document snapshot plus the selection context to restore.
#[derive(Clone)]
pub(crate) struct Snapshot {
    level: Level,
    contents: BTreeMap<String, Zone>,
    active: Option<String>,
    sel: Sel,
}

/// The undo/redo stacks plus the last-settled `baseline`.
#[derive(Default)]
pub(crate) struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The document as of the last settled frame; `None` until first seeded.
    baseline: Option<Snapshot>,
}

/// Cap the stacks so a long editing session can't grow memory without bound.
const MAX_DEPTH: usize = 128;

impl EditorApp {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            level: self.level.clone(),
            contents: self.contents.clone(),
            active: self.active.clone(),
            sel: self.sel.clone(),
        }
    }

    /// Restore a snapshot into the live editor, clamping a now-stale selection.
    fn restore(&mut self, s: Snapshot) {
        self.level = s.level;
        self.contents = s.contents;
        self.active = s.active;
        self.sel = s.sel;
        self.clamp_selection();
    }

    /// Whether the live document (manifest + contents) differs from `base`.
    /// Selection is deliberately excluded — it isn't an edit.
    fn doc_differs(&self, base: &Snapshot) -> bool {
        self.level != base.level || self.contents != base.contents
    }

    /// Drop selection entries that no longer point at a live instance.
    fn clamp_selection(&mut self) {
        let n = self
            .active
            .as_ref()
            .and_then(|s| self.contents.get(s))
            .map(|z| z.instances.len())
            .unwrap_or(0);
        self.sel.retain_below(n);
    }

    /// Seed the baseline and clear the stacks — call after a load/new so the
    /// freshly opened document is the undo floor.
    pub(crate) fn history_reset(&mut self) {
        self.history.undo.clear();
        self.history.redo.clear();
        self.history.baseline = Some(self.snapshot());
    }

    /// End-of-frame commit: if the document changed and nothing is being
    /// actively dragged/typed, fold the change into one undo step.
    pub(crate) fn commit_if_settled(&mut self, ctx: &egui::Context) {
        let interacting =
            ctx.input(|i| i.pointer.any_down()) || ctx.memory(|m| m.focused().is_some());
        if interacting {
            // Ensure we get a frame after the interaction ends to commit on.
            if self
                .history
                .baseline
                .as_ref()
                .is_some_and(|b| self.doc_differs(b))
            {
                ctx.request_repaint();
            }
            return;
        }
        let changed = self
            .history
            .baseline
            .as_ref()
            .is_some_and(|b| self.doc_differs(b));
        if changed {
            let base = self.history.baseline.take().unwrap();
            self.history.undo.push(base);
            if self.history.undo.len() > MAX_DEPTH {
                self.history.undo.remove(0);
            }
            self.history.redo.clear();
            self.history.baseline = Some(self.snapshot());
        }
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.history.undo.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    pub(crate) fn undo(&mut self) {
        if let Some(prev) = self.history.undo.pop() {
            self.history.redo.push(self.snapshot());
            self.restore(prev.clone());
            // The restored state is the new baseline, so the next settle frame
            // sees no diff and doesn't re-commit.
            self.history.baseline = Some(prev);
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Some(next) = self.history.redo.pop() {
            self.history.undo.push(self.snapshot());
            self.restore(next.clone());
            self.history.baseline = Some(next);
        }
    }
}
