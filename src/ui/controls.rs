//! Unified interactive controls for modal overlays.
//!
//! Edamame's controls share one visual language so a settings row, a
//! prompt field, and a button all read as members of the same family.
//! Each *control* is a label plus an interactive widget rendered as one
//! unit, and the owning container (e.g. the settings overlay) aligns a
//! column of them by reserving a fixed label width — the label owns the
//! padding, so when a row is focused the whole label column (marker +
//! padding) takes the focus fill, the widget included.
//!
//! There are four control flavors:
//!
//! - **Toggle** — an on/off slider: a 3-cell track with a sliding 1-cell
//!   handle (a light `text` cell with a `|` grip mark) plus an external `on`/`off` text label.  The fill behind
//!   the handle is `success` when on and `text_muted` when off; the label
//!   takes the same value color.  The toggle is the one control
//!   whose *widget* does not change on focus — focus is shown only by the
//!   row's label column (see [`toggle_spans`]).
//! - **Pill** — a multi-value (2+) selector shown as the current value
//!   framed by `‹ value ›` arrows, cycled with ←/→.  The arrows mark it
//!   as cycle-able and distinguish it from a bracketed button.
//! - **Text input** — an inline editable value.
//! - **Button** — a press-to-act target (usually label-less: the label
//!   *is* the value inside the widget).  Lives in [`super::button_row`].
//!
//! ## Style scheme
//!
//! One rule ties the family together: `REVERSED` means "filled
//! affordance".  Focus is one language everywhere — a `primary` fill
//! (`REVERSED` + bold) — except the toggle, whose value-colored track
//! would lose its meaning if inverted.
//!
//! | State     | Pill / Text input        | Button (see `button_row`) | Toggle widget            |
//! | --------- | ------------------------ | ------------------------- | ------------------------ |
//! | Focused   | `primary` fill, rev, bold | `primary` fill, rev, bold | track value-colored; row label takes the fill |
//! | Unfocused | `secondary` fg, no bg    | `secondary` fill, rev     | track value-colored      |
//! | Disabled  | `text_muted` fg, no bg, dim | `text_muted` fg, no bg, dim | track no bg, muted    |
//!
//! Color-independent modifiers (`REVERSED` / `BOLD` / `DIM`) keep the
//! states distinct on a monochrome terminal where bg/fg collapse; the
//! toggle additionally encodes its value by handle position and the literal
//! `on`/`off` text.
//!
//! The option-set data ([`Pill`], [`ON_OFF`], [`ASK_ALWAYS_NEVER`]) and
//! the cycle / cascade logic ([`cycle_enum`], [`apply_images_cascade`])
//! are re-exported from [`super::cycle_pill`] so callers have a single
//! import path; that module still owns the legacy chip rendering used by
//! the welcome modal until it migrates onto this scheme.

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::config::Theme;

pub use super::cycle_pill::{
    apply_images_cascade, cycle_enum, Pill, PillStyle, ASK_ALWAYS_NEVER, ON_OFF,
};

// ── Shared control styles ─────────────────────────────────────────────────

/// Focus fill shared by every control's *focused* state (and by a
/// focused row's label column): a `primary` fill built as
/// `fg(primary)` + `REVERSED` + `BOLD`, so it fills in color and
/// reverse-videos in monochrome.
pub fn focused_style(theme: &Theme) -> Style {
    theme.modal_button_focused
}

/// Resting style for an unfocused pill or text-input value: `secondary`
/// foreground, no fill, no bold.  Sits directly on the modal surface.
pub fn value_unfocused_style(theme: &Theme) -> Style {
    Style::default().fg(theme.palette.secondary)
}

/// Style for a disabled (cascade- or capability-locked) control:
/// `text_muted` foreground, no fill, dimmed.
pub fn disabled_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.palette.text_muted)
        .add_modifier(Modifier::DIM)
}

/// Value style for a text input: the focus fill when focused (the cursor
/// block, when editing, is spliced into the value string by the caller),
/// the resting `secondary` foreground otherwise.
pub fn text_value_style(focused: bool, theme: &Theme) -> Style {
    if focused {
        focused_style(theme)
    } else {
        value_unfocused_style(theme)
    }
}

// ── Pill ──────────────────────────────────────────────────────────────────

/// Total rendered width (in cells) of a pill over `labels`: the widest
/// label plus the four framing cells (`‹ `…` ›`).  Independent of the
/// current value and focus so a row never jitters as the value cycles.
pub fn pill_width(labels: &[&str]) -> usize {
    max_label_chars(labels) + 4
}

/// Build the styled spans for the pill's current value.  `current_index`
/// selects the displayed label; `focused` is whether the owning row has
/// focus; `disabled` renders the pill inert.  The arrows are always
/// present — they advertise that the value cycles.
pub fn pill_spans(
    labels: &[&str],
    current_index: usize,
    focused: bool,
    disabled: bool,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let slot = max_label_chars(labels);
    let label = labels.get(current_index).copied().unwrap_or("");
    let text = format!("‹ {} ›", center(label, slot));
    let style = if disabled {
        disabled_style(theme)
    } else if focused {
        focused_style(theme)
    } else {
        value_unfocused_style(theme)
    };
    vec![Span::styled(text, style)]
}

// ── Toggle ──────────────────────────────────────────────────────────────────

/// Fixed rendered width of a toggle: a 3-cell track plus a 4-cell label
/// slot (`" on "` / `" off"`).  Constant so a column of toggles aligns
/// and never jitters as the value flips.
pub const TOGGLE_WIDTH: usize = 7;

/// Total rendered width (in cells) of a toggle.  See [`TOGGLE_WIDTH`].
pub fn toggle_width() -> usize {
    TOGGLE_WIDTH
}

/// Build the styled spans for an on/off toggle slider.
///
/// The 3-cell track is a 1-cell handle — a solid `text`-colored cell
/// carrying a `|` grip mark in slightly darker `text_muted` — plus 2 cells of
/// colored fill (`success` when on, `text_muted` when off), with the
/// handle flush right when on and flush left when off, so the colored
/// fill reads as the "behind the switch" surface (iOS-style).  The light
/// handle keeps it unambiguous which cell is the handle, and never
/// vanishes against the off track.  The external label (`on` / `off`) carries the same
/// value color with no fill.  `focused` is intentionally ignored by the
/// widget: a toggle's track keeps its value color even when focused
/// (inverting it would destroy the on-is-green reading), so focus is
/// surfaced by the row's label column instead.  `disabled` drops the
/// fill and dims the handle + label; the handle position still encodes
/// the value.
pub fn toggle_spans(on: bool, _focused: bool, disabled: bool, theme: &Theme) -> Vec<Span<'static>> {
    let p = &theme.palette;
    let label = if on { " on " } else { " off" };

    if disabled {
        let muted = Style::default()
            .fg(p.text_muted)
            .add_modifier(Modifier::DIM);
        // No fill: the empty cells show nothing, the dim `|` handle alone
        // marks the position.
        let track = if on { "  |" } else { "|  " };
        return vec![Span::styled(track, muted), Span::styled(label, muted)];
    }

    let value = if on { p.success } else { p.text_muted };
    // A solid light `text` handle carrying a `|` grip mark in a slightly
    // darker `text_muted` fg — reads as a grippable handle without letting
    // the colored track show through.
    let handle = Span::styled("|", Style::default().fg(p.text_muted).bg(p.text));
    let fill = Span::styled("  ", Style::default().bg(value));
    // Handle flush right when on, flush left when off.
    let mut spans = if on {
        vec![fill, handle]
    } else {
        vec![handle, fill]
    };
    spans.push(Span::styled(label, Style::default().fg(value)));
    spans
}

// ── Internals ───────────────────────────────────────────────────────────────

fn max_label_chars(labels: &[&str]) -> usize {
    labels.iter().map(|l| l.chars().count()).max().unwrap_or(0)
}

/// Center `label` within a `width`-char slot, biasing extra padding to
/// the right.  Returns the label unchanged when it already fills the slot.
fn center(label: &str, width: usize) -> String {
    let n = label.chars().count();
    if n >= width {
        return label.to_owned();
    }
    let pad = width - n;
    let left = pad / 2;
    format!("{}{}{}", " ".repeat(left), label, " ".repeat(pad - left))
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::*;
    use crate::config::Theme;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn spans_text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn pill_width_is_stable_across_value_and_focus() {
        let labels = ASK_ALWAYS_NEVER.labels;
        let want = pill_width(labels);
        for idx in 0..labels.len() {
            for focused in [true, false] {
                let spans = pill_spans(labels, idx, focused, false, theme());
                assert_eq!(
                    UnicodeWidthStr::width(spans_text(&spans).as_str()),
                    want,
                    "value {idx} focused={focused}",
                );
            }
        }
    }

    #[test]
    fn toggle_width_is_constant_across_value() {
        for on in [true, false] {
            let spans = toggle_spans(on, false, false, theme());
            assert_eq!(
                UnicodeWidthStr::width(spans_text(&spans).as_str()),
                TOGGLE_WIDTH,
                "on={on}",
            );
        }
    }

    #[test]
    fn disabled_toggle_drops_the_fill() {
        let spans = toggle_spans(true, false, true, theme());
        assert_eq!(spans[0].style.bg, None);
        assert!(spans[0].style.add_modifier.contains(Modifier::DIM));
    }
}
