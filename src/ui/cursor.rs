//! Cursor appearance — the shared text-field cursor helper used by the
//! editor views and modal inputs alike.
//!
//! Every cursor in edamame is *fake*: a styled buffer cell, never the
//! terminal's hardware cursor.  The hardware cursor can show only one
//! position at a time and its color can't be set portably (OSC 12 is
//! unsupported by kitty and has broken restore in VTE), so a fake cursor is
//! what lets us (a) color the cursor per context and (b) show the editor
//! cursor and a modal input cursor at the same time.
//!
//! The cursor is always a **block**: the cell at the insertion point is
//! recolored with the cursor style while the underlying character stays
//! visible.  Shape is uniform across the app, so context is signalled by
//! *color* — the editor cursor mirrors the per-mode status chip
//! (`app::cursor_style`), and modal inputs use `cursor` (see
//! `docs/theming.md`).
//!
//! # Two block-cursor mechanisms
//!
//! There are two ways to render that block, and they are NOT interchangeable
//! — the choice is forced by whether the call site controls the styling of the
//! cursor's individual cell:
//!
//! 1. **Recolor-the-cell** ([`text_field_spans`]) — the preferred form.  The
//!    character under the cursor keeps its glyph and is re-*styled* with the
//!    cursor color; past end-of-line a single styled space stands in.  The
//!    output is always exactly one cell wide and the glyph stays legible, so
//!    the field neither jitters on blink nor hides the character it sits on.
//!    Use this anywhere the value is assembled here as its own `Span`s
//!    (modal text inputs, the vim command line).
//!
//! 2. **Insert-a-glyph** ([`CURSOR_BLOCK`], the U+2588 full block) — the
//!    fallback.  Some rows (`settings_overlay`, `export_theme_modal`) push the
//!    value through a shared row formatter (`format_modal_row`) or a
//!    horizontal-scroll window that owns the cell styling, so we can't hand
//!    those layers a pre-styled cursor cell.  Instead we splice a literal `█`
//!    glyph into the string.  Unlike mechanism 1 this is a *true glyph*: it
//!    occupies its own cell and obscures nothing (it is only ever placed at an
//!    append-only end-of-value position, so there is no character to hide).
//!    To stay blink-stable the caller MUST emit a same-width space on the
//!    hidden phase — `█` when visible, `" "` when not — or the field width
//!    changes between blink phases.
//!
//! Reach for mechanism 2 only when mechanism 1 genuinely can't apply; don't
//! "unify" the `█` sites onto the recolor path without first giving those rows
//! per-cell styling control.

use ratatui::style::Style;
use ratatui::text::Span;

/// Full-cell block glyph (U+2588) used as the cursor where a value flows
/// through a uniform row formatter (`settings_overlay`, `export_theme_modal`)
/// and a separately-styled cursor cell isn't available — the glyph itself
/// reads as the block.  Prefer [`text_field_spans`] wherever the cursor cell
/// can carry its own style.
pub const CURSOR_BLOCK: char = '█';

/// Build the three styled spans for a single-line text field's value with a
/// blink-stable block cursor at char index `cursor`.
///
/// The middle span is always exactly one cell wide — the character under the
/// cursor (or a space when the cursor sits past the last char) styled
/// `cursor_style` when `visible`, and styled `value_style` (i.e. shown as
/// ordinary text) when not — so the field's rendered width never changes
/// between blink phases.  Callers push the three spans in order, surrounded by
/// their own label / padding spans.
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
    // The cursor cell holds the character it sits on (a space past
    // end-of-line) — recolored on the visible blink phase, shown as normal
    // text on the hidden phase.  Either way it is one cell wide, so the field
    // never jitters on blink.
    let cell_style = if visible { cursor_style } else { value_style };
    [
        Span::styled(pre, value_style),
        Span::styled(under.unwrap_or(' ').to_string(), cell_style),
        Span::styled(post, value_style),
    ]
}

/// Split `s` at char index `cursor` into two owned halves.  `cursor` past
/// the end yields `(s.to_owned(), String::new())`.
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
        // "é" is two bytes; splitting after it must land on a char boundary.
        assert_eq!(split_at_char("é!", 1), ("é".into(), "!".into()));
    }

    #[test]
    fn text_field_slot_is_constant_width_across_blink() {
        let vis = text_field_spans("note", 2, true, Style::default(), Style::default());
        let hid = text_field_spans("note", 2, false, Style::default(), Style::default());
        let width = |spans: &[Span<'static>]| -> usize {
            spans.iter().map(|s| s.content.chars().count()).sum()
        };
        // Same total cell count regardless of the cursor's visibility phase.
        assert_eq!(width(&vis), width(&hid));
        // The cursor cell holds the character under it ('t'), not a glyph.
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
}
