//! Fuzzy-searchable command palette.
//!
//! The palette is a centred modal with a single-line input on top of a
//! scrollable list of matched actions.  When the input is empty, all
//! actions are shown organized into named sections (`Suggested`, `File`,
//! `Edit`, …); once the user types, [`nucleo_matcher`] ranks every entry
//! by fuzzy score against the query and sections collapse into a flat
//! ranked list.
//!
//! The widget is deliberately UI-only: selecting a row produces an
//! [`Action`], which the caller dispatches through the normal
//! `edit_ops::apply` path.  No palette-specific handlers exist.

mod actions;

use self::actions::{label_for, ALL_ACTIONS};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::config::{Action, KeyMap, Theme};
use crate::ui::content_width::max_row_width;
use crate::ui::modal_row::{format_modal_row, RowLayout};
use crate::ui::scroll_container::ScrollContainerState;
use crate::ui::searchable_list::{
    draw_searchable_list_chrome, fuzzy_filter, render_searchable_list_scrollbar,
    SearchableListChrome,
};

/// One palette row: an action plus its display label.
///
/// `chord` is the human-readable keybinding string for the action when
/// one exists in the active [`KeyMap`].  Showing it next to the label
/// is how users learn bindings organically — typing "save" surfaces
/// `Save file  (Ctrl-S)` so the next time they don't even open the
/// palette.
#[derive(Debug, Clone)]
pub struct PaletteEntry {
    pub action: Action,
    pub label: String,
    pub chord: Option<String>,
}

/// A row in the palette's display list: either a non-selectable section
/// header or a selectable entry.
#[derive(Debug, Clone)]
pub enum DisplayRow {
    SectionHeader(&'static str),
    Entry(usize),
}

/// Outcome of dispatching a key event to the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteResponse {
    /// No transition — keep rendering the palette.
    Continue,
    /// User cancelled (Escape).  Caller should drop the palette state.
    Cancelled,
    /// User picked a row.  Caller should drop the palette state and
    /// dispatch this action through the normal action pipeline.
    Selected(Action),
}

/// Mutable state for an open palette.  The caller constructs this when
/// the palette opens (typically in response to `Ctrl-P`) and discards
/// it on `Cancelled` / `Selected`.
#[derive(Debug, Clone)]
pub struct PaletteState {
    pub query: String,
    /// Index into `display_rows` pointing at a [`DisplayRow::Entry`].
    pub focused: usize,
    /// All entries the palette can possibly show, regardless of query.
    /// The set is built once from the active [`KeyMap`] in
    /// [`PaletteState::open`] — there is no reason to rebuild on every
    /// keystroke since the action surface is static.
    pub entries: Vec<PaletteEntry>,
    /// Vertical scroll bookkeeping for the result list.  Up/Down move
    /// `focused` and pull the viewport via `ensure_visible`; PgUp/PgDn
    /// and the mouse wheel drive `scroll_state.scroll` directly without
    /// touching focus.
    pub scroll_state: ScrollContainerState,
    /// Display list for the current query.  For an empty query this is
    /// a sectioned view (headers + entries); for a typed query it is a
    /// flat list of fuzzy-matched entries.  Recomputed lazily so we
    /// don't pay the matcher cost for non-input keys.
    display_rows: Vec<DisplayRow>,
    /// Cached query string the `display_rows` list was computed for.
    /// When the live `query` differs we recompute.
    matched_for_query: Option<String>,
    /// Absolute terminal rect of the rendered `esc` close hint.
    /// Populated each render; used by the App layer for click
    /// hit-testing.
    pub esc_button_rect: Option<Rect>,
}

impl PaletteState {
    /// Build the full palette entry list from `keymap` and seed an
    /// empty query.
    pub fn open(keymap: &KeyMap) -> Self {
        let entries = build_entries(keymap);
        Self {
            query: String::new(),
            focused: 0,
            entries,
            scroll_state: ScrollContainerState::default(),
            display_rows: Vec::new(),
            matched_for_query: None,
            esc_button_rect: None,
        }
    }

    /// Apply a key event.  Returns the high-level response — most
    /// keystrokes are absorbed (`Continue`); Enter selects the focused
    /// row; Escape cancels.
    pub fn handle_key(&mut self, key: &KeyEvent) -> PaletteResponse {
        // PgUp/PgDn/Home/End move the viewport without touching focus —
        // standard list-box behaviour, mirrors the editor's scroll keys.
        if self.scroll_state.handle_paging_key(key) {
            return PaletteResponse::Continue;
        }
        match key.code {
            KeyCode::Esc => PaletteResponse::Cancelled,
            KeyCode::Enter => {
                self.refresh_display();
                if let Some(DisplayRow::Entry(idx)) = self.display_rows.get(self.focused) {
                    if let Some(entry) = self.entries.get(*idx) {
                        return PaletteResponse::Selected(entry.action.clone());
                    }
                }
                // No matches — Enter is a no-op rather than a cancel
                // so the user can keep typing.
                PaletteResponse::Continue
            }
            KeyCode::Up => {
                self.refresh_display();
                if let Some(prev) = (0..self.focused)
                    .rev()
                    .find(|&i| matches!(self.display_rows[i], DisplayRow::Entry(_)))
                {
                    self.focused = prev;
                    self.scroll_state.ensure_visible(self.focused as u16);
                }
                PaletteResponse::Continue
            }
            KeyCode::Down => {
                self.refresh_display();
                if let Some(next) = (self.focused + 1..self.display_rows.len())
                    .find(|&i| matches!(self.display_rows[i], DisplayRow::Entry(_)))
                {
                    self.focused = next;
                    self.scroll_state.ensure_visible(self.focused as u16);
                }
                PaletteResponse::Continue
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.invalidate_display();
                PaletteResponse::Continue
            }
            KeyCode::Char(c) => {
                // Ignore Ctrl/Alt-modified chars so chords like Ctrl-P
                // don't end up typed into the query.
                use crossterm::event::KeyModifiers;
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    return PaletteResponse::Continue;
                }
                self.query.push(c);
                self.invalidate_display();
                PaletteResponse::Continue
            }
            _ => PaletteResponse::Continue,
        }
    }

    /// The visible row count for the current query.  Used by tests in
    /// this module that want to assert "this query yielded N matches".
    #[allow(dead_code)]
    pub fn match_count(&mut self) -> usize {
        self.refresh_display();
        self.display_rows
            .iter()
            .filter(|r| matches!(r, DisplayRow::Entry(_)))
            .count()
    }

    /// The action currently focused, after applying the query.  Returns
    /// `None` when the visible list is empty.  Used by tests in this
    /// module.
    #[allow(dead_code)]
    pub fn focused_action(&mut self) -> Option<Action> {
        self.refresh_display();
        match self.display_rows.get(self.focused)? {
            DisplayRow::Entry(i) => self.entries.get(*i).map(|e| e.action.clone()),
            DisplayRow::SectionHeader(_) => None,
        }
    }

    fn invalidate_display(&mut self) {
        self.display_rows.clear();
        self.matched_for_query = None;
        self.focused = 0;
        self.scroll_state.scroll = 0;
    }

    /// Recompute `display_rows` if the cached query is stale.
    fn refresh_display(&mut self) {
        if self.matched_for_query.as_deref() == Some(self.query.as_str()) {
            return;
        }
        self.display_rows.clear();
        if self.query.is_empty() {
            // Sectioned view: Suggested first, then action categories.
            // Each suggested action also appears in its category section.
            self.display_rows
                .push(DisplayRow::SectionHeader("Suggested"));
            let mut suggested: Vec<usize> = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| is_suggested(&e.action))
                .map(|(i, _)| i)
                .collect();
            suggested.sort_by_key(|&i| {
                SUGGESTED_ACTIONS
                    .iter()
                    .position(|a| a == &self.entries[i].action)
                    .unwrap_or(usize::MAX)
            });
            for idx in suggested {
                self.display_rows.push(DisplayRow::Entry(idx));
            }
            for &section in SECTION_ORDER {
                let items: Vec<usize> = self
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| section_of(&e.action) == section)
                    .map(|(i, _)| i)
                    .collect();
                if !items.is_empty() {
                    self.display_rows.push(DisplayRow::SectionHeader(section));
                    for idx in items {
                        self.display_rows.push(DisplayRow::Entry(idx));
                    }
                }
            }
        } else {
            // `entries` is already alphabetical by label, so index-
            // order tie-breaking inside `fuzzy_filter` is equivalent
            // to the previous explicit label-order tie-break.
            for i in fuzzy_filter(&self.entries, &self.query, |e| e.label.as_str()) {
                self.display_rows.push(DisplayRow::Entry(i));
            }
        }
        self.matched_for_query = Some(self.query.clone());
        // Ensure focused points to a selectable Entry row.
        if !matches!(
            self.display_rows.get(self.focused),
            Some(DisplayRow::Entry(_))
        ) {
            self.focused = self
                .display_rows
                .iter()
                .position(|r| matches!(r, DisplayRow::Entry(_)))
                .unwrap_or(0);
        }
    }
}

/// View-only widget that renders the palette over the editor.
pub struct PaletteView<'a> {
    pub theme: &'a Theme,
    pub cursor_visible: bool,
}

impl<'a> StatefulWidget for PaletteView<'a> {
    type State = PaletteState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        state.refresh_display();

        let chrome = SearchableListChrome {
            title: "Command Palette",
            query: &state.query,
            content_width: palette_content_width(state).max(NO_MATCHES_WIDTH),
            row_count: state.display_rows.len() as u16,
            cursor_visible: self.cursor_visible,
            theme: self.theme,
        };
        let Some(layout) = draw_searchable_list_chrome(area, buf, chrome, &mut state.scroll_state)
        else {
            return;
        };
        state.esc_button_rect = layout.esc_hit_rect;
        let list_area = layout.list_area;
        if list_area.height == 0 {
            return;
        }

        let scroll = state.scroll_state.scroll as usize;
        let visible_rows = list_area.height as usize;

        let mut lines: Vec<Line<'_>> = Vec::with_capacity(visible_rows);
        let has_entries = state
            .display_rows
            .iter()
            .any(|r| matches!(r, DisplayRow::Entry(_)));
        if !has_entries {
            lines.push(Line::from(Span::styled(
                "(no matches)".to_owned(),
                self.theme.modal_item,
            )));
        } else {
            for (visible_idx, row) in state
                .display_rows
                .iter()
                .skip(scroll)
                .take(visible_rows)
                .enumerate()
            {
                let absolute_idx = visible_idx + scroll;
                match row {
                    DisplayRow::SectionHeader(title) => {
                        lines.push(format_section_header(title, self.theme, list_area.width));
                    }
                    DisplayRow::Entry(entry_idx) => {
                        let entry = &state.entries[*entry_idx];
                        let focused = absolute_idx == state.focused;
                        lines.push(format_row(entry, focused, self.theme, list_area.width));
                    }
                }
            }
        }

        Paragraph::new(lines)
            .style(self.theme.modal_bg)
            .render(list_area, buf);

        render_searchable_list_scrollbar(&layout, &state.scroll_state, self.theme, buf);
    }
}

/// Width of "(no matches)" copy, used as a floor so the modal doesn't
/// snap narrower than the placeholder.
const NO_MATCHES_WIDTH: u16 = 12;

/// Content-aware width for the palette body: max over `entries` of
/// `marker(2) + label_w + 1 (gap) + chord_w`.  We size on the *whole*
/// entry list rather than the current `display_rows` set so the modal
/// doesn't jiggle in width as the user types.
fn palette_content_width(state: &PaletteState) -> u16 {
    max_row_width(&state.entries, |e| {
        let label_w = e.label.chars().count();
        let chord_w = e.chord.as_deref().map(|c| c.chars().count()).unwrap_or(0);
        // 2 marker + label + 1 gap + chord
        2 + label_w + 1 + chord_w
    })
}

/// Format one palette row: focused rows fill with `modal_item_selected`
/// (interactive bg + default_bg fg) and render the chord with the
/// hint color; unfocused rows use `modal_item` and a dim chord.
fn format_row(entry: &PaletteEntry, focused: bool, theme: &Theme, width: u16) -> Line<'static> {
    let chord = entry.chord.as_deref().unwrap_or("");
    format_modal_row(
        &entry.label,
        chord,
        focused,
        false,
        theme,
        RowLayout::RightAlign(width),
    )
}

/// Format a section header as a thin separator: `─ Title ───────`.
fn format_section_header(title: &str, theme: &Theme, width: u16) -> Line<'static> {
    let prefix = format!("─ {} ", title);
    let prefix_w = prefix.chars().count();
    let remaining = (width as usize).saturating_sub(prefix_w);
    let text = format!("{}{}", prefix, "─".repeat(remaining));
    Line::from(Span::styled(text, theme.modal_section_heading))
}

/// Curated "Suggested" entries shown when the palette opens with no
/// input.  Ordering is the user-pinned grouping: configuration
/// surfaces first, then the table-insert / handle-toggle pair,
/// export, then the "open externally / look up syntax" pair at the
/// end.  `InsertTable` is intentionally surfaced even though its
/// handler is still a stub — landing the palette entry now means
/// muscle-memory stays stable when the real implementation arrives.
const SUGGESTED_ACTIONS: &[Action] = &[
    Action::OpenSettings,
    Action::SwitchTheme,
    Action::OpenKeybinds,
    Action::GoToSection,
    Action::InsertTable,
    Action::ToggleTableButtons,
    Action::SaveCopy,
    Action::ExportHtml,
    Action::OpenInExternalEditor,
    Action::ShowMarkdownCheatSheet,
    Action::OpenGitHub,
];

/// True when `action` is part of the curated suggested list.
fn is_suggested(action: &Action) -> bool {
    SUGGESTED_ACTIONS.contains(action)
}

/// Section ordering for the empty-state view.  Each action maps to
/// exactly one section via [`section_of`]; the Suggested section is
/// handled separately (see [`refresh_display`]).
const SECTION_ORDER: &[&str] = &["File", "Edit", "View", "Navigate", "Table", "Tools"];

fn section_of(action: &Action) -> &'static str {
    match action {
        Action::Save | Action::SaveCopy | Action::Open | Action::ExportHtml | Action::Quit => {
            "File"
        }
        Action::Undo
        | Action::Redo
        | Action::Copy
        | Action::Cut
        | Action::Paste
        | Action::SelectAll
        | Action::BoldSelection
        | Action::ItalicizeSelection
        | Action::ToggleCheckbox
        | Action::InsertTable
        | Action::InsertFootnote
        | Action::DeleteFootnote
        | Action::RenumberFootnotes => "Edit",
        Action::ExitToPreview
        | Action::ToggleRawMode
        | Action::EnterEditMode
        | Action::ToggleTableButtons => "View",
        Action::FollowLinkUnderCursor
        | Action::NavigateBack
        | Action::NavigateForward
        | Action::GoToSection => "Navigate",
        Action::TableMoveRowUp
        | Action::TableMoveRowDown
        | Action::TableMoveColumnLeft
        | Action::TableMoveColumnRight
        | Action::TableInsertRowAbove
        | Action::TableInsertRowBelow
        | Action::TableInsertColumnLeft
        | Action::TableInsertColumnRight
        | Action::TableDeleteRow
        | Action::TableDeleteColumn => "Table",
        Action::OpenSettings
        | Action::OpenKeybinds
        | Action::SwitchTheme
        | Action::CreateCustomTheme
        | Action::ShowMarkdownCheatSheet
        | Action::OpenInExternalEditor
        | Action::OpenGitHub => "Tools",
        _ => "Other",
    }
}

/// Build the full action list shown in the palette.  Each entry has a
/// human-readable label and (optionally) the bound chord.  The list is
/// the canonical action surface for the palette: any action that
/// shouldn't be reachable here returns `None` from [`label_for`].
fn build_entries(keymap: &KeyMap) -> Vec<PaletteEntry> {
    // Walk every Action variant that has a label.  We drive this via a
    // hand-written table rather than reflecting the enum so the labels
    // stay user-facing (verbs over symbols) and so we can pin order /
    // exclude internal actions.
    let mut entries: Vec<PaletteEntry> = ALL_ACTIONS
        .iter()
        .filter_map(|a| {
            let label = label_for(a)?;
            let chord = keymap.first_key_for(a);
            Some(PaletteEntry {
                action: a.clone(),
                label: label.to_owned(),
                chord,
            })
        })
        .collect();
    // Stable user-facing order: alphabetical by label.  The empty-state
    // suggested list overrides this with `SUGGESTED_ORDER`; anything else
    // (i.e. a typed query) is sorted by fuzzy score.
    entries.sort_by(|a, b| a.label.cmp(&b.label));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KeyBindingOverrides;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn keymap() -> KeyMap {
        KeyMap::build(&KeyBindingOverrides::default()).unwrap()
    }

    #[test]
    fn empty_state_has_suggested_section_first_in_curated_order() {
        let mut state = PaletteState::open(&keymap());
        state.refresh_display();
        // First row is the "Suggested" section header.
        assert!(matches!(
            state.display_rows[0],
            DisplayRow::SectionHeader("Suggested")
        ));
        // Entries between the first and second headers are the curated
        // suggestions in SUGGESTED_ORDER.
        let second_header = state
            .display_rows
            .iter()
            .skip(1)
            .position(|r| matches!(r, DisplayRow::SectionHeader(_)))
            .unwrap()
            + 1;
        let labels: Vec<String> = state.display_rows[1..second_header]
            .iter()
            .filter_map(|r| match r {
                DisplayRow::Entry(i) => Some(state.entries[*i].label.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            labels,
            vec![
                "Open settings".to_owned(),
                "Switch theme".to_owned(),
                "Open keybindings".to_owned(),
                "Go to section".to_owned(),
                "Insert table".to_owned(),
                "Toggle table buttons".to_owned(),
                "Save a copy".to_owned(),
                "Export HTML".to_owned(),
                "Open current file in system editor".to_owned(),
                "Show Markdown cheat sheet".to_owned(),
                "View Edamame on GitHub".to_owned()
            ]
        );
    }

    #[test]
    fn empty_state_shows_all_actions_in_category_sections() {
        let mut state = PaletteState::open(&keymap());
        state.refresh_display();
        // Every action from ALL_ACTIONS should appear at least once in
        // a category section (i.e. not counting the Suggested section).
        let second_header_pos = state
            .display_rows
            .iter()
            .skip(1)
            .position(|r| matches!(r, DisplayRow::SectionHeader(_)))
            .unwrap()
            + 1;
        let category_actions: Vec<Action> = state.display_rows[second_header_pos..]
            .iter()
            .filter_map(|r| match r {
                DisplayRow::Entry(i) => Some(state.entries[*i].action.clone()),
                _ => None,
            })
            .collect();
        for action in ALL_ACTIONS {
            if label_for(action).is_some() {
                assert!(
                    category_actions.contains(action),
                    "action {action} missing from category sections"
                );
            }
        }
    }

    #[test]
    fn empty_state_suggested_actions_also_in_categories() {
        let mut state = PaletteState::open(&keymap());
        state.refresh_display();
        // Every suggested action must also appear in its category.
        let second_header_pos = state
            .display_rows
            .iter()
            .skip(1)
            .position(|r| matches!(r, DisplayRow::SectionHeader(_)))
            .unwrap()
            + 1;
        let category_actions: Vec<Action> = state.display_rows[second_header_pos..]
            .iter()
            .filter_map(|r| match r {
                DisplayRow::Entry(i) => Some(state.entries[*i].action.clone()),
                _ => None,
            })
            .collect();
        for action in SUGGESTED_ACTIONS {
            assert!(
                category_actions.contains(action),
                "suggested action {action} not duplicated in category section"
            );
        }
    }

    #[test]
    fn typing_save_file_finds_save_file() {
        // "save" alone is ambiguous now that "Save a copy" is also in
        // the catalogue — the fuzzy ranker prefers the shorter label.
        // Typing "save f" disambiguates to "Save file".
        let mut state = PaletteState::open(&keymap());
        for c in "save f".chars() {
            state.handle_key(&key(KeyCode::Char(c)));
        }
        let action = state.focused_action().expect("save matched");
        assert_eq!(action, Action::Save);
    }

    #[test]
    fn typing_save_a_copy_finds_save_copy() {
        let mut state = PaletteState::open(&keymap());
        for c in "save a copy".chars() {
            state.handle_key(&key(KeyCode::Char(c)));
        }
        let action = state.focused_action().expect("save a copy matched");
        assert_eq!(action, Action::SaveCopy);
    }

    #[test]
    fn enter_with_no_matches_is_continue() {
        let mut state = PaletteState::open(&keymap());
        for c in "zzzznotanything".chars() {
            state.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(state.match_count(), 0);
        let resp = state.handle_key(&key(KeyCode::Enter));
        assert_eq!(resp, PaletteResponse::Continue);
    }

    #[test]
    fn escape_cancels() {
        let mut state = PaletteState::open(&keymap());
        let resp = state.handle_key(&key(KeyCode::Esc));
        assert_eq!(resp, PaletteResponse::Cancelled);
    }

    #[test]
    fn enter_returns_focused_action() {
        let mut state = PaletteState::open(&keymap());
        // Type enough to leave only one match — "markd" is uniquely
        // prefixed by "Show Markdown Cheat Sheet" amongst our labels.
        for c in "markd".chars() {
            state.handle_key(&key(KeyCode::Char(c)));
        }
        let resp = state.handle_key(&key(KeyCode::Enter));
        assert_eq!(
            resp,
            PaletteResponse::Selected(Action::ShowMarkdownCheatSheet)
        );
    }

    #[test]
    fn down_advances_focus_skipping_headers() {
        let mut state = PaletteState::open(&keymap());
        let count = state.match_count();
        assert!(count > 1);
        // Focused starts on the first Entry (past the Suggested header).
        let first = state.focused;
        assert!(matches!(state.display_rows[first], DisplayRow::Entry(_)));
        state.handle_key(&key(KeyCode::Down));
        assert!(state.focused > first);
        assert!(matches!(
            state.display_rows[state.focused],
            DisplayRow::Entry(_)
        ));
        // Focus is clamped at the last Entry row.
        for _ in 0..state.display_rows.len() + 5 {
            state.handle_key(&key(KeyCode::Down));
        }
        let last_entry = state
            .display_rows
            .iter()
            .rposition(|r| matches!(r, DisplayRow::Entry(_)))
            .unwrap();
        assert_eq!(state.focused, last_entry);
    }

    #[test]
    fn ctrl_chars_do_not_pollute_query() {
        let mut state = PaletteState::open(&keymap());
        let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        state.handle_key(&ctrl_p);
        assert!(state.query.is_empty());
    }

    #[test]
    fn entries_carry_the_chord_for_save() {
        let entries = build_entries(&keymap());
        let save = entries.iter().find(|e| e.action == Action::Save).unwrap();
        assert!(save.chord.is_some(), "Save chord should be Ctrl-S");
    }

    // ── Scroll-container integration ────────────────────────────────────

    use crate::config::Theme;
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn render(state: &mut PaletteState, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_stateful_widget(
                    PaletteView {
                        theme: theme(),
                        cursor_visible: true,
                    },
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
    fn palette_renders_scrollbar_when_more_rows_than_visible_height() {
        let mut state = PaletteState::open(&keymap());
        // Empty-state sectioned view already exceeds the list height
        // in a small terminal; typing a char is no longer necessary
        // but we keep it for consistency with the fuzzy-match path.
        state.handle_key(&key(KeyCode::Char('e')));
        // 80 cols × 6 rows leaves only ~2 list rows after the input,
        // divider, and frame chrome — guaranteed overflow.
        let contents = render(&mut state, 80, 10);
        assert!(
            contents.contains('█'),
            "expected scrollbar thumb glyph, got: {contents}"
        );
    }

    #[test]
    fn palette_pgdown_advances_scroll_without_moving_focus() {
        let mut state = PaletteState::open(&keymap());
        state.handle_key(&key(KeyCode::Char('e')));
        // Render once so scroll_state.last_visible is populated.
        render(&mut state, 80, 10);
        let focused_before = state.focused;
        state.handle_key(&key(KeyCode::PageDown));
        assert_eq!(state.focused, focused_before, "PgDn must not move focus");
        assert!(
            state.scroll_state.scroll > 0,
            "PgDn must advance scroll, got {}",
            state.scroll_state.scroll
        );
    }

    #[test]
    fn palette_wheel_scrolls_list() {
        let mut state = PaletteState::open(&keymap());
        state.handle_key(&key(KeyCode::Char('e')));
        render(&mut state, 80, 10);
        let focused_before = state.focused;
        state.scroll_state.scroll_by(2);
        assert_eq!(state.scroll_state.scroll, 2);
        assert_eq!(state.focused, focused_before);
    }

    #[test]
    fn palette_down_arrow_pulls_viewport_back_to_focus_after_wheel_off_top() {
        let mut state = PaletteState::open(&keymap());
        state.handle_key(&key(KeyCode::Char('e')));
        render(&mut state, 80, 10);
        // Wheel scrolls past the focused row.
        state.scroll_state.scroll_by(5);
        // Down arrow must move focus AND pull the viewport with it.
        state.handle_key(&key(KeyCode::Down));
        assert!(
            state.focused as u16 >= state.scroll_state.scroll,
            "focus {} must be at or below viewport top {}",
            state.focused,
            state.scroll_state.scroll
        );
        let visible_top = state.scroll_state.scroll;
        let visible_bottom = visible_top + state.scroll_state.last_visible;
        assert!(
            (state.focused as u16) < visible_bottom,
            "focus {} must be above viewport bottom {}",
            state.focused,
            visible_bottom
        );
    }

    #[test]
    fn palette_modal_width_shrinks_to_longest_row_in_wide_terminal() {
        let mut state = PaletteState::open(&keymap());
        state.refresh_display();
        // 200-col terminal: a 70%-width modal would be 140 cols.
        // Content-aware sizing should produce something much narrower
        // (the longest entry label is ~35 chars).
        let term_w = 200u16;
        let term_h = 30u16;
        let contents = render(&mut state, term_w, term_h);
        // Find a row containing horizontal-border glyphs and measure
        // its run length.  Scan every row and pick the one with the
        // most border chars.
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
        assert!(
            max_border > 0,
            "expected to find a border row, got {max_border}"
        );
        // Border content is modal_width - 2 (corners).  +2 to compare.
        let modal_width = max_border + 2;
        assert!(
            modal_width < 100,
            "expected content-aware width well below 70% of 200, got modal width {modal_width}"
        );
        assert!(
            modal_width >= 30,
            "expected modal at least 30 cols wide, got modal width {modal_width}"
        );
    }

    #[test]
    fn palette_top_y_does_not_change_when_match_count_changes() {
        // The MAX_LIST_ROWS cap keeps the modal height stable across
        // most queries: both the empty-state sectioned view and a
        // broad fuzzy query hit the cap, so the centred position stays
        // constant and the input row doesn't jump.
        let term_w = 80u16;
        let term_h = 60u16;

        // y of the topmost row that contains a horizontal-border glyph
        // (the modal's top edge).
        let modal_top_y = |state: &mut PaletteState| -> u16 {
            let contents = render(state, term_w, term_h);
            (0..term_h)
                .find(|&y| {
                    contents
                        .chars()
                        .skip((y as usize) * term_w as usize)
                        .take(term_w as usize)
                        .any(|c| c == '─')
                })
                .expect("palette frame border not found in render")
        };

        let mut state = PaletteState::open(&keymap());
        let y_empty = modal_top_y(&mut state);
        // Single-char query that produces a different (typically larger)
        // match count than the curated empty-state list.
        state.handle_key(&key(KeyCode::Char('e')));
        let y_typed = modal_top_y(&mut state);
        assert_eq!(
            y_empty, y_typed,
            "modal top must not move when match count changes"
        );
    }

    #[test]
    fn section_headers_are_not_focusable() {
        let mut state = PaletteState::open(&keymap());
        state.refresh_display();
        // Verify focused never lands on a header, even after many
        // Up/Down cycles.
        for _ in 0..state.display_rows.len() + 5 {
            state.handle_key(&key(KeyCode::Down));
            assert!(
                matches!(state.display_rows[state.focused], DisplayRow::Entry(_)),
                "focused landed on a section header at index {}",
                state.focused
            );
        }
        for _ in 0..state.display_rows.len() + 5 {
            state.handle_key(&key(KeyCode::Up));
            assert!(
                matches!(state.display_rows[state.focused], DisplayRow::Entry(_)),
                "focused landed on a section header at index {}",
                state.focused
            );
        }
    }
}
