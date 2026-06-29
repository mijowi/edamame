//! Column-width-injection warning.  Shown the first time a
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

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse};

pub struct WidthInjectionWarning {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
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
            chrome: ModalChrome::new(ModalKind::Warning, true),
        }
    }

    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths so a mouse click on a button behaves exactly like
    /// pressing it.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
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
}

impl Default for WidthInjectionWarning {
    fn default() -> Self {
        Self::new()
    }
}

impl Modal for WidthInjectionWarning {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome.render(
            frame,
            area,
            ctx,
            "Custom column widths",
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
