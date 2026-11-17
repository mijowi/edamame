//! Shown when the watched file is deleted on disk: the buffer is now the only copy, so
//! offer `[Save]` (original path), `[Save as…]` ([`super::SaveAsModal`], for when the
//! directory is gone too), or `[Dismiss]`.  A deletion never enters diff review — there is
//! nothing to diff against — and `file_changed.rs` collapses any open diff first.

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
    /// Default location for `[Save as…]`.
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

    /// Map a chrome response to an outcome; shared by the key and click paths.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::Close,
            // Both saves ask about mixed endings first, with this modal still open beneath, so
            // `Esc` there returns here and its idempotency guard keeps holding meanwhile.  The
            // write proceeds only if this modal is still up when the answer comes: a recreate
            // on disk in between tears it down, and the review it starts must not be saved over.
            ModalResponse::ButtonPressed(0) => ModalOutcome::ContinueAnd(Box::new(|app| {
                app.with_line_ending_choice(Box::new(|app| {
                    if !app.modal_stack.remove_first::<FileDeletedModal>() {
                        return;
                    }
                    match app.save_buffer() {
                        Ok(()) => app.flash("Saved", MessageKind::Success),
                        Err(e) => app.notify(format!("Save failed: {e}"), ModalKind::Error),
                    }
                }));
            })),
            ModalResponse::ButtonPressed(1) => {
                let default = self.path.display().to_string();
                ModalOutcome::ContinueAnd(Box::new(move |app| {
                    app.with_line_ending_choice(Box::new(move |app| {
                        if app.modal_stack.remove_first::<FileDeletedModal>() {
                            app.modal_stack
                                .push(Box::new(SaveAsModal::for_deleted_file(default)));
                        }
                    }));
                }))
            }
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
        assert!(app.editor.dirty, "dismiss must not clear the dirty flag");
        assert!(app.transient.is_none());
    }

    #[test]
    fn save_as_button_opens_path_entry_modal() {
        let mut app = make_app();
        app.modal_stack
            .push(Box::new(FileDeletedModal::new("/tmp/gone.md".into())));
        app.dispatch_modal_key(key(KeyCode::Tab), 40, 80);
        app.dispatch_modal_key(key(KeyCode::Enter), 40, 80);
        assert!(!app.modal_stack.contains::<FileDeletedModal>());
        assert!(
            app.modal_stack.contains::<SaveAsModal>(),
            "[Save as…] must open the path-entry modal",
        );
    }
}
