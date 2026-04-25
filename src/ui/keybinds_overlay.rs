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
    widgets::{Block, Borders, Clear, Paragraph, StatefulWidget, Widget},
};

use crate::config::{Action, KeyBindingOverrides, KeyMap, KeyMapError, Theme};

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
        let rect = overlay_rect(area, state.rows.len() as u16);
        Clear.render(rect, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Keybindings ", self.theme.modal_title))
            .style(self.theme.status_bar);
        let inner = block.inner(rect);
        block.render(rect, buf);
        if inner.height < 2 || inner.width == 0 {
            return;
        }

        let mut lines: Vec<Line<'_>> = Vec::with_capacity(state.rows.len() + 2);
        for (idx, row) in state.rows.iter().enumerate() {
            match row {
                Row::Header(title) => {
                    // Skip header *separator* row when at top of body.
                    if !lines.is_empty() {
                        lines.push(Line::from(""));
                    }
                    lines.push(Line::from(Span::styled(
                        format!("— {} —", title),
                        self.theme.h2,
                    )));
                }
                Row::Binding { action, label } => {
                    let focused = idx == state.focused;
                    let chord = if focused && state.editing.is_some() {
                        format!("{}▏", state.editing.as_deref().unwrap_or(""))
                    } else {
                        self.keymap.first_key_for(action).unwrap_or_default()
                    };
                    let marker = if focused { "› " } else { "  " };
                    let label_style = if focused {
                        self.theme.modal_button_focused
                    } else {
                        self.theme.status_bar
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("{marker}{:<22}", label), label_style),
                        Span::styled(chord, self.theme.status_filename),
                    ]));
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
    let target_width = (area.width as usize * 8 / 10).max(50);
    let width = target_width.min(area.width as usize) as u16;
    // Each category adds a blank separator + the header row, plus
    // the binding rows themselves.  Add 4 for borders + error line.
    let height = (rows + 6).min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
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
}
