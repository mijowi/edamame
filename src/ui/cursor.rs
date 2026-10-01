//! Shared text-field cursor helper for editor views and modal inputs.
//!
//! Every cursor is *fake* (a styled cell, never the hardware cursor): the hardware cursor shows
//! one position and its color can't be set portably (OSC 12 is unsupported by kitty and
//! mis-restores in VTE).  The cursor is always a block; context is signaled by color
//! (see `docs/dev/theming.md`).
//!
//! Two block mechanisms, not interchangeable:
//! 1. **Recolor-the-cell** ([`text_field_spans`]), preferred: the glyph under the cursor is
//!    restyled, always one cell wide, so the field never jitters on blink.
//! 2. **Insert-a-glyph** ([`CURSOR_BLOCK`]), fallback for rows whose cell styling is owned by a
//!    shared formatter or scroll window (`settings_overlay`, `export_theme_modal`).  Only ever
//!    placed at an append-only end-of-value position; the caller MUST emit a same-width space
//!    on the hidden blink phase.  Don't move those sites onto mechanism 1 without first giving
//!    them per-cell styling control.

use ratatui::style::Style;
use ratatui::text::Span;
use unicode_width::UnicodeWidthChar;

/// Full-cell block glyph for mechanism 2 (see the module doc).  Prefer [`text_field_spans`].
pub const CURSOR_BLOCK: char = '█';

/// The three spans of a single-line field value with a blink-stable block cursor at char index
/// `cursor`.  The middle span is always one cell (the char under the cursor, or a space past
/// the end), styled `cursor_style` when `visible` and `value_style` otherwise.
pub fn text_field_spans(
    value: &str,
    cursor: usize,
    visible: bool,
    value_style: Style,
    cursor_style: Style,
) -> [Span<'static>; 3] {
    let (pre, rest) = split_at_char(value, cursor);
    let mut rest_chars = rest.chars();
    let under = rest_chars.next();
    let post: String = rest_chars.collect();
    let cell_style = if visible { cursor_style } else { value_style };
    [
        Span::styled(pre, value_style),
        Span::styled(under.unwrap_or(' ').to_string(), cell_style),
        Span::styled(post, value_style),
    ]
}

/// The visible slice of a single-line field `width` cells wide, scrolled so the cursor cell
/// (the char under the cursor, or one blank cell past the end) is always on screen.  `scroll`
/// is the first visible char index from the previous frame: the window moves only as far as the
/// cursor forces it, and slides back left while the text no longer fills it (after a delete).
/// Returns the new scroll offset and the visible text; the cursor sits at `cursor - scroll` in
/// it, ready for [`text_field_spans`].
pub fn scroll_field(value: &str, cursor: usize, scroll: usize, width: usize) -> (usize, String) {
    let chars: Vec<char> = value.chars().collect();
    let cursor = cursor.min(chars.len());
    let cells = |cs: &[char]| -> usize { cs.iter().map(|c| c.width().unwrap_or(0)).sum() };
    let cursor_cell = chars
        .get(cursor)
        .map_or(1, |c| c.width().unwrap_or(0).max(1));

    let mut scroll = scroll.min(cursor);
    while scroll < cursor && cells(&chars[scroll..cursor]) + cursor_cell > width {
        scroll += 1;
    }
    // Strictly less than `width` reserves the end-of-text cursor cell whether or not the cursor
    // is there, so stepping the cursor back from the end never shifts the text.
    while scroll > 0 && cells(&chars[scroll - 1..]) < width {
        scroll -= 1;
    }

    let mut used = 0;
    let visible = chars[scroll..]
        .iter()
        .take_while(|c| {
            used += c.width().unwrap_or(0);
            used <= width
        })
        .collect();
    (scroll, visible)
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
    let byte_idx = s
        .char_indices()
        .nth(cursor)
        .map(|(b, _)| b)
        .unwrap_or(s.len());
    s.insert(byte_idx, ch);
}

/// Remove the char at char index `cursor`; no-op when out of bounds.
pub fn remove_char_at(s: &mut String, cursor: usize) {
    if let Some((byte_idx, ch)) = s.char_indices().nth(cursor) {
        s.replace_range(byte_idx..byte_idx + ch.len_utf8(), "");
    }
}

/// Split `s` at char index `cursor` (clamped to the end) into two owned halves.
fn split_at_char(s: &str, cursor: usize) -> (String, String) {
    let byte_idx = s
        .char_indices()
        .nth(cursor)
        .map(|(b, _)| b)
        .unwrap_or(s.len());
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

    #[test]
    fn insert_and_remove_char_at_respect_char_boundaries() {
        let mut s = "é!".to_owned();
        insert_char_at(&mut s, 1, 'x');
        assert_eq!(s, "éx!");
        remove_char_at(&mut s, 0);
        assert_eq!(s, "x!");
        remove_char_at(&mut s, 9);
        assert_eq!(s, "x!");
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
