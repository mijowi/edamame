//! Asked before the first write of a file that mixes LF and CRLF line endings (issue #101).
//! The rope holds pure `\n`, so a write can use only one ending: `[Use LF]` / `[Use CRLF]`
//! records the choice via [`crate::document::Buffer::set_line_ending`], which holds for the rest
//! of the session, then runs the write that asked.  `Esc` cancels: the continuation is dropped,
//! the file stays untouched and the buffer stays dirty.
//!
//! Pushed only by [`App::with_line_ending_choice`], the one gate every interactive write goes
//! through, autosave included.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::save_as::AfterSave;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::{App, MessageKind};
use crate::document::{LineEnding, MixedEndings};
use crate::ui::{ModalButton, ModalResponse};

/// Button order; index `i` writes `CHOICES[i]`.
const CHOICES: [LineEnding; 2] = [LineEnding::Lf, LineEnding::Crlf];

pub struct LineEndingModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
    /// The write that asked; dropped on `Esc`.
    then: Option<AfterSave>,
    /// Flashed on `Esc`: what the cancel abandoned.
    cancelled: &'static str,
}

impl LineEndingModal {
    /// `current` is the ending the file's first line break uses, focused as the default;
    /// `cancelled` is the flash shown on `Esc`.
    pub fn new(
        mixed: MixedEndings,
        current: LineEnding,
        cancelled: &'static str,
        then: AfterSave,
    ) -> Self {
        let body = vec![
            Line::raw(format!("This file mixes line endings: {mixed}.")),
            Line::raw(""),
            Line::raw("Saving writes every line with the same ending. Which should it use?"),
        ];
        let mut chrome = ModalChrome::new(ModalKind::Warning, true);
        chrome.state.focused = CHOICES.iter().position(|&e| e == current).unwrap_or(0);
        Self {
            body,
            buttons: vec![ModalButton::new("Use LF"), ModalButton::new("Use CRLF")],
            chrome,
            then: Some(then),
            cancelled,
        }
    }

    /// Shared by the key and click paths so a click behaves exactly like the key.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => {
                let cancelled = self.cancelled;
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    app.flash(cancelled, MessageKind::Info);
                }))
            }
            ModalResponse::ButtonPressed(idx) => {
                let Some(&choice) = CHOICES.get(idx) else {
                    return ModalOutcome::Close;
                };
                let then = self.then.take();
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    app.editor.buffer.set_line_ending(choice);
                    if let Some(then) = then {
                        then(app);
                    }
                }))
            }
        }
    }
}

impl Modal for LineEndingModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome.render(
            frame,
            area,
            ctx,
            "Mixed line endings",
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
    //! App-level wiring, driven through `App` directly rather than the event loop.

    use std::path::{Path, PathBuf};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::modal::QuitConfirmModal;
    use crate::app::test_utils::{close_startup_modals, make_app};
    use crate::config::Action;

    const MIXED: &[u8] = b"a\r\nb\nc\r\n";

    /// An app editing a freshly written mixed file, made dirty by one inserted char.
    fn app_with_mixed_file(dir: &Path) -> (App, PathBuf) {
        let path = dir.join("mixed.md");
        std::fs::write(&path, MIXED).expect("seed");
        let mut app = make_app();
        close_startup_modals(&mut app);
        app.load_file_into_editor(path.clone()).expect("load");
        close_startup_modals(&mut app);
        app.editor.buffer.insert(0, "X");
        app.editor.dirty = true;
        (app, path)
    }

    fn press(app: &mut App, code: KeyCode) {
        app.dispatch_modal_key(KeyEvent::new(code, KeyModifiers::NONE), 40, 80);
    }

    fn save(app: &mut App) {
        app.dispatch_action(Action::Save, 40, 80);
    }

    #[test]
    fn focus_starts_on_the_first_line_breaks_ending() {
        let mixed = MixedEndings { crlf: 2, lf: 1 };
        let lf = LineEndingModal::new(mixed, LineEnding::Lf, "", Box::new(|_| {}));
        let crlf = LineEndingModal::new(mixed, LineEnding::Crlf, "", Box::new(|_| {}));
        assert_eq!(lf.chrome.state.focused, 0);
        assert_eq!(crlf.chrome.state.focused, 1);
    }

    #[test]
    fn saving_a_dirty_mixed_file_asks_and_esc_leaves_it_byte_identical() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, path) = app_with_mixed_file(dir.path());
        save(&mut app);
        assert!(app.modal_stack.contains::<LineEndingModal>());
        press(&mut app, KeyCode::Esc);
        assert!(app.modal_stack.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), MIXED);
        assert!(app.editor.dirty, "a cancelled save leaves the buffer dirty");
        let flash = app.transient.as_ref().map(|t| t.text.clone());
        assert_eq!(flash.as_deref(), Some("Save cancelled"));
    }

    #[test]
    fn esc_before_opening_the_external_editor_names_what_was_cancelled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, path) = app_with_mixed_file(dir.path());
        app.dispatch_action(Action::OpenInExternalEditor, 40, 80);
        assert!(app.modal_stack.contains::<LineEndingModal>());
        press(&mut app, KeyCode::Esc);
        assert!(!app.pending_open_file_in_editor);
        let flash = app.transient.as_ref().map(|t| t.text.clone());
        assert_eq!(flash.as_deref(), Some("Open in editor cancelled"));
        assert_eq!(std::fs::read(&path).unwrap(), MIXED);
    }

    #[test]
    fn each_choice_writes_its_ending_and_later_saves_do_not_ask() {
        for (focus_moves, expected) in [
            // The first line break is CRLF, so focus starts on `Use CRLF`.
            (0, &b"Xa\r\nb\r\nc\r\n"[..]),
            (1, &b"Xa\nb\nc\n"[..]),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let (mut app, path) = app_with_mixed_file(dir.path());
            save(&mut app);
            for _ in 0..focus_moves {
                press(&mut app, KeyCode::Left);
            }
            press(&mut app, KeyCode::Enter);
            assert!(app.modal_stack.is_empty());
            assert_eq!(std::fs::read(&path).unwrap(), expected);
            assert!(!app.editor.dirty);

            app.editor.buffer.insert(0, "Y");
            app.editor.dirty = true;
            save(&mut app);
            assert!(
                app.modal_stack.is_empty(),
                "the choice holds for the session"
            );
            assert!(!app.editor.dirty);
        }
    }

    #[test]
    fn saving_a_clean_mixed_file_neither_asks_nor_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mixed.md");
        std::fs::write(&path, MIXED).unwrap();
        let mut app = make_app();
        app.load_file_into_editor(path.clone()).unwrap();
        close_startup_modals(&mut app);
        save(&mut app);
        assert!(app.modal_stack.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), MIXED);
    }

    #[test]
    fn cancelling_the_choice_from_the_quit_prompt_aborts_the_quit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, path) = app_with_mixed_file(dir.path());
        app.open_quit_confirm();
        press(&mut app, KeyCode::Enter); // [Save]
        assert!(!app.modal_stack.contains::<QuitConfirmModal>());
        assert!(app.modal_stack.contains::<LineEndingModal>());
        press(&mut app, KeyCode::Esc);
        assert!(!app.should_quit);
        assert!(app.editor.dirty);
        assert_eq!(std::fs::read(&path).unwrap(), MIXED);
    }

    #[test]
    fn choosing_from_the_quit_prompt_saves_then_quits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, path) = app_with_mixed_file(dir.path());
        app.open_quit_confirm();
        press(&mut app, KeyCode::Enter); // [Save]
        press(&mut app, KeyCode::Enter); // [Use CRLF], focused
        assert!(app.should_quit);
        assert_eq!(std::fs::read(&path).unwrap(), b"Xa\r\nb\r\nc\r\n");
    }
}
