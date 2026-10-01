//! Shared path-entry widget ([`SaveCopyState`] + [`SaveCopyView`]): one "Path" field above a
//! Save / Cancel row, used by Save As, the file-deleted recovery prompt, the dirty-conflict
//! "save aside" flow, and the pasted-image path prompt.  Each modal supplies its own title and decides what the path does when
//! [`SaveCopyResponse::Save`] fires; this widget is UI-only.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget, Wrap},
};

use crate::config::Theme;
use crate::ui::button_row::{button_row_width, footer_row_count, render_button_row};
use crate::ui::controls;
use crate::ui::cursor::{insert_char_at, remove_char_at, text_field_spans};
use crate::ui::scroll_container::{
    centered_rect_for_content, draw_frame, modal_inner_width, wrapped_rows, ContentSize, FrameOpts,
    ModalKind, MAX_PAD_H,
};

const BUTTON_LABELS: &[&str] = &["Save", "Cancel"];

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveCopyResponse {
    Continue,
    Cancelled,
    /// Save pressed with a non-empty (trimmed) path.
    Save(String),
}

#[derive(Debug, Clone)]
pub struct SaveCopyState {
    /// Seeded by the App via [`default_save_as_path`].
    pub path: String,
    /// Char index into [`Self::path`]; starts at the end so the default can be edited at once.
    pub cursor: usize,
    pub focus: SaveCopyField,
    /// Last validation message; cleared when the field changes.
    pub last_error: Option<String>,
    /// Absolute rect of the rendered `esc` close hint.
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

    /// Apply a key event: field editing on the path, Tab / Shift-Tab / Up / Down cycle focus,
    /// Enter submits.  Ctrl / Alt chords are ignored so they never pollute the field.
    pub fn handle_key(&mut self, key: &KeyEvent) -> SaveCopyResponse {
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
            // Left / Right move the field cursor, or swap buttons when focus is on one.
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
            // Space activates a focused button; on the path field the `Char` arm above
            // already inserted a literal space.
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

    /// Insert a bracketed paste at the cursor (no-op on a button), flattened and capped by
    /// [`crate::ui::sanitize_paste`].
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

/// Default Save As destination: the buffer's path made absolute (so the directory is visible
/// and editable), or `untitled.md` under the cwd for an unnamed buffer.
pub fn default_save_as_path(original: Option<&Path>) -> String {
    let path = original
        .map(Path::to_owned)
        .unwrap_or_else(|| PathBuf::from("untitled.md"));
    absolutize(path)
}

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

pub struct SaveCopyView<'a> {
    pub theme: &'a Theme,
    pub cursor_visible: bool,
    /// Frame title, supplied by the owning modal.
    pub title: &'static str,
    /// Optional line above the path field saying what the path means.
    pub note: Option<&'static str>,
}

impl<'a> StatefulWidget for SaveCopyView<'a> {
    type State = SaveCopyState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let note = self
            .note
            .map(|note| Line::from(Span::styled(note, self.theme.modal_description)));
        let label_w = "Path".chars().count() as u16;
        let path_w = (state.path.chars().count() as u16 + 4).max(40);
        // The note asks for one line; a terminal too narrow for that wraps it.
        let note_w = note.as_ref().map_or(0, |line| line.width() as u16);
        let buttons_w = button_row_width(BUTTON_LABELS);
        let content_width = (label_w + 2 + path_w).max(buttons_w).max(note_w);
        let note_h = note.as_ref().map_or(0, |line| {
            let width = modal_inner_width(content_width, area.width, MAX_PAD_H);
            wrapped_rows(std::slice::from_ref(line), width)
        });
        // Note + spacer, 1 path row + optional error row + 1 spacer, then the footer.
        let note_rows = if note.is_some() { note_h + 1 } else { 0 };
        let base_rows = note_rows + if state.last_error.is_some() { 3 } else { 2 };
        // The footer wraps rather than clipping; a flat one-row reservation would leave a
        // wrapped button unpainted but still focusable.
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
        if let Some(note) = note {
            let height = note_h.min(inner.height);
            Paragraph::new(note)
                .wrap(Wrap { trim: false })
                .style(self.theme.modal_bg)
                .render(Rect { height, ..inner }, buf);
            row_y = row_y.saturating_add(note_rows);
            if row_y >= inner.y + inner.height {
                return;
            }
        }
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

        if row_y < inner.y + inner.height {
            row_y = row_y.saturating_add(1);
        }
        if row_y >= inner.y + inner.height {
            return;
        }
        let button_area = Rect {
            x: inner.x,
            y: row_y,
            width: inner.width,
            height: (inner.y + inner.height).saturating_sub(row_y),
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
    spans.push(Span::styled(" ", value_style));
    if focused {
        spans.extend(text_field_spans(
            value,
            cursor,
            cursor_visible,
            value_style,
            theme.cursor,
        ));
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
        // A real tempdir rather than a `/`-rooted literal: that is not absolute on Windows.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.md");
        assert_eq!(default_save_as_path(Some(&p)), p.display().to_string());

        let cwd = std::env::current_dir().expect("cwd");
        let rel = default_save_as_path(Some(Path::new("notes.md")));
        assert_eq!(rel, cwd.join("notes.md").display().to_string());

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
        s.handle_key(&key(KeyCode::Delete));
        assert_eq!(s.path, "abc");
        assert_eq!(s.cursor, 3);
    }

    #[test]
    fn cursor_handles_multibyte_chars() {
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
    fn a_narrow_terminal_wraps_the_footer_and_still_paints_both_buttons() {
        // Regression: a flat one-row footer reservation left Cancel unpainted but focusable.
        let backend = TestBackend::new(18, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = SaveCopyState::new("/tmp/a.md".to_owned());
        terminal
            .draw(|frame| {
                let m = SaveCopyView {
                    theme: theme(),
                    cursor_visible: true,
                    title: "Save a Copy",
                    note: None,
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
        assert!(painted.contains("[ Save ]"), "{painted}");
        assert!(painted.contains("[ Cancel ]"), "{painted}");
    }

    /// `SaveCopyView` with `note`, drawn at `width` x `height`, one string per row.
    fn render_with_note(note: &'static str, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut state = SaveCopyState::new("images/shot.png".to_owned());
        terminal
            .draw(|frame| {
                let m = SaveCopyView {
                    theme: theme(),
                    cursor_visible: true,
                    title: "Paste Image",
                    note: Some(note),
                };
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect()
    }

    const NOTE: &str = "Save the image to this path, relative to the document's folder:";

    #[test]
    fn a_note_fits_on_one_line_when_there_is_room() {
        let rows = render_with_note(NOTE, 100, 14);
        assert!(rows.iter().any(|row| row.contains(NOTE)), "{rows:#?}");
    }

    #[test]
    fn a_narrow_terminal_wraps_the_note_above_the_path() {
        let rows = render_with_note(NOTE, 44, 16);
        let row_of = |needle: &str| {
            rows.iter()
                .position(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} missing: {rows:#?}"))
        };
        let (first, last) = (row_of("Save the image"), row_of("folder:"));
        assert!(first < last, "the note wraps: {rows:#?}");
        assert!(last < row_of("Path"), "the path row follows it: {rows:#?}");
        row_of("[ Cancel ]");
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
                    note: None,
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
