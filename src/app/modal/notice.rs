//! Generic message-only modal used for warnings, errors, and stub
//! notices that were previously surfaced as hint-line flashes.  Carries
//! one or more body lines and a [`ModalKind`] that drives the title
//! colour and label; no footer buttons — the user dismisses with `Esc`
//! or the `esc` close affordance.
//!
//! Replaces the old sticky `MessageKind::Error` flash and the
//! auto-expiring `Warning` flash for genuine refusals (e.g. "Insert
//! Table requires a blank line"): a modal can't be missed and doesn't
//! depend on a per-theme warning colour being visually loud.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse, ModalState, ModalView};

pub struct NoticeModal {
    title: &'static str,
    text: String,
    buttons: Vec<ModalButton>,
    state: ModalState,
    kind: ModalKind,
    dismissable: bool,
}

impl NoticeModal {
    /// Build a notice from a single message string.  Multi-line input
    /// is split on `\n` at render time so each source line becomes its
    /// own `Line` and `ModalView`'s wrap logic only has to handle
    /// horizontal overflow.
    pub fn new(text: impl Into<String>, kind: ModalKind) -> Self {
        Self {
            title: title_for(kind),
            text: text.into(),
            buttons: Vec::new(),
            state: ModalState::new(),
            kind,
            dismissable: true,
        }
    }

    /// Raw message text, retained verbatim for duplicate-detection in
    /// [`crate::app::App::notify`].  Compared against the next incoming
    /// notice so a retry loop doesn't pile identical modals on the
    /// stack.  Deliberately not exposed via the [`Modal`] trait —
    /// `notify` downcasts to `NoticeModal` so other modal types don't
    /// accidentally participate in dedup and silently get suppressed.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Split the raw message into per-line `Line`s for `ModalView`.
    /// Empty input still yields one blank line so the modal renders a
    /// non-zero body.
    fn body_lines(&self) -> Vec<Line<'static>> {
        let lines: Vec<Line<'static>> =
            self.text.lines().map(|l| Line::raw(l.to_owned())).collect();
        if lines.is_empty() {
            vec![Line::raw("")]
        } else {
            lines
        }
    }
}

fn title_for(kind: ModalKind) -> &'static str {
    match kind {
        ModalKind::Error => "Error",
        ModalKind::Warning => "Warning",
        ModalKind::Normal => "Notice",
    }
}

impl Modal for NoticeModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let body = self.body_lines();
        let view = ModalView::new(
            self.title,
            &body,
            &self.buttons,
            ctx.theme,
            self.kind,
            self.dismissable,
        );
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        match self
            .state
            .handle_key(&key, self.buttons.len(), self.dismissable)
        {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        super::types::close_if_esc_clicked(self.state.esc_button_rect, col, row)
    }

    fn kind(&self) -> ModalKind {
        self.kind
    }

    fn dismissable(&self) -> bool {
        self.dismissable
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
    use super::*;
    use crate::app::test_utils::make_app;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn title_follows_kind() {
        assert_eq!(NoticeModal::new("x", ModalKind::Error).title, "Error");
        assert_eq!(NoticeModal::new("x", ModalKind::Warning).title, "Warning");
        assert_eq!(NoticeModal::new("x", ModalKind::Normal).title, "Notice");
    }

    #[test]
    fn multi_line_message_is_split() {
        let m = NoticeModal::new("first\nsecond", ModalKind::Warning);
        let body = m.body_lines();
        assert_eq!(body.len(), 2);
        assert_eq!(body[0].to_string(), "first");
        assert_eq!(body[1].to_string(), "second");
    }

    #[test]
    fn empty_message_still_renders_one_line() {
        let m = NoticeModal::new("", ModalKind::Warning);
        assert_eq!(m.body_lines().len(), 1);
    }

    #[test]
    fn escape_dismisses() {
        let mut app = make_app();
        app.modal_stack
            .push(Box::new(NoticeModal::new("boom", ModalKind::Error)));
        assert!(app.modal_stack.contains::<NoticeModal>());
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 40, 80);
        assert!(!app.modal_stack.contains::<NoticeModal>());
    }
}
