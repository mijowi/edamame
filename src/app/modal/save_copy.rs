//! Save-a-copy path-input modal.  Adapter wrapping
//! [`crate::ui::SaveCopyState`].
//!
//! On Save the buffer is written to the entered path via
//! `Buffer::save_copy` — which intentionally does NOT update the
//! buffer's own path, so the user's next `Save` still goes to the
//! original file.  On error the modal stays open with `last_error`
//! set so the user can correct the path.

use std::any::Any;
use std::path::Path;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::{App, MessageKind};
use crate::ui::{default_copy_path, SaveCopyResponse, SaveCopyState, SaveCopyView};

pub struct SaveCopyModal {
    state: SaveCopyState,
}

impl SaveCopyModal {
    pub fn new(default_path: String) -> Self {
        Self {
            state: SaveCopyState::new(default_path),
        }
    }

    /// Convenience constructor that derives the default path from the
    /// supplied buffer path (or a generic one if `None`).  Mirrors the
    /// legacy [`App::open_save_copy_modal`] one-liner.
    pub fn for_buffer_path(buffer_path: Option<&Path>) -> Self {
        Self::new(default_copy_path(buffer_path))
    }
}

impl Modal for SaveCopyModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = SaveCopyView {
            theme: ctx.theme,
            cursor_visible: ctx.cursor_visible,
        };
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        match self.state.handle_key(&key) {
            SaveCopyResponse::Continue => ModalOutcome::Continue,
            SaveCopyResponse::Cancelled => ModalOutcome::Close,
            SaveCopyResponse::Save(path_str) => {
                let path = Path::new(&path_str).to_owned();
                match app.editor.buffer.save_copy(&path) {
                    Ok(()) => {
                        let msg = format!("Copy saved to {path_str}");
                        ModalOutcome::CloseAnd(Box::new(move |app| {
                            app.flash(msg, MessageKind::Success);
                        }))
                    }
                    Err(e) => {
                        // Keep the modal open so the user can correct
                        // the path; surface the error in the modal's
                        // error row.
                        self.state.last_error = Some(format!("{e}"));
                        ModalOutcome::Continue
                    }
                }
            }
        }
    }

    fn handle_paste(&mut self, text: &str) -> ModalOutcome {
        self.state.paste(text);
        ModalOutcome::Continue
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        super::types::close_if_esc_clicked(self.state.esc_button_rect, col, row)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
