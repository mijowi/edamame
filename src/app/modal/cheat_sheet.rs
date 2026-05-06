//! Markdown syntax cheat-sheet popover.  Static body, single button,
//! dismiss-only — the simplest of the modal implementations and the
//! reference example for trait-based migration.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::Theme;
use crate::ui::{markdown_cheat_sheet_body, ModalButton, ModalResponse, ModalState, ModalView};

pub struct CheatSheetModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    state: ModalState,
}

impl CheatSheetModal {
    pub fn new(theme: &Theme) -> Self {
        Self {
            body: markdown_cheat_sheet_body(theme),
            buttons: vec![ModalButton::new("OK")],
            state: ModalState::new(),
        }
    }
}

impl Modal for CheatSheetModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = ModalView {
            title: "Markdown Cheat Sheet",
            body: &self.body,
            buttons: &self.buttons,
            theme: ctx.theme,
        };
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        match self.state.handle_key(&key, self.buttons.len()) {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_by(delta);
    }

    fn as_any(&self) -> &dyn Any {
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
