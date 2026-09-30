//! Draw-throttle timing: the quiesce / throttle constants and the [`App::next_deadline`]
//! aggregator the run loop uses to decide when to wake and draw.

use std::time::{Duration, Instant};

use crate::editor::RAW_REVEAL_DELAY;
use crate::terminal::ImageProtocol;

use super::App;

/// After scrolling stops for this long, images upgrade from halfblocks back to the native
/// protocol.  Must exceed the typical wheel-tick gap (well under 50 ms).
pub(super) const SCROLL_QUIESCE: Duration = Duration::from_millis(150);

/// [`SCROLL_QUIESCE`] on a Sixel terminal, which has no image store: every upgrade re-sends each
/// visible image in full, measured at 1.3-2.2 MB and ~300-450 ms of terminal time in foot
/// (the write, plus the next frame's write stalling while foot finishes decoding).  No input is
/// read meanwhile, so a scroll resumed inside that window is applied late, in one jump.  The
/// longer wait keeps a slow scroll's short pauses from each paying that cost.
pub(super) const SIXEL_SCROLL_QUIESCE: Duration = Duration::from_millis(400);

/// Minimum interval between `terminal.draw()` calls (~60 fps); events still mutate state
/// in between and show up on the next draw.
pub(super) const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Draws are suppressed for this long after a `Resize` so an edge drag (one event per
/// pixel) settles into a single draw at the final size.
pub(super) const RESIZE_QUIESCE: Duration = Duration::from_millis(80);

/// Pure form of [`App::is_scrolling`], testable without an `App`.
pub(super) fn is_scrolling_within(last_scroll_at: Option<Instant>, quiesce: Duration) -> bool {
    last_scroll_at.is_some_and(|t| t.elapsed() < quiesce)
}

impl App {
    /// Record a scroll; the image painter falls back to halfblocks while scrolling.
    pub(super) fn mark_scrolling(&mut self) {
        self.last_scroll_at = Some(Instant::now());
        self.settle_frame_pending = true;
    }

    /// True when `mark_scrolling` has fired within the protocol's quiesce window.
    pub(super) fn is_scrolling(&self) -> bool {
        is_scrolling_within(self.last_scroll_at, self.scroll_quiesce())
    }

    /// How long scrolling must stop before images upgrade back to the native protocol.
    fn scroll_quiesce(&self) -> Duration {
        match self.capabilities.image_protocol {
            Some(ImageProtocol::Sixel) => SIXEL_SCROLL_QUIESCE,
            _ => SCROLL_QUIESCE,
        }
    }

    /// Earliest instant the event loop must wake to apply a time-driven change, or `None`
    /// when it can block indefinitely on input.  Only deadlines still in the future
    /// contribute, so an elapsed one drops out after its redraw fires.
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
        push(self.last_scroll_at.map(|t| t + self.scroll_quiesce()));
        push(self.resize_quiesce_at);
        push(self.transient_deadline());
        push(self.editor.cursor_blink.next_toggle());
        push(self.editor.yank_flash_deadline());
        push(self.autosave_deadline());
        // Figure render debounce: wake when the window expires so the deferred render
        // dispatches after the user stops typing, even with no key event of its own.
        push(self.diagram_render_hold_until);
        push(self.images_prompt_due);
        push(self.section_jump_deadline());
        push(self.diff_advance_deadline());
        push(self.search_advance_deadline());
        push(self.modal_stack.next_deadline());
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
        let now = Instant::now();
        std::thread::sleep(Duration::from_millis(20));
        assert!(!is_scrolling_within(Some(now), Duration::from_millis(5)));
    }

    #[test]
    fn is_scrolling_is_true_within_a_short_window() {
        let now = Instant::now();
        assert!(is_scrolling_within(
            Some(now),
            Duration::from_millis(10_000)
        ));
    }

    #[test]
    fn sixel_waits_longer_before_upgrading() {
        let mut app = crate::app::test_utils::make_app();
        assert_eq!(app.scroll_quiesce(), SCROLL_QUIESCE);
        app.capabilities.image_protocol = Some(ImageProtocol::Sixel);
        assert_eq!(app.scroll_quiesce(), SIXEL_SCROLL_QUIESCE);
    }
}
