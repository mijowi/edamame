//! Idle-debounce autosave: every dirtying edit resets a window of
//! `config.editor.autosave_idle_ms`; when it expires the buffer is written silently and
//! an `Autosaved` flash is shown.  Pathless buffers are skipped without UI; save failures
//! escalate to a sticky `NoticeModal` via [`App::notify`].  A file mixing line endings opens
//! the [`LineEndingModal`] first, as a manual save does.
//!
//! Driven from [`App::tick_timers`]; [`App::autosave_deadline`] feeds [`App::next_deadline`]
//! so the loop wakes exactly when the window expires rather than polling.

use std::time::{Duration, Instant};

use crate::ui::ModalKind;

use super::flash::MessageKind;
use super::modal::LineEndingModal;
use super::App;

impl App {
    /// Per-iteration autosave step.  Edits are detected via
    /// [`Buffer::version`](crate::document::Buffer::version), so every keystroke restarts
    /// the window.  Both outcomes already set `needs_draw`.
    pub(super) fn tick_autosave(&mut self) {
        // Saving mid-review would clobber the file being reconciled against; also drop the
        // armed timer so it can't fire the instant diff mode exits.
        if self.editor.mode == crate::editor::Mode::Diff {
            self.autosave_pending_since = None;
            return;
        }
        // A live `:s` preview rewrites the buffer through raw edits (version bumps, `dirty`
        // untouched); on an already-dirty buffer the preview text would reach disk.
        if self.editor.substitute_preview.is_some() {
            self.autosave_pending_since = None;
            return;
        }
        let enabled = self.config.editor.autosave_enabled;
        let version = self.editor.buffer.version();

        // Track the version even when disabled so re-enabling doesn't fire off a stale stamp.
        if version != self.autosave_last_seen_version {
            self.autosave_last_seen_version = version;
            if enabled && self.editor.dirty && self.editor.buffer.path().is_some() {
                self.autosave_pending_since = Some(Instant::now());
            }
        }

        if !self.editor.dirty {
            self.autosave_pending_since = None;
            return;
        }

        let Some(since) = self.autosave_pending_since else {
            return;
        };

        // Toggled off after arming: drop without writing (mirrors `autosave_deadline`).
        if !enabled {
            self.autosave_pending_since = None;
            return;
        }

        let idle = Duration::from_millis(self.config.editor.autosave_idle_ms);
        if since.elapsed() < idle {
            return;
        }

        // A mixed file asks which ending to use, as a manual save does.  Disarmed either way:
        // a choice saves through the continuation, and after `Esc` the next edit re-arms, so
        // the prompt returns at the next idle.  Never stacked on a prompt already open.
        if self.editor.buffer.needs_line_ending_choice().is_some() {
            self.autosave_pending_since = None;
            if !self.modal_stack.contains::<LineEndingModal>() {
                self.with_line_ending_choice_or("Autosave cancelled", Box::new(Self::autosave_now));
                self.needs_draw = true;
            }
            return;
        }
        self.autosave_now();
    }

    /// Write the buffer and report the outcome.
    fn autosave_now(&mut self) {
        match self.save_buffer() {
            Ok(()) => {
                self.autosave_pending_since = None;
                self.flash("Autosaved", MessageKind::Success);
            }
            Err(e) => {
                // Back off rather than retry every tick; the next edit re-arms.
                self.autosave_pending_since = None;
                tracing::warn!(error = %e, "autosave failed");
                self.notify(format!("Autosave failed: {e}"), ModalKind::Error);
            }
        }
    }

    /// When the run loop must wake to fire the pending autosave, if any.
    pub(super) fn autosave_deadline(&self) -> Option<Instant> {
        if self.editor.mode == crate::editor::Mode::Diff || self.editor.substitute_preview.is_some()
        {
            return None;
        }
        let since = self.autosave_pending_since?;
        if !self.config.editor.autosave_enabled {
            return None;
        }
        Some(since + Duration::from_millis(self.config.editor.autosave_idle_ms))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::make_app;
    use crate::config::Action;
    use crate::document::Buffer;
    use crate::editor::edit_ops;

    /// Enable autosave with a short window; returns it so callers can `sleep` past it.
    fn shrink_window(app: &mut App) -> Duration {
        app.config.editor.autosave_enabled = true;
        app.config.editor.autosave_idle_ms = 25;
        Duration::from_millis(25)
    }

    fn dirty_edit(app: &mut App) {
        let len = app.editor.buffer.len_chars();
        app.editor.buffer.insert_char(len, 'x');
        app.editor.dirty = true;
    }

    /// Edit, arm, and expire the window as if the user had gone idle.
    fn edit_then_idle(app: &mut App) {
        dirty_edit(app);
        app.tick_autosave(); // arm
        app.autosave_pending_since = Some(Instant::now() - Duration::from_secs(1));
        app.tick_autosave();
    }

    /// An app editing a freshly written mixed file (first break CRLF) with autosave on.
    fn app_with_mixed_file(dir: &std::path::Path) -> (App, std::path::PathBuf) {
        let path = dir.join("mixed.md");
        std::fs::write(&path, "a\r\nb\n").unwrap();
        let mut app = make_app();
        crate::app::test_utils::close_startup_modals(&mut app);
        app.editor.buffer = Buffer::load_file(&path).unwrap();
        shrink_window(&mut app);
        (app, path)
    }

    fn press(app: &mut App, code: KeyCode) {
        app.dispatch_modal_key(KeyEvent::new(code, KeyModifiers::NONE), 40, 80);
    }

    #[test]
    fn a_mixed_file_asks_once_per_idle_and_esc_waits_for_the_next_edit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, path) = app_with_mixed_file(dir.path());

        edit_then_idle(&mut app);
        assert!(app.modal_stack.contains::<LineEndingModal>());
        assert!(
            app.autosave_pending_since.is_none(),
            "disarmed while asking"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"a\r\nb\n");

        // Another expired window while the prompt is open stacks no second one.
        app.autosave_pending_since = Some(Instant::now() - Duration::from_secs(1));
        app.tick_autosave();
        assert_eq!(app.modal_stack.count::<LineEndingModal>(), 1);

        press(&mut app, KeyCode::Esc);
        assert!(app.modal_stack.is_empty());
        assert!(app.editor.dirty);
        assert_eq!(std::fs::read(&path).unwrap(), b"a\r\nb\n");
        let flash = app.transient.as_ref().map(|t| t.text.clone());
        assert_eq!(flash.as_deref(), Some("Autosave cancelled"));

        app.tick_autosave();
        assert!(
            app.modal_stack.is_empty(),
            "no edit since, so no new prompt"
        );

        edit_then_idle(&mut app);
        assert!(app.modal_stack.contains::<LineEndingModal>(), "asks again");
    }

    #[test]
    fn choosing_from_the_autosave_prompt_saves_and_later_autosaves_do_not_ask() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, path) = app_with_mixed_file(dir.path());

        edit_then_idle(&mut app);
        press(&mut app, KeyCode::Left); // focus starts on `Use CRLF`
        press(&mut app, KeyCode::Enter); // `Use LF`
        assert!(app.modal_stack.is_empty());
        assert!(!app.editor.dirty);
        assert_eq!(std::fs::read(&path).unwrap(), b"a\nb\nx");
        let flash = app.transient.as_ref().map(|t| t.text.clone());
        assert_eq!(flash.as_deref(), Some("Autosaved"));

        edit_then_idle(&mut app);
        assert!(app.modal_stack.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), b"a\nb\nxx");
    }

    #[test]
    fn fresh_app_with_no_dirty_buffer_is_a_noop() {
        let mut app = make_app();
        app.tick_autosave();
        assert!(app.autosave_pending_since.is_none());
    }

    #[test]
    fn dirtying_an_unnamed_buffer_does_not_arm_the_timer() {
        let mut app = make_app();
        assert!(app.editor.buffer.path().is_none());
        dirty_edit(&mut app);
        app.tick_autosave();
        assert!(
            app.autosave_pending_since.is_none(),
            "unnamed buffers must skip autosave silently"
        );
    }

    #[test]
    fn dirtying_a_named_buffer_arms_the_timer() {
        let mut app = make_app();
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        app.editor.buffer = Buffer::for_new_file(tmp.path());
        app.config.editor.autosave_enabled = true;
        dirty_edit(&mut app);
        app.tick_autosave();
        assert!(
            app.autosave_pending_since.is_some(),
            "dirtying a named buffer must arm the debounce timer"
        );
    }

    #[test]
    fn autosave_fires_after_idle_window_elapses() {
        let mut app = make_app();
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        let path = tmp.path().to_owned();
        app.editor.buffer = Buffer::for_new_file(&path);
        let window = shrink_window(&mut app);
        dirty_edit(&mut app);
        app.tick_autosave(); // arm
        assert!(app.editor.dirty);
        std::thread::sleep(window + Duration::from_millis(20));
        app.tick_autosave();
        assert!(!app.editor.dirty, "buffer must be clean after autosave");
        assert!(app.autosave_pending_since.is_none());
        let on_disk = std::fs::read_to_string(&path).expect("read back");
        assert!(
            on_disk.ends_with('x'),
            "autosaved contents must reach the file"
        );
        let msg = app.transient.as_ref().expect("Autosaved flash recorded");
        assert_eq!(msg.text, "Autosaved");
        assert!(matches!(msg.kind, MessageKind::Success));
    }

    #[test]
    fn fresh_edit_restarts_the_debounce_window() {
        let mut app = make_app();
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        app.editor.buffer = Buffer::for_new_file(tmp.path());
        let window = shrink_window(&mut app);
        dirty_edit(&mut app);
        app.tick_autosave();
        let first = app.autosave_pending_since.expect("armed");
        std::thread::sleep(window / 3);
        dirty_edit(&mut app);
        app.tick_autosave();
        let second = app.autosave_pending_since.expect("still armed");
        assert!(
            second > first,
            "follow-up edit must push the debounce window forward"
        );
        assert!(app.editor.dirty, "no autosave should have fired yet");
    }

    #[test]
    fn disabling_autosave_skips_the_save() {
        let mut app = make_app();
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        app.editor.buffer = Buffer::for_new_file(tmp.path());
        let window = shrink_window(&mut app);
        app.config.editor.autosave_enabled = false;
        dirty_edit(&mut app);
        app.tick_autosave();
        assert!(
            app.autosave_pending_since.is_none(),
            "disabled autosave must not arm the timer"
        );
        std::thread::sleep(window + Duration::from_millis(20));
        app.tick_autosave();
        assert!(app.editor.dirty, "buffer must remain dirty when disabled");
    }

    #[test]
    fn disabling_autosave_after_arming_cancels_pending_save() {
        // Regression: the `enabled` flag was once consulted only on the arm branch.
        let mut app = make_app();
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        app.editor.buffer = Buffer::for_new_file(tmp.path());
        let window = shrink_window(&mut app);
        dirty_edit(&mut app);
        app.tick_autosave();
        assert!(app.autosave_pending_since.is_some(), "armed");
        app.config.editor.autosave_enabled = false;
        std::thread::sleep(window + Duration::from_millis(20));
        app.tick_autosave();
        assert!(
            app.autosave_pending_since.is_none(),
            "pending timer must be cleared when autosave is disabled"
        );
        assert!(
            app.editor.dirty,
            "buffer must remain dirty when autosave was disabled before firing"
        );
    }

    #[test]
    fn real_edit_dispatch_arms_the_autosave_timer() {
        // Unlike the others, this drives the real edit path to pin that `dirty` and
        // `Buffer::version()` move in lockstep.
        let mut app = make_app();
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        app.editor.buffer = Buffer::for_new_file(tmp.path());
        app.config.editor.autosave_enabled = true;
        app.editor.mode = crate::editor::Mode::Rendered;
        let version_before = app.editor.buffer.version();
        edit_ops::apply(&mut app.editor, Action::InsertChar('a'), 24, 80);
        assert!(
            app.editor.dirty,
            "edit_ops::apply(InsertChar) must set dirty"
        );
        assert_ne!(
            app.editor.buffer.version(),
            version_before,
            "edit_ops::apply(InsertChar) must bump Buffer::version()"
        );
        app.tick_autosave();
        assert!(
            app.autosave_pending_since.is_some(),
            "tick_autosave must arm the debounce timer after a real edit",
        );
    }

    #[test]
    fn diff_mode_clears_pending_autosave_and_skips() {
        let mut app = make_app();
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        app.editor.buffer = Buffer::for_new_file(tmp.path());
        let window = shrink_window(&mut app);
        dirty_edit(&mut app);
        app.tick_autosave();
        assert!(app.autosave_pending_since.is_some(), "armed");
        app.editor.mode = crate::editor::Mode::Diff;
        std::thread::sleep(window + Duration::from_millis(20));
        app.tick_autosave();
        assert!(
            app.autosave_pending_since.is_none(),
            "diff mode must clear the armed timer",
        );
        assert!(
            app.editor.dirty,
            "buffer must remain dirty (no save fired in diff mode)",
        );
        assert!(
            app.autosave_deadline().is_none(),
            "deadline suppressed in diff mode"
        );
    }

    #[test]
    fn live_substitute_preview_suspends_autosave() {
        let mut app = make_app();
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        let path = tmp.path().to_owned();
        app.editor.buffer = Buffer::for_new_file(&path);
        let window = shrink_window(&mut app);
        dirty_edit(&mut app); // appends 'x'
        app.tick_autosave();
        assert!(app.autosave_pending_since.is_some(), "armed");
        crate::editor::vim_ops::update_substitute_preview(&mut app.editor, "%s/x/y/", None, 24, 80);
        assert!(app.editor.substitute_preview.is_some(), "preview active");
        std::thread::sleep(window + Duration::from_millis(20));
        app.tick_autosave();
        assert!(
            app.autosave_pending_since.is_none(),
            "an active preview must clear the armed timer"
        );
        assert!(app.editor.dirty, "no save fired during the preview");
        assert!(
            app.autosave_deadline().is_none(),
            "deadline suppressed during the preview"
        );
        let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !on_disk.contains('y'),
            "preview text must never reach the disk"
        );
    }

    #[test]
    fn deadline_is_none_when_no_edit_pending() {
        let app = make_app();
        assert!(app.autosave_deadline().is_none());
    }

    #[test]
    fn deadline_is_none_when_disabled_even_if_armed() {
        let mut app = make_app();
        app.autosave_pending_since = Some(Instant::now());
        app.config.editor.autosave_enabled = false;
        assert!(app.autosave_deadline().is_none());
    }
}
