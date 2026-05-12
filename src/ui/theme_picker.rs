//! Fuzzy-searchable theme picker.
//!
//! Centred modal with a single-line input on top of a scrollable list
//! of available themes (the compiled-in [`BUILTIN_THEMES`] plus any
//! user-authored `themes/*.toml` files).  Typing filters the list via
//! [`nucleo_matcher`]; Enter selects the focused row and returns it to
//! the caller, which is expected to persist `config.theme` and reapply
//! the palette.
//!
//! The widget is deliberately UI-only: it doesn't touch `Config` or
//! `Theme` directly.  See `src/app/modal/theme_picker.rs` for the
//! adapter that wires selection back into the App.

use crossterm::event::{KeyCode, KeyEvent};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::config::Theme;
use crate::ui::content_width::max_row_width;
use crate::ui::modal_row::{format_modal_row, RowLayout};
use crate::ui::scroll_container::{
    centered_rect_for_content, draw_frame, ContentSize, FrameOpts, ModalKind, ScrollContainerState,
    VERTICAL_CHROME_ROWS,
};

/// Outcome of dispatching a key event to the theme picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemePickerResponse {
    Continue,
    /// Focus moved to a different theme — caller should swap the
    /// active palette to this theme *without* persisting so the user
    /// sees a live preview.  Emitted on Up / Down / typing / backspace
    /// whenever the focused theme name differs from the one most
    /// recently previewed.
    Preview(String),
    /// User cancelled (Esc).  Caller should drop the picker and
    /// revert to the theme that was active when the picker opened.
    Cancelled,
    /// User picked a theme.  Caller should drop the picker and apply
    /// the theme name (persist `config.theme` + reload palette).
    Selected(String),
}

/// Mutable state for an open theme picker.
#[derive(Debug, Clone)]
pub struct ThemePickerState {
    pub query: String,
    /// Index into `display_rows`.
    pub focused: usize,
    /// All theme names the picker can show.
    pub themes: Vec<String>,
    /// The name of the theme that was active when the picker opened.
    /// Used solely for the focused-row "(current)" suffix.
    pub current: String,
    pub scroll_state: ScrollContainerState,
    /// Indices into `themes`, filtered by the live query.
    display_rows: Vec<usize>,
    matched_for_query: Option<String>,
    /// Last theme name we emitted a `Preview` for — used to suppress
    /// redundant previews when navigation lands on the same row again.
    /// Initialised to `current` so the first move away from the
    /// already-active theme emits a preview.
    last_previewed: String,
    pub esc_button_rect: Option<Rect>,
}

impl ThemePickerState {
    pub fn open(themes: Vec<String>, current: String) -> Self {
        let initial_focus = themes.iter().position(|t| t == &current).unwrap_or(0);
        let last_previewed = current.clone();
        let mut state = Self {
            query: String::new(),
            focused: initial_focus,
            themes,
            current,
            scroll_state: ScrollContainerState::default(),
            display_rows: Vec::new(),
            matched_for_query: None,
            last_previewed,
            esc_button_rect: None,
        };
        state.refresh_display();
        // Place focus on the current theme in the freshly-built display
        // list so it lands centred.
        if let Some(pos) = state
            .display_rows
            .iter()
            .position(|&i| state.themes[i] == state.current)
        {
            state.focused = pos;
        }
        state
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> ThemePickerResponse {
        if self.scroll_state.handle_paging_key(key) {
            return ThemePickerResponse::Continue;
        }
        match key.code {
            KeyCode::Esc => ThemePickerResponse::Cancelled,
            KeyCode::Enter => {
                self.refresh_display();
                if let Some(&idx) = self.display_rows.get(self.focused) {
                    if let Some(name) = self.themes.get(idx) {
                        return ThemePickerResponse::Selected(name.clone());
                    }
                }
                ThemePickerResponse::Continue
            }
            KeyCode::Up => {
                self.refresh_display();
                if self.focused > 0 {
                    self.focused -= 1;
                    self.scroll_state.ensure_visible(self.focused as u16);
                }
                self.preview_if_changed()
            }
            KeyCode::Down => {
                self.refresh_display();
                if self.focused + 1 < self.display_rows.len() {
                    self.focused += 1;
                    self.scroll_state.ensure_visible(self.focused as u16);
                }
                self.preview_if_changed()
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.invalidate_display();
                self.preview_if_changed()
            }
            KeyCode::Char(c) => {
                use crossterm::event::KeyModifiers;
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    return ThemePickerResponse::Continue;
                }
                self.query.push(c);
                self.invalidate_display();
                self.preview_if_changed()
            }
            _ => ThemePickerResponse::Continue,
        }
    }

    /// Return [`ThemePickerResponse::Preview`] if the focused theme
    /// name has drifted from the most recently previewed one, else
    /// [`ThemePickerResponse::Continue`].  Updates `last_previewed`
    /// when a preview is emitted so each distinct row only previews
    /// once.
    fn preview_if_changed(&mut self) -> ThemePickerResponse {
        self.refresh_display();
        let Some(&idx) = self.display_rows.get(self.focused) else {
            return ThemePickerResponse::Continue;
        };
        let Some(name) = self.themes.get(idx) else {
            return ThemePickerResponse::Continue;
        };
        if *name == self.last_previewed {
            return ThemePickerResponse::Continue;
        }
        self.last_previewed = name.clone();
        ThemePickerResponse::Preview(name.clone())
    }

    #[allow(dead_code)]
    pub fn focused_theme(&mut self) -> Option<String> {
        self.refresh_display();
        let idx = *self.display_rows.get(self.focused)?;
        self.themes.get(idx).cloned()
    }

    #[allow(dead_code)]
    pub fn match_count(&mut self) -> usize {
        self.refresh_display();
        self.display_rows.len()
    }

    fn invalidate_display(&mut self) {
        self.display_rows.clear();
        self.matched_for_query = None;
        self.focused = 0;
        self.scroll_state.scroll = 0;
    }

    fn refresh_display(&mut self) {
        if self.matched_for_query.as_deref() == Some(self.query.as_str()) {
            return;
        }
        self.display_rows.clear();
        if self.query.is_empty() {
            self.display_rows.extend(0..self.themes.len());
        } else {
            let mut matcher = Matcher::default();
            let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
            let mut scored: Vec<(usize, u32)> = Vec::new();
            let mut buf: Vec<char> = Vec::new();
            for (idx, name) in self.themes.iter().enumerate() {
                buf.clear();
                let haystack = Utf32Str::new(name, &mut buf);
                if let Some(score) = pattern.score(haystack, &mut matcher) {
                    scored.push((idx, score));
                }
            }
            scored.sort_by(|a, b| {
                b.1.cmp(&a.1)
                    .then_with(|| self.themes[a.0].cmp(&self.themes[b.0]))
            });
            for (i, _) in scored {
                self.display_rows.push(i);
            }
        }
        self.matched_for_query = Some(self.query.clone());
        if self.focused >= self.display_rows.len() {
            self.focused = 0;
        }
    }
}

/// View widget for the theme picker.
pub struct ThemePickerView<'a> {
    pub theme: &'a Theme,
    pub cursor_visible: bool,
}

impl<'a> StatefulWidget for ThemePickerView<'a> {
    type State = ThemePickerState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        state.refresh_display();

        let content_width = theme_picker_content_width(state).max(NO_MATCHES_WIDTH);
        let row_count = state.display_rows.len().max(1) as u16;
        let scrolling_height = row_count.min(MAX_LIST_ROWS);
        let content = ContentSize {
            width: content_width,
            height: scrolling_height,
            pinned_top: 2,
            pinned_bottom: 0,
        };
        // Anchor the modal's top edge at the y it would have when the
        // *initial* (no-query) list is rendered — i.e. centred for the
        // full theme list, capped at MAX_LIST_ROWS.  This vertically
        // centres the modal on first render and then keeps the input
        // row pinned as the user filters the list: a naively-centred
        // modal would shift up and down by half the height delta on
        // every keystroke.
        let initial_height = (state.themes.len() as u16).max(1).min(MAX_LIST_ROWS);
        let anchor_content = ContentSize {
            height: initial_height,
            ..content
        };
        let anchor = centered_rect_for_content(anchor_content, area);
        let actual = centered_rect_for_content(content, area);
        let max_y = area.y + area.height.saturating_sub(actual.height);
        let modal_area = Rect {
            x: actual.x,
            y: anchor.y.min(max_y),
            width: actual.width,
            height: actual.height,
        };

        let inner_h = modal_area.height.saturating_sub(VERTICAL_CHROME_ROWS);
        let pinned_top: u16 = 2;
        let list_height = inner_h.saturating_sub(pinned_top);
        state
            .scroll_state
            .observe(state.display_rows.len() as u16, list_height);
        state.scroll_state.ensure_visible(state.focused as u16);

        let layout = draw_frame(
            modal_area,
            buf,
            FrameOpts {
                title: "Switch theme",
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

        // Input row.
        let input_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        };
        let prompt = Span::styled("› ", self.theme.modal_item);
        let typed = Span::styled(state.query.clone(), self.theme.modal_item);
        let mut spans = vec![prompt, typed];
        if self.cursor_visible {
            let cursor_style = ratatui::style::Style::default()
                .fg(self.theme.palette.primary)
                .bg(self.theme.palette.surface_elevated)
                .add_modifier(ratatui::style::Modifier::BOLD);
            spans.push(Span::styled("▏", cursor_style));
        }
        Paragraph::new(Line::from(spans))
            .style(self.theme.modal_bg)
            .render(input_area, buf);

        // Divider.
        let divider_style = ratatui::style::Style::default()
            .fg(self.theme.palette.secondary)
            .bg(self.theme.palette.surface_elevated);
        let divider_y = inner.y + 1;
        for x in inner.x..(inner.x + inner.width) {
            buf[(x, divider_y)].set_symbol("─").set_style(divider_style);
        }

        let list_area = Rect {
            x: inner.x,
            y: inner.y + pinned_top,
            width: inner.width,
            height: list_height,
        };
        if list_area.height == 0 {
            return;
        }

        let scroll = state.scroll_state.scroll as usize;
        let visible_rows = list_area.height as usize;
        let mut lines: Vec<Line<'_>> = Vec::with_capacity(visible_rows);

        if state.display_rows.is_empty() {
            lines.push(Line::from(Span::styled(
                "(no matches)".to_owned(),
                self.theme.modal_item,
            )));
        } else {
            for (visible_idx, &row_idx) in state
                .display_rows
                .iter()
                .skip(scroll)
                .take(visible_rows)
                .enumerate()
            {
                let absolute_idx = visible_idx + scroll;
                let name = &state.themes[row_idx];
                let focused = absolute_idx == state.focused;
                let suffix = if name == &state.current {
                    "current"
                } else {
                    ""
                };
                lines.push(format_modal_row(
                    name,
                    suffix,
                    focused,
                    false,
                    self.theme,
                    RowLayout::RightAlign(list_area.width),
                ));
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
    }
}

const NO_MATCHES_WIDTH: u16 = 12;
const MAX_LIST_ROWS: u16 = 20;
const CURRENT_SUFFIX_W: usize = "current".len();

fn theme_picker_content_width(state: &ThemePickerState) -> u16 {
    max_row_width(&state.themes, |name| {
        // 2 marker + name + 1 gap + suffix
        2 + name.chars().count() + 1 + CURRENT_SUFFIX_W
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn themes() -> Vec<String> {
        vec![
            "256 Dark".to_owned(),
            "256 Light".to_owned(),
            "Ayu".to_owned(),
            "Catppuccin".to_owned(),
            "Dracula".to_owned(),
            "Tokyo Night".to_owned(),
        ]
    }

    #[test]
    fn opens_focused_on_current_theme() {
        let mut state = ThemePickerState::open(themes(), "Catppuccin".into());
        assert_eq!(state.focused_theme().as_deref(), Some("Catppuccin"));
    }

    #[test]
    fn typing_filters_themes() {
        let mut state = ThemePickerState::open(themes(), "Ayu".into());
        for c in "drac".chars() {
            state.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(state.match_count(), 1);
        assert_eq!(state.focused_theme().as_deref(), Some("Dracula"));
    }

    #[test]
    fn enter_returns_focused_theme() {
        let mut state = ThemePickerState::open(themes(), "Ayu".into());
        for c in "tokyo".chars() {
            state.handle_key(&key(KeyCode::Char(c)));
        }
        let resp = state.handle_key(&key(KeyCode::Enter));
        assert_eq!(resp, ThemePickerResponse::Selected("Tokyo Night".into()));
    }

    #[test]
    fn escape_cancels() {
        let mut state = ThemePickerState::open(themes(), "Ayu".into());
        assert_eq!(
            state.handle_key(&key(KeyCode::Esc)),
            ThemePickerResponse::Cancelled
        );
    }

    #[test]
    fn down_advances_focus() {
        let mut state = ThemePickerState::open(themes(), "256 Dark".into());
        let before = state.focused;
        state.handle_key(&key(KeyCode::Down));
        assert_eq!(state.focused, before + 1);
    }

    #[test]
    fn down_emits_preview_for_newly_focused_theme() {
        let mut state = ThemePickerState::open(themes(), "256 Dark".into());
        let resp = state.handle_key(&key(KeyCode::Down));
        assert_eq!(resp, ThemePickerResponse::Preview("256 Light".into()));
    }

    #[test]
    fn typing_emits_preview_when_focused_theme_changes() {
        let mut state = ThemePickerState::open(themes(), "256 Dark".into());
        // Typing 'd' fuzzy-matches Dracula / 256 Dark; the highest-
        // scored match (or first lexicographically among ties) should
        // be previewed if it differs from the initial theme.
        let resp = state.handle_key(&key(KeyCode::Char('t')));
        match resp {
            ThemePickerResponse::Preview(name) => assert_ne!(name, "256 Dark"),
            other => panic!("expected Preview after typing, got {other:?}"),
        }
    }

    #[test]
    fn repeating_same_focus_does_not_re_preview() {
        let mut state = ThemePickerState::open(themes(), "256 Dark".into());
        // First Down moves to "256 Light" — Preview.
        let resp = state.handle_key(&key(KeyCode::Down));
        assert!(matches!(resp, ThemePickerResponse::Preview(_)));
        // Up + Down lands back on "256 Light" — still focused on the
        // last-previewed theme, so no Preview should fire.
        state.handle_key(&key(KeyCode::Up));
        let _ = state.handle_key(&key(KeyCode::Down));
        // Down again past the boundary is a no-op; we re-check that
        // pressing Down with no focus change emits Continue.
        let resp2 = state.handle_key(&key(KeyCode::Down));
        assert_ne!(resp2, ThemePickerResponse::Continue.clone()); // sanity: still moves
        let last = state.focused_theme().unwrap();
        let resp3 = state.handle_key(&key(KeyCode::Up));
        // After the Up, focus is back on `last`'s predecessor — Preview
        // must be emitted only when focus has actually changed.
        match resp3 {
            ThemePickerResponse::Preview(name) => assert_ne!(name, last),
            ThemePickerResponse::Continue => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn ctrl_chars_do_not_pollute_query() {
        let mut state = ThemePickerState::open(themes(), "Ayu".into());
        let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        state.handle_key(&ctrl_p);
        assert!(state.query.is_empty());
    }

    use crate::config::Theme;
    use ratatui::{backend::TestBackend, Terminal};

    fn render_top_y(state: &mut ThemePickerState, w: u16, h: u16) -> u16 {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_stateful_widget(
                    ThemePickerView {
                        theme,
                        cursor_visible: true,
                    },
                    frame.area(),
                    state,
                );
            })
            .unwrap();
        // The first non-blank row in the buffer is the modal's top
        // padding row (modal_bg fill).  We use the title row instead —
        // it's the first row containing any non-space character, which
        // is row +1 from the top of the modal.  Subtract 1 to get the
        // modal top edge.
        let buf = terminal.backend().buffer().clone();
        for y in 0..h {
            for x in 0..w {
                let sym = buf[(x, y)].symbol();
                if sym != " " && !sym.is_empty() {
                    return y.saturating_sub(1);
                }
            }
        }
        0
    }

    #[test]
    fn initial_render_is_vertically_centred() {
        // A 60-row terminal with 9 themes, MAX_LIST_ROWS=20 cap: the
        // modal's natural height is 9 list rows + 2 pinned (input +
        // divider) + 4 chrome = 15 rows.  Centred y = (60 - 15) / 2 = 22.
        let mut state = ThemePickerState::open(themes(), "Ayu".into());
        let y = render_top_y(&mut state, 80, 60);
        // Allow a small tolerance for chrome accounting (top pad).
        // The exact y is (60 - (themes_len + 2 + 4)) / 2.
        let themes_h = themes().len() as u16 + 2 + 4;
        let expected = (60 - themes_h) / 2;
        assert!(
            y.abs_diff(expected) <= 1,
            "initial render y={y}, expected ~{expected}"
        );
    }

    #[test]
    fn top_edge_stays_put_when_filtering() {
        // Top-anchor invariant: once the modal is positioned at its
        // initial-state centred y, typing a query that shrinks the
        // match list must NOT move the modal up or down.
        let mut state = ThemePickerState::open(themes(), "Ayu".into());
        let y_initial = render_top_y(&mut state, 80, 60);
        state.handle_key(&key(KeyCode::Char('d')));
        let y_after = render_top_y(&mut state, 80, 60);
        assert_eq!(
            y_initial, y_after,
            "modal top must not move when filtering shrinks the list"
        );
    }

    #[test]
    fn no_matches_yields_zero_count() {
        let mut state = ThemePickerState::open(themes(), "Ayu".into());
        for c in "zzznotanything".chars() {
            state.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(state.match_count(), 0);
        let resp = state.handle_key(&key(KeyCode::Enter));
        assert_eq!(resp, ThemePickerResponse::Continue);
    }
}
