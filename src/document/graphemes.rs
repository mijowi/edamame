//! Grapheme-cluster boundary helpers over `Buffer`.
//!
//! `Cursor::offset` is a Rust `char` (Unicode scalar value) index into the
//! rope.  But for navigation and editing we want to step over user-perceived
//! characters — *grapheme clusters* — so flag emoji, skin-tone modifiers,
//! ZWJ sequences, and combining marks behave as one keystroke per character
//! to the user.  These helpers convert "the next grapheme from here" into
//! a char offset the buffer can index with.
//!
//! Implementation: read a short windowed slice from the rope around the
//! query offset and let `unicode_segmentation` segment it.  The window is
//! generously sized (32 chars) to cover every standardized grapheme cluster
//! including long ZWJ sequences (e.g. family emoji).

use unicode_segmentation::UnicodeSegmentation;

use crate::document::Buffer;

/// Maximum chars we read on either side of the query offset.  Grapheme
/// clusters in practice top out around 7 chars (family emoji
/// `👨‍👩‍👧‍👦`); 32 leaves comfortable headroom for any reasonable
/// extension without paying for a full-rope materialization.
const GRAPHEME_WINDOW: usize = 32;

/// Char offset of the grapheme-cluster boundary that follows `char_offset`.
///
/// Returns `buf.len_chars()` when already at end of buffer.  When
/// `char_offset` is itself in the middle of a grapheme (rare — shouldn't
/// happen if all cursor moves go through these helpers), returns the end
/// of that containing grapheme, which is the natural "step forward" answer.
pub fn next_grapheme_offset(buf: &Buffer, char_offset: usize) -> usize {
    let len = buf.len_chars();
    if char_offset >= len {
        return len;
    }
    let end = (char_offset + GRAPHEME_WINDOW).min(len);
    let s = buf.rope().slice(char_offset..end).to_string();
    match s.graphemes(true).next() {
        Some(g) => char_offset + g.chars().count(),
        None => char_offset,
    }
}

/// Char offset of the grapheme-cluster boundary that precedes `char_offset`.
///
/// Returns `0` when already at the start of the buffer.
pub fn prev_grapheme_offset(buf: &Buffer, char_offset: usize) -> usize {
    if char_offset == 0 {
        return 0;
    }
    let start = char_offset.saturating_sub(GRAPHEME_WINDOW);
    let s = buf.rope().slice(start..char_offset).to_string();
    match s.graphemes(true).next_back() {
        Some(g) => char_offset - g.chars().count(),
        None => char_offset.saturating_sub(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

    #[test]
    fn next_steps_over_ascii_one_char_at_a_time() {
        let buf = b("hi");
        assert_eq!(next_grapheme_offset(&buf, 0), 1);
        assert_eq!(next_grapheme_offset(&buf, 1), 2);
        assert_eq!(next_grapheme_offset(&buf, 2), 2); // EOF
    }

    #[test]
    fn prev_steps_over_ascii_one_char_at_a_time() {
        let buf = b("hi");
        assert_eq!(prev_grapheme_offset(&buf, 2), 1);
        assert_eq!(prev_grapheme_offset(&buf, 1), 0);
        assert_eq!(prev_grapheme_offset(&buf, 0), 0); // BOF
    }

    #[test]
    fn next_treats_single_codepoint_emoji_as_one_step() {
        // 🥇 is one Rust char and one grapheme — same step in both views.
        let buf = b("🥇");
        assert_eq!(next_grapheme_offset(&buf, 0), 1);
        assert_eq!(prev_grapheme_offset(&buf, 1), 0);
    }

    #[test]
    fn next_treats_zwj_family_as_one_grapheme() {
        // 👨‍👩‍👧‍👦 = 7 chars (man + ZWJ + woman + ZWJ + girl + ZWJ + boy).
        let buf = b("👨\u{200D}👩\u{200D}👧\u{200D}👦x");
        assert_eq!(buf.len_chars(), 8);
        assert_eq!(next_grapheme_offset(&buf, 0), 7);
        assert_eq!(prev_grapheme_offset(&buf, 7), 0);
        // Step over the trailing 'x' as its own grapheme.
        assert_eq!(next_grapheme_offset(&buf, 7), 8);
    }

    #[test]
    fn next_treats_combining_mark_as_single_grapheme() {
        // 'e' + U+0301 (combining acute) = "é" as 2 chars / 1 grapheme.
        let buf = b("e\u{0301}!");
        assert_eq!(buf.len_chars(), 3);
        assert_eq!(next_grapheme_offset(&buf, 0), 2);
        assert_eq!(prev_grapheme_offset(&buf, 2), 0);
    }

    #[test]
    fn next_treats_skin_tone_modifier_as_one_grapheme() {
        // 👍 + 🏽 = 2 chars / 1 grapheme.
        let buf = b("👍\u{1F3FD}!");
        assert_eq!(next_grapheme_offset(&buf, 0), 2);
        assert_eq!(prev_grapheme_offset(&buf, 2), 0);
    }
}
