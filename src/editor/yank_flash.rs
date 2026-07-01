//! Post-yank selection "flash" — a brief highlight over the just-yanked
//! text so a `yy` / `yw` / visual `y` gives visible confirmation that the
//! copy landed, the way neovim's `vim.highlight.on_yank` does.
//!
//! The state is a byte range, the [`Instant`] it was armed, and the
//! `Buffer::version()` it was captured against.  It lives on
//! [`EditorState`] (not the App) because the overlay painter reads it off
//! `&EditorState` exactly like a search match, and because the yank that
//! arms it happens deep in the editor layer (`vim_ops::operator`).  The
//! App only contributes the expiry to `next_deadline` and clears it once
//! due (see `App::tick_timers`).
//!
//! The byte offsets are only valid for the buffer version they were
//! captured against, so — like [`crate::search::SearchState`] — the flash
//! is version-keyed: any mutation bumps the counter and
//! [`EditorState::active_yank_flash`] returns `None`, dismissing the flash
//! rather than letting the overlay painters slice a shifted, mid-char
//! span.

use std::time::{Duration, Instant};

use crate::editor::EditorState;

/// How long the yank highlight stays painted.  Matches neovim's default
/// `vim.highlight.on_yank { timeout = 150 }` — long enough to register as
/// a flash, short enough not to linger over the next keystroke.
pub const YANK_FLASH_DURATION: Duration = Duration::from_millis(150);

/// A pending yank-confirmation highlight: the yanked span in buffer bytes
/// and when it was armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YankFlash {
    /// Start byte offset of the yanked span in the live buffer.
    pub start: usize,
    /// End byte offset (exclusive) of the yanked span.
    pub end: usize,
    /// Instant the yank happened; the flash fades `YANK_FLASH_DURATION`
    /// after this.
    pub armed_at: Instant,
    /// `Buffer::version()` the span was captured against.  The byte
    /// offsets are only valid for this version — any subsequent mutation
    /// bumps the counter and invalidates the flash (see
    /// `active_yank_flash`), so the overlay painters can never slice a
    /// shifted, mid-char span.  Mirrors `SearchState`'s version-keying.
    pub armed_version: u64,
}

impl EditorState {
    /// Arm the post-yank flash over the char range `[start_char, end_char)`.
    /// Converts to byte offsets (the overlay painters work in bytes) and
    /// no-ops for an empty span so a zero-width yank leaves nothing behind.
    pub fn flash_yank(&mut self, start_char: usize, end_char: usize) {
        if start_char >= end_char {
            self.yank_flash = None;
            return;
        }
        let rope = self.buffer.rope();
        let len = rope.len_chars();
        let start = rope.char_to_byte(start_char.min(len));
        let end = rope.char_to_byte(end_char.min(len));
        if start >= end {
            self.yank_flash = None;
            return;
        }
        self.yank_flash = Some(YankFlash {
            start,
            end,
            armed_at: Instant::now(),
            armed_version: self.buffer.version(),
        });
    }

    /// The active flash range, or `None` once it has faded, was never
    /// armed, or the buffer has been mutated since it was armed.  Read by
    /// the overlay painters; returns `None` past the window (so a
    /// still-set-but-expired flash paints nothing) and on any version
    /// change (so the stale byte offsets can never slice a span that has
    /// since shifted — the painters would otherwise panic on a mid-char
    /// boundary).
    pub fn active_yank_flash(&self) -> Option<YankFlash> {
        let version = self.buffer.version();
        self.yank_flash
            .filter(|f| f.armed_version == version && f.armed_at.elapsed() < YANK_FLASH_DURATION)
    }

    /// Instant the current flash expires, for the run loop's
    /// `next_deadline` so the fade-out redraw fires without a keypress.
    pub fn yank_flash_deadline(&self) -> Option<Instant> {
        self.yank_flash.map(|f| f.armed_at + YANK_FLASH_DURATION)
    }

    /// Drop the flash once it is no longer active — its window has
    /// elapsed or the buffer was mutated since it was armed.  Returns
    /// `true` when it actually cleared something (so the caller can
    /// request a redraw).
    pub fn expire_yank_flash(&mut self) -> bool {
        if self.yank_flash.is_some() && self.active_yank_flash().is_none() {
            self.yank_flash = None;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::document::Buffer;
    use crate::editor::EditorState;

    fn editor(text: &str) -> EditorState {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        EditorState::new(Buffer::from_str(text), theme)
    }

    #[test]
    fn flash_yank_records_byte_range() {
        // "héllo": 'é' is two bytes, so char 2 == byte 3.
        let mut ed = editor("héllo");
        ed.flash_yank(0, 3);
        let f = ed.yank_flash.expect("flash armed");
        assert_eq!((f.start, f.end), (0, 4)); // chars 0..3 == bytes 0..4
        assert!(ed.active_yank_flash().is_some());
    }

    #[test]
    fn empty_span_arms_nothing() {
        let mut ed = editor("hello");
        ed.flash_yank(2, 2);
        assert!(ed.yank_flash.is_none());
    }

    #[test]
    fn out_of_range_chars_are_clamped() {
        let mut ed = editor("hi");
        ed.flash_yank(0, 999);
        let f = ed.yank_flash.expect("flash armed");
        assert_eq!((f.start, f.end), (0, 2));
    }

    #[test]
    fn buffer_mutation_invalidates_the_flash() {
        let mut ed = editor("hello");
        ed.flash_yank(0, 3);
        assert!(ed.active_yank_flash().is_some());
        // Any content mutation bumps `Buffer::version()`, so the flash's
        // stale byte offsets are no longer valid — active returns None and
        // the next tick clears it.
        ed.buffer.insert(0, "x");
        assert!(ed.active_yank_flash().is_none());
        assert!(ed.expire_yank_flash());
        assert!(ed.yank_flash.is_none());
    }

    #[test]
    fn expired_flash_is_inactive_and_clears() {
        let mut ed = editor("hello");
        ed.flash_yank(0, 3);
        // Force the arm time into the past so the window has elapsed.
        if let Some(f) = ed.yank_flash.as_mut() {
            f.armed_at = Instant::now() - YANK_FLASH_DURATION - Duration::from_millis(5);
        }
        assert!(ed.active_yank_flash().is_none());
        assert!(ed.yank_flash_deadline().is_some());
        assert!(ed.expire_yank_flash());
        assert!(ed.yank_flash.is_none());
        // A second expire is a no-op.
        assert!(!ed.expire_yank_flash());
    }
}
