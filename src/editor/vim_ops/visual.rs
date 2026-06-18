//! VisualLine selection widening — the single helper shared by the render
//! path (`visual_line_mode` on `RenderedView`/`RawView`), the Visual-mode
//! operators, and the system `Copy`/`Cut` path, so they can never disagree
//! on what "the whole lines" means.  See `docs/vim-implementation-plan.md`
//! §2.6.
//!
//! The `selection` itself stays charwise (never snapped to whole lines);
//! these functions derive the line span on demand from the same rule
//! everywhere, so a `v`↔`V` toggle is lossless and the render and operator
//! expansions can never drift apart.

use std::ops::Range;

use crate::document::{Buffer, Selection};

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
}
