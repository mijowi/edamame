//! Phase 9 quit-confirm modal.  Three buttons: Save / Discard / Cancel.
//! Save persists the buffer then exits; failure surfaces a sticky
//! error transient and aborts the quit.  Discard exits without
//! saving; Cancel / Escape dismisses.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::{App, MessageKind};
use crate::ui::{ModalButton, ModalResponse, ModalState, ModalView};

pub struct QuitConfirmModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    state: ModalState,
}

impl QuitConfirmModal {
    /// Build the prompt body with the supplied display name (typically
    /// the buffer's filename, or "Current buffer" when unsaved with
    /// no path).
    pub fn new(display_name: &str) -> Self {
        let body = vec![
            Line::raw(format!("{display_name} has unsaved changes.")),
            Line::raw(""),
            Line::raw("What would you like to do?"),
        ];
        Self {
            body,
            buttons: vec![
                ModalButton::new("Save"),
                ModalButton::new("Discard"),
                ModalButton::new("Cancel"),
            ],
            state: ModalState::new(),
        }
    }
}

impl Modal for QuitConfirmModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = ModalView {
            title: "Unsaved changes",
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
            ModalResponse::Cancelled => ModalOutcome::Close,
            ModalResponse::ButtonPressed(0) => ModalOutcome::CloseAnd(Box::new(|app| {
                if app.editor.buffer.save_file().is_ok() {
                    app.editor.dirty = false;
                    app.should_quit = true;
                } else {
                    app.flash("Save failed — quit aborted", MessageKind::Error);
                }
            })),
            ModalResponse::ButtonPressed(1) => ModalOutcome::CloseAnd(Box::new(|app| {
                app.should_quit = true;
            })),
            ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_by(delta);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
