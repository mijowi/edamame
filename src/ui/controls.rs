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

use crossterm::event::KeyCode;
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
    /// Press-to-act target rendered as a bracketed `[ label ]` chip in the
    /// value column (e.g. an "Open externally" action row).  Activated with
    /// Enter; the label is fixed (it does not reflect a config value).
    Button(&'static str),
}

// ── Control values, inputs, and events ──────────────────────────────────────
//
// These types and the `Control::apply` / `control_input_for` /
// `control_row_spans` helpers below are the shared transition layer the
// modal overlays migrate onto over the phased controls refactor (see
// `docs/controls-refactor.md`).  The export-HTML modal is the first consumer
// (Phase 1); a few variants not yet *constructed* in non-test code carry a
// variant-level `#[allow(dead_code)]` until a later phase wires them — the
// bin target re-includes these modules (`main.rs` declares `mod ui;`), so an
// unconstructed variant would otherwise trip `dead_code` under `clippy
// --all-targets -D warnings` (`pub` only exempts a *library* crate's API).

/// Normalized value a control carries, independent of the domain enum it
/// projects (`ImagesEnabled`, a bool config field, a stylesheet index, …).
/// The owning modal converts to/from this when it reads a control's current
/// value and writes back the result of an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlValue {
    /// On/off value for a [`Control::Toggle`].
    Toggle(bool),
    /// Selected index into a [`Control::Pill`]'s label slice.  Constructed by
    /// the welcome modal's pill rows, fed through [`Control::apply`], and
    /// mapped back to the domain enum.  (Export's stylesheet pill is
    /// dynamic-label and cycles via [`cycle_index`], not `apply`.)
    Choice(usize),
    /// A valueless [`Control::Button`].  Constructed by a button caller
    /// (the settings overlay, Phase 3); until then it is built only in tests.
    #[allow(dead_code)]
    Button,
}

/// A semantic input aimed at the focused control.  The parent maps raw
/// key/mouse events to these (see [`control_input_for`]); the control maps
/// these to a value change (see [`Control::apply`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlInput {
    /// ←: decrement a pill / turn a toggle off.
    Left,
    /// →: increment a pill / turn a toggle on.
    Right,
    /// Enter / Space / click: flip a toggle, advance a pill, press a button.
    Activate,
}

/// What a control did with a [`ControlInput`].  `Changed` carries the new
/// value to write back; `Activated` fires a button; `Ignored` means the
/// input was a no-op (e.g. ← on an already-off toggle, or any arrow on a
/// button).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlEvent {
    Changed(ControlValue),
    Activated,
    Ignored,
}

impl Control {
    /// Single source of truth for "what does this input do to this value".
    ///
    /// - **Toggle:** `Left` → off, `Right` → on, `Activate` → flip.  Arrows
    ///   are direction-bound (so → always means *on*); a press that doesn't
    ///   change the value returns [`ControlEvent::Ignored`].
    /// - **Pill:** `Left` → −1, `Right` / `Activate` → +1, wrapping at both
    ///   ends.  A single-label pill (no other value to move to) is a no-op.
    /// - **Button:** `Activate` → [`ControlEvent::Activated`]; arrows are
    ///   ignored.
    ///
    /// A value whose shape doesn't match the control kind (e.g. a
    /// `Choice` handed to a `Toggle`) is ignored rather than panicking.
    pub fn apply(&self, current: ControlValue, input: ControlInput) -> ControlEvent {
        match (self, current) {
            (Control::Toggle, ControlValue::Toggle(on)) => {
                let next = match input {
                    ControlInput::Left => false,
                    ControlInput::Right => true,
                    ControlInput::Activate => !on,
                };
                if next == on {
                    ControlEvent::Ignored
                } else {
                    ControlEvent::Changed(ControlValue::Toggle(next))
                }
            }
            (Control::Pill(labels), ControlValue::Choice(i)) => {
                if labels.len() < 2 {
                    return ControlEvent::Ignored;
                }
                let next = cycle_index(i, labels.len(), input_delta(input));
                if next == i {
                    ControlEvent::Ignored
                } else {
                    ControlEvent::Changed(ControlValue::Choice(next))
                }
            }
            (Control::Button(_), _) => match input {
                ControlInput::Activate => ControlEvent::Activated,
                _ => ControlEvent::Ignored,
            },
            _ => ControlEvent::Ignored,
        }
    }
}

/// Map a key code to the [`ControlInput`] it drives on the focused control.
/// Returns `None` for keys the caller handles itself (Tab / Esc / typing),
/// so a modal's `handle_key` can route control input through one match arm
/// instead of repeating the Left/Right/Enter/Space arms per field.
pub fn control_input_for(code: KeyCode) -> Option<ControlInput> {
    match code {
        KeyCode::Left => Some(ControlInput::Left),
        KeyCode::Right => Some(ControlInput::Right),
        KeyCode::Enter | KeyCode::Char(' ') => Some(ControlInput::Activate),
        _ => None,
    }
}

/// The signed cycle step a [`ControlInput`] drives on an *index-valued*
/// control (a pill, or any [`cycle_index`] caller): `Left` → −1, `Right` /
/// `Activate` → +1.  Shared by [`Control::apply`]'s pill arm and by callers
/// that cycle a dynamic-length list directly (e.g. the export-HTML
/// stylesheet pill), so the direction mapping lives in one place.  Not used
/// for a toggle, whose arrows are direction-bound to a bool (and whose
/// `Activate` flips) rather than stepping an index.
pub fn input_delta(input: ControlInput) -> i32 {
    match input {
        ControlInput::Left => -1,
        ControlInput::Right | ControlInput::Activate => 1,
    }
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

/// Compose a `label` column + `control` widget into one row's spans, the
/// single label+control composition shared by every modal that lays out a
/// labeled control.  The label is left-padded to `label_col_w` cells and
/// styled via [`control_label_style`] (so a focused row's fill spans the
/// whole label column up to the widget), then the caller's already-built
/// `control` spans are appended.  Callers that prefix a focus marker pass it
/// inside `label` and widen `label_col_w` to match.
pub fn control_row_spans(
    label: &str,
    label_col_w: usize,
    control: Vec<Span<'static>>,
    focused: bool,
    disabled: bool,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let label_padded = format!("{label:<label_col_w$}");
    let mut spans = vec![Span::styled(
        label_padded,
        control_label_style(focused, disabled, theme),
    )];
    spans.extend(control);
    spans
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

// ── Button ──────────────────────────────────────────────────────────────────

/// Rendered width (in cells) of an inline `[ label ]` button chip: the
/// label plus the four framing cells (`[ `…` ]`).  Matches
/// [`super::button_row::Button`]'s width math so the two stay aligned.
pub fn button_width(label: &str) -> usize {
    label.chars().count() + 4
}

/// Build the styled span(s) for an inline button chip rendered in a
/// control row's value column.  Shares [`button_style`] (and therefore the
/// `[ … ]` focus fill) with the centred [`super::button_row`] helpers so a
/// settings-row button reads identically to a footer button.
pub fn button_spans(label: &str, focused: bool, theme: &Theme) -> Vec<Span<'static>> {
    vec![Span::styled(
        format!("[ {label} ]"),
        button_style(focused, theme),
    )]
}

// ── Cycle / cascade logic ──────────────────────────────────────────────────

/// Step a `current` index through `len` slots by `delta` (signed), wrapping
/// at both ends.  The single wrap-around primitive: [`cycle_enum`] and
/// [`Control::apply`]'s pill arm both delegate here, and callers that cycle a
/// *dynamic*-length list by index (e.g. the export-HTML stylesheet pill,
/// whose labels aren't `'static`) call it directly.  Returns `current`
/// unchanged when `len` is 0.
pub fn cycle_index(current: usize, len: usize, delta: i32) -> usize {
    if len == 0 {
        return current;
    }
    ((current as i32 + delta).rem_euclid(len as i32)) as usize
}

/// Cycle `current` through `order` by `delta` (signed step), wrapping at
/// both ends.  Falls back to the first element when `current` isn't found
/// in `order`; returns `current` unchanged for an empty `order`.  Shared
/// by every pill caller so the wrap-around math lives in one place.
pub fn cycle_enum<T: PartialEq + Copy>(current: T, order: &[T], delta: i32) -> T {
    if order.is_empty() {
        return current;
    }
    let i = order.iter().position(|v| *v == current).unwrap_or(0);
    order[cycle_index(i, order.len(), delta)]
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

    // ── Control::apply ──────────────────────────────────────────────────

    #[test]
    fn apply_toggle_is_direction_bound_with_activate_flip() {
        use ControlEvent::*;
        use ControlInput::*;
        // → always means on; ← always means off.
        assert_eq!(
            Control::Toggle.apply(ControlValue::Toggle(false), Right),
            Changed(ControlValue::Toggle(true))
        );
        assert_eq!(
            Control::Toggle.apply(ControlValue::Toggle(true), Left),
            Changed(ControlValue::Toggle(false))
        );
        // A press that doesn't change the value is a no-op.
        assert_eq!(
            Control::Toggle.apply(ControlValue::Toggle(true), Right),
            Ignored
        );
        assert_eq!(
            Control::Toggle.apply(ControlValue::Toggle(false), Left),
            Ignored
        );
        // Activate flips regardless of current value.
        assert_eq!(
            Control::Toggle.apply(ControlValue::Toggle(false), Activate),
            Changed(ControlValue::Toggle(true))
        );
        assert_eq!(
            Control::Toggle.apply(ControlValue::Toggle(true), Activate),
            Changed(ControlValue::Toggle(false))
        );
    }

    #[test]
    fn apply_pill_cycles_and_wraps_both_ways() {
        use ControlEvent::*;
        use ControlInput::*;
        let pill = Control::Pill(ASK_ALWAYS_NEVER); // len 3
        assert_eq!(
            pill.apply(ControlValue::Choice(0), Right),
            Changed(ControlValue::Choice(1))
        );
        // Activate advances like Right.
        assert_eq!(
            pill.apply(ControlValue::Choice(1), Activate),
            Changed(ControlValue::Choice(2))
        );
        // Wrap forward off the end…
        assert_eq!(
            pill.apply(ControlValue::Choice(2), Right),
            Changed(ControlValue::Choice(0))
        );
        // …and backward off the start.
        assert_eq!(
            pill.apply(ControlValue::Choice(0), Left),
            Changed(ControlValue::Choice(2))
        );
    }

    #[test]
    fn apply_single_label_pill_is_a_noop() {
        let pill = Control::Pill(&["Only"]);
        assert_eq!(
            pill.apply(ControlValue::Choice(0), ControlInput::Right),
            ControlEvent::Ignored
        );
    }

    #[test]
    fn apply_button_activates_only_on_activate() {
        let btn = Control::Button("Open");
        assert_eq!(
            btn.apply(ControlValue::Button, ControlInput::Activate),
            ControlEvent::Activated
        );
        assert_eq!(
            btn.apply(ControlValue::Button, ControlInput::Left),
            ControlEvent::Ignored
        );
        assert_eq!(
            btn.apply(ControlValue::Button, ControlInput::Right),
            ControlEvent::Ignored
        );
    }

    #[test]
    fn apply_ignores_mismatched_value_shape() {
        // A Choice handed to a Toggle (and vice versa) is ignored, not a panic.
        assert_eq!(
            Control::Toggle.apply(ControlValue::Choice(1), ControlInput::Activate),
            ControlEvent::Ignored
        );
        assert_eq!(
            Control::Pill(ASK_ALWAYS_NEVER).apply(ControlValue::Toggle(true), ControlInput::Right),
            ControlEvent::Ignored
        );
    }

    // ── control_input_for ───────────────────────────────────────────────

    #[test]
    fn control_input_for_maps_the_control_keys() {
        assert_eq!(control_input_for(KeyCode::Left), Some(ControlInput::Left));
        assert_eq!(control_input_for(KeyCode::Right), Some(ControlInput::Right));
        assert_eq!(
            control_input_for(KeyCode::Enter),
            Some(ControlInput::Activate)
        );
        assert_eq!(
            control_input_for(KeyCode::Char(' ')),
            Some(ControlInput::Activate)
        );
        // Keys the caller handles itself fall through.
        assert_eq!(control_input_for(KeyCode::Tab), None);
        assert_eq!(control_input_for(KeyCode::Esc), None);
        assert_eq!(control_input_for(KeyCode::Char('x')), None);
    }

    // ── input_delta ─────────────────────────────────────────────────────

    #[test]
    fn input_delta_steps_left_back_and_right_or_activate_forward() {
        assert_eq!(input_delta(ControlInput::Left), -1);
        assert_eq!(input_delta(ControlInput::Right), 1);
        assert_eq!(input_delta(ControlInput::Activate), 1);
    }

    // ── control_row_spans ───────────────────────────────────────────────

    #[test]
    fn control_row_spans_pads_label_and_appends_control() {
        let theme = theme();
        let control = vec![Span::raw("‹ Ask ›")];
        let spans = control_row_spans("Show images", 20, control, true, false, theme);
        // First span is the padded label column…
        assert_eq!(spans[0].content.chars().count(), 20);
        assert!(spans[0].content.starts_with("Show images"));
        assert_eq!(spans[0].style, control_label_style(true, false, theme));
        // …followed by the control widget spans.
        assert_eq!(spans[1].content.as_ref(), "‹ Ask ›");
    }

    #[test]
    fn control_row_spans_does_not_truncate_an_overlong_label() {
        let theme = theme();
        let spans = control_row_spans("A very long label", 4, Vec::new(), false, false, theme);
        // `{:<width}` only pads; it never clips, so the label survives intact.
        assert_eq!(spans[0].content.as_ref(), "A very long label");
    }
}
