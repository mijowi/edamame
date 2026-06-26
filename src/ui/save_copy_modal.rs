//! Shared path-entry widget ([`SaveCopyState`] + [`SaveCopyView`]) used by
//! the path-input modals — Save As, the file-deleted recovery prompt, and
//! the dirty-conflict "save aside" flow.  Each modal supplies its own frame
//! title and decides what the entered path does on submit.
//!
//! A single text field ("Path") above a Save / Cancel button row.  Tab
//! / Shift-Tab and Up / Down move between the three focus targets;
//! while focus is on the field, character keys insert at the cursor,
//! Left / Right move the cursor through the text, Home / End jump to
//! the ends, Backspace / Delete remove characters around the cursor,
//! and Enter submits.  Left / Right switch between buttons when focus
//! is on a button.
//!
//! The widget is UI-only: the App layer reads the entered path when
//! [`SaveCopyResponse::Save`] fires and decides whether to re-point the
//! buffer (Save As) or write a detached copy.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::config::Theme;
use crate::ui::button_row::{button_row_width, render_button_row};
use crate::ui::controls;
use crate::ui::cursor::text_field_spans;
use crate::ui::scroll_container::{
    centered_rect_for_content, draw_frame, ContentSize, FrameOpts, ModalKind,
};

const BUTTON_LABELS: &[&str] = &["Save", "Cancel"];

/// One of the three focus targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveCopyField {
    Path,
    Save,
    Cancel,
}

impl SaveCopyField {
    fn next(self) -> Self {
        match self {
            Self::Path => Self::Save,
            Self::Save => Self::Cancel,
            Self::Cancel => Self::Path,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Path => Self::Cancel,
            Self::Save => Self::Path,
            Self::Cancel => Self::Save,
        }
    }

    fn is_path(self) -> bool {
        matches!(self, Self::Path)
    }
}

/// Outcome of dispatching a key event to the modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveCopyResponse {
    /// Modal stays open; the caller just redraws.
    Continue,
    /// User dismissed (Escape or the Cancel button).
    Cancelled,
    /// User pressed Save with a non-empty path.  The caller writes the
    /// buffer to this path via `Buffer::save_copy`.
    Save(String),
}

/// Mutable state for an open Save Copy modal.
#[derive(Debug, Clone)]
pub struct SaveCopyState {
    /// The path the user is editing.  Seeded by the App with a sensible
    /// default derived from the current buffer's filename
    /// (see [`default_save_as_path`]).
    pub path: String,
    /// Cursor position into [`Self::path`] expressed as a Unicode-scalar
    /// (char) index, so paths containing multi-byte characters behave
    /// the way the user expects when navigating with Left / Right.
    /// Initialized to the end of `path` so the user can immediately
    /// backspace / type to rename without first jumping past the
    /// pre-filled default.
    pub cursor: usize,
    /// Which focus target receives keystrokes.
    pub focus: SaveCopyField,
    /// Last validation message, e.g. "Path required".  Cleared when the
    /// user mutates the field.
    pub last_error: Option<String>,
    /// Absolute terminal rect of the rendered `esc` close hint.
    pub esc_button_rect: Option<Rect>,
}

impl SaveCopyState {
    pub fn new(default_path: String) -> Self {
        let cursor = default_path.chars().count();
        Self {
            path: default_path,
            cursor,
            focus: SaveCopyField::Path,
            last_error: None,
            esc_button_rect: None,
        }
    }

    /// Apply a key event.  When focus is on the path field: characters
    /// insert at the cursor, Left / Right / Home / End move the cursor,
    /// Backspace / Delete remove characters.  Tab / Shift-Tab / Up /
    /// Down cycle focus; Enter submits.
    pub fn handle_key(&mut self, key: &KeyEvent) -> SaveCopyResponse {
        // Modifier-augmented chords (Ctrl-foo, Alt-foo) are ignored so
        // chords don't pollute the path field.
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return SaveCopyResponse::Continue;
        }

        match key.code {
            KeyCode::Esc => return SaveCopyResponse::Cancelled,
            KeyCode::Tab | KeyCode::Down => self.focus = self.focus.next(),
            KeyCode::BackTab | KeyCode::Up => self.focus = self.focus.prev(),
            // On the path field, Left / Right move the in-field cursor.
            // On a button, they swap between Save and Cancel so the
            // arrow keys remain useful no matter where focus sits.
            KeyCode::Left => {
                if self.focus.is_path() {
                    self.cursor = self.cursor.saturating_sub(1);
                } else {
                    self.focus = self.focus.prev();
                }
            }
            KeyCode::Right => {
                if self.focus.is_path() {
                    let len = self.path.chars().count();
                    if self.cursor < len {
                        self.cursor += 1;
                    }
                } else {
                    self.focus = self.focus.next();
                }
            }
            KeyCode::Home if self.focus.is_path() => {
                self.cursor = 0;
            }
            KeyCode::End if self.focus.is_path() => {
                self.cursor = self.path.chars().count();
            }
            KeyCode::Backspace if self.focus.is_path() => {
                if self.cursor > 0 {
                    let target = self.cursor - 1;
                    remove_char_at(&mut self.path, target);
                    self.cursor = target;
                    self.last_error = None;
                }
            }
            KeyCode::Delete if self.focus.is_path() => {
                if self.cursor < self.path.chars().count() {
                    remove_char_at(&mut self.path, self.cursor);
                    self.last_error = None;
                }
            }
            KeyCode::Char(c) if self.focus.is_path() => {
                insert_char_at(&mut self.path, self.cursor, c);
                self.cursor += 1;
                self.last_error = None;
            }
            KeyCode::Enter => {
                return match self.focus {
                    SaveCopyField::Cancel => SaveCopyResponse::Cancelled,
                    SaveCopyField::Save | SaveCopyField::Path => self.try_save(),
                };
            }
            // Space activates a focused button (mirrors the
            // InsertTable modal).  On the path field, Space falls
            // through to the `Char` arm above and inserts a literal
            // space — paths with spaces are valid.
            KeyCode::Char(' ') if !self.focus.is_path() => {
                return match self.focus {
                    SaveCopyField::Cancel => SaveCopyResponse::Cancelled,
                    SaveCopyField::Save => self.try_save(),
                    SaveCopyField::Path => unreachable!(),
                };
            }
            _ => {}
        }
        SaveCopyResponse::Continue
    }

    /// Insert a bracketed paste into the path field at the cursor.
    /// No-op when focus is on a button.  The paste is flattened to one
    /// line and length-capped by [`crate::ui::sanitize_paste`].
    pub fn paste(&mut self, text: &str) {
        if !self.focus.is_path() {
            return;
        }
        let clean = crate::ui::sanitize_paste(text);
        if clean.is_empty() {
            return;
        }
        for c in clean.chars() {
            insert_char_at(&mut self.path, self.cursor, c);
            self.cursor += 1;
        }
        self.last_error = None;
    }

    fn try_save(&mut self) -> SaveCopyResponse {
        let trimmed = self.path.trim();
        if trimmed.is_empty() {
            self.last_error = Some("Path required".to_owned());
            self.focus = SaveCopyField::Path;
            return SaveCopyResponse::Continue;
        }
        SaveCopyResponse::Save(trimmed.to_owned())
    }
}

/// Insert `ch` at char-index `cursor` in `s`.  When `cursor` is past
/// the end, the char is appended.
fn insert_char_at(s: &mut String, cursor: usize, ch: char) {
    let byte_idx = s
        .char_indices()
        .nth(cursor)
        .map(|(b, _)| b)
        .unwrap_or(s.len());
    s.insert(byte_idx, ch);
}

/// Remove the char at char-index `cursor` from `s`.  No-op when the
/// index is out of bounds.
fn remove_char_at(s: &mut String, cursor: usize) {
    if let Some((byte_idx, ch)) = s.char_indices().nth(cursor) {
        s.replace_range(byte_idx..byte_idx + ch.len_utf8(), "");
    }
}

/// Build the default destination shown in the Save As field: the buffer's
/// current path resolved to an absolute path (so the directory is visible
/// and the user can retarget it), or `<cwd>/untitled.md` for an unnamed
/// buffer.  The filename is left unchanged — Save As re-points the buffer
/// to the same name (possibly in a different directory).
pub fn default_save_as_path(original: Option<&Path>) -> String {
    let path = original
        .map(Path::to_owned)
        .unwrap_or_else(|| PathBuf::from("untitled.md"));
    absolutize(path)
}

/// Resolve `path` to an absolute path against the current working
/// directory (leaving an already-absolute path untouched) and render it
/// for display in a path field.
fn absolutize(path: PathBuf) -> String {
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    };
    absolute.display().to_string()
}

/// View-only widget that renders the modal over the editor.  The same
/// path-entry widget backs several modals (Save a Copy, Save As, the
/// file-conflict copy), so the frame title is supplied by the caller.
pub struct SaveCopyView<'a> {
    pub theme: &'a Theme,
    pub cursor_visible: bool,
    /// Frame title, e.g. `"Save a Copy"` or `"Save As"`.
    pub title: &'static str,
}

impl<'a> StatefulWidget for SaveCopyView<'a> {
    type State = SaveCopyState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Layout: 1 path row + (optional) 1 error row + 1 spacer + 1
        // buttons row.
        let body_rows = if state.last_error.is_some() { 4 } else { 3 };
        // A path can be long; size the modal generously but cap so we
        // don't fill the whole screen.  `centered_rect_for_content`
        // clamps to the terminal width on its own.
        let label_w = "Path".chars().count() as u16;
        let path_w = (state.path.chars().count() as u16 + 4).max(40);
        let buttons_w = button_row_width(BUTTON_LABELS);
        let content_width = (label_w + 2 + path_w).max(buttons_w);
        let content = ContentSize {
            width: content_width,
            height: 0,
            pinned_top: body_rows,
            pinned_bottom: 0,
            ..Default::default()
        };
        let modal_area = centered_rect_for_content(content, area);
        let layout = draw_frame(
            modal_area,
            buf,
            FrameOpts {
                title: self.title,
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
        // Path row.
        render_path_row(
            buf,
            inner,
            row_y,
            &state.path,
            state.cursor,
            state.focus == SaveCopyField::Path,
            self.theme,
            self.cursor_visible,
        );
        row_y = row_y.saturating_add(1);

        // Error row (optional, between field and buttons).
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

        // Spacer between field/error and buttons.
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
            height: 1,
        };
        render_buttons(button_area, buf, state.focus, self.theme);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_path_row(
    buf: &mut Buffer,
    inner: Rect,
    y: u16,
    value: &str,
    cursor: usize,
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
    let value_style = controls::text_value_style(focused, theme);

    let mut spans: Vec<Span<'_>> = Vec::with_capacity(6);
    spans.push(Span::styled("Path", theme.modal_item));
    spans.push(Span::raw("  "));
    // Leading pad so the value sits one cell off the label.
    spans.push(Span::styled(" ", value_style));
    if focused {
        // Shared cursor renderer: a blink-stable `▏` insertion-point bar at
        // the cursor, so the field width never changes between blink phases.
        spans.extend(text_field_spans(
            value,
            cursor,
            cursor_visible,
            value_style,
            theme.cursor,
        ));
        // Trailing pad mirrors the unfocused branch's right-side pad.
        spans.push(Span::styled(" ", value_style));
    } else {
        spans.push(Span::styled(value.to_owned(), value_style));
        spans.push(Span::styled(" ", value_style));
    }
    Paragraph::new(Line::from(spans))
        .style(theme.modal_bg)
        .render(area, buf);
}

fn render_buttons(area: Rect, buf: &mut Buffer, focus: SaveCopyField, theme: &Theme) {
    let focused_idx = match focus {
        SaveCopyField::Save => 0,
        SaveCopyField::Cancel => 1,
        SaveCopyField::Path => usize::MAX,
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
    fn save_as_default_keeps_name_and_shows_absolute_directory() {
        // Save As keeps the filename (no "… copy") but resolves to an
        // absolute path so the destination directory is visible/editable.
        let p = Path::new("/tmp/notes.md");
        assert_eq!(default_save_as_path(Some(p)), "/tmp/notes.md");

        let cwd = std::env::current_dir().expect("cwd");
        let rel = default_save_as_path(Some(Path::new("notes.md")));
        assert_eq!(rel, cwd.join("notes.md").display().to_string());

        // An unnamed buffer defaults to <cwd>/untitled.md.
        let unnamed = default_save_as_path(None);
        assert_eq!(unnamed, cwd.join("untitled.md").display().to_string());
    }

    #[test]
    fn cursor_initially_at_end_of_default_path() {
        let s = SaveCopyState::new("/tmp/notes copy.md".to_owned());
        assert_eq!(s.cursor, "/tmp/notes copy.md".chars().count());
    }

    #[test]
    fn left_arrow_in_path_moves_cursor_back() {
        let mut s = SaveCopyState::new("abc".to_owned());
        assert_eq!(s.cursor, 3);
        s.handle_key(&key(KeyCode::Left));
        assert_eq!(s.cursor, 2);
        // Focus must not change.
        assert_eq!(s.focus, SaveCopyField::Path);
    }

    #[test]
    fn left_arrow_clamps_at_zero() {
        let mut s = SaveCopyState::new("ab".to_owned());
        for _ in 0..10 {
            s.handle_key(&key(KeyCode::Left));
        }
        assert_eq!(s.cursor, 0);
        assert_eq!(s.focus, SaveCopyField::Path);
    }

    #[test]
    fn right_arrow_in_path_advances_cursor_clamped() {
        let mut s = SaveCopyState::new("ab".to_owned());
        s.cursor = 0;
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(s.cursor, 1);
        s.handle_key(&key(KeyCode::Right));
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(s.cursor, 2, "must clamp at path length");
    }

    #[test]
    fn home_jumps_to_start_end_jumps_to_end() {
        let mut s = SaveCopyState::new("hello".to_owned());
        s.handle_key(&key(KeyCode::Home));
        assert_eq!(s.cursor, 0);
        s.handle_key(&key(KeyCode::End));
        assert_eq!(s.cursor, 5);
    }

    #[test]
    fn typing_inserts_at_cursor_position() {
        let mut s = SaveCopyState::new("ac".to_owned());
        s.cursor = 1;
        s.handle_key(&key(KeyCode::Char('b')));
        assert_eq!(s.path, "abc");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn backspace_removes_char_before_cursor() {
        let mut s = SaveCopyState::new("abc".to_owned());
        s.cursor = 2;
        s.handle_key(&key(KeyCode::Backspace));
        assert_eq!(s.path, "ac");
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut s = SaveCopyState::new("abc".to_owned());
        s.cursor = 0;
        s.handle_key(&key(KeyCode::Backspace));
        assert_eq!(s.path, "abc");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn delete_removes_char_at_cursor() {
        let mut s = SaveCopyState::new("abc".to_owned());
        s.cursor = 1;
        s.handle_key(&key(KeyCode::Delete));
        assert_eq!(s.path, "ac");
        assert_eq!(s.cursor, 1, "Delete must not move the cursor");
    }

    #[test]
    fn delete_at_end_is_noop() {
        let mut s = SaveCopyState::new("abc".to_owned());
        // Cursor starts at end.
        s.handle_key(&key(KeyCode::Delete));
        assert_eq!(s.path, "abc");
        assert_eq!(s.cursor, 3);
    }

    #[test]
    fn cursor_handles_multibyte_chars() {
        // "naïve" is 5 chars, but the 'ï' is 2 bytes.  Inserting at
        // char-index 3 should split between 'ï' and 'v'.
        let mut s = SaveCopyState::new("naïve".to_owned());
        assert_eq!(s.cursor, 5);
        s.cursor = 3;
        s.handle_key(&key(KeyCode::Char('-')));
        assert_eq!(s.path, "naï-ve");
    }

    #[test]
    fn typing_appends_to_path() {
        let mut s = SaveCopyState::new(String::new());
        s.handle_key(&key(KeyCode::Char('a')));
        s.handle_key(&key(KeyCode::Char('/')));
        s.handle_key(&key(KeyCode::Char('b')));
        assert_eq!(s.path, "a/b");
    }

    #[test]
    fn space_in_path_field_inserts_literal_space() {
        let mut s = SaveCopyState::new("foo".to_owned());
        s.handle_key(&key(KeyCode::Char(' ')));
        s.handle_key(&key(KeyCode::Char('b')));
        assert_eq!(s.path, "foo b");
    }

    #[test]
    fn backspace_pops_from_path() {
        let mut s = SaveCopyState::new("abc".to_owned());
        s.handle_key(&key(KeyCode::Backspace));
        assert_eq!(s.path, "ab");
    }

    #[test]
    fn tab_cycles_through_path_save_cancel() {
        let mut s = SaveCopyState::new(String::new());
        s.handle_key(&key(KeyCode::Tab));
        assert_eq!(s.focus, SaveCopyField::Save);
        s.handle_key(&key(KeyCode::Tab));
        assert_eq!(s.focus, SaveCopyField::Cancel);
        s.handle_key(&key(KeyCode::Tab));
        assert_eq!(s.focus, SaveCopyField::Path);
    }

    #[test]
    fn shift_tab_cycles_backwards() {
        let mut s = SaveCopyState::new(String::new());
        s.handle_key(&KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(s.focus, SaveCopyField::Cancel);
    }

    #[test]
    fn escape_cancels() {
        let mut s = SaveCopyState::new("foo.md".to_owned());
        let r = s.handle_key(&key(KeyCode::Esc));
        assert_eq!(r, SaveCopyResponse::Cancelled);
    }

    #[test]
    fn enter_on_path_submits_with_value() {
        let mut s = SaveCopyState::new("foo.md".to_owned());
        let r = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(r, SaveCopyResponse::Save("foo.md".to_owned()));
    }

    #[test]
    fn enter_on_save_button_submits() {
        let mut s = SaveCopyState::new("foo.md".to_owned());
        s.focus = SaveCopyField::Save;
        let r = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(r, SaveCopyResponse::Save("foo.md".to_owned()));
    }

    #[test]
    fn enter_on_cancel_button_cancels() {
        let mut s = SaveCopyState::new("foo.md".to_owned());
        s.focus = SaveCopyField::Cancel;
        let r = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(r, SaveCopyResponse::Cancelled);
    }

    #[test]
    fn empty_path_blocks_submit_and_flags_error() {
        let mut s = SaveCopyState::new(String::new());
        let r = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(r, SaveCopyResponse::Continue);
        assert!(s.last_error.is_some());
        assert_eq!(s.focus, SaveCopyField::Path);
    }

    #[test]
    fn whitespace_only_path_blocks_submit() {
        let mut s = SaveCopyState::new("   ".to_owned());
        let r = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(r, SaveCopyResponse::Continue);
        assert!(s.last_error.is_some());
    }

    #[test]
    fn ctrl_chars_do_not_pollute_path() {
        let mut s = SaveCopyState::new("a".to_owned());
        let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        s.handle_key(&ctrl_p);
        assert_eq!(s.path, "a");
    }

    #[test]
    fn left_right_swap_buttons_when_focused_on_a_button() {
        let mut s = SaveCopyState::new(String::new());
        s.focus = SaveCopyField::Save;
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(s.focus, SaveCopyField::Cancel);
        s.handle_key(&key(KeyCode::Left));
        assert_eq!(s.focus, SaveCopyField::Save);
    }

    #[test]
    fn renders_title_path_and_buttons() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = SaveCopyState::new("/tmp/notes copy.md".to_owned());
        terminal
            .draw(|frame| {
                let m = SaveCopyView {
                    theme: theme(),
                    cursor_visible: true,
                    title: "Save a Copy",
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
            contents.contains("Save a Copy"),
            "title missing: {contents}"
        );
        assert!(contents.contains("Path"), "path label missing: {contents}");
        assert!(
            contents.contains("notes copy.md"),
            "path value missing: {contents}"
        );
        assert!(contents.contains("Save"), "save button missing: {contents}");
        assert!(
            contents.contains("Cancel"),
            "cancel button missing: {contents}"
        );
    }
}
