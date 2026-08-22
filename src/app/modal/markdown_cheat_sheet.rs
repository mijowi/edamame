//! Markdown syntax cheat-sheet popover.  No footer buttons — dismissed
//! via Escape or the `esc` close hint.  The simplest of the modal
//! implementations and the reference example for trait-based migration.
//!
//! The body is rebuilt each frame rather than cached, because it is a
//! function of the width: the code-block and block-quote rows are
//! background washes sized to the body, and a wash built for a wider
//! terminal wraps onto a second, ragged row.  Everything else is
//! content and wraps normally.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::chrome::{self, ModalChrome};
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{markdown_cheat_sheet_body, ModalButton, ModalResponse};

pub struct CheatSheetModal {
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
}

impl CheatSheetModal {
    pub fn new() -> Self {
        Self {
            buttons: Vec::new(),
            chrome: ModalChrome::new(ModalKind::Normal, true),
        }
    }
}

impl Default for CheatSheetModal {
    fn default() -> Self {
        Self::new()
    }
}

impl Modal for CheatSheetModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let body = markdown_cheat_sheet_body(ctx.theme, chrome::body_columns(area));
        self.chrome.render(
            frame,
            area,
            ctx,
            "Markdown Cheat Sheet",
            &body,
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
        match self.chrome.on_key(&key, self.buttons.len()) {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.chrome.on_wheel(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        match self.chrome.on_click(col, row) {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
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
    use super::*;
    use crate::app::test_utils::make_app;

    #[test]
    fn open_markdown_cheat_sheet_pushes_to_stack() {
        let mut app = make_app();
        app.open_markdown_cheat_sheet();
        assert!(app.modal_stack.contains::<CheatSheetModal>());
        // Body-content regression assertions live alongside
        // `markdown_cheat_sheet_body` in `crate::ui::markdown_cheat_sheet`.
    }
}
