//! Shared text-field cursor helper for editor views and modal inputs.
//!
//! Every cursor is *fake* (a styled cell, never the hardware cursor): the hardware cursor shows
//! one position and its color can't be set portably (OSC 12 is unsupported by kitty and
//! mis-restores in VTE).  The cursor is always a block; context is signaled by color
//! (see `docs/dev/theming.md`).
//!
//! The block recolors the grapheme cluster under the cursor ([`text_field_spans`]), so it is
//! always that cluster's cells, or one blank cell past the end, and the field never jitters on
//! blink.  A field that can outgrow its width goes through [`scrolled_field_spans`], which
//! windows the value around the cursor and pads the field out to its full width.
//!
//! Cursors are char indices, but they step and delete by grapheme cluster through the `str_*`
//! helpers in [`crate::document::graphemes`], as the editor's own cursor does, so a cursor never
//! stops inside a ZWJ emoji sequence or on a combining mark.

use ratatui::style::Style;
use ratatui::text::Span;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::document::str_byte_index;

/// The three spans of a single-line field value with a blink-stable block cursor at char index
/// `cursor`.  The middle span is the grapheme cluster under the cursor (a space past the end),
/// styled `cursor_style` when `visible` and `value_style` otherwise.
pub fn text_field_spans(
    value: &str,
    cursor: usize,
    visible: bool,
    value_style: Style,
    cursor_style: Style,
) -> [Span<'static>; 3] {
    let (pre, rest) = split_at_char(value, cursor);
    let (under, post) = match rest.graphemes(true).next() {
        Some(g) => (g.to_owned(), rest[g.len()..].to_owned()),
        None => (" ".to_owned(), String::new()),
    };
    let cell_style = if visible { cursor_style } else { value_style };
    [
        Span::styled(pre, value_style),
        Span::styled(under, cell_style),
        Span::styled(post, value_style),
    ]
}

/// The visible slice of a single-line field `width` cells wide, scrolled so the cursor's cluster
/// (or one blank cell past the end) is always on screen.  `scroll` is the first visible char
/// index from the previous frame: the window moves only as far as the cursor forces it, and
/// slides back left while the text no longer fills it (after a delete).  The window moves a
/// whole grapheme cluster at a time, so neither edge splits one.  Returns the new scroll offset
/// and the visible text; the cursor sits at `cursor - scroll` in it, ready for
/// [`text_field_spans`].
pub fn scroll_field(value: &str, cursor: usize, scroll: usize, width: usize) -> (usize, String) {
    // Each cluster: its first char index, its cells, its text.
    let mut units: Vec<(usize, usize, &str)> = Vec::new();
    let mut len = 0;
    for g in value.graphemes(true) {
        units.push((len, g.width(), g));
        len += g.chars().count();
    }
    // The cluster holding char index `ch` (`units.len()` at or past the end).
    let unit_of = |ch: usize| -> usize {
        if ch >= len {
            units.len()
        } else {
            units.partition_point(|u| u.0 <= ch).saturating_sub(1)
        }
    };
    let cells = |from: usize, to: usize| -> usize { units[from..to].iter().map(|u| u.1).sum() };
    let cursor = unit_of(cursor);
    let cursor_cell = units.get(cursor).map_or(1, |u| u.1.max(1));

    let mut scroll = unit_of(scroll).min(cursor);
    while scroll < cursor && cells(scroll, cursor) + cursor_cell > width {
        scroll += 1;
    }
    // Strictly less than `width` reserves the end-of-text cursor cell whether or not the cursor
    // is there, so stepping the cursor back from the end never shifts the text.
    while scroll > 0 && cells(scroll - 1, units.len()) < width {
        scroll -= 1;
    }

    let mut used = 0;
    let visible = units[scroll..]
        .iter()
        .take_while(|u| {
            used += u.1;
            used <= width
        })
        .map(|u| u.2)
        .collect();
    (units.get(scroll).map_or(len, |u| u.0), visible)
}

/// The spans of a single-line field exactly `width` cells wide: the value windowed by
/// [`scroll_field`] (which updates `scroll`), the blink-stable cursor from
/// [`text_field_spans`], then `value_style` padding out to `width`, so the field's background
/// spans its full width however long the value is.  `visible` is the cursor's blink phase.
pub fn scrolled_field_spans(
    value: &str,
    cursor: usize,
    scroll: &mut usize,
    width: usize,
    visible: bool,
    value_style: Style,
    cursor_style: Style,
) -> Vec<Span<'static>> {
    let cursor = cursor.min(value.chars().count());
    let (new_scroll, shown) = scroll_field(value, cursor, *scroll, width);
    *scroll = new_scroll;
    let mut spans = Vec::from(text_field_spans(
        &shown,
        cursor - new_scroll,
        visible,
        value_style,
        cursor_style,
    ));
    let used: usize = spans.iter().map(Span::width).sum();
    let pad = width.saturating_sub(used);
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), value_style));
    }
    spans
}

/// Insert `ch` at char index `cursor` (appends when past the end).
pub fn insert_char_at(s: &mut String, cursor: usize, ch: char) {
    s.insert(str_byte_index(s, cursor), ch);
}

/// Remove the last grapheme cluster of an append-only field; `false` when `s` was empty.
pub fn pop_grapheme(s: &mut String) -> bool {
    match s.grapheme_indices(true).next_back() {
        Some((byte_idx, _)) => {
            s.truncate(byte_idx);
            true
        }
        None => false,
    }
}

/// Split `s` at char index `cursor` (clamped to the end) into two owned halves.
fn split_at_char(s: &str, cursor: usize) -> (String, String) {
    let byte_idx = str_byte_index(s, cursor);
    (s[..byte_idx].to_owned(), s[byte_idx..].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_at_char_mid_and_past_end() {
        assert_eq!(split_at_char("hello", 2), ("he".into(), "llo".into()));
        assert_eq!(split_at_char("hi", 5), ("hi".into(), String::new()));
    }

    #[test]
    fn split_at_char_respects_char_boundaries() {
        assert_eq!(split_at_char("é!", 1), ("é".into(), "!".into()));
    }

    #[test]
    fn text_field_slot_is_constant_width_across_blink() {
        let vis = text_field_spans("note", 2, true, Style::default(), Style::default());
        let hid = text_field_spans("note", 2, false, Style::default(), Style::default());
        let width = |spans: &[Span<'static>]| -> usize {
            spans.iter().map(|s| s.content.chars().count()).sum()
        };
        assert_eq!(width(&vis), width(&hid));
        assert_eq!(vis[1].content.as_ref(), "t");
        assert_eq!(hid[1].content.as_ref(), "t");
    }

    #[test]
    fn text_field_cursor_past_end_is_a_space_cell() {
        let spans = text_field_spans("hi", 2, true, Style::default(), Style::default());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "hi ");
        assert_eq!(spans[1].content.as_ref(), " ");
        assert!(spans[2].content.is_empty());
    }

    /// The cursor cell holds the whole cluster, so a combining mark or the rest of a ZWJ
    /// sequence never paints as a cell of its own.
    #[test]
    fn text_field_cursor_covers_the_whole_cluster() {
        let s = "e\u{0301}👨\u{200D}👩x";
        let spans = text_field_spans(s, 0, true, Style::default(), Style::default());
        assert_eq!(spans[1].content.as_ref(), "e\u{0301}");
        assert_eq!(spans[2].content.as_ref(), "👨\u{200D}👩x");
        let spans = text_field_spans(s, 2, true, Style::default(), Style::default());
        assert_eq!(spans[1].content.as_ref(), "👨\u{200D}👩");
        assert_eq!(spans[2].content.as_ref(), "x");
    }

    #[test]
    fn scroll_field_leaves_a_fitting_value_unscrolled() {
        assert_eq!(scroll_field("hello", 5, 0, 10), (0, "hello".into()));
        assert_eq!(scroll_field("hello", 0, 0, 10), (0, "hello".into()));
    }

    #[test]
    fn scroll_field_keeps_the_end_of_text_cursor_cell_on_screen() {
        // Five cells: four chars plus the blank cursor cell past the end.
        assert_eq!(scroll_field("abcdefgh", 8, 0, 5), (4, "efgh".into()));
    }

    #[test]
    fn scroll_field_moves_only_as_far_as_the_cursor_forces() {
        // Cursor still inside the window: the previous scroll stands.
        assert_eq!(scroll_field("abcdefgh", 5, 4, 5), (4, "efgh".into()));
        // Cursor left of the window: it becomes the first visible char.
        assert_eq!(scroll_field("abcdefgh", 2, 4, 5), (2, "cdefg".into()));
    }

    #[test]
    fn scroll_field_slides_back_when_the_text_shrinks() {
        // After deletes, "abcdef" with the cursor at the end fits from index 2 in five cells.
        assert_eq!(scroll_field("abcdef", 6, 5, 5), (2, "cdef".into()));
    }

    #[test]
    fn scroll_field_counts_wide_chars_as_two_cells() {
        // 日本語 is six cells; the end cursor needs one more, so only two glyphs fit in five.
        assert_eq!(scroll_field("日本語", 3, 0, 5), (1, "本語".into()));
    }

    /// The window scrolls by whole clusters: the left edge never lands on a combining mark,
    /// and a cluster is measured as it paints, not as the sum of its chars.
    #[test]
    fn scroll_field_never_splits_a_cluster() {
        // Four `é`s, each two chars and one cell; the end cursor needs a cell, so three fit.
        let s = "e\u{0301}".repeat(4);
        assert_eq!(scroll_field(&s, 8, 0, 4), (2, "e\u{0301}".repeat(3)));
        // A stale scroll inside a cluster snaps back to that cluster's start.
        assert_eq!(scroll_field(&s, 8, 3, 4), (2, "e\u{0301}".repeat(3)));
        // 👍🏽 paints two cells, not the four its two chars would sum to.
        assert_eq!(
            scroll_field("👍\u{1F3FD}ab", 4, 0, 5),
            (0, "👍\u{1F3FD}ab".into())
        );
    }

    #[test]
    fn insert_char_at_respects_char_boundaries() {
        let mut s = "é!".to_owned();
        insert_char_at(&mut s, 1, 'x');
        assert_eq!(s, "éx!");
        insert_char_at(&mut s, 9, '?');
        assert_eq!(s, "éx!?");
    }

    #[test]
    fn pop_grapheme_removes_the_last_cluster() {
        let mut s = "a👍\u{1F3FD}".to_owned();
        assert!(pop_grapheme(&mut s));
        assert_eq!(s, "a");
        assert!(pop_grapheme(&mut s));
        assert!(!pop_grapheme(&mut s));
    }

    #[test]
    fn scrolled_field_spans_pad_to_the_full_width() {
        let width = |spans: &[Span<'static>]| -> usize { spans.iter().map(Span::width).sum() };
        let mut scroll = 0;
        let short = scrolled_field_spans(
            "hi",
            2,
            &mut scroll,
            10,
            true,
            Style::default(),
            Style::default(),
        );
        assert_eq!(width(&short), 10);
        let long = scrolled_field_spans(
            "abcdefghijkl",
            12,
            &mut scroll,
            10,
            true,
            Style::default(),
            Style::default(),
        );
        assert_eq!(width(&long), 10);
        assert_eq!(scroll, 3, "the window followed the cursor to the end");
        let wide = scrolled_field_spans(
            "日本",
            1,
            &mut 0,
            6,
            false,
            Style::default(),
            Style::default(),
        );
        assert_eq!(width(&wide), 6);
    }
}
