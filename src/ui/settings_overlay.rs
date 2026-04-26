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

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, StatefulWidget, Widget},
};

use crate::config::{Config, ImagesEnabled, RemoteImagePolicy, StatusBarLayout, Theme};

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
            rows: build_rows(),
        };
        state.focused = state.first_focusable_index().unwrap_or(0);
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
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as i32;
        let mut idx = self.focused as i32 + delta;
        while (0..len).contains(&idx) {
            if self.rows[idx as usize].kind.focusable {
                self.focused = idx as usize;
                return;
            }
            idx += delta;
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
}

impl<'a> StatefulWidget for SettingsView<'a> {
    type State = SettingsState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let rect = overlay_rect(area, state.rows.len() as u16);
        Clear.render(rect, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Settings ", self.theme.modal_title))
            .style(self.theme.status_bar);
        let inner = block.inner(rect);
        block.render(rect, buf);
        if inner.height < 2 || inner.width == 0 {
            return;
        }

        let mut lines: Vec<Line<'_>> = Vec::new();

        for (idx, row) in state.rows.iter().enumerate() {
            // A non-focusable empty row is a blank-line divider used
            // to set the "open externally" pair apart from the
            // editable settings beneath it.
            if !row.kind.focusable && row.label.is_empty() {
                lines.push(Line::from(""));
                continue;
            }
            let focused = idx == state.focused;
            let marker = if focused { "› " } else { "  " };
            let label_style = if focused {
                self.theme.modal_button_focused
            } else {
                self.theme.status_bar
            };
            let value = if focused && state.editing.is_some() {
                format!("{}▏", state.editing.as_deref().unwrap_or(""))
            } else {
                (row.kind.read)(self.config, &state.theme_names)
            };
            let label_padded = format!("{marker}{:<28}", row.label);
            lines.push(Line::from(vec![
                Span::styled(label_padded, label_style),
                Span::styled(value, self.theme.status_filename),
            ]));
            if focused {
                if let Some(desc) = row.description {
                    lines.push(Line::from(Span::styled(
                        format!("    {}", desc),
                        self.theme.status_info.add_modifier(Modifier::DIM),
                    )));
                }
            }
        }

        if let Some(err) = state.last_error.as_ref() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("✗ {err}"),
                self.theme.transient_error,
            )));
        }

        Paragraph::new(lines)
            .style(self.theme.status_bar)
            .render(inner, buf);
    }
}

fn overlay_rect(area: Rect, rows: u16) -> Rect {
    let target_width = (area.width as usize * 8 / 10).max(60);
    let width = target_width.min(area.width as usize) as u16;
    // +1 for the focused row's description, +4 for top/bottom
    // borders and an error-line spacer.  The "open externally" pair
    // and divider are normal rows, counted in `rows`.
    let height = (rows + 5).min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

// ─── Row catalogue ─────────────────────────────────────────────────────────

/// Static table of rows.  `read` formats the field's current value
/// for display; `cycle` is `Some` for fields whose value cycles on
/// Left/Right or Enter (booleans, enum-valued fields, theme name);
/// `write_string` handles the inline-editor confirm path.
struct RowDef {
    label: &'static str,
    description: Option<&'static str>,
    kind: RowKind,
}

/// `(config, delta, theme_names) -> changed?`.  Aliased so the
/// `Option<…>` field below stays under clippy's complexity threshold.
type CycleFn = fn(&mut Config, i32, &[String]) -> bool;

struct RowKind {
    focusable: bool,
    action: RowAction,
    read: fn(&Config, &[String]) -> String,
    write_string: fn(&mut Config, &str) -> Result<(), String>,
    cycle: Option<CycleFn>,
}

#[derive(Debug)]
enum RowAction {
    /// "Open config.toml in default editor" sentinel.
    OpenExternalEditor,
    /// "Open Config folder" sentinel — fires the `OpenConfigFolder`
    /// action via the OS file manager (`xdg-open` on Linux,
    /// `open` on macOS, `explorer` on Windows).
    OpenConfigFolder,
    /// Enter cycles the value (boolean toggle / enum advance).
    Cycle,
    /// Enter opens an inline text editor (numeric field).
    Edit,
}

fn no_write(_: &mut Config, _: &str) -> Result<(), String> {
    Err("row is not editable in place".to_owned())
}

fn parse_u64(s: &str) -> Result<u64, String> {
    s.trim()
        .parse::<u64>()
        .map_err(|e| format!("invalid number: {e}"))
}

fn parse_usize(s: &str) -> Result<usize, String> {
    s.trim()
        .parse::<usize>()
        .map_err(|e| format!("invalid number: {e}"))
}

/// Cycle helper for `Ask` → `Always` → `Never` → `Ask`.  `delta`
/// chooses direction.
fn cycle_images_enabled(value: ImagesEnabled, delta: i32) -> ImagesEnabled {
    let order = [
        ImagesEnabled::Ask,
        ImagesEnabled::Always,
        ImagesEnabled::Never,
    ];
    let i = order.iter().position(|v| *v == value).unwrap_or(0) as i32;
    let n = order.len() as i32;
    order[((i + delta).rem_euclid(n)) as usize]
}

fn cycle_remote_policy(value: RemoteImagePolicy, delta: i32) -> RemoteImagePolicy {
    let order = [
        RemoteImagePolicy::Ask,
        RemoteImagePolicy::Always,
        RemoteImagePolicy::Never,
    ];
    let i = order.iter().position(|v| *v == value).unwrap_or(0) as i32;
    let n = order.len() as i32;
    order[((i + delta).rem_euclid(n)) as usize]
}

fn images_enabled_label(v: ImagesEnabled) -> &'static str {
    match v {
        ImagesEnabled::Ask => "Ask",
        ImagesEnabled::Always => "Always",
        ImagesEnabled::Never => "Never",
    }
}

fn remote_policy_label(v: RemoteImagePolicy) -> &'static str {
    match v {
        RemoteImagePolicy::Ask => "Ask",
        RemoteImagePolicy::Always => "Always",
        RemoteImagePolicy::Never => "Never",
    }
}

fn parse_images_enabled(s: &str) -> Result<ImagesEnabled, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "ask" => Ok(ImagesEnabled::Ask),
        "always" => Ok(ImagesEnabled::Always),
        "never" => Ok(ImagesEnabled::Never),
        other => Err(format!("expected Ask/Always/Never, got {other:?}")),
    }
}

fn parse_remote_policy(s: &str) -> Result<RemoteImagePolicy, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "ask" => Ok(RemoteImagePolicy::Ask),
        "always" => Ok(RemoteImagePolicy::Always),
        "never" => Ok(RemoteImagePolicy::Never),
        other => Err(format!("expected Ask/Always/Never, got {other:?}")),
    }
}

/// Read all `.toml` file stems from `<config_dir>/themes/`.  Sorted
/// for stable cycle order.  Falls back to `["default"]` so the cycle
/// never hangs on an unreadable directory.
fn list_theme_names() -> Vec<String> {
    let mut out = Vec::new();
    if let Some(dir) = Config::config_dir() {
        let themes = dir.join("themes");
        if let Ok(read) = std::fs::read_dir(&themes) {
            for entry in read.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        out.push(stem.to_owned());
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    if out.is_empty() {
        out.push("default".to_owned());
    }
    out
}

fn cycle_theme(config: &mut Config, delta: i32, themes: &[String]) -> bool {
    if themes.is_empty() {
        return false;
    }
    let n = themes.len() as i32;
    let cur = themes
        .iter()
        .position(|t| *t == config.theme)
        .map(|i| i as i32)
        .unwrap_or(0);
    let next = (cur + delta).rem_euclid(n);
    let new_name = themes[next as usize].clone();
    if new_name == config.theme {
        return false;
    }
    config.theme = new_name;
    true
}

/// Build the static row table.  Order is the user-facing display
/// order; nothing else depends on it.  See [`Config::config`] for
/// each field's persistence semantics.
fn build_rows() -> Vec<RowDef> {
    vec![
        RowDef {
            label: "Open config folder",
            description: Some("Press Enter to open externally"),
            kind: RowKind {
                focusable: true,
                action: RowAction::OpenConfigFolder,
                read: |_, _| {
                    Config::config_dir()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                },
                write_string: no_write,
                cycle: None,
            },
        },
        RowDef {
            label: "Open config.toml in default editor",
            description: Some("Press Enter to open externally"),
            kind: RowKind {
                focusable: true,
                action: RowAction::OpenExternalEditor,
                read: |_, _| String::new(),
                write_string: no_write,
                cycle: None,
            },
        },
        // Blank divider — sets the "open externally" pair apart
        // from the editable settings beneath.  Non-focusable so
        // arrow-key navigation skips it; the View renders an empty
        // line for any non-focusable row with an empty label.
        RowDef {
            label: "",
            description: None,
            kind: RowKind {
                focusable: false,
                action: RowAction::Cycle,
                read: |_, _| String::new(),
                write_string: no_write,
                cycle: None,
            },
        },
        RowDef {
            label: "Theme",
            description: Some("Active theme (resolves to themes/<name>.toml)"),
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| c.theme.clone(),
                write_string: |c, v| {
                    let v = v.trim();
                    if v.is_empty() {
                        return Err("theme name cannot be empty".into());
                    }
                    c.theme = v.to_owned();
                    Ok(())
                },
                cycle: Some(cycle_theme),
            },
        },
        RowDef {
            label: "Use hint line",
            description: Some("Show or hide the hint line (status bar remains)"),
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| matches!(c.editor.status_bar, StatusBarLayout::TwoLine).to_string(),
                write_string: no_write,
                cycle: Some(|c, _, _| {
                    c.editor.status_bar = match c.editor.status_bar {
                        StatusBarLayout::TwoLine => StatusBarLayout::Compact,
                        StatusBarLayout::Compact => StatusBarLayout::TwoLine,
                    };
                    true
                }),
            },
        },
        RowDef {
            label: "Hint duration",
            description: Some("Hint line message duration in ms"),
            kind: RowKind {
                focusable: true,
                action: RowAction::Edit,
                read: |c, _| c.editor.transient_ms.to_string(),
                write_string: |c, v| {
                    c.editor.transient_ms = parse_u64(v)?;
                    Ok(())
                },
                cycle: None,
            },
        },
        RowDef {
            label: "Use visual line navigation",
            description: Some("Up/Down move by visual lines (vs. logical)"),
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| c.editor.visual_line_nav.to_string(),
                write_string: no_write,
                cycle: Some(|c, _, _| {
                    c.editor.visual_line_nav = !c.editor.visual_line_nav;
                    true
                }),
            },
        },
        RowDef {
            label: "Scroll speed",
            description: Some("Lines per mouse-wheel tick (also applies to touchpads)"),
            kind: RowKind {
                focusable: true,
                action: RowAction::Edit,
                read: |c, _| c.editor.mouse_scroll_lines.to_string(),
                write_string: |c, v| {
                    c.editor.mouse_scroll_lines = parse_usize(v)?;
                    Ok(())
                },
                cycle: None,
            },
        },
        RowDef {
            label: "Show images",
            description: Some("Show images in preview and render mode"),
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| images_enabled_label(c.images.enabled).to_owned(),
                write_string: |c, v| {
                    c.images.enabled = parse_images_enabled(v)?;
                    Ok(())
                },
                cycle: Some(|c, delta, _| {
                    c.images.enabled = cycle_images_enabled(c.images.enabled, delta);
                    true
                }),
            },
        },
        RowDef {
            label: "Show remote images",
            description: Some("Fetch and display remote images"),
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| remote_policy_label(c.images.remote_policy).to_owned(),
                write_string: |c, v| {
                    c.images.remote_policy = parse_remote_policy(v)?;
                    Ok(())
                },
                cycle: Some(|c, delta, _| {
                    c.images.remote_policy = cycle_remote_policy(c.images.remote_policy, delta);
                    true
                }),
            },
        },
        RowDef {
            label: "Show table drag handles",
            description: Some("Show table row/column move/resize glyphs"),
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| c.table.show_drag_handles.to_string(),
                write_string: no_write,
                cycle: Some(|c, _, _| {
                    c.table.show_drag_handles = !c.table.show_drag_handles;
                    true
                }),
            },
        },
        RowDef {
            label: "Export inlined images",
            description: Some("Embed local images as data: URIs in HTML export"),
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| c.export.html.inline_images.to_string(),
                write_string: no_write,
                cycle: Some(|c, _, _| {
                    c.export.html.inline_images = !c.export.html.inline_images;
                    true
                }),
            },
        },
        RowDef {
            label: "Export diagrams as SVG",
            description: Some("Render Mermaid diagrams as SVG and inline in HTML export"),
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| c.export.html.diagrams.to_string(),
                write_string: no_write,
                cycle: Some(|c, _, _| {
                    c.export.html.diagrams = !c.export.html.diagrams;
                    true
                }),
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn enter_on_first_row_emits_open_config_folder() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        // Focus defaults to row 0, which is now "Open Config folder".
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(resp, SettingsResponse::OpenConfigFolder);
    }

    #[test]
    fn enter_on_second_row_emits_open_external_editor() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        focus_row(&mut state, "Open config.toml in default editor");
        let resp = state.handle_key(&key(KeyCode::Enter), &mut config);
        assert_eq!(resp, SettingsResponse::OpenInExternalEditor);
    }

    #[test]
    fn arrow_navigation_skips_divider_row() {
        let mut config = Config::default();
        let mut state = SettingsState::new();
        // Focus starts on "Open Config folder" (row 0).
        state.handle_key(&key(KeyCode::Down), &mut config);
        assert_eq!(
            state.rows[state.focused].label,
            "Open config.toml in default editor"
        );
        // The next Down must skip the blank divider and land on
        // "Theme", not on the divider itself.
        state.handle_key(&key(KeyCode::Down), &mut config);
        assert_eq!(state.rows[state.focused].label, "Theme");
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
                "Open Config folder",
                "Open config.toml in default editor",
                "",
                "Theme",
                "Use hint line",
                "Hint duration",
                "Use visual line navigation",
                "Scroll speed",
                "Show images",
                "Show remote images",
                "Show table drag handles",
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
                !labels.iter().any(|l| *l == stale),
                "stale row '{stale}' is still in the schema"
            );
        }
    }
}
