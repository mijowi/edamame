//! Visual-selection span widening — the single pair of helpers shared by the
//! render path (`visual_kind` on `RenderedView`/`RawView`), the Visual-mode
//! operators, and the system `Copy`/`Cut` path, so they can never disagree
//! on what "the selection" means.  See `docs/vim-implementation-plan.md`
//! §2.6.
//!
//! **`Selection` stays half-open everywhere; vim's semantics are derived,
//! never stored.**  The stored `selection` is the same `anchor`/`active`
//! pair the mouse and shift-arrow selections use, with `active` equal to
//! `cursor.offset` — so charwise inclusivity ([`visual_charwise_range`]) and
//! the whole-line expansion ([`visual_line_char_range`]) are both computed
//! on demand from the same rule at every call site.  That is what keeps a
//! `v`↔`V` toggle lossless and keeps the render and operator expansions from
//! drifting apart.  Don't "fix" it by snapping `active`: that field is shared
//! with the non-vim selection paths, which are genuinely half-open.

use std::ops::Range;

use crate::document::{next_grapheme_offset, Buffer, Selection};

/// Which flavor of Visual sub-mode a `selection` is being read under, so the
/// render path can pick the matching widening without depending on
/// `VimState`.  `None` (no vim, or vim outside Visual) means the raw
/// half-open span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualKind {
    /// Charwise `v` — inclusive of the character under the cursor.
    Char,
    /// Linewise `V` — expanded to whole lines.
    Line,
}

/// The char range a charwise Visual `selection` covers: the stored half-open
/// span with its *high* end extended over the character it sits on, matching
/// stock vim's inclusive Visual selection (so `v` alone covers one char and
/// `v l` covers two).
///
/// The extension is suppressed at a line end or end-of-buffer.  edamame's
/// vim cursor is an insertion point that can sit on the newline slot (`$`
/// resolves to `line_end_offset`), where vim's cursor could never be — the
/// half-open span already reaches past the last character there, so
/// extending would swallow the newline.  Together those two rules make `v$`
/// and `v` + `l`-to-the-end agree, both covering the line's content exactly.
///
/// Reverse selections need no special case: the rule applies to whichever
/// end is the high one, so the anchor's character is included when a motion
/// has carried the cursor behind it.
pub fn visual_charwise_range(sel: &Selection, buf: &Buffer) -> Range<usize> {
    let len = buf.len_chars();
    let (lo, hi) = sel.range();
    let (lo, hi) = (lo.min(len), hi.min(len));
    let end = if hi < len && buf.rope().char(hi) != '\n' {
        next_grapheme_offset(buf, hi)
    } else {
        hi
    };
    lo..end
}

/// The char range `sel` covers under `kind` — the one dispatcher every
/// consumer of a vim Visual selection should call, so the highlight, the
/// operators, and the clipboard can't disagree.  `None` yields the raw
/// half-open span (the non-vim selection paths).
pub fn visual_span(sel: &Selection, buf: &Buffer, kind: Option<VisualKind>) -> Range<usize> {
    match kind {
        Some(VisualKind::Char) => visual_charwise_range(sel, buf),
        Some(VisualKind::Line) => visual_line_char_range(sel, buf),
        None => {
            let (lo, hi) = sel.range();
            lo..hi
        }
    }
}

/// The inclusive buffer-line range a VisualLine `selection` covers: from the
/// line holding the selection start to the line holding its end.
pub fn visual_line_bounds(sel: &Selection, buf: &Buffer) -> (usize, usize) {
    let len = buf.len_chars();
    let (start, end) = sel.range();
    let first = buf.char_to_line(start.min(len));
    let last = buf.char_to_line(end.min(len));
    (first, last)
}

/// The char range a VisualLine `selection` expands to: the whole lines from
/// [`visual_line_bounds`], including the trailing newline of the last line
/// (or up to end-of-buffer on the final line).  Used by the render overlay
/// and the system clipboard copy/cut so the highlighted rows and the copied
/// text match exactly.
pub fn visual_line_char_range(sel: &Selection, buf: &Buffer) -> Range<usize> {
    let (first, last) = visual_line_bounds(sel, buf);
    let line_count = buf.line_count();
    let start = buf.line_to_char(first);
    let end = if last + 1 < line_count {
        buf.line_to_char(last + 1)
    } else {
        buf.len_chars()
    };
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

    fn sel(anchor: usize, active: usize) -> Selection {
        Selection { anchor, active }
    }

    #[test]
    fn bounds_span_the_touched_lines() {
        let b = buf("alpha\nbeta\ngamma\n");
        // A selection from the middle of line 0 to the middle of line 2.
        assert_eq!(visual_line_bounds(&sel(2, 13), &b), (0, 2));
        // Reversed (active before anchor) normalizes the same way.
        assert_eq!(visual_line_bounds(&sel(13, 2), &b), (0, 2));
    }

    #[test]
    fn char_range_covers_whole_lines_with_trailing_newline() {
        let b = buf("alpha\nbeta\ngamma\n");
        // Lines 0..=1 → "alpha\nbeta\n" → [0, 11).
        assert_eq!(visual_line_char_range(&sel(2, 7), &b), 0..11);
    }

    #[test]
    fn char_range_on_final_line_clamps_to_eof() {
        let b = buf("alpha\nbeta");
        // Line 1 has no trailing newline, so the range runs to EOF.
        assert_eq!(visual_line_char_range(&sel(7, 9), &b), 6..10);
    }

    #[test]
    fn charwise_range_includes_the_char_under_the_cursor() {
        let b = buf("abc\ndef");
        // `v` alone (anchor == active) covers exactly one char.
        assert_eq!(visual_charwise_range(&sel(0, 0), &b), 0..1);
        // `v l` covers two.
        assert_eq!(visual_charwise_range(&sel(0, 1), &b), 0..2);
    }

    #[test]
    fn charwise_range_stops_at_a_line_end() {
        let b = buf("abc\ndef");
        // Cursor on the last char: extended over it, up to but not past `\n`.
        assert_eq!(visual_charwise_range(&sel(0, 2), &b), 0..3);
        // Cursor on the newline slot (where `$` parks it): no extension, so
        // `v$` covers the same content span as walking `l` to the end.
        assert_eq!(visual_charwise_range(&sel(0, 3), &b), 0..3);
    }

    #[test]
    fn charwise_range_at_end_of_buffer_does_not_overrun() {
        let b = buf("abc");
        assert_eq!(visual_charwise_range(&sel(0, 3), &b), 0..3);
        // A stale offset past the end clamps rather than panicking.
        assert_eq!(visual_charwise_range(&sel(0, 9), &b), 0..3);
    }

    #[test]
    fn charwise_range_extends_the_high_end_of_a_reverse_selection() {
        let b = buf("abcd");
        // Anchor on 'c', cursor carried back to 'a': the anchor's char is the
        // high end and so is the one included.
        assert_eq!(visual_charwise_range(&sel(2, 0), &b), 0..3);
    }

    #[test]
    fn charwise_range_extends_by_a_whole_grapheme() {
        // A combining sequence is one cursor step, so one selection step.
        let b = buf("e\u{301}x");
        assert_eq!(visual_charwise_range(&sel(0, 0), &b), 0..2);
    }

    #[test]
    fn visual_span_dispatches_on_kind() {
        let b = buf("alpha\nbeta\n");
        let s = sel(0, 2);
        assert_eq!(visual_span(&s, &b, Some(VisualKind::Char)), 0..3);
        assert_eq!(visual_span(&s, &b, Some(VisualKind::Line)), 0..6);
        assert_eq!(visual_span(&s, &b, None), 0..2);
    }
}
