//! Vim search-query construction.
//!
//! The only entry point so far is [`word_under_cursor_at`], which extracts
//! the keyword under (or next on the line after) the cursor for `*` / `#`.
//! `vim_feed` turns the returned word into a literal search query handed
//! to the base search feature (smartcase lives there, not here).

use crate::document::Buffer;

use super::motion::{class, Class};

/// The keyword under the cursor — or, if the cursor is not on one, the
/// next keyword on the same line — for `*` / `#`.  Returns the keyword's
/// **start** char offset together with its literal text, or `None` when
/// the line has no keyword at or after the cursor.
///
/// A keyword is a run of [`Class::Word`] chars (alphanumeric + `_`),
/// matching vim's `iw` object and the `w`/`e`/`b` word class; the scan
/// never crosses a newline.  The match is literal substring text — `*`
/// does not add `\<word\>` boundaries, since the base search is
/// literal-substring, not regex.
///
/// `*` / `#` reposition the cursor to the returned start before searching
/// — vim's behavior — so a backward `#` from the middle of an occurrence
/// lands on the *previous* occurrence rather than snapping to the start of
/// the current one.
pub fn word_under_cursor_at(buf: &Buffer, cursor: usize) -> Option<(usize, String)> {
    let len = buf.len_chars();
    if len == 0 {
        return None;
    }
    let rope = buf.rope();
    let line = buf.char_to_line(cursor.min(len.saturating_sub(1)));
    let line_start = buf.line_to_char(line);
    // The line end is the first newline at/after the line start (or EOF).
    let mut line_end = line_start;
    while line_end < len && rope.char(line_end) != '\n' {
        line_end += 1;
    }

    // Find the first keyword char at or after the cursor, within the line.
    let mut pos = cursor.clamp(line_start, line_end);
    while pos < line_end && class(rope.char(pos), false) != Class::Word {
        pos += 1;
    }
    if pos >= line_end {
        return None;
    }

    // Expand to the full keyword run around `pos`.
    let mut start = pos;
    while start > line_start && class(rope.char(start - 1), false) == Class::Word {
        start -= 1;
    }
    let mut end = pos;
    while end < line_end && class(rope.char(end), false) == Class::Word {
        end += 1;
    }
    Some((start, buf.slice_to_string(start, end)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

    /// The keyword text, dropping the start offset, for the text-only assertions.
    fn word(b: &Buffer, cursor: usize) -> Option<String> {
        word_under_cursor_at(b, cursor).map(|(_, t)| t)
    }

    #[test]
    fn word_under_cursor_returns_the_keyword_the_cursor_is_in() {
        let b = buf("foo bar baz");
        assert_eq!(word(&b, 0).as_deref(), Some("foo"));
        // Mid-word still yields the whole word.
        assert_eq!(word(&b, 5).as_deref(), Some("bar"));
        assert_eq!(word(&b, 10).as_deref(), Some("baz"));
    }

    #[test]
    fn word_under_cursor_reports_the_word_start_offset() {
        let b = buf("foo bar baz");
        // From the middle of "bar" (offset 5) the start is 4, not 5 — the
        // fix that makes `#` jump to the previous occurrence.
        assert_eq!(word_under_cursor_at(&b, 5), Some((4, "bar".to_owned())));
        // Skipping forward over the space reports the next word's start.
        assert_eq!(word_under_cursor_at(&b, 3), Some((4, "bar".to_owned())));
    }

    #[test]
    fn word_under_cursor_skips_forward_to_the_next_keyword() {
        // Cursor on the space → next word on the line.
        let b = buf("a   word");
        assert_eq!(word(&b, 1).as_deref(), Some("word"));
    }

    #[test]
    fn word_under_cursor_does_not_cross_a_newline() {
        let b = buf("end\nnext");
        // Cursor past the last keyword char on its line → no word.
        assert_eq!(word(&b, 3), None);
    }

    #[test]
    fn word_under_cursor_includes_underscores_and_digits() {
        let b = buf("call foo_bar2 now");
        assert_eq!(word(&b, 5).as_deref(), Some("foo_bar2"));
    }

    #[test]
    fn word_under_cursor_is_none_on_empty_buffer() {
        assert_eq!(word_under_cursor_at(&buf(""), 0), None);
    }
}
