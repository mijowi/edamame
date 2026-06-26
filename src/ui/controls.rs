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
//! The option-set data ([`Control`], [`ASK_ALWAYS_NEVER`]) and the cycle /
//! cascade logic ([`cycle_enum`], [`apply_images_cascade`]) live here too,
//! so every interactive control has a single import path.

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::config::{ImagesEnabled, RemoteImagePolicy, Theme};

// ── Control kinds ─────────────────────────────────────────────────────────

/// How an option-valued row renders its current value.  Chosen at the
/// definition site so a two-value setting that is *not* semantically
/// on/off can still cycle as a pill rather than collapse into a toggle.
///
/// On/off is no longer a pill flavor — a binary setting uses the dedicated
/// [`toggle_spans`] slider via [`Control::Toggle`].  A pill is reserved for
/// genuine multi-value (2+) choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// Binary on/off, rendered as the toggle slider.  The value is read as
    /// `"on"` / `"off"`; `"on"` is the enabled state.
    Toggle,
    /// Multi-value (2+) cycle pill over a fixed, ordered label set.
    Pill(&'static [&'static str]),
}

/// Canonical `Ask` / `Always` / `Never` tri-state (image, remote-image,
/// and diagram policies).  Shared by the settings overlay and the welcome
/// modal so the labels can't drift.
pub const ASK_ALWAYS_NEVER: &[&str] = &["Ask", "Always", "Never"];

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

/// Style for a control row's *label column* (marker + label + padding),
/// the single source of truth shared by every modal that lays controls
/// out in a labeled column.  A row is one unit: when it's focused the
/// whole label column takes the `primary` focus fill — so the parent only
/// has to say whether the row is `focused` / `disabled`, never craft the
/// style itself.
///
/// - **Focused** → `modal_item_selected` (filled `primary`, inverse text,
///   bold) — the same fill the control widget shows, so label and widget
///   read as a single focused control.
/// - **Disabled** → `modal_close_hint` (muted, no fill).
/// - **Resting** → `modal_item` (plain text on the modal surface).
pub fn control_label_style(focused: bool, disabled: bool, theme: &Theme) -> Style {
    if disabled {
        theme.modal_close_hint
    } else if focused {
        theme.modal_item_selected
    } else {
        theme.modal_item
    }
}

/// Chip style for a bracketed action button (`[ Save ]`): the shared
/// `primary` focus fill when focused, a resting `text`-on-`surface` chip
/// (BOLD to read as "live" in monochrome) otherwise.  Buttons in the
/// modal button rows are never disabled, so only the focus axis varies.
pub fn button_style(focused: bool, theme: &Theme) -> Style {
    if focused {
        focused_style(theme)
    } else {
        Style::default()
            .fg(theme.palette.text)
            .bg(theme.palette.surface)
            .add_modifier(Modifier::BOLD)
    }
}

// ── Cycle / cascade logic ──────────────────────────────────────────────────

/// Cycle `current` through `order` by `delta` (signed step), wrapping at
/// both ends.  Falls back to the first element when `current` isn't found
/// in `order`; returns `current` unchanged for an empty `order`.  Shared
/// by every pill caller so the wrap-around math lives in one place.
pub fn cycle_enum<T: PartialEq + Copy>(current: T, order: &[T], delta: i32) -> T {
    if order.is_empty() {
        return current;
    }
    let i = order.iter().position(|v| *v == current).unwrap_or(0) as i32;
    let n = order.len() as i32;
    order[((i + delta).rem_euclid(n)) as usize]
}

/// Apply the images→remote cascade and return the remote policy to store.
///
/// Centralizes the rule shared by the settings overlay and the welcome
/// modal: turning images *off* (`Never`) forces remote images to `Never`
/// while stashing the prior choice in `pre_cascade_remote`; turning
/// images back *on* restores that stashed choice.  `was_never` is the
/// value of `images.enabled` *before* the change.
pub fn apply_images_cascade(
    new_images: ImagesEnabled,
    was_never: bool,
    current_remote: RemoteImagePolicy,
    pre_cascade_remote: &mut Option<RemoteImagePolicy>,
) -> RemoteImagePolicy {
    let now_never = matches!(new_images, ImagesEnabled::Never);
    if !was_never && now_never {
        *pre_cascade_remote = Some(current_remote);
        RemoteImagePolicy::Never
    } else if was_never && !now_never {
        pre_cascade_remote.take().unwrap_or(current_remote)
    } else {
        current_remote
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
    use crate::config::{ImagesEnabled, RemoteImagePolicy, Theme};

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn spans_text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn pill_width_is_stable_across_value_and_focus() {
        let labels = ASK_ALWAYS_NEVER;
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

    #[test]
    fn cascade_stashes_and_restores_remote() {
        let mut stash = None;
        // Images on -> off: remote forced Never, prior stashed.
        let r = apply_images_cascade(
            ImagesEnabled::Never,
            false,
            RemoteImagePolicy::Always,
            &mut stash,
        );
        assert_eq!(r, RemoteImagePolicy::Never);
        assert_eq!(stash, Some(RemoteImagePolicy::Always));
        // Images off -> on: prior restored, stash cleared.
        let r = apply_images_cascade(
            ImagesEnabled::Ask,
            true,
            RemoteImagePolicy::Never,
            &mut stash,
        );
        assert_eq!(r, RemoteImagePolicy::Always);
        assert_eq!(stash, None);
    }

    #[test]
    fn cascade_noop_when_never_unchanged() {
        let mut stash = None;
        let r = apply_images_cascade(
            ImagesEnabled::Always,
            false,
            RemoteImagePolicy::Ask,
            &mut stash,
        );
        assert_eq!(r, RemoteImagePolicy::Ask);
        assert_eq!(stash, None);
    }
}
