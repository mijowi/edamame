//! Path-entry modal pushed atop [`super::DirtyConflictModal`] when the
//! user picks `[Save a copy]`.  Mirrors the regular
//! [`super::SaveCopyModal`] UI (reuses [`SaveCopyState`] +
//! [`SaveCopyView`]) but its post-save effect is "save buffer to the
//! chosen path, then reload the on-disk contents into the editor's
//! buffer" — the in-flight conflict resolution flow.  Carries the
//! on-disk contents already read by the watcher worker so the
//! reload skips a disk re-read.

use std::any::Any;
use std::path::Path;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::dirty_conflict::DirtyConflictModal;
use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::flash::MessageKind;
use crate::app::App;
use crate::ui::{SaveCopyResponse, SaveCopyState, SaveCopyView};

pub struct DirtyConflictSaveCopyModal {
    state: SaveCopyState,
    on_disk_contents: String,
}

impl DirtyConflictSaveCopyModal {
    pub fn new(default_path: String, on_disk_contents: String) -> Self {
        Self {
            state: SaveCopyState::new(default_path),
            on_disk_contents,
        }
    }

    /// Replace the carried on-disk contents with the bytes from a
    /// freshly-arrived external write.  Called from
    /// `App::handle_file_changed` when a new change is observed while
    /// this modal is open, so the user's eventual `Save` confirmation
    /// reloads against the *current* disk state rather than the stale
    /// snapshot that originally opened the modal.
    pub fn set_on_disk_contents(&mut self, contents: String) {
        self.on_disk_contents = contents;
    }
}

impl Modal for DirtyConflictSaveCopyModal {
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
            // Cancel returns to the underlying DirtyConflictModal —
            // intact, so the user can pick a different action.
            SaveCopyResponse::Cancelled => ModalOutcome::Close,
            SaveCopyResponse::Save(path_str) => {
                let path = Path::new(&path_str).to_owned();
                match app.editor.buffer.save_copy(&path) {
                    Ok(()) => {
                        // After the copy is on disk, replace the
                        // in-memory buffer with the on-disk contents
                        // that triggered the conflict.  The user
                        // explicitly chose "save my edits aside and
                        // load the disk version."
                        let contents = std::mem::take(&mut self.on_disk_contents);
                        let display = path_str.clone();
                        ModalOutcome::CloseAnd(Box::new(move |app| {
                            // Close the parent DirtyConflictModal
                            // underneath; the post-reload "Reloaded
                            // from disk" flash from
                            // `reload_buffer_from_disk` is the
                            // primary signal, so we deliberately do
                            // not double up with a "copy saved" toast
                            // — keep the chrome quiet.  We do flash
                            // the path so the user can find the file
                            // they just wrote.
                            app.modal_stack.remove_first::<DirtyConflictModal>();
                            app.flash(format!("Buffer saved to {display}"), MessageKind::Success);
                            app.reload_buffer_from_disk(contents);
                        }))
                    }
                    Err(e) => {
                        // Stay open so the user can correct the
                        // path; show the validation error inline.
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
