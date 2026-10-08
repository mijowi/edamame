//! Unified interactive controls for modal overlays: toggle, pill, text input, and button (the
//! last lives in [`super::button_row`]).  A control is a label plus a widget rendered as one
//! unit; the label owns the column padding, so a focused row's whole label column takes the
//! focus fill.  See `docs/dev/ui-controls.md` for the full design.
//!
//! Style scheme: `REVERSED` means "filled affordance", and focus is one language everywhere —
//! the `primary` fill (`REVERSED` + bold) — except the toggle, whose value-colored track would
//! lose its meaning if inverted, so it shows focus via the row label only.
//!
//! | State     | Pill / Text input        | Button (see `button_row`) | Toggle widget            |
//! | --------- | ------------------------ | ------------------------- | ------------------------ |
//! | Focused   | `primary` fill, rev, bold | `primary` fill, rev, bold | track value-colored; row label takes the fill |
//! | Unfocused | `secondary` fg, no bg    | `secondary` fill, rev     | track value-colored      |
//! | Disabled  | `text_muted` fg, no bg, dim | `text_muted` fg, no bg, dim | track desaturated (`bg_muted` fill), muted, dim |
//!
//! The modifiers keep the states distinct on a monochrome terminal; the toggle also encodes its
//! value by handle position and the literal `on`/`off` text.

use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::config::{ImagesEnabled, RemoteImagePolicy, Theme};
use crate::ui::cursor::scrolled_field_spans;

// ── Control kinds ─────────────────────────────────────────────────────────

/// How an option-valued row renders its value.  Chosen at the definition site so a two-value
/// setting that is not semantically on/off can still be a pill rather than a toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Toggle,
    /// Multi-value (2+) cycle pill over a fixed, ordered label set.
    Pill(&'static [&'static str]),
    /// Bracketed `[ label ]` chip in the value column; the label is fixed, not a config value.
    Button(&'static str),
}

// ── Control values, inputs, and events ──────────────────────────────────────
//
// The shared transition layer the modal overlays are migrating onto.  Variants not yet
// constructed outside tests carry `#[allow(dead_code)]`: `pub` only exempts a library crate's
// API, and the bin target would trip `dead_code` under `-D warnings`.

/// Normalized value a control carries, independent of the domain enum it projects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlValue {
    Toggle(bool),
    /// Index into a [`Control::Pill`]'s label slice.
    Choice(usize),
    /// A valueless [`Control::Button`]; currently built only in tests.
    #[allow(dead_code)]
    Button,
}

/// Semantic input aimed at the focused control; see [`control_input_for`] and [`Control::apply`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlInput {
    /// ←: decrement a pill / turn a toggle off.
    Left,
    /// →: increment a pill / turn a toggle on.
    Right,
    /// Enter / Space / click: flip a toggle, advance a pill, press a button.
    Activate,
}

/// What a control did with a [`ControlInput`]; `Ignored` is a no-op (e.g. ← on an off toggle).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlEvent {
    Changed(ControlValue),
    Activated,
    Ignored,
}

impl Control {
    /// Single source of truth for what an input does to a value.  Toggle arrows are
    /// direction-bound (→ always means on) and `Activate` flips; a pill wraps at both ends; a
    /// button ignores arrows.  A mismatched value shape is ignored rather than panicking.
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

/// Map a key code to a [`ControlInput`]; `None` for keys the caller handles itself.
pub fn control_input_for(code: KeyCode) -> Option<ControlInput> {
    match code {
        KeyCode::Left => Some(ControlInput::Left),
        KeyCode::Right => Some(ControlInput::Right),
        KeyCode::Enter | KeyCode::Char(' ') => Some(ControlInput::Activate),
        _ => None,
    }
}

/// Signed step for an index-valued control; also used by callers that cycle a dynamic-length
/// list via [`cycle_index`] directly.  Not for toggles, whose arrows are direction-bound.
pub fn input_delta(input: ControlInput) -> i32 {
    match input {
        ControlInput::Left => -1,
        ControlInput::Right | ControlInput::Activate => 1,
    }
}

/// Canonical tri-state labels for the image, remote-image, and diagram policies.
pub const ASK_ALWAYS_NEVER: &[&str] = &["Ask", "Always", "Never"];

// ── Shared control styles ─────────────────────────────────────────────────

/// The one focus fill shared by every control and by a focused row's label column.
pub fn focused_style(theme: &Theme) -> Style {
    theme.modal_button_focused
}

/// Resting style for an unfocused pill or text-input value.
pub fn value_unfocused_style(theme: &Theme) -> Style {
    Style::default().fg(theme.palette.secondary)
}

/// Style for an inline modal-body link (see [`crate::ui::modal_links`]).  Modal authors must
/// use this rather than `theme.link_text` directly, or the focused half drifts per modal.
pub(crate) fn link_style(focused: bool, theme: &Theme) -> Style {
    if focused {
        focused_style(theme)
    } else {
        theme.link_text
    }
}

/// Style for a disabled (cascade- or capability-locked) control.
pub fn disabled_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.palette.text_muted)
        .add_modifier(Modifier::DIM)
}

/// Value style for a text input; the caller splices the cursor block into the value itself.
pub fn text_value_style(focused: bool, theme: &Theme) -> Style {
    if focused {
        focused_style(theme)
    } else {
        value_unfocused_style(theme)
    }
}

/// Style for a control row's label column (marker + label + padding); shared by every modal
/// that lays controls out in a column, so parents never craft the style themselves.
pub fn control_label_style(focused: bool, disabled: bool, theme: &Theme) -> Style {
    if disabled {
        theme.modal_close_hint
    } else if focused {
        theme.modal_item_selected
    } else {
        theme.modal_item
    }
}

/// Compose a label column (padded to `label_col_w`, styled via [`control_label_style`]) with
/// pre-built `control` spans.  A focus marker goes inside `label`, widening `label_col_w`.
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

/// Chip style for a bracketed button; buttons are never disabled, so only focus varies.
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

/// Width of a `[ label ]` chip; must match [`super::button_row::Button`]'s width math.
pub fn button_width(label: &str) -> usize {
    label.width() + 4
}

/// Spans for an inline button chip in a row's value column; same style as a footer button.
pub fn button_spans(label: &str, focused: bool, theme: &Theme) -> Vec<Span<'static>> {
    vec![Span::styled(
        format!("[ {label} ]"),
        button_style(focused, theme),
    )]
}

// ── Text input ──────────────────────────────────────────────────────────────

/// Width of a text field sized to show each of `values` whole, never under `min`: the widest
/// value plus [`render_text_field_row`]'s two padding cells, the end-of-text cursor cell, and
/// one spare.  Computed once when a modal opens, so the field does not resize while typing; a
/// longer value scrolls.
pub fn text_field_width(values: &[&str], min: u16) -> u16 {
    let widest = values
        .iter()
        .map(|v| Line::from(*v).width())
        .max()
        .unwrap_or(0);
    u16::try_from(widest)
        .unwrap_or(u16::MAX)
        .saturating_add(4)
        .max(min)
}

/// Render a one-row labeled text input across `area`: `label` (its trailing gap included; empty
/// for a bare field) in `modal_item`, then the field, whose background runs to the right edge of
/// `area` with one cell of padding at each end.  The value scrolls through
/// [`scrolled_field_spans`] (which updates `scroll`); the cursor is drawn only when `focused` and
/// in its `cursor_visible` blink phase.
#[allow(clippy::too_many_arguments)]
pub fn render_text_field_row(
    buf: &mut Buffer,
    area: Rect,
    label: &str,
    value: &str,
    cursor: usize,
    scroll: &mut usize,
    focused: bool,
    cursor_visible: bool,
    theme: &Theme,
) {
    let value_style = text_value_style(focused, theme);
    let label = Span::styled(label.to_owned(), theme.modal_item);
    let field_w = (area.width as usize).saturating_sub(label.width() + 2);

    let mut spans = vec![label, Span::styled(" ", value_style)];
    spans.extend(scrolled_field_spans(
        value,
        cursor,
        scroll,
        field_w,
        focused && cursor_visible,
        value_style,
        theme.cursor,
    ));
    spans.push(Span::styled(" ", value_style));
    Paragraph::new(Line::from(spans))
        .style(theme.modal_bg)
        .render(area, buf);
}

/// Every row of `buf` as text, paired with how many of its cells carry a `REVERSED` fill (the
/// focus fill, from [`focused_style`]).  For tests asserting a focused field spans its width.
#[cfg(test)]
pub(crate) fn rows_with_fill(buf: &Buffer) -> Vec<(String, usize)> {
    (0..buf.area.height)
        .map(|y| {
            let cells: Vec<_> = (0..buf.area.width).map(|x| &buf[(x, y)]).collect();
            let text = cells.iter().map(|c| c.symbol()).collect();
            let filled = cells
                .iter()
                .filter(|c| c.modifier.contains(Modifier::REVERSED))
                .count();
            (text, filled)
        })
        .collect()
}

// ── Cycle / cascade logic ──────────────────────────────────────────────────

/// Step `current` by `delta`, wrapping at both ends; the single wrap-around primitive.
/// Returns `current` unchanged when `len` is 0.
pub fn cycle_index(current: usize, len: usize, delta: i32) -> usize {
    if len == 0 {
        return current;
    }
    ((current as i32 + delta).rem_euclid(len as i32)) as usize
}

/// The images→remote cascade shared by the settings overlay and welcome modal: images `Never`
/// forces remote to `Never` and stashes the prior choice; turning images back on restores it.
/// `was_never` is the value *before* the change.
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

/// Pill width over `labels`: widest label + 4 framing cells, so rows never jitter as it cycles.
pub fn pill_width(labels: &[&str]) -> usize {
    max_label_cells(labels) + 4
}

/// Spans for the pill's current value.  The arrows are always present: they advertise cycling.
pub fn pill_spans(
    labels: &[&str],
    current_index: usize,
    focused: bool,
    disabled: bool,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let slot = max_label_cells(labels);
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

/// 3-cell track + 4-cell label slot (`" on "` / `" off"`); constant so columns never jitter.
pub const TOGGLE_WIDTH: usize = 7;

pub fn toggle_width() -> usize {
    TOGGLE_WIDTH
}

/// Spans for an iOS-style on/off slider: a light `|` handle flush right when on, left when off,
/// over a value-colored fill.  `focused` is deliberately ignored — inverting the track would
/// destroy the on-is-green reading, so focus is shown by the row's label column instead.
///
/// A `disabled` toggle drops the to `bg_muted`(edamame's muted-track surface — scrollbar / table
/// stripes) with the handle one step lighter on `text_muted`, and the whole widget is `DIM` to mark it inert.
pub fn toggle_spans(on: bool, _focused: bool, disabled: bool, theme: &Theme) -> Vec<Span<'static>> {
    let p = &theme.palette;
    let label = if on { " on " } else { " off" };

    let (fill_bg, handle_bg, grip_fg, label_fg) = if disabled {
        (p.bg_muted, p.text_muted, p.bg_muted, p.text_muted)
    } else {
        let value = if on { p.success } else { p.text_muted };
        (value, p.text, p.text_muted, value)
    };
    let extra = if disabled {
        Modifier::DIM
    } else {
        Modifier::empty()
    };

    let handle = Span::styled(
        "|",
        Style::default()
            .fg(grip_fg)
            .bg(handle_bg)
            .add_modifier(extra),
    );
    let fill = Span::styled("  ", Style::default().bg(fill_bg).add_modifier(extra));
    let mut spans = if on {
        vec![fill, handle]
    } else {
        vec![handle, fill]
    };
    spans.push(Span::styled(
        label,
        Style::default().fg(label_fg).add_modifier(extra),
    ));
    spans
}

// ── Internals ───────────────────────────────────────────────────────────────

/// The widest label in cells: pill labels can be user file names (the export modal's
/// stylesheets), so a char count would size the slot short of a wide one.
fn max_label_cells(labels: &[&str]) -> usize {
    labels.iter().map(|l| l.width()).max().unwrap_or(0)
}

/// Center `label` in a `width`-cell slot, biasing extra padding to the right.
fn center(label: &str, width: usize) -> String {
    let n = label.width();
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

    /// A wide label (a user stylesheet's file name) is measured in cells: the pill keeps one
    /// width, the wide one's, as it cycles.  It once counted chars, sizing the slot 7 short.
    #[test]
    fn pill_width_counts_cells_for_wide_labels() {
        let labels: &[&str] = &["default.css", "日本語スタイル.css"];
        assert_eq!(pill_width(labels), 22);
        for idx in 0..labels.len() {
            let spans = pill_spans(labels, idx, false, false, theme());
            assert_eq!(
                UnicodeWidthStr::width(spans_text(&spans).as_str()),
                22,
                "value {idx}"
            );
        }
        assert_eq!(button_width("日本語"), 10);
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
    fn cascade_stashes_and_restores_remote() {
        let mut stash = None;
        let r = apply_images_cascade(
            ImagesEnabled::Never,
            false,
            RemoteImagePolicy::Always,
            &mut stash,
        );
        assert_eq!(r, RemoteImagePolicy::Never);
        assert_eq!(stash, Some(RemoteImagePolicy::Always));
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
        assert_eq!(
            Control::Toggle.apply(ControlValue::Toggle(false), Right),
            Changed(ControlValue::Toggle(true))
        );
        assert_eq!(
            Control::Toggle.apply(ControlValue::Toggle(true), Left),
            Changed(ControlValue::Toggle(false))
        );
        assert_eq!(
            Control::Toggle.apply(ControlValue::Toggle(true), Right),
            Ignored
        );
        assert_eq!(
            Control::Toggle.apply(ControlValue::Toggle(false), Left),
            Ignored
        );
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
        assert_eq!(
            pill.apply(ControlValue::Choice(1), Activate),
            Changed(ControlValue::Choice(2))
        );
        assert_eq!(
            pill.apply(ControlValue::Choice(2), Right),
            Changed(ControlValue::Choice(0))
        );
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
        assert_eq!(spans[0].content.chars().count(), 20);
        assert!(spans[0].content.starts_with("Show images"));
        assert_eq!(spans[0].style, control_label_style(true, false, theme));
        assert_eq!(spans[1].content.as_ref(), "‹ Ask ›");
    }

    #[test]
    fn control_row_spans_does_not_truncate_an_overlong_label() {
        let theme = theme();
        let spans = control_row_spans("A very long label", 4, Vec::new(), false, false, theme);
        assert_eq!(spans[0].content.as_ref(), "A very long label");
    }
}
