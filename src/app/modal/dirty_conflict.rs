//! Four-button reconciliation modal shown when a file changes on disk while the in-memory
//! buffer is dirty: `[Merge]` (diff review), `[Save a copy]`, `[Discard & reload]` (both push a
//! sibling modal), `[Keep buffer]` (explicit no-op close).
//!
//! Carries the on-disk `contents: String` the watcher worker already read, so the downstream
//! callbacks never re-read disk.

use std::any::Any;
use std::path::{Path, PathBuf};

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::dirty_conflict_discard_confirm::DirtyConflictDiscardConfirmModal;
use super::dirty_conflict_save_copy::DirtyConflictSaveCopyModal;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse};

pub struct DirtyConflictModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
    /// On-disk contents already read by the watcher worker, so the button callbacks can reload
    /// without re-reading (and racing the next watcher event).  Mutated through
    /// [`Self::set_on_disk_contents`].
    pub(crate) on_disk_contents: String,
}

impl DirtyConflictModal {
    pub fn new(on_disk_contents: String) -> Self {
        let body = vec![
            Line::raw("The file has changed on disk, but you have"),
            Line::raw("unsaved edits in this buffer."),
            Line::raw(""),
            Line::raw("How would you like to reconcile them?"),
        ];
        Self {
            body,
            buttons: vec![
                ModalButton::new("Merge"),
                ModalButton::new("Save a copy"),
                ModalButton::new("Discard & reload"),
                ModalButton::new("Keep buffer"),
            ],
            chrome: ModalChrome::new(ModalKind::Warning, false),
            on_disk_contents,
        }
    }

    /// Refresh the carried bytes when a new external write lands while a child reconciliation
    /// modal is open.  Without this, cancelling out of the child would return the user to a
    /// modal carrying stale bytes.
    pub fn set_on_disk_contents(&mut self, contents: String) {
        self.on_disk_contents = contents;
    }
}

/// Suggested copy filename: `<stem>.local.<ext>`, or `<stem>.local` with no extension, or
/// `copy.md` when `original` is `None`.  Returned absolute, matching
/// [`crate::ui::default_save_as_path`].
pub(crate) fn local_copy_path(original: Option<&Path>) -> String {
    let Some(p) = original else {
        return "copy.md".to_owned();
    };
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("copy");
    let ext = p.extension().and_then(|s| s.to_str());
    let name = match ext {
        Some(e) => format!("{stem}.local.{e}"),
        None => format!("{stem}.local"),
    };
    let copy_path: PathBuf = match p.parent() {
        Some(par) if !par.as_os_str().is_empty() => par.join(&name),
        _ => PathBuf::from(&name),
    };
    let absolute = if copy_path.is_absolute() {
        copy_path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&copy_path))
            .unwrap_or(copy_path)
    };
    absolute.display().to_string()
}

impl DirtyConflictModal {
    /// Map a resolved response to an outcome.  Shared by the key and click paths.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            // Non-dismissable, so this is unreachable; a no-op keeps the match exhaustive.
            ModalResponse::Cancelled => ModalOutcome::Continue,
            ModalResponse::ButtonPressed(0) => {
                // [Merge] — close first so the diff view (and any intro modal atop it) renders
                // cleanly.
                let on_disk = self.on_disk_contents.clone();
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    app.enter_diff_mode(on_disk);
                }))
            }
            ModalResponse::ButtonPressed(1) => {
                // [Save a copy] — stay on the stack under the path-entry sibling so a cancel
                // there returns the user here, intact.
                let on_disk = self.on_disk_contents.clone();
                // A mixed-endings prompt comes first, on top of this modal.
                ModalOutcome::ContinueAnd(Box::new(move |app| {
                    app.with_line_ending_choice(Box::new(move |app| {
                        let default = local_copy_path(app.editor.buffer.path());
                        app.modal_stack
                            .push(Box::new(DirtyConflictSaveCopyModal::new(default, on_disk)));
                    }));
                }))
            }
            ModalResponse::ButtonPressed(2) => {
                // [Discard & reload] — destructive; gated behind a second confirmation.
                let on_disk = self.on_disk_contents.clone();
                ModalOutcome::ContinueAnd(Box::new(move |app| {
                    app.modal_stack
                        .push(Box::new(DirtyConflictDiscardConfirmModal::new(on_disk)));
                }))
            }
            ModalResponse::ButtonPressed(3) => {
                // [Keep buffer] — explicit no-op.
                ModalOutcome::Close
            }
            ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }
}

impl Modal for DirtyConflictModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome.render(
            frame,
            area,
            ctx,
            "File changed on disk",
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
    use super::*;

    // A real tempdir rather than a `/tmp/…` literal: a `/`-rooted path has no drive letter, so
    // on Windows it is not absolute and would get the cwd prepended.
    #[test]
    fn local_copy_path_appends_dot_local_with_extension() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.md");
        let expected = dir.path().join("notes.local.md").display().to_string();
        assert_eq!(local_copy_path(Some(&p)), expected);
    }

    #[test]
    fn local_copy_path_appends_dot_local_without_extension() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("README");
        let expected = dir.path().join("README.local").display().to_string();
        assert_eq!(local_copy_path(Some(&p)), expected);
    }

    #[test]
    fn local_copy_path_falls_back_to_copy_md_when_none() {
        assert_eq!(local_copy_path(None), "copy.md");
    }

    use crate::app::modal::dirty_conflict_discard_confirm::DirtyConflictDiscardConfirmModal;
    use crate::app::modal::dirty_conflict_save_copy::DirtyConflictSaveCopyModal;
    use crate::app::test_utils::make_app;
    use crate::document::Buffer;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn open_with_disk_contents(contents: &str) -> (crate::app::App, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().expect("temp");
        std::fs::write(tmp.path(), "buffer").expect("seed");
        let mut app = make_app();
        app.editor.buffer = Buffer::for_new_file(tmp.path());
        app.editor.buffer.insert(0, "buffer");
        app.editor.dirty = true;
        app.file_path = Some(tmp.path().to_path_buf());
        app.set_disk_hash(b"buffer");
        app.modal_stack
            .push(Box::new(DirtyConflictModal::new(contents.to_owned())));
        (app, tmp)
    }

    #[test]
    fn keep_buffer_button_closes_modal_without_change() {
        let (mut app, _tmp) = open_with_disk_contents("external");
        // Tab to button 3 ([Keep buffer]).
        for _ in 0..3 {
            app.dispatch_modal_key(key(KeyCode::Tab), 24, 80);
        }
        app.dispatch_modal_key(key(KeyCode::Enter), 24, 80);
        assert!(
            !app.modal_stack.contains::<DirtyConflictModal>(),
            "keep buffer closes the modal",
        );
        assert_eq!(app.editor.buffer.contents(), "buffer");
        assert!(app.editor.dirty);
    }

    #[test]
    fn discard_reload_opens_confirmation_modal() {
        let (mut app, _tmp) = open_with_disk_contents("external");
        // Default focus is button 0 ([Merge]); Tab twice to reach [Discard & reload].
        app.dispatch_modal_key(key(KeyCode::Tab), 24, 80);
        app.dispatch_modal_key(key(KeyCode::Tab), 24, 80);
        app.dispatch_modal_key(key(KeyCode::Enter), 24, 80);
        assert!(
            app.modal_stack
                .contains::<DirtyConflictDiscardConfirmModal>(),
            "discard requires confirmation modal",
        );
        assert!(app.modal_stack.contains::<DirtyConflictModal>());
    }

    #[test]
    fn save_a_copy_opens_save_copy_modal_atop_conflict() {
        let (mut app, _tmp) = open_with_disk_contents("external");
        app.dispatch_modal_key(key(KeyCode::Tab), 24, 80);
        app.dispatch_modal_key(key(KeyCode::Enter), 24, 80);
        assert!(
            app.modal_stack.contains::<DirtyConflictSaveCopyModal>(),
            "save a copy pushes path-entry modal",
        );
        assert!(app.modal_stack.contains::<DirtyConflictModal>());
    }

    #[test]
    fn merge_button_enters_diff_mode_and_closes_modal() {
        let (mut app, _tmp) = open_with_disk_contents("external");
        app.dispatch_modal_key(key(KeyCode::Enter), 24, 80);
        assert!(
            !app.modal_stack.contains::<DirtyConflictModal>(),
            "merge must close the dirty-conflict modal",
        );
        assert_eq!(app.editor.mode, crate::editor::Mode::Diff);
        assert!(app.editor.diff.is_some(), "diff state must be initialised");
    }

    #[test]
    fn esc_does_not_dismiss_non_dismissable_modal() {
        let (mut app, _tmp) = open_with_disk_contents("external");
        app.dispatch_modal_key(key(KeyCode::Esc), 24, 80);
        assert!(
            app.modal_stack.contains::<DirtyConflictModal>(),
            "Esc must not dismiss DirtyConflictModal",
        );
    }
}
