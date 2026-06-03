//! Deferred post-decision focus advance for diff review.
//!
//! When the user accepts or rejects a hunk we want them to *see* the
//! decision land — the checkbox flipping to `[✓] Accepted` / `[x]
//! Rejected` and the hunk's color settling — before focus jumps to the
//! next pending hunk.  So the accept/reject handlers set the decision
//! and arm a short timer here instead of advancing immediately; once
//! [`DIFF_ADVANCE_DELAY`] elapses the run loop advances focus.
//!
//! The shape mirrors [`super::section_jump`]: an `Option<Instant>` on
//! `App`, contributed to [`App::next_deadline`] via
//! [`App::diff_advance_deadline`] and drained in
//! [`App::tick_diff_advance`] (called from `tick_timers`).  Rapid
//! tapping flushes the pending advance first (see
//! [`App::apply_diff_advance`]) so power users aren't gated by the
//! delay.

use std::time::{Duration, Instant};

use super::App;

/// How long a resolved hunk stays focused before focus auto-advances.
/// Long enough to register the checkbox + color change, short enough
/// not to feel sluggish when reviewing many hunks — a few × the
/// `RAW_REVEAL_DELAY` jitter window.
pub(super) const DIFF_ADVANCE_DELAY: Duration = Duration::from_millis(350);

impl App {
    /// Arm (or re-arm) the post-decision advance timer.  Called by the
    /// accept/reject handlers right after recording the decision.
    pub(crate) fn arm_diff_advance(&mut self) {
        self.diff_advance_pending_since = Some(Instant::now());
    }

    /// Clear any pending advance without performing it.  Used when the
    /// user takes manual control (hunk navigation, exit, accept-all /
    /// reject-all) so a mid-flight timer doesn't fire afterward.
    pub(crate) fn cancel_diff_advance(&mut self) {
        self.diff_advance_pending_since = None;
    }

    /// Perform the deferred advance now: move focus to the next pending
    /// hunk, request a scroll-into-view, and re-check whether the diff
    /// is fully resolved (which pops the confirm modal).  Clearing the
    /// timer is unconditional so callers can use this both as the
    /// timer's action and as a flush before applying a fresh decision.
    pub(crate) fn apply_diff_advance(&mut self) {
        self.diff_advance_pending_since = None;
        if let Some(d) = self.editor.diff.as_mut() {
            d.advance_to_next_pending();
            // The viewport height isn't known here; defer the scroll to
            // the next `prepare_viewport` like the diff-entry path does.
            self.editor.pending_focus_scroll = true;
            self.needs_draw = true;
        }
        self.check_diff_resolution();
    }

    /// Per-iteration step: once the reveal window has elapsed, advance
    /// focus.  No-op while nothing is pending or the window is still
    /// open.
    pub(super) fn tick_diff_advance(&mut self) {
        let Some(since) = self.diff_advance_pending_since else {
            return;
        };
        if since.elapsed() < DIFF_ADVANCE_DELAY {
            return;
        }
        self.apply_diff_advance();
    }

    /// Earliest instant the run loop must wake to fire a pending
    /// advance.  Contributed to [`App::next_deadline`] so `recv_timeout`
    /// wakes exactly when the window expires — no polling.
    pub(super) fn diff_advance_deadline(&self) -> Option<Instant> {
        self.diff_advance_pending_since
            .map(|t| t + DIFF_ADVANCE_DELAY)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::app::test_utils::make_app;
    use crate::diff::{Decision, DiffState};

    /// Build an app in diff mode with three single-line replace hunks,
    /// all pending, focused on the first.
    fn app_in_diff() -> App {
        let mut app = make_app();
        let old = "a\nb\nc\nd\ne\n";
        let new = "A\nb\nC\nd\nE\n"; // hunks at lines 0, 2, 4
        let diff = DiffState::new(old, new).expect("non-empty diff");
        assert_eq!(diff.hunks.len(), 3);
        app.editor.enter_diff_mode(diff);
        app
    }

    #[test]
    fn decision_defers_advance_until_window_elapses() {
        let mut app = app_in_diff();
        let first = app.editor.diff.as_ref().unwrap().focused_id;

        // Record a decision the way the action handler does.
        app.editor
            .diff
            .as_mut()
            .unwrap()
            .decide_focused(Decision::Accepted);
        app.arm_diff_advance();

        // Focus has NOT moved yet — the user still sees the resolved hunk.
        assert_eq!(app.editor.diff.as_ref().unwrap().focused_id, first);
        assert!(app.diff_advance_pending_since.is_some());

        // Before the window: tick is a no-op.
        app.tick_diff_advance();
        assert_eq!(app.editor.diff.as_ref().unwrap().focused_id, first);

        // Force the window open; the tick advances to the next pending hunk.
        app.diff_advance_pending_since =
            Some(Instant::now() - DIFF_ADVANCE_DELAY - Duration::from_millis(5));
        app.tick_diff_advance();
        assert_ne!(app.editor.diff.as_ref().unwrap().focused_id, first);
        assert!(app.diff_advance_pending_since.is_none());
        assert!(app.editor.pending_focus_scroll);
    }

    #[test]
    fn cancel_drops_pending_advance() {
        let mut app = app_in_diff();
        let first = app.editor.diff.as_ref().unwrap().focused_id;

        // Decide and arm, then force the window fully open so a live
        // timer *would* fire on the next tick.
        app.editor
            .diff
            .as_mut()
            .unwrap()
            .decide_focused(Decision::Accepted);
        app.arm_diff_advance();
        app.diff_advance_pending_since =
            Some(Instant::now() - DIFF_ADVANCE_DELAY - Duration::from_millis(5));

        // Cancelling clears the timer, so the (otherwise-due) tick is a
        // no-op and focus stays put.
        app.cancel_diff_advance();
        assert!(app.diff_advance_pending_since.is_none());
        app.tick_diff_advance();
        assert_eq!(app.editor.diff.as_ref().unwrap().focused_id, first);
    }

    #[test]
    fn resolving_last_hunk_pops_confirm_modal_after_delay() {
        let mut app = app_in_diff();
        // Resolve all three hunks; only the deferred advance should
        // trigger the confirm modal, and only after the window.
        for _ in 0..3 {
            app.editor
                .diff
                .as_mut()
                .unwrap()
                .decide_focused(Decision::Accepted);
            app.diff_advance_pending_since =
                Some(Instant::now() - DIFF_ADVANCE_DELAY - Duration::from_millis(5));
            app.tick_diff_advance();
        }
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::DiffResolveConfirmModal>());
    }
}
