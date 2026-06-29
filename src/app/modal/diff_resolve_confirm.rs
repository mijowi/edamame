//! Confirmation gate shown when every diff hunk has been decided.
//! `[Apply]` swaps the merged rope into the editor buffer and exits
//! diff mode; `[Keep reviewing]` (or Esc) dismisses the modal with
//! every decision intact so the user can change their mind.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse};

pub struct DiffResolveConfirmModal {
    chrome: ModalChrome,
    buttons: Vec<ModalButton>,
    summary: String,
}

impl DiffResolveConfirmModal {
    pub fn new(accepted: usize, rejected: usize) -> Self {
        let summary = format!("{accepted} accepted, {rejected} rejected");
        Self {
            chrome: ModalChrome::new(ModalKind::Normal, true),
            buttons: vec![
                ModalButton::new("Apply"),
                ModalButton::new("Keep reviewing"),
            ],
            summary,
        }
    }

    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths so a mouse click on a button behaves exactly like
    /// pressing it.
    fn resolve(&self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::Close,
            ModalResponse::ButtonPressed(0) => {
                ModalOutcome::CloseAnd(Box::new(|app| app.apply_diff_resolution()))
            }
            ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
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
        self.chrome.render(
            frame,
            area,
            ctx,
            "Apply merged result?",
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
