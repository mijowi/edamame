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

/// One button in a row, rendered wrapped in `[ … ]` (e.g. `[ Save ]`).
#[derive(Debug, Clone, Copy)]
pub struct Button<'a> {
    pub label: &'a str,
}

impl<'a> Button<'a> {
    pub fn bracketed(label: &'a str) -> Self {
        Self { label }
    }

    /// Column width of the rendered button: the label plus 4 (one space
    /// + two brackets + one space).
    fn width(&self) -> u16 {
        self.label.chars().count() as u16 + 4
    }

    fn rendered(&self) -> String {
        format!("[ {} ]", self.label)
    }
}

/// Width in columns of a row of [`Button`]s — each button is `label + 4`
/// columns, and adjacent buttons are separated by a 2-column gap.
pub fn buttons_row_width(buttons: &[Button]) -> u16 {
    let labels_w: usize = buttons.iter().map(|b| b.width() as usize).sum();
    let gaps = buttons.len().saturating_sub(1) * 2;
    (labels_w + gaps) as u16
}

/// Width in columns of a row of all-bracketed buttons rendered as
/// `[ label ]  [ label ]`.  Convenience wrapper over
/// [`buttons_row_width`] for the common case.
pub fn button_row_width(labels: &[&str]) -> u16 {
    let buttons: Vec<Button> = labels.iter().map(|l| Button::bracketed(l)).collect();
    buttons_row_width(&buttons)
}

/// Render the button row, horizontally centred in `area`, with the
/// button at `focused_idx` drawn focused (`primary` chip) and
/// the rest as a neutral `text_muted` chip (see `controls::button_style`).
///
/// Returns the absolute terminal rect of each rendered button, in the
/// same order as `labels`, so callers that need to hit-test mouse
/// clicks can do so without duplicating the centring / bracket-padding
/// arithmetic.  Single source of truth for button layout.
pub fn render_button_row(
    area: Rect,
    buf: &mut Buffer,
    labels: &[&str],
    focused_idx: usize,
    theme: &Theme,
) -> Vec<Rect> {
    let buttons: Vec<Button> = labels.iter().map(|l| Button::bracketed(l)).collect();
    render_buttons(area, buf, &buttons, focused_idx, theme)
}

/// Render a row of [`Button`]s, horizontally centred in `area`, with the
/// button at `focused_idx` drawn focused (`primary` chip) and the
/// rest in `theme.modal_item`.  Bare buttons render their label without
/// the `[ … ]` wrapper.
///
/// Returns the absolute terminal rect of each rendered button, in the
/// same order as `buttons`, so callers that need to hit-test mouse
/// clicks can do so without duplicating the centring arithmetic.  Single
/// source of truth for button layout.
pub fn render_buttons(
    area: Rect,
    buf: &mut Buffer,
    buttons: &[Button],
    focused_idx: usize,
    theme: &Theme,
) -> Vec<Rect> {
    let mut spans: Vec<Span<'_>> = Vec::with_capacity(buttons.len() * 2 + 1);
    for (i, button) in buttons.iter().enumerate() {
        let style = crate::ui::controls::button_style(i == focused_idx, theme);
        spans.push(Span::styled(button.rendered(), style));
        if i + 1 < buttons.len() {
            spans.push(Span::raw("  "));
        }
    }
    Paragraph::new(Line::from(spans))
        .alignment(Alignment::Center)
        .style(theme.modal_bg)
        .render(area, buf);

    // Mirror Paragraph's centred layout: total row width is
    // buttons_row_width(buttons), starting at the centred offset inside
    // `area`.  Each button occupies its own width, with a 2-column gap
    // between buttons.
    let total = buttons_row_width(buttons);
    let start_x = area.x + area.width.saturating_sub(total) / 2;
    let mut rects = Vec::with_capacity(buttons.len());
    let mut x = start_x;
    for button in buttons {
        let w = button.width();
        rects.push(Rect {
            x,
            y: area.y,
            width: w,
            height: 1,
        });
        x += w + 2;
    }
    rects
}

/// Render a single [`Button`] left-aligned at the start of `area` (rather
/// than centred like [`render_buttons`]), filling the row with the modal
/// background.  Returns the button's absolute rect for hit-testing.
///
/// Used where a button reads as an inline affordance pinned to the body's
/// left edge rather than a centred footer row — e.g. the welcome modal's
/// "Switch theme" button.  Shares the bracket formatting, width math, and
/// `controls::button_style` focus styling with the rest of this module so
/// callers never hand-roll a button.
pub fn render_button_at(
    area: Rect,
    buf: &mut Buffer,
    button: Button,
    focused: bool,
    theme: &Theme,
) -> Rect {
    let style = crate::ui::controls::button_style(focused, theme);
    Paragraph::new(Line::from(Span::styled(button.rendered(), style)))
        .alignment(Alignment::Left)
        .style(theme.modal_bg)
        .render(area, buf);
    Rect {
        x: area.x,
        y: area.y,
        width: button.width(),
        height: 1,
    }
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
