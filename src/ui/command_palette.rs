//! Phase 10 — fuzzy-searchable command palette.
//!
//! The palette is a centred modal with a single-line input on top of a
//! scrollable list of matched actions.  When the input is empty, the
//! list shows a curated "Suggested" set rather than every bound action;
//! once the user types, [`nucleo_matcher`] ranks every entry by fuzzy
//! score against the query.
//!
//! The widget is deliberately UI-only: selecting a row produces an
//! [`Action`], which the caller dispatches through the normal
//! `edit_ops::apply` path.  No palette-specific handlers exist.

use crossterm::event::{KeyCode, KeyEvent};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::config::{Action, KeyMap, Theme};
use crate::ui::scroll_container::{
    draw_frame, top_anchored_rect_for_content, ContentSize, ScrollContainerState,
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

impl PaletteEntry {
    pub fn new(action: Action, label: impl Into<String>) -> Self {
        Self {
            action,
            label: label.into(),
            chord: None,
        }
    }
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
    /// Index into the *currently visible* list, not the full action set.
    pub focused: usize,
    /// All entries the palette can possibly show, regardless of query.
    /// The Phase 10 set is built once from the active [`KeyMap`] in
    /// [`PaletteState::open`] — there is no reason to rebuild on every
    /// keystroke since the action surface is static.
    pub entries: Vec<PaletteEntry>,
    /// Vertical scroll bookkeeping for the result list.  Up/Down move
    /// `focused` and pull the viewport via `ensure_visible`; PgUp/PgDn
    /// and the mouse wheel drive `scroll_state.scroll` directly without
    /// touching focus.
    pub scroll_state: ScrollContainerState,
    /// Indices into `entries` that match the current `query`, ordered
    /// by fuzzy score.  Recomputed lazily inside [`PaletteView::render`]
    /// rather than on every keystroke so we don't pay the matcher cost
    /// for keys that have no input effect (cursor moves, modifiers).
    matched: Vec<usize>,
    /// Cached query string the `matched` list was computed for.  When
    /// the live `query` differs we recompute.
    matched_for_query: Option<String>,
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
            matched: Vec::new(),
            matched_for_query: None,
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
                self.refresh_matched();
                if let Some(&idx) = self.matched.get(self.focused) {
                    if let Some(entry) = self.entries.get(idx) {
                        return PaletteResponse::Selected(entry.action.clone());
                    }
                }
                // No matches — Enter is a no-op rather than a cancel
                // so the user can keep typing.
                PaletteResponse::Continue
            }
            KeyCode::Up => {
                self.refresh_matched();
                if !self.matched.is_empty() && self.focused > 0 {
                    self.focused -= 1;
                    self.scroll_state.ensure_visible(self.focused as u16);
                }
                PaletteResponse::Continue
            }
            KeyCode::Down => {
                self.refresh_matched();
                if !self.matched.is_empty() && self.focused + 1 < self.matched.len() {
                    self.focused += 1;
                    self.scroll_state.ensure_visible(self.focused as u16);
                }
                PaletteResponse::Continue
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.invalidate_matched();
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
                self.invalidate_matched();
                PaletteResponse::Continue
            }
            _ => PaletteResponse::Continue,
        }
    }

    /// The visible row count for the current query.  Useful in tests
    /// that want to assert "this query yielded N matches" without
    /// poking at private fields.
    pub fn match_count(&mut self) -> usize {
        self.refresh_matched();
        self.matched.len()
    }

    /// The action currently focused, after applying the query.  Returns
    /// `None` when the visible list is empty.
    pub fn focused_action(&mut self) -> Option<Action> {
        self.refresh_matched();
        self.matched
            .get(self.focused)
            .and_then(|&i| self.entries.get(i))
            .map(|e| e.action.clone())
    }

    fn invalidate_matched(&mut self) {
        self.matched.clear();
        self.matched_for_query = None;
        self.focused = 0;
        self.scroll_state.scroll = 0;
    }

    /// Recompute `matched` if the cached query is stale.  Matcher state
    /// is held internally and recreated each refresh — `Matcher` is
    /// cheap to construct and avoids carrying lifetime baggage across
    /// the struct.
    fn refresh_matched(&mut self) {
        if self.matched_for_query.as_deref() == Some(self.query.as_str()) {
            return;
        }
        if self.query.is_empty() {
            // Empty-state listing: surface the curated "Suggested"
            // entries rather than every bound action.  An entry is
            // suggested when [`PaletteEntry::is_suggested`] returns
            // true (see [`build_entries`] for the rationale).
            self.matched = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| is_suggested(&e.action))
                .map(|(i, _)| i)
                .collect();
            // Stable order: keep the curated order from `SUGGESTED_ORDER`.
            self.matched.sort_by_key(|&i| {
                SUGGESTED_ORDER
                    .iter()
                    .position(|a| a == &self.entries[i].action)
                    .unwrap_or(usize::MAX)
            });
        } else {
            let mut matcher = Matcher::default();
            let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
            let mut scored: Vec<(usize, u32)> = Vec::new();
            let mut buf: Vec<char> = Vec::new();
            for (idx, entry) in self.entries.iter().enumerate() {
                buf.clear();
                let haystack = Utf32Str::new(&entry.label, &mut buf);
                if let Some(score) = pattern.score(haystack, &mut matcher) {
                    scored.push((idx, score));
                }
            }
            // Higher score first; tie-break by stable label order.
            scored.sort_by(|a, b| {
                b.1.cmp(&a.1)
                    .then_with(|| self.entries[a.0].label.cmp(&self.entries[b.0].label))
            });
            self.matched = scored.into_iter().map(|(i, _)| i).collect();
        }
        self.matched_for_query = Some(self.query.clone());
        if self.focused >= self.matched.len() {
            self.focused = self.matched.len().saturating_sub(1);
        }
    }
}

/// View-only widget that renders the palette over the editor.
pub struct PaletteView<'a> {
    pub theme: &'a Theme,
}

impl<'a> StatefulWidget for PaletteView<'a> {
    type State = PaletteState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        state.refresh_matched();

        // Content-aware sizing: width tracks the longest entry row,
        // height tracks the match count.  Pinned-top covers the input
        // row + divider.  The "(no matches)" / "(no suggested
        // commands)" placeholder factors into width too so the modal
        // doesn't snap narrower than its empty-state copy.
        let content_width = palette_content_width(state).max(NO_MATCHES_WIDTH);
        let scrolling_height = state.matched.len().max(1) as u16;
        let content = ContentSize {
            width: content_width,
            height: scrolling_height,
            pinned_top: 2, // input row + divider
            pinned_bottom: 0,
        };
        // Top-anchored rather than centred: the palette's match count
        // changes per keystroke, so a centred modal would shift the
        // input row up and down as the user types.  Anchoring near the
        // top pins the input and lets only the result list grow or
        // shrink underneath it.
        let modal_area = top_anchored_rect_for_content(content, area);

        // Pre-compute layout so the title's arrow indicator reflects
        // the post-observe scroll bounds.
        let inner_h = modal_area.height.saturating_sub(2);
        let pinned_top: u16 = 2;
        let list_height = inner_h.saturating_sub(pinned_top);
        state
            .scroll_state
            .observe(state.matched.len() as u16, list_height);
        // Rebound focus into the visible window so wheel scrolling +
        // arrow keys cooperate naturally.
        state.scroll_state.ensure_visible(state.focused as u16);

        let inner = draw_frame(
            modal_area,
            buf,
            "Command Palette",
            state.scroll_state.arrow(),
            self.theme,
        );
        if inner.height < 2 || inner.width == 0 {
            return;
        }

        // Top row: the live input — `›` prompt + query + a static cursor
        // glyph so the user sees where typing lands even though the
        // palette uses a single Paragraph rather than a real text widget.
        let input_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        };
        let prompt = Span::styled("› ", self.theme.status_info);
        let typed = Span::styled(state.query.clone(), self.theme.status_filename);
        let cursor = Span::styled("▏", self.theme.cursor);
        Paragraph::new(Line::from(vec![prompt, typed, cursor]))
            .style(self.theme.status_bar)
            .render(input_area, buf);

        // Divider between input and result list.
        let divider_y = inner.y + 1;
        for x in inner.x..(inner.x + inner.width) {
            buf[(x, divider_y)]
                .set_symbol("─")
                .set_style(self.theme.rule);
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
        if state.matched.is_empty() {
            let label = if state.query.is_empty() {
                "(no suggested commands)"
            } else {
                "(no matches)"
            };
            lines.push(Line::from(Span::styled(
                label.to_owned(),
                self.theme.status_info,
            )));
        } else {
            for (visible_idx, &entry_idx) in state
                .matched
                .iter()
                .skip(scroll)
                .take(visible_rows)
                .enumerate()
            {
                let entry = &state.entries[entry_idx];
                let absolute_idx = visible_idx + scroll;
                let focused = absolute_idx == state.focused;
                lines.push(format_row(entry, focused, self.theme, list_area.width));
            }
        }

        Paragraph::new(lines)
            .style(self.theme.status_bar)
            .render(list_area, buf);
    }
}

/// Width of "(no suggested commands)" / "(no matches)" copy, used so
/// the empty-state palette doesn't snap narrower than the placeholder.
const NO_MATCHES_WIDTH: u16 = 23;

/// Content-aware width for the palette body: max over `entries` of
/// `marker(2) + label_w + 1 (gap) + chord_w`.  We size on the *whole*
/// entry list rather than the current `matched` set so the modal
/// doesn't jiggle in width as the user types.
fn palette_content_width(state: &PaletteState) -> u16 {
    state
        .entries
        .iter()
        .map(|e| {
            let label_w = e.label.chars().count();
            let chord_w = e.chord.as_deref().map(|c| c.chars().count()).unwrap_or(0);
            // 2 marker + label + 1 gap + chord
            2 + label_w + 1 + chord_w
        })
        .max()
        .unwrap_or(0) as u16
}

/// Format one palette row: focused rows render in `modal_button_focused`
/// style with a leading `›`; the chord (when bound) is right-aligned in
/// `status_info` so the eye scans it as metadata, not part of the label.
fn format_row(entry: &PaletteEntry, focused: bool, theme: &Theme, width: u16) -> Line<'static> {
    let marker = if focused { "› " } else { "  " };
    let label_style = if focused {
        theme.modal_button_focused
    } else {
        Style::default()
    };
    let label = format!("{}{}", marker, entry.label);
    let chord = entry.chord.clone().unwrap_or_default();
    let label_w = label.chars().count();
    let chord_w = chord.chars().count();
    let total = label_w + chord_w + 1;
    let pad = (width as usize).saturating_sub(total);
    let pad_str = " ".repeat(pad.max(1));
    let chord_style = if focused {
        Style::default().add_modifier(Modifier::DIM)
    } else {
        theme.status_info
    };
    Line::from(vec![
        Span::styled(label, label_style),
        Span::raw(pad_str),
        Span::styled(chord, chord_style),
    ])
}

/// Curated "Suggested" entries shown when the palette opens with no
/// input.  Ordering is the user-pinned grouping: configuration
/// surfaces first, then the table-insert / handle-toggle pair,
/// export, then the "open externally / look up syntax" pair at the
/// end.  `InsertTable` is intentionally surfaced even though its
/// handler is still a stub — landing the palette entry now means
/// muscle-memory stays stable when the real implementation arrives.
const SUGGESTED_ORDER: &[Action] = &[
    Action::OpenSettings,
    Action::OpenKeybinds,
    Action::InsertTable,
    Action::ToggleTableButtons,
    Action::ExportHtml,
    Action::OpenInExternalEditor,
    Action::ShowMarkdownCheatSheet,
];

/// True when `action` is part of the curated suggested list.
fn is_suggested(action: &Action) -> bool {
    SUGGESTED_ORDER.contains(action)
}

/// Hint-line surfaced actions are intentionally excluded from the
/// suggested empty-state listing — they already have a discovery
/// surface (the chord row) so we don't double-count them.  They remain
/// matchable by typed input.  Currently only consumed by the unit test
/// that asserts the empty-state list excludes these actions; gated on
/// `cfg(test)` so a release build doesn't carry the helper.
#[cfg(test)]
fn is_hint_line_surfaced(action: &Action) -> bool {
    matches!(
        action,
        Action::Save
            | Action::Copy
            | Action::Cut
            | Action::Paste
            | Action::Quit
            | Action::ExitToPreview
            | Action::ToggleRawMode
            | Action::EnterEditMode
            | Action::Undo
            | Action::Redo
    )
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

/// Static catalogue of every Action variant we expose in the palette.
/// Ordering doesn't matter — `build_entries` re-sorts.  Cursor-movement
/// and selection actions are excluded: they have no meaning when
/// dispatched from a modal palette (the cursor's already moved by the
/// time the user clicks `Move Right`).  `ShowCheatSheet` is also
/// excluded because the Phase 10 review merged it into `OpenKeybinds`
/// — surfacing both would be confusing.
const ALL_ACTIONS: &[Action] = &[
    // Phase 10 entries (palette-only).  `OpenConfigFolder` is no
    // longer surfaced here — it lives on the first row of the
    // settings overlay (the "Open Config folder" entry), which is
    // where users go to discover config-file locations.  Surfacing
    // it twice was redundant and made the palette noisier.
    // `ReloadFromDisk` is dropped from the palette until Phase 11
    // implements it; today it would just flash a "see Phase 11" hint
    // and add nothing.
    Action::ShowMarkdownCheatSheet,
    Action::OpenSettings,
    Action::OpenKeybinds,
    Action::ExportHtml,
    Action::OpenInExternalEditor,
    Action::ToggleTableButtons,
    Action::InsertTable,
    // File ops.
    Action::Save,
    Action::Open,
    // History.
    Action::Undo,
    Action::Redo,
    // Clipboard.
    Action::Copy,
    Action::Cut,
    Action::Paste,
    // Selection / mode.
    Action::SelectAll,
    Action::ExitToPreview,
    Action::ToggleRawMode,
    Action::EnterEditMode,
    Action::Quit,
    // List + checkbox.
    Action::ToggleCheckbox,
    // Navigation (link / nav stack).
    Action::FollowLinkUnderCursor,
    Action::NavigateBack,
    Action::NavigateForward,
    // Tables — surface only the structural ops.  Cell navigation
    // (Tab/Shift+Tab) doesn't make sense from a palette.
    Action::TableMoveRowUp,
    Action::TableMoveRowDown,
    Action::TableMoveColumnLeft,
    Action::TableMoveColumnRight,
    Action::TableInsertRowAbove,
    Action::TableInsertRowBelow,
    Action::TableInsertColumnLeft,
    Action::TableInsertColumnRight,
    Action::TableDeleteRow,
    Action::TableDeleteColumn,
];

/// User-facing label for an [`Action`].  Returning `None` excludes the
/// action from the palette entirely.
fn label_for(action: &Action) -> Option<&'static str> {
    Some(match action {
        Action::ShowMarkdownCheatSheet => "Show Markdown cheat sheet",
        Action::OpenSettings => "Open settings",
        Action::OpenKeybinds => "Open keybindings",
        Action::ExportHtml => "Export HTML",
        Action::OpenInExternalEditor => "Open current file in system editor",
        Action::ToggleTableButtons => "Toggle table buttons",
        Action::InsertTable => "Insert table",
        Action::Save => "Save file",
        Action::Open => "Open file",
        Action::Undo => "Undo",
        Action::Redo => "Redo",
        Action::Copy => "Copy",
        Action::Cut => "Cut",
        Action::Paste => "Paste",
        Action::SelectAll => "Select all",
        Action::ExitToPreview => "Exit to preview",
        Action::ToggleRawMode => "Toggle raw mode",
        Action::EnterEditMode => "Enter edit mode",
        Action::Quit => "Quit",
        Action::ToggleCheckbox => "Toggle checkbox",
        Action::FollowLinkUnderCursor => "Follow link under cursor",
        Action::NavigateBack => "Navigate back",
        Action::NavigateForward => "Navigate forward",
        Action::TableMoveRowUp => "Table: Move row up",
        Action::TableMoveRowDown => "Table: Move row down",
        Action::TableMoveColumnLeft => "Table: Move column left",
        Action::TableMoveColumnRight => "Table: Move column right",
        Action::TableInsertRowAbove => "Table: Insert row above",
        Action::TableInsertRowBelow => "Table: Insert row below",
        Action::TableInsertColumnLeft => "Table: Insert column left",
        Action::TableInsertColumnRight => "Table: Insert column right",
        Action::TableDeleteRow => "Table: Delete row",
        Action::TableDeleteColumn => "Table: Delete column",
        _ => return None,
    })
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
    fn empty_state_lists_suggested_actions_in_curated_order() {
        let mut state = PaletteState::open(&keymap());
        // Trigger `refresh_matched` and read back the visible labels.
        state.refresh_matched();
        let labels: Vec<String> = state
            .matched
            .iter()
            .map(|&i| state.entries[i].label.clone())
            .collect();
        assert_eq!(
            labels,
            vec![
                "Open settings".to_owned(),
                "Open keybindings".to_owned(),
                "Insert table".to_owned(),
                "Toggle table buttons".to_owned(),
                "Export HTML".to_owned(),
                "Open current file in system editor".to_owned(),
                "Show Markdown cheat sheet".to_owned(),
            ]
        );
    }

    #[test]
    fn empty_state_excludes_hint_line_surfaced_actions() {
        let mut state = PaletteState::open(&keymap());
        state.refresh_matched();
        let actions: Vec<Action> = state
            .matched
            .iter()
            .map(|&i| state.entries[i].action.clone())
            .collect();
        for a in actions {
            assert!(
                !is_hint_line_surfaced(&a),
                "hint-line action surfaced in empty-state suggested list: {a}"
            );
        }
    }

    #[test]
    fn typing_save_finds_save_file() {
        let mut state = PaletteState::open(&keymap());
        for c in "save".chars() {
            state.handle_key(&key(KeyCode::Char(c)));
        }
        let action = state.focused_action().expect("save matched");
        assert_eq!(action, Action::Save);
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
    fn down_advances_focus_within_match_count() {
        let mut state = PaletteState::open(&keymap());
        let count = state.match_count();
        assert!(count > 1);
        state.handle_key(&key(KeyCode::Down));
        assert_eq!(state.focused, 1);
        // Focus is clamped at the last visible row.
        for _ in 0..count + 5 {
            state.handle_key(&key(KeyCode::Down));
        }
        assert_eq!(state.focused, count - 1);
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
                frame.render_stateful_widget(PaletteView { theme: theme() }, frame.area(), state);
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
    fn palette_renders_scroll_arrow_when_more_rows_than_visible_height() {
        let mut state = PaletteState::open(&keymap());
        // Empty-state list is ~7 rows; force a long match list by
        // typing a single char that fuzzy-matches most actions.
        state.handle_key(&key(KeyCode::Char('e')));
        // 80 cols × 6 rows leaves only ~2 list rows after the input,
        // divider, and frame chrome — guaranteed overflow.
        let contents = render(&mut state, 80, 6);
        assert!(
            contents.contains("Command Palette ↓"),
            "expected ↓ arrow, got: {contents}"
        );
    }

    #[test]
    fn palette_pgdown_advances_scroll_without_moving_focus() {
        let mut state = PaletteState::open(&keymap());
        state.handle_key(&key(KeyCode::Char('e')));
        // Render once so scroll_state.last_visible is populated.
        render(&mut state, 80, 6);
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
        render(&mut state, 80, 6);
        let focused_before = state.focused;
        state.scroll_state.scroll_by(2);
        assert_eq!(state.scroll_state.scroll, 2);
        assert_eq!(state.focused, focused_before);
    }

    #[test]
    fn palette_down_arrow_pulls_viewport_back_to_focus_after_wheel_off_top() {
        let mut state = PaletteState::open(&keymap());
        state.handle_key(&key(KeyCode::Char('e')));
        render(&mut state, 80, 6);
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
        state.refresh_matched();
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
        // Regression: previously the palette was vertically centred so
        // its y position shifted by half the height delta on every
        // keystroke that altered the match count, making the input row
        // jump as the user typed.  Top-anchored placement keeps the
        // top of the modal at a fixed offset, provided the modal
        // actually fits in the terminal — pick a generous height so a
        // worst-case match list (every entry typed `e` matches) still
        // fits without the bottom-edge clamp pulling y back up.
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
}
