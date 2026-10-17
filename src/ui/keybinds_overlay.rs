//! Keybindings overlay: a categorized view + editor.  Enter on a row arms one-press chord capture.
//!
//! Edits are buffered in a draft `KeyMap` / `KeyBindingOverrides` owned by the overlay; nothing
//! reaches the live keymap or `keybindings.toml` until `[ Save ]`.  Conflicts are checked by
//! [`KeyMap::rebind`] against the draft, so chained edits (e.g. swapping two bindings) are checked
//! against each other, and surface via [`KeybindsState::last_error`].

mod categories;

use self::categories::CATEGORIES;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::config::keymap::{format_key, format_key_parseable};
use crate::config::{Action, KeyBindingOverrides, KeyMap, KeyMapError, Theme};
use crate::input::diff_hint;
use crate::ui::button_row::{button_row_width, footer_row_count, render_button_row};
use crate::ui::content_width::{max_row_width, optional_text_width};
use crate::ui::modal_row::{format_modal_row, RowLayout};
use crate::ui::overlay_nav::next_focusable;
use crate::ui::scroll_container::{
    centered_rect_for_content, draw_frame, ContentSize, FrameOpts, ModalKind, ScrollContainerState,
    VERTICAL_CHROME_ROWS,
};

/// Width of the action-label column; sized to fit the longest action name.
const LABEL_PAD: usize = 22;

/// Twice the default [`MAX_PAD_H`](crate::ui::scroll_container::MAX_PAD_H): the slack absorbs
/// most "already bound to …" errors without re-flowing the modal wider mid-capture.
const KEYBINDS_MAX_PAD_H: u16 = 8;

const CAPTURE_HINT: &str = "Press a key… (Esc to cancel)";
/// Footer buttons, left-to-right; `Down` from the list lands on Cancel first.
const BUTTON_LABELS: &[&str] = &["Cancel", "Save"];

/// Outcome of dispatching a key event to the keybinds overlay.
#[derive(Debug, Clone)]
pub enum KeybindsResponse {
    Continue,
    /// Draft discarded; the live keymap is untouched.
    Cancelled,
    /// Carries the draft, ready to install and persist to `keybindings.toml`.
    Save {
        keymap: KeyMap,
        overrides: KeyBindingOverrides,
    },
}

/// `Header` rows are display-only and skipped by focus; `Binding` rows are editable.
#[derive(Debug, Clone)]
enum Row {
    Header(&'static str),
    Binding { action: Action, label: &'static str },
}

/// Which focus group receives keystrokes; the buttons sit outside the scrollable list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusArea {
    List,
    Save,
    Cancel,
}

/// Mutable state for an open keybinds overlay.
pub struct KeybindsState {
    /// Modal handler active when this overlay was opened.
    pub handler_label: &'static str,
    /// Index into [`Self::rows`].  Invariant: never rests on a `Header` — every assignment site
    /// guards on the row being a `Binding` (a new site owes the same guard).  Kept across
    /// focus-area transitions so Tab-back lands on the user's last row.
    pub focused: usize,
    pub focus_area: FocusArea,
    /// Chord-capture mode: the next non-modifier key press becomes the draft binding.
    pub capturing: bool,
    /// Cleared on the next successful edit, cancel, or focus move.
    pub last_error: Option<String>,
    /// Up/Down move `focused` and pull the viewport via `ensure_visible`; PgUp/PgDn and the
    /// wheel drive `scroll` directly without touching focus.
    pub scroll_state: ScrollContainerState,
    pub esc_button_rect: Option<Rect>,
    pub cancel_button_rect: Option<Rect>,
    pub save_button_rect: Option<Rect>,
    /// `(index into rows, rect)` for the visible `Binding` rows; rebuilt every render.
    pub row_hit_rects: Vec<(usize, Rect)>,
    /// Clone of the live keymap, mutated by every rebind.
    pub draft_keymap: KeyMap,
    pub draft_overrides: KeyBindingOverrides,
    /// Built once from `CATEGORIES` and never rebuilt.
    rows: Vec<Row>,
    /// Body-line index where `rows[i]` renders (headers add a blank separator).
    focus_offsets: Vec<usize>,
}

impl KeybindsState {
    /// Open with a draft cloned from the live keymap and overrides.
    pub fn open(keymap: &KeyMap, overrides: &KeyBindingOverrides, modal_active: bool) -> Self {
        let rows = build_rows(modal_active);
        let focus_offsets = compute_focus_offsets(&rows);
        let mut state = Self {
            handler_label: "Default",
            focused: 0,
            focus_area: FocusArea::List,
            capturing: false,
            last_error: None,
            scroll_state: ScrollContainerState::default(),
            esc_button_rect: None,
            cancel_button_rect: None,
            save_button_rect: None,
            row_hit_rects: Vec::new(),
            draft_keymap: keymap.clone(),
            draft_overrides: overrides.clone(),
            rows,
            focus_offsets,
        };
        state.focused = state.first_binding_index().unwrap_or(0);
        state
    }

    /// Test-only accessor; `dead_code` allowed because the callers are `#[cfg(test)]`.
    #[allow(dead_code)]
    pub fn focused_action(&self) -> Option<Action> {
        match self.rows.get(self.focused) {
            Some(Row::Binding { action, .. }) => Some(action.clone()),
            _ => None,
        }
    }

    /// Focus the row bound to `target`; returns `false` if absent.  Used only by tests (incl.
    /// `tests/palette.rs`), hence `dead_code` allowed.
    #[allow(dead_code)]
    pub fn focus_action(&mut self, target: &Action) -> bool {
        for (idx, row) in self.rows.iter().enumerate() {
            if let Row::Binding { action, .. } = row {
                if action == target {
                    self.focused = idx;
                    self.focus_area = FocusArea::List;
                    return true;
                }
            }
        }
        false
    }

    /// Apply a key event.
    pub fn handle_key(&mut self, key: &KeyEvent) -> KeybindsResponse {
        if self.capturing {
            return self.handle_capture_key(key);
        }

        if self.scroll_state.handle_paging_key(key) {
            return KeybindsResponse::Continue;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Tab, KeyModifiers::NONE) => {
                self.cycle_focus(1);
                return KeybindsResponse::Continue;
            }
            (KeyCode::BackTab, _) => {
                self.cycle_focus(-1);
                return KeybindsResponse::Continue;
            }
            _ => {}
        }

        match self.focus_area {
            FocusArea::List => self.handle_list_key(key),
            FocusArea::Save | FocusArea::Cancel => self.handle_button_key(key),
        }
    }

    fn handle_capture_key(&mut self, key: &KeyEvent) -> KeybindsResponse {
        // Bare Esc cancels capture (binding Esc itself needs hand-editing `keybindings.toml`);
        // Esc-with-modifiers is a valid chord and falls through to the rebind path.
        if key.code == KeyCode::Esc && key.modifiers == KeyModifiers::NONE {
            self.capturing = false;
            self.last_error = None;
            return KeybindsResponse::Continue;
        }
        if is_bare_modifier(key) {
            return KeybindsResponse::Continue;
        }
        let action = match self.rows.get(self.focused) {
            Some(Row::Binding { action, .. }) => action.clone(),
            _ => return KeybindsResponse::Continue,
        };
        // Build the `parse_key` form directly: going via `format_key` + `replace('-', '+')`
        // mangles the `-` and `+` keys.  `None` (media/lock keys) has no parseable spelling.
        let new_key = match format_key_parseable(key) {
            Some(s) => s,
            None => {
                self.last_error = Some("Unsupported key — try a different chord".into());
                return KeybindsResponse::Continue;
            }
        };
        match self
            .draft_keymap
            .rebind(&action, &new_key, &mut self.draft_overrides)
        {
            Ok(()) => {
                self.capturing = false;
                self.last_error = None;
            }
            Err(KeyMapError::ConflictingBinding {
                action: existing_action,
                ..
            }) => {
                // Human-readable `Ctrl-Q`, not the normalized `ctrl+q` carried by the error.
                let display_key = format_key(key);
                self.last_error = Some(format!(
                    "'{display_key}' is already bound to {existing_action}"
                ));
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
            }
        }
        KeybindsResponse::Continue
    }

    fn handle_list_key(&mut self, key: &KeyEvent) -> KeybindsResponse {
        match key.code {
            KeyCode::Esc => KeybindsResponse::Cancelled,
            KeyCode::Up => {
                self.move_focus(-1);
                KeybindsResponse::Continue
            }
            KeyCode::Down => {
                if !self.move_focus(1) {
                    self.focus_area = FocusArea::Cancel;
                    self.last_error = None;
                }
                KeybindsResponse::Continue
            }
            KeyCode::Enter => {
                if matches!(self.rows.get(self.focused), Some(Row::Binding { .. })) {
                    self.capturing = true;
                    self.last_error = None;
                }
                KeybindsResponse::Continue
            }
            _ => KeybindsResponse::Continue,
        }
    }

    fn handle_button_key(&mut self, key: &KeyEvent) -> KeybindsResponse {
        match key.code {
            KeyCode::Esc => self.cancel(),
            KeyCode::Up => {
                self.focus_area = FocusArea::List;
                self.last_error = None;
                KeybindsResponse::Continue
            }
            KeyCode::Down | KeyCode::Left | KeyCode::Right => {
                self.focus_area = match self.focus_area {
                    FocusArea::Save => FocusArea::Cancel,
                    FocusArea::Cancel => FocusArea::Save,
                    FocusArea::List => FocusArea::List,
                };
                KeybindsResponse::Continue
            }
            KeyCode::Enter | KeyCode::Char(' ') => match self.focus_area {
                FocusArea::Save => self.save(),
                FocusArea::Cancel => self.cancel(),
                FocusArea::List => KeybindsResponse::Continue,
            },
            _ => KeybindsResponse::Continue,
        }
    }

    /// Cycle Tab focus: List → Cancel → Save → List (visual button order).
    fn cycle_focus(&mut self, delta: i32) {
        const ORDER: [FocusArea; 3] = [FocusArea::List, FocusArea::Cancel, FocusArea::Save];
        let cur = ORDER
            .iter()
            .position(|a| *a == self.focus_area)
            .unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(ORDER.len() as i32) as usize;
        self.focus_area = ORDER[next];
        self.last_error = None;
    }

    fn save(&mut self) -> KeybindsResponse {
        KeybindsResponse::Save {
            keymap: self.draft_keymap.clone(),
            overrides: self.draft_overrides.clone(),
        }
    }

    fn cancel(&mut self) -> KeybindsResponse {
        KeybindsResponse::Cancelled
    }

    /// Step focus by `delta` rows, skipping headers.  Returns `false` when it would run off
    /// either end, so the caller can cross into the button row.
    fn move_focus(&mut self, delta: i32) -> bool {
        if let Some(idx) = next_focusable(&self.rows, self.focused, delta, |r| {
            matches!(r, Row::Binding { .. })
        }) {
            self.focused = idx;
            self.last_error = None;
            // ensure_visible takes body-line coords, not row indices.
            let body_row = self.focus_offsets.get(self.focused).copied().unwrap_or(0) as u16;
            self.scroll_state.ensure_visible(body_row);
            true
        } else {
            false
        }
    }

    /// Apply a left-button click.  During capture only the `esc` hint responds (and it cancels
    /// capture, not the overlay); otherwise a click on a binding row focuses it and arms capture.
    pub fn handle_click(&mut self, col: u16, row: u16) -> KeybindsResponse {
        if self.capturing {
            if rect_contains(self.esc_button_rect, col, row) {
                self.capturing = false;
                self.last_error = None;
            }
            return KeybindsResponse::Continue;
        }
        if rect_contains(self.esc_button_rect, col, row)
            || rect_contains(self.cancel_button_rect, col, row)
        {
            return KeybindsResponse::Cancelled;
        }
        if rect_contains(self.save_button_rect, col, row) {
            return self.save();
        }
        if let Some(row_idx) = self.row_hit_rects.iter().find_map(|(idx, r)| {
            if rect_contains(Some(*r), col, row) {
                Some(*idx)
            } else {
                None
            }
        }) {
            if matches!(self.rows.get(row_idx), Some(Row::Binding { .. })) {
                self.focused = row_idx;
                self.focus_area = FocusArea::List;
                self.capturing = true;
                self.last_error = None;
            }
        }
        KeybindsResponse::Continue
    }

    fn first_binding_index(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(r, Row::Binding { .. }))
    }
}

/// Renderer for the keybinds overlay.
pub struct KeybindsView<'a> {
    pub theme: &'a Theme,
}

impl<'a> StatefulWidget for KeybindsView<'a> {
    type State = KeybindsState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let body_lines = build_body_lines(state, &state.draft_keymap, self.theme);

        let content_width = keybinds_content_width(state);

        // Footer: spacer + buttons (always), + capture hint, + error.  The buttons wrap rather
        // than clip, so their row count depends on the width the frame will give them; a flat
        // one-row reservation leaves a wrapped button unpainted yet still focusable.
        let extra_status = (state.capturing as u16) + (state.last_error.is_some() as u16);
        let footer_rows =
            footer_row_count(BUTTON_LABELS, content_width, area.width, KEYBINDS_MAX_PAD_H);
        let pinned_bottom: u16 = 1 + footer_rows + extra_status;

        let content = ContentSize {
            width: content_width,
            height: body_lines.len() as u16,
            pinned_top: 0,
            pinned_bottom,
            max_pad_h: KEYBINDS_MAX_PAD_H,
        };
        let rect = centered_rect_for_content(content, area);

        let inner_h = rect.height.saturating_sub(VERTICAL_CHROME_ROWS);
        let table_height = inner_h.saturating_sub(pinned_bottom);
        state
            .scroll_state
            .observe(body_lines.len() as u16, table_height);
        // Do NOT call ensure_visible here: it would undo wheel/PgUp/PgDn scrolls on every
        // redraw.  It runs only when focus moves (see `move_focus`).

        let title = format!("Keybindings · {}", state.handler_label);
        let layout = draw_frame(
            rect,
            buf,
            FrameOpts {
                title: &title,
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
        let visible: Vec<Line<'_>> = body_lines
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

        let mut footer_y = inner.y + table_height;
        if state.capturing {
            let hint_area = Rect {
                x: inner.x,
                y: footer_y,
                width: inner.width,
                height: 1,
            };
            Paragraph::new(Line::from(Span::styled(
                CAPTURE_HINT,
                self.theme.modal_description,
            )))
            .alignment(Alignment::Center)
            .style(self.theme.modal_bg)
            .render(hint_area, buf);
            footer_y += 1;
        }
        if let Some(err) = state.last_error.as_ref() {
            let err_area = Rect {
                x: inner.x,
                y: footer_y,
                width: inner.width,
                height: 1,
            };
            Paragraph::new(Line::from(Span::styled(
                format!("✗ {err}"),
                self.theme.transient_error,
            )))
            .alignment(Alignment::Center)
            .style(self.theme.modal_bg)
            .render(err_area, buf);
            footer_y += 1;
        }
        footer_y += 1;
        let button_area = Rect {
            x: inner.x,
            y: footer_y,
            width: inner.width,
            height: (inner.y + inner.height).saturating_sub(footer_y),
        };
        let focused_idx = match state.focus_area {
            FocusArea::Cancel => 0,
            FocusArea::Save => 1,
            FocusArea::List => usize::MAX,
        };
        let button_rects =
            render_button_row(button_area, buf, BUTTON_LABELS, focused_idx, self.theme);
        state.cancel_button_rect = button_rects.first().copied();
        state.save_button_rect = button_rects.get(1).copied();

        state.row_hit_rects.clear();
        for (row_idx, row) in state.rows.iter().enumerate() {
            if !matches!(row, Row::Binding { .. }) {
                continue;
            }
            let body_y = match state.focus_offsets.get(row_idx) {
                Some(y) => *y,
                None => continue,
            };
            if body_y < scroll || body_y >= scroll + visible_rows {
                continue;
            }
            let screen_y = table_area.y + (body_y - scroll) as u16;
            state.row_hit_rects.push((
                row_idx,
                Rect {
                    x: table_area.x,
                    y: screen_y,
                    width: table_area.width,
                    height: 1,
                },
            ));
        }
    }
}

/// Build the body lines; must agree with [`compute_focus_offsets`] on line counts.
fn build_body_lines<'a>(state: &KeybindsState, keymap: &KeyMap, theme: &'a Theme) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(state.rows.len() + 2);
    for (idx, row) in state.rows.iter().enumerate() {
        match row {
            Row::Header(title) => {
                if !lines.is_empty() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(Span::styled(
                    format!("— {} —", title),
                    theme.modal_section_heading,
                )));
            }
            Row::Binding { action, label } => {
                let focused = idx == state.focused && state.focus_area == FocusArea::List;
                let capturing = focused && state.capturing;
                let chord = if capturing {
                    "…".to_owned()
                } else {
                    display_chord(keymap, action)
                };
                lines.push(format_modal_row(
                    label,
                    &chord,
                    focused,
                    capturing,
                    theme,
                    RowLayout::FixedPad(LABEL_PAD),
                ));
            }
        }
    }
    lines
}

/// Chord text for `action`.  Diff-review and search-flow actions are hard-bound, not in the
/// [`KeyMap`], so their glyph comes from [`diff_hint`] / [`crate::search::search_hint`].
fn display_chord(keymap: &KeyMap, action: &Action) -> String {
    let hint = diff_hint(action);
    if !hint.is_empty() {
        return hint.to_owned();
    }
    let hint = crate::search::search_hint(action);
    if !hint.is_empty() {
        return hint.to_owned();
    }
    keymap.first_key_for(action).unwrap_or_default()
}

/// Body-line index where each `rows[i]` renders (see [`KeybindsState::focus_offsets`]).
fn compute_focus_offsets(rows: &[Row]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(rows.len());
    let mut line: usize = 0;
    let mut started = false;
    for row in rows {
        match row {
            Row::Header(_) => {
                if started {
                    line += 1; // blank separator
                }
                offsets.push(line);
                line += 1; // header line
                started = true;
            }
            Row::Binding { .. } => {
                offsets.push(line);
                line += 1;
                started = true;
            }
        }
    }
    offsets
}

/// Widest of all rows, the capture hint, the button row, and the error, so the width doesn't
/// jiggle as focus moves.
fn keybinds_content_width(state: &KeybindsState) -> u16 {
    const FOCUS_MARKER_WIDTH: usize = 2;
    let row_max = max_row_width(&state.rows, |r| match r {
        Row::Header(t) => t.chars().count() + 4, // "— x —"
        Row::Binding { action, .. } => {
            let chord_w = display_chord(&state.draft_keymap, action).chars().count();
            FOCUS_MARKER_WIDTH + LABEL_PAD + chord_w
        }
    });
    let err_max = optional_text_width(state.last_error.as_deref(), 2);
    let hint_w = CAPTURE_HINT.chars().count() as u16;
    let buttons_w = button_row_width(BUTTON_LABELS);
    row_max.max(err_max).max(hint_w).max(buttons_w)
}

/// With crossterm's keyboard enhancement, a modifier pressed alone arrives as its own event;
/// capture swallows it so the user can hold the modifier and then press the key.
fn is_bare_modifier(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Modifier(_))
}

/// Point-in-rect test; `None` (not yet rendered) always misses.
fn rect_contains(rect: Option<Rect>, col: u16, row: u16) -> bool {
    match rect {
        Some(r) => col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height,
        None => false,
    }
}

/// Build the row list from `CATEGORIES`. A modal handler hides `ExitToPreview` because Normal
/// replaces Preview as the resting mode, so the action is a no-op.
fn build_rows(modal_active: bool) -> Vec<Row> {
    let mut rows = Vec::new();
    for (title, bindings) in CATEGORIES {
        rows.push(Row::Header(title));
        for (action, label) in *bindings {
            if modal_active && *action == Action::ExitToPreview {
                continue;
            }
            rows.push(Row::Binding {
                action: action.clone(),
                label,
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn keymap() -> KeyMap {
        KeyMap::build(&KeyBindingOverrides::default()).unwrap()
    }

    fn open() -> KeybindsState {
        KeybindsState::open(&keymap(), &KeyBindingOverrides::default(), false)
    }

    fn open_vim() -> KeybindsState {
        KeybindsState::open(&keymap(), &KeyBindingOverrides::default(), true)
    }

    #[test]
    fn hard_bound_flow_actions_display_their_table_glyphs() {
        let km = keymap();
        assert_eq!(display_chord(&km, &Action::DiffAcceptHunk), "y");
        assert_eq!(display_chord(&km, &Action::SearchNext), "Tab");
        assert_eq!(display_chord(&km, &Action::SearchPrev), "⇧Tab");
        assert_eq!(display_chord(&km, &Action::SearchReplace), "r");
        assert_eq!(display_chord(&km, &Action::SearchReplaceAll), "a");
        assert_eq!(display_chord(&km, &Action::SearchExit), "Esc");
        assert_eq!(display_chord(&km, &Action::OpenSearch), "Ctrl-F");
    }

    #[test]
    fn initial_focus_is_first_binding_not_a_header() {
        let state = open();
        assert_eq!(state.focused_action(), Some(Action::Save));
        assert_eq!(state.focus_area, FocusArea::List);
    }

    #[test]
    fn down_skips_over_header_rows() {
        // Derives the expected crossings from the row table itself so reordering CATEGORIES
        // can't make the test pass for the wrong reason.
        let mut state = open();
        let rows = state.rows.clone();
        let starting_header = current_header(&rows, state.focused);
        assert!(
            starting_header.is_some(),
            "initial focus must be inside a category"
        );
        let mut category_starts: Vec<(&'static str, Action)> = Vec::new();
        let mut last_header: Option<&'static str> = None;
        for r in &rows {
            match r {
                Row::Header(t) => last_header = Some(*t),
                Row::Binding { action, .. } => {
                    if let Some(h) = last_header.take() {
                        category_starts.push((h, action.clone()));
                    }
                }
            }
        }
        let mut crossings: Vec<(&'static str, Action)> = Vec::new();
        let mut prev_header = starting_header;
        for _ in 0..rows.len() + 8 {
            let before = state.focused;
            state.handle_key(&key(KeyCode::Down));
            if state.focused == before {
                break;
            }
            let now = current_header(&rows, state.focused);
            if now != prev_header {
                let action = state.focused_action().unwrap();
                crossings.push((now.unwrap(), action));
                prev_header = now;
            }
        }
        let expected: Vec<_> = category_starts.into_iter().skip(1).collect();
        assert_eq!(
            crossings, expected,
            "every Down crossing must land on the first Binding of the next category"
        );
        assert!(
            !crossings.is_empty(),
            "expected at least one category crossing"
        );
    }

    /// Nearest `Header` title at or before `idx`.
    fn current_header(rows: &[Row], idx: usize) -> Option<&'static str> {
        rows[..=idx].iter().rev().find_map(|r| match r {
            Row::Header(t) => Some(*t),
            _ => None,
        })
    }

    #[test]
    fn enter_arms_capture_mode() {
        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        assert!(state.capturing);
    }

    #[test]
    fn captured_chord_writes_to_draft_only() {
        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        let resp = state.handle_key(&key(KeyCode::F(7)));
        assert!(matches!(resp, KeybindsResponse::Continue));
        assert!(!state.capturing, "successful rebind exits capture mode");
        assert_eq!(
            state.draft_overrides.0.get("Save").map(String::as_str),
            Some("f7")
        );
        assert_eq!(
            state.draft_keymap.first_key_for(&Action::Save).as_deref(),
            Some("F7")
        );
    }

    #[test]
    fn captured_chord_with_modifiers_rebinds() {
        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        let chord = KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        state.handle_key(&chord);
        assert_eq!(
            state.draft_keymap.first_key_for(&Action::Save).as_deref(),
            Some("Ctrl-Alt-J")
        );
    }

    #[test]
    fn pageup_capture_round_trips_through_format_and_parse() {
        // Regression: parse_key must accept the `PgUp` spelling format_key emits.  Shift+PgUp
        // avoids the conflict with ScrollPageUp's default binding.
        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        let shift_pgup = KeyEvent::new(KeyCode::PageUp, KeyModifiers::SHIFT);
        state.handle_key(&shift_pgup);
        assert!(
            state.last_error.is_none(),
            "Shift+PgUp capture must not error, got {:?}",
            state.last_error
        );
        assert_eq!(
            state.draft_keymap.first_key_for(&Action::Save).as_deref(),
            Some("Shift-PgUp")
        );
    }

    #[test]
    fn unsupported_keycode_surfaces_inline_error_and_keeps_capture() {
        use crossterm::event::MediaKeyCode;
        // A Debug-stringified media key would surface as UnparseableKey on next load.
        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        let media = KeyEvent::new(KeyCode::Media(MediaKeyCode::PlayPause), KeyModifiers::NONE);
        state.handle_key(&media);
        assert!(state.capturing, "unsupported key must keep capture armed");
        assert!(
            state
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("Unsupported")),
            "expected 'Unsupported' in error, got: {:?}",
            state.last_error
        );
        assert_eq!(
            state.draft_keymap.first_key_for(&Action::Save).as_deref(),
            Some("Ctrl-S")
        );
    }

    #[test]
    fn hyphen_and_plus_keys_are_capturable() {
        // Regression: see the `format_key_parseable` note in `handle_capture_key`.
        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        state.handle_key(&key(KeyCode::Char('-')));
        assert!(
            state.last_error.is_none(),
            "`-` capture must not error, got {:?}",
            state.last_error
        );
        assert_eq!(
            state.draft_keymap.first_key_for(&Action::Save).as_deref(),
            Some("-")
        );

        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        state.handle_key(&key(KeyCode::Char('+')));
        assert!(
            state.last_error.is_none(),
            "`+` capture must not error, got {:?}",
            state.last_error
        );
        assert_eq!(
            state.draft_keymap.first_key_for(&Action::Save).as_deref(),
            Some("+")
        );
    }

    #[test]
    fn conflict_error_uses_human_readable_chord() {
        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        state.handle_key(&KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        let msg = state
            .last_error
            .as_deref()
            .expect("conflict surfaces error");
        assert!(
            msg.contains("Ctrl-Q") && !msg.contains("ctrl+q"),
            "expected human-readable chord in conflict message, got: {msg}"
        );
    }

    #[test]
    fn conflicting_chord_is_rejected_with_sticky_error() {
        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        let conflict = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        let resp = state.handle_key(&conflict);
        assert!(matches!(resp, KeybindsResponse::Continue));
        assert!(state.capturing, "conflict keeps user in capture mode");
        assert!(state.last_error.is_some());
        assert_eq!(
            state.draft_keymap.first_key_for(&Action::Save).as_deref(),
            Some("Ctrl-S")
        );
    }

    #[test]
    fn escape_cancels_capture_only() {
        let mut state = open();
        state.handle_key(&key(KeyCode::Enter));
        assert!(state.capturing);
        let resp = state.handle_key(&key(KeyCode::Esc));
        assert!(matches!(resp, KeybindsResponse::Continue));
        assert!(!state.capturing);
    }

    #[test]
    fn bare_modifier_press_is_ignored_in_capture_mode() {
        use crossterm::event::ModifierKeyCode;
        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        let bare = KeyEvent::new(
            KeyCode::Modifier(ModifierKeyCode::LeftControl),
            KeyModifiers::CONTROL,
        );
        let resp = state.handle_key(&bare);
        assert!(matches!(resp, KeybindsResponse::Continue));
        assert!(state.capturing, "bare modifier must not exit capture");
        assert_eq!(
            state.draft_keymap.first_key_for(&Action::Save).as_deref(),
            Some("Ctrl-S")
        );
    }

    #[test]
    fn tab_cycles_focus_list_cancel_save() {
        let mut state = open();
        assert_eq!(state.focus_area, FocusArea::List);
        state.handle_key(&key(KeyCode::Tab));
        assert_eq!(state.focus_area, FocusArea::Cancel);
        state.handle_key(&key(KeyCode::Tab));
        assert_eq!(state.focus_area, FocusArea::Save);
        state.handle_key(&key(KeyCode::Tab));
        assert_eq!(state.focus_area, FocusArea::List);
    }

    #[test]
    fn shift_tab_cycles_backwards() {
        let mut state = open();
        state.handle_key(&KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(state.focus_area, FocusArea::Save);
    }

    #[test]
    fn down_from_last_binding_focuses_cancel_first() {
        let mut state = open();
        loop {
            let before = state.focused;
            state.handle_key(&key(KeyCode::Down));
            if state.focused == before {
                break;
            }
        }
        assert_eq!(state.focus_area, FocusArea::Cancel);
    }

    #[test]
    fn enter_on_save_button_emits_save_response() {
        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        state.handle_key(&key(KeyCode::F(7)));
        state.focus_area = FocusArea::Save;
        let resp = state.handle_key(&key(KeyCode::Enter));
        match resp {
            KeybindsResponse::Save { keymap, overrides } => {
                assert_eq!(overrides.0.get("Save").map(String::as_str), Some("f7"));
                assert_eq!(keymap.first_key_for(&Action::Save).as_deref(), Some("F7"));
            }
            other => panic!("expected Save, got {:?}", other),
        }
    }

    #[test]
    fn enter_on_cancel_button_discards_draft() {
        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        state.handle_key(&key(KeyCode::F(7)));
        assert_eq!(
            state.draft_keymap.first_key_for(&Action::Save).as_deref(),
            Some("F7")
        );
        state.focus_area = FocusArea::Cancel;
        let resp = state.handle_key(&key(KeyCode::Enter));
        assert!(matches!(resp, KeybindsResponse::Cancelled));
    }

    #[test]
    fn escape_in_list_cancels_overlay_without_save() {
        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        state.handle_key(&key(KeyCode::F(7)));
        let resp = state.handle_key(&key(KeyCode::Esc));
        assert!(matches!(resp, KeybindsResponse::Cancelled));
    }

    #[test]
    fn left_right_swap_buttons_when_focused_on_a_button() {
        let mut state = open();
        state.focus_area = FocusArea::Save;
        state.handle_key(&key(KeyCode::Right));
        assert_eq!(state.focus_area, FocusArea::Cancel);
        state.handle_key(&key(KeyCode::Left));
        assert_eq!(state.focus_area, FocusArea::Save);
    }

    #[test]
    fn down_from_cancel_moves_to_save() {
        let mut state = open();
        state.focus_area = FocusArea::Cancel;
        state.handle_key(&key(KeyCode::Down));
        assert_eq!(state.focus_area, FocusArea::Save);
    }

    #[test]
    fn down_from_save_moves_to_cancel() {
        let mut state = open();
        state.focus_area = FocusArea::Save;
        state.handle_key(&key(KeyCode::Down));
        assert_eq!(state.focus_area, FocusArea::Cancel);
    }

    #[test]
    fn up_from_button_returns_focus_to_list() {
        let mut state = open();
        state.focus_area = FocusArea::Save;
        state.handle_key(&key(KeyCode::Up));
        assert_eq!(state.focus_area, FocusArea::List);
    }

    #[test]
    fn excluded_actions_are_not_rows() {
        let rows = build_rows(false);
        for excluded in [Action::ScrollPageUp, Action::ScrollPageDown] {
            for row in &rows {
                if let Row::Binding { action, .. } = row {
                    assert_ne!(
                        action, &excluded,
                        "{excluded} should not appear in keybindings overlay"
                    );
                }
            }
        }
    }

    #[test]
    fn editor_section_includes_mode_switching() {
        let rows = build_rows(false);
        let mut current_header: Option<&'static str> = None;
        for row in &rows {
            match row {
                Row::Header(t) => current_header = Some(t),
                Row::Binding { action, label } => {
                    if matches!(action, Action::ExitToPreview | Action::ToggleRawMode) {
                        assert_eq!(current_header, Some("Editor"));
                        if action == &Action::ToggleRawMode {
                            assert_eq!(*label, "Toggle raw/render");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn vim_mode_hides_preview_mode_row() {
        let default_rows = build_rows(false);
        assert!(
            default_rows.iter().any(|r| matches!(
                r,
                Row::Binding { action, .. } if *action == Action::ExitToPreview
            )),
            "Preview mode row should be present without vim"
        );
        let vim_rows = build_rows(true);
        assert!(
            !vim_rows.iter().any(|r| matches!(
                r,
                Row::Binding { action, .. } if *action == Action::ExitToPreview
            )),
            "Preview mode row should be hidden with vim enabled"
        );
        assert!(vim_rows.iter().any(|r| matches!(
            r,
            Row::Binding { action, .. } if *action == Action::Save
        )));
    }

    #[test]
    fn vim_overlay_initial_focus_is_valid() {
        let state = open_vim();
        assert!(matches!(
            state.rows.get(state.focused),
            Some(Row::Binding { .. })
        ));
    }

    #[test]
    fn table_section_has_cell_navigation() {
        let rows = build_rows(false);
        let mut in_table = false;
        let mut found_next_cell = false;
        for row in &rows {
            match row {
                Row::Header(t) => in_table = *t == "Table",
                Row::Binding { action, .. } if in_table => {
                    if matches!(action, Action::TableNextCell) {
                        found_next_cell = true;
                    }
                }
                _ => {}
            }
        }
        assert!(found_next_cell, "Table section missing TableNextCell row");
    }

    // ── Scroll-container integration ────────────────────────────────────

    use ratatui::{backend::TestBackend, Terminal};

    fn theme_ref() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn render(state: &mut KeybindsState, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_stateful_widget(
                    KeybindsView { theme: theme_ref() },
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
    fn keybinds_renders_scrollbar_when_more_rows_than_visible_height() {
        let mut state = open();
        let contents = render(&mut state, 80, 12);
        assert!(
            contents.contains('█'),
            "expected scrollbar thumb glyph, got: {contents}"
        );
    }

    #[test]
    fn keybinds_pgdown_advances_scroll_without_moving_focus() {
        let mut state = open();
        render(&mut state, 80, 12);
        let focused_before = state.focused;
        state.handle_key(&key(KeyCode::PageDown));
        assert_eq!(state.focused, focused_before);
        assert!(state.scroll_state.scroll > 0, "PgDn must advance scroll");
    }

    #[test]
    fn keybinds_pgdown_scroll_survives_subsequent_render() {
        // Regression: ensure_visible used to run on every render and snap scroll back.
        let mut state = open();
        render(&mut state, 80, 18);
        state.handle_key(&key(KeyCode::PageDown));
        let scroll_after_pgdn = state.scroll_state.scroll;
        assert!(scroll_after_pgdn > 0, "PgDn must advance scroll");
        render(&mut state, 80, 18);
        assert_eq!(
            state.scroll_state.scroll, scroll_after_pgdn,
            "render after PgDn must preserve scroll, not snap back to focused row",
        );
    }

    #[test]
    fn keybinds_wheel_scrolls_list() {
        let mut state = open();
        render(&mut state, 80, 12);
        let focused_before = state.focused;
        state.scroll_state.scroll_by(2);
        assert_eq!(state.scroll_state.scroll, 2);
        assert_eq!(state.focused, focused_before);
    }

    #[test]
    fn keybinds_wheel_scroll_survives_subsequent_render() {
        let mut state = open();
        render(&mut state, 80, 18);
        state.scroll_state.scroll_by(5);
        let scroll_after_wheel = state.scroll_state.scroll;
        assert!(scroll_after_wheel > 0);
        render(&mut state, 80, 18);
        assert_eq!(state.scroll_state.scroll, scroll_after_wheel);
    }

    #[test]
    fn keybinds_modal_width_shrinks_to_content_in_wide_terminal() {
        let mut state = open();
        let term_w = 200u16;
        let term_h = 40u16;
        let contents = render(&mut state, term_w, term_h);
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
    fn keybinds_modal_uses_raised_horizontal_padding() {
        use crate::ui::scroll_container::{compute_pad_h, MIN_PAD_H};
        assert_eq!(
            compute_pad_h(200, 30, KEYBINDS_MAX_PAD_H),
            KEYBINDS_MAX_PAD_H,
            "wide-terminal pad_h must reach the keybinds cap"
        );
        assert_eq!(
            compute_pad_h(32, 30, KEYBINDS_MAX_PAD_H),
            MIN_PAD_H,
            "narrow-terminal pad_h must still degrade to MIN_PAD_H"
        );
        use crate::ui::scroll_container::{centered_rect_for_content, ContentSize};
        use ratatui::layout::Rect;
        let state = open();
        let cw = keybinds_content_width(&state);
        let area = Rect::new(0, 0, 200, 40);
        let r = centered_rect_for_content(
            ContentSize {
                width: cw,
                height: 5,
                pinned_top: 0,
                pinned_bottom: 2,
                max_pad_h: KEYBINDS_MAX_PAD_H,
            },
            area,
        );
        assert_eq!(
            r.width,
            cw + 2 * KEYBINDS_MAX_PAD_H,
            "wide-terminal modal width must include raised padding on both sides"
        );
    }

    #[test]
    fn a_narrow_terminal_wraps_the_footer_and_still_paints_both_buttons() {
        // The footer reservation must be computed at KEYBINDS_MAX_PAD_H, not the default.
        let mut state = open();
        let contents = render(&mut state, 20, 24);
        assert!(contents.contains("[ Cancel ]"), "{contents}");
        assert!(contents.contains("[ Save ]"), "{contents}");
    }

    #[test]
    fn buttons_row_is_always_rendered() {
        let mut state = open();
        let contents = render(&mut state, 80, 40);
        assert!(contents.contains("[ Save ]"), "Save button missing");
        assert!(contents.contains("[ Cancel ]"), "Cancel button missing");
    }

    #[test]
    fn click_on_cancel_button_returns_cancelled() {
        let mut state = open();
        render(&mut state, 80, 40);
        let rect = state.cancel_button_rect.expect("Cancel rect populated");
        let resp = state.handle_click(rect.x + 2, rect.y);
        assert!(matches!(resp, KeybindsResponse::Cancelled));
    }

    #[test]
    fn click_on_save_button_returns_save_with_drafts() {
        let mut state = open();
        // Stage a draft edit so the Save response carries observable state.
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        state.handle_key(&key(KeyCode::F(7)));
        render(&mut state, 80, 40);
        let rect = state.save_button_rect.expect("Save rect populated");
        let resp = state.handle_click(rect.x + 2, rect.y);
        match resp {
            KeybindsResponse::Save { overrides, .. } => {
                assert_eq!(overrides.0.get("Save").map(String::as_str), Some("f7"));
            }
            other => panic!("expected Save, got {other:?}"),
        }
    }

    #[test]
    fn click_on_binding_row_focuses_and_arms_capture() {
        let mut state = open();
        render(&mut state, 80, 40);
        let (row_idx, rect) = state
            .row_hit_rects
            .iter()
            .find(|(idx, _)| {
                matches!(
                    state.rows.get(*idx),
                    Some(Row::Binding { action, .. }) if *action == Action::Copy
                )
            })
            .map(|(i, r)| (*i, *r))
            .expect("Copy row visible");
        let resp = state.handle_click(rect.x + 4, rect.y);
        assert!(matches!(resp, KeybindsResponse::Continue));
        assert_eq!(state.focused, row_idx);
        assert!(state.capturing, "click on a binding row must arm capture");
    }

    #[test]
    fn click_during_capture_is_ignored_except_esc() {
        let mut state = open();
        render(&mut state, 80, 40);
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        assert!(state.capturing);
        let save_rect = state.save_button_rect.expect("Save rect populated");
        let resp = state.handle_click(save_rect.x + 2, save_rect.y);
        assert!(matches!(resp, KeybindsResponse::Continue));
        assert!(state.capturing, "non-esc clicks must not exit capture");
        let esc_rect = state.esc_button_rect.expect("Esc rect populated");
        let resp = state.handle_click(esc_rect.x, esc_rect.y);
        assert!(matches!(resp, KeybindsResponse::Continue));
        assert!(!state.capturing, "esc click must exit capture mode");
    }

    #[test]
    fn click_on_esc_hint_cancels_overlay() {
        let mut state = open();
        render(&mut state, 80, 40);
        let rect = state.esc_button_rect.expect("Esc rect populated");
        let resp = state.handle_click(rect.x, rect.y);
        assert!(matches!(resp, KeybindsResponse::Cancelled));
    }

    #[test]
    fn capture_hint_appears_in_footer_when_capturing() {
        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        let contents = render(&mut state, 80, 40);
        assert!(
            contents.contains("Press a key"),
            "expected capture hint in footer, got: {contents}"
        );
    }
}
