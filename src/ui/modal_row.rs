//! Shared formatter for the focused-row pattern used across modal
//! overlays (command palette, settings, keybinds).
//!
//! Every overlay shows rows of the form
//!
//! ```text
//! › Label                      value
//! ```
//!
//! with the focus marker (`"› "` vs `"  "`) on the left, the label
//! styled by focus, and a value (or chord, or hint) styled by focus +
//! editing.  This module owns the styling rules so they stay
//! consistent.

use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::config::Theme;

/// How the value column is positioned relative to the label.
#[derive(Debug, Clone, Copy)]
pub enum RowLayout {
    /// Pad the label to a fixed character width (settings, keybinds).
    /// The value follows immediately after the padded label.
    FixedPad(usize),
    /// Right-align the value against the given total line width
    /// (command palette).  At least one space always separates the
    /// label from the value.
    RightAlign(u16),
}

/// Build a styled `Line` for one focusable modal row.
///
/// `editing` only changes the value style — when true the value is
/// drawn in `theme.modal_input_focused` regardless of focus, matching
/// the in-place edit affordance used by the settings and keybinds
/// overlays.  Pass `false` from the palette, which has no edit mode.
pub fn format_modal_row(
    label: &str,
    value: &str,
    focused: bool,
    editing: bool,
    theme: &Theme,
    layout: RowLayout,
) -> Line<'static> {
    let marker = if focused { "› " } else { "  " };
    let label_style = if focused {
        theme.modal_item_selected
    } else {
        theme.modal_item
    };
    let value_style = if editing {
        theme.modal_input_focused
    } else if focused {
        theme.modal_item_selected_hint
    } else {
        theme.modal_item_hint
    };

    match layout {
        RowLayout::FixedPad(pad) => {
            let label_padded = format!("{marker}{:<pad$}", label, pad = pad);
            Line::from(vec![
                Span::styled(label_padded, label_style),
                Span::styled(value.to_owned(), value_style),
            ])
        }
        RowLayout::RightAlign(width) => {
            let label_full = format!("{marker}{label}");
            let label_w = label_full.chars().count();
            let value_w = value.chars().count();
            let total = label_w + value_w + 1;
            let pad = (width as usize).saturating_sub(total).max(1);
            let pad_str = " ".repeat(pad);
            Line::from(vec![
                Span::styled(label_full, label_style),
                Span::styled(pad_str, label_style),
                Span::styled(value.to_owned(), value_style),
            ])
        }
    }
}

/// Truncate `text` so its display width fits within `max_cells`,
/// appending `…` to signal the cut.  Counts grapheme display cells via
/// `unicode-width` so wide characters (CJK, emoji) consume their full
/// column budget.
///
/// Edge cases:
/// - returns the input unchanged when it already fits
/// - returns `""` when `max_cells == 0`
/// - returns a single `…` when `max_cells == 1` (the ellipsis itself
///   takes one cell), or as much input as fits when no room is left
///   for an ellipsis
pub fn truncate_to_cells(text: &str, max_cells: usize) -> String {
    if max_cells == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= max_cells {
        return text.to_owned();
    }
    // Reserve one cell for the ellipsis; fill the rest with as many
    // input cells as we can.
    let budget = max_cells - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn fixed_pad_focused_uses_marker_and_pad() {
        let line = format_modal_row(
            "Theme",
            "dark",
            true,
            false,
            theme(),
            RowLayout::FixedPad(10),
        );
        assert_eq!(line_text(&line), "› Theme     dark");
    }

    #[test]
    fn fixed_pad_unfocused_uses_blank_marker() {
        let line = format_modal_row(
            "Theme",
            "dark",
            false,
            false,
            theme(),
            RowLayout::FixedPad(10),
        );
        assert_eq!(line_text(&line), "  Theme     dark");
    }

    #[test]
    fn right_align_pads_to_width() {
        // Original palette behaviour leaves one column of slack so the
        // value never butts up against the modal frame.
        let line = format_modal_row(
            "Save",
            "Ctrl+S",
            false,
            false,
            theme(),
            RowLayout::RightAlign(20),
        );
        assert_eq!(line_text(&line).chars().count(), 19);
        assert!(line_text(&line).ends_with("Ctrl+S"));
    }

    #[test]
    fn right_align_keeps_one_space_minimum_when_overflowing() {
        let line = format_modal_row(
            "Save",
            "Ctrl+S",
            false,
            false,
            theme(),
            RowLayout::RightAlign(4),
        );
        assert!(line_text(&line).contains(" Ctrl+S"));
    }

    #[test]
    fn editing_overrides_value_style() {
        let t = theme();
        let line = format_modal_row("X", "v", true, true, t, RowLayout::FixedPad(4));
        let value_span = line.spans.last().unwrap();
        assert_eq!(value_span.style, t.modal_input_focused);
    }

    #[test]
    fn truncate_returns_input_when_it_fits() {
        assert_eq!(truncate_to_cells("hello", 10), "hello");
        assert_eq!(truncate_to_cells("hello", 5), "hello");
    }

    #[test]
    fn truncate_appends_ellipsis_when_too_long() {
        // 6 cells of input, budget 5 → 4 cells of input + `…`.
        assert_eq!(truncate_to_cells("abcdef", 5), "abcd…");
    }

    #[test]
    fn truncate_respects_wide_characters() {
        // "漢字" is 4 display cells.  Budget 3 → reserve 1 for `…`,
        // remaining 2 cells holds exactly the first wide char.
        assert_eq!(truncate_to_cells("漢字", 3), "漢…");
    }

    #[test]
    fn truncate_zero_budget_returns_empty() {
        assert_eq!(truncate_to_cells("abc", 0), "");
    }

    #[test]
    fn truncate_one_cell_budget_returns_just_ellipsis() {
        // Budget = 1, content too wide: reserve 1 for `…`, can't fit
        // any input char.
        assert_eq!(truncate_to_cells("abc", 1), "…");
    }
}
