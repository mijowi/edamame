//! Warning modal shown when the watched file is deleted on disk while
//! a buffer for it is open.  The in-memory buffer is now the only copy
//! of its contents, so the modal offers to write it back out:
//!
//! - `[Save]` re-creates the file at its original path via
//!   [`App::save_buffer`].
//! - `[Save as…]` opens a [`super::SaveAsModal`] to write the buffer to
//!   a new path and re-point at it (useful when the original directory
//!   is also gone).
//! - `Esc` / `[Dismiss]` closes and keeps the buffer in memory,
//!   unchanged — the user can save later by any normal means.
//!
//! Unlike an external *change*, a deletion never enters diff review:
//! there is nothing on disk to diff against.  The watcher arm in
//! `file_changed.rs` collapses any open diff before pushing this modal.

use std::any::Any;
use std::path::PathBuf;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use super::SaveAsModal;
use crate::app::App;
use crate::app::MessageKind;
use crate::ui::{ModalButton, ModalResponse};

pub struct FileDeletedModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
    /// The path that was deleted — used as the default location when
    /// the user picks `[Save as…]`.
    path: PathBuf,
}

impl FileDeletedModal {
    pub fn new(path: PathBuf) -> Self {
        let body = vec![
            Line::raw(format!("{} was deleted on disk.", path.display())),
            Line::raw(""),
            Line::raw("The open buffer is now the only copy of its contents."),
            Line::raw("Save it back to disk, or keep editing in memory."),
        ];
        Self {
            body,
            buttons: vec![
                ModalButton::new("Save"),
                ModalButton::new("Save as…"),
                ModalButton::new("Dismiss"),
            ],
            chrome: ModalChrome::new(ModalKind::Warning, true),
            path,
        }
    }

    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths so a mouse click on a button behaves exactly like
    /// pressing it.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            // Esc / [Dismiss] — keep the buffer in memory, do nothing.
            ModalResponse::Cancelled => ModalOutcome::Close,
            // [Save] — re-create the file at its original path.
            ModalResponse::ButtonPressed(0) => {
                ModalOutcome::CloseAnd(Box::new(|app| match app.save_buffer() {
                    Ok(()) => app.flash("Saved", MessageKind::Success),
                    Err(e) => app.notify(format!("Save failed: {e}"), ModalKind::Error),
                }))
            }
            // [Save as…] — open the path-entry modal to write elsewhere
            // and re-point the buffer.
            ModalResponse::ButtonPressed(1) => {
                let default = self.path.display().to_string();
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    app.modal_stack
                        .push(Box::new(SaveAsModal::for_deleted_file(default)));
                }))
            }
            // [Dismiss].
            ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }
}

impl Modal for FileDeletedModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome.render(
            frame,
            area,
            ctx,
            "File deleted on disk",
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

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::make_app;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn escape_dismisses_without_side_effects() {
        let mut app = make_app();
        app.editor.dirty = true;
        app.modal_stack
            .push(Box::new(FileDeletedModal::new("/tmp/gone.md".into())));
        app.dispatch_modal_key(key(KeyCode::Esc), 40, 80);
        assert!(!app.modal_stack.contains::<FileDeletedModal>());
        // Dismiss leaves the buffer exactly as it was.
        assert!(app.editor.dirty, "dismiss must not clear the dirty flag");
        assert!(app.transient.is_none());
    }

    #[test]
    fn save_as_button_opens_path_entry_modal() {
        let mut app = make_app();
        app.modal_stack
            .push(Box::new(FileDeletedModal::new("/tmp/gone.md".into())));
        // Tab once to focus [Save as…] (index 1), then activate it.
        app.dispatch_modal_key(key(KeyCode::Tab), 40, 80);
        app.dispatch_modal_key(key(KeyCode::Enter), 40, 80);
        assert!(!app.modal_stack.contains::<FileDeletedModal>());
        assert!(
            app.modal_stack.contains::<SaveAsModal>(),
            "[Save as…] must open the path-entry modal",
        );
    }
}
