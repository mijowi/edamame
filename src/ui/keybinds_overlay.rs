//! Phase 10 — keybindings overlay.
//!
//! Combined view + editor for keybindings.  Rows are grouped into
//! categories (Editor, Navigation, Links, List, Table, …) so the user
//! can scan related chords at a glance, and Enter on any row opens an
//! inline chord-string editor.  On confirm the live `KeyMap` is
//! mutated and the overrides table is updated; the caller is then
//! responsible for persisting via [`KeyBindingOverrides::save_to`].
//!
//! Conflict detection delegates to [`KeyMap::rebind`]: the caller
//! pattern-matches on [`KeyMapError::ConflictingBinding`] to flash a
//! sticky `Error`.  The overlay also surfaces the same error inline
//! so the user sees immediately why the rebind didn't take.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::config::{Action, KeyBindingOverrides, KeyMap, KeyMapError, Theme};
use crate::ui::scroll_container::{
    centered_rect_for_content, draw_frame, ContentSize, ScrollContainerState,
};

/// Outcome of dispatching a key event to the keybinds overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeybindsResponse {
    Continue,
    Cancelled,
    /// A rebind succeeded.  Caller should persist
    /// [`KeyBindingOverrides`] (which has already been mutated by
    /// [`KeybindsState::handle_key`]) and flash a confirmation.
    Rebound {
        action: Action,
        key: String,
    },
    /// A conflict was detected; the rebind was rejected.  Caller is
    /// expected to flash a sticky `Error` describing the conflict.
    Conflict {
        key: String,
        existing_action: String,
    },
}

/// One row in the overlay.  `Header` rows are display-only — the
/// focus skips over them; `Binding` rows are editable.
#[derive(Debug, Clone)]
enum Row {
    Header(&'static str),
    Binding { action: Action, label: &'static str },
}

/// Mutable state for an open keybinds overlay.
pub struct KeybindsState {
    /// Index into [`Self::rows`].  May land on a `Header` after a
    /// rebuild; [`Self::clamp_focus`] re-snaps to the nearest
    /// `Binding` row.
    pub focused: usize,
    /// `Some(buffer)` while an inline chord editor is open on the
    /// focused row.
    pub editing: Option<String>,
    /// Last error message produced by an invalid value.  Cleared on
    /// the next successful edit / cancel.
    pub last_error: Option<String>,
    /// Vertical scroll bookkeeping for the row table.  Up/Down move
    /// `focused` and pull the viewport via `ensure_visible`; PgUp/PgDn
    /// and the mouse wheel drive `scroll_state.scroll` directly without
    /// touching focus.
    pub scroll_state: ScrollContainerState,
    /// All rows, including category headers.  Built once at
    /// construction time from the static `CATEGORIES` table; cheap to
    /// clone for tests.
    rows: Vec<Row>,
}

impl KeybindsState {
    /// Construct the overlay state.  `_keymap` is currently unused
    /// (the row list is hard-coded), but is accepted in case future
    /// phases want to dynamically include only bound actions.
    pub fn open(_keymap: &KeyMap) -> Self {
        let rows = build_rows();
        let mut state = Self {
            focused: 0,
            editing: None,
            last_error: None,
            scroll_state: ScrollContainerState::default(),
            rows,
        };
        state.focused = state.first_binding_index().unwrap_or(0);
        state
    }

    /// The action of the currently focused row, if any.  Used by
    /// tests so the row layout can be queried without exposing the
    /// internal `Row` enum.
    pub fn focused_action(&self) -> Option<Action> {
        match self.rows.get(self.focused) {
            Some(Row::Binding { action, .. }) => Some(action.clone()),
            _ => None,
        }
    }

    /// Move `focused` to the row whose `Binding.action == target`.
    /// Returns true on success.  Used by tests + by the
    /// `dispatch_palette_action` smoke test.
    pub fn focus_action(&mut self, target: &Action) -> bool {
        for (idx, row) in self.rows.iter().enumerate() {
            if let Row::Binding { action, .. } = row {
                if action == target {
                    self.focused = idx;
                    return true;
                }
            }
        }
        false
    }

    /// Apply a key event.  When in editing mode, keystrokes form a
    /// new chord-string buffer; on Enter the rebind is attempted.
    pub fn handle_key(
        &mut self,
        key: &KeyEvent,
        keymap: &mut KeyMap,
        overrides: &mut KeyBindingOverrides,
    ) -> KeybindsResponse {
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
                    let new_key = buf.trim().to_owned();
                    let action = match self.rows.get(self.focused) {
                        Some(Row::Binding { action, .. }) => action.clone(),
                        _ => return KeybindsResponse::Continue,
                    };
                    return match keymap.rebind(&action, &new_key, overrides) {
                        Ok(()) => {
                            self.editing = None;
                            self.last_error = None;
                            KeybindsResponse::Rebound {
                                action,
                                key: new_key,
                            }
                        }
                        Err(KeyMapError::ConflictingBinding {
                            key,
                            action: existing_action,
                        }) => {
                            self.last_error =
                                Some(format!("'{key}' is already bound to {existing_action}"));
                            KeybindsResponse::Conflict {
                                key,
                                existing_action,
                            }
                        }
                        Err(e) => {
                            self.last_error = Some(e.to_string());
                            KeybindsResponse::Continue
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
            return KeybindsResponse::Continue;
        }
        // PgUp/PgDn/Home/End move the viewport without touching focus.
        if self.scroll_state.handle_paging_key(key) {
            return KeybindsResponse::Continue;
        }
        match key.code {
            KeyCode::Esc => KeybindsResponse::Cancelled,
            KeyCode::Up => {
                self.move_focus(-1);
                KeybindsResponse::Continue
            }
            KeyCode::Down => {
                self.move_focus(1);
                KeybindsResponse::Continue
            }
            KeyCode::Enter => {
                // Pre-fill the inline buffer with the existing chord
                // so the user can edit rather than retype from scratch.
                let action = match self.rows.get(self.focused) {
                    Some(Row::Binding { action, .. }) => action.clone(),
                    _ => return KeybindsResponse::Continue,
                };
                let initial = keymap
                    .first_key_for(&action)
                    .map(|s| s.to_ascii_lowercase().replace('-', "+"))
                    .unwrap_or_default();
                self.editing = Some(initial);
                self.last_error = None;
                KeybindsResponse::Continue
            }
            _ => KeybindsResponse::Continue,
        }
    }

    /// Step the focus by `delta` rows, skipping over `Header` rows.
    /// Stops at the first/last binding rather than wrapping — wrapping
    /// makes overlay navigation feel jumpy.
    fn move_focus(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as i32;
        let mut idx = self.focused as i32 + delta;
        while (0..len).contains(&idx) {
            if matches!(self.rows[idx as usize], Row::Binding { .. }) {
                self.focused = idx as usize;
                self.scroll_state.ensure_visible(self.focused as u16);
                return;
            }
            idx += delta;
        }
        // No further binding row in that direction — leave focus where it was.
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
    pub keymap: &'a KeyMap,
}

impl<'a> StatefulWidget for KeybindsView<'a> {
    type State = KeybindsState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Build all body lines first.  Headers introduce a blank
        // separator above themselves (except at the top), so the
        // expanded line count is greater than `state.rows.len()` and
        // must be computed up-front for accurate scroll bookkeeping.
        let body_lines = build_body_lines(state, self.keymap, self.theme);
        let scroll_offsets_per_row = focus_offsets(state);

        let content_width = keybinds_content_width(state, self.keymap);
        let pinned_bottom: u16 = if state.last_error.is_some() { 2 } else { 0 };
        let content = ContentSize {
            width: content_width,
            height: body_lines.len() as u16,
            pinned_top: 0,
            pinned_bottom,
        };
        let rect = centered_rect_for_content(content, area);

        let inner_h = rect.height.saturating_sub(2);
        let table_height = inner_h.saturating_sub(pinned_bottom);
        state
            .scroll_state
            .observe(body_lines.len() as u16, table_height);
        // ensure_visible operates on the *body-line* coordinate, not
        // the rows index: the focused binding row sits at
        // `scroll_offsets_per_row[focused]` body lines down.
        let focus_body_row = scroll_offsets_per_row
            .get(state.focused)
            .copied()
            .unwrap_or(0) as u16;
        state.scroll_state.ensure_visible(focus_body_row);

        let inner = draw_frame(
            rect,
            buf,
            "Keybindings",
            state.scroll_state.arrow(),
            self.theme,
        );
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

        if let Some(err) = state.last_error.as_ref() {
            // Blank spacer row, then the error.
            let err_area = Rect {
                x: inner.x,
                y: inner.y + table_height + 1,
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

/// Build the full body line list, mirroring the renderer above so
/// scroll bookkeeping uses identical line counts.
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
                let focused = idx == state.focused;
                let editing = focused && state.editing.is_some();
                let chord = if editing {
                    format!("{}▏", state.editing.as_deref().unwrap_or(""))
                } else {
                    keymap.first_key_for(action).unwrap_or_default()
                };
                let marker = if focused { "› " } else { "  " };
                let label_style = if focused {
                    theme.modal_item_selected
                } else {
                    theme.modal_item
                };
                let chord_style = if editing {
                    theme.modal_input_focused
                } else if focused {
                    theme.modal_item_selected_hint
                } else {
                    theme.modal_item.patch(theme.footnote)
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{marker}{:<22}", label), label_style),
                    Span::styled(chord, chord_style),
                ]));
            }
        }
    }
    lines
}

/// For each `state.rows[i]`, the body-line index where that row
/// renders.  Used by `ensure_visible` to translate focused-row index
/// into the body coords the scroll state operates in.
fn focus_offsets(state: &KeybindsState) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(state.rows.len());
    let mut line: usize = 0;
    let mut started = false;
    for row in &state.rows {
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

/// Content-aware width: max over rows of `marker(2) + label_pad(22) +
/// chord_w`, plus the longest header (`— Title —`) and the longest
/// error so neither gets clipped.  Sized over the whole row set so
/// width doesn't jiggle as focus moves.
fn keybinds_content_width(state: &KeybindsState, keymap: &KeyMap) -> u16 {
    let row_max = state
        .rows
        .iter()
        .map(|r| match r {
            Row::Header(t) => t.chars().count() + 4, // "— x —"
            Row::Binding { action, .. } => {
                let chord_w = keymap
                    .first_key_for(action)
                    .map(|s| s.chars().count())
                    .unwrap_or(0);
                2 + 22 + chord_w
            }
        })
        .max()
        .unwrap_or(0);
    let err_max = state
        .last_error
        .as_deref()
        .map(|e| 2 + e.chars().count())
        .unwrap_or(0);
    row_max.max(err_max) as u16
}

/// Build the row list from the static category table.  Headers and
/// bindings interleave exactly the way they appear on screen.  Adding
/// a category here is the only edit needed to surface a new section
/// in the overlay.
fn build_rows() -> Vec<Row> {
    let mut rows = Vec::new();
    for (title, bindings) in CATEGORIES {
        rows.push(Row::Header(title));
        for (action, label) in *bindings {
            rows.push(Row::Binding {
                action: action.clone(),
                label,
            });
        }
    }
    rows
}

/// Hard-coded category layout.  Editor / Navigation / Links / List /
/// Table, mirroring the original Phase 9 cheat-sheet groupings with a
/// few corrections from review:
///
/// * `PgUp` / `PgDown` (`ScrollPageUp` / `ScrollPageDown`) are
///   intentionally absent — they're discovered by trying the obvious
///   keys and don't need a row in the overlay.
/// * `Toggle raw/render` lives under Editor (not a separate `View`
///   section) because the user thinks of mode-switching as part of
///   the editing surface.
/// * Table cell-navigation actions (Tab / Shift-Tab / Enter / etc.)
///   appear in the Table section so the row/column reorder chords
///   sit alongside the navigation chords that complement them.
const CATEGORIES: &[(&str, &[(Action, &str)])] = &[
    (
        "Editor",
        &[
            (Action::Save, "Save file"),
            (Action::Copy, "Copy"),
            (Action::Cut, "Cut"),
            (Action::Paste, "Paste"),
            (Action::Undo, "Undo"),
            (Action::Redo, "Redo"),
            (Action::Quit, "Quit"),
            (Action::ExitToPreview, "Preview mode"),
            (Action::ToggleRawMode, "Toggle raw/render"),
        ],
    ),
    (
        "Navigation",
        &[
            (Action::MoveWordLeft, "Word left"),
            (Action::MoveWordRight, "Word right"),
            (Action::MoveLineEnd, "Line end"),
            (Action::MoveDocStart, "Doc start"),
            (Action::MoveDocEnd, "Doc end"),
            (Action::SelectAll, "Select all"),
        ],
    ),
    ("Links", &[(Action::FollowLinkUnderCursor, "Follow link")]),
    ("List", &[(Action::ToggleCheckbox, "Toggle checkbox")]),
    (
        "Table",
        &[
            (Action::TableNextCell, "Next cell"),
            (Action::TablePrevCell, "Prev cell"),
            (Action::TableNextRow, "Next row"),
            (Action::TablePrevRow, "Prev row"),
            (Action::TableMoveRowUp, "Move row up"),
            (Action::TableMoveRowDown, "Move row down"),
            (Action::TableMoveColumnLeft, "Move col left"),
            (Action::TableMoveColumnRight, "Move col right"),
            (Action::TableInsertRowAbove, "Insert row above"),
            (Action::TableInsertRowBelow, "Insert row below"),
            (Action::TableInsertColumnLeft, "Insert col left"),
            (Action::TableInsertColumnRight, "Insert col right"),
            (Action::TableDeleteRow, "Delete row"),
            (Action::TableDeleteColumn, "Delete column"),
            (Action::TableInsertBreak, "Cell line break"),
        ],
    ),
];

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

    #[test]
    fn initial_focus_is_first_binding_not_a_header() {
        let km = keymap();
        let state = KeybindsState::open(&km);
        // The initial row is "Save file" under the Editor header.
        assert_eq!(state.focused_action(), Some(Action::Save));
    }

    #[test]
    fn down_skips_over_header_rows() {
        let km = keymap();
        let mut state = KeybindsState::open(&km);
        let mut km = km;
        let mut overrides = KeyBindingOverrides::default();

        // Walk from the start to the last Editor binding (Toggle
        // raw/render).  Then one more Down should skip the
        // Navigation header and land on Word left.
        while state.focused_action() != Some(Action::ToggleRawMode) {
            state.handle_key(&key(KeyCode::Down), &mut km, &mut overrides);
        }
        state.handle_key(&key(KeyCode::Down), &mut km, &mut overrides);
        assert_eq!(state.focused_action(), Some(Action::MoveWordLeft));
    }

    #[test]
    fn enter_seeds_inline_editor_with_current_chord() {
        let km = keymap();
        let mut state = KeybindsState::open(&km);
        assert!(state.focus_action(&Action::Save));
        let mut km = km;
        let mut overrides = KeyBindingOverrides::default();
        state.handle_key(&key(KeyCode::Enter), &mut km, &mut overrides);
        assert_eq!(state.editing.as_deref(), Some("ctrl+s"));
    }

    #[test]
    fn rebinding_to_unused_chord_succeeds() {
        let mut km = keymap();
        let mut overrides = KeyBindingOverrides::default();
        let mut state = KeybindsState::open(&km);
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter), &mut km, &mut overrides);
        for _ in 0..6 {
            state.handle_key(&key(KeyCode::Backspace), &mut km, &mut overrides);
        }
        for c in "f7".chars() {
            state.handle_key(&key(KeyCode::Char(c)), &mut km, &mut overrides);
        }
        let resp = state.handle_key(&key(KeyCode::Enter), &mut km, &mut overrides);
        assert!(matches!(resp, KeybindsResponse::Rebound { .. }));
        assert_eq!(overrides.0.get("Save").map(String::as_str), Some("f7"));
        assert_eq!(km.first_key_for(&Action::Save).as_deref(), Some("F7"));
    }

    #[test]
    fn conflicting_chord_is_rejected_with_sticky_error() {
        let mut km = keymap();
        let mut overrides = KeyBindingOverrides::default();
        let mut state = KeybindsState::open(&km);
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter), &mut km, &mut overrides);
        for _ in 0..6 {
            state.handle_key(&key(KeyCode::Backspace), &mut km, &mut overrides);
        }
        for c in "ctrl+q".chars() {
            state.handle_key(&key(KeyCode::Char(c)), &mut km, &mut overrides);
        }
        let resp = state.handle_key(&key(KeyCode::Enter), &mut km, &mut overrides);
        assert!(matches!(resp, KeybindsResponse::Conflict { .. }));
        assert!(state.last_error.is_some());
        assert_eq!(km.first_key_for(&Action::Save).as_deref(), Some("Ctrl-S"));
    }

    #[test]
    fn escape_in_inline_editor_cancels_only_the_edit() {
        let mut km = keymap();
        let mut overrides = KeyBindingOverrides::default();
        let mut state = KeybindsState::open(&km);
        state.handle_key(&key(KeyCode::Enter), &mut km, &mut overrides);
        assert!(state.editing.is_some());
        let resp = state.handle_key(&key(KeyCode::Esc), &mut km, &mut overrides);
        assert_eq!(resp, KeybindsResponse::Continue);
        assert!(state.editing.is_none());
    }

    #[test]
    fn excluded_actions_are_not_rows() {
        // PgUp / PgDown were removed from the overlay per the
        // Phase 10 review — confirm both via the Action set the row
        // builder produces.
        let rows = build_rows();
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
        // Per review, "Preview mode" and "Toggle raw/render" sit
        // under Editor (not a separate View category).
        let rows = build_rows();
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
    fn table_section_has_cell_navigation() {
        let rows = build_rows();
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

    fn render(state: &mut KeybindsState, keymap: &KeyMap, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_stateful_widget(
                    KeybindsView {
                        theme: theme_ref(),
                        keymap,
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
    fn keybinds_renders_scroll_arrow_when_more_rows_than_visible_height() {
        let km = keymap();
        let mut state = KeybindsState::open(&km);
        // 80 cols × 8 rows: only ~5 row slots; keybinds has many more.
        let contents = render(&mut state, &km, 80, 8);
        assert!(
            contents.contains("Keybindings ↓") || contents.contains("Keybindings ↑↓"),
            "expected scroll arrow in title, got: {contents}"
        );
    }

    #[test]
    fn keybinds_pgdown_advances_scroll_without_moving_focus() {
        let mut km = keymap();
        let mut overrides = KeyBindingOverrides::default();
        let mut state = KeybindsState::open(&km);
        render(&mut state, &km, 80, 8);
        let focused_before = state.focused;
        state.handle_key(&key(KeyCode::PageDown), &mut km, &mut overrides);
        assert_eq!(state.focused, focused_before);
        assert!(state.scroll_state.scroll > 0, "PgDn must advance scroll");
    }

    #[test]
    fn keybinds_wheel_scrolls_list() {
        let km = keymap();
        let mut state = KeybindsState::open(&km);
        render(&mut state, &km, 80, 8);
        let focused_before = state.focused;
        state.scroll_state.scroll_by(2);
        assert_eq!(state.scroll_state.scroll, 2);
        assert_eq!(state.focused, focused_before);
    }

    #[test]
    fn keybinds_modal_width_shrinks_to_content_in_wide_terminal() {
        let km = keymap();
        let mut state = KeybindsState::open(&km);
        let term_w = 200u16;
        let term_h = 40u16;
        let contents = render(&mut state, &km, term_w, term_h);
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
}
