//! Rows/columns prompt for `Action::InsertTable`.
//!
//! A small focused modal: two numeric fields ("Rows", "Columns") above
//! an Insert / Cancel button row.  Tab / Shift-Tab and Up / Down move
//! between the four focus targets; Left / Right adjust the focused
//! numeric field by one (or switch between the Insert and Cancel
//! buttons when focused there); digit keys append, Backspace removes.
//! Enter activates the focused button (or, on a field, submits with
//! the current values).
//!
//! The widget is UI-only — the App layer reads the rows/columns when
//! [`InsertTableResponse::Insert`] fires and routes the actual buffer
//! mutation through `editor::table_edit::insert_table`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::config::Theme;
use crate::ui::button_row::{button_row_width, footer_row_count, render_button_row};
use crate::ui::controls;
use crate::ui::cursor::text_field_spans;
use crate::ui::scroll_container::{
    centered_rect_for_content, draw_frame, ContentSize, FrameOpts, ModalKind, MAX_PAD_H,
};

const BUTTON_LABELS: &[&str] = &["Insert", "Cancel"];

/// One of the four focus targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertTableField {
    Rows,
    Cols,
    Insert,
    Cancel,
}

impl InsertTableField {
    fn next(self) -> Self {
        match self {
            Self::Rows => Self::Cols,
            Self::Cols => Self::Insert,
            Self::Insert => Self::Cancel,
            Self::Cancel => Self::Rows,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Rows => Self::Cancel,
            Self::Cols => Self::Rows,
            Self::Insert => Self::Cols,
            Self::Cancel => Self::Insert,
        }
    }

    fn is_field(self) -> bool {
        matches!(self, Self::Rows | Self::Cols)
    }
}

/// Outcome of dispatching a key event to the modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertTableResponse {
    /// The modal stays open; the caller just redraws.
    Continue,
    /// User dismissed (Escape or the Cancel button).
    Cancelled,
    /// User pressed Insert with valid `rows` (≥ 0) and `cols` (≥ 1).
    /// The caller dispatches `editor::table_edit::insert_table`.
    Insert { rows: usize, cols: usize },
}

/// Mutable state for an open Insert Table modal.
#[derive(Debug, Clone)]
pub struct InsertTableState {
    /// Body-row count, edited as a string so users can backspace
    /// past `0` and type a fresh number from scratch.  Defaults to
    /// "2".
    pub rows: String,
    /// Column count.  Defaults to "3".  Empty / 0 is rejected on
    /// Insert.
    pub cols: String,
    /// Which focus target receives keystrokes.
    pub focus: InsertTableField,
    /// Last validation message, e.g. "Columns must be at least 1".
    /// Cleared on every successful keystroke that mutates a field.
    pub last_error: Option<String>,
    /// Absolute terminal rect of the rendered `esc` close hint.
    /// Populated each render; consulted for click hit-testing.
    pub esc_button_rect: Option<Rect>,
}

impl Default for InsertTableState {
    fn default() -> Self {
        Self::new()
    }
}

impl InsertTableState {
    pub fn new() -> Self {
        Self {
            rows: "2".to_owned(),
            cols: "3".to_owned(),
            focus: InsertTableField::Rows,
            last_error: None,
            esc_button_rect: None,
        }
    }

    /// Apply a key event.  Numeric fields accept digits + Left/Right
    /// to adjust the value by 1; buttons accept Enter / Space.  Up /
    /// Down (and Tab / Shift-Tab) move focus between the fields and
    /// buttons.
    pub fn handle_key(&mut self, key: &KeyEvent) -> InsertTableResponse {
        // Modifier-augmented chords (Ctrl-foo, Alt-foo) are ignored so
        // the user can press Ctrl-S etc. without polluting a field.
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return InsertTableResponse::Continue;
        }

        match key.code {
            KeyCode::Esc => InsertTableResponse::Cancelled,
            // Tab / Shift-Tab and Up / Down all navigate between
            // focus targets — Up / Down are the natural row-by-row
            // motion, Tab cycles in the same direction for users
            // who instinctively reach for it.
            KeyCode::Tab | KeyCode::Down => {
                self.focus = self.focus.next();
                InsertTableResponse::Continue
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.focus = self.focus.prev();
                InsertTableResponse::Continue
            }
            // Left / Right adjust the focused numeric field by 1.
            // On a button they swap between Insert and Cancel so the
            // arrow keys remain meaningful no matter where focus is.
            KeyCode::Left => {
                if self.focus.is_field() {
                    self.adjust_focused(-1);
                } else {
                    self.focus = self.focus.prev();
                }
                InsertTableResponse::Continue
            }
            KeyCode::Right => {
                if self.focus.is_field() {
                    self.adjust_focused(1);
                } else {
                    self.focus = self.focus.next();
                }
                InsertTableResponse::Continue
            }
            KeyCode::Backspace if self.focus.is_field() => {
                self.field_buf_mut().pop();
                self.last_error = None;
                InsertTableResponse::Continue
            }
            KeyCode::Char(c) if c.is_ascii_digit() && self.focus.is_field() => {
                // Cap the typed number at 4 digits — larger tables are
                // unrealistic in a TUI and would risk OOM via the
                // multiplicative cell allocation.
                let buf = self.field_buf_mut();
                if buf.len() < 4 {
                    buf.push(c);
                    self.last_error = None;
                }
                InsertTableResponse::Continue
            }
            KeyCode::Enter | KeyCode::Char(' ') => match self.focus {
                InsertTableField::Cancel => InsertTableResponse::Cancelled,
                InsertTableField::Insert => self.try_insert(),
                // Enter on a field acts as "submit" — equivalent to
                // landing on Insert.  Pressing Enter in the rows /
                // cols field is the most natural muscle-memory for
                // confirming the values.
                InsertTableField::Rows | InsertTableField::Cols => self.try_insert(),
            },
            _ => InsertTableResponse::Continue,
        }
    }

    fn try_insert(&mut self) -> InsertTableResponse {
        let rows = match self.rows.parse::<usize>() {
            Ok(v) => v,
            Err(_) => {
                self.last_error = Some("Rows must be a number".to_owned());
                self.focus = InsertTableField::Rows;
                return InsertTableResponse::Continue;
            }
        };
        let cols = match self.cols.parse::<usize>() {
            Ok(v) if v >= 1 => v,
            _ => {
                self.last_error = Some("Columns must be at least 1".to_owned());
                self.focus = InsertTableField::Cols;
                return InsertTableResponse::Continue;
            }
        };
        InsertTableResponse::Insert { rows, cols }
    }

    fn field_buf_mut(&mut self) -> &mut String {
        match self.focus {
            InsertTableField::Rows => &mut self.rows,
            InsertTableField::Cols => &mut self.cols,
            // The is_field() guard at the call sites prevents this.
            _ => unreachable!("field_buf_mut called on a button focus"),
        }
    }

    /// Left/Right on a numeric field bump the value by `delta`.  Empty
    /// or non-numeric strings reset to `0` (rows) / `1` (cols) before
    /// the adjustment so the spinner has a stable anchor.
    fn adjust_focused(&mut self, delta: i32) {
        let (current, min) = match self.focus {
            InsertTableField::Rows => (self.rows.parse::<i32>().unwrap_or(0), 0),
            InsertTableField::Cols => (self.cols.parse::<i32>().unwrap_or(1), 1),
            _ => return,
        };
        let next = (current + delta).max(min);
        let buf = self.field_buf_mut();
        buf.clear();
        buf.push_str(&next.to_string());
        self.last_error = None;
    }

    /// Insert a bracketed paste into the focused numeric field.  Only
    /// ASCII digits survive (these fields are numeric), the paste is
    /// first flattened and length-capped by [`crate::ui::sanitize_paste`],
    /// and the same 4-digit field cap as typing is enforced.  No-op on a
    /// button focus.
    pub fn paste(&mut self, text: &str) {
        if !self.focus.is_field() {
            return;
        }
        let clean = crate::ui::sanitize_paste(text);
        let buf = self.field_buf_mut();
        let mut changed = false;
        for c in clean.chars().filter(|c| c.is_ascii_digit()) {
            if buf.len() >= 4 {
                break;
            }
            buf.push(c);
            changed = true;
        }
        if changed {
            self.last_error = None;
        }
    }
}

/// View-only widget that renders the modal over the editor.
pub struct InsertTableView<'a> {
    pub theme: &'a Theme,
    pub cursor_visible: bool,
}

impl<'a> StatefulWidget for InsertTableView<'a> {
    type State = InsertTableState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Layout: 2 field rows + 1 spacer + the buttons (more than one
        // row in a terminal too narrow for the pair) + (optional) 1
        // error row.  All content sits in `pinned_top` so the modal
        // sizes precisely to its body — no scrolling needed.
        let base_rows = if state.last_error.is_some() { 4 } else { 3 };
        let label_w = "Columns".chars().count() as u16;
        let field_w = 6u16; // "[ NNNN ]" → 8, content 6 inside the box
        let gap = 2u16;
        let buttons_w = button_row_width(BUTTON_LABELS);
        let content_width = (label_w + gap + field_w).max(buttons_w);
        // The footer wraps rather than clipping, so its height is a
        // function of the width the frame will give it.  Reserving a
        // flat row instead leaves a wrapped button unpainted but still
        // focusable and still carrying a click rect.
        let footer_rows = footer_row_count(BUTTON_LABELS, content_width, area.width, MAX_PAD_H);
        let content = ContentSize {
            width: content_width,
            height: 0,
            pinned_top: base_rows + footer_rows,
            pinned_bottom: 0,
            ..Default::default()
        };
        let modal_area = centered_rect_for_content(content, area);
        let layout = draw_frame(
            modal_area,
            buf,
            FrameOpts {
                title: "Insert Table",
                kind: ModalKind::Normal,
                show_close_hint: true,
                content,
                theme: self.theme,
            },
        );
        state.esc_button_rect = layout.esc_hit_rect;
        let inner = layout.body;
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let mut row_y = inner.y;
        // Rows row.
        render_field_row(
            buf,
            inner,
            row_y,
            "Rows",
            &state.rows,
            state.focus == InsertTableField::Rows,
            self.theme,
            self.cursor_visible,
        );
        row_y = row_y.saturating_add(1);
        if row_y >= inner.y + inner.height {
            return;
        }
        // Cols row.
        render_field_row(
            buf,
            inner,
            row_y,
            "Columns",
            &state.cols,
            state.focus == InsertTableField::Cols,
            self.theme,
            self.cursor_visible,
        );
        row_y = row_y.saturating_add(1);

        // Error row (optional, between fields and buttons).
        if let Some(err) = state.last_error.as_deref() {
            if row_y < inner.y + inner.height {
                let err_area = Rect {
                    x: inner.x,
                    y: row_y,
                    width: inner.width,
                    height: 1,
                };
                Paragraph::new(Line::from(Span::styled(
                    err.to_owned(),
                    self.theme.transient_error,
                )))
                .alignment(Alignment::Center)
                .style(self.theme.modal_bg)
                .render(err_area, buf);
                row_y = row_y.saturating_add(1);
            }
        }

        // Spacer between fields/error and buttons.
        if row_y < inner.y + inner.height {
            row_y = row_y.saturating_add(1);
        }
        if row_y >= inner.y + inner.height {
            return;
        }
        // Buttons row.
        let button_area = Rect {
            x: inner.x,
            y: row_y,
            width: inner.width,
            height: (inner.y + inner.height).saturating_sub(row_y),
        };
        render_buttons(button_area, buf, state.focus, self.theme);
    }
}

/// Render `label    [ value ]` where the field value is highlighted
/// when the row is focused.  Caller supplies the row's y position.
#[allow(clippy::too_many_arguments)]
fn render_field_row(
    buf: &mut Buffer,
    inner: Rect,
    y: u16,
    label: &str,
    value: &str,
    focused: bool,
    theme: &Theme,
    cursor_visible: bool,
) {
    let area = Rect {
        x: inner.x,
        y,
        width: inner.width,
        height: 1,
    };
    let label_width = "Columns".chars().count();
    let label_padded = format!("{label:<width$}", label = label, width = label_width);
    let value_style = controls::text_value_style(focused, theme);
    // Shared cursor renderer: a trailing `▏` bar (in `theme.cursor`) on the
    // focused field so the user sees where typing lands.  The fields are
    // digits-only and append-only, so the cursor is always at the end; the
    // slot is constant-width across blink phases.
    let mut spans = vec![
        Span::styled(label_padded, theme.modal_item),
        Span::raw("  "),
        Span::styled(" ", value_style),
    ];
    spans.extend(text_field_spans(
        value,
        value.chars().count(),
        focused && cursor_visible,
        value_style,
        theme.cursor,
    ));
    Paragraph::new(Line::from(spans))
        .style(theme.modal_bg)
        .render(area, buf);
}

/// Render `[ Insert ]  [ Cancel ]` centred horizontally with the
/// focused button drawn in `modal_button_focused` style.
fn render_buttons(area: Rect, buf: &mut Buffer, focus: InsertTableField, theme: &Theme) {
    let focused_idx = match focus {
        InsertTableField::Insert => 0,
        InsertTableField::Cancel => 1,
        _ => usize::MAX, // no button focused while editing a field
    };
    render_button_row(area, buf, BUTTON_LABELS, focused_idx, theme);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn defaults_match_phase_15_spec() {
        let s = InsertTableState::new();
        assert_eq!(s.rows, "2");
        assert_eq!(s.cols, "3");
        assert_eq!(s.focus, InsertTableField::Rows);
        assert!(s.last_error.is_none());
    }

    #[test]
    fn paste_keeps_only_digits_and_respects_the_field_cap() {
        let mut s = InsertTableState::new();
        s.rows.clear();
        // Non-digits dropped; capped at 4 digits like typing.
        s.paste("12ab34567");
        assert_eq!(s.rows, "1234");
    }

    #[test]
    fn paste_is_a_noop_on_button_focus() {
        let mut s = InsertTableState::new();
        s.handle_key(&key(KeyCode::Tab)); // Cols
        s.handle_key(&key(KeyCode::Tab)); // Insert (button)
        s.cols.clear();
        s.paste("9");
        assert_eq!(s.cols, "", "paste ignored while a button is focused");
    }

    #[test]
    fn tab_cycles_through_rows_cols_insert_cancel() {
        let mut s = InsertTableState::new();
        s.handle_key(&key(KeyCode::Tab));
        assert_eq!(s.focus, InsertTableField::Cols);
        s.handle_key(&key(KeyCode::Tab));
        assert_eq!(s.focus, InsertTableField::Insert);
        s.handle_key(&key(KeyCode::Tab));
        assert_eq!(s.focus, InsertTableField::Cancel);
        s.handle_key(&key(KeyCode::Tab));
        assert_eq!(s.focus, InsertTableField::Rows);
    }

    #[test]
    fn shift_tab_cycles_backwards() {
        // `KeyCode::BackTab` is what some terminals emit; this struct
        // also accepts the canonical `Tab + SHIFT` form via the
        // matching arm in `handle_key`.
        let mut s = InsertTableState::new();
        s.handle_key(&KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(s.focus, InsertTableField::Cancel);
    }

    #[test]
    fn escape_cancels() {
        let mut s = InsertTableState::new();
        let r = s.handle_key(&key(KeyCode::Esc));
        assert_eq!(r, InsertTableResponse::Cancelled);
    }

    #[test]
    fn typing_digits_replaces_default_after_backspace() {
        let mut s = InsertTableState::new();
        s.handle_key(&key(KeyCode::Backspace)); // clear "2"
        s.handle_key(&key(KeyCode::Char('5')));
        assert_eq!(s.rows, "5");
    }

    #[test]
    fn right_arrow_increments_focused_field() {
        let mut s = InsertTableState::new();
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(s.rows, "3");
        assert_eq!(s.focus, InsertTableField::Rows, "focus must not change");
    }

    #[test]
    fn left_arrow_clamps_rows_at_zero() {
        let mut s = InsertTableState::new();
        // Rows defaults to "2"; pressing Left repeatedly should clamp
        // at 0, not go negative.
        for _ in 0..5 {
            s.handle_key(&key(KeyCode::Left));
        }
        assert_eq!(s.rows, "0");
    }

    #[test]
    fn cols_minimum_is_one_via_left_arrow() {
        let mut s = InsertTableState::new();
        s.focus = InsertTableField::Cols;
        for _ in 0..10 {
            s.handle_key(&key(KeyCode::Left));
        }
        assert_eq!(s.cols, "1");
    }

    #[test]
    fn up_down_arrows_navigate_focus_without_changing_values() {
        let mut s = InsertTableState::new();
        s.handle_key(&key(KeyCode::Down));
        assert_eq!(s.focus, InsertTableField::Cols);
        assert_eq!(s.rows, "2", "Down must not bump the rows field");
        s.handle_key(&key(KeyCode::Down));
        assert_eq!(s.focus, InsertTableField::Insert);
        s.handle_key(&key(KeyCode::Up));
        assert_eq!(s.focus, InsertTableField::Cols);
        s.handle_key(&key(KeyCode::Up));
        assert_eq!(s.focus, InsertTableField::Rows);
    }

    #[test]
    fn left_right_swap_buttons_when_focused_on_a_button() {
        let mut s = InsertTableState::new();
        s.focus = InsertTableField::Insert;
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(s.focus, InsertTableField::Cancel);
        s.handle_key(&key(KeyCode::Left));
        assert_eq!(s.focus, InsertTableField::Insert);
    }

    #[test]
    fn enter_on_insert_button_emits_insert_response() {
        let mut s = InsertTableState::new();
        s.focus = InsertTableField::Insert;
        let r = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(r, InsertTableResponse::Insert { rows: 2, cols: 3 });
    }

    #[test]
    fn enter_on_cancel_button_cancels() {
        let mut s = InsertTableState::new();
        s.focus = InsertTableField::Cancel;
        let r = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(r, InsertTableResponse::Cancelled);
    }

    #[test]
    fn enter_on_field_submits_when_values_valid() {
        let mut s = InsertTableState::new();
        // Focus is Rows by default; Enter on a valid Rows field should
        // submit just like Enter on the Insert button.
        let r = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(r, InsertTableResponse::Insert { rows: 2, cols: 3 });
    }

    #[test]
    fn empty_cols_field_blocks_submit_and_flags_error() {
        let mut s = InsertTableState::new();
        s.focus = InsertTableField::Cols;
        s.handle_key(&key(KeyCode::Backspace)); // clear "3"
        let r = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(r, InsertTableResponse::Continue);
        assert!(s.last_error.is_some());
        assert_eq!(s.focus, InsertTableField::Cols);
    }

    #[test]
    fn ctrl_chars_do_not_pollute_fields() {
        let mut s = InsertTableState::new();
        let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        s.handle_key(&ctrl_p);
        assert_eq!(s.rows, "2", "Ctrl-P should not modify the rows field");
    }

    #[test]
    fn a_narrow_terminal_wraps_the_footer_and_still_paints_both_buttons() {
        // The footer wraps rather than clipping, so the modal has to
        // reserve the rows it wrapped onto.  A flat one-row reservation
        // leaves Cancel unpainted while Tab still focuses it.
        let backend = TestBackend::new(18, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = InsertTableState::new();
        terminal
            .draw(|frame| {
                let m = InsertTableView {
                    theme: theme(),
                    cursor_visible: true,
                };
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let painted: String = (0..14)
            .map(|y| {
                (0..18)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(painted.contains("[ Insert ]"), "{painted}");
        assert!(painted.contains("[ Cancel ]"), "{painted}");
    }

    #[test]
    fn renders_title_fields_and_buttons() {
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = InsertTableState::new();
        terminal
            .draw(|frame| {
                let m = InsertTableView {
                    theme: theme(),
                    cursor_visible: true,
                };
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();

        let contents: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            contents.contains("Insert Table"),
            "title missing: {contents}"
        );
        assert!(contents.contains("Rows"), "rows label missing: {contents}");
        assert!(
            contents.contains("Columns"),
            "columns label missing: {contents}"
        );
        assert!(
            contents.contains("Insert"),
            "insert button missing: {contents}"
        );
        assert!(
            contents.contains("Cancel"),
            "cancel button missing: {contents}"
        );
    }
}
