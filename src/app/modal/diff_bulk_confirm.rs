//! Confirmation gate for the bulk decision actions `DiffAcceptAll` /
//! `DiffRejectAll`.  Accept-all / reject-all override *every* hunk's
//! decision in one keystroke, so an accidental `Shift-Y` / `Shift-N`
//! would silently wipe out a mix of careful per-hunk choices.  Unlike
//! a single-hunk mistake — recoverable by navigating back and
//! re-deciding (or `DiffResetHunk`) — a bulk flip is the one case
//! navigation can't undo, and decisions are deliberately not on an
//! undo stack.  This modal is that guard: `[Yes]` applies the bulk
//! decision (and then routes through the normal resolve-confirm flow);
//! `[No]` (or `Esc`) dismisses with every prior decision intact.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::diff::Decision;
use crate::ui::{ModalButton, ModalResponse};

/// `[Yes, No]` — `Yes` is default-focused so the common case (the user
/// meant to bulk-decide) is a single confirming Enter, while `Esc` /
/// `No` still cancels an accidental flip.
const YES_IDX: usize = 0;

pub struct DiffBulkConfirmModal {
    chrome: ModalChrome,
    buttons: Vec<ModalButton>,
    /// The decision to apply to every hunk on confirmation.
    decision: Decision,
    title: String,
    body_line: String,
}

impl DiffBulkConfirmModal {
    pub fn new(decision: Decision) -> Self {
        let (title, body_line) = match decision {
            Decision::Accepted => (
                "Accept every hunk?".to_owned(),
                "Accept all changes, overriding your current decisions?".to_owned(),
            ),
            Decision::Rejected => (
                "Reject every hunk?".to_owned(),
                "Reject all changes, overriding your current decisions?".to_owned(),
            ),
            // `DiffAcceptAll` / `DiffRejectAll` only ever construct this
            // modal with a non-pending decision; guard defensively so a
            // future caller can't render a blank prompt.
            Decision::Pending => (
                "Apply to every hunk?".to_owned(),
                "Override your current decisions?".to_owned(),
            ),
        };
        Self {
            chrome: ModalChrome::new(ModalKind::Warning, true),
            buttons: vec![ModalButton::new("Yes"), ModalButton::new("No")],
            decision,
            title,
            body_line,
        }
    }

    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths so a mouse click on a button behaves exactly like
    /// pressing it.
    fn resolve(&self, response: ModalResponse) -> ModalOutcome {
        let decision = self.decision;
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::Close,
            ModalResponse::ButtonPressed(YES_IDX) => {
                ModalOutcome::CloseAnd(Box::new(move |app| app.apply_diff_bulk_decision(decision)))
            }
            // `No` (or any stray index) just dismisses, decisions intact.
            ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }
}

impl Modal for DiffBulkConfirmModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let body = vec![Line::raw(self.body_line.clone())];
        self.chrome
            .render(frame, area, ctx, &self.title, &body, &self.buttons);
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

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::make_app;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn yes_closes_with_callback() {
        let mut app = make_app();
        let mut modal = DiffBulkConfirmModal::new(Decision::Accepted);
        // `Yes` is default-focused, so a bare Enter confirms.
        let out = modal.handle_key(key(KeyCode::Enter), &mut app, 40, 80);
        assert!(matches!(out, ModalOutcome::CloseAnd(_)));
    }

    #[test]
    fn no_dismisses_without_callback() {
        let mut app = make_app();
        let mut modal = DiffBulkConfirmModal::new(Decision::Rejected);
        // Tab onto `No` (index 1), then Enter.
        modal.handle_key(key(KeyCode::Tab), &mut app, 40, 80);
        let out = modal.handle_key(key(KeyCode::Enter), &mut app, 40, 80);
        assert!(matches!(out, ModalOutcome::Close));
    }

    #[test]
    fn esc_dismisses() {
        let mut app = make_app();
        let mut modal = DiffBulkConfirmModal::new(Decision::Accepted);
        let out = modal.handle_key(key(KeyCode::Esc), &mut app, 40, 80);
        assert!(matches!(out, ModalOutcome::Close));
    }
}
