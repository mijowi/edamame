//! Frame-rate / draw-throttle timing helpers extracted from `app.rs`
//! in Step 2 of `refactor-app.md`.
//!
//! Owns the wall-clock instants the run loop consults to decide when
//! to draw — `last_draw_at`, `last_scroll_at`, `resize_quiesce_at` —
//! plus the related quiesce / throttle constants and the
//! [`App::next_deadline`] aggregator.

use std::time::{Duration, Instant};

use crate::editor::RAW_REVEAL_DELAY;

use super::App;

/// After the scroll position stops changing for this long, images
/// upgrade from the halfblocks partial render back to the native
/// protocol.  Tuned so the upgrade feels "immediate" to a human but
/// never fires during continuous scroll input (typical wheel tick gap
/// is well under 50 ms).
pub(super) const SCROLL_QUIESCE: Duration = Duration::from_millis(150);

/// Minimum interval between successive `terminal.draw()` calls.  The
/// event loop processes events as fast as they arrive, but draws are
/// coalesced to at most one per this interval (~60 fps).  Under this
/// threshold, events still mutate state; the accumulated changes show
/// up on the next draw that actually fires.  Tuned so a wheel-tick
/// burst produces a handful of draws instead of one per tick.
pub(super) const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Grace window after a `Resize` event during which draws are
/// suppressed.  Dragging a terminal window's edge fires a burst of
/// Resize events — one per pixel.  Drawing on each one produces
/// flickery partial-width output and pins CPU; instead we wait for
/// the burst to settle and draw exactly once at the final size.
pub(super) const RESIZE_QUIESCE: Duration = Duration::from_millis(80);

/// Pure helper: true when `last_scroll_at` is `Some` and its elapsed
/// time is shorter than `quiesce`.  Extracted so tests can exercise it
/// without constructing a full `App`.
pub(super) fn is_scrolling_within(last_scroll_at: Option<Instant>, quiesce: Duration) -> bool {
    last_scroll_at.is_some_and(|t| t.elapsed() < quiesce)
}

impl App {
    /// Record that the scroll position has just changed; used by the
    /// image painter to decide whether to fall back to halfblocks
    /// partial rendering on non-Kitty terminals.
    pub(super) fn mark_scrolling(&mut self) {
        self.last_scroll_at = Some(Instant::now());
    }

    /// True when `mark_scrolling` has fired within `SCROLL_QUIESCE`.
    pub(super) fn is_scrolling(&self) -> bool {
        is_scrolling_within(self.last_scroll_at, SCROLL_QUIESCE)
    }

    /// Earliest wall-clock instant at which the event loop must wake
    /// up to apply a time-driven state change, even if no external
    /// event arrives.  Returns `None` when the loop can block
    /// indefinitely on `rx.recv()` — the common idle case.
    ///
    /// Only deadlines still in the future contribute.  Once a deadline
    /// has elapsed (and the post-elapse redraw has fired), it drops
    /// out of the computation so we can go back to blocking on input.
    ///
    /// Deadlines tracked:
    /// - `cursor_block_entered_at + RAW_REVEAL_DELAY` — wake to reveal
    ///   the raw cursor-block view when the jitter-suppression window
    ///   expires.
    /// - `last_scroll_at + SCROLL_QUIESCE` — wake to upgrade images
    ///   from halfblocks to the native graphics protocol once the
    ///   user stops scrolling.
    /// - `resize_quiesce_at` — wake to redraw once a terminal-resize
    ///   drag has settled (carries its own absolute deadline rather
    ///   than an offset, since it's set to `now + RESIZE_QUIESCE` on
    ///   each event).
    pub(super) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let mut earliest: Option<Instant> = None;
        let mut push = |candidate: Option<Instant>| {
            if let Some(c) = candidate.filter(|&c| c > now) {
                earliest = Some(earliest.map_or(c, |e: Instant| e.min(c)));
            }
        };
        push(
            self.editor
                .cursor_block_entered_at
                .map(|t| t + RAW_REVEAL_DELAY),
        );
        push(self.last_scroll_at.map(|t| t + SCROLL_QUIESCE));
        push(self.resize_quiesce_at);
        // Phase 9: wake in time to expire a transient hint-line
        // message so the hint reverts to chords even if the user
        // isn't typing.
        push(self.transient_deadline());
        push(self.editor.cursor_blink.next_toggle());
        // Autosave: wake when the idle-debounce window expires so the
        // save fires without the user having to press a key.
        push(self.autosave_deadline());
        // Section picker: wake when the live-preview debounce expires
        // so the viewport reposition happens even if the user has
        // stopped pressing arrow keys.
        push(self.section_jump_deadline());
        // Diff review: wake to auto-advance focus after the
        // post-decision reveal window elapses.
        push(self.diff_advance_deadline());
        earliest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_scrolling_is_false_when_never_scrolled() {
        assert!(!is_scrolling_within(None, SCROLL_QUIESCE));
    }

    #[test]
    fn is_scrolling_is_true_right_after_mark() {
        let now = Instant::now();
        assert!(is_scrolling_within(Some(now), SCROLL_QUIESCE));
    }

    #[test]
    fn is_scrolling_is_false_after_quiesce_elapsed() {
        // `Instant` can't be forged into the past directly; instead use
        // a tiny quiesce window and sleep past it.
        let now = Instant::now();
        std::thread::sleep(Duration::from_millis(20));
        assert!(!is_scrolling_within(Some(now), Duration::from_millis(5)));
    }

    #[test]
    fn is_scrolling_is_true_within_a_short_window() {
        // With a generous window, a just-marked timestamp is still
        // "scrolling".
        let now = Instant::now();
        assert!(is_scrolling_within(
            Some(now),
            Duration::from_millis(10_000)
        ));
    }
}
