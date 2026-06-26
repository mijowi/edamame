//! Settings overlay.
//!
//! Friendly, curated subset of `config.toml` editable in-place.  The
//! overlay is intentionally narrower than the file: anything that's
//! truly esoteric (developer logging, modal handler name, image cell
//! ceilings) stays in the file where users who care can edit it
//! directly.  The in-app overlay should always be simpler than
//! reading the TOML.
//!
//! Layout:
//!
//! ```text
//! Open Config folder              <config_dir>
//! Open config.toml in default editor
//!
//! Autosave                        |   off
//!   Automatically save changes when idle
//!
//!   Char limit                     100
//! Maximum content width in characters when limit is on
//! ...
//! ```
//!
//! Editable rows are listed alphabetically by label, except `Show line
//! numbers`, which sits below the two image-visibility rows so the image
//! group stays contiguous.  The remote-images row is locked (greyed +
//! skipped, in the muted hint color) while `Show images` is `Never`,
//! mirroring the welcome modal's cascade.
//!
//! The first two rows are "open externally" actions — the folder row
//! shells `xdg-open` (or the OS equivalent) on the config directory;
//! the file row suspends the TUI and runs `$VISUAL`/`$EDITOR` on
//! `config.toml`.  A blank divider separates them from the editable
//! settings beneath.
//!
//! Each row's description appears beneath it only when the row is
//! focused, conserving vertical space.  Booleans render as on/off toggle
//! sliders and enum-valued fields as cycle pills — both change on
//! Left/Right (or Enter).  Numeric fields are text inputs that are
//! editable the moment the row has focus (no "press Enter to begin"); the
//! draft commits on Enter or when focus leaves the row, and an invalid
//! draft reverts on leave.

mod rows;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::config::{Config, ImagesEnabled, RemoteImagePolicy, Theme};
use crate::ui::content_width::{max_row_width, optional_text_width};
use crate::ui::controls::{self, PillStyle};
use crate::ui::overlay_nav::next_focusable;

/// Width of the label column in the settings overlay (column count of the
/// padded `{label:<LABEL_PAD$}` slot before the value column begins).
/// Wide enough to fit the longest setting label without clipping; narrow
/// enough to leave room for the value on terminals around 80 columns.
const LABEL_PAD: usize = 28;
use crate::ui::scroll_container::{
    centered_rect_for_content, draw_frame, ContentSize, FrameOpts, ModalKind, ScrollContainerState,
    VERTICAL_CHROME_ROWS,
};

use self::rows::{build_rows, RowAction, RowDef};

// Re-exports for the bin-only `app::modal::settings` live-update wiring.
// The lib itself never reads them, so allow(dead_code) on the helper
// suppresses the otherwise-spurious `cargo clippy --lib` warning.
#[allow(unused_imports)]
pub(crate) use self::rows::{
    HEADER_NOTE, LABEL_BIG_H1, LABEL_BLINK_CURSOR, LABEL_SCROLL_SPEED, LABEL_VIM_MODE,
    LABEL_VISUAL_LINE_NAV,
};

/// All row labels in display order, including non-focusable dividers.
/// Used by the App-level live-update wiring tests in
/// `app/modal/settings.rs` so that adding a new row to `build_rows`
/// becomes an explicit, reviewable change at the App layer too.
#[allow(dead_code)]
pub(crate) fn all_row_labels() -> Vec<&'static str> {
    build_rows().into_iter().map(|r| r.label).collect()
}

/// Outcome of dispatching a key event to the settings overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsResponse {
    Continue,
    Cancelled,
    /// User chose `Open config.toml in default editor` — caller
    /// should suspend the TUI and run `$VISUAL`/`$EDITOR`, falling
    /// back to `open::that(&config_path)`.
    OpenInExternalEditor,
    /// User chose the new top-row "Open Config folder" entry —
    /// caller should `open::that(&config_dir)` so the OS file
    /// manager surfaces the folder.  Distinct from
    /// `OpenInExternalEditor` because this path doesn't need to
    /// suspend the TUI: `xdg-open` returns immediately and edamame
    /// stays in the foreground.
    OpenConfigFolder,
    /// A field changed.  The overlay's [`Config`] reference has
    /// already been mutated; the caller is expected to call
    /// `Config::save` and flash a `Configuration updated`
    /// notification.  Carries the human-readable label of the
    /// changed field.
    FieldChanged(&'static str),
}

/// Mutable state for an open settings overlay.
pub struct SettingsState {
    /// Index into [`Self::rows`].  May land on a divider after
    /// rebuild; [`Self::clamp_focus`] re-snaps to the next selectable
    /// row.
    pub focused: usize,
    /// Editable draft for the focused text-input (numeric) row.  Seeded
    /// by [`Self::open_draft_for_focused`] whenever focus lands on such a
    /// row (so it's editable on focus) and committed at a boundary (Enter
    /// or focus-leave); `None` on toggle / pill / action rows.
    pub editing: Option<String>,
    /// Last error from a rejected edit.  Cleared on the next
    /// successful edit / cancel.
    pub last_error: Option<String>,
    /// Empty placeholder — kept so the row table's `read` / `cycle`
    /// function pointers can continue to take `&[String]`.  Theme
    /// selection moved out of the settings overlay into a dedicated
    /// `Action::SwitchTheme` modal, so no live row needs this list
    /// anymore.
    pub theme_names: Vec<String>,
    /// Vertical scroll bookkeeping for the row table.  Up/Down move
    /// `focused` and pull the viewport via `ensure_visible`; PgUp/PgDn
    /// and the mouse wheel drive `scroll_state.scroll` directly without
    /// touching focus.
    pub scroll_state: ScrollContainerState,
    /// Absolute terminal rect of the rendered `esc` close hint.
    pub esc_button_rect: Option<Rect>,
    /// Cached "remote policy before the images→Never cascade" so that
    /// flipping `Show images` back out of `Never` restores the user's
    /// prior `Show remote images` choice.  Mirrors the welcome modal's
    /// `pre_cascade_remote` (see `ui::welcome`).
    pre_cascade_remote: Option<RemoteImagePolicy>,
    rows: Vec<RowDef>,
}

impl SettingsState {
    pub fn new() -> Self {
        let mut state = Self {
            focused: 0,
            editing: None,
            last_error: None,
            theme_names: Vec::new(),
            scroll_state: ScrollContainerState::default(),
            esc_button_rect: None,
            pre_cascade_remote: None,
            rows: build_rows(),
        };
        // Default focus to the first editable setting rather than the
        // "open externally" pair at the top.  Most users open the
        // overlay to tweak a setting; the externals are still one Up
        // arrow away.  Picking the first Cycle/Edit row keeps this
        // correct as the (alphabetized) row order changes.
        state.focused = state
            .rows
            .iter()
            .position(|r| {
                r.kind.focusable && matches!(r.kind.action, RowAction::Cycle | RowAction::Edit)
            })
            .or_else(|| state.first_focusable_index())
            .unwrap_or(0);
        state
    }

    /// Apply a key event, possibly mutating `config`.
    ///
    /// A numeric (text-input) row is *editable as soon as it has focus* —
    /// there is no "press Enter to begin": [`Self::open_draft_for_focused`]
    /// seeds an editable draft whenever focus lands on such a row, and
    /// typing edits the draft in place.  The draft is **committed** (parsed,
    /// validated, written to `config`) only at a boundary — Enter, or when
    /// focus leaves the row — so a multi-keystroke value still produces a
    /// single config write / flash.  An invalid draft is reverted when
    /// focus leaves; Esc closes the overlay and abandons any uncommitted
    /// draft.  Toggle / pill rows change immediately on Left/Right (or
    /// Enter), like before.
    pub fn handle_key(&mut self, key: &KeyEvent, config: &mut Config) -> SettingsResponse {
        // PgUp/PgDn/Home/End move the viewport without touching focus or
        // the open draft.
        if self.scroll_state.handle_paging_key(key) {
            return SettingsResponse::Continue;
        }

        match key.code {
            KeyCode::Esc => {
                // Abandon any uncommitted text draft and close.
                self.editing = None;
                self.last_error = None;
                SettingsResponse::Cancelled
            }
            KeyCode::Up => self.move_focus_committing(-1, config),
            KeyCode::Down => self.move_focus_committing(1, config),
            KeyCode::Left => self.cycle_focused(config, -1),
            KeyCode::Right => self.cycle_focused(config, 1),
            KeyCode::Enter => self.activate_focused(config),
            KeyCode::Backspace => {
                if let Some(buf) = self.editing.as_mut() {
                    buf.pop();
                    self.last_error = None;
                }
                SettingsResponse::Continue
            }
            KeyCode::Char(c) => {
                use crossterm::event::KeyModifiers;
                if let Some(buf) = self.editing.as_mut() {
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    {
                        buf.push(c);
                        self.last_error = None;
                    }
                }
                SettingsResponse::Continue
            }
            _ => SettingsResponse::Continue,
        }
    }

    /// Insert a bracketed paste into the focused row's editable draft, if
    /// one is open.  No-op on toggle / pill rows (cycled, not typed into).
    /// The paste is flattened to one line and length-capped by
    /// [`crate::ui::sanitize_paste`]; the value is still validated when the
    /// draft commits via the row's `write_string`.
    pub fn paste(&mut self, text: &str) {
        if let Some(buf) = self.editing.as_mut() {
            buf.push_str(&crate::ui::sanitize_paste(text));
            self.last_error = None;
        }
    }

    /// Enter on the focused row: open externally, cycle a toggle / pill, or
    /// commit a text-input draft in place (staying on the row).
    fn activate_focused(&mut self, config: &mut Config) -> SettingsResponse {
        let action = self.rows.get(self.focused).map(|r| r.kind.action);
        match action {
            Some(RowAction::OpenExternalEditor) => SettingsResponse::OpenInExternalEditor,
            Some(RowAction::OpenConfigFolder) => SettingsResponse::OpenConfigFolder,
            Some(RowAction::Cycle) => self.cycle_focused(config, 1),
            Some(RowAction::Edit) => self.commit_draft(config),
            None => SettingsResponse::Continue,
        }
    }

    /// Commit the focused text-input row's draft to `config`.  Returns
    /// [`SettingsResponse::FieldChanged`] when a *changed* draft validates
    /// and is written (the draft is refreshed to the normalized value, so
    /// the row stays editable); on a validation error the draft is kept and
    /// `last_error` set; an unchanged draft is a no-op.  No-op on non-edit
    /// rows.
    fn commit_draft(&mut self, config: &mut Config) -> SettingsResponse {
        let draft = match self.editing.as_deref() {
            Some(d) => d.to_owned(),
            None => return SettingsResponse::Continue,
        };
        let (is_edit, label, read, write_string) = match self.rows.get(self.focused) {
            Some(r) => (
                matches!(r.kind.action, RowAction::Edit),
                r.label,
                r.kind.read,
                r.kind.write_string,
            ),
            None => return SettingsResponse::Continue,
        };
        if !is_edit || draft == read(config, &self.theme_names) {
            return SettingsResponse::Continue;
        }
        match write_string(config, &draft) {
            Ok(()) => {
                self.last_error = None;
                self.editing = Some(read(config, &self.theme_names));
                SettingsResponse::FieldChanged(label)
            }
            Err(e) => {
                self.last_error = Some(e);
                SettingsResponse::Continue
            }
        }
    }

    /// Seed (or clear) the editable draft for the currently-focused row:
    /// a text-input row gets its current value as an editable draft, every
    /// other row clears the draft.  Called whenever focus settles on a new
    /// row so text inputs are editable on focus without an explicit "begin
    /// edit" step.
    pub(super) fn open_draft_for_focused(&mut self, config: &Config) {
        self.editing = match self.rows.get(self.focused) {
            Some(r) if matches!(r.kind.action, RowAction::Edit) => {
                Some((r.kind.read)(config, &self.theme_names))
            }
            _ => None,
        };
    }

    /// Cycle the focused row's value by `delta` (-1 / +1).  Only
    /// applies to bool/enum/theme rows; no-op on numeric/edit-only
    /// rows so an accidental Left arrow doesn't surprise the user.
    /// A disabled row (e.g. the cascade-locked remote-images row) never
    /// cycles.  Cycling `Show images` cascades the remote-images policy
    /// to / from `Never`, matching the welcome modal.
    fn cycle_focused(&mut self, config: &mut Config, delta: i32) -> SettingsResponse {
        // Copy the bits we need before mutating `config` / `self` so the
        // borrow of `self.rows` doesn't outlive the row's cycle call.
        let (label, cycle, disabled) = match self.rows.get(self.focused) {
            Some(r) => (r.label, r.kind.cycle, r.is_disabled(config)),
            None => return SettingsResponse::Continue,
        };
        if disabled {
            return SettingsResponse::Continue;
        }
        let was_images_never = matches!(config.images.enabled, ImagesEnabled::Never);
        let cycled = match cycle {
            Some(f) => f(config, delta, &self.theme_names),
            None => false,
        };
        if cycled && label == rows::LABEL_SHOW_IMAGES {
            self.apply_images_cascade(config, was_images_never);
        }
        if cycled {
            SettingsResponse::FieldChanged(label)
        } else {
            SettingsResponse::Continue
        }
    }

    /// Apply the images→remote cascade after `Show images` changed.
    /// Delegates to the shared [`cycle_pill::apply_images_cascade`] so
    /// the welcome modal and the settings overlay can't drift.
    fn apply_images_cascade(&mut self, config: &mut Config, was_never: bool) {
        config.images.remote_policy = controls::apply_images_cascade(
            config.images.enabled,
            was_never,
            config.images.remote_policy,
            &mut self.pre_cascade_remote,
        );
    }

    /// Move focus by `delta`, committing the row being left.  The current
    /// text-input draft (if any) is committed first — a valid change is
    /// written and surfaces as [`SettingsResponse::FieldChanged`]; an
    /// invalid or unchanged draft is silently dropped (revert-on-leave).
    /// Focus then moves and the new row's draft is opened.
    fn move_focus_committing(&mut self, delta: i32, config: &mut Config) -> SettingsResponse {
        let committed = self.commit_draft(config);
        // Leaving the row abandons any still-uncommitted (invalid) draft
        // and its error.
        self.editing = None;
        self.last_error = None;
        self.move_focus(delta, config);
        self.open_draft_for_focused(config);
        committed
    }

    fn move_focus(&mut self, delta: i32, config: &Config) {
        if let Some(idx) = next_focusable(&self.rows, self.focused, delta, |r| {
            r.focus_eligible(config)
        }) {
            self.focused = idx;
            self.scroll_state.ensure_visible(self.focused as u16);
        }
    }

    fn first_focusable_index(&self) -> Option<usize> {
        self.rows.iter().position(|r| r.kind.focusable)
    }
}

impl Default for SettingsState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── View ──────────────────────────────────────────────────────────────────

pub struct SettingsView<'a> {
    pub theme: &'a Theme,
    pub config: &'a Config,
    pub cursor_visible: bool,
}

impl<'a> StatefulWidget for SettingsView<'a> {
    type State = SettingsState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Build per-row spans (without the inline description) so we
        // can size the modal and figure out which rows fit on screen.
        let row_lines = build_row_lines(state, self.config, self.theme, self.cursor_visible);
        let content_width = settings_content_width(state, self.config);

        // Pinned-bottom region: 1 description row when the focused row
        // has one, plus 2 rows for the error footer (blank + ✗ msg).
        let focused_row = state.rows.get(state.focused);
        let focused_desc = focused_row.and_then(|r| r.resolved_description(self.config));
        let has_description = focused_desc.is_some();
        let pinned_bottom: u16 = (if has_description { 1 } else { 0 })
            + (if state.last_error.is_some() { 2 } else { 0 });

        let content = ContentSize {
            width: content_width,
            height: row_lines.len() as u16,
            pinned_top: 0,
            pinned_bottom,
            ..Default::default()
        };
        let rect = centered_rect_for_content(content, area);

        // Pre-compute layout so the title's arrow indicator reflects
        // the post-observe scroll bounds.  Vertical chrome is fixed by
        // `draw_frame`; the body area below it holds the scroll list +
        // pinned footer.
        let inner_h = rect.height.saturating_sub(VERTICAL_CHROME_ROWS);
        let table_height = inner_h.saturating_sub(pinned_bottom);
        state
            .scroll_state
            .observe(row_lines.len() as u16, table_height);
        state.scroll_state.ensure_visible(state.focused as u16);

        let layout = draw_frame(
            rect,
            buf,
            FrameOpts {
                title: "Settings",
                kind: ModalKind::Normal,
                show_close_hint: true,
                content,
                theme: self.theme,
            },
        );
        state.esc_button_rect = layout.esc_hit_rect;
        let inner = layout.body;
        if inner.height < 2 || inner.width == 0 {
            return;
        }

        let scroll = state.scroll_state.scroll as usize;
        let visible_rows = table_height as usize;

        let table_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: table_height,
        };
        let visible: Vec<Line<'_>> = row_lines
            .into_iter()
            .skip(scroll)
            .take(visible_rows)
            .collect();
        Paragraph::new(visible)
            .style(self.theme.modal_bg)
            .render(table_area, buf);
        if state.scroll_state.max_scroll() > 0 {
            let bar_area = Rect {
                x: layout.scrollbar_col,
                y: table_area.y,
                width: 1,
                height: table_area.height,
            };
            crate::ui::scrollbar::render_for_scroll_state(
                bar_area,
                &state.scroll_state,
                self.theme,
                buf,
            );
        }

        // Pinned footer: description (when present) followed by error.
        let mut footer_y = inner.y + table_height;
        if has_description {
            if let Some(desc) = focused_desc.as_deref() {
                let desc_area = Rect {
                    x: inner.x,
                    y: footer_y,
                    width: inner.width,
                    height: 1,
                };
                // No leading indent: the description left-aligns with the
                // header note ("Common options shown below …") at the body
                // edge rather than under the row labels.
                Paragraph::new(Line::from(Span::styled(
                    desc.to_owned(),
                    self.theme.modal_description,
                )))
                .style(self.theme.modal_bg)
                .render(desc_area, buf);
                footer_y += 1;
            }
        }
        if let Some(err) = state.last_error.as_ref() {
            // Blank spacer row, then the error.
            let err_area = Rect {
                x: inner.x,
                y: footer_y + 1,
                width: inner.width,
                height: 1,
            };
            Paragraph::new(Line::from(Span::styled(
                format!("✗ {err}"),
                self.theme.transient_error,
            )))
            .style(self.theme.modal_bg)
            .render(err_area, buf);
        }
    }
}

/// Build one display line per row.  The focused row's description is
/// *not* included here — it's pinned into the modal's footer instead.
fn build_row_lines<'a>(
    state: &SettingsState,
    config: &Config,
    theme: &'a Theme,
    cursor_visible: bool,
) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(state.rows.len());
    for (idx, row) in state.rows.iter().enumerate() {
        if !row.kind.focusable && row.label.is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        if !row.kind.focusable && row.label == HEADER_NOTE {
            lines.push(Line::from(Span::styled(
                row.label.to_owned(),
                theme.modal_close_hint,
            )));
            continue;
        }
        let focused = idx == state.focused;
        let editing = focused && state.editing.is_some();
        let disabled = row.is_disabled(config);

        // The label column (marker + padding) is one unit per the control
        // scheme: when the row is focused the whole column takes the focus
        // fill — for a toggle that's the only place focus shows, since the
        // toggle widget keeps its value color.
        let marker = if focused { "› " } else { "  " };
        let label_padded = format!("{marker}{:<pad$}", row.label, pad = LABEL_PAD);
        let label_style = if disabled {
            theme.modal_close_hint
        } else if focused {
            theme.modal_item_selected
        } else {
            theme.modal_item
        };
        let mut spans: Vec<Span<'static>> = vec![Span::styled(label_padded, label_style)];

        if let Some(pill) = row.kind.options {
            // Option rows render the current value as a toggle (on/off
            // slider) or a multi-value pill, chosen by the pill's style.
            let current = (row.kind.read)(config, &state.theme_names);
            let current_index = pill
                .labels
                .iter()
                .position(|l| l.eq_ignore_ascii_case(&current))
                .unwrap_or(0);
            match pill.style {
                // ON_OFF maps `true -> 0`, so index 0 is the "on" state.
                PillStyle::Toggle => spans.extend(controls::toggle_spans(
                    current_index == 0,
                    focused,
                    disabled,
                    theme,
                )),
                PillStyle::Cycle => spans.extend(controls::pill_spans(
                    pill.labels,
                    current_index,
                    focused,
                    disabled,
                    theme,
                )),
            }
        } else if editing {
            // Focused text-input row: render the live, editable draft with
            // a distinctly-colored (accent `theme.cursor`) blink-stable
            // block cursor at the append-only end, so the cursor itself is
            // the "type here" signal and never blends into the focus fill.
            let draft = state.editing.as_deref().unwrap_or("");
            let cursor = draft.chars().count();
            spans.extend(crate::ui::cursor::text_field_spans(
                draft,
                cursor,
                cursor_visible,
                controls::text_value_style(true, theme),
                theme.cursor,
            ));
        } else {
            // Unfocused text input or an external-action row: the value
            // styled as a text-input control (focus fill when focused,
            // `secondary` foreground at rest).
            let value = (row.kind.read)(config, &state.theme_names);
            spans.push(Span::styled(
                value,
                controls::text_value_style(focused, theme),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Content-aware width: max over rows of `marker(2) + label_pad +
/// value_w`, plus the longest description so the pinned-footer copy
/// doesn't get clipped.  Sizes against the *whole* row set so the
/// modal width doesn't jiggle as focus moves.
fn settings_content_width(state: &SettingsState, config: &Config) -> u16 {
    const FOCUS_MARKER_WIDTH: usize = 2;
    let row_max = max_row_width(&state.rows, |r| {
        if !r.kind.focusable && r.label == HEADER_NOTE {
            return r.label.chars().count();
        }
        let value_w = match r.kind.options {
            Some(pill) => match pill.style {
                PillStyle::Toggle => controls::toggle_width(),
                PillStyle::Cycle => controls::pill_width(pill.labels),
            },
            None => (r.kind.read)(config, &state.theme_names).chars().count(),
        };
        FOCUS_MARKER_WIDTH + LABEL_PAD + value_w
    });
    // The description is left-aligned at the body edge (no indent), so
    // size against its raw length.  Resolve it so a dynamic description
    // (e.g. the blink cadence) can't clip.
    let desc_max = max_row_width(&state.rows, |r| {
        r.resolved_description(config)
            .map(|d| d.chars().count())
            .unwrap_or(0)
    });
    let err_max = optional_text_width(state.last_error.as_deref(), 2);
    row_max.max(desc_max).max(err_max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ImagesEnabled, RemoteImagePolicy};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn focus_row(state: &mut SettingsState, config: &Config, label: &str) {
        let idx = state
            .rows
            .iter()
            .position(|r| r.label == label)
            .unwrap_or_else(|| panic!("missing row {label}"));
        state.focused = idx;
        // Mirror real navigation: landing on a row opens its draft so a
        // text-input row is editable on focus.
        state.open_draft_for_focused(config);
    }

    #[test]
    fn cycle_toggles_use_visual_line_navigation() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "Use visual line navigation");
        assert!(config.editor.visual_line_nav); // default true
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert!(matches!(resp, SettingsResponse::FieldChanged(_)));
        assert!(!config.editor.visual_line_nav);
    }

    #[test]
    fn cycle_toggles_vim_mode_handler() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "Vim mode");
        assert_eq!(config.modal.handler, "default"); // default: off
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert!(matches!(resp, SettingsResponse::FieldChanged(_)));
        assert_eq!(config.modal.handler, "vim");
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(config.modal.handler, "default");
    }

    #[test]
    fn cycle_advances_show_images_through_ask_always_never() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "Show images");
        assert_eq!(config.images.enabled, ImagesEnabled::Ask);
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(config.images.enabled, ImagesEnabled::Always);
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(config.images.enabled, ImagesEnabled::Never);
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(config.images.enabled, ImagesEnabled::Ask);
    }

    #[test]
    fn editor_max_width_is_editable_on_focus_and_round_trips() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        // No "Enter to begin": focusing the row opens its draft.
        focus_row(&mut state, &config, "  Char limit");
        assert_eq!(state.editing.as_deref(), Some("100"));
        for _ in 0..3 {
            state.handle_key(&key(KeyCode::Backspace), &mut config);
        }
        for c in "200".chars() {
            state.handle_key(&key(KeyCode::Char(c)), &mut config);
        }
        // Enter commits in place and the row stays editable.
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert!(matches!(resp, SettingsResponse::FieldChanged(_)));
        assert_eq!(config.editor.max_width_cols, 200);
        assert_eq!(state.editing.as_deref(), Some("200"));
    }

    #[test]
    fn text_input_commits_on_focus_leave() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "  Char limit");
        for _ in 0..3 {
            state.handle_key(&key(KeyCode::Backspace), &mut config);
        }
        for c in "200".chars() {
            state.handle_key(&key(KeyCode::Char(c)), &mut config);
        }
        // Leaving the row (without Enter) commits the valid draft.
        let resp = state.handle_key(&key(KeyCode::Up), &mut config);
        assert!(matches!(resp, SettingsResponse::FieldChanged(_)));
        assert_eq!(config.editor.max_width_cols, 200);
        assert_ne!(state.rows[state.focused].label, "  Char limit");
    }

    #[test]
    fn invalid_draft_reverts_on_focus_leave() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "  Char limit");
        for _ in 0..3 {
            state.handle_key(&key(KeyCode::Backspace), &mut config);
        }
        for c in "abc".chars() {
            state.handle_key(&key(KeyCode::Char(c)), &mut config);
        }
        // Leaving with an invalid draft silently reverts — no write, no
        // lingering error.
        let resp = state.handle_key(&key(KeyCode::Up), &mut config);
        assert_eq!(resp, SettingsResponse::Continue);
        assert_eq!(config.editor.max_width_cols, 100);
        assert!(state.last_error.is_none());
    }

    #[test]
    fn enter_on_open_config_folder_row_emits_open_config_folder() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "Open config folder");
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(resp, SettingsResponse::OpenConfigFolder);
    }

    #[test]
    fn enter_on_config_toml_row_emits_open_external_editor() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "Open config.toml in default editor");
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(resp, SettingsResponse::OpenInExternalEditor);
    }

    #[test]
    fn default_focus_is_first_editable_row() {
        // Most users open Settings to adjust a setting, not the
        // externals.  Default focus skips past the open-externally
        // pair (and the divider) and lands on the first editable row
        // (alphabetically, "Autosave").
        let state = SettingsState::new();
        assert_eq!(state.rows[state.focused].label, "Autosave");
    }

    #[test]
    fn arrow_navigation_skips_divider_row() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        // Default focus is the first editable row — Up must skip the
        // blank divider and land on "Open config.toml in default editor".
        state.handle_key(&key(KeyCode::Up), &mut config);
        assert_eq!(
            state.rows[state.focused].label,
            "Open config.toml in default editor"
        );
        state.handle_key(&key(KeyCode::Up), &mut config);
        assert_eq!(state.rows[state.focused].label, "Open config folder");
    }

    #[test]
    fn left_right_cycle_show_remote_images() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "  Show remote images");
        assert_eq!(config.images.remote_policy, RemoteImagePolicy::Ask);
        state.handle_key(&key(KeyCode::Right), &mut config);
        assert_eq!(config.images.remote_policy, RemoteImagePolicy::Always);
        state.handle_key(&key(KeyCode::Left), &mut config);
        assert_eq!(config.images.remote_policy, RemoteImagePolicy::Ask);
    }

    #[test]
    fn invalid_inline_value_is_rejected() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "  Char limit");
        // Replace `100` with garbage (draft is already open on focus).
        for _ in 0..3 {
            state.handle_key(&key(KeyCode::Backspace), &mut config);
        }
        for c in "abc".chars() {
            state.handle_key(&key(KeyCode::Char(c)), &mut config);
        }
        // Enter on an invalid draft flags the error and keeps the draft
        // (the row stays editable so the user can fix it).
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert!(matches!(resp, SettingsResponse::Continue));
        assert!(state.last_error.is_some());
        assert_eq!(config.editor.max_width_cols, 100); // unchanged
        assert_eq!(state.editing.as_deref(), Some("abc"));
    }

    #[test]
    fn escape_cancels_overlay_when_not_editing() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        let resp = state.handle_key(&key(KeyCode::Esc), &mut config);
        assert_eq!(resp, SettingsResponse::Cancelled);
    }

    #[test]
    fn escape_closes_overlay_and_abandons_uncommitted_draft() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "  Char limit");
        // Type an uncommitted change, then Esc.
        for c in "9".chars() {
            state.handle_key(&key(KeyCode::Char(c)), &mut config);
        }
        assert!(state.editing.is_some());
        let resp = state.handle_key(&key(KeyCode::Esc), &mut config);
        // Esc closes the overlay (it no longer just cancels an edit) and
        // the uncommitted draft is dropped — config is untouched.
        assert_eq!(resp, SettingsResponse::Cancelled);
        assert!(state.editing.is_none());
        assert_eq!(config.editor.max_width_cols, 100);
    }

    #[test]
    fn rows_match_curated_list() {
        // The phase 10 review pinned the exact set of rows.  Lock it
        // in here so adding a new row to `build_rows` becomes an
        // explicit, reviewable change.  The empty entry between the
        // "open externally" pair and the editable settings is the
        // non-focusable divider row.
        let labels: Vec<&str> = build_rows().iter().map(|r| r.label).collect();
        assert_eq!(
            labels,
            vec![
                rows::HEADER_NOTE,
                "",
                "Open config folder",
                "Open config.toml in default editor",
                "",
                // Editable settings, alphabetized by label — except
                // "Show line numbers", grouped below the image rows.
                "Autosave",
                "Big H1 headings",
                "Blink cursor",
                "Limit editor width",
                "  Char limit",
                "Scroll speed",
                "Show diagrams",
                "Show images",
                "  Show remote images",
                "Show line numbers",
                "Show table buttons",
                "Use visual line navigation",
                "Vim mode",
            ]
        );
    }

    #[test]
    fn dropped_legacy_rows_are_absent() {
        // The review removed several rows: tab_width, line_wrap,
        // code_block_wrap, preserve_blank_lines,
        // suppress_capability_warnings, images.max_width/max_height,
        // dev.logging.  Confirm none accidentally come back via a
        // future schema rebase.
        let labels: Vec<&str> = build_rows().iter().map(|r| r.label).collect();
        for stale in [
            "editor.tab_width",
            "editor.line_wrap",
            "editor.code_block_wrap",
            "editor.preserve_blank_lines",
            "editor.suppress_capability_warnings",
            "images.max_width",
            "images.max_height",
            "dev.logging",
            // Removed from the overlay: hint duration and diff intro are
            // file-only now, and the export toggles moved to the
            // export-flow modal.
            "Hint duration",
            "Diff intro",
            "Export inlined images",
            "Export diagrams as SVG",
        ] {
            assert!(
                !labels.contains(&stale),
                "stale row '{stale}' is still in the schema"
            );
        }
    }

    // ── Scroll-container integration ────────────────────────────────────

    use ratatui::{backend::TestBackend, Terminal};

    fn theme_ref() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn render(state: &mut SettingsState, config: &Config, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_stateful_widget(
                    SettingsView {
                        theme: theme_ref(),
                        config,
                        cursor_visible: true,
                    },
                    frame.area(),
                    state,
                );
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect()
    }

    #[test]
    fn settings_renders_scrollbar_when_more_rows_than_visible_height() {
        let config = Config::default();
        let mut state = SettingsState::new();
        // 80 cols × 8 rows: only ~5 row slots after frame + footer.
        // Settings has 13 rows.
        let contents = render(&mut state, &config, 80, 12);
        assert!(
            contents.contains('█'),
            "expected scrollbar thumb glyph, got: {contents}"
        );
    }

    #[test]
    fn settings_pgdown_advances_scroll_without_moving_focus() {
        let config = Config::default();
        let mut state = SettingsState::new();
        render(&mut state, &config, 80, 12);
        let focused_before = state.focused;
        state.handle_key(&key(KeyCode::PageDown), &mut Config::default());
        assert_eq!(state.focused, focused_before, "PgDn must not move focus");
        assert!(state.scroll_state.scroll > 0, "PgDn must advance scroll");
    }

    #[test]
    fn settings_wheel_scrolls_list() {
        let config = Config::default();
        let mut state = SettingsState::new();
        render(&mut state, &config, 80, 12);
        let focused_before = state.focused;
        state.scroll_state.scroll_by(2);
        assert_eq!(state.scroll_state.scroll, 2);
        assert_eq!(state.focused, focused_before);
    }

    #[test]
    fn settings_modal_width_shrinks_to_content_in_wide_terminal() {
        let config = Config::default();
        let mut state = SettingsState::new();
        let term_w = 200u16;
        let term_h = 30u16;
        let contents = render(&mut state, &config, term_w, term_h);
        let max_border = (0..term_h)
            .map(|y| {
                let row: String = contents
                    .chars()
                    .skip((y as usize) * term_w as usize)
                    .take(term_w as usize)
                    .collect();
                row.chars().filter(|&c| c == '─').count()
            })
            .max()
            .unwrap_or(0);
        let modal_width = max_border + 2;
        // 80% of 200 would be 160; content is much narrower.
        assert!(
            modal_width < 130,
            "expected content-aware width well below 80% of 200, got modal width {modal_width}"
        );
    }

    #[test]
    fn option_row_marks_current_value_with_focused_style_when_focused() {
        let config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "Show images");
        let theme = theme_ref();
        let lines = build_row_lines(&state, &config, theme, true);
        let row = lines
            .iter()
            .find(|l| {
                l.spans
                    .first()
                    .is_some_and(|s| s.content.contains("Show images"))
            })
            .expect("Show images row");
        let ask_pill = row
            .spans
            .iter()
            .find(|s| s.content.contains("Ask"))
            .unwrap();
        assert_eq!(ask_pill.style, theme.modal_button_focused);
    }

    #[test]
    fn settings_description_appears_in_pinned_footer() {
        let config = Config::default();
        let mut state = SettingsState::new();
        // Default focus is on the first editable row ("Autosave"),
        // which has a description.
        let contents = render(&mut state, &config, 100, 25);
        assert!(
            contents.contains("Automatically save"),
            "expected focused-row description in pinned footer, got: {contents}"
        );
    }

    #[test]
    fn blink_cursor_description_embeds_config_cadence() {
        // The "Blink cursor" row's description is dynamic: it reads the
        // file-only `cursor_blink_ms` so the hint reflects the user's
        // configured cadence rather than a hardcoded value.
        let mut config = Config::default();
        config.editor.cursor_blink_ms = 777;
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "Blink cursor");
        let contents = render(&mut state, &config, 100, 30);
        assert!(
            contents.contains("Blink cursor every 777 ms"),
            "expected dynamic blink description, got: {contents}"
        );
    }

    #[test]
    fn blink_cursor_row_toggles_config_flag() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "Blink cursor");
        assert!(config.editor.cursor_blink); // default on
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert!(matches!(resp, SettingsResponse::FieldChanged(_)));
        assert!(!config.editor.cursor_blink);
    }

    #[test]
    fn show_images_never_cascades_remote_to_never_and_locks_row() {
        // Mirrors the welcome modal: cycling Show images to Never forces
        // remote to Never, disables the remote row, and skips it on
        // navigation; leaving Never restores the prior remote choice.
        let mut config = Config::default();
        config.images.remote_policy = RemoteImagePolicy::Always;
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "Show images");
        // Ask → Always → Never.
        state.handle_key(&key(KeyCode::Enter), &mut config);
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(config.images.enabled, ImagesEnabled::Never);
        assert_eq!(config.images.remote_policy, RemoteImagePolicy::Never);

        // The remote row is now disabled and skipped by Down navigation.
        let remote_idx = state
            .rows
            .iter()
            .position(|r| r.label == "  Show remote images")
            .unwrap();
        assert!(state.rows[remote_idx].is_disabled(&config));
        state.focused = remote_idx - 1; // row just above the locked one
        state.handle_key(&key(KeyCode::Down), &mut config);
        assert_ne!(state.focused, remote_idx, "Down must skip the locked row");

        // Leave Never (Never → Ask) — prior remote choice is restored.
        focus_row(&mut state, &config, "Show images");
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(config.images.enabled, ImagesEnabled::Ask);
        assert_eq!(config.images.remote_policy, RemoteImagePolicy::Always);
        assert!(!state.rows[remote_idx].is_disabled(&config));
    }
}
