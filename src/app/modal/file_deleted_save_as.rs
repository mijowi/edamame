//! Path-entry modal pushed atop [`super::FileDeletedModal`] when the
//! user picks `[Save as…]` after their file was deleted on disk.
//! Mirrors the regular [`super::SaveCopyModal`] UI (reuses
//! [`SaveCopyState`] + [`SaveCopyView`]) but its post-save effect
//! *re-points* the buffer: because the original file is gone, the
//! buffer, the App's `file_path`, and the filesystem watcher all adopt
//! the new path via [`App::save_buffer_as`] rather than writing a copy
//! aside and keeping the deleted path.
//!
//! Defaults to the deleted path so the user can recreate the file in
//! place with a single keystroke, or edit the field to write elsewhere
//! (e.g. when the original directory is also gone).

use std::any::Any;
use std::path::Path;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::flash::MessageKind;
use crate::app::App;
use crate::ui::{SaveCopyResponse, SaveCopyState, SaveCopyView};

pub struct FileDeletedSaveAsModal {
    state: SaveCopyState,
}

impl FileDeletedSaveAsModal {
    pub fn new(default_path: String) -> Self {
        Self {
            state: SaveCopyState::new(default_path),
        }
    }
}

impl Modal for FileDeletedSaveAsModal {
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
            // Cancel discards the save-as without re-opening the
            // file-deleted modal — the user can re-trigger by any
            // normal save if they change their mind.
            SaveCopyResponse::Cancelled => ModalOutcome::Close,
            SaveCopyResponse::Save(path_str) => {
                let path = Path::new(&path_str).to_owned();
                match app.save_buffer_as(&path) {
                    Ok(()) => {
                        let msg = format!("Buffer saved to {path_str}");
                        ModalOutcome::CloseAnd(Box::new(move |app| {
                            app.flash(msg, MessageKind::Success);
                        }))
                    }
                    Err(e) => {
                        // Stay open so the user can correct the path;
                        // surface the error inline.
                        self.state.last_error = Some(format!("{e}"));
                        ModalOutcome::Continue
                    }
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::make_app;
    use crate::document::Buffer;

    #[test]
    fn save_writes_buffer_and_repoints_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("recreated.md");

        let mut app = make_app();
        app.editor.buffer = Buffer::for_new_file(&dir.path().join("gone.md"));
        app.editor.buffer.insert(0, "rescued contents");
        app.editor.refresh_parsed();
        app.editor.dirty = true;

        app.modal_stack.push(Box::new(FileDeletedSaveAsModal::new(
            target.display().to_string(),
        )));
        // The default path is pre-filled; Enter accepts it.
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);

        assert!(!app.modal_stack.contains::<FileDeletedSaveAsModal>());
        assert_eq!(
            std::fs::read_to_string(&target).expect("file written"),
            "rescued contents",
        );
        // The buffer and App now live at the new path, dirty cleared.
        assert_eq!(app.file_path.as_deref(), Some(target.as_path()));
        assert_eq!(app.editor.buffer.path(), Some(target.as_path()));
        assert!(!app.editor.dirty);
    }
}
