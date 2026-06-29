//! Confirmation prompt shown when a "Save As" / `:w <path>` write would
//! clobber a *different* existing file (the buffer's own path is a
//! normal in-place save and never reaches here — see
//! [`crate::document::Buffer::would_overwrite`]).
//!
//! `[Overwrite]` writes the buffer to the chosen path and runs any
//! deferred continuation (the `after_save` of a save-then-quit flow).
//! The write is one of two modes ([`WriteMode`]): *adopt* the path as the
//! buffer's home ([`App::save_buffer_as`], for Save As / `:saveas`) or
//! write a detached *copy* ([`crate::document::Buffer::save_copy`], for
//! vim `:w <path>`).  `[Cancel]` (and `Esc`) abandon the write *and* drop
//! the continuation, leaving the user back in the editor.
//!
//! Reached three ways, all of which hand off ownership of the path and
//! the continuation so this modal can complete the write on its own:
//! - the [`super::SaveAsModal`] closes and pushes this (adopt) when its
//!   typed path collides,
//! - the vim re-point path ([`App::save_buffer_as_confirmed`]) pushes it
//!   (adopt) when `:saveas <path>` (without `!`) collides, and
//! - the vim copy path ([`App::save_copy_confirmed`]) pushes it (copy)
//!   when `:w <path>` (without `!`) collides.

use std::any::Any;
use std::path::PathBuf;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::save_as::AfterSave;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::{App, MessageKind};
use crate::ui::{ModalButton, ModalResponse};

/// How a confirmed overwrite writes the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// Re-point the buffer at the path (Save As / `:saveas`).
    Adopt,
    /// Write a detached snapshot, leaving the buffer's path unchanged
    /// (vim `:w <path>`).
    Copy,
}

pub struct OverwriteConfirmModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
    /// Destination to write once the user confirms.
    path: PathBuf,
    /// Whether confirming adopts the path or writes a detached copy.
    mode: WriteMode,
    /// Continuation to run after a confirmed write succeeds (e.g. quit
    /// for `:wq <path>`, navigate for the dirty-guard flow).  Dropped if
    /// the user cancels.
    after_save: Option<AfterSave>,
}

impl OverwriteConfirmModal {
    /// Confirm an *adopt* write (Save As / `:saveas`): the buffer is
    /// re-pointed at `path`.
    pub fn new(path: PathBuf, after_save: Option<AfterSave>) -> Self {
        Self::with_mode(path, WriteMode::Adopt, after_save)
    }

    /// Confirm a *copy* write (vim `:w <path>`): a snapshot is written and
    /// the buffer keeps its current path.
    pub fn for_copy(path: PathBuf, after_save: Option<AfterSave>) -> Self {
        Self::with_mode(path, WriteMode::Copy, after_save)
    }

    fn with_mode(path: PathBuf, mode: WriteMode, after_save: Option<AfterSave>) -> Self {
        let body = vec![
            Line::raw(format!("{} already exists.", path.display())),
            Line::raw(""),
            Line::raw("Overwrite it with the current buffer?"),
        ];
        Self {
            body,
            buttons: vec![ModalButton::new("Overwrite"), ModalButton::new("Cancel")],
            chrome: ModalChrome::new(ModalKind::Warning, true),
            path,
            mode,
            after_save,
        }
    }

    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths so a mouse click behaves exactly like the keypress.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::Close,
            // [Overwrite]: write the buffer to the chosen path and run
            // the deferred continuation.  A write error here is unusual
            // (the path was confirmed to exist) — surface it as a sticky
            // notice rather than silently dropping the action.
            ModalResponse::ButtonPressed(0) => {
                let path = self.path.clone();
                let mode = self.mode;
                let after = self.after_save.take();
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    let (result, msg) = match mode {
                        WriteMode::Adopt => (
                            app.save_buffer_as(&path),
                            format!("Saved to {}", path.display()),
                        ),
                        WriteMode::Copy => (
                            app.editor.buffer.save_copy(&path),
                            format!("Copy saved to {}", path.display()),
                        ),
                    };
                    match result {
                        Ok(()) => {
                            app.flash(msg, MessageKind::Success);
                            if let Some(after) = after {
                                after(app);
                            }
                        }
                        Err(e) => app.notify(format!("Save failed: {e}"), ModalKind::Error),
                    }
                }))
            }
            // [Cancel] or any other button: abandon the write and the
            // continuation, leaving the user where they were.
            ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }
}

impl Modal for OverwriteConfirmModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome
            .render(frame, area, ctx, "File exists", &self.body, &self.buttons);
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

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::make_app;
    use crate::document::Buffer;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn overwrite_writes_buffer_and_runs_continuation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("existing.md");
        std::fs::write(&target, "old contents").expect("seed target");

        let mut app = make_app();
        app.editor.buffer = Buffer::for_new_file(&dir.path().join("scratch.md"));
        app.editor.buffer.insert(0, "new contents");
        app.editor.refresh_parsed();
        app.editor.dirty = true;

        app.modal_stack.push(Box::new(OverwriteConfirmModal::new(
            target.clone(),
            Some(Box::new(|app| app.should_quit = true)),
        )));
        // Default focus is [Overwrite]; Enter confirms.
        app.dispatch_modal_key(key(KeyCode::Enter), 40, 80);

        assert!(!app.modal_stack.contains::<OverwriteConfirmModal>());
        assert_eq!(
            std::fs::read_to_string(&target).expect("file written"),
            "new contents",
        );
        assert_eq!(app.editor.buffer.path(), Some(target.as_path()));
        assert!(!app.editor.dirty);
        assert!(app.should_quit, "continuation must run after overwrite");
    }

    #[test]
    fn copy_mode_writes_snapshot_but_keeps_buffer_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let original = dir.path().join("original.md");
        let target = dir.path().join("existing.md");
        std::fs::write(&target, "old contents").expect("seed target");

        let mut app = make_app();
        app.editor.buffer = Buffer::for_new_file(&original);
        app.editor.buffer.insert(0, "live contents");
        app.editor.refresh_parsed();
        app.editor.dirty = true;

        app.modal_stack
            .push(Box::new(OverwriteConfirmModal::for_copy(
                target.clone(),
                None,
            )));
        app.dispatch_modal_key(key(KeyCode::Enter), 40, 80);

        // The copy lands on disk…
        assert_eq!(
            std::fs::read_to_string(&target).expect("file written"),
            "live contents",
        );
        // …but the buffer keeps editing the original, still dirty.
        assert_eq!(app.editor.buffer.path(), Some(original.as_path()));
        assert!(app.editor.dirty, "a copy must not clear the dirty flag");
    }

    #[test]
    fn cancel_leaves_file_untouched_and_drops_continuation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("existing.md");
        std::fs::write(&target, "old contents").expect("seed target");

        let mut app = make_app();
        app.editor.buffer = Buffer::for_new_file(&dir.path().join("scratch.md"));
        app.editor.buffer.insert(0, "new contents");
        app.editor.refresh_parsed();

        app.modal_stack.push(Box::new(OverwriteConfirmModal::new(
            target.clone(),
            Some(Box::new(|app| app.should_quit = true)),
        )));
        app.dispatch_modal_key(key(KeyCode::Esc), 40, 80);

        assert!(!app.modal_stack.contains::<OverwriteConfirmModal>());
        assert_eq!(
            std::fs::read_to_string(&target).expect("file intact"),
            "old contents",
            "Esc must not overwrite the existing file",
        );
        assert!(!app.should_quit, "cancel must drop the continuation");
    }
}
