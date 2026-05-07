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
}
