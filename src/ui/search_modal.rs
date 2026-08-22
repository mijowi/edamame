//! Search-and-replace input modal for `Action::OpenSearch`.
//!
//! Two free-text fields ("Search" and "Replace") above a Search /
//! Cancel button row.  Tab / Shift-Tab and Up / Down move between the
//! four focus targets; while focus is on a field, character keys insert
//! at the in-field cursor, Left / Right move it, Home / End jump to the
//! ends, Backspace / Delete remove characters, and Enter submits.  The
//! replace field may be left empty — that selects a navigate-only flow
//! (no Replace / Replace-all keys); any text in it enables the replace
//! flow.
//!
//! The widget is UI-only: the App layer reads the terms when
//! [`SearchModalResponse::Search`] fires and starts the flow via
//! `App::enter_search_flow`.

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

const BUTTON_LABELS: &[&str] = &["Search", "Cancel"];

/// One of the four focus targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchModalField {
    Query,
    Replace,
    Search,
    Cancel,
}

impl SearchModalField {
    fn next(self) -> Self {
        match self {
            Self::Query => Self::Replace,
            Self::Replace => Self::Search,
            Self::Search => Self::Cancel,
            Self::Cancel => Self::Query,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Query => Self::Cancel,
            Self::Replace => Self::Query,
            Self::Search => Self::Replace,
            Self::Cancel => Self::Search,
        }
    }

    fn is_field(self) -> bool {
        matches!(self, Self::Query | Self::Replace)
    }
}

/// Outcome of dispatching a key event to the modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchModalResponse {
    /// Modal stays open; the caller just redraws.
    Continue,
    /// User dismissed (Escape or the Cancel button).
    Cancelled,
    /// User confirmed with a non-empty search term.  `replace` is
    /// `None` when the replace field was left empty (navigate-only
    /// flow).
    Search {
        query: String,
        replace: Option<String>,
    },
}

/// Mutable state for an open search/replace modal.
#[derive(Debug, Clone)]
pub struct SearchModalState {
    /// The search term being edited.
    pub query: String,
    /// The replacement text being edited.  Empty selects the
    /// navigate-only flow.
    pub replace: String,
    /// In-field cursor for [`Self::query`], as a char index.
    pub query_cursor: usize,
    /// In-field cursor for [`Self::replace`], as a char index.
    pub replace_cursor: usize,
    /// Which focus target receives keystrokes.
    pub focus: SearchModalField,
    /// Last validation message ("Search term required").  Cleared when
    /// the user mutates a field.
    pub last_error: Option<String>,
    /// Absolute terminal rect of the rendered `esc` close hint.
    pub esc_button_rect: Option<Rect>,
}

impl SearchModalState {
    /// Build the state, pre-filled when re-opened over an active flow.
    /// Cursors start at the end of each pre-filled value so the user
    /// can immediately extend or backspace.
    pub fn new(query: String, replace: String) -> Self {
        let query_cursor = query.chars().count();
        let replace_cursor = replace.chars().count();
        Self {
            query,
            replace,
            query_cursor,
            replace_cursor,
            focus: SearchModalField::Query,
            last_error: None,
            esc_button_rect: None,
        }
    }

    /// Apply a key event.  Mirrors `SaveCopyState::handle_key`, with
    /// the field-editing arms operating on whichever field is focused.
    pub fn handle_key(&mut self, key: &KeyEvent) -> SearchModalResponse {
        // Modifier-augmented chords are ignored so chords don't
        // pollute the fields.
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return SearchModalResponse::Continue;
        }

        match key.code {
            KeyCode::Esc => return SearchModalResponse::Cancelled,
            KeyCode::Tab | KeyCode::Down => self.focus = self.focus.next(),
            KeyCode::BackTab | KeyCode::Up => self.focus = self.focus.prev(),
            // On a field, Left / Right move the in-field cursor; on a
            // button they swap between Search and Cancel.
            KeyCode::Left => {
                if self.focus.is_field() {
                    let cursor = self.focused_cursor_mut();
                    *cursor = cursor.saturating_sub(1);
                } else {
                    self.focus = self.focus.prev();
                }
            }
            KeyCode::Right => {
                if self.focus.is_field() {
                    let len = self.focused_value().chars().count();
                    let cursor = self.focused_cursor_mut();
                    if *cursor < len {
                        *cursor += 1;
                    }
                } else {
                    self.focus = self.focus.next();
                }
            }
            KeyCode::Home if self.focus.is_field() => *self.focused_cursor_mut() = 0,
            KeyCode::End if self.focus.is_field() => {
                *self.focused_cursor_mut() = self.focused_value().chars().count();
            }
            KeyCode::Backspace if self.focus.is_field() => {
                let (value, cursor) = self.focused_pair_mut();
                if *cursor > 0 {
                    let target = *cursor - 1;
                    remove_char_at(value, target);
                    *cursor = target;
                    self.last_error = None;
                }
            }
            KeyCode::Delete if self.focus.is_field() => {
                let (value, cursor) = self.focused_pair_mut();
                if *cursor < value.chars().count() {
                    let at = *cursor;
                    remove_char_at(value, at);
                    self.last_error = None;
                }
            }
            KeyCode::Enter => {
                return match self.focus {
                    SearchModalField::Cancel => SearchModalResponse::Cancelled,
                    _ => self.try_search(),
                };
            }
            // Space activates a focused button; on a field it falls
            // through to the `Char` arm and inserts a literal space.
            KeyCode::Char(' ') if !self.focus.is_field() => {
                return match self.focus {
                    SearchModalField::Cancel => SearchModalResponse::Cancelled,
                    _ => self.try_search(),
                };
            }
            KeyCode::Char(c) if self.focus.is_field() => {
                let (value, cursor) = self.focused_pair_mut();
                insert_char_at(value, *cursor, c);
                *cursor += 1;
                self.last_error = None;
            }
            _ => {}
        }
        SearchModalResponse::Continue
    }

    /// Insert a bracketed paste into the focused field (Search or
    /// Replace) at its in-field cursor.  No-op when focus is on a
    /// button.
    ///
    /// The payload is **escaped first** (`search::escape::escape`) and
    /// only then flattened and length-capped by
    /// [`crate::ui::sanitize_paste`].  These fields are written in
    /// escape syntax, so a pasted multi-line snippet becomes a working
    /// `\n`-joined query instead of silently collapsing to one line, and
    /// a pasted backslash searches for itself.
    pub fn paste(&mut self, text: &str) {
        if !self.focus.is_field() {
            return;
        }
        let clean = crate::ui::sanitize_paste(&crate::search::escape::escape(text));
        if clean.is_empty() {
            return;
        }
        let (value, cursor) = self.focused_pair_mut();
        for c in clean.chars() {
            insert_char_at(value, *cursor, c);
            *cursor += 1;
        }
        self.last_error = None;
    }

    fn try_search(&mut self) -> SearchModalResponse {
        if self.query.is_empty() {
            self.last_error = Some("Search term required".to_owned());
            self.focus = SearchModalField::Query;
            return SearchModalResponse::Continue;
        }
        let replace = (!self.replace.is_empty()).then(|| self.replace.clone());
        // Validate the escapes here so a malformed one lands in the
        // modal's own error row, with focus on the offending field —
        // the flow-entry path can only flash, which is a worse place to
        // correct a typo from.
        if let Err(e) = crate::search::escape::decode(&self.query) {
            self.last_error = Some(e.to_string());
            self.focus = SearchModalField::Query;
            return SearchModalResponse::Continue;
        }
        if let Some(Err(e)) = replace.as_deref().map(crate::search::escape::decode) {
            self.last_error = Some(e.to_string());
            self.focus = SearchModalField::Replace;
            return SearchModalResponse::Continue;
        }
        SearchModalResponse::Search {
            query: self.query.clone(),
            replace,
        }
    }

    fn focused_value(&self) -> &str {
        match self.focus {
            SearchModalField::Replace => &self.replace,
            _ => &self.query,
        }
    }

    fn focused_cursor_mut(&mut self) -> &mut usize {
        match self.focus {
            SearchModalField::Replace => &mut self.replace_cursor,
            _ => &mut self.query_cursor,
        }
    }

    fn focused_pair_mut(&mut self) -> (&mut String, &mut usize) {
        match self.focus {
            SearchModalField::Replace => (&mut self.replace, &mut self.replace_cursor),
            _ => (&mut self.query, &mut self.query_cursor),
        }
    }
}

/// Insert `ch` at char-index `cursor` in `s`; appends past the end.
fn insert_char_at(s: &mut String, cursor: usize, ch: char) {
    let byte_idx = s
        .char_indices()
        .nth(cursor)
        .map(|(b, _)| b)
        .unwrap_or(s.len());
    s.insert(byte_idx, ch);
}

/// Remove the char at char-index `cursor` from `s`; no-op out of bounds.
fn remove_char_at(s: &mut String, cursor: usize) {
    if let Some((byte_idx, ch)) = s.char_indices().nth(cursor) {
        s.replace_range(byte_idx..byte_idx + ch.len_utf8(), "");
    }
}

/// Indent of the note row, aligning it under the input's value column
/// ("Search " label + a two-cell gap).
const NOTE_INDENT: usize = 9;

/// The metadata line under the search field.  The matcher depends on the
/// flow: a navigate-only search (empty replace field) is smartcase —
/// case-insensitive unless the query has an uppercase letter — while a
/// replace flow stays strictly case-sensitive so a lowercase find never
/// rewrites a casing variant the user didn't type (see
/// `SearchState::ensure_fresh`).  The escape hint rides on the same row
/// so the modal keeps its height; `\n` is the escape people come looking
/// for, and it implies the rest.
fn matching_mode_note(state: &SearchModalState) -> &'static str {
    if state.replace.is_empty() {
        r"(Smart case · \n for a line break)"
    } else {
        r"(Case sensitive · \n for a line break)"
    }
}

/// View-only widget that renders the modal over the editor.
pub struct SearchModalView<'a> {
    pub theme: &'a Theme,
    pub cursor_visible: bool,
}

impl<'a> StatefulWidget for SearchModalView<'a> {
    type State = SearchModalState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Layout: search field row + case note row + replace field row
        // + (optional) 1 error row + 1 spacer + the buttons, which take
        // more than one row in a terminal too narrow for the pair.
        let base_rows = if state.last_error.is_some() { 5 } else { 4 };
        let label_w = "Replace".chars().count() as u16;
        let longest_value = state
            .query
            .chars()
            .count()
            .max(state.replace.chars().count()) as u16;
        let value_w = (longest_value + 4).max(32);
        let buttons_w = button_row_width(BUTTON_LABELS);
        // The note row is indented under the value column, so its own
        // width has to be part of the sizing or it renders clipped.
        let note = matching_mode_note(state);
        let note_w = NOTE_INDENT as u16 + note.chars().count() as u16;
        let content_width = (label_w + 2 + value_w).max(buttons_w).max(note_w);
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
                title: "Search and Replace",
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
        render_field_row(
            buf,
            inner,
            row_y,
            "Search ",
            &state.query,
            state.query_cursor,
            state.focus == SearchModalField::Query,
            self.theme,
            self.cursor_visible,
        );
        row_y = row_y.saturating_add(1);
        // Matching-mode note (see `matching_mode_note`), in quiet hint
        // styling so it reads as metadata, not another field.
        if row_y < inner.y + inner.height {
            let note_area = Rect {
                x: inner.x,
                y: row_y,
                width: inner.width,
                height: 1,
            };
            Paragraph::new(Line::from(vec![
                Span::raw(" ".repeat(NOTE_INDENT)),
                Span::styled(note, self.theme.text_muted()),
            ]))
            .style(self.theme.modal_bg)
            .render(note_area, buf);
            row_y = row_y.saturating_add(1);
        }
        render_field_row(
            buf,
            inner,
            row_y,
            "Replace",
            &state.replace,
            state.replace_cursor,
            state.focus == SearchModalField::Replace,
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

        // Spacer between fields/error and buttons.
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
        let focused_idx = match state.focus {
            SearchModalField::Search => 0,
            SearchModalField::Cancel => 1,
            _ => usize::MAX,
        };
        render_button_row(button_area, buf, BUTTON_LABELS, focused_idx, self.theme);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_field_row(
    buf: &mut Buffer,
    inner: Rect,
    y: u16,
    label: &str,
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

    let mut spans: Vec<Span<'_>> = Vec::with_capacity(7);
    spans.push(Span::styled(label.to_owned(), theme.modal_item));
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn tab_cycles_query_replace_search_cancel() {
        let mut s = SearchModalState::new(String::new(), String::new());
        assert_eq!(s.focus, SearchModalField::Query);
        s.handle_key(&key(KeyCode::Tab));
        assert_eq!(s.focus, SearchModalField::Replace);
        s.handle_key(&key(KeyCode::Tab));
        assert_eq!(s.focus, SearchModalField::Search);
        s.handle_key(&key(KeyCode::Tab));
        assert_eq!(s.focus, SearchModalField::Cancel);
        s.handle_key(&key(KeyCode::Tab));
        assert_eq!(s.focus, SearchModalField::Query);
    }

    #[test]
    fn typing_targets_the_focused_field() {
        let mut s = SearchModalState::new(String::new(), String::new());
        s.handle_key(&key(KeyCode::Char('f')));
        s.handle_key(&key(KeyCode::Char('o')));
        s.handle_key(&key(KeyCode::Tab));
        s.handle_key(&key(KeyCode::Char('b')));
        assert_eq!(s.query, "fo");
        assert_eq!(s.replace, "b");
    }

    #[test]
    fn each_field_keeps_its_own_cursor() {
        let mut s = SearchModalState::new("abc".to_owned(), "xyz".to_owned());
        s.handle_key(&key(KeyCode::Left));
        assert_eq!(s.query_cursor, 2);
        s.handle_key(&key(KeyCode::Tab));
        assert_eq!(s.replace_cursor, 3, "replace cursor untouched");
        s.handle_key(&key(KeyCode::Char('!')));
        assert_eq!(s.replace, "xyz!");
        assert_eq!(s.query, "abc");
    }

    #[test]
    fn enter_with_empty_query_blocks_and_flags_error() {
        let mut s = SearchModalState::new(String::new(), String::new());
        let r = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(r, SearchModalResponse::Continue);
        assert!(s.last_error.is_some());
        assert_eq!(s.focus, SearchModalField::Query);
    }

    #[test]
    fn enter_submits_with_replace_none_when_field_empty() {
        let mut s = SearchModalState::new("term".to_owned(), String::new());
        let r = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(
            r,
            SearchModalResponse::Search {
                query: "term".to_owned(),
                replace: None,
            }
        );
    }

    #[test]
    fn enter_submits_with_replace_some_when_field_filled() {
        let mut s = SearchModalState::new("term".to_owned(), "swap".to_owned());
        s.focus = SearchModalField::Search;
        let r = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(
            r,
            SearchModalResponse::Search {
                query: "term".to_owned(),
                replace: Some("swap".to_owned()),
            }
        );
    }

    #[test]
    fn esc_and_cancel_button_dismiss() {
        let mut s = SearchModalState::new("term".to_owned(), String::new());
        assert_eq!(
            s.handle_key(&key(KeyCode::Esc)),
            SearchModalResponse::Cancelled
        );
        let mut s = SearchModalState::new("term".to_owned(), String::new());
        s.focus = SearchModalField::Cancel;
        assert_eq!(
            s.handle_key(&key(KeyCode::Enter)),
            SearchModalResponse::Cancelled
        );
    }

    #[test]
    fn space_inserts_literally_in_fields_and_activates_buttons() {
        let mut s = SearchModalState::new("a".to_owned(), String::new());
        s.handle_key(&key(KeyCode::Char(' ')));
        assert_eq!(s.query, "a ");
        s.query = "a b".to_owned();
        s.focus = SearchModalField::Search;
        let r = s.handle_key(&key(KeyCode::Char(' ')));
        assert!(matches!(r, SearchModalResponse::Search { .. }));
    }

    #[test]
    fn ctrl_chords_do_not_pollute_fields() {
        let mut s = SearchModalState::new("a".to_owned(), String::new());
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        s.handle_key(&ctrl_f);
        assert_eq!(s.query, "a");
    }

    #[test]
    fn paste_inserts_into_focused_field_and_escapes_newlines() {
        let mut s = SearchModalState::new(String::new(), String::new());
        s.paste("foo\nbar");
        // The field is written in escape syntax, so a pasted break
        // survives as `\n` rather than vanishing.
        assert_eq!(s.query, r"foo\nbar", "newline escaped, query targeted");
        assert_eq!(s.query_cursor, 8);
        s.handle_key(&key(KeyCode::Tab)); // focus Replace
        s.paste("baz");
        assert_eq!(s.replace, "baz");
        assert_eq!(
            s.query, r"foo\nbar",
            "query untouched while Replace focused"
        );
    }

    #[test]
    fn paste_escapes_a_literal_backslash() {
        let mut s = SearchModalState::new(String::new(), String::new());
        s.paste(r"C:\dir");
        assert_eq!(s.query, r"C:\\dir");
        // …and it decodes back to what was on the clipboard.
        assert_eq!(crate::search::escape::decode(&s.query).unwrap(), r"C:\dir");
    }

    #[test]
    fn paste_respects_the_in_field_cursor() {
        let mut s = SearchModalState::new("ad".to_owned(), String::new());
        s.handle_key(&key(KeyCode::Left)); // cursor between 'a' and 'd'
        s.paste("bc");
        assert_eq!(s.query, "abcd");
        assert_eq!(s.query_cursor, 3);
    }

    #[test]
    fn paste_is_a_noop_on_button_focus() {
        let mut s = SearchModalState::new("q".to_owned(), String::new());
        s.focus = SearchModalField::Search;
        s.paste("ignored");
        assert_eq!(s.query, "q");
        assert_eq!(s.replace, "");
    }

    #[test]
    fn a_narrow_terminal_wraps_the_footer_and_still_paints_both_buttons() {
        // The footer wraps rather than clipping, so the modal has to
        // reserve the rows it wrapped onto.  A flat one-row reservation
        // leaves Cancel unpainted while Tab still focuses it.
        let backend = TestBackend::new(20, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = SearchModalState::new("a".to_owned(), "b".to_owned());
        terminal
            .draw(|frame| {
                let m = SearchModalView {
                    theme: theme(),
                    cursor_visible: true,
                };
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let rows: Vec<String> = (0..16)
            .map(|y| (0..20).map(|x| buf[(x, y)].symbol().to_owned()).collect())
            .collect();
        let painted = rows.join("\n");
        assert!(painted.contains("[ Search ]"), "{painted}");
        assert!(painted.contains("[ Cancel ]"), "{painted}");
    }

    #[test]
    fn prefill_starts_cursors_at_field_ends() {
        let s = SearchModalState::new("naïve".to_owned(), "no".to_owned());
        assert_eq!(s.query_cursor, 5);
        assert_eq!(s.replace_cursor, 2);
    }

    #[test]
    fn renders_title_fields_and_buttons() {
        let backend = TestBackend::new(80, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = SearchModalState::new("needle".to_owned(), "thread".to_owned());
        terminal
            .draw(|frame| {
                let m = SearchModalView {
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
        assert!(contents.contains("Search and Replace"), "{contents}");
        assert!(contents.contains("needle"), "{contents}");
        assert!(contents.contains("thread"), "{contents}");
        // A filled replace field selects the case-sensitive replace flow.
        assert!(contents.contains("(Case sensitive"), "{contents}");
        assert!(contents.contains("Cancel"), "{contents}");
    }

    #[test]
    fn note_reflects_smartcase_when_replace_is_empty() {
        // An empty replace field selects the navigate-only flow, which
        // matches smartcase — the note must say so, not "(Case sensitive)".
        let backend = TestBackend::new(80, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = SearchModalState::new("needle".to_owned(), String::new());
        terminal
            .draw(|frame| {
                let m = SearchModalView {
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
        assert!(contents.contains("(Smart case"), "{contents}");
        assert!(!contents.contains("(Case sensitive"), "{contents}");
        // The escape hint shares the row.
        assert!(contents.contains(r"\n for a line break"), "{contents}");
    }
}
