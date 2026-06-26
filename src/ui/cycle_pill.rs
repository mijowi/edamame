//! Shared chip styling for on/off and multi-choice settings, plus the
//! "cycle pill" widget built on it.
//!
//! Every interactive chip — a cycle pill, a toggle, or an action button —
//! is a filled cell run.  The fill makes it read as a *control* rather
//! than plain text, and the brackets/arrows distinguish the two flavors:
//! brackets (`[ Save ]`) mean "press to act", arrows (`‹ off ›`) mean
//! "cycle to change".
//!
//! ## Style table
//!
//! Three states, each carrying a **color-independent modifier** so the
//! distinction survives a monochrome terminal (where bg/fg collapse but
//! `REVERSED` / `BOLD` / `DIM` do not):
//!
//! | Control       | Focused (REVERSED)     | Unfocused (BOLD)          | Disabled (DIM)        |
//! | ------------- | ---------------------- | ------------------------- | --------------------- |
//! | Cycle pill    | `primary` fill, `bg` fg | `bg_muted` bg, `secondary` fg | `text_muted` fg, flat |
//! | "on" toggle   | `success` fill, `bg` fg | `bg_muted` bg, `success` fg | `success` fg, flat    |
//! | "off" toggle  | `text_muted` fill, `bg` fg | `bg_muted` bg, `text` fg | `text_muted` fg, flat |
//! | Button        | `primary` fill, `bg` fg | `bg_muted` bg, `text` fg  | `text_muted` fg, flat |
//!
//! Reading of the three axes:
//! - **Unfocused** is a *dark* chip (`bg_muted`, one tone below the modal
//!   surface) with a *light* foreground whose hue encodes the role —
//!   `success` for "on", `secondary` for a multi-choice value, plain
//!   `text` for "off" / a neutral button.  Normal polarity (dark bg,
//!   light fg) so it reads as a resting control, not a highlight.  `BOLD`
//!   marks "live" in monochrome.
//! - **Focused** *inverts* that into a bright fill with dark (`bg`) text.
//!   The fill hue is the role's accent — `primary` for cycle/button,
//!   `success`/`text_muted` for the on/off toggle (a toggle keeps its
//!   value identity even when focused; the row label turns `primary` to
//!   signal focus).  Built as `fg(accent)` + `REVERSED` (like
//!   `modal_button_focused`) so it fills in color and reverse-videos in
//!   monochrome.
//! - **Disabled** drops the fill (flat on the modal surface) and dims, so
//!   "chip = live control, flat-dim = inert" holds in every color depth.
//!   "on" keeps its `success` hue so a locked-on control still reads on.
//!
//! ## Cycle pill
//!
//! A cycle pill shows the *current* value of an option set as a single
//! arrow-flanked `‹ value ›` slot, changed by cycling (←/→, or
//! Space/Enter).  The arrows are always shown; the alternatives are
//! discovered by cycling, not listed inline.  Pill width is stable for a
//! given option set — every label is centered in a slot sized to the
//! widest option and the two arrow cells are always present — so a row
//! never jitters as its value cycles or focus moves.

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::config::{ImagesEnabled, RemoteImagePolicy, Theme};

/// Visual flavor for a cycle pill.  Chosen at the definition site rather
/// than inferred from the option count, so a two-option setting that is
/// not semantically on/off keeps the neutral [`PillStyle::Cycle`] look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PillStyle {
    /// Binary on/off: index 0 is the "on" state, index 1 the "off" state.
    /// Callers binding a `bool` should map `true -> 0`, `false -> 1`.
    Toggle,
    /// Neutral multi-choice cycle.  The `secondary` hue marks the selected
    /// value; no green/muted on-off coding.
    Cycle,
}

/// An option set plus the style it should render with.  Bundling the two
/// means a row stores a single value and can't pair the wrong labels with
/// the wrong treatment.
#[derive(Clone, Copy, Debug)]
pub struct Pill {
    /// Display labels, in cycle order; index 0 is the first value.
    pub labels: &'static [&'static str],
    /// Toggle (on/off color) vs. neutral multi-choice.
    pub style: PillStyle,
}

/// Canonical on/off toggle.  Index 0 is `on`, index 1 is `off`.  Callers
/// binding a `bool` map `true -> 0`.
pub const ON_OFF: Pill = Pill {
    labels: &["on", "off"],
    style: PillStyle::Toggle,
};

/// Canonical `Ask` / `Always` / `Never` tri-state (image, remote-image,
/// and diagram policies).  Shared by the settings overlay and the welcome
/// modal so the labels can't drift.
pub const ASK_ALWAYS_NEVER: Pill = Pill {
    labels: &["Ask", "Always", "Never"],
    style: PillStyle::Cycle,
};

/// Total rendered width (in cells) of `pill`.
///
/// Independent of the current value and focus state: every label is
/// padded to the widest option and framed with two cells on each side
/// (`‹ `…` ›`).  Use this to size a modal so its width can't jitter as
/// values change.
pub fn pill_width(pill: Pill) -> usize {
    max_label_chars(pill.labels) + 4
}

/// Build the styled spans for the current value of `pill` rendered as a
/// cycle pill.  `current_index` selects the displayed value; `focused`
/// is whether the owning *row* has keyboard focus; `disabled` renders the
/// pill inert (cascade- or capability-locked).
///
/// Returns a single-span `Vec` (kept as a `Vec` so callers can `extend` a
/// line's span list uniformly).  See the module-level style table.
pub fn pill_spans(
    pill: Pill,
    current_index: usize,
    focused: bool,
    disabled: bool,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let slot = max_label_chars(pill.labels);
    let label = pill.labels.get(current_index).copied().unwrap_or("");
    let centered = center(label, slot);
    // Arrows are always present — they mark the value as cycle-able and
    // distinguish it from a bracketed action button.
    let text = format!("‹ {centered} ›");

    let role = match pill.style {
        PillStyle::Toggle if current_index == 0 => ChipRole::On,
        PillStyle::Toggle => ChipRole::Off,
        PillStyle::Cycle => ChipRole::Cycle,
    };

    vec![Span::styled(
        text,
        chip_style(role, focused, disabled, theme),
    )]
}

/// Chip style for an action button: a dark `bg_muted` chip with `text`,
/// `primary` fill when focused.  Buttons in the modal button rows are
/// never disabled, so only the focus axis varies.
pub fn button_style(focused: bool, theme: &Theme) -> Style {
    chip_style(ChipRole::Button, focused, false, theme)
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

/// Cycle `current` through `order` by `delta` (signed step), wrapping at
/// both ends.  Falls back to the first element when `current` isn't found
/// in `order`; returns `current` unchanged for an empty `order`.  Shared
/// by every cycle-pill caller so the wrap-around math lives in one place.
pub fn cycle_enum<T: PartialEq + Copy>(current: T, order: &[T], delta: i32) -> T {
    if order.is_empty() {
        return current;
    }
    let i = order.iter().position(|v| *v == current).unwrap_or(0) as i32;
    let n = order.len() as i32;
    order[((i + delta).rem_euclid(n)) as usize]
}

// ── Internals ───────────────────────────────────────────────────────────────

/// Color identity of a chip.  Selects the focused fill, the unfocused
/// foreground hue, and the disabled foreground.  See the module-level
/// style table.
#[derive(Clone, Copy)]
enum ChipRole {
    /// Multi-choice cycle pill — `secondary` value, `primary` on focus.
    Cycle,
    /// A toggle's "on" value — `success` throughout.
    On,
    /// A toggle's "off" value — neutral; `text_muted` fill on focus (not
    /// `primary`, so the chip keeps its off identity and focus is read
    /// from the row label).
    Off,
    /// A neutral action button — `text` value, `primary` on focus.
    Button,
}

/// Resolve the chip style for `role` in the given focus/disabled state.
/// Each state layers a color-independent modifier (`REVERSED` / `BOLD` /
/// `DIM`) so the three stay distinct on a monochrome terminal.
fn chip_style(role: ChipRole, focused: bool, disabled: bool, theme: &Theme) -> Style {
    let p = &theme.palette;
    if focused {
        // Bright fill, inverse text.  Built as `fg(fill)` + REVERSED (like
        // `modal_button_focused`) so it fills in color terminals and
        // reverse-videos in monochrome.  Cycle/Button reuse the shared
        // focused field; On/Off mirror it with their own hue.
        return match role {
            ChipRole::Cycle | ChipRole::Button => theme.modal_button_focused,
            ChipRole::On => Style::default()
                .fg(p.success)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            ChipRole::Off => Style::default()
                .fg(p.text_muted)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),
        };
    }
    if disabled {
        // Flat on the modal surface (no chip fill), dimmed.  "on" keeps its
        // hue as a foreground so a locked-on control still reads as on.
        let fg = match role {
            ChipRole::On => p.success,
            _ => p.text_muted,
        };
        return Style::default()
            .fg(fg)
            .bg(p.surface_elevated)
            .add_modifier(Modifier::DIM);
    }
    // Unfocused, enabled: a dark `bg_muted` chip (normal polarity) whose
    // foreground hue encodes the role; BOLD marks "live" in monochrome.
    let fg = match role {
        ChipRole::Cycle => p.secondary,
        ChipRole::On => p.success,
        ChipRole::Off | ChipRole::Button => p.text,
    };
    Style::default()
        .fg(fg)
        .bg(p.surface)
        .add_modifier(Modifier::BOLD)
}

fn max_label_chars(options: &[&str]) -> usize {
    options.iter().map(|l| l.chars().count()).max().unwrap_or(0)
}

/// Center `label` within a `width`-char slot, biasing extra padding to
/// the right.  Returns the label unchanged when it already fills (or
/// overflows) the slot.
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

    fn span_text(pill: Pill, idx: usize, focused: bool, disabled: bool) -> String {
        pill_spans(pill, idx, focused, disabled, theme())[0]
            .content
            .to_string()
    }

    #[test]
    fn width_is_stable_across_value_and_focus() {
        // Every tri-state value, focused or not, occupies exactly pill_width.
        let want = pill_width(ASK_ALWAYS_NEVER);
        for idx in 0..ASK_ALWAYS_NEVER.labels.len() {
            for focused in [true, false] {
                let w = UnicodeWidthStr::width(
                    span_text(ASK_ALWAYS_NEVER, idx, focused, false).as_str(),
                );
                assert_eq!(w, want, "value {idx} focused={focused}");
            }
        }
        // Toggle likewise — `on` and `off` differ in length but pad equal.
        let bw = pill_width(ON_OFF);
        for idx in 0..ON_OFF.labels.len() {
            for focused in [true, false] {
                let w = UnicodeWidthStr::width(span_text(ON_OFF, idx, focused, false).as_str());
                assert_eq!(w, bw, "toggle {idx} focused={focused}");
            }
        }
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
