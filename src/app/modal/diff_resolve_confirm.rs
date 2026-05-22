//! Confirmation gate shown when every diff hunk has been decided.
//! `[Apply]` swaps the merged rope into the editor buffer and exits
//! diff mode; `[Keep reviewing]` (or Esc) dismisses the modal with
//! every decision intact so the user can change their mind.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse, ModalState, ModalView};

pub struct DiffResolveConfirmModal {
    state: ModalState,
    buttons: Vec<ModalButton>,
    summary: String,
    kind: ModalKind,
    dismissable: bool,
}

impl DiffResolveConfirmModal {
    pub fn new(accepted: usize, rejected: usize) -> Self {
        let summary = format!("{accepted} accepted, {rejected} rejected");
        Self {
            state: ModalState::new(),
            buttons: vec![
                ModalButton::new("Apply"),
                ModalButton::new("Keep reviewing"),
            ],
            summary,
            kind: ModalKind::Normal,
            dismissable: true,
        }
    }
}

impl Modal for DiffResolveConfirmModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let body = vec![
            Line::raw("All hunks have been reviewed."),
            Line::raw(""),
            Line::raw(self.summary.clone()),
            Line::raw(""),
            Line::raw("Apply the merged result to your buffer?"),
        ];
        let view = ModalView::new(
            "Apply merged result?",
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
            ModalResponse::Cancelled => ModalOutcome::Close,
            ModalResponse::ButtonPressed(0) => {
                ModalOutcome::CloseAnd(Box::new(|app| app.apply_diff_resolution()))
            }
            ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
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
