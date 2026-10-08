//! Shared rendering for the centered `[ Save ]  [ Cancel ]` button row at the bottom of every
//! modal/overlay: bracket formatting, focus styling, gap, and wrap packing in one place.

use std::ops::Range;

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::config::Theme;
use crate::ui::scroll_container::modal_inner_width;

/// One button in a row, rendered wrapped in `[ … ]` (e.g. `[ Save ]`).
#[derive(Debug, Clone, Copy)]
pub struct Button<'a> {
    pub label: &'a str,
}

impl<'a> Button<'a> {
    pub fn bracketed(label: &'a str) -> Self {
        Self { label }
    }

    fn width(&self) -> u16 {
        self.label.width() as u16 + 4
    }

    fn rendered(&self) -> String {
        format!("[ {} ]", self.label)
    }
}

/// Width in columns of a row of [`Button`]s including the gaps between them.
pub fn buttons_row_width(buttons: &[Button]) -> u16 {
    let labels_w: usize = buttons.iter().map(|b| b.width() as usize).sum();
    let gaps = buttons.len().saturating_sub(1) * 2;
    (labels_w + gaps) as u16
}

const BUTTON_GAP: u16 = 2;

/// Greedily pack `buttons` into rows of at most `width` columns, returning one index range per
/// row.  Overflow wraps rather than clips (a clipped button is still focusable and clickable but
/// invisible); a single button wider than `width` gets its own row and is clipped there rather
/// than dropped.
pub fn button_rows(buttons: &[Button], width: u16) -> Vec<Range<usize>> {
    let mut rows: Vec<Range<usize>> = Vec::new();
    let mut start = 0;
    let mut row_w = 0;
    for (i, button) in buttons.iter().enumerate() {
        let w = button.width();
        if i > start && row_w + BUTTON_GAP + w > width {
            rows.push(start..i);
            start = i;
            row_w = w;
        } else {
            row_w += if i > start { BUTTON_GAP + w } else { w };
        }
    }
    if start < buttons.len() {
        rows.push(start..buttons.len());
    }
    rows
}

/// Blank rows between wrapped footer rows, so the rows read as alternatives rather than a list.
const ROW_SPACING: u16 = 1;

/// Rows [`render_buttons`] paints for `buttons` at `width`, including [`ROW_SPACING`] blanks.
/// Must derive from the same packing the render uses, since sizing runs before the rect exists.
pub fn button_rows_height(buttons: &[Button], width: u16) -> u16 {
    let rows = button_rows(buttons, width).len() as u16;
    rows.saturating_mul(1 + ROW_SPACING)
        .saturating_sub(ROW_SPACING)
}

/// Rows a footer of `labels` needs inside a modal of `content_w` columns with padding capped at
/// `max_pad_h`, in a terminal `area_w` wide.
///
/// Runs the frame's real sizing arithmetic ([`modal_inner_width`]) so the
/// reservation can never disagree with the packing in [`render_buttons`]; a flat
/// [`crate::ui::MIN_PAD_H`] shortcut overestimates the inner width and reserves one row for a
/// footer that wraps onto two.  `max_pad_h` must be the caller's own
/// [`crate::ui::scroll_container::ContentSize::max_pad_h`] (the keybinds overlay raises it).
pub fn footer_row_count(labels: &[&str], content_w: u16, area_w: u16, max_pad_h: u16) -> u16 {
    let buttons: Vec<Button> = labels.iter().map(|l| Button::bracketed(l)).collect();
    let inner_w = modal_inner_width(content_w, area_w, max_pad_h);
    button_rows_height(&buttons, inner_w).max(1)
}

/// [`buttons_row_width`] for a row of bracketed `labels`.
pub fn button_row_width(labels: &[&str]) -> u16 {
    let buttons: Vec<Button> = labels.iter().map(|l| Button::bracketed(l)).collect();
    buttons_row_width(&buttons)
}

/// [`render_buttons`] for a row of bracketed `labels`.
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

/// Render [`Button`]s centered in `area`, wrapping per [`button_rows`], with `focused_idx`
/// drawn focused (see `controls::button_style`).  Returns each button's absolute rect, in
/// order, for hit-testing.
pub fn render_buttons(
    area: Rect,
    buf: &mut Buffer,
    buttons: &[Button],
    focused_idx: usize,
    theme: &Theme,
) -> Vec<Rect> {
    let mut rects = Vec::with_capacity(buttons.len());
    for (row_idx, row) in button_rows(buttons, area.width).into_iter().enumerate() {
        let y = area.y + row_idx as u16 * (1 + ROW_SPACING);
        // A row past the bottom of `area` is not painted, but its rects are still produced:
        // callers index the result by button.
        let visible = y < area.y + area.height;
        let row_buttons = &buttons[row.clone()];
        let mut spans: Vec<Span<'_>> = Vec::with_capacity(row_buttons.len() * 2);
        for (i, button) in row_buttons.iter().enumerate() {
            let style = crate::ui::controls::button_style(row.start + i == focused_idx, theme);
            spans.push(Span::styled(button.rendered(), style));
            if i + 1 < row_buttons.len() {
                spans.push(Span::raw(" ".repeat(BUTTON_GAP as usize)));
            }
        }
        if visible {
            let row_area = Rect {
                height: 1,
                y,
                ..area
            };
            Paragraph::new(Line::from(spans))
                .alignment(Alignment::Center)
                .style(theme.modal_bg)
                .render(row_area, buf);
        }

        // Mirror Paragraph's centered layout.
        let mut x = area.x + area.width.saturating_sub(buttons_row_width(row_buttons)) / 2;
        for button in row_buttons {
            let w = button.width();
            rects.push(Rect {
                x,
                y,
                width: w,
                height: 1,
            });
            x += w + BUTTON_GAP;
        }
    }
    rects
}

/// Render one [`Button`] left-aligned at the start of `area` (an inline affordance, e.g. the
/// welcome modal's "Switch theme"), filling the row with the modal background.  Returns its
/// absolute rect.  A `disabled` button uses the shared disabled control style; the caller is
/// responsible for ignoring its rect.
pub fn render_button_at(
    area: Rect,
    buf: &mut Buffer,
    button: Button,
    focused: bool,
    disabled: bool,
    theme: &Theme,
) -> Rect {
    let style = if disabled {
        crate::ui::controls::control_label_style(false, true, theme)
    } else {
        crate::ui::controls::button_style(focused, theme)
    };
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
    use crate::ui::scroll_container::compute_pad_h;
    use crate::ui::scroll_container::MAX_PAD_H;

    #[test]
    fn width_two_buttons_with_gap() {
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

    fn buttons(labels: &[&'static str]) -> Vec<Button<'static>> {
        labels.iter().map(|l| Button::bracketed(l)).collect()
    }

    #[test]
    fn a_row_that_fits_stays_one_row() {
        let b = buttons(&["Save", "Cancel"]);
        assert_eq!(button_rows(&b, 40), vec![0..2]);
        assert_eq!(button_rows_height(&b, 40), 1);
    }

    #[test]
    fn buttons_wrap_instead_of_clipping() {
        let b = buttons(&["Save", "Cancel"]);
        assert_eq!(button_rows(&b, 18), vec![0..1, 1..2]);
        assert_eq!(button_rows_height(&b, 18), 3);
    }

    #[test]
    fn a_wrapped_footer_is_spaced_out() {
        let theme = Theme::default();
        let area = Rect::new(0, 0, 18, 5);
        let mut buf = Buffer::empty(area);
        let b = buttons(&["Save", "Cancel", "Discard"]);
        let rects = render_buttons(area, &mut buf, &b, 0, &theme);
        let ys: Vec<u16> = rects.iter().map(|r| r.y).collect();
        assert_eq!(ys, vec![0, 2, 4], "a blank row sits between each pair");
    }

    #[test]
    fn a_button_wider_than_the_row_still_gets_one() {
        let b = buttons(&["Check for updates"]);
        assert_eq!(button_rows(&b, 4), vec![0..1]);
    }

    #[test]
    fn a_row_that_does_not_fit_still_reports_its_rects() {
        // A short vector would panic on a modal that reserved one row for a footer that wrapped.
        let theme = Theme::default();
        let area = Rect::new(0, 0, 12, 1);
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 4));
        let b = buttons(&["Save", "Cancel"]);
        let rects = render_buttons(area, &mut buf, &b, 0, &theme);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[1].y, 2, "the second row is placed below the first");
    }

    #[test]
    fn footer_row_count_asks_at_the_width_the_frame_really_gives() {
        // With `content_w` 30 in 34 columns the modal keeps 2 columns of padding a side, so a
        // 31-column footer packs against 30 and wraps; a MIN_PAD_H shortcut would claim 32.
        let labels: &[&str] = &["aaaaaaaaaaa", "bbbbbbbbbb"];
        assert_eq!(button_row_width(labels), 31);
        assert_eq!(footer_row_count(labels, 30, 34, MAX_PAD_H), 3);
        let modal_w = 30u16.saturating_add(2 * MAX_PAD_H).min(34);
        let inner_w = modal_w - 2 * compute_pad_h(modal_w, 30, MAX_PAD_H);
        assert_eq!(inner_w, 30);
        assert_eq!(
            footer_row_count(labels, 30, 34, MAX_PAD_H),
            button_rows_height(&buttons(labels), inner_w)
        );
    }

    #[test]
    fn footer_row_count_honours_a_raised_padding_cap() {
        let labels: &[&str] = &["Cancel", "Save"];
        assert_eq!(button_row_width(labels), 20);
        assert_eq!(footer_row_count(labels, 20, 36, 4), 1);
        assert_eq!(footer_row_count(labels, 20, 36, 8), 1);
        assert_eq!(footer_row_count(labels, 20, 20, 8), 3);
    }

    #[test]
    fn packing_is_greedy_so_a_wrapped_row_refills() {
        let b = buttons(&["Ok", "Ok", "Ok"]);
        assert_eq!(button_rows(&b, 14), vec![0..2, 2..3]);
    }
}
