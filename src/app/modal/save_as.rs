//! Path-entry modal for "Save As" — write the buffer to a chosen path
//! and *adopt* it as the buffer's home.  Reuses the shared
//! [`SaveCopyState`] + [`SaveCopyView`] widget, but its post-save effect
//! re-points the buffer, the App's `file_path`, and the filesystem
//! watcher at the new location via [`App::save_buffer_as`].  When the
//! typed path would clobber a *different* existing file it hands off to
//! [`super::OverwriteConfirmModal`] (a vim `:w <path>` instead writes a
//! detached copy and leaves the buffer's path untouched).
//!
//! Reached from several places:
//! - [`Action::SaveAs`](crate::config::Action::SaveAs) (command palette),
//! - a `Save` (`Ctrl-S` / vim `:w`) on a path-less buffer, which has no
//!   destination yet, and
//! - the file-deleted `[Save as…]` button, where the original file is gone
//!   so the buffer must move to a new home.
//!
//! An optional `after_save` continuation runs once the write succeeds —
//! used by the "save then quit" (`:wq`, quit-confirm) and "save then
//! navigate" (dirty-guard) flows so a path-less buffer can complete the
//! deferred action after the user supplies a path.

use std::any::Any;
use std::path::Path;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::{App, MessageKind};
use crate::ui::{default_save_as_path, SaveCopyResponse, SaveCopyState, SaveCopyView};

/// A side effect to run after a successful save (e.g. quit, navigate).
pub type AfterSave = Box<dyn FnOnce(&mut App)>;

pub struct SaveAsModal {
    state: SaveCopyState,
    after_save: Option<AfterSave>,
    /// True when this modal is the file-deletion recovery flow (opened
    /// from [`super::FileDeletedModal`]'s `[Save as…]`).  The watcher
    /// dedup in `file_changed.rs` suppresses external events only for
    /// *this* variant — a voluntary save-as on a live file must not hide
    /// an external change that arrives while the prompt is open.
    from_deletion: bool,
}

impl SaveAsModal {
    /// Open as the file-deletion recovery flow, seeded with the deleted
    /// path.  Distinguished from a voluntary save-as so the watcher dedup
    /// can treat an open prompt as an in-progress deletion flow.
    pub fn for_deleted_file(default_path: String) -> Self {
        Self {
            state: SaveCopyState::new(default_path),
            after_save: None,
            from_deletion: true,
        }
    }

    /// Seed the path field with the buffer's current path resolved to an
    /// absolute path (so the destination directory is visible and the user
    /// can retarget it), or `<cwd>/untitled.md` for an unnamed buffer, and
    /// optionally attach a continuation to run after a successful save.
    pub fn for_buffer_path(buffer_path: Option<&Path>, after_save: Option<AfterSave>) -> Self {
        Self {
            state: SaveCopyState::new(default_save_as_path(buffer_path)),
            after_save,
            from_deletion: false,
        }
    }

    /// Whether this is the file-deletion recovery flow (see
    /// [`Self::for_deleted_file`]).
    pub fn is_deletion_recovery(&self) -> bool {
        self.from_deletion
    }
}

impl Modal for SaveAsModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = SaveCopyView {
            theme: ctx.theme,
            cursor_visible: ctx.cursor_visible,
            title: "Save As",
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
            // Cancel abandons the save-as; any deferred continuation
            // (quit / navigate) is dropped, leaving the user where they
            // were so a mis-press is recoverable.
            SaveCopyResponse::Cancelled => ModalOutcome::Close,
            SaveCopyResponse::Save(path_str) => {
                let path = Path::new(&path_str).to_owned();
                // Writing over a *different* existing file: confirm first.
                // Hand the destination and any deferred continuation to the
                // confirm modal and close this prompt.  Declining there
                // returns the user to the editor (like Cancel) — they
                // re-open Save As to pick another name.
                if app.editor.buffer.would_overwrite(&path) {
                    let after = self.after_save.take();
                    return ModalOutcome::CloseAnd(Box::new(move |app| {
                        app.modal_stack
                            .push(Box::new(super::OverwriteConfirmModal::new(path, after)));
                    }));
                }
                match app.save_buffer_as(&path) {
                    Ok(()) => {
                        let after = self.after_save.take();
                        let msg = format!("Saved to {path_str}");
                        ModalOutcome::CloseAnd(Box::new(move |app| {
                            app.flash(msg, MessageKind::Success);
                            if let Some(after) = after {
                                after(app);
                            }
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

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::make_app;
    use crate::document::Buffer;

    #[test]
    fn save_writes_buffer_and_repoints_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("renamed.md");

        let mut app = make_app();
        app.editor.buffer = Buffer::for_new_file(&dir.path().join("original.md"));
        app.editor.buffer.insert(0, "moved contents");
        app.editor.refresh_parsed();
        app.editor.dirty = true;

        app.modal_stack
            .push(Box::new(SaveAsModal::for_buffer_path(Some(&target), None)));
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);

        assert!(!app.modal_stack.contains::<SaveAsModal>());
        assert_eq!(
            std::fs::read_to_string(&target).expect("file written"),
            "moved contents",
        );
        // The buffer and App now live at the new path, dirty cleared.
        assert_eq!(app.file_path.as_deref(), Some(target.as_path()));
        assert_eq!(app.editor.buffer.path(), Some(target.as_path()));
        assert!(!app.editor.dirty);
    }

    #[test]
    fn after_save_continuation_runs_on_success() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("quit.md");

        let mut app = make_app();
        app.editor.buffer = Buffer::new();
        app.editor.buffer.insert(0, "scratch");
        app.editor.refresh_parsed();
        app.editor.dirty = true;

        // Seed the field with the target path (an absolute temp path that
        // doesn't yet exist, so no overwrite confirm) and accept it.
        app.modal_stack.push(Box::new(SaveAsModal::for_buffer_path(
            Some(&target),
            Some(Box::new(|app| app.should_quit = true)),
        )));
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);

        assert!(!app.modal_stack.contains::<SaveAsModal>());
        assert!(
            app.should_quit,
            "continuation must run after a successful save"
        );
        assert_eq!(app.editor.buffer.path(), Some(target.as_path()));
    }
}
