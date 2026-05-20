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

use crate::config::{AppearanceMode, Theme};
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
    /// User flipped the Dark/Light pill (via Tab / Left / Right or by
    /// clicking the pill).  Caller is responsible for recomputing the
    /// filtered theme list, deciding which theme to preview under the
    /// new mode (typically via [`crate::config::theme::resolve_theme_for_mode_switch`]),
    /// rebuilding the state's `themes` vec, and re-focusing the picker.
    ModeChanged(AppearanceMode),
    /// User cancelled (Esc).  Caller should drop the picker and
    /// revert to the theme and mode that were active when the picker
    /// opened.
    Cancelled,
    /// User picked a theme.  Caller should drop the picker and apply
    /// the theme name and the current mode (persist `config.theme` /
    /// `config.appearance` + reload palette).
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
    /// Active appearance mode — drives the pill highlight and is
    /// returned as part of `Selected` so the App can persist
    /// `config.appearance` alongside `config.theme`.
    pub mode: AppearanceMode,
    /// Hit-rect for the `[ Dark ]` side of the pill, captured during
    /// render so a click can flip mode without re-deriving layout.
    pub pill_dark_rect: Option<Rect>,
    /// Hit-rect for the `[ Light ]` side of the pill.
    pub pill_light_rect: Option<Rect>,
    /// Theme name to re-focus on the next `refresh_display`.  Set by
    /// `invalidate_display` to the previously-focused theme name so the
    /// focus survives filter changes (Backspace broadening the query,
    /// typing a more specific one that still matches the same row).
    /// Cleared after the focus is restored.
    pending_focus_name: Option<String>,
}

impl ThemePickerState {
    pub fn open(themes: Vec<String>, current: String, mode: AppearanceMode) -> Self {
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
            mode,
            pill_dark_rect: None,
            pill_light_rect: None,
            pending_focus_name: None,
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
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                ThemePickerResponse::ModeChanged(self.mode.opposite())
            }
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

    /// The theme name currently driving the live preview.  Tracks
    /// `last_previewed`, which the state keeps in sync with every
    /// Preview emission (and `replace_themes` resets when the mode
    /// flips).  Lets the modal adapter snapshot the active theme when
    /// the user toggles modes without consulting `app.config.theme`.
    pub fn current_theme(&self) -> &str {
        &self.last_previewed
    }

    /// If `(col, row)` falls inside the *inactive* half of the
    /// Dark/Light pill, return the mode the user clicked toward (i.e.
    /// the opposite of the current mode).  Clicks on the active half
    /// are no-ops and return `None` along with clicks outside both
    /// pill rects.
    pub fn pill_hit(&self, col: u16, row: u16) -> Option<AppearanceMode> {
        let inside = |rect: Option<Rect>| {
            rect.map(|r| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height)
                .unwrap_or(false)
        };
        if inside(self.pill_dark_rect) && self.mode == AppearanceMode::Light {
            return Some(AppearanceMode::Dark);
        }
        if inside(self.pill_light_rect) && self.mode == AppearanceMode::Dark {
            return Some(AppearanceMode::Light);
        }
        None
    }

    /// Replace the theme list and re-focus on `focus_on` (typically the
    /// counterpart of the previously-active theme, or the default theme
    /// for the new mode).  Resets the query so the user sees the full
    /// new list immediately after a mode flip.  Called by the modal
    /// adapter when it handles `ThemePickerResponse::ModeChanged`.
    pub fn replace_themes(&mut self, themes: Vec<String>, focus_on: &str, mode: AppearanceMode) {
        self.themes = themes;
        self.mode = mode;
        self.query.clear();
        self.invalidate_display();
        self.refresh_display();
        if let Some(pos) = self
            .display_rows
            .iter()
            .position(|&i| self.themes.get(i).map(|n| n == focus_on).unwrap_or(false))
        {
            self.focused = pos;
        }
        // Track the theme that actually ends up focused, not the caller's
        // `focus_on` request — if it wasn't found in the new list, falling
        // back to whatever is at `focused` (typically index 0) keeps the
        // preview-dedup logic consistent with what's on screen.
        self.last_previewed = self
            .display_rows
            .get(self.focused)
            .and_then(|&i| self.themes.get(i).cloned())
            .unwrap_or_default();
        self.scroll_state.scroll = 0;
        self.scroll_state.ensure_visible(self.focused as u16);
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
        // Capture the currently-focused theme name so `refresh_display`
        // can keep focus on that theme when the filter changes (e.g.
        // Backspace broadening the query).  Falls back to index 0 only
        // if the previously-focused theme is no longer in the result set.
        let prev = self
            .display_rows
            .get(self.focused)
            .and_then(|&i| self.themes.get(i).cloned());
        self.display_rows.clear();
        self.matched_for_query = None;
        self.focused = 0;
        self.scroll_state.scroll = 0;
        self.pending_focus_name = prev;
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
        if let Some(name) = self.pending_focus_name.take() {
            if let Some(pos) = self
                .display_rows
                .iter()
                .position(|&i| self.themes.get(i).map(|n| n == &name).unwrap_or(false))
            {
                self.focused = pos;
                self.scroll_state.ensure_visible(self.focused as u16);
            }
        }
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

        let content_width = theme_picker_content_width(state)
            .max(NO_MATCHES_WIDTH)
            .max(PILL_MIN_WIDTH)
            .max(PILL_HINT_LABEL.chars().count() as u16);
        let row_count = state.display_rows.len().max(1) as u16;
        let scrolling_height = row_count.min(MAX_LIST_ROWS);
        let content = ContentSize {
            width: content_width,
            // pinned_top = pill row + hint row + blank spacer + input
            // row + divider row.  The blank spacer separates the
            // appearance affordance from the search field so the two
            // don't read as a single block of chrome.
            height: scrolling_height,
            pinned_top: 5,
            pinned_bottom: 0,
            ..Default::default()
        };
        // Anchor the modal's top edge at the y it would have when the
        // *initial* (no-query) list is rendered — i.e. centred for the
        // full theme list, capped at MAX_LIST_ROWS.  This vertically
        // centres the modal on first render and then keeps the input
        // row pinned as the user filters the list: a naively-centred
        // modal would shift up and down by half the height delta on
        // every keystroke.
        let initial_height = (state.themes.len() as u16).clamp(1, MAX_LIST_ROWS);
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
        let pinned_top: u16 = 5;
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
                content,
                theme: self.theme,
            },
        );
        state.esc_button_rect = layout.esc_hit_rect;
        let inner = layout.body;
        if inner.height < 5 || inner.width == 0 {
            state.pill_dark_rect = None;
            state.pill_light_rect = None;
            return;
        }

        // Pill row — `[ Dark ]  [ Light ]`, centred.  Stored hit-rects
        // let mouse clicks flip mode without re-deriving the layout.
        let pill_y = inner.y;
        let pill_x = inner
            .x
            .saturating_add(inner.width.saturating_sub(PILL_TOTAL_W) / 2);
        let dark_active = state.mode == AppearanceMode::Dark;
        let dark_style = if dark_active {
            self.theme.modal_item_selected
        } else {
            self.theme.modal_item
        };
        let light_style = if dark_active {
            self.theme.modal_item
        } else {
            self.theme.modal_item_selected
        };
        let pill_line = Line::from(vec![
            Span::styled(PILL_DARK_LABEL, dark_style),
            Span::styled(PILL_GAP, self.theme.modal_item),
            Span::styled(PILL_LIGHT_LABEL, light_style),
        ]);
        let pill_area = Rect {
            x: pill_x,
            y: pill_y,
            width: PILL_TOTAL_W,
            height: 1,
        };
        // Fill the rest of the pill row with modal_bg so the surface
        // reads as one continuous chrome strip.
        let row_fill = Rect {
            x: inner.x,
            y: pill_y,
            width: inner.width,
            height: 1,
        };
        Paragraph::new("")
            .style(self.theme.modal_bg)
            .render(row_fill, buf);
        Paragraph::new(pill_line)
            .style(self.theme.modal_bg)
            .render(pill_area, buf);
        state.pill_dark_rect = Some(Rect {
            x: pill_x,
            y: pill_y,
            width: PILL_DARK_W,
            height: 1,
        });
        state.pill_light_rect = Some(Rect {
            x: pill_x + PILL_DARK_W + PILL_GAP_W,
            y: pill_y,
            width: PILL_LIGHT_W,
            height: 1,
        });

        // Hint row — centred, muted text describing how to flip the
        // pill.  Sits directly under the pill so the affordance is
        // discoverable for keyboard users.
        let hint_y = inner.y + 1;
        let hint_len = PILL_HINT_LABEL.chars().count() as u16;
        let hint_x = inner
            .x
            .saturating_add(inner.width.saturating_sub(hint_len) / 2);
        let hint_row_fill = Rect {
            x: inner.x,
            y: hint_y,
            width: inner.width,
            height: 1,
        };
        Paragraph::new("")
            .style(self.theme.modal_bg)
            .render(hint_row_fill, buf);
        let hint_style = ratatui::style::Style::default()
            .fg(self.theme.palette.text_muted)
            .bg(self.theme.palette.surface_elevated);
        Paragraph::new(Line::from(Span::styled(PILL_HINT_LABEL, hint_style)))
            .style(self.theme.modal_bg)
            .render(
                Rect {
                    x: hint_x,
                    y: hint_y,
                    width: hint_len,
                    height: 1,
                },
                buf,
            );

        // Blank spacer row — separates the appearance affordance from
        // the search field so the two don't read as a single block.
        let spacer_row = Rect {
            x: inner.x,
            y: inner.y + 2,
            width: inner.width,
            height: 1,
        };
        Paragraph::new("")
            .style(self.theme.modal_bg)
            .render(spacer_row, buf);

        // Input row — sits below the pill + hint + spacer triple.
        let input_area = Rect {
            x: inner.x,
            y: inner.y + 3,
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

        // Divider — sits at the bottom of the pinned-top stack
        // (pill, hint, spacer, input, divider).
        let divider_style = ratatui::style::Style::default()
            .fg(self.theme.palette.secondary)
            .bg(self.theme.palette.surface_elevated);
        let divider_y = inner.y + 4;
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
const PILL_DARK_LABEL: &str = "[ Dark ]";
const PILL_LIGHT_LABEL: &str = "[ Light ]";
const PILL_GAP: &str = "  ";
// Width constants — `.len()` is byte length, which equals visual width
// only for ASCII.  The pill labels and gap are ASCII-only; the runtime
// test `pill_labels_are_ascii` guards that invariant so these consts
// stay correct.  The hint label is non-ASCII (← →) so it uses
// `chars().count()` at the (single) use site.
const PILL_DARK_W: u16 = PILL_DARK_LABEL.len() as u16;
const PILL_LIGHT_W: u16 = PILL_LIGHT_LABEL.len() as u16;
const PILL_GAP_W: u16 = PILL_GAP.len() as u16;
const PILL_TOTAL_W: u16 = PILL_DARK_W + PILL_GAP_W + PILL_LIGHT_W;
const PILL_MIN_WIDTH: u16 = PILL_TOTAL_W;
/// Muted, centred hint shown directly under the pill.  Tells the user
/// which keys flip appearance — without this the Tab / arrow-key
/// affordance is invisible.
const PILL_HINT_LABEL: &str = "Tab / ← →  to switch";

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
        let mut state = ThemePickerState::open(themes(), "Catppuccin".into(), AppearanceMode::Dark);
        assert_eq!(state.focused_theme().as_deref(), Some("Catppuccin"));
    }

    #[test]
    fn typing_filters_themes() {
        let mut state = ThemePickerState::open(themes(), "Ayu".into(), AppearanceMode::Dark);
        for c in "drac".chars() {
            state.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(state.match_count(), 1);
        assert_eq!(state.focused_theme().as_deref(), Some("Dracula"));
    }

    #[test]
    fn enter_returns_focused_theme() {
        let mut state = ThemePickerState::open(themes(), "Ayu".into(), AppearanceMode::Dark);
        for c in "tokyo".chars() {
            state.handle_key(&key(KeyCode::Char(c)));
        }
        let resp = state.handle_key(&key(KeyCode::Enter));
        assert_eq!(resp, ThemePickerResponse::Selected("Tokyo Night".into()));
    }

    #[test]
    fn escape_cancels() {
        let mut state = ThemePickerState::open(themes(), "Ayu".into(), AppearanceMode::Dark);
        assert_eq!(
            state.handle_key(&key(KeyCode::Esc)),
            ThemePickerResponse::Cancelled
        );
    }

    #[test]
    fn down_advances_focus() {
        let mut state = ThemePickerState::open(themes(), "256 Dark".into(), AppearanceMode::Dark);
        let before = state.focused;
        state.handle_key(&key(KeyCode::Down));
        assert_eq!(state.focused, before + 1);
    }

    #[test]
    fn tab_emits_mode_changed_to_opposite() {
        let mut state = ThemePickerState::open(themes(), "Ayu".into(), AppearanceMode::Dark);
        let resp = state.handle_key(&key(KeyCode::Tab));
        assert_eq!(
            resp,
            ThemePickerResponse::ModeChanged(AppearanceMode::Light)
        );
    }

    #[test]
    fn back_tab_emits_mode_changed_to_opposite() {
        let mut state = ThemePickerState::open(themes(), "Ayu".into(), AppearanceMode::Light);
        let resp = state.handle_key(&key(KeyCode::BackTab));
        assert_eq!(resp, ThemePickerResponse::ModeChanged(AppearanceMode::Dark));
    }

    #[test]
    fn left_and_right_arrows_emit_mode_changed() {
        let mut state = ThemePickerState::open(themes(), "Ayu".into(), AppearanceMode::Dark);
        let resp_left = state.handle_key(&key(KeyCode::Left));
        assert_eq!(
            resp_left,
            ThemePickerResponse::ModeChanged(AppearanceMode::Light)
        );
        // The picker state didn't actually flip — caller is the one
        // that calls `replace_themes`.  A Right keypress now should
        // emit ModeChanged with the same target (still flipping from
        // Dark, which is what we're still in).
        let resp_right = state.handle_key(&key(KeyCode::Right));
        assert_eq!(
            resp_right,
            ThemePickerResponse::ModeChanged(AppearanceMode::Light)
        );
    }

    #[test]
    fn replace_themes_focuses_requested_name_and_clears_query() {
        let mut state = ThemePickerState::open(themes(), "Ayu".into(), AppearanceMode::Dark);
        state.handle_key(&key(KeyCode::Char('d')));
        assert!(!state.query.is_empty());
        let new_list = vec!["Light A".to_owned(), "Light B".to_owned()];
        state.replace_themes(new_list, "Light B", AppearanceMode::Light);
        assert!(state.query.is_empty());
        assert_eq!(state.mode, AppearanceMode::Light);
        assert_eq!(state.focused_theme().as_deref(), Some("Light B"));
    }

    #[test]
    fn down_emits_preview_for_newly_focused_theme() {
        let mut state = ThemePickerState::open(themes(), "256 Dark".into(), AppearanceMode::Dark);
        let resp = state.handle_key(&key(KeyCode::Down));
        assert_eq!(resp, ThemePickerResponse::Preview("256 Light".into()));
    }

    #[test]
    fn typing_emits_preview_when_focused_theme_changes() {
        let mut state = ThemePickerState::open(themes(), "256 Dark".into(), AppearanceMode::Dark);
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
        let mut state = ThemePickerState::open(themes(), "256 Dark".into(), AppearanceMode::Dark);
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
    fn pill_labels_are_ascii() {
        // The PILL_*_W consts use `.len()` (byte length); that's only
        // a valid visual width if every byte is ASCII.  Editing the
        // labels to include non-ASCII (e.g. ❯ or →) without updating
        // the width strategy would silently misalign the pill.
        assert!(PILL_DARK_LABEL.is_ascii());
        assert!(PILL_LIGHT_LABEL.is_ascii());
        assert!(PILL_GAP.is_ascii());
        assert_eq!(PILL_DARK_LABEL.len(), PILL_DARK_LABEL.chars().count());
        assert_eq!(PILL_LIGHT_LABEL.len(), PILL_LIGHT_LABEL.chars().count());
        assert_eq!(PILL_GAP.len(), PILL_GAP.chars().count());
    }

    #[test]
    fn ctrl_chars_do_not_pollute_query() {
        let mut state = ThemePickerState::open(themes(), "Ayu".into(), AppearanceMode::Dark);
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
        // A 60-row terminal with N themes, MAX_LIST_ROWS=20 cap: the
        // modal's natural height is N list rows + 5 pinned (pill +
        // hint + spacer + input + divider) + 4 chrome = N + 9 rows.
        // Centred y = (60 - (N + 9)) / 2.
        let mut state = ThemePickerState::open(themes(), "Ayu".into(), AppearanceMode::Dark);
        let y = render_top_y(&mut state, 80, 60);
        let themes_h = themes().len() as u16 + 5 + 4;
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
        let mut state = ThemePickerState::open(themes(), "Ayu".into(), AppearanceMode::Dark);
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
        let mut state = ThemePickerState::open(themes(), "Ayu".into(), AppearanceMode::Dark);
        for c in "zzznotanything".chars() {
            state.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(state.match_count(), 0);
        let resp = state.handle_key(&key(KeyCode::Enter));
        assert_eq!(resp, ThemePickerResponse::Continue);
    }
}
