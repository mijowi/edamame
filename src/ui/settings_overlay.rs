//! Phase 10 — settings overlay.
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
//! Theme                           catppuccin
//!   Active theme (resolves to themes/<name>.toml)
//!
//! Use hint line                   true
//! Hint duration                   1500
//!   Hint line message duration in ms
//! ...
//! ```
//!
//! The first two rows are "open externally" actions — the folder row
//! shells `xdg-open` (or the OS equivalent) on the config directory;
//! the file row suspends the TUI and runs `$VISUAL`/`$EDITOR` on
//! `config.toml`.  A blank divider separates them from the editable
//! settings beneath.
//!
//! Each row's description appears beneath it only when the row is
//! focused, conserving vertical space.  Booleans and enum-valued
//! fields cycle on Enter; numeric and theme-name fields open an
//! inline editor (Theme also cycles via Left/Right when not editing
//! through the available `themes/*.toml` files).

mod rows;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::config::{Config, Theme};
use crate::ui::content_width::{max_row_width, optional_text_width};
use crate::ui::modal_row::{format_modal_row, RowLayout};
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

use self::rows::{build_rows, list_theme_names, RowAction, RowDef};

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
    /// `Some(buffer)` while an inline editor is open.  Only used by
    /// numeric and theme-name fields.
    pub editing: Option<String>,
    /// Last error from a rejected edit.  Cleared on the next
    /// successful edit / cancel.
    pub last_error: Option<String>,
    /// Snapshot of available theme names (file stems of
    /// `<config_dir>/themes/*.toml`).  Built once when the overlay
    /// opens; used by `cycle_theme` to advance the Theme field.
    /// Falls back to `["default"]` when the config dir is not
    /// readable so cycling is never a no-op.
    pub theme_names: Vec<String>,
    /// Vertical scroll bookkeeping for the row table.  Up/Down move
    /// `focused` and pull the viewport via `ensure_visible`; PgUp/PgDn
    /// and the mouse wheel drive `scroll_state.scroll` directly without
    /// touching focus.
    pub scroll_state: ScrollContainerState,
    /// Absolute terminal rect of the rendered `esc` close hint.
    pub esc_button_rect: Option<Rect>,
    rows: Vec<RowDef>,
}

impl SettingsState {
    pub fn new() -> Self {
        let theme_names = list_theme_names();
        let mut state = Self {
            focused: 0,
            editing: None,
            last_error: None,
            theme_names,
            scroll_state: ScrollContainerState::default(),
            esc_button_rect: None,
            rows: build_rows(),
        };
        // Default focus to the first editable setting ("Theme") rather
        // than the "open externally" pair at the top.  Most users open
        // the overlay to tweak a setting; the externals are still one
        // Up arrow away.
        state.focused = state
            .rows
            .iter()
            .position(|r| r.label == "Theme")
            .or_else(|| state.first_focusable_index())
            .unwrap_or(0);
        state
    }

    /// Apply a key event, possibly mutating `config`.  Bool / enum /
    /// theme fields cycle on Enter; numeric fields open an inline
    /// editor.
    pub fn handle_key(&mut self, key: &KeyEvent, config: &mut Config) -> SettingsResponse {
        if let Some(buf) = self.editing.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    self.editing = None;
                    self.last_error = None;
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Enter => {
                    let value = buf.clone();
                    let row = match self.rows.get(self.focused) {
                        Some(r) => r,
                        None => return SettingsResponse::Continue,
                    };
                    return match (row.kind.write_string)(config, &value) {
                        Ok(()) => {
                            self.editing = None;
                            self.last_error = None;
                            SettingsResponse::FieldChanged(row.label)
                        }
                        Err(e) => {
                            self.last_error = Some(e);
                            SettingsResponse::Continue
                        }
                    };
                }
                KeyCode::Char(c) => {
                    use crossterm::event::KeyModifiers;
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    {
                        buf.push(c);
                    }
                }
                _ => {}
            }
            return SettingsResponse::Continue;
        }

        // PgUp/PgDn/Home/End move the viewport without touching focus.
        if self.scroll_state.handle_paging_key(key) {
            return SettingsResponse::Continue;
        }

        match key.code {
            KeyCode::Esc => SettingsResponse::Cancelled,
            KeyCode::Up => {
                self.move_focus(-1);
                SettingsResponse::Continue
            }
            KeyCode::Down => {
                self.move_focus(1);
                SettingsResponse::Continue
            }
            KeyCode::Left => self.cycle_focused(config, -1),
            KeyCode::Right => self.cycle_focused(config, 1),
            KeyCode::Enter => {
                let row = match self.rows.get(self.focused) {
                    Some(r) => r,
                    None => return SettingsResponse::Continue,
                };
                match &row.kind.action {
                    RowAction::OpenExternalEditor => SettingsResponse::OpenInExternalEditor,
                    RowAction::OpenConfigFolder => SettingsResponse::OpenConfigFolder,
                    RowAction::Cycle => self.cycle_focused(config, 1),
                    RowAction::Edit => {
                        let current = (row.kind.read)(config, &self.theme_names);
                        self.editing = Some(current);
                        self.last_error = None;
                        SettingsResponse::Continue
                    }
                }
            }
            _ => SettingsResponse::Continue,
        }
    }

    /// Cycle the focused row's value by `delta` (-1 / +1).  Only
    /// applies to bool/enum/theme rows; no-op on numeric/edit-only
    /// rows so an accidental Left arrow doesn't surprise the user.
    fn cycle_focused(&mut self, config: &mut Config, delta: i32) -> SettingsResponse {
        let row = match self.rows.get(self.focused) {
            Some(r) => r,
            None => return SettingsResponse::Continue,
        };
        let cycled = match row.kind.cycle {
            Some(f) => f(config, delta, &self.theme_names),
            None => false,
        };
        if cycled {
            SettingsResponse::FieldChanged(row.label)
        } else {
            SettingsResponse::Continue
        }
    }

    fn move_focus(&mut self, delta: i32) {
        if let Some(idx) = next_focusable(&self.rows, self.focused, delta, |r| r.kind.focusable) {
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
        let has_description = focused_row.and_then(|r| r.description).is_some();
        let pinned_bottom: u16 = (if has_description { 1 } else { 0 })
            + (if state.last_error.is_some() { 2 } else { 0 });

        let content = ContentSize {
            width: content_width,
            height: row_lines.len() as u16,
            pinned_top: 0,
            pinned_bottom,
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
                content_width,
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
            if let Some(desc) = focused_row.and_then(|r| r.description) {
                let desc_area = Rect {
                    x: inner.x,
                    y: footer_y,
                    width: inner.width,
                    height: 1,
                };
                Paragraph::new(Line::from(Span::styled(
                    format!("    {}", desc),
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
        let focused = idx == state.focused;
        let editing = focused && state.editing.is_some();
        let value = if editing && cursor_visible {
            format!("{}▏", state.editing.as_deref().unwrap_or(""))
        } else if editing {
            state.editing.as_deref().unwrap_or("").to_owned()
        } else {
            (row.kind.read)(config, &state.theme_names)
        };
        lines.push(format_modal_row(
            row.label,
            &value,
            focused,
            editing,
            theme,
            RowLayout::FixedPad(LABEL_PAD),
        ));
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
        let value_w = (r.kind.read)(config, &state.theme_names).chars().count();
        FOCUS_MARKER_WIDTH + LABEL_PAD + value_w
    });
    // 4 = "    " description indent
    let desc_max = max_row_width(&state.rows, |r| {
        r.description.map(|d| 4 + d.chars().count()).unwrap_or(0)
    });
    let err_max = optional_text_width(state.last_error.as_deref(), 2);
    row_max.max(desc_max).max(err_max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ImagesEnabled, RemoteImagePolicy, StatusBarLayout};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn focus_row(state: &mut SettingsState, label: &str) {
        let idx = state
            .rows
            .iter()
            .position(|r| r.label == label)
            .unwrap_or_else(|| panic!("missing row {label}"));
        state.focused = idx;
    }

    #[test]
    fn cycle_toggles_use_visual_line_navigation() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, "Use visual line navigation");
        assert!(config.editor.visual_line_nav); // default true
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert!(matches!(resp, SettingsResponse::FieldChanged(_)));
        assert!(!config.editor.visual_line_nav);
    }

    #[test]
    fn cycle_advances_show_images_through_ask_always_never() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, "Show images");
        assert_eq!(config.images.enabled, ImagesEnabled::Ask);
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(config.images.enabled, ImagesEnabled::Always);
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(config.images.enabled, ImagesEnabled::Never);
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(config.images.enabled, ImagesEnabled::Ask);
    }

    #[test]
    fn use_hint_line_toggles_status_bar_layout() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, "Use hint line");
        assert_eq!(config.editor.status_bar, StatusBarLayout::TwoLine);
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(config.editor.status_bar, StatusBarLayout::Compact);
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(config.editor.status_bar, StatusBarLayout::TwoLine);
    }

    #[test]
    fn hint_duration_opens_inline_editor_and_round_trips() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, "Hint duration");
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(state.editing.as_deref(), Some("1500"));
        for _ in 0..4 {
            state.handle_key(&key(KeyCode::Backspace), &mut config);
        }
        for c in "2500".chars() {
            state.handle_key(&key(KeyCode::Char(c)), &mut config);
        }
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert!(matches!(resp, SettingsResponse::FieldChanged(_)));
        assert_eq!(config.editor.transient_ms, 2500);
    }

    #[test]
    fn enter_on_open_config_folder_row_emits_open_config_folder() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, "Open config folder");
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(resp, SettingsResponse::OpenConfigFolder);
    }

    #[test]
    fn enter_on_config_toml_row_emits_open_external_editor() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, "Open config.toml in default editor");
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(resp, SettingsResponse::OpenInExternalEditor);
    }

    #[test]
    fn default_focus_is_theme_row() {
        // Most users open Settings to adjust a setting, not the
        // externals.  Default focus skips past the open-externally
        // pair and lands on Theme.
        let state = SettingsState::new();
        assert_eq!(state.rows[state.focused].label, "Theme");
    }

    #[test]
    fn arrow_navigation_skips_divider_row() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        // Default focus is "Theme" — Up must skip the blank divider
        // and land on "Open config.toml in default editor".
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
        focus_row(&mut state, "Show remote images");
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
        focus_row(&mut state, "Hint duration");
        state.handle_key(&key(KeyCode::Enter), &mut config);
        // Replace `1500` with garbage.
        for _ in 0..4 {
            state.handle_key(&key(KeyCode::Backspace), &mut config);
        }
        for c in "abc".chars() {
            state.handle_key(&key(KeyCode::Char(c)), &mut config);
        }
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert!(matches!(resp, SettingsResponse::Continue));
        assert!(state.last_error.is_some());
        assert_eq!(config.editor.transient_ms, 1500); // unchanged
    }

    #[test]
    fn escape_cancels_overlay_when_not_editing() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        let resp = state.handle_key(&key(KeyCode::Esc), &mut config);
        assert_eq!(resp, SettingsResponse::Cancelled);
    }

    #[test]
    fn escape_inside_inline_editor_only_cancels_the_edit() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, "Hint duration");
        state.handle_key(&key(KeyCode::Enter), &mut config);
        assert!(state.editing.is_some());
        let resp = state.handle_key(&key(KeyCode::Esc), &mut config);
        assert_eq!(resp, SettingsResponse::Continue);
        assert!(state.editing.is_none());
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
                "Open config folder",
                "Open config.toml in default editor",
                "",
                "Theme",
                "Use hint line",
                "Hint duration",
                "Limit editor width",
                "Editor max width",
                "Big H1 headings",
                "Use visual line navigation",
                "Scroll speed",
                "Show images",
                "Show remote images",
                "Show table buttons",
                "Export inlined images",
                "Export diagrams as SVG",
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
    fn settings_description_appears_in_pinned_footer() {
        let config = Config::default();
        let mut state = SettingsState::new();
        // Default focus is on the Theme row, which has a description.
        let contents = render(&mut state, &config, 100, 25);
        // Description text should be present somewhere in the rendered
        // buffer — the pinned-footer slot is at the bottom of the modal.
        assert!(
            contents.contains("Active theme"),
            "expected Theme description in pinned footer, got: {contents}"
        );
    }
}
