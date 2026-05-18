//! Idle-debounce autosave.
//!
//! Every dirtying edit resets a debounce window; once the user stops
//! typing for `config.editor.autosave_idle_ms`, the dirty buffer is
//! written to disk (no dialog, no confirmation) and an `Autosaved`
//! transient is flashed on the hint line.  Buffers without an
//! associated path skip without any UI — the user named no file, so
//! there's nowhere to save.  Save failures escalate to a sticky
//! `NoticeModal` via [`App::notify`] so the user can't miss them.
//!
//! The run loop drives this from [`App::tick_timers`] (once per
//! iteration) and consults [`App::autosave_deadline`] in
//! [`App::next_deadline`] so the channel `recv_timeout` wakes exactly
//! when the debounce window expires — no polling.

use std::time::{Duration, Instant};

use crate::ui::ModalKind;

use super::flash::MessageKind;
use super::App;

impl App {
    /// Per-iteration autosave step.  Detects edits via
    /// [`Buffer::version`](crate::document::Buffer::version), so a
    /// typing burst restarts the debounce window on every keystroke
    /// rather than firing mid-burst.  No return value: both the
    /// success path ([`App::flash`]) and the failure path
    /// ([`App::notify`]) already set `needs_draw`, so the caller in
    /// `tick_timers` doesn't need to.
    pub(super) fn tick_autosave(&mut self) {
        let enabled = self.config.editor.autosave_enabled;
        let version = self.editor.buffer.version();

        // Detect edits: any version change is an edit.  Reset the
        // debounce window even when autosave is disabled so re-enabling
        // it later doesn't fire instantly off a stale timestamp.
        if version != self.autosave_last_seen_version {
            self.autosave_last_seen_version = version;
            if enabled && self.editor.dirty && self.editor.buffer.path().is_some() {
                self.autosave_pending_since = Some(Instant::now());
            }
        }

        // Clean buffer (e.g. manual Save) — clear any pending timer.
        if !self.editor.dirty {
            self.autosave_pending_since = None;
            return;
        }

        // No pending timer (autosave disabled, unnamed buffer, or
        // freshly-cleared) — nothing to do this tick.
        let Some(since) = self.autosave_pending_since else {
            return;
        };

        // Autosave was toggled off after the timer was armed: drop the
        // pending save without writing.  Re-enabling later will re-arm
        // on the next edit.  Symmetric with `autosave_deadline`, which
        // already returns `None` when disabled.
        if !enabled {
            self.autosave_pending_since = None;
            return;
        }

        let idle = Duration::from_millis(self.config.editor.autosave_idle_ms);
        if since.elapsed() < idle {
            return;
        }

        // Window elapsed: persist.
        match self.editor.buffer.save_file() {
            Ok(()) => {
                self.editor.dirty = false;
                self.autosave_pending_since = None;
                self.flash("Autosaved", MessageKind::Success);
            }
            Err(e) => {
                // Back off so we don't hammer a failing write every tick.
                // The user's edits stay in the in-memory buffer; the
                // next edit re-arms the timer for another attempt.
                self.autosave_pending_since = None;
                tracing::warn!(error = %e, "autosave failed");
                self.notify(format!("Autosave failed: {e}"), ModalKind::Error);
            }
        }
    }

    /// The instant at which the run loop must wake to fire the pending
    /// autosave, if any.  Contributes to [`App::next_deadline`] so the
    /// `recv_timeout` blocks exactly long enough — no idle CPU.
    pub(super) fn autosave_deadline(&self) -> Option<Instant> {
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

    use super::*;
    use crate::app::test_utils::make_app;
    use crate::config::Action;
    use crate::document::Buffer;
    use crate::editor::edit_ops;

    /// Force the autosave debounce window to a known short value so
    /// tests can advance past it with `sleep`.  Returns the configured
    /// duration so callers can `sleep` for it + a small jitter.
    fn shrink_window(app: &mut App) -> Duration {
        app.config.editor.autosave_idle_ms = 25;
        Duration::from_millis(25)
    }

    fn dirty_edit(app: &mut App) {
        // Bump the version directly via a buffer edit so the test
        // doesn't depend on the action dispatch path.
        let len = app.editor.buffer.len_chars();
        app.editor.buffer.insert_char(len, 'x');
        app.editor.dirty = true;
    }

    #[test]
    fn fresh_app_with_no_dirty_buffer_is_a_noop() {
        let mut app = make_app();
        app.tick_autosave();
        assert!(app.autosave_pending_since.is_none());
    }

    #[test]
    fn dirtying_an_unnamed_buffer_does_not_arm_the_timer() {
        // `make_app` builds an app with no path; autosave must silently
        // skip until a path is associated.
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
        // Swap in a buffer associated with a temp path so autosave is
        // eligible.  Use `tempfile` to keep the test hermetic.
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        app.editor.buffer = Buffer::for_new_file(tmp.path());
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
        // Sleep less than the window, then dirty again — the timer must
        // restart, not fire.
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
        // Regression: previously `tick_autosave` only consulted the
        // `enabled` flag on the arm branch.  If the user disabled
        // autosave after the timer was already armed, any tick after
        // the window elapsed would still call `save_file()`.
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
        // The other tests poke `buffer.insert_char` + `editor.dirty`
        // directly to keep the autosave logic in isolation.  This one
        // exercises the *real* edit path (`edit_ops::apply` with an
        // `InsertChar` action) to verify that `dirty` and
        // `Buffer::version()` move in lockstep — otherwise autosave
        // would silently fail to arm in production.
        let mut app = make_app();
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        app.editor.buffer = Buffer::for_new_file(tmp.path());
        // Move out of Preview so InsertChar actually types instead of
        // just switching modes.
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
    fn deadline_is_none_when_no_edit_pending() {
        let app = make_app();
        assert!(app.autosave_deadline().is_none());
    }

    #[test]
    fn deadline_is_none_when_disabled_even_if_armed() {
        // Pathological state: timer was armed when autosave was on,
        // then the user toggled the setting off.  The deadline must
        // not contribute to next_deadline anymore.
        let mut app = make_app();
        app.autosave_pending_since = Some(Instant::now());
        app.config.editor.autosave_enabled = false;
        assert!(app.autosave_deadline().is_none());
    }
}
