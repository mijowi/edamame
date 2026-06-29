//! Destructive-confirmation step gating `DirtyConflictModal`'s
//! `[Discard & reload]` button.  Two buttons: `[Discard & reload]`
//! (destructive, secondary focus) and `[Cancel]` (default focus).
//! Esc or Cancel returns the user to the underlying
//! [`super::DirtyConflictModal`].
//!
//! Carries the on-disk contents already read by the watcher worker
//! so the reload is byte-identical to the change that triggered the
//! conflict, without a re-read race.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::dirty_conflict::DirtyConflictModal;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse};

pub struct DirtyConflictDiscardConfirmModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
    /// `pub(crate)` so the sibling `file_changed.rs` test module can
    /// inspect what bytes the modal would reload with — see the
    /// `second_external_change_refreshes_open_discard_confirm_modal`
    /// test.  Mutated through [`Self::set_on_disk_contents`] in
    /// production code.
    pub(crate) on_disk_contents: String,
}

impl DirtyConflictDiscardConfirmModal {
    /// Replace the carried on-disk contents with the bytes from a
    /// freshly-arrived external write.  Called from
    /// `App::handle_file_changed` when a new change is observed while
    /// this modal is open, so the user's eventual `Discard & reload`
    /// confirmation reloads against the *current* disk state rather
    /// than the stale snapshot that originally opened the modal.
    pub fn set_on_disk_contents(&mut self, contents: String) {
        self.on_disk_contents = contents;
    }

    pub fn new(on_disk_contents: String) -> Self {
        let body = vec![
            Line::raw("Discard your unsaved edits?"),
            Line::raw(""),
            Line::raw("They cannot be recovered."),
        ];
        // Cancel first (default focus) so a confused user can back
        // out with a single Enter press — destructive button is
        // intentionally one Tab away.
        Self {
            body,
            buttons: vec![
                ModalButton::new("Cancel"),
                ModalButton::new("Discard & reload"),
            ],
            chrome: ModalChrome::new(ModalKind::Warning, true),
            on_disk_contents,
        }
    }

    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths so a mouse click on a button behaves exactly like
    /// pressing it.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            // Esc returns to the DirtyConflictModal underneath — no
            // state change here.
            ModalResponse::Cancelled => ModalOutcome::Close,
            ModalResponse::ButtonPressed(0) => ModalOutcome::Close,
            ModalResponse::ButtonPressed(1) => {
                let contents = std::mem::take(&mut self.on_disk_contents);
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    // Close the parent DirtyConflictModal underneath
                    // before reloading so the modal stack is empty
                    // when the reload's flash appears.
                    app.modal_stack.remove_first::<DirtyConflictModal>();
                    app.reload_buffer_from_disk(contents);
                }))
            }
            ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }
}

impl Modal for DirtyConflictDiscardConfirmModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome.render(
            frame,
            area,
            ctx,
            "Discard unsaved edits?",
            &self.body,
            &self.buttons,
        );
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        let response = self.chrome.on_key(&key, self.buttons.len());
        self.resolve(response)
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.chrome.on_wheel(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        let response = self.chrome.on_click(col, row);
        self.resolve(response)
    }

    fn kind(&self) -> ModalKind {
        self.chrome.kind()
    }

    fn dismissable(&self) -> bool {
        self.chrome.dismissable()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
