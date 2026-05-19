//! Phase 10 — keybindings overlay.
//!
//! Combined view + editor for keybindings.  Rows are grouped into
//! categories (Editor, Navigation, Links, List, Table, …) so the user
//! can scan related chords at a glance, and Enter on any row arms a
//! one-press chord-capture mode for that row.
//!
//! Edits are *buffered*: rebinds mutate an internal draft `KeyMap`
//! and draft `KeyBindingOverrides` that the overlay owns from open
//! to close.  Nothing is written back to the live keymap or to
//! `keybindings.toml` until the user activates the `[ Save ]` button
//! (Tab into it, or click).  `[ Cancel ]` and Esc both discard the
//! draft so a mistaken rebind is trivially undoable.
//!
//! Conflict detection delegates to [`KeyMap::rebind`] against the
//! draft, so chains of pending edits (e.g. swapping two bindings)
//! are checked against each other rather than against the original
//! keymap.  The resulting error surfaces inline via
//! [`KeybindsState::last_error`].

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
use crate::ui::button_row::{button_row_width, render_button_row};
use crate::ui::content_width::{max_row_width, optional_text_width};
use crate::ui::modal_row::{format_modal_row, RowLayout};
use crate::ui::overlay_nav::next_focusable;
use crate::ui::scroll_container::{
    centered_rect_for_content, draw_frame, ContentSize, FrameOpts, ModalKind, ScrollContainerState,
    VERTICAL_CHROME_ROWS,
};

/// Width of the action-label column in the keybinds overlay (column count
/// of the padded slot before the chord begins).  Sized to fit the longest
/// action name without clipping; chords sit in the remaining width.
const LABEL_PAD: usize = 22;

const CAPTURE_HINT: &str = "Press a key… (Esc to cancel)";
/// Footer buttons, left-to-right.  Cancel is the leading (left) button so
/// the destructive-by-default option matches the user's reading order and
/// matches the keyboard flow: `Down` from the list lands on Cancel first.
const BUTTON_LABELS: &[&str] = &["Cancel", "Save"];

/// Outcome of dispatching a key event to the keybinds overlay.
#[derive(Debug, Clone)]
pub enum KeybindsResponse {
    Continue,
    /// User discarded the draft (Esc, Cancel button, or close hint).
    /// Caller drops the overlay without touching the live keymap.
    Cancelled,
    /// User activated the Save button.  Carries the draft keymap and
    /// overrides, ready to be installed into the app and persisted to
    /// `keybindings.toml`.
    Save {
        keymap: KeyMap,
        overrides: KeyBindingOverrides,
    },
}

/// One row in the overlay.  `Header` rows are display-only — the
/// focus skips over them; `Binding` rows are editable.
#[derive(Debug, Clone)]
enum Row {
    Header(&'static str),
    Binding { action: Action, label: &'static str },
}

/// Which focus group currently receives keystrokes.  The Save/Cancel
/// buttons sit outside the scrollable list and are reachable via Tab
/// or by Down-arrowing past the last binding row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusArea {
    List,
    Save,
    Cancel,
}

/// Mutable state for an open keybinds overlay.
pub struct KeybindsState {
    /// Index into [`Self::rows`].  May land on a `Header` after a
    /// rebuild; [`Self::clamp_focus`] re-snaps to the nearest
    /// `Binding` row.  Only meaningful when [`Self::focus_area`] is
    /// `List`, but kept across area transitions so Tab-back lands on
    /// the user's last row.
    pub focused: usize,
    /// Which focus group is active: the list, the Save button, or
    /// the Cancel button.
    pub focus_area: FocusArea,
    /// `true` while the focused row is in chord-capture mode: the next
    /// non-modifier key press becomes the new binding in the draft.
    pub capturing: bool,
    /// Last error message produced by an invalid value.  Cleared on
    /// the next successful edit, cancel, or focus move.
    pub last_error: Option<String>,
    /// Vertical scroll bookkeeping for the row table.  Up/Down move
    /// `focused` and pull the viewport via `ensure_visible`; PgUp/PgDn
    /// and the mouse wheel drive `scroll_state.scroll` directly without
    /// touching focus.
    pub scroll_state: ScrollContainerState,
    /// Absolute terminal rect of the rendered `esc` close hint.
    pub esc_button_rect: Option<Rect>,
    /// Cached terminal rect of the `[ Cancel ]` button, populated each
    /// render and consumed by [`Self::handle_click`].
    pub cancel_button_rect: Option<Rect>,
    /// Cached terminal rect of the `[ Save ]` button, populated each
    /// render and consumed by [`Self::handle_click`].
    pub save_button_rect: Option<Rect>,
    /// Per-binding-row hit rects in absolute terminal coords for the
    /// currently visible portion of the list.  Each tuple is `(row index
    /// into [`Self::rows`], rect)`; only `Binding` rows are recorded.
    /// Rebuilt on every render so scroll / resize stay accurate.
    pub row_hit_rects: Vec<(usize, Rect)>,
    /// Draft keymap — starts as a clone of the live keymap and is
    /// mutated by every rebind.  Returned to the caller on Save and
    /// discarded on Cancel.
    pub draft_keymap: KeyMap,
    /// Draft overrides matching [`Self::draft_keymap`].
    pub draft_overrides: KeyBindingOverrides,
    /// All rows, including category headers.  Built once at
    /// construction time from the static `CATEGORIES` table; cheap to
    /// clone for tests.
    rows: Vec<Row>,
    /// For each `rows[i]`, the body-line index where that row renders.
    /// Pre-computed at construction time — the row list is static once
    /// the overlay is open, so the offsets never change.
    focus_offsets: Vec<usize>,
}

impl KeybindsState {
    /// Construct the overlay state with a draft cloned from the live
    /// keymap and overrides.  Mutations stay in the draft until the
    /// user saves.
    pub fn open(keymap: &KeyMap, overrides: &KeyBindingOverrides) -> Self {
        let rows = build_rows();
        let focus_offsets = compute_focus_offsets(&rows);
        let mut state = Self {
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

    /// The action of the currently focused row, if any.  Used by the
    /// inline unit tests below to query the row layout without exposing
    /// the internal `Row` enum.  `#[allow(dead_code)]` is required
    /// because the only callers are in `#[cfg(test)]` blocks, which the
    /// dead-code lint does not see on a non-test compile.
    #[allow(dead_code)]
    pub fn focused_action(&self) -> Option<Action> {
        match self.rows.get(self.focused) {
            Some(Row::Binding { action, .. }) => Some(action.clone()),
            _ => None,
        }
    }

    /// Move `focused` to the row whose `Binding.action == target`.
    /// Returns true on success.  Used by tests in this module and by
    /// the `tests/palette.rs` integration tests.  Marked
    /// `#[allow(dead_code)]` because integration tests live in a
    /// separate crate that the dead-code lint cannot see — without the
    /// attribute the lib compile errors under `-D warnings`.
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

    /// Apply a key event.  In capture mode, the next non-modifier key
    /// press becomes the new chord in the draft keymap.
    pub fn handle_key(&mut self, key: &KeyEvent) -> KeybindsResponse {
        if self.capturing {
            return self.handle_capture_key(key);
        }

        // PgUp/PgDn/Home/End move the viewport without touching focus.
        if self.scroll_state.handle_paging_key(key) {
            return KeybindsResponse::Continue;
        }

        // Tab / Shift-Tab cycle across focus groups.  Allow these even
        // when focus is in the list so the user can reach the Save /
        // Cancel buttons without arrow-navigating to the bottom row.
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
        // Bare Esc cancels capture within the overlay; binding Esc
        // itself requires hand-editing `keybindings.toml`.  The
        // `modifiers == NONE` guard is intentional: Esc-with-modifiers
        // (e.g. `Shift+Esc`, `Ctrl+Esc`) is a perfectly valid chord and
        // should fall through to the rebind path below rather than
        // exiting capture.
        if key.code == KeyCode::Esc && key.modifiers == KeyModifiers::NONE {
            self.capturing = false;
            self.last_error = None;
            return KeybindsResponse::Continue;
        }
        // Ignore bare modifier presses (Ctrl/Shift/Alt held alone) so
        // the user can naturally hold a modifier and then press the
        // actual key.
        if is_bare_modifier(key) {
            return KeybindsResponse::Continue;
        }
        let action = match self.rows.get(self.focused) {
            Some(Row::Binding { action, .. }) => action.clone(),
            _ => return KeybindsResponse::Continue,
        };
        // Build the `parse_key`-compatible form directly.  Going via
        // `format_key` + `replace('-', '+')` would mangle keys whose
        // own glyph is `-` or `+` (`"-"` → `"+"` → UnparseableKey).
        // `None` means the key has no parseable spelling (e.g. media
        // keys, lock keys) — surface that rather than writing an
        // un-parseable string into the overrides.
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
                // Display the chord in human-readable form (`Ctrl-Q`)
                // rather than the normalised `ctrl+q` carried by the
                // error, so the message matches the rest of the UI.
                let display_key = format_key(key);
                self.last_error =
                    Some(format!("'{display_key}' is already bound to {existing_action}"));
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
                    // Already on the last binding — Down crosses into
                    // the Cancel button (the leading/leftmost footer
                    // button) so the user can reach the buttons without
                    // hunting for Tab.
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
                // Return focus to the last list row the user was on.
                self.focus_area = FocusArea::List;
                self.last_error = None;
                KeybindsResponse::Continue
            }
            KeyCode::Down => KeybindsResponse::Continue,
            KeyCode::Left | KeyCode::Right => {
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

    /// Cycle Tab focus: List → Cancel → Save → List.  Order matches the
    /// visual button order (Cancel left, Save right).
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

    /// Emit Save.  Hands the draft keymap + overrides back to the
    /// caller, which is responsible for installing them on the app and
    /// writing `keybindings.toml`.
    fn save(&mut self) -> KeybindsResponse {
        KeybindsResponse::Save {
            keymap: self.draft_keymap.clone(),
            overrides: self.draft_overrides.clone(),
        }
    }

    fn cancel(&mut self) -> KeybindsResponse {
        KeybindsResponse::Cancelled
    }

    /// Step the focus by `delta` rows, skipping over `Header` rows.
    /// Returns `true` if focus moved, `false` if it would have run off
    /// either end — letting the caller decide whether to cross into
    /// the button row.
    fn move_focus(&mut self, delta: i32) -> bool {
        if let Some(idx) = next_focusable(&self.rows, self.focused, delta, |r| {
            matches!(r, Row::Binding { .. })
        }) {
            self.focused = idx;
            // Navigating away from a row drops any sticky conflict
            // message that was tied to it.
            self.last_error = None;
            // ensure_visible operates on body-line coords (headers
            // and blank separators inflate the body past the row
            // count), so translate via the pre-computed focus_offsets.
            let body_row = self.focus_offsets.get(self.focused).copied().unwrap_or(0) as u16;
            self.scroll_state.ensure_visible(body_row);
            true
        } else {
            false
        }
    }

    /// Apply a left-button click at terminal coords `(col, row)`.
    ///
    /// During capture mode every click except the `esc` close hint is
    /// ignored — the user must complete or cancel the in-flight chord
    /// before mousing elsewhere.  Outside capture mode:
    /// - clicking the `esc` hint or the Cancel button discards the
    ///   draft;
    /// - clicking the Save button hands the draft back to the caller;
    /// - clicking a binding row focuses it and arms capture immediately
    ///   (so the next keystroke becomes the new chord).
    pub fn handle_click(&mut self, col: u16, row: u16) -> KeybindsResponse {
        if self.capturing {
            if rect_contains(self.esc_button_rect, col, row) {
                // Match the Esc-key behaviour: only cancel capture, not
                // the whole overlay.
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
        // Build all body lines first.  Headers introduce a blank
        // separator above themselves (except at the top), so the
        // expanded line count is greater than `state.rows.len()` and
        // must be computed up-front for accurate scroll bookkeeping.
        let body_lines = build_body_lines(state, &state.draft_keymap, self.theme);

        let content_width = keybinds_content_width(state);

        // Pinned-bottom footer layout:
        //   1 spacer + 1 buttons     (always)
        //   + 1 capture hint         (when capturing)
        //   + 1 error                (when last_error present)
        let extra_status = (state.capturing as u16) + (state.last_error.is_some() as u16);
        let pinned_bottom: u16 = 2 + extra_status;

        let content = ContentSize {
            width: content_width,
            height: body_lines.len() as u16,
            pinned_top: 0,
            pinned_bottom,
        };
        let rect = centered_rect_for_content(content, area);

        let inner_h = rect.height.saturating_sub(VERTICAL_CHROME_ROWS);
        let table_height = inner_h.saturating_sub(pinned_bottom);
        state
            .scroll_state
            .observe(body_lines.len() as u16, table_height);
        // NB: do NOT call ensure_visible here.  Doing so would snap
        // scroll back to the focused row on every redraw, undoing
        // wheel/PgUp/PgDn scrolls that intentionally moved the
        // viewport without changing focus.  ensure_visible runs only
        // when focus actually moves (see KeybindsState::move_focus).

        let layout = draw_frame(
            rect,
            buf,
            FrameOpts {
                title: "Keybindings",
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

        // Pinned footer: capture hint, error, spacer, buttons.
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
        // Spacer (always reserved).
        footer_y += 1;
        let button_area = Rect {
            x: inner.x,
            y: footer_y,
            width: inner.width,
            height: 1,
        };
        // Button order on screen matches BUTTON_LABELS: Cancel (idx 0)
        // on the left, Save (idx 1) on the right.
        let focused_idx = match state.focus_area {
            FocusArea::Cancel => 0,
            FocusArea::Save => 1,
            FocusArea::List => usize::MAX,
        };
        let button_rects =
            render_button_row(button_area, buf, BUTTON_LABELS, focused_idx, self.theme);
        // BUTTON_LABELS is [Cancel, Save]; render_button_row returns
        // rects in the same order.
        state.cancel_button_rect = button_rects.first().copied();
        state.save_button_rect = button_rects.get(1).copied();

        // Record hit rects for visible binding rows so a click on a
        // chord can focus + arm capture without re-deriving the layout.
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
                let focused = idx == state.focused && state.focus_area == FocusArea::List;
                let capturing = focused && state.capturing;
                // The capture prompt lives in the pinned footer; the
                // chord cell shows `…` so the row still has a visible
                // affordance while the user picks a chord.
                let chord = if capturing {
                    "…".to_owned()
                } else {
                    keymap.first_key_for(action).unwrap_or_default()
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

/// For each `rows[i]`, the body-line index where that row renders.
/// Computed once at construction; used by `ensure_visible` to translate
/// focused-row index into the body coords the scroll state operates in.
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

/// Content-aware width: max over rows of `marker(2) + label_pad +
/// chord_w`, plus the longest header (`— Title —`), the capture hint,
/// the button row, and the longest error.  Sized over the whole row
/// set so width doesn't jiggle as focus moves.
fn keybinds_content_width(state: &KeybindsState) -> u16 {
    const FOCUS_MARKER_WIDTH: usize = 2;
    let row_max = max_row_width(&state.rows, |r| match r {
        Row::Header(t) => t.chars().count() + 4, // "— x —"
        Row::Binding { action, .. } => {
            let chord_w = state
                .draft_keymap
                .first_key_for(action)
                .map(|s| s.chars().count())
                .unwrap_or(0);
            FOCUS_MARKER_WIDTH + LABEL_PAD + chord_w
        }
    });
    let err_max = optional_text_width(state.last_error.as_deref(), 2);
    let hint_w = CAPTURE_HINT.chars().count() as u16;
    let buttons_w = button_row_width(BUTTON_LABELS);
    row_max.max(err_max).max(hint_w).max(buttons_w)
}

/// Detect a key event that is *only* a modifier key being held down
/// (no real key yet).  With crossterm's keyboard-enhancement enabled,
/// `KeyCode::Modifier(_)` events are emitted when Ctrl/Shift/Alt are
/// pressed in isolation; we swallow these so the user can naturally
/// hold a modifier and then press the actual key.
fn is_bare_modifier(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Modifier(_))
}

/// Point-in-rect test for an optional cached hit rect.  `None` always
/// misses so callers can pass an unpopulated rect (e.g. before the
/// first render) without a separate guard.
fn rect_contains(rect: Option<Rect>, col: u16, row: u16) -> bool {
    match rect {
        Some(r) => col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height,
        None => false,
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
        KeybindsState::open(&keymap(), &KeyBindingOverrides::default())
    }

    #[test]
    fn initial_focus_is_first_binding_not_a_header() {
        let state = open();
        // The initial row is "Save file" under the Editor header.
        assert_eq!(state.focused_action(), Some(Action::Save));
        assert_eq!(state.focus_area, FocusArea::List);
    }

    #[test]
    fn down_skips_over_header_rows() {
        // Walks Down through the entire list and asserts every focus
        // transition that crosses a category boundary lands on the
        // first Binding row of the next category (not on the Header
        // itself).  This holds for *every* boundary regardless of the
        // specific actions in each category, so reordering CATEGORIES
        // can't make the test pass for the wrong reason.
        let mut state = open();
        let rows = state.rows.clone();
        let starting_header = current_header(&rows, state.focused);
        assert!(
            starting_header.is_some(),
            "initial focus must be inside a category"
        );
        // Build the expected sequence of (header, first action) pairs
        // from the row table itself.
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
        // Cap the walk — far more steps than any plausible row count.
        for _ in 0..rows.len() + 8 {
            let before = state.focused;
            state.handle_key(&key(KeyCode::Down));
            if state.focused == before {
                // Hit the bottom of the list.
                break;
            }
            let now = current_header(&rows, state.focused);
            if now != prev_header {
                let action = state.focused_action().unwrap();
                crossings.push((now.unwrap(), action));
                prev_header = now;
            }
        }
        // Skip the first entry of `category_starts` — that's the user's
        // starting category, not a crossing.
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

    /// Walk backward through `rows` from `idx` and return the nearest
    /// preceding `Header` title.  Used by the boundary-crossing test
    /// instead of hard-coding the layout.
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
        // Regression for review issue #1: format_key emits `PgUp` /
        // `PgDn` for KeyCode::PageUp/PageDown; parse_key must accept
        // those back so capture doesn't surface an UnparseableKey
        // error.  Bare PgUp would conflict with ScrollPageUp's default
        // binding, so capture Shift+PgUp instead — same code path,
        // different chord.
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
        // Media keys (and other KeyCode variants without a parseable
        // spelling) must NOT be written into the overrides as the
        // Debug-stringified form — they have no round-trip and would
        // surface as UnparseableKey on next load.  The capture handler
        // should reject them with an "Unsupported key" error and stay
        // in capture mode so the user can try a different chord.
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
        // Save's binding is untouched in the draft.
        assert_eq!(
            state.draft_keymap.first_key_for(&Action::Save).as_deref(),
            Some("Ctrl-S")
        );
    }

    #[test]
    fn hyphen_and_plus_keys_are_capturable() {
        // Regression: capturing `-` or `+` used to mangle the chord
        // because the old normalisation went through `format_key`'s
        // dash-separated form (`replace('-', '+')` turned `"-"` into
        // `"+"` and then UnparseableKey).  Both must round-trip cleanly
        // through capture → rebind → format_key.
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
        // Regression: the conflict error used to surface the
        // `parse_key`-normalised form (`ctrl+q`) carried by the error
        // value.  It should match the rest of the UI (`Ctrl-Q`).
        let mut state = open();
        assert!(state.focus_action(&Action::Save));
        state.handle_key(&key(KeyCode::Enter));
        state.handle_key(&KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        let msg = state.last_error.as_deref().expect("conflict surfaces error");
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
        // Walk to the very last binding row.
        loop {
            let before = state.focused;
            state.handle_key(&key(KeyCode::Down));
            if state.focused == before {
                break;
            }
        }
        // One more Down crosses into the button row — Cancel first.
        state.handle_key(&key(KeyCode::Down));
        assert_eq!(state.focus_area, FocusArea::Cancel);
    }

    #[test]
    fn enter_on_save_button_emits_save_response() {
        let mut state = open();
        // Make one change so the test verifies the drafts come back.
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
        // Sanity: draft has the rebind.
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
        // A rebind in the draft must NOT survive Esc.
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
    fn up_from_button_returns_focus_to_list() {
        let mut state = open();
        state.focus_area = FocusArea::Save;
        state.handle_key(&key(KeyCode::Up));
        assert_eq!(state.focus_area, FocusArea::List);
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
        // Regression: ensure_visible used to run on every render and
        // would snap scroll back to the focused row, so PgDn / mouse
        // wheel could not move the viewport past the focused binding.
        let mut state = open();
        render(&mut state, 80, 18);
        state.handle_key(&key(KeyCode::PageDown));
        let scroll_after_pgdn = state.scroll_state.scroll;
        assert!(scroll_after_pgdn > 0, "PgDn must advance scroll");
        // The next render must NOT undo the scroll.
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
        // Regression: same root cause as the PgDn test — wheel scroll
        // must persist across renders.
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
        // Find the cached rect for Action::Copy and click inside it.
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
        // A click on the Save button mid-capture must NOT save.
        let save_rect = state.save_button_rect.expect("Save rect populated");
        let resp = state.handle_click(save_rect.x + 2, save_rect.y);
        assert!(matches!(resp, KeybindsResponse::Continue));
        assert!(state.capturing, "non-esc clicks must not exit capture");
        // A click on the Esc hint mid-capture cancels capture only.
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
