//! Settings overlay: a curated subset of `config.toml` editable in place.  Esoteric
//! options (developer logging, modal handler name, image cell ceilings) stay file-only so
//! the overlay remains simpler than the TOML.
//!
//! Rows: two "open externally" actions, a blank divider, then editable settings in
//! alphabetical order by label — except `Show line numbers`, kept below the image rows so
//! that group stays contiguous.  The remote-images row is locked while `Show images` is
//! `Never`, mirroring the welcome modal's cascade.  The focused row's description is
//! pinned into the footer.  Booleans are toggles and enums are pills (Left/Right/Enter);
//! numeric fields are text inputs editable the moment the row has focus, committing on
//! Enter or focus-leave and reverting an invalid draft on leave.

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
use crate::ui::controls::{self, Control, ControlEvent, ControlInput};
use crate::ui::overlay_nav::next_focusable;

/// Width of the padded label column; sized to fit the longest label while leaving room
/// for the value on an 80-column terminal.
const LABEL_PAD: usize = 28;
/// Width of the focus-marker column (`› ` / two spaces) before every label.
const FOCUS_MARKER_WIDTH: usize = 2;
use crate::ui::scroll_container::{
    centered_rect_for_content, draw_frame, ContentSize, FrameOpts, ModalKind, ScrollContainerState,
    VERTICAL_CHROME_ROWS,
};

use self::rows::{build_rows, RowAction, RowDef};

// Re-exports for the bin-only `app::modal::settings` live-update wiring; the lib never
// reads them, hence the allows.
#[allow(unused_imports)]
pub(crate) use self::rows::{
    HEADER_NOTE, LABEL_AUTOSAVE, LABEL_BIG_H1, LABEL_BLINK_CURSOR, LABEL_DIFF_ON_CHANGE,
    LABEL_LIMIT_WIDTH, LABEL_LINE_NUMBERS, LABEL_MATH_PREVIEW, LABEL_OUTLINE, LABEL_REFLOW,
    LABEL_SCROLL_SPEED, LABEL_SHOW_DIAGRAMS, LABEL_SHOW_IMAGES, LABEL_SHOW_REMOTE_IMAGES,
    LABEL_SYNTAX_HIGHLIGHTING, LABEL_TABLE_BUTTONS, LABEL_VIM_MODE, LABEL_VISUAL_LINE_NAV,
};

/// All row labels in display order, dividers included; pinned by the App-level
/// live-update wiring tests so a new row is a reviewable change there too.
#[allow(dead_code)]
pub(crate) fn all_row_labels() -> Vec<&'static str> {
    build_rows().into_iter().map(|r| r.label).collect()
}

/// Outcome of dispatching a key event to the settings overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsResponse {
    Continue,
    Cancelled,
    /// Caller should suspend the TUI and run `$VISUAL`/`$EDITOR` on `config.toml`.
    OpenInExternalEditor,
    /// Caller should `open::that(&config_dir)`; no TUI suspension needed since the opener
    /// returns immediately.
    OpenConfigFolder,
    /// A field changed (label carried).  `config` is already mutated; the caller saves it
    /// and flashes a notice.
    FieldChanged(&'static str),
}

/// Mutable state for an open settings overlay.
pub struct SettingsState {
    /// Index into [`Self::rows`].  Invariant: never rests on a non-focusable or
    /// cascade-locked row.  Every assignment site must check `focus_eligible` against the
    /// live config; `rows` is never rebuilt, so nothing re-snaps a stale index.
    pub focused: usize,
    /// Editable draft for the focused text-input row; `None` on toggle / pill / action rows.
    pub editing: Option<String>,
    /// Last error from a rejected edit; cleared on the next successful edit / cancel.
    pub last_error: Option<String>,
    /// Empty placeholder kept so the row table's `read` / `cycle` function pointers can keep
    /// taking `&[String]`; theme selection moved to its own modal.
    pub theme_names: Vec<String>,
    /// Up/Down move `focused` and pull the viewport; PgUp/PgDn and the wheel scroll without
    /// touching focus.
    pub scroll_state: ScrollContainerState,
    /// Absolute terminal rect of the rendered `esc` close hint.
    pub esc_button_rect: Option<Rect>,
    /// Remote policy before the images→Never cascade, restored on leaving `Never`.  Mirrors
    /// the welcome modal.
    pre_cascade_remote: Option<RemoteImagePolicy>,
    /// `(row index, rect)` per focusable row visible in the last render, absolute coords;
    /// rebuilt every frame so scrolling can't leave stale geometry.
    row_hit_rects: Vec<(usize, Rect)>,
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
            row_hit_rects: Vec::new(),
            rows: build_rows(),
        };
        // Default focus lands on the first editable setting, not the "open externally" pair.
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
    /// A text-input draft is committed only at a boundary (Enter or focus-leave) so a
    /// multi-keystroke value produces a single config write / flash; Esc closes the overlay
    /// and abandons any uncommitted draft.
    pub fn handle_key(&mut self, key: &KeyEvent, config: &mut Config) -> SettingsResponse {
        if self.scroll_state.handle_paging_key(key) {
            return SettingsResponse::Continue;
        }

        match key.code {
            KeyCode::Esc => {
                self.editing = None;
                self.last_error = None;
                SettingsResponse::Cancelled
            }
            KeyCode::Up => self.move_focus_committing(-1, config),
            KeyCode::Down => self.move_focus_committing(1, config),
            KeyCode::Left => self.apply_control_input(config, ControlInput::Left),
            KeyCode::Right => self.apply_control_input(config, ControlInput::Right),
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

    /// Insert a bracketed paste into the open draft, if any.  Sanitized by
    /// [`crate::ui::sanitize_paste`]; still validated at commit.
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
            Some(RowAction::Cycle) => self.apply_control_input(config, ControlInput::Activate),
            Some(RowAction::Edit) => self.commit_draft(config),
            None => SettingsResponse::Continue,
        }
    }

    /// Commit the focused text-input row's draft to `config`.  A changed, valid draft is
    /// written (and refreshed to the normalized value) and yields `FieldChanged`; an invalid
    /// one keeps the draft and sets `last_error`; unchanged or non-edit rows are no-ops.
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

    /// Seed the draft for a focused text-input row (or clear it for any other row).  Called
    /// whenever focus settles so text inputs are editable on focus.
    pub(super) fn open_draft_for_focused(&mut self, config: &Config) {
        self.editing = match self.rows.get(self.focused) {
            Some(r) if matches!(r.kind.action, RowAction::Edit) => {
                Some((r.kind.read)(config, &self.theme_names))
            }
            _ => None,
        };
    }

    /// Apply a [`ControlInput`] to the focused toggle / pill row via [`Control::apply`] and
    /// write the result into `config`.  No-op on numeric, button, and disabled rows.
    /// Changing `Show images` cascades the remote-images policy, matching the welcome modal.
    fn apply_control_input(
        &mut self,
        config: &mut Config,
        input: ControlInput,
    ) -> SettingsResponse {
        let (label, control, read_value, write_value, disabled) = match self.rows.get(self.focused)
        {
            Some(r) => (
                r.label,
                r.kind.options,
                r.kind.read_value,
                r.kind.write_value,
                r.is_disabled(config),
            ),
            None => return SettingsResponse::Continue,
        };
        if disabled {
            return SettingsResponse::Continue;
        }
        let (Some(control), Some(read_value), Some(write_value)) =
            (control, read_value, write_value)
        else {
            return SettingsResponse::Continue;
        };
        let was_images_never = matches!(config.images.enabled, ImagesEnabled::Never);
        match control.apply(read_value(config), input) {
            ControlEvent::Changed(next) => {
                write_value(config, next);
                if label == rows::LABEL_SHOW_IMAGES {
                    self.apply_images_cascade(config, was_images_never);
                }
                SettingsResponse::FieldChanged(label)
            }
            ControlEvent::Activated | ControlEvent::Ignored => SettingsResponse::Continue,
        }
    }

    /// Images→remote cascade after `Show images` changed; shared with the welcome modal via
    /// [`controls::apply_images_cascade`].
    fn apply_images_cascade(&mut self, config: &mut Config, was_never: bool) {
        config.images.remote_policy = controls::apply_images_cascade(
            config.images.enabled,
            was_never,
            config.images.remote_policy,
            &mut self.pre_cascade_remote,
        );
    }

    /// Move focus by `delta`, committing the row being left (an invalid or unchanged draft
    /// is dropped) and opening the new row's draft.
    fn move_focus_committing(&mut self, delta: i32, config: &mut Config) -> SettingsResponse {
        let committed = self.commit_draft(config);
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

    /// Route a click to the row whose cached hit-rect contains it: focus it (committing the
    /// row being left, as an arrow move would), then activate an option / action row.  A
    /// miss or a disabled row is a no-op.
    pub fn handle_click(&mut self, col: u16, row: u16, config: &mut Config) -> SettingsResponse {
        let Some(&(idx, _)) = self
            .row_hit_rects
            .iter()
            .find(|(_, r)| rect_contains(*r, col, row))
        else {
            return SettingsResponse::Continue;
        };
        // Eligibility may have changed since the last render (cascade lock).
        if !self
            .rows
            .get(idx)
            .map(|r| r.focus_eligible(config))
            .unwrap_or(false)
        {
            return SettingsResponse::Continue;
        }
        let committed = self.focus_clicked_row(idx, config);
        match self.rows.get(self.focused).map(|r| r.kind.action) {
            Some(RowAction::OpenExternalEditor) => SettingsResponse::OpenInExternalEditor,
            Some(RowAction::OpenConfigFolder) => SettingsResponse::OpenConfigFolder,
            Some(RowAction::Cycle) => self.apply_control_input(config, ControlInput::Activate),
            // Surface the commit of the row we left so its live-update still fires.
            Some(RowAction::Edit) | None => committed,
        }
    }

    /// Click counterpart of [`Self::move_focus_committing`] for an explicit row index.
    fn focus_clicked_row(&mut self, idx: usize, config: &mut Config) -> SettingsResponse {
        let committed = self.commit_draft(config);
        self.editing = None;
        self.last_error = None;
        self.focused = idx;
        self.scroll_state.ensure_visible(self.focused as u16);
        self.open_draft_for_focused(config);
        committed
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
        let row_lines = build_row_lines(state, self.config, self.theme, self.cursor_visible);
        let content_width = settings_content_width(state, self.config);

        // Pinned footer: one row per description line, plus blank + error row when set.
        let focused_row = state.rows.get(state.focused);
        let focused_desc = focused_row.and_then(|r| r.resolved_description(self.config));
        let desc_rows = focused_desc
            .as_deref()
            .map(|d| d.lines().count() as u16)
            .unwrap_or(0);
        let pinned_bottom: u16 = desc_rows + (if state.last_error.is_some() { 2 } else { 0 });

        let content = ContentSize {
            width: content_width,
            height: row_lines.len() as u16,
            pinned_top: 0,
            pinned_bottom,
            ..Default::default()
        };
        let rect = centered_rect_for_content(content, area);

        // Observe before draw_frame so the title's arrow indicator sees the new scroll bounds.
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

        // Hit-rects span the whole `marker + label + value` run.  A label longer than
        // `LABEL_PAD` is not truncated, so the control shifts right with it.
        let end = (scroll + visible_rows).min(state.rows.len());
        let mut hit_rects: Vec<(usize, Rect)> = Vec::new();
        for idx in scroll..end {
            let row = &state.rows[idx];
            if !row.kind.focusable {
                continue;
            }
            let label_w = row.label.chars().count().max(LABEL_PAD);
            let value_w = row_value_width(row, self.config, &state.theme_names).max(1);
            let w = ((FOCUS_MARKER_WIDTH + label_w + value_w) as u16).min(table_area.width);
            hit_rects.push((
                idx,
                Rect {
                    x: table_area.x,
                    y: table_area.y + (idx - scroll) as u16,
                    width: w,
                    height: 1,
                },
            ));
        }
        state.row_hit_rects = hit_rects;

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

        let mut footer_y = inner.y + table_height;
        if let Some(desc) = focused_desc.as_deref() {
            // No indent: the description left-aligns with the header note, not the labels.
            for line in desc.lines() {
                let desc_area = Rect {
                    x: inner.x,
                    y: footer_y,
                    width: inner.width,
                    height: 1,
                };
                Paragraph::new(Line::from(Span::styled(
                    line.to_owned(),
                    self.theme.modal_description,
                )))
                .style(self.theme.modal_bg)
                .render(desc_area, buf);
                footer_y += 1;
            }
        }
        if let Some(err) = state.last_error.as_ref() {
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

/// One display line per row; the focused row's description is pinned into the footer, not
/// included here.
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

        // The focused label column takes the focus fill — for a toggle that's the only place
        // focus shows, since the toggle keeps its value color.
        let marker = if focused { "› " } else { "  " };
        let label_padded = format!("{marker}{:<pad$}", row.label, pad = LABEL_PAD);
        let label_style = controls::control_label_style(focused, disabled, theme);
        let mut spans: Vec<Span<'static>> = vec![Span::styled(label_padded, label_style)];

        if let Some(control) = row.kind.options {
            let current = (row.kind.read)(config, &state.theme_names);
            match control {
                Control::Toggle => spans.extend(controls::toggle_spans(
                    current.eq_ignore_ascii_case("on"),
                    focused,
                    disabled,
                    theme,
                )),
                Control::Pill(labels) => {
                    let current_index = labels
                        .iter()
                        .position(|l| l.eq_ignore_ascii_case(&current))
                        .unwrap_or(0);
                    spans.extend(controls::pill_spans(
                        labels,
                        current_index,
                        focused,
                        disabled,
                        theme,
                    ));
                }
                Control::Button(label) => {
                    spans.extend(controls::button_spans(label, focused, theme));
                }
            }
        } else if editing {
            // Accent-colored block cursor at the append-only end is the "type here" signal;
            // it must not blend into the focus fill.
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

/// Content width: the widest row, description line, or error.  Sized against the *whole*
/// row set (descriptions resolved) so the modal doesn't jiggle as focus moves.
fn settings_content_width(state: &SettingsState, config: &Config) -> u16 {
    let row_max = max_row_width(&state.rows, |r| {
        if !r.kind.focusable && r.label == HEADER_NOTE {
            return r.label.chars().count();
        }
        FOCUS_MARKER_WIDTH + LABEL_PAD + row_value_width(r, config, &state.theme_names)
    });
    let desc_max = max_row_width(&state.rows, |r| {
        r.resolved_description(config)
            .map(|d| d.lines().map(|l| l.chars().count()).max().unwrap_or(0))
            .unwrap_or(0)
    });
    let err_max = optional_text_width(state.last_error.as_deref(), 2);
    row_max.max(desc_max).max(err_max)
}

/// Rendered width of a row's value column; shared by the sizing pass and the hit-rect
/// capture so the two can't disagree.
fn row_value_width(row: &RowDef, config: &Config, theme_names: &[String]) -> usize {
    match row.kind.options {
        Some(Control::Toggle) => controls::toggle_width(),
        Some(Control::Pill(labels)) => controls::pill_width(labels),
        Some(Control::Button(label)) => controls::button_width(label),
        None => (row.kind.read)(config, theme_names).chars().count(),
    }
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
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
        state.open_draft_for_focused(config);
    }

    #[test]
    fn toggle_arrows_are_direction_bound() {
        // Left = off, Right = on (Enter still flips); a toggle used to flip on either arrow.
        let mut config = Config::default();
        config.editor.autosave_enabled = false;
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "Autosave");
        let resp = state.handle_key(&key(KeyCode::Left), &mut config);
        assert_eq!(resp, SettingsResponse::Continue);
        assert!(!config.editor.autosave_enabled, "Left means off");
        let resp = state.handle_key(&key(KeyCode::Right), &mut config);
        assert!(matches!(resp, SettingsResponse::FieldChanged(_)));
        assert!(config.editor.autosave_enabled, "Right means on");
        let resp = state.handle_key(&key(KeyCode::Right), &mut config);
        assert_eq!(resp, SettingsResponse::Continue);
        assert!(config.editor.autosave_enabled, "Right when on is a no-op");
        let resp = state.handle_key(&key(KeyCode::Left), &mut config);
        assert!(matches!(resp, SettingsResponse::FieldChanged(_)));
        assert!(!config.editor.autosave_enabled, "Left means off");
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
        focus_row(&mut state, &config, "  Char limit");
        assert_eq!(state.editing.as_deref(), Some("100"));
        for _ in 0..3 {
            state.handle_key(&key(KeyCode::Backspace), &mut config);
        }
        for c in "200".chars() {
            state.handle_key(&key(KeyCode::Char(c)), &mut config);
        }
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
        focus_row(&mut state, &config, "Open config.toml");
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(resp, SettingsResponse::OpenInExternalEditor);
    }

    #[test]
    fn default_focus_is_first_editable_row() {
        let state = SettingsState::new();
        assert_eq!(state.rows[state.focused].label, "Autosave");
    }

    #[test]
    fn arrow_navigation_skips_divider_row() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        state.handle_key(&key(KeyCode::Up), &mut config);
        assert_eq!(state.rows[state.focused].label, "Open config.toml");
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
        for _ in 0..3 {
            state.handle_key(&key(KeyCode::Backspace), &mut config);
        }
        for c in "abc".chars() {
            state.handle_key(&key(KeyCode::Char(c)), &mut config);
        }
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
        for c in "9".chars() {
            state.handle_key(&key(KeyCode::Char(c)), &mut config);
        }
        assert!(state.editing.is_some());
        let resp = state.handle_key(&key(KeyCode::Esc), &mut config);
        assert_eq!(resp, SettingsResponse::Cancelled);
        assert!(state.editing.is_none());
        assert_eq!(config.editor.max_width_cols, 100);
    }

    #[test]
    fn outline_toggle_changes_requested_preference_without_touching_focus() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "Show outline");
        assert!(!config.editor.show_outline);
        assert_eq!(
            state.handle_key(&key(KeyCode::Enter), &mut config),
            SettingsResponse::FieldChanged(rows::LABEL_OUTLINE)
        );
        assert!(config.editor.show_outline);
        assert_eq!(
            state.handle_key(&key(KeyCode::Enter), &mut config),
            SettingsResponse::FieldChanged(rows::LABEL_OUTLINE)
        );
        assert!(!config.editor.show_outline);
    }

    #[test]
    fn rows_match_curated_list() {
        // Pins the row set so a new row is an explicit, reviewable change.
        let labels: Vec<&str> = build_rows().iter().map(|r| r.label).collect();
        assert_eq!(
            labels,
            vec![
                rows::HEADER_NOTE,
                "",
                "Open config folder",
                "Open config.toml",
                "",
                "Autosave",
                "Big H1 headings",
                "Reflow paragraphs",
                "Blink cursor",
                "Check for updates",
                "Daily tips",
                "Limit editor width",
                "  Char limit",
                "Scroll speed",
                "Diff when file changes",
                "Show figures",
                "  Math edit preview",
                "Show images",
                "  Show remote images",
                "Show line numbers",
                "Show outline",
                "Show table buttons",
                "Syntax highlighting",
                "Use visual line navigation",
                "Vim mode",
            ]
        );
    }

    #[test]
    fn dropped_legacy_rows_are_absent() {
        // Rows deliberately removed from the overlay must not come back via a schema rebase.
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
        let contents = render(&mut state, &config, 100, 25);
        assert!(
            contents.contains("Automatically save"),
            "expected focused-row description in pinned footer, got: {contents}"
        );
    }

    #[test]
    fn blink_cursor_description_embeds_config_cadence() {
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
    fn check_for_updates_row_toggles_config_flag() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "Check for updates");
        assert!(config.editor.check_for_updates, "opt-out, so on by default");
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert!(matches!(resp, SettingsResponse::FieldChanged(_)));
        assert!(!config.editor.check_for_updates);
    }

    #[test]
    fn daily_tips_row_toggles_config_flag() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "Daily tips");
        assert!(config.editor.daily_tips, "opt-out, so on by default");
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert!(matches!(resp, SettingsResponse::FieldChanged(_)));
        assert!(!config.editor.daily_tips);
    }

    // ── Click dispatch ──────────────────────────────────────────────────

    /// Resolve the cached control rect for a labeled row after a render.
    fn rect_for(state: &SettingsState, label: &str) -> Rect {
        let idx = state
            .rows
            .iter()
            .position(|r| r.label == label)
            .unwrap_or_else(|| panic!("missing row {label}"));
        state
            .row_hit_rects
            .iter()
            .find(|(i, _)| *i == idx)
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("row {label} not in hit rects (off-screen?)"))
    }

    #[test]
    fn click_cycles_a_pill_row_and_focuses_it() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        render(&mut state, &config, 120, 40);
        assert_eq!(config.images.enabled, ImagesEnabled::Ask);
        let r = rect_for(&state, "Show images");
        let resp = state.handle_click(r.x, r.y, &mut config);
        assert!(matches!(resp, SettingsResponse::FieldChanged(_)));
        assert_eq!(config.images.enabled, ImagesEnabled::Always);
        assert_eq!(state.rows[state.focused].label, "Show images");
    }

    #[test]
    fn click_flips_a_toggle_row() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        render(&mut state, &config, 120, 40);
        let before = config.editor.autosave_enabled;
        let r = rect_for(&state, "Autosave");
        let resp = state.handle_click(r.x, r.y, &mut config);
        assert!(matches!(resp, SettingsResponse::FieldChanged(_)));
        assert_eq!(config.editor.autosave_enabled, !before);
    }

    #[test]
    fn click_on_a_long_label_row_operates_its_shifted_control() {
        // A label longer than `LABEL_PAD` shifts the control right; the hit-rect must cover it.
        const LONG: &str = "Autosave with an unusually long label";
        assert!(
            LONG.chars().count() > LABEL_PAD,
            "label must exceed the pad to exercise the shift"
        );
        let mut config = Config::default();
        let mut state = SettingsState::new();
        let idx = state
            .rows
            .iter()
            .position(|r| r.label == "Autosave")
            .expect("autosave row");
        state.rows[idx].label = LONG;
        render(&mut state, &config, 120, 40);

        let r = rect_for(&state, LONG);
        let control_col = r.x + (FOCUS_MARKER_WIDTH + LONG.chars().count()) as u16;
        assert!(
            control_col < r.x + r.width,
            "hit-rect must reach the shifted control"
        );
        let before = config.editor.autosave_enabled;
        let resp = state.handle_click(control_col, r.y, &mut config);
        assert!(matches!(resp, SettingsResponse::FieldChanged(_)));
        assert_eq!(config.editor.autosave_enabled, !before);

        state.handle_click(r.x, r.y, &mut config);
        assert_eq!(config.editor.autosave_enabled, before);
    }

    #[test]
    fn click_external_action_row_emits_its_response() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        render(&mut state, &config, 120, 40);
        let r = rect_for(&state, "Open config folder");
        assert_eq!(
            state.handle_click(r.x, r.y, &mut config),
            SettingsResponse::OpenConfigFolder
        );
    }

    #[test]
    fn click_on_a_locked_row_is_a_noop() {
        let mut config = Config::default();
        config.images.enabled = ImagesEnabled::Never;
        config.images.remote_policy = RemoteImagePolicy::Never;
        let mut state = SettingsState::new();
        render(&mut state, &config, 120, 40);
        let focused_before = state.focused;
        let r = rect_for(&state, "  Show remote images");
        let resp = state.handle_click(r.x, r.y, &mut config);
        assert_eq!(resp, SettingsResponse::Continue);
        assert_eq!(
            state.focused, focused_before,
            "locked row absorbs the click"
        );
        assert_eq!(config.images.remote_policy, RemoteImagePolicy::Never);
    }

    #[test]
    fn click_that_misses_every_row_is_a_noop() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        render(&mut state, &config, 120, 40);
        let focused_before = state.focused;
        let resp = state.handle_click(0, 0, &mut config);
        assert_eq!(resp, SettingsResponse::Continue);
        assert_eq!(state.focused, focused_before);
    }

    #[test]
    fn show_images_never_cascades_remote_to_never_and_locks_row() {
        let mut config = Config::default();
        config.images.remote_policy = RemoteImagePolicy::Always;
        let mut state = SettingsState::new();
        focus_row(&mut state, &config, "Show images");
        state.handle_key(&key(KeyCode::Enter), &mut config);
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(config.images.enabled, ImagesEnabled::Never);
        assert_eq!(config.images.remote_policy, RemoteImagePolicy::Never);

        let remote_idx = state
            .rows
            .iter()
            .position(|r| r.label == "  Show remote images")
            .unwrap();
        assert!(state.rows[remote_idx].is_disabled(&config));
        state.focused = remote_idx - 1; // row just above the locked one
        state.handle_key(&key(KeyCode::Down), &mut config);
        assert_ne!(state.focused, remote_idx, "Down must skip the locked row");

        focus_row(&mut state, &config, "Show images");
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(config.images.enabled, ImagesEnabled::Ask);
        assert_eq!(config.images.remote_policy, RemoteImagePolicy::Always);
        assert!(!state.rows[remote_idx].is_disabled(&config));
    }
}
