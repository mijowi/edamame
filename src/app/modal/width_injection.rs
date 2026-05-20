//! Phase 13 column-width-injection warning.  Shown the first time a
//! user commits a column-border drag on a table without an existing
//! `<!-- tui-columns: ... -->` comment.  Buttons:
//!   0 → `Continue` — write the comment for this table; ask again next time.
//!   1 → `Continue and don't ask again` — flip
//!       `config.table.warn_on_width_injection` to false and persist it.
//! Escape (or the `esc` close hint) discards the live width preview
//! without writing.
//!
//! The pending table-byte-start is stored in
//! [`crate::editor::EditorState::pending_column_widths_commit`] (set by
//! the column-border drag's release handler), not on the modal — both
//! `commit_pending_column_widths` and `cancel_pending_column_widths`
//! read it from there.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse, ModalState, ModalView};

pub struct WidthInjectionWarning {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    state: ModalState,
    kind: ModalKind,
    dismissable: bool,
}

impl WidthInjectionWarning {
    pub fn new() -> Self {
        Self {
            body: vec![
                Line::raw("Setting custom column widths adds a"),
                Line::raw("<!-- tui-columns: [...] --> comment to the"),
                Line::raw("Markdown source so the layout persists."),
                Line::raw(""),
                Line::raw("Continue?"),
            ],
            buttons: vec![
                ModalButton::new("Continue"),
                ModalButton::new("Continue and don't ask again"),
            ],
            state: ModalState::new(),
            kind: ModalKind::Warning,
            dismissable: true,
        }
    }
}

impl Default for WidthInjectionWarning {
    fn default() -> Self {
        Self::new()
    }
}

impl Modal for WidthInjectionWarning {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = ModalView::new(
            "Custom column widths",
            &self.body,
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
            ModalResponse::Cancelled => ModalOutcome::CloseAnd(Box::new(|app| {
                app.editor.cancel_pending_column_widths();
            })),
            ModalResponse::ButtonPressed(0) => ModalOutcome::CloseAnd(Box::new(|app| {
                app.editor.commit_pending_column_widths();
            })),
            ModalResponse::ButtonPressed(_) => ModalOutcome::CloseAnd(Box::new(|app| {
                app.config.table.warn_on_width_injection = false;
                app.save_config_with_flash("failed to persist table.warn_on_width_injection");
                app.editor.commit_pending_column_widths();
            })),
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
}
