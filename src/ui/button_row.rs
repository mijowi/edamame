//! Shared rendering for the centred button row at the bottom of every
//! modal/overlay (`[ Save ]  [ Cancel ]`).  Keeps the bracket
//! formatting, focus styling, and 2-space gap consistent across
//! `modal`, `save_copy_modal`, and `insert_table_modal`.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::config::Theme;

/// Width in columns of a row of buttons rendered as
/// `[ label ]  [ label ]` — each button is `label + 4` columns
/// (one space + two brackets + one space) and adjacent buttons are
/// separated by a 2-column gap.
pub fn button_row_width(labels: &[&str]) -> u16 {
    let labels_w: usize = labels.iter().map(|l| l.chars().count() + 4).sum();
    let gaps = labels.len().saturating_sub(1) * 2;
    (labels_w + gaps) as u16
}

/// Render the button row, horizontally centred in `area`, with the
/// button at `focused_idx` drawn in `theme.modal_button_focused` and
/// the rest in `theme.modal_item`.
pub fn render_button_row(
    area: Rect,
    buf: &mut Buffer,
    labels: &[&str],
    focused_idx: usize,
    theme: &Theme,
) {
    let mut spans: Vec<Span<'_>> = Vec::with_capacity(labels.len() * 2 + 1);
    for (i, label) in labels.iter().enumerate() {
        let style = if i == focused_idx {
            theme.modal_button_focused
        } else {
            theme.modal_item
        };
        spans.push(Span::styled(format!("[ {label} ]"), style));
        if i + 1 < labels.len() {
            spans.push(Span::raw("  "));
        }
    }
    Paragraph::new(Line::from(spans))
        .alignment(Alignment::Center)
        .style(theme.modal_bg)
        .render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_two_buttons_with_gap() {
        // "[ Save ]"   = 8 chars, "[ Cancel ]" = 10 chars, gap = 2.
        assert_eq!(button_row_width(&["Save", "Cancel"]), 8 + 10 + 2);
    }

    #[test]
    fn width_single_button_has_no_gap() {
        assert_eq!(button_row_width(&["Ok"]), 6);
    }

    #[test]
    fn width_zero_buttons() {
        assert_eq!(button_row_width(&[]), 0);
    }
}
