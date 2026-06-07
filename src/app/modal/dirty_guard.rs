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

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse};

pub struct DirtyGuardModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
    /// The destination that was about to be followed when the guard
    /// fired.  Restored to the App via the close callback after Save
    /// or Discard.
    pending: PathBuf,
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
            chrome: ModalChrome::new(ModalKind::Warning, true),
            pending,
        }
    }

    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths so a mouse click on a button behaves exactly like
    /// pressing it.
    ///
    /// The cursor-visibility calls read the cached doc dimensions from
    /// `App` (`last_doc_height` / `last_doc_width`) rather than taking
    /// them as parameters: `Modal::handle_click` has no live `DocDims`
    /// to thread in, so both paths share the same App-sourced values.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::CloseAnd(Box::new(move |app| {
                let (h, w) = (app.last_doc_height, app.last_doc_width);
                app.editor.ensure_cursor_visible(h, w);
            })),
            ModalResponse::ButtonPressed(idx) => {
                let pending = std::mem::take(&mut self.pending);
                match idx {
                    0 => ModalOutcome::CloseAnd(Box::new(move |app| {
                        match app.save_buffer() {
                            Ok(()) => app.navigate_to_file(pending),
                            Err(e) => {
                                tracing::warn!(target: "link", error = %e, "save-before-navigate failed");
                            }
                        }
                        let (h, w) = (app.last_doc_height, app.last_doc_width);
                        app.editor.ensure_cursor_visible(h, w);
                    })),
                    _ => ModalOutcome::CloseAnd(Box::new(move |app| {
                        app.editor.dirty = false;
                        app.navigate_to_file(pending);
                        let (h, w) = (app.last_doc_height, app.last_doc_width);
                        app.editor.ensure_cursor_visible(h, w);
                    })),
                }
            }
        }
    }
}

impl Modal for DirtyGuardModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome.render(
            frame,
            area,
            ctx,
            "Unsaved changes",
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

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
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
