//! Export a theme to a user-editable `.toml` file.
//!
//! Three focus targets: a scrollable theme-list picker (defaults to
//! the active theme), a single-line name input (defaults to the
//! selected theme's name), and an `Export` button.  Tab / Shift-Tab
//! cycles focus; Up/Down inside the list moves selection and inside
//! the input or button moves focus to the previous/next target.
//!
//! UI-only — the adapter in `app/modal/export_theme.rs` writes the
//! resulting `<name>.toml` and applies the new theme.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::config::Theme;
use crate::ui::button_row::{button_row_width, render_button_row};
use crate::ui::modal_row::{format_modal_row, RowLayout};
use crate::ui::scroll_container::{
    centered_rect_for_content, draw_frame, ContentSize, FrameOpts, ModalKind, ScrollContainerState,
};

const BUTTON_LABELS: &[&str] = &["Export"];
const MAX_LIST_ROWS: u16 = 12;
/// Fixed total width of the name-input row (including both padding
/// cells).  Keeps the modal a stable size as the user types — long
/// values scroll horizontally inside the input rather than widening
/// the modal frame.
const NAME_INPUT_WIDTH: u16 = 28;

/// Focus targets in the modal.
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
    /// Validation against existing theme names happens here in the
    /// UI; on collision the modal stays open with `last_error` set.
    Export {
        source: String,
        new_name: String,
    },
}

#[derive(Debug, Clone)]
pub struct ExportThemeState {
    pub themes: Vec<String>,
    /// Currently-selected theme in the list.
    pub selected: usize,
    /// New name entered by the user.
    pub name: String,
    /// Character-index cursor into `name`.
    pub cursor: usize,
    pub focus: ExportThemeField,
    pub last_error: Option<String>,
    pub scroll_state: ScrollContainerState,
    pub esc_button_rect: Option<Rect>,
    /// Flipped to `true` the first time the user types/backspaces in
    /// the Name field.  Once set, moving the list selection no longer
    /// re-syncs the Name field — the user's edit wins.
    name_user_edited: bool,
    /// Horizontal scroll offset (in chars) for the name input.
    /// Updated at render time so the cursor stays in view when the
    /// value is wider than the input area.
    name_scroll: usize,
}

impl ExportThemeState {
    pub fn new(themes: Vec<String>, active: &str) -> Self {
        let selected = themes.iter().position(|t| t == active).unwrap_or(0);
        let initial_name = themes
            .get(selected)
            .map(|t| default_copy_name(t))
            .unwrap_or_default();
        let cursor = initial_name.chars().count();
        Self {
            themes,
            selected,
            name: initial_name,
            cursor,
            focus: ExportThemeField::ThemeList,
            last_error: None,
            scroll_state: ScrollContainerState::default(),
            esc_button_rect: None,
            name_user_edited: false,
            name_scroll: 0,
        }
    }

    /// Apply a key event.
    pub fn handle_key(&mut self, key: &KeyEvent, existing: &[String]) -> ExportThemeResponse {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return ExportThemeResponse::Continue;
        }

        // PageUp/PgDn/Home/End drive list scroll only when the list
        // has focus.
        if matches!(self.focus, ExportThemeField::ThemeList)
            && self.scroll_state.handle_paging_key(key)
        {
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
            KeyCode::Up => match self.focus {
                ExportThemeField::ThemeList => {
                    if self.selected > 0 {
                        self.selected -= 1;
                        self.scroll_state.ensure_visible(self.selected as u16);
                        self.sync_name_from_selection();
                    }
                    ExportThemeResponse::Continue
                }
                _ => {
                    self.focus = self.focus.prev();
                    ExportThemeResponse::Continue
                }
            },
            KeyCode::Down => match self.focus {
                ExportThemeField::ThemeList => {
                    if self.selected + 1 < self.themes.len() {
                        self.selected += 1;
                        self.scroll_state.ensure_visible(self.selected as u16);
                        self.sync_name_from_selection();
                    }
                    ExportThemeResponse::Continue
                }
                _ => {
                    self.focus = self.focus.next();
                    ExportThemeResponse::Continue
                }
            },
            KeyCode::Left => match self.focus {
                ExportThemeField::Name => {
                    self.cursor = self.cursor.saturating_sub(1);
                    ExportThemeResponse::Continue
                }
                _ => ExportThemeResponse::Continue,
            },
            KeyCode::Right => match self.focus {
                ExportThemeField::Name => {
                    let len = self.name.chars().count();
                    if self.cursor < len {
                        self.cursor += 1;
                    }
                    ExportThemeResponse::Continue
                }
                _ => ExportThemeResponse::Continue,
            },
            KeyCode::Home if matches!(self.focus, ExportThemeField::Name) => {
                self.cursor = 0;
                ExportThemeResponse::Continue
            }
            KeyCode::End if matches!(self.focus, ExportThemeField::Name) => {
                self.cursor = self.name.chars().count();
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
            KeyCode::Enter => match self.focus {
                // Enter on the picker advances to the Name field
                // rather than firing Export — gives the user a chance
                // to rename before committing.
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

    /// Insert a bracketed paste into the Name field at the cursor.
    /// No-op unless the Name field is focused.  The paste is flattened
    /// to one line and length-capped by [`crate::ui::sanitize_paste`].
    pub fn paste(&mut self, text: &str) {
        if !matches!(self.focus, ExportThemeField::Name) {
            return;
        }
        let clean = crate::ui::sanitize_paste(text);
        if clean.is_empty() {
            return;
        }
        for c in clean.chars() {
            insert_char_at(&mut self.name, self.cursor, c);
            self.cursor += 1;
        }
        self.last_error = None;
        self.name_user_edited = true;
    }

    fn try_export(&mut self, existing: &[String]) -> ExportThemeResponse {
        let Some(source) = self.themes.get(self.selected).cloned() else {
            self.last_error = Some("No theme selected".to_owned());
            return ExportThemeResponse::Continue;
        };
        // Strip whitespace and leading dots so the resulting file name
        // can't be `.`, `..`, or a hidden dotfile.
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
        let Some(current) = self.themes.get(self.selected) else {
            return;
        };
        self.name = default_copy_name(current);
        self.cursor = self.name.chars().count();
        self.name_scroll = 0;
        self.last_error = None;
    }
}

/// Default name for a copied theme: `"{source} copy"`.  Chosen so
/// pressing Enter on the seeded value doesn't immediately collide
/// with the source theme's name.
fn default_copy_name(source: &str) -> String {
    format!("{source} copy")
}

fn char_byte_index(s: &str, cursor: usize) -> usize {
    s.char_indices()
        .nth(cursor)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

fn insert_char_at(s: &mut String, cursor: usize, ch: char) {
    let byte_idx = char_byte_index(s, cursor);
    s.insert(byte_idx, ch);
}

fn remove_char_at(s: &mut String, cursor: usize) {
    if let Some((byte_idx, ch)) = s.char_indices().nth(cursor) {
        s.replace_range(byte_idx..byte_idx + ch.len_utf8(), "");
    }
}

pub struct ExportThemeView<'a> {
    pub theme: &'a Theme,
    pub cursor_visible: bool,
}

impl<'a> StatefulWidget for ExportThemeView<'a> {
    type State = ExportThemeState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Layout:
        //   pinned_top: heading row
        //   scrolling: theme list (capped at MAX_LIST_ROWS)
        //   pinned_bottom: spacer + label + name row + spacer
        //                + button row + (optional) error row.
        let list_total = state.themes.len().max(1) as u16;
        let list_visible = list_total.min(MAX_LIST_ROWS);
        let error_row = if state.last_error.is_some() { 1 } else { 0 };
        let pinned_bottom: u16 = 5 + error_row;

        let content_width = {
            let label_w = "Choose an existing theme to export from:".chars().count() as u16;
            let longest_theme = state
                .themes
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
        };

        let content = ContentSize {
            width: content_width,
            height: list_visible,
            pinned_top: 1,
            pinned_bottom,
            ..Default::default()
        };
        let modal_area = centered_rect_for_content(content, area);
        let layout = draw_frame(
            modal_area,
            buf,
            FrameOpts {
                title: "Create custom theme",
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

        // Heading row.
        let heading_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        };
        Paragraph::new(Line::from(Span::styled(
            "Choose an existing theme to export from:",
            self.theme.modal_section_heading,
        )))
        .style(self.theme.modal_bg)
        .render(heading_area, buf);

        // List area.
        let inner_h = inner.height;
        if inner_h < 1 + pinned_bottom {
            return;
        }
        let list_height = inner_h.saturating_sub(1 + pinned_bottom);
        state
            .scroll_state
            .observe(state.themes.len() as u16, list_height);
        state.scroll_state.ensure_visible(state.selected as u16);

        let list_area = Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: list_height,
        };
        let scroll = state.scroll_state.scroll as usize;
        let visible_rows = list_area.height as usize;
        let focused_list = matches!(state.focus, ExportThemeField::ThemeList);
        let mut lines: Vec<Line<'_>> = Vec::with_capacity(visible_rows);
        if state.themes.is_empty() {
            lines.push(Line::from(Span::styled(
                "(no themes available)".to_owned(),
                self.theme.modal_item,
            )));
        } else {
            for row_idx in (scroll..state.themes.len()).take(visible_rows) {
                let name = &state.themes[row_idx];
                let is_selected = row_idx == state.selected;
                // When the list itself isn't focused, render the
                // current selection in the "selected but unfocused"
                // style (outlined, not filled) per theming.md.
                if is_selected && !focused_list {
                    let marker = "  ";
                    lines.push(Line::from(vec![
                        Span::styled(marker.to_owned(), self.theme.modal_item),
                        Span::styled(name.clone(), self.theme.modal_item_selected_unfocused),
                    ]));
                } else {
                    lines.push(format_modal_row(
                        name,
                        "",
                        is_selected,
                        false,
                        self.theme,
                        RowLayout::RightAlign(list_area.width),
                    ));
                }
            }
        }
        Paragraph::new(lines)
            .style(self.theme.modal_bg)
            .render(list_area, buf);

        if state.scroll_state.max_scroll() > 0 {
            let bar_area = Rect {
                x: layout.scrollbar_col,
                y: list_area.y,
                width: 1,
                height: list_area.height,
            };
            crate::ui::scrollbar::render_for_scroll_state(
                bar_area,
                &state.scroll_state,
                self.theme,
                buf,
            );
        }

        let mut row_y = inner.y + 1 + list_height;
        // Spacer.
        if row_y < inner.y + inner.height {
            Paragraph::new("").style(self.theme.modal_bg).render(
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

        // Label for the name input.
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

        // Name input row.
        if row_y < inner.y + inner.height {
            render_name_row(
                buf,
                inner,
                row_y,
                &state.name,
                state.cursor,
                &mut state.name_scroll,
                matches!(state.focus, ExportThemeField::Name),
                self.theme,
                self.cursor_visible,
            );
            row_y += 1;
        }

        // Optional error row.
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

        // Spacer before button row.
        if row_y < inner.y + inner.height {
            Paragraph::new("").style(self.theme.modal_bg).render(
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

        // Button row.
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

#[allow(clippy::too_many_arguments)]
fn render_name_row(
    buf: &mut Buffer,
    inner: Rect,
    y: u16,
    value: &str,
    cursor: usize,
    name_scroll: &mut usize,
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
    let value_style = if focused {
        theme.modal_input_focused
    } else {
        theme.modal_input_unfocused
    };
    let total = value.chars().count();
    let mut spans: Vec<Span<'_>> = Vec::with_capacity(6);
    spans.push(Span::styled(" ", value_style));

    if focused {
        // Layout in the input area: 1 leading-padding cell + N
        // character cells + 1 cursor-glyph cell + 1 trailing-padding
        // cell.  So `visible` characters fit when inner.width = N + 3.
        let visible = (inner.width as usize).saturating_sub(3);
        // Keep the cursor inside [scroll, scroll + visible].
        if cursor < *name_scroll {
            *name_scroll = cursor;
        } else if visible > 0 && cursor > *name_scroll + visible {
            *name_scroll = cursor - visible;
        }
        // Avoid blank trailing space when the value shrinks: clamp
        // scroll so we always fill the window when possible.
        let max_scroll = total.saturating_sub(visible);
        if *name_scroll > max_scroll {
            *name_scroll = max_scroll;
        }
        let start = *name_scroll;
        let end = (start + visible).min(total);

        let pre_end = cursor.min(end);
        let post_start = cursor.max(start);
        let pre: String = value.chars().skip(start).take(pre_end - start).collect();
        let post: String = value
            .chars()
            .skip(post_start)
            .take(end - post_start)
            .collect();

        if !pre.is_empty() {
            spans.push(Span::styled(pre, value_style));
        }
        if cursor_visible {
            spans.push(Span::styled("▏", theme.cursor));
        }
        if !post.is_empty() {
            spans.push(Span::styled(post, value_style));
        }
        spans.push(Span::styled(" ", value_style));
    } else {
        // Unfocused: no cursor cell to reserve.
        let visible = (inner.width as usize).saturating_sub(2);
        let shown: String = value.chars().take(visible).collect();
        spans.push(Span::styled(shown, value_style));
        spans.push(Span::styled(" ", value_style));
    }
    Paragraph::new(Line::from(spans))
        .style(theme.modal_bg)
        .render(area, buf);
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

    #[test]
    fn opens_focused_on_list_with_active_selected_and_name_seeded() {
        let s = ExportThemeState::new(sample_themes(), "Edamame");
        assert_eq!(s.focus, ExportThemeField::ThemeList);
        assert_eq!(s.themes[s.selected], "Edamame");
        assert_eq!(s.name, "Edamame copy");
        assert_eq!(s.cursor, "Edamame copy".chars().count());
    }

    #[test]
    fn down_on_list_advances_selection_and_syncs_name() {
        let mut s = ExportThemeState::new(sample_themes(), "Ayu");
        s.handle_key(&key(KeyCode::Down), &[]);
        assert_eq!(s.themes[s.selected], "Catppuccin");
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
        // First Enter advances from the list to the Name field.
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
        // Rename to something not in the list.  Default seed is
        // "Ayu copy" (8 chars).
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
        // Replace the default "Ayu copy" seed with "Ayu" — a name
        // already in the existing list — to force a collision.
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
            ExportThemeResponse::Export { new_name, .. } => {
                assert_eq!(new_name, "hidden");
            }
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
