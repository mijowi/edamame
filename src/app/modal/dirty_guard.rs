//! Dirty-buffer guard shown before navigating away from an unsaved
//! document.  Two buttons: Save / Discard.  Escape (or the `esc`
//! close hint) abandons the navigation entirely.  Carries the pending
//! navigation target across the modal's lifetime so the App can
//! resume it once the user picks a button.

use std::any::Any;
use std::path::PathBuf;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse, ModalState, ModalView};

pub struct DirtyGuardModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    state: ModalState,
    /// The destination that was about to be followed when the guard
    /// fired.  Restored to the App via the close callback after Save
    /// or Discard.
    pending: PathBuf,
    kind: ModalKind,
    dismissable: bool,
}

impl DirtyGuardModal {
    pub fn new(current_display: &str, pending: PathBuf) -> Self {
        let body = vec![
            Line::raw(format!("{current_display} has unsaved changes.")),
            Line::raw(""),
            Line::raw(format!("Opening {} will abandon them.", pending.display())),
            Line::raw(""),
            Line::raw("What would you like to do?"),
        ];
        Self {
            body,
            buttons: vec![ModalButton::new("Save"), ModalButton::new("Discard")],
            state: ModalState::new(),
            pending,
            kind: ModalKind::Warning,
            dismissable: true,
        }
    }
}

impl Modal for DirtyGuardModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = ModalView::new(
            "Unsaved changes",
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
        doc_height: usize,
        doc_width: usize,
    ) -> ModalOutcome {
        match self
            .state
            .handle_key(&key, self.buttons.len(), self.dismissable)
        {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::CloseAnd(Box::new(move |app| {
                app.editor.ensure_cursor_visible(doc_height, doc_width);
            })),
            ModalResponse::ButtonPressed(idx) => {
                let pending = std::mem::take(&mut self.pending);
                match idx {
                    0 => ModalOutcome::CloseAnd(Box::new(move |app| {
                        if app.editor.buffer.save_file().is_ok() {
                            app.editor.dirty = false;
                            app.navigate_to_file(pending);
                        } else {
                            tracing::warn!(target: "link", "save-before-navigate failed");
                        }
                        app.editor.ensure_cursor_visible(doc_height, doc_width);
                    })),
                    _ => ModalOutcome::CloseAnd(Box::new(move |app| {
                        app.editor.dirty = false;
                        app.navigate_to_file(pending);
                        app.editor.ensure_cursor_visible(doc_height, doc_width);
                    })),
                }
            }
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
