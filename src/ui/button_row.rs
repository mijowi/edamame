//! Shared rendering for the centred button row at the bottom of every
//! modal/overlay (`[ Save ]  [ Cancel ]`).  Keeps the bracket
//! formatting, focus styling, and 2-space gap consistent across
//! `modal`, `save_copy_modal`, and `insert_table_modal`.

use std::ops::Range;

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::config::Theme;
use crate::ui::scroll_container::compute_pad_h;

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

/// Column gap between two buttons on the same row.
const BUTTON_GAP: u16 = 2;

/// Split `buttons` into the rows they occupy at `width` columns, greedily
/// filling each row before starting the next.
///
/// A row that would overflow wraps rather than clipping, because a
/// clipped footer is a button the user cannot see *or* reach: the
/// keyboard still cycles focus onto it and the click rect still points
/// off the modal.  A single button wider than `width` gets a row of its
/// own and is clipped there — nothing else can be done with it, and the
/// alternative (dropping it) hides an action entirely.
///
/// Returns one index range per row, so callers can render and hit-test
/// in button order without re-deriving the packing.
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

/// Blank rows between two wrapped footer rows.  A wrapped footer reads
/// as one block of buttons stacked tight otherwise, which is exactly the
/// thing a footer must not look like — the rows are alternatives, not a
/// list.
const ROW_SPACING: u16 = 1;

/// Rows [`render_buttons`] paints for `buttons` at `width` columns,
/// including the [`ROW_SPACING`] blanks between wrapped rows.  The
/// sizing pass asks this before the modal rect exists, so it must be
/// derived from the same packing the render uses.
pub fn button_rows_height(buttons: &[Button], width: u16) -> u16 {
    let rows = button_rows(buttons, width).len() as u16;
    rows.saturating_mul(1 + ROW_SPACING)
        .saturating_sub(ROW_SPACING)
}

/// Rows a footer of `labels` needs inside a modal whose content is
/// `content_w` columns wide and whose padding caps at `max_pad_h`, laid
/// out in a terminal of `area_w`.
///
/// The caller is in the same bind [`crate::ui::ModalView`] is: the
/// footer's width is only known once the frame exists, and the frame's
/// height depends on the footer.  Both resolve it by running the real
/// sizing arithmetic — `modal_dimensions_for`'s width clamp followed by
/// [`compute_pad_h`] — so the reservation and the packing in
/// [`render_buttons`] can never disagree.  Reproducing it by hand with a
/// flat [`crate::ui::MIN_PAD_H`] instead overestimates the inner width
/// by up to `2 * (max_pad_h - MIN_PAD_H)` columns, which reserves one
/// row for a footer that then wraps onto two.
///
/// `max_pad_h` is the caller's own
/// [`crate::ui::scroll_container::ContentSize::max_pad_h`] — the
/// keybinds overlay raises it, so a hardcoded [`crate::ui::MAX_PAD_H`]
/// would be wrong there.
pub fn footer_row_count(labels: &[&str], content_w: u16, area_w: u16, max_pad_h: u16) -> u16 {
    let buttons: Vec<Button> = labels.iter().map(|l| Button::bracketed(l)).collect();
    let modal_w = content_w.saturating_add(2 * max_pad_h).min(area_w);
    let pad_h = compute_pad_h(modal_w, content_w, max_pad_h);
    let inner_w = modal_w.saturating_sub(2 * pad_h).max(1);
    button_rows_height(&buttons, inner_w).max(1)
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
    let mut rects = Vec::with_capacity(buttons.len());
    for (row_idx, row) in button_rows(buttons, area.width).into_iter().enumerate() {
        let y = area.y + row_idx as u16 * (1 + ROW_SPACING);
        // A row past the bottom of `area` is not painted, but its rects
        // are still produced: the return value is indexed by button, and
        // a caller that reserved only one row (the bespoke overlays,
        // whose footers fit at any width worth using) must not find its
        // vector short of the index it knows a button by.
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

        // Mirror Paragraph's centred layout: the row's own width,
        // starting at the centred offset inside `area`.  Each button
        // occupies its own width, with a `BUTTON_GAP` gap between
        // neighbours.
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

/// Render a single [`Button`] left-aligned at the start of `area` (rather
/// than centred like [`render_buttons`]), filling the row with the modal
/// background.  Returns the button's absolute rect for hit-testing.
///
/// Used where a button reads as an inline affordance pinned to the body's
/// left edge rather than a centred footer row — e.g. the welcome modal's
/// "Switch theme" button.  Shares the bracket formatting, width math, and
/// `controls::button_style` focus styling with the rest of this module so
/// callers never hand-roll a button.
///
/// A `disabled` button renders in the shared disabled style
/// (`controls::control_label_style`, so it reads the same as a disabled
/// control row); the caller is responsible for ignoring its rect.
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
    use crate::ui::scroll_container::MAX_PAD_H;

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
        // "[ Save ]" is 8 and "[ Cancel ]" is 10, so 2 columns short of
        // the 20 the pair needs.
        let b = buttons(&["Save", "Cancel"]);
        assert_eq!(button_rows(&b, 18), vec![0..1, 1..2]);
        // Two button rows and the blank between them.
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
        // Clipped, but present: dropping it would hide the action
        // outright, and its focus / click rect must stay in step with
        // the index the caller knows it by.
        let b = buttons(&["Check for updates"]);
        assert_eq!(button_rows(&b, 4), vec![0..1]);
    }

    #[test]
    fn a_row_that_does_not_fit_still_reports_its_rects() {
        // Callers index the result by button; a short vector would panic
        // on a modal that reserved one row for a footer that wrapped.
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
        // The reservation has to run the frame's own arithmetic, not a
        // flat MIN_PAD_H shortcut: with `content_w` 30 in 34 columns the
        // modal keeps 2 columns of padding a side, so the footer packs
        // against 30 — not the 32 a MIN_PAD_H subtraction would claim.
        // Reserving against 32 puts a 31-column footer on one row while
        // the render wraps it onto two, leaving the second unpainted.
        let labels: &[&str] = &["aaaaaaaaaaa", "bbbbbbbbbb"];
        assert_eq!(button_row_width(labels), 31);
        assert_eq!(footer_row_count(labels, 30, 34, MAX_PAD_H), 3);
        // Cross-check against the real sizing path at the same numbers.
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
        // The keybinds overlay raises `max_pad_h` to 8, which takes 16
        // columns out of the footer's width — a hardcoded MAX_PAD_H
        // would reserve one row for a footer that wraps onto two.
        let labels: &[&str] = &["Cancel", "Save"];
        assert_eq!(button_row_width(labels), 20);
        assert_eq!(footer_row_count(labels, 20, 36, 4), 1);
        assert_eq!(footer_row_count(labels, 20, 36, 8), 1);
        // In a terminal that cannot afford the raised padding *and* the
        // row, both agree it wraps.
        assert_eq!(footer_row_count(labels, 20, 20, 8), 3);
    }

    #[test]
    fn packing_is_greedy_so_a_wrapped_row_refills() {
        let b = buttons(&["Ok", "Ok", "Ok"]);
        // Each is 6 wide; 14 columns fit two with the gap, not three.
        assert_eq!(button_rows(&b, 14), vec![0..2, 2..3]);
    }
}
