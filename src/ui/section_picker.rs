//! "Go to section" modal — a fuzzy-searchable list of every heading in
//! the current document.  Mirrors the command palette's structure
//! ([`crate::ui::PaletteState`] / [`crate::ui::PaletteView`]) but trades
//! the action catalogue for a per-document heading list, and renders
//! each row in its heading-level style (fg + bold, indent by depth) so
//! the list visually resembles the document's outline.
//!
//! Selection is live-previewed: navigating with arrows or filtering by
//! query emits [`SectionPickerResponse::Preview`] carrying the target
//! scroll offset, which the modal adapter routes through a debounced
//! viewport reposition.  Enter confirms ([`Selected`], cursor moves to
//! end-of-line); Esc cancels and the modal adapter restores the
//! original scroll.
//!
//! All the bookkeeping (target_scroll precomputed per entry, current-
//! section preselection) happens in the `App::open_section_picker`
//! constructor — this widget only sorts/filters/renders.

use crossterm::event::{KeyCode, KeyEvent};
use pulldown_cmark::HeadingLevel;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::config::Theme;
use crate::ui::modal_row::truncate_to_cells;
use crate::ui::scroll_container::ScrollContainerState;
use crate::ui::searchable_list::{
    draw_searchable_list_chrome, fuzzy_filter, render_searchable_list_scrollbar,
    SearchableListChrome,
};

/// One heading in the document.  Constructed by
/// [`App::open_section_picker`](crate::app::App) so the picker doesn't
/// need to know how to walk `ParsedDoc` itself.
///
/// `target_scroll` is the `EditorState::scroll` value that puts the
/// heading's first visual row at the top of the viewport.  It's
/// precomputed at open time using the current mode + viewport width;
/// the picker only echoes it back through [`SectionPickerResponse::Preview`]
/// or [`SectionPickerResponse::Selected`].
#[derive(Debug, Clone)]
pub struct HeadingEntry {
    pub level: HeadingLevel,
    pub text: String,
    pub buffer_line: usize,
    pub target_scroll: usize,
}

/// Outcome of dispatching a key event to the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionPickerResponse {
    /// No transition — keep rendering the modal.
    Continue,
    /// Selection changed; the modal adapter should arm a debounced
    /// scroll to `target_scroll`.
    Preview { target_scroll: usize },
    /// Esc — the adapter should restore the original viewport.
    Cancelled,
    /// Enter — apply the scroll immediately and move the cursor to
    /// `buffer_line`.
    Selected {
        buffer_line: usize,
        target_scroll: usize,
    },
}

/// Mutable state for an open section picker.
#[derive(Debug, Clone)]
pub struct SectionPickerState {
    pub query: String,
    /// Index into `display_indices` pointing at the focused entry.
    pub focused: usize,
    pub entries: Vec<HeadingEntry>,
    pub scroll_state: ScrollContainerState,
    /// Indices into `entries` that survive the current query, in
    /// display order.  Empty query → all entries in document order.
    display_indices: Vec<usize>,
    /// Cached query the `display_indices` list was computed for.
    matched_for_query: Option<String>,
    /// Absolute rect of the rendered `esc` close hint, for click hit-
    /// testing by the modal adapter.
    pub esc_button_rect: Option<Rect>,
    /// Absolute rect the heading list is rendered into.  Cached on each
    /// render so [`Self::handle_click`] can map a click `(col, row)` to
    /// a display-index row + scroll offset.  `None` before the first
    /// render or when the chrome had no room for a list.
    pub list_area: Option<Rect>,
    /// Set in [`Self::open`]; consumed on the first render after the
    /// visible-window size is known.  Centres the preselected entry in
    /// the visible window so a cursor that's deep in the document
    /// doesn't open the picker with the highlighted row off-screen.
    pending_center: bool,
}

impl SectionPickerState {
    /// Build picker state from a precomputed entry list.  `focused`
    /// names the index into `entries` that should be preselected — the
    /// caller is expected to compute this from the cursor's current
    /// position (nearest preceding heading) so the modal opens on the
    /// section the user is already reading.  When `entries` is empty
    /// the picker still opens and shows a `(no headings)` placeholder.
    pub fn open(entries: Vec<HeadingEntry>, focused: usize) -> Self {
        let n = entries.len();
        let display_indices: Vec<usize> = (0..n).collect();
        let clamped = if n == 0 { 0 } else { focused.min(n - 1) };
        Self {
            query: String::new(),
            focused: clamped,
            entries,
            scroll_state: ScrollContainerState::default(),
            display_indices,
            matched_for_query: Some(String::new()),
            esc_button_rect: None,
            list_area: None,
            pending_center: true,
        }
    }

    /// Pull `focused` into the middle of the visible window.  Used once
    /// after the first render observes the visible-window height so a
    /// document whose cursor sits past `MAX_LIST_ROWS` headings opens
    /// the picker with the preselected row centred rather than scrolled
    /// to zero.  When the focus is near the start or end of the list,
    /// the clamp inside `ScrollContainerState` keeps it at the nearest
    /// in-range row instead.
    fn center_on_focused(&mut self) {
        let visible = self.scroll_state.last_visible as usize;
        if visible == 0 {
            return;
        }
        let half = visible / 2;
        let target = self.focused.saturating_sub(half) as u16;
        self.scroll_state.scroll = target;
        let max = self.scroll_state.max_scroll();
        if self.scroll_state.scroll > max {
            self.scroll_state.scroll = max;
        }
    }

    /// Apply a key event.
    pub fn handle_key(&mut self, key: &KeyEvent) -> SectionPickerResponse {
        if self.scroll_state.handle_paging_key(key) {
            return SectionPickerResponse::Continue;
        }
        match key.code {
            KeyCode::Esc => SectionPickerResponse::Cancelled,
            KeyCode::Enter => {
                self.refresh_display();
                if let Some(entry) = self.focused_entry() {
                    SectionPickerResponse::Selected {
                        buffer_line: entry.buffer_line,
                        target_scroll: entry.target_scroll,
                    }
                } else {
                    SectionPickerResponse::Continue
                }
            }
            KeyCode::Up => {
                self.refresh_display();
                if self.focused > 0 {
                    self.focused -= 1;
                    self.scroll_state.ensure_visible(self.focused as u16);
                    return self.preview_focused();
                }
                SectionPickerResponse::Continue
            }
            KeyCode::Down => {
                self.refresh_display();
                if self.focused + 1 < self.display_indices.len() {
                    self.focused += 1;
                    self.scroll_state.ensure_visible(self.focused as u16);
                    return self.preview_focused();
                }
                SectionPickerResponse::Continue
            }
            KeyCode::Backspace => {
                if self.query.pop().is_some() {
                    self.invalidate_display();
                    self.refresh_display();
                    return self.preview_focused();
                }
                SectionPickerResponse::Continue
            }
            KeyCode::Char(c) => {
                use crossterm::event::KeyModifiers;
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    return SectionPickerResponse::Continue;
                }
                self.query.push(c);
                self.invalidate_display();
                self.refresh_display();
                self.preview_focused()
            }
            _ => SectionPickerResponse::Continue,
        }
    }

    /// Insert a bracketed paste into the filter query, then re-filter
    /// and live-preview the newly focused heading.  The paste is
    /// flattened to one line and length-capped by
    /// [`crate::ui::sanitize_paste`].
    pub fn paste(&mut self, text: &str) -> SectionPickerResponse {
        let clean = crate::ui::sanitize_paste(text);
        if clean.is_empty() {
            return SectionPickerResponse::Continue;
        }
        self.query.push_str(&clean);
        self.invalidate_display();
        self.refresh_display();
        self.preview_focused()
    }

    /// Hit-test a mouse click against the rendered heading list.  A
    /// click inside [`Self::list_area`] on a populated row commits the
    /// jump for that row (same outcome as pressing Enter on it); a
    /// click outside the list area or on an empty row returns
    /// `Continue` so the modal adapter can route it elsewhere (e.g. the
    /// `esc` close hint).
    pub fn handle_click(&mut self, col: u16, row: u16) -> SectionPickerResponse {
        let Some(area) = self.list_area else {
            return SectionPickerResponse::Continue;
        };
        let inside = col >= area.x
            && col < area.x.saturating_add(area.width)
            && row >= area.y
            && row < area.y.saturating_add(area.height);
        if !inside {
            return SectionPickerResponse::Continue;
        }
        let row_offset = (row - area.y) as usize;
        let scroll = self.scroll_state.scroll as usize;
        let display_idx = scroll + row_offset;
        let Some(&entry_idx) = self.display_indices.get(display_idx) else {
            return SectionPickerResponse::Continue;
        };
        let Some(entry) = self.entries.get(entry_idx) else {
            return SectionPickerResponse::Continue;
        };
        SectionPickerResponse::Selected {
            buffer_line: entry.buffer_line,
            target_scroll: entry.target_scroll,
        }
    }

    fn focused_entry(&self) -> Option<&HeadingEntry> {
        self.display_indices
            .get(self.focused)
            .and_then(|i| self.entries.get(*i))
    }

    /// Emit a `Preview` for the currently focused entry, or `Continue`
    /// when the visible list is empty (typing a query that filters
    /// everything out shouldn't move the viewport).
    fn preview_focused(&self) -> SectionPickerResponse {
        match self.focused_entry() {
            Some(e) => SectionPickerResponse::Preview {
                target_scroll: e.target_scroll,
            },
            None => SectionPickerResponse::Continue,
        }
    }

    fn invalidate_display(&mut self) {
        self.display_indices.clear();
        self.matched_for_query = None;
        self.focused = 0;
        self.scroll_state.scroll = 0;
    }

    fn refresh_display(&mut self) {
        if self.matched_for_query.as_deref() == Some(self.query.as_str()) {
            return;
        }
        self.display_indices = if self.query.is_empty() {
            (0..self.entries.len()).collect()
        } else {
            fuzzy_filter(&self.entries, &self.query, |e| e.text.as_str())
        };
        self.matched_for_query = Some(self.query.clone());
        if self.focused >= self.display_indices.len() {
            self.focused = 0;
        }
    }
}

/// View-only widget that renders the picker over the editor.
pub struct SectionPickerView<'a> {
    pub theme: &'a Theme,
    pub cursor_visible: bool,
}

impl<'a> StatefulWidget for SectionPickerView<'a> {
    type State = SectionPickerState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        state.refresh_display();

        // The section picker has no row cap — it grows to fill the
        // available height (the modal adapter has already trimmed the
        // bottom region from `area`).  Padding insets the modal from the
        // top and bottom edges, but only when the terminal is tall
        // enough to spare it; on a short terminal the rows are too
        // precious, so we drop the padding entirely.
        let vertical_pad = if area.height < SHORT_TERMINAL_ROWS {
            0
        } else {
            SECTION_PICKER_VERTICAL_PAD
        };
        let chrome = SearchableListChrome {
            title: "Go to Section",
            query: &state.query,
            content_width: picker_content_width(state).max(NO_HEADINGS_WIDTH),
            row_count: state.display_indices.len() as u16,
            cursor_visible: self.cursor_visible,
            max_list_rows: u16::MAX,
            vertical_pad,
            theme: self.theme,
        };
        let Some(layout) = draw_searchable_list_chrome(area, buf, chrome, &mut state.scroll_state)
        else {
            return;
        };
        state.esc_button_rect = layout.esc_hit_rect;
        let list_area = layout.list_area;
        state.list_area = Some(list_area);
        if list_area.height == 0 {
            return;
        }
        if state.pending_center {
            state.center_on_focused();
            state.pending_center = false;
        }

        let scroll = state.scroll_state.scroll as usize;
        let visible_rows = list_area.height as usize;

        let mut lines: Vec<Line<'_>> = Vec::with_capacity(visible_rows);
        if state.display_indices.is_empty() {
            let placeholder = if state.entries.is_empty() {
                "(no headings)"
            } else {
                "(no matches)"
            };
            lines.push(Line::from(Span::styled(
                placeholder.to_owned(),
                self.theme.modal_item,
            )));
        } else {
            for (visible_idx, &entry_idx) in state
                .display_indices
                .iter()
                .skip(scroll)
                .take(visible_rows)
                .enumerate()
            {
                let absolute_idx = visible_idx + scroll;
                let entry = &state.entries[entry_idx];
                let focused = absolute_idx == state.focused;
                lines.push(format_heading_row(
                    entry,
                    focused,
                    self.theme,
                    list_area.width,
                ));
            }
        }

        Paragraph::new(lines)
            .style(self.theme.modal_bg)
            .render(list_area, buf);

        render_searchable_list_scrollbar(&layout, &state.scroll_state, self.theme, buf);
    }
}

/// Width floor used when the heading list is empty so the modal doesn't
/// snap narrower than `(no headings)`.
const NO_HEADINGS_WIDTH: u16 = 16;

/// Blank rows kept above and below the picker on a terminal tall enough
/// to spare them.
const SECTION_PICKER_VERTICAL_PAD: u16 = 4;

/// Terminal-height threshold below which the picker drops its vertical
/// padding and grows edge-to-edge so the cramped screen isn't wasted.
const SHORT_TERMINAL_ROWS: u16 = 20;

/// Indent (in spaces) used to render `level` in the list.  H1 = 1 space,
/// H2 = 2, …, H6 = 6.  Visually mirrors the editor's heading prefix
/// without committing to the exact rendered width (which big-H1 / rule
/// can change).  `HeadingLevel` is `repr(usize)` with H1..H6 = 1..6 so
/// the cast lines up with the indent we want.
fn indent_for(level: HeadingLevel) -> usize {
    level as usize
}

/// Pre-render width estimate: max over every entry of `indent + text`,
/// so the modal sizes itself for the longest heading.  Adding 2 keeps a
/// small right margin so focused-row backgrounds don't read flush.
fn picker_content_width(state: &SectionPickerState) -> u16 {
    let mut max_w: u16 = 0;
    for e in &state.entries {
        let w = indent_for(e.level).saturating_add(UnicodeWidthStr::width(e.text.as_str())) as u16;
        if w > max_w {
            max_w = w;
        }
    }
    max_w.saturating_add(2)
}

/// Format one heading row.  Focused rows match the command palette's
/// selection styling (`theme.modal_item_selected` — dark fg on the
/// primary bg, bold) so the focus affordance is consistent across
/// fuzzy-searchable modals.  Unfocused rows pick up the heading's
/// per-level color + bold (with `UNDERLINED` stripped — the editor uses
/// underline on H2–H6, but the picker is a list, not a document), so
/// the unfocused list visually echoes the document's outline.
///
/// Long headings are truncated with `…` to fit `width`; the marker and
/// indent are preserved so focus stays readable on narrow terminals.
fn format_heading_row(
    entry: &HeadingEntry,
    focused: bool,
    theme: &Theme,
    width: u16,
) -> Line<'static> {
    let indent = " ".repeat(indent_for(entry.level));
    // Marker matches the palette layout (`format_modal_row`): `"› "`
    // when focused, `"  "` otherwise — same cell width either way so
    // the indent doesn't jitter as focus moves.
    let marker = if focused { "› " } else { "  " };
    let prefix_w = UnicodeWidthStr::width(marker) + UnicodeWidthStr::width(indent.as_str());
    let text_budget = (width as usize).saturating_sub(prefix_w);
    let text = truncate_to_cells(&entry.text, text_budget);
    let label = format!("{marker}{indent}{text}");

    if focused {
        // Pad the row to `width` so the selection highlight extends to
        // the right edge of the list, matching the palette.
        let label_w = UnicodeWidthStr::width(label.as_str());
        let pad = (width as usize).saturating_sub(label_w);
        let padded = format!("{label}{}", " ".repeat(pad));
        Line::from(Span::styled(padded, theme.modal_item_selected))
    } else {
        let style = theme
            .heading_style(entry.level)
            .remove_modifier(Modifier::UNDERLINED)
            .add_modifier(Modifier::BOLD);
        Line::from(Span::styled(label, style))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn h(level: HeadingLevel, text: &str, line: usize, target: usize) -> HeadingEntry {
        HeadingEntry {
            level,
            text: text.to_owned(),
            buffer_line: line,
            target_scroll: target,
        }
    }

    fn sample() -> Vec<HeadingEntry> {
        vec![
            h(HeadingLevel::H1, "Overview", 0, 0),
            h(HeadingLevel::H2, "Installation", 4, 8),
            h(HeadingLevel::H2, "Usage", 12, 20),
            h(HeadingLevel::H3, "Quick start", 16, 30),
        ]
    }

    #[test]
    fn open_with_preselected_focus_clamps_to_bounds() {
        let st = SectionPickerState::open(sample(), 99);
        assert_eq!(st.focused, 3);
        let st = SectionPickerState::open(Vec::new(), 0);
        assert_eq!(st.focused, 0);
    }

    #[test]
    fn empty_query_lists_all_entries_in_document_order() {
        let mut st = SectionPickerState::open(sample(), 0);
        st.refresh_display();
        assert_eq!(st.display_indices, vec![0, 1, 2, 3]);
    }

    #[test]
    fn typing_filters_via_fuzzy_match() {
        let mut st = SectionPickerState::open(sample(), 0);
        for c in "quick".chars() {
            st.handle_key(&key(KeyCode::Char(c)));
        }
        st.refresh_display();
        assert_eq!(st.display_indices, vec![3]);
        assert_eq!(st.focused_entry().unwrap().text, "Quick start");
    }

    #[test]
    fn arrow_down_emits_preview_for_next_entry() {
        let mut st = SectionPickerState::open(sample(), 0);
        let resp = st.handle_key(&key(KeyCode::Down));
        assert_eq!(resp, SectionPickerResponse::Preview { target_scroll: 8 });
        assert_eq!(st.focused, 1);
    }

    #[test]
    fn arrow_up_at_top_is_noop() {
        let mut st = SectionPickerState::open(sample(), 0);
        let resp = st.handle_key(&key(KeyCode::Up));
        assert_eq!(resp, SectionPickerResponse::Continue);
        assert_eq!(st.focused, 0);
    }

    #[test]
    fn enter_emits_selected_for_focused_entry() {
        let mut st = SectionPickerState::open(sample(), 2);
        let resp = st.handle_key(&key(KeyCode::Enter));
        assert_eq!(
            resp,
            SectionPickerResponse::Selected {
                buffer_line: 12,
                target_scroll: 20,
            }
        );
    }

    #[test]
    fn enter_with_no_matches_is_continue() {
        let mut st = SectionPickerState::open(sample(), 0);
        for c in "zzznotanyheading".chars() {
            st.handle_key(&key(KeyCode::Char(c)));
        }
        let resp = st.handle_key(&key(KeyCode::Enter));
        assert_eq!(resp, SectionPickerResponse::Continue);
    }

    #[test]
    fn escape_cancels() {
        let mut st = SectionPickerState::open(sample(), 0);
        let resp = st.handle_key(&key(KeyCode::Esc));
        assert_eq!(resp, SectionPickerResponse::Cancelled);
    }

    #[test]
    fn ctrl_chars_do_not_pollute_query() {
        let mut st = SectionPickerState::open(sample(), 0);
        let ctrl_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
        st.handle_key(&ctrl_g);
        assert!(st.query.is_empty());
    }

    #[test]
    fn center_on_focused_pulls_focus_into_middle_of_visible_window() {
        let entries: Vec<_> = (0..50)
            .map(|i| h(HeadingLevel::H2, &format!("Heading {i}"), i, i * 2))
            .collect();
        let mut st = SectionPickerState::open(entries, 30);
        // Simulate observe() seeing a 10-row visible window over 50 entries.
        st.scroll_state.observe(50, 10);
        st.center_on_focused();
        // visible/2 = 5, so scroll = 30 - 5 = 25; focused (30) is in [25, 35).
        assert_eq!(st.scroll_state.scroll, 25);
    }

    #[test]
    fn center_on_focused_clamps_when_focus_near_end() {
        let entries: Vec<_> = (0..50)
            .map(|i| h(HeadingLevel::H2, &format!("Heading {i}"), i, i * 2))
            .collect();
        let mut st = SectionPickerState::open(entries, 48);
        st.scroll_state.observe(50, 10);
        st.center_on_focused();
        // Want scroll = 48 - 5 = 43, but max_scroll = 50 - 10 = 40.  Clamp.
        assert_eq!(st.scroll_state.scroll, 40);
    }

    #[test]
    fn center_on_focused_clamps_when_focus_near_start() {
        let entries: Vec<_> = (0..50)
            .map(|i| h(HeadingLevel::H2, &format!("Heading {i}"), i, i * 2))
            .collect();
        let mut st = SectionPickerState::open(entries, 2);
        st.scroll_state.observe(50, 10);
        st.center_on_focused();
        // saturating_sub keeps scroll at 0 when focused < visible/2.
        assert_eq!(st.scroll_state.scroll, 0);
    }

    #[test]
    fn click_on_row_returns_selected_for_that_entry() {
        use ratatui::layout::Rect;
        let mut st = SectionPickerState::open(sample(), 0);
        // Simulate a render that placed the 4-row list at (10, 5, w=30, h=4)
        // with no scroll offset.
        st.list_area = Some(Rect {
            x: 10,
            y: 5,
            width: 30,
            height: 4,
        });
        // Click row 5 + 2 = the 3rd visible row → entries[2] = "Usage".
        let resp = st.handle_click(20, 7);
        assert_eq!(
            resp,
            SectionPickerResponse::Selected {
                buffer_line: 12,
                target_scroll: 20,
            }
        );
    }

    #[test]
    fn click_outside_list_area_is_continue() {
        use ratatui::layout::Rect;
        let mut st = SectionPickerState::open(sample(), 0);
        st.list_area = Some(Rect {
            x: 10,
            y: 5,
            width: 30,
            height: 4,
        });
        // Above the list.
        assert_eq!(st.handle_click(20, 4), SectionPickerResponse::Continue);
        // Below the list.
        assert_eq!(st.handle_click(20, 9), SectionPickerResponse::Continue);
        // Left of the list.
        assert_eq!(st.handle_click(9, 6), SectionPickerResponse::Continue);
    }

    #[test]
    fn click_on_empty_row_below_last_entry_is_continue() {
        use ratatui::layout::Rect;
        let mut st = SectionPickerState::open(sample(), 0);
        // List area has 10 rows of space but only 4 entries.  Clicking
        // row 8 (past the last entry) should be a no-op.
        st.list_area = Some(Rect {
            x: 10,
            y: 5,
            width: 30,
            height: 10,
        });
        assert_eq!(st.handle_click(20, 5 + 8), SectionPickerResponse::Continue);
    }

    #[test]
    fn click_respects_scroll_offset() {
        use ratatui::layout::Rect;
        // 50 entries, viewport of 10, scrolled by 20: the first visible
        // row maps to entries[20], not entries[0].
        let entries: Vec<_> = (0..50)
            .map(|i| h(HeadingLevel::H2, &format!("Heading {i}"), i, i * 2))
            .collect();
        let mut st = SectionPickerState::open(entries, 0);
        st.scroll_state.observe(50, 10);
        st.scroll_state.scroll = 20;
        st.list_area = Some(Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 10,
        });
        let resp = st.handle_click(5, 0);
        assert_eq!(
            resp,
            SectionPickerResponse::Selected {
                buffer_line: 20,
                target_scroll: 40,
            }
        );
    }

    #[test]
    fn typing_resets_focus_and_emits_preview_of_first_match() {
        let mut st = SectionPickerState::open(sample(), 0);
        // Focus a later entry first.
        st.handle_key(&key(KeyCode::Down));
        st.handle_key(&key(KeyCode::Down));
        let resp = st.handle_key(&key(KeyCode::Char('u')));
        // "u" matches several entries; focus snaps back to the top of
        // the new ranked list and we preview that entry.
        assert_eq!(st.focused, 0);
        assert!(matches!(resp, SectionPickerResponse::Preview { .. }));
    }
}
