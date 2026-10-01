//! Export a theme to a user-editable `.toml` file ("Create custom theme"): a [`SearchableList`]
//! theme picker, a name input seeded from the selection, and an `Export` button.  UI-only — the
//! adapter in `app/modal/export_theme.rs` writes the file and applies the new theme.

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
use crate::ui::cursor::{insert_char_at, remove_char_at};
use crate::ui::modal_row::{format_modal_row, RowLayout};
use crate::ui::scroll_container::{draw_frame, FrameOpts, ModalKind};
use crate::ui::searchable_list::{
    anchor_searchable_modal, FocusPolicy, ListChrome, ListEvent, RowCtx, SearchableList,
};

const BUTTON_LABELS: &[&str] = &["Export"];
const MAX_LIST_ROWS: u16 = 12;
const PLACEHOLDER: &str = "Type to filter themes…";
/// Total width of the name-input row, padding cells included.
const NAME_INPUT_WIDTH: u16 = 28;
/// Pinned rows above the list: heading, search input, divider.
const PINNED_TOP: u16 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportThemeField {
    ThemeList,
    Name,
    Export,
}

impl ExportThemeField {
    fn next(self) -> Self {
        match self {
            Self::ThemeList => Self::Name,
            Self::Name => Self::Export,
            Self::Export => Self::ThemeList,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::ThemeList => Self::Export,
            Self::Name => Self::ThemeList,
            Self::Export => Self::Name,
        }
    }
}

/// Outcome of dispatching a key event to the export-theme modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportThemeResponse {
    Continue,
    Cancelled,
    /// User pressed Export with a valid `(source_theme, new_name)`.
    Export {
        source: String,
        new_name: String,
    },
}

pub struct ExportThemeState {
    list: SearchableList<String>,
    pub name: String,
    /// Char-index cursor into `name`.
    pub cursor: usize,
    pub focus: ExportThemeField,
    pub last_error: Option<String>,
    pub esc_button_rect: Option<Rect>,
    /// Set on the first edit of the Name field; after that, list moves no longer re-seed it.
    name_user_edited: bool,
    /// First visible char of the name input, kept between frames (see
    /// [`crate::ui::cursor::scrolled_field_spans`]).
    name_scroll: usize,
}

impl ExportThemeState {
    pub fn new(themes: Vec<String>, active: &str) -> Self {
        let mut list = SearchableList::new(themes, |s: &String| s.as_str())
            .with_focus_policy(FocusPolicy::ResetToTop);
        let active = active.to_owned();
        list.focus_matching(|t| *t == active);
        let initial_name = list
            .focused_item()
            .map(|t| default_copy_name(t))
            .unwrap_or_default();
        let cursor = initial_name.chars().count();
        Self {
            list,
            name: initial_name,
            cursor,
            focus: ExportThemeField::ThemeList,
            last_error: None,
            esc_button_rect: None,
            name_user_edited: false,
            name_scroll: 0,
        }
    }

    /// All theme names (for duplicate-name validation).
    pub fn theme_names(&self) -> Vec<String> {
        self.list.items().to_vec()
    }

    pub fn scroll_by(&mut self, delta: i32) {
        self.list.scroll_by(delta);
    }

    /// The theme name backing the currently-selected row.
    pub fn selected_theme(&self) -> Option<&String> {
        self.list.focused_item()
    }

    pub fn handle_key(&mut self, key: &KeyEvent, existing: &[String]) -> ExportThemeResponse {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return ExportThemeResponse::Continue;
        }

        if matches!(self.focus, ExportThemeField::ThemeList)
            && matches!(
                key.code,
                KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
            )
        {
            self.list.handle_key(key);
            return ExportThemeResponse::Continue;
        }

        match key.code {
            KeyCode::Esc => ExportThemeResponse::Cancelled,
            KeyCode::Tab => {
                self.focus = self.focus.next();
                ExportThemeResponse::Continue
            }
            KeyCode::BackTab => {
                self.focus = self.focus.prev();
                ExportThemeResponse::Continue
            }
            KeyCode::Up => {
                match self.focus {
                    ExportThemeField::ThemeList => self.list_key(key),
                    _ => self.focus = self.focus.prev(),
                }
                ExportThemeResponse::Continue
            }
            KeyCode::Down => {
                match self.focus {
                    ExportThemeField::ThemeList => self.list_key(key),
                    _ => self.focus = self.focus.next(),
                }
                ExportThemeResponse::Continue
            }
            KeyCode::Left if matches!(self.focus, ExportThemeField::Name) => {
                self.cursor = self.cursor.saturating_sub(1);
                ExportThemeResponse::Continue
            }
            KeyCode::Right if matches!(self.focus, ExportThemeField::Name) => {
                if self.cursor < self.name.chars().count() {
                    self.cursor += 1;
                }
                ExportThemeResponse::Continue
            }
            KeyCode::Home if matches!(self.focus, ExportThemeField::Name) => {
                self.cursor = 0;
                ExportThemeResponse::Continue
            }
            KeyCode::End if matches!(self.focus, ExportThemeField::Name) => {
                self.cursor = self.name.chars().count();
                ExportThemeResponse::Continue
            }
            KeyCode::Backspace if matches!(self.focus, ExportThemeField::ThemeList) => {
                self.list_key(key);
                ExportThemeResponse::Continue
            }
            KeyCode::Backspace if matches!(self.focus, ExportThemeField::Name) => {
                if self.cursor > 0 {
                    let target = self.cursor - 1;
                    remove_char_at(&mut self.name, target);
                    self.cursor = target;
                    self.last_error = None;
                    self.name_user_edited = true;
                }
                ExportThemeResponse::Continue
            }
            KeyCode::Delete if matches!(self.focus, ExportThemeField::Name) => {
                if self.cursor < self.name.chars().count() {
                    remove_char_at(&mut self.name, self.cursor);
                    self.last_error = None;
                    self.name_user_edited = true;
                }
                ExportThemeResponse::Continue
            }
            KeyCode::Char(c) if matches!(self.focus, ExportThemeField::Name) => {
                insert_char_at(&mut self.name, self.cursor, c);
                self.cursor += 1;
                self.last_error = None;
                self.name_user_edited = true;
                ExportThemeResponse::Continue
            }
            KeyCode::Char(c) if matches!(self.focus, ExportThemeField::ThemeList) => {
                let synthetic = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
                self.list_key(&synthetic);
                ExportThemeResponse::Continue
            }
            KeyCode::Enter => match self.focus {
                // Enter on the picker advances to Name rather than exporting, so the user can rename.
                ExportThemeField::ThemeList => {
                    self.focus = ExportThemeField::Name;
                    ExportThemeResponse::Continue
                }
                ExportThemeField::Name | ExportThemeField::Export => self.try_export(existing),
            },
            KeyCode::Char(' ') if matches!(self.focus, ExportThemeField::Export) => {
                self.try_export(existing)
            }
            _ => ExportThemeResponse::Continue,
        }
    }

    /// Route a key to the list and re-seed the unedited name from the new selection.
    fn list_key(&mut self, key: &KeyEvent) {
        self.list.handle_key(key);
        self.last_error = None;
        self.sync_name_from_selection();
    }

    pub fn paste(&mut self, text: &str) {
        let clean = crate::ui::sanitize_paste(text);
        if clean.is_empty() {
            return;
        }
        match self.focus {
            ExportThemeField::Name => {
                for c in clean.chars() {
                    insert_char_at(&mut self.name, self.cursor, c);
                    self.cursor += 1;
                }
                self.last_error = None;
                self.name_user_edited = true;
            }
            ExportThemeField::ThemeList => {
                self.list.paste(&clean);
                self.last_error = None;
                self.sync_name_from_selection();
            }
            ExportThemeField::Export => {}
        }
    }

    /// A click on a theme row selects it and re-seeds the name.
    pub fn handle_click(&mut self, col: u16, row: u16) {
        if let ListEvent::Submitted(i) = self.list.handle_click(col, row) {
            self.list.focus_item(i);
            self.focus = ExportThemeField::ThemeList;
            self.last_error = None;
            self.sync_name_from_selection();
        }
    }

    fn try_export(&mut self, existing: &[String]) -> ExportThemeResponse {
        let Some(source) = self.selected_theme().cloned() else {
            self.last_error = Some("No theme selected".to_owned());
            return ExportThemeResponse::Continue;
        };
        // Leading dots stripped so the file name can't be `.`, `..`, or a dotfile.
        let new_name = self.name.trim().trim_start_matches('.').to_owned();
        if new_name.is_empty() {
            self.last_error = Some("Name required".to_owned());
            self.focus = ExportThemeField::Name;
            return ExportThemeResponse::Continue;
        }
        if new_name.contains(|c: char| c == '/' || c == '\\' || c == '\0' || c.is_control()) {
            self.last_error =
                Some("Name cannot contain path separators or control chars".to_owned());
            self.focus = ExportThemeField::Name;
            return ExportThemeResponse::Continue;
        }
        if existing.iter().any(|t| t.eq_ignore_ascii_case(&new_name)) {
            self.last_error = Some(format!(
                "A theme named \"{new_name}\" already exists. Choose a different name."
            ));
            self.focus = ExportThemeField::Name;
            return ExportThemeResponse::Continue;
        }
        ExportThemeResponse::Export { source, new_name }
    }

    fn sync_name_from_selection(&mut self) {
        if self.name_user_edited {
            return;
        }
        let Some(current) = self.selected_theme() else {
            return;
        };
        self.name = default_copy_name(current);
        self.cursor = self.name.chars().count();
        self.name_scroll = 0;
        self.last_error = None;
    }
}

fn default_copy_name(source: &str) -> String {
    format!("{source} copy")
}

pub struct ExportThemeView<'a> {
    pub theme: &'a Theme,
    pub cursor_visible: bool,
}

impl<'a> StatefulWidget for ExportThemeView<'a> {
    type State = ExportThemeState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // pinned_bottom: spacer + label + name row + spacer + button + optional error row.
        let error_row = if state.last_error.is_some() { 1 } else { 0 };
        let pinned_bottom: u16 = 5 + error_row;

        let content_width = {
            let label_w = "Choose an existing theme to export from:".chars().count() as u16;
            let longest_theme = state
                .list
                .items()
                .iter()
                .map(|t| t.chars().count())
                .max()
                .unwrap_or(0) as u16
                + 2; // marker
            let buttons_w = button_row_width(BUTTON_LABELS);
            label_w
                .max(longest_theme)
                .max(NAME_INPUT_WIDTH)
                .max(buttons_w)
                .max(PLACEHOLDER.chars().count() as u16 + 2)
        };

        let total_rows = state.list.visible_len() as u16;
        let geom = anchor_searchable_modal(
            area,
            content_width,
            total_rows,
            MAX_LIST_ROWS,
            PINNED_TOP,
            pinned_bottom,
            0,
        );
        let layout = draw_frame(
            geom.modal_area,
            buf,
            FrameOpts {
                title: "Create custom theme",
                kind: ModalKind::Normal,
                show_close_hint: true,
                content: geom.content,
                theme: self.theme,
            },
        );
        state.esc_button_rect = layout.esc_hit_rect;
        let inner = layout.body;
        if inner.height < PINNED_TOP + pinned_bottom + 1 || inner.width == 0 {
            return;
        }

        Paragraph::new(Line::from(Span::styled(
            "Choose an existing theme to export from:",
            self.theme.modal_section_heading,
        )))
        .style(self.theme.modal_bg)
        .render(
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: 1,
            },
            buf,
        );

        let list_height = inner.height - PINNED_TOP - pinned_bottom;
        let focused_list = matches!(state.focus, ExportThemeField::ThemeList);
        let empty_text = if state.list.items().is_empty() {
            "(no themes available)"
        } else {
            "(no matches)"
        };
        let theme = self.theme;
        state.list.render(
            Rect {
                x: inner.x,
                y: inner.y + 1,
                width: inner.width,
                height: 2 + list_height,
            },
            buf,
            ListChrome {
                theme,
                cursor_visible: self.cursor_visible,
                field_focused: focused_list,
                placeholder: PLACEHOLDER,
                empty_text,
                scrollbar_col: layout.scrollbar_col,
            },
            |ctx| match ctx {
                RowCtx::Item {
                    item,
                    focused,
                    width,
                } => {
                    if focused && !focused_list {
                        Line::from(vec![
                            Span::styled("  ".to_owned(), theme.modal_item),
                            Span::styled(item.clone(), theme.modal_item_selected_unfocused),
                        ])
                    } else {
                        format_modal_row(
                            item,
                            "",
                            focused,
                            false,
                            theme,
                            RowLayout::RightAlign(width),
                        )
                    }
                }
                RowCtx::Header { title, .. } => {
                    Line::from(Span::styled(title.to_owned(), theme.modal_item))
                }
            },
        );

        let mut row_y = inner.y + PINNED_TOP + list_height;
        if row_y < inner.y + inner.height {
            fill_row(buf, inner, row_y, self.theme);
            row_y += 1;
        }
        if row_y < inner.y + inner.height {
            Paragraph::new(Line::from(Span::styled(
                "New theme name:",
                self.theme.modal_close_hint,
            )))
            .style(self.theme.modal_bg)
            .render(
                Rect {
                    x: inner.x,
                    y: row_y,
                    width: inner.width,
                    height: 1,
                },
                buf,
            );
            row_y += 1;
        }
        if row_y < inner.y + inner.height {
            controls::render_text_field_row(
                buf,
                Rect {
                    y: row_y,
                    height: 1,
                    ..inner
                },
                "",
                &state.name,
                state.cursor,
                &mut state.name_scroll,
                matches!(state.focus, ExportThemeField::Name),
                self.cursor_visible,
                self.theme,
            );
            row_y += 1;
        }
        if let Some(err) = state.last_error.as_deref() {
            if row_y < inner.y + inner.height {
                Paragraph::new(Line::from(Span::styled(
                    err.to_owned(),
                    self.theme.transient_error,
                )))
                .alignment(Alignment::Center)
                .style(self.theme.modal_bg)
                .render(
                    Rect {
                        x: inner.x,
                        y: row_y,
                        width: inner.width,
                        height: 1,
                    },
                    buf,
                );
                row_y += 1;
            }
        }
        if row_y < inner.y + inner.height {
            fill_row(buf, inner, row_y, self.theme);
            row_y += 1;
        }
        if row_y < inner.y + inner.height {
            let focused_idx = if matches!(state.focus, ExportThemeField::Export) {
                0
            } else {
                usize::MAX
            };
            render_button_row(
                Rect {
                    x: inner.x,
                    y: row_y,
                    width: inner.width,
                    height: 1,
                },
                buf,
                BUTTON_LABELS,
                focused_idx,
                self.theme,
            );
        }
    }
}

fn fill_row(buf: &mut Buffer, inner: Rect, y: u16, theme: &Theme) {
    Paragraph::new("").style(theme.modal_bg).render(
        Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        },
        buf,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample_themes() -> Vec<String> {
        vec![
            "Ayu".to_owned(),
            "Catppuccin".to_owned(),
            "Edamame".to_owned(),
            "Dracula".to_owned(),
        ]
    }

    /// The name-input row as text, plus how many of its cells carry the focused field fill.
    fn name_row(s: &mut ExportThemeState) -> (String, usize) {
        use ratatui::{backend::TestBackend, Terminal};
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        terminal
            .draw(|f| {
                let view = ExportThemeView {
                    theme,
                    cursor_visible: false,
                };
                f.render_stateful_widget(view, f.area(), s);
            })
            .unwrap();
        let rows = controls::rows_with_fill(terminal.backend().buffer());
        let label = rows
            .iter()
            .position(|(text, _)| text.contains("New theme name:"))
            .expect("the name label");
        rows[label + 1].clone()
    }

    #[test]
    fn the_name_field_fills_its_row_and_scrolls_a_long_name() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        s.focus = ExportThemeField::Name;
        let (_, filled) = name_row(&mut s);
        assert!(
            filled > "Ayu copy".len() + 2,
            "the fill spans the row: {filled}"
        );

        for _ in 0..80 {
            s.handle_key(&key(KeyCode::Char('x')), &[]);
        }
        s.handle_key(&key(KeyCode::Char('Z')), &[]);
        let (text, filled_after) = name_row(&mut s);
        assert_eq!(filled_after, filled, "a long name fills the same width");
        assert!(text.contains("xZ"), "the cursor end is in view: {text}");
        assert!(!text.contains("Ayu"), "the start scrolled away: {text}");
    }

    #[test]
    fn opens_focused_on_list_with_active_selected_and_name_seeded() {
        let s = ExportThemeState::new(sample_themes(), "Edamame");
        assert_eq!(s.focus, ExportThemeField::ThemeList);
        assert_eq!(s.selected_theme().map(String::as_str), Some("Edamame"));
        assert_eq!(s.name, "Edamame copy");
        assert_eq!(s.cursor, "Edamame copy".chars().count());
    }

    #[test]
    fn down_on_list_advances_selection_and_syncs_name() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        s.handle_key(&key(KeyCode::Down), &[]);
        assert_eq!(s.selected_theme().map(String::as_str), Some("Catppuccin"));
        assert_eq!(s.name, "Catppuccin copy");
    }

    #[test]
    fn user_edited_name_is_not_overwritten_by_selection_move() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        s.focus = ExportThemeField::Name;
        s.handle_key(&key(KeyCode::Char('x')), &[]);
        assert_eq!(s.name, "Ayu copyx");
        s.focus = ExportThemeField::ThemeList;
        s.handle_key(&key(KeyCode::Down), &[]);
        assert_eq!(s.name, "Ayu copyx", "user edits must persist");
    }

    #[test]
    fn default_name_does_not_collide_with_source() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        s.handle_key(&key(KeyCode::Enter), &sample_themes());
        assert_eq!(s.focus, ExportThemeField::Name);
        let resp = s.handle_key(&key(KeyCode::Enter), &sample_themes());
        match resp {
            ExportThemeResponse::Export { source, new_name } => {
                assert_eq!(source, "Ayu");
                assert_eq!(new_name, "Ayu copy");
            }
            other => panic!("expected Export, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_list_advances_focus_to_name_instead_of_exporting() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        assert_eq!(s.focus, ExportThemeField::ThemeList);
        let resp = s.handle_key(&key(KeyCode::Enter), &sample_themes());
        assert_eq!(resp, ExportThemeResponse::Continue);
        assert_eq!(s.focus, ExportThemeField::Name);
    }

    #[test]
    fn tab_cycles_focus_forward() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        s.handle_key(&key(KeyCode::Tab), &[]);
        assert_eq!(s.focus, ExportThemeField::Name);
        s.handle_key(&key(KeyCode::Tab), &[]);
        assert_eq!(s.focus, ExportThemeField::Export);
        s.handle_key(&key(KeyCode::Tab), &[]);
        assert_eq!(s.focus, ExportThemeField::ThemeList);
    }

    #[test]
    fn escape_cancels() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        assert_eq!(
            s.handle_key(&key(KeyCode::Esc), &[]),
            ExportThemeResponse::Cancelled
        );
    }

    #[test]
    fn enter_with_unique_name_emits_export() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        s.focus = ExportThemeField::Name;
        for _ in 0.."Ayu copy".chars().count() {
            s.handle_key(&key(KeyCode::Backspace), &[]);
        }
        for c in "My".chars() {
            s.handle_key(&key(KeyCode::Char(c)), &[]);
        }
        let resp = s.handle_key(&key(KeyCode::Enter), &sample_themes());
        match resp {
            ExportThemeResponse::Export { source, new_name } => {
                assert_eq!(source, "Ayu");
                assert_eq!(new_name, "My");
            }
            other => panic!("expected Export, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_name_rejected_with_error() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        s.focus = ExportThemeField::Name;
        for _ in 0.."Ayu copy".chars().count() {
            s.handle_key(&key(KeyCode::Backspace), &[]);
        }
        for c in "Ayu".chars() {
            s.handle_key(&key(KeyCode::Char(c)), &[]);
        }
        let resp = s.handle_key(&key(KeyCode::Enter), &sample_themes());
        assert_eq!(resp, ExportThemeResponse::Continue);
        assert!(s.last_error.is_some());
        assert_eq!(s.focus, ExportThemeField::Name);
    }

    #[test]
    fn empty_name_rejected() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        s.focus = ExportThemeField::Name;
        for _ in 0..10 {
            s.handle_key(&key(KeyCode::Backspace), &[]);
        }
        let resp = s.handle_key(&key(KeyCode::Enter), &sample_themes());
        assert_eq!(resp, ExportThemeResponse::Continue);
        assert!(s.last_error.is_some());
    }

    #[test]
    fn path_separator_in_name_rejected() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        s.focus = ExportThemeField::Name;
        for _ in 0.."Ayu copy".chars().count() {
            s.handle_key(&key(KeyCode::Backspace), &[]);
        }
        for c in "a/b".chars() {
            s.handle_key(&key(KeyCode::Char(c)), &[]);
        }
        let resp = s.handle_key(&key(KeyCode::Enter), &sample_themes());
        assert_eq!(resp, ExportThemeResponse::Continue);
        assert!(s.last_error.is_some());
    }

    #[test]
    fn leading_dots_are_stripped_before_validation() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        s.focus = ExportThemeField::Name;
        for _ in 0.."Ayu copy".chars().count() {
            s.handle_key(&key(KeyCode::Backspace), &[]);
        }
        for c in "..hidden".chars() {
            s.handle_key(&key(KeyCode::Char(c)), &[]);
        }
        let resp = s.handle_key(&key(KeyCode::Enter), &sample_themes());
        match resp {
            ExportThemeResponse::Export { new_name, .. } => assert_eq!(new_name, "hidden"),
            other => panic!("expected Export, got {other:?}"),
        }
    }

    #[test]
    fn dot_only_name_rejected_after_stripping() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        s.focus = ExportThemeField::Name;
        for _ in 0.."Ayu copy".chars().count() {
            s.handle_key(&key(KeyCode::Backspace), &[]);
        }
        for c in "..".chars() {
            s.handle_key(&key(KeyCode::Char(c)), &[]);
        }
        let resp = s.handle_key(&key(KeyCode::Enter), &sample_themes());
        assert_eq!(resp, ExportThemeResponse::Continue);
        assert!(s.last_error.is_some());
    }

    #[test]
    fn typing_on_list_filters_themes() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        assert_eq!(s.focus, ExportThemeField::ThemeList);
        for c in "drac".chars() {
            s.handle_key(&key(KeyCode::Char(c)), &[]);
        }
        assert_eq!(s.selected_theme().map(String::as_str), Some("Dracula"));
    }

    #[test]
    fn filter_reseeds_name_and_selection() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        for c in "cat".chars() {
            s.handle_key(&key(KeyCode::Char(c)), &[]);
        }
        assert_eq!(s.selected_theme().map(String::as_str), Some("Catppuccin"));
        assert_eq!(s.name, "Catppuccin copy");
    }

    #[test]
    fn filter_does_not_clobber_user_edited_name() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        s.focus = ExportThemeField::Name;
        s.handle_key(&key(KeyCode::Char('!')), &[]);
        assert_eq!(s.name, "Ayu copy!");
        s.focus = ExportThemeField::ThemeList;
        for c in "drac".chars() {
            s.handle_key(&key(KeyCode::Char(c)), &[]);
        }
        assert_eq!(s.name, "Ayu copy!", "user-edited name survives filtering");
    }

    #[test]
    fn export_uses_filtered_selection() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        for c in "drac".chars() {
            s.handle_key(&key(KeyCode::Char(c)), &[]);
        }
        s.handle_key(&key(KeyCode::Enter), &sample_themes());
        let resp = s.handle_key(&key(KeyCode::Enter), &sample_themes());
        match resp {
            ExportThemeResponse::Export { source, new_name } => {
                assert_eq!(source, "Dracula");
                assert_eq!(new_name, "Dracula copy");
            }
            other => panic!("expected Export, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_name_check_is_case_insensitive() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        s.focus = ExportThemeField::Name;
        for _ in 0.."Ayu copy".chars().count() {
            s.handle_key(&key(KeyCode::Backspace), &[]);
        }
        for c in "ayu".chars() {
            s.handle_key(&key(KeyCode::Char(c)), &[]);
        }
        let resp = s.handle_key(&key(KeyCode::Enter), &sample_themes());
        assert_eq!(resp, ExportThemeResponse::Continue);
        assert!(s.last_error.is_some());
    }
}
