//! Shared, generic fuzzy-searchable list component for modal widgets
//! (command palette, section picker, theme picker, export-theme).
//!
//! [`SearchableList`] owns the query / focus / filter / scroll state machine
//! plus the input-row, divider, and scrollable-list rendering.  Callers supply
//! the items, a fuzzy-haystack extractor, and a per-row formatter; the
//! component handles typing, arrow navigation, paging, paste, wheel scroll,
//! and click-to-submit, emitting a [`ListEvent`] the modal adapter acts on.
//!
//! The component is *embeddable*: [`SearchableList::render`] paints into any
//! `Rect`, so a parent can stack extra chrome above or below it (a Dark-mode
//! toggle, a name field, buttons).  For the common "input + list fills the
//! whole modal" case, [`draw_searchable_list_modal`] composes the centred,
//! top-anchored frame around it.  [`fuzzy_filter`] is the shared nucleo
//! scoring loop.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::config::Theme;
use crate::ui::scroll_container::{
    centered_rect_for_content, draw_frame, ContentSize, FrameOpts, ModalKind, ScrollContainerState,
    VERTICAL_CHROME_ROWS,
};

/// Default cap on the scrolling list height for searchable-list modals.
/// Callers pass their own cap via [`ListModalOpts::max_list_rows`]; the command
/// palette uses this default, while the section picker passes `u16::MAX` so it
/// grows to fill the available height instead.
pub const MAX_LIST_ROWS: u16 = 20;

/// Rows the input + divider occupy in a [`draw_searchable_list_modal`] body.
/// Pinned above the scrolling list so they don't move as the body scrolls.
const PINNED_TOP: u16 = 2;

/// Score `items` against `query` using nucleo's smart-case fuzzy
/// matcher and return their indices ordered by descending score, with
/// stable index-order tie-breaking.  Items that don't match are
/// excluded.  Pass `key` to extract the haystack string from each
/// item.
///
/// The caller's items are expected to be in their canonical order
/// already (alphabetical for the palette, document order for the
/// section picker); index-order tie-breaking preserves whatever that
/// canonical order is.
pub fn fuzzy_filter<T, F: Fn(&T) -> &str>(items: &[T], query: &str, key: F) -> Vec<usize> {
    let mut matcher = Matcher::default();
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(usize, u32)> = Vec::new();
    let mut buf: Vec<char> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        buf.clear();
        let haystack = Utf32Str::new(key(item), &mut buf);
        if let Some(score) = pattern.score(haystack, &mut matcher) {
            scored.push((idx, score));
        }
    }
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.into_iter().map(|(i, _)| i).collect()
}

// ── Generic searchable-list component ───────────────────────────────────────
//
// `SearchableList<T>` owns the query/focus/filter/scroll state machine and the
// input + list rendering shared by every fuzzy-searchable modal (command
// palette, section picker, theme picker, export-theme).  Callers supply the
// items, a fuzzy-haystack extractor, and a per-row formatter; the component
// handles typing, arrow navigation, paging, paste, wheel scroll, and
// click-to-submit, emitting a [`ListEvent`] the modal adapter acts on.
//
// The component is *embeddable*: [`SearchableList::render`] paints an input
// row, a divider, and the scrollable list into whatever `Rect` the caller
// gives it, so a parent can stack extra chrome (a Dark-mode toggle, a name
// field, buttons) above or below it.  For the common "input + list fills the
// whole modal" case, [`anchor_searchable_modal`] computes the centred,
// top-anchored frame rect to draw into.

/// One row in the visible (post-filter) list: either a non-selectable section
/// header (command palette only) or a selectable item, carrying its index into
/// the caller's `items`.  Returned by a [`SearchableList::with_sections`]
/// builder to describe the empty-query layout.
#[derive(Debug, Clone)]
pub enum VisibleRow {
    Header(String),
    Item(usize),
}

/// Context handed to the caller's row formatter for each rendered row.
pub enum RowCtx<'a, T> {
    /// A non-selectable section header.  `width` is the list column width.
    Header { title: &'a str, width: u16 },
    /// A selectable item row.  `focused` is true for the highlighted row.
    Item {
        item: &'a T,
        focused: bool,
        width: u16,
    },
}

/// Outcome of dispatching input to a [`SearchableList`].  The modal adapter
/// maps these onto its domain (dispatching an action, previewing a theme,
/// arming a scroll, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListEvent {
    /// No transition — keep rendering.
    Continue,
    /// Esc — caller should close / revert.
    Cancelled,
    /// The focused *item* changed (carries its index into `items`).  Drives
    /// live preview (theme picker, section picker).  Emitted only when the
    /// resolved focused item actually differs from the previous one, so a
    /// caller can preview unconditionally without re-deduping.
    FocusChanged(usize),
    /// Enter or a row click (carries the item index into `items`).
    Submitted(usize),
}

/// Builder that produces the empty-query sectioned layout (command palette).
type SectionsFn<T> = fn(&[T]) -> Vec<VisibleRow>;

/// How focus is resolved after the query changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPolicy {
    /// Focus jumps to the top match (command palette, section picker,
    /// export-theme list).
    ResetToTop,
    /// Focus stays on the same item when it survives the new filter (theme
    /// picker — keeps the live preview stable as the query is broadened).
    PreserveByIdentity,
}

/// What the first render after a focus change should do to bring the focused
/// row on-screen.  Consumed once, then cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reveal {
    None,
    /// Scroll the minimum amount to make the focused row visible.
    Ensure,
    /// Centre the focused row in the visible window (section picker open).
    Center,
}

/// Chrome parameters for [`SearchableList::render`].
pub struct ListChrome<'a> {
    pub theme: &'a Theme,
    /// App blink phase for the input cursor.
    pub cursor_visible: bool,
    /// Whether the input field is the active one (the cursor is only drawn
    /// when both this and `cursor_visible` are true).  Pass `true` for modals
    /// whose only field is the list query; the export modal passes `false`
    /// when its Name field has focus.
    pub field_focused: bool,
    /// Muted hint shown after the prompt when the query is empty.
    pub placeholder: &'a str,
    /// Shown in place of the list when nothing matches (e.g. "(no matches)").
    pub empty_text: &'a str,
    /// Column the scrollbar is painted in (from the frame layout).
    pub scrollbar_col: u16,
}

/// Generic state + rendering for a fuzzy-searchable list.
#[derive(Debug, Clone)]
pub struct SearchableList<T> {
    items: Vec<T>,
    key: fn(&T) -> &str,
    section_titles: Option<SectionsFn<T>>,
    focus_policy: FocusPolicy,

    query: String,
    /// Index into `visible` — always points at a [`VisibleRow::Item`] when one
    /// exists.
    focused: usize,
    visible: Vec<VisibleRow>,
    matched_for_query: Option<String>,
    scroll: ScrollContainerState,

    /// Cached list rect for click hit-testing; set each render.
    list_area: Option<Rect>,
    reveal: Reveal,
    /// For [`FocusPolicy::PreserveByIdentity`]: the key of the item focused
    /// before the filter changed, restored on the next refresh.
    pending_focus_key: Option<String>,
    /// Last focused item index, for [`ListEvent::FocusChanged`] de-duping.
    last_focused_item: Option<usize>,
}

impl<T> SearchableList<T> {
    /// Build a list over `items`, fuzzy-matching on `key`.  Defaults to a flat
    /// list (no section headers) and [`FocusPolicy::ResetToTop`].
    pub fn new(items: Vec<T>, key: fn(&T) -> &str) -> Self {
        let mut list = Self {
            items,
            key,
            section_titles: None,
            focus_policy: FocusPolicy::ResetToTop,
            query: String::new(),
            focused: 0,
            visible: Vec::new(),
            matched_for_query: None,
            scroll: ScrollContainerState::default(),
            list_area: None,
            reveal: Reveal::None,
            pending_focus_key: None,
            last_focused_item: None,
        };
        list.refresh();
        list.last_focused_item = list.focused_item_index();
        list
    }

    /// Supply an empty-query sectioned layout (command palette).  `build`
    /// returns the display rows — headers interleaved with item indices — for
    /// the empty-query view.  A non-empty query always falls back to the flat
    /// fuzzy-ranked list.
    pub fn with_sections(mut self, build: SectionsFn<T>) -> Self {
        self.section_titles = Some(build);
        self.invalidate();
        self.refresh();
        self.last_focused_item = self.focused_item_index();
        self
    }

    /// Set the focus policy (default [`FocusPolicy::ResetToTop`]).
    pub fn with_focus_policy(mut self, policy: FocusPolicy) -> Self {
        self.focus_policy = policy;
        self
    }

    // ── Query ───────────────────────────────────────────────────────────────

    #[allow(dead_code)] // accessor used by adapter tests
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// Number of selectable items in the current (filtered) view.
    #[allow(dead_code)] // used by widget tests
    pub fn match_count(&mut self) -> usize {
        self.refresh();
        self.visible
            .iter()
            .filter(|r| matches!(r, VisibleRow::Item(_)))
            .count()
    }

    /// Number of display rows (items + any headers) in the current view.  Used
    /// to size the scrolling region.
    pub fn visible_len(&mut self) -> usize {
        self.refresh();
        self.visible.len()
    }

    // ── Focus ─────────────────────────────────────────────────────────────

    /// Index into `items` of the focused row, or `None` when nothing matches.
    pub fn focused_item_index(&self) -> Option<usize> {
        match self.visible.get(self.focused) {
            Some(VisibleRow::Item(i)) => Some(*i),
            _ => None,
        }
    }

    /// The focused item, or `None` when nothing matches.
    pub fn focused_item(&self) -> Option<&T> {
        self.focused_item_index().map(|i| &self.items[i])
    }

    /// Pre-focus the item at `item_idx` (an index into `items`) and arrange for
    /// the next render to scroll it into view.  No-op if the item isn't in the
    /// current view.
    pub fn focus_item(&mut self, item_idx: usize) {
        self.refresh();
        if let Some(pos) = self
            .visible
            .iter()
            .position(|r| matches!(r, VisibleRow::Item(i) if *i == item_idx))
        {
            self.focused = pos;
            self.last_focused_item = Some(item_idx);
            self.reveal = Reveal::Ensure;
        }
    }

    /// Pre-focus the first item matching `pred`.
    pub fn focus_matching<P: Fn(&T) -> bool>(&mut self, pred: P) {
        if let Some(idx) = self.items.iter().position(pred) {
            self.focus_item(idx);
        }
    }

    /// Centre the focused row in the visible window on the next render (used
    /// when a picker opens on a row deep in a long list).
    pub fn request_center(&mut self) {
        self.reveal = Reveal::Center;
    }

    /// Replace the item set (theme picker mode switch): resets the query and
    /// focus.  Re-focus afterwards with [`focus_item`](Self::focus_item) /
    /// [`focus_matching`](Self::focus_matching).
    pub fn set_items(&mut self, items: Vec<T>) {
        self.items = items;
        self.query.clear();
        self.matched_for_query = None;
        self.pending_focus_key = None;
        self.focused = 0;
        self.scroll.scroll = 0;
        self.refresh();
        self.last_focused_item = self.focused_item_index();
    }

    // ── Input ─────────────────────────────────────────────────────────────

    /// Dispatch a key event.  Esc cancels, Enter submits, Up/Down move focus
    /// (skipping headers), typing/backspace filter, PgUp/PgDn/Home/End scroll.
    pub fn handle_key(&mut self, key: &KeyEvent) -> ListEvent {
        if self.scroll.handle_paging_key(key) {
            return ListEvent::Continue;
        }
        match key.code {
            KeyCode::Esc => ListEvent::Cancelled,
            KeyCode::Enter => {
                self.refresh();
                match self.focused_item_index() {
                    Some(i) => ListEvent::Submitted(i),
                    None => ListEvent::Continue,
                }
            }
            KeyCode::Up => {
                self.refresh();
                if let Some(prev) = self.prev_item_row(self.focused) {
                    self.focused = prev;
                    self.scroll.ensure_visible(self.focused as u16);
                    return self.focus_event();
                }
                ListEvent::Continue
            }
            KeyCode::Down => {
                self.refresh();
                if let Some(next) = self.next_item_row(self.focused) {
                    self.focused = next;
                    self.scroll.ensure_visible(self.focused as u16);
                    return self.focus_event();
                }
                ListEvent::Continue
            }
            KeyCode::Backspace => {
                if self.query.pop().is_some() {
                    self.invalidate();
                    self.refresh();
                    return self.focus_event();
                }
                ListEvent::Continue
            }
            KeyCode::Char(c) => {
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    return ListEvent::Continue;
                }
                self.query.push(c);
                self.invalidate();
                self.refresh();
                self.focus_event()
            }
            _ => ListEvent::Continue,
        }
    }

    /// Append a bracketed paste to the query (sanitised + flattened) and
    /// re-filter.
    pub fn paste(&mut self, text: &str) -> ListEvent {
        let clean = crate::ui::sanitize_paste(text);
        if clean.is_empty() {
            return ListEvent::Continue;
        }
        self.query.push_str(&clean);
        self.invalidate();
        self.refresh();
        self.focus_event()
    }

    /// Scroll the visible window by `delta` rows without moving focus (mouse
    /// wheel).
    pub fn scroll_by(&mut self, delta: i32) {
        self.scroll.scroll_by(delta);
    }

    /// Hit-test a click against the rendered list.  A click on a populated
    /// item row submits it (same as Enter on that row); clicks elsewhere
    /// return `Continue` so the adapter can route them (e.g. the esc hint).
    pub fn handle_click(&mut self, col: u16, row: u16) -> ListEvent {
        let Some(area) = self.list_area else {
            return ListEvent::Continue;
        };
        let inside = col >= area.x
            && col < area.x.saturating_add(area.width)
            && row >= area.y
            && row < area.y.saturating_add(area.height);
        if !inside {
            return ListEvent::Continue;
        }
        let display_idx = self.scroll.scroll as usize + (row - area.y) as usize;
        match self.visible.get(display_idx) {
            Some(VisibleRow::Item(i)) => ListEvent::Submitted(*i),
            _ => ListEvent::Continue,
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────────

    fn next_item_row(&self, from: usize) -> Option<usize> {
        (from + 1..self.visible.len()).find(|&i| matches!(self.visible[i], VisibleRow::Item(_)))
    }

    fn prev_item_row(&self, from: usize) -> Option<usize> {
        (0..from)
            .rev()
            .find(|&i| matches!(self.visible[i], VisibleRow::Item(_)))
    }

    fn first_item_row(&self) -> usize {
        self.visible
            .iter()
            .position(|r| matches!(r, VisibleRow::Item(_)))
            .unwrap_or(0)
    }

    /// Emit [`ListEvent::FocusChanged`] when the focused item differs from the
    /// last one we reported; otherwise `Continue`.
    fn focus_event(&mut self) -> ListEvent {
        let cur = self.focused_item_index();
        if cur != self.last_focused_item {
            self.last_focused_item = cur;
            if let Some(i) = cur {
                return ListEvent::FocusChanged(i);
            }
        }
        ListEvent::Continue
    }

    fn invalidate(&mut self) {
        if self.focus_policy == FocusPolicy::PreserveByIdentity {
            self.pending_focus_key = self.focused_item().map(|it| (self.key)(it).to_owned());
        }
        self.visible.clear();
        self.matched_for_query = None;
        self.focused = 0;
        self.scroll.scroll = 0;
    }

    fn refresh(&mut self) {
        if self.matched_for_query.as_deref() == Some(self.query.as_str()) {
            return;
        }
        self.visible = if self.query.is_empty() {
            if let Some(build) = self.section_titles {
                build(&self.items)
            } else {
                (0..self.items.len()).map(VisibleRow::Item).collect()
            }
        } else {
            fuzzy_filter(&self.items, &self.query, self.key)
                .into_iter()
                .map(VisibleRow::Item)
                .collect()
        };
        self.matched_for_query = Some(self.query.clone());

        // Resolve focus.
        if let Some(target) = self.pending_focus_key.take() {
            self.focused = self
                .visible
                .iter()
                .position(
                    |r| matches!(r, VisibleRow::Item(i) if (self.key)(&self.items[*i]) == target),
                )
                .unwrap_or_else(|| self.first_item_row());
        } else if !matches!(self.visible.get(self.focused), Some(VisibleRow::Item(_))) {
            self.focused = self.first_item_row();
        }
    }

    fn center_on_focused(&mut self) {
        let visible = self.scroll.last_visible as usize;
        if visible == 0 {
            return;
        }
        let target = self.focused.saturating_sub(visible / 2) as u16;
        self.scroll.scroll = target.min(self.scroll.max_scroll());
    }

    // ── Render ────────────────────────────────────────────────────────────

    /// Render the input row, divider, and scrollable list into `area` (input
    /// at the top row, divider below it, list filling the rest).  `fmt` styles
    /// each row.  Caches the list rect for [`handle_click`](Self::handle_click).
    pub fn render<F>(&mut self, area: Rect, buf: &mut Buffer, chrome: ListChrome<'_>, fmt: F)
    where
        F: Fn(RowCtx<'_, T>) -> Line<'static>,
    {
        self.refresh();
        if area.width == 0 || area.height < 3 {
            self.list_area = None;
            return;
        }
        let theme = chrome.theme;

        // Input row.
        let input_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        render_search_input_row(
            input_area,
            buf,
            theme,
            &self.query,
            chrome.cursor_visible && chrome.field_focused,
            chrome.placeholder,
        );

        // Divider.
        let divider_style = Style::default()
            .fg(theme.palette.primary)
            .bg(theme.palette.surface_elevated);
        let divider_y = area.y + 1;
        for x in area.x..(area.x + area.width) {
            buf[(x, divider_y)].set_symbol("─").set_style(divider_style);
        }

        // List.
        let list_area = Rect {
            x: area.x,
            y: area.y + 2,
            width: area.width,
            height: area.height - 2,
        };
        self.list_area = Some(list_area);
        self.scroll
            .observe(self.visible.len() as u16, list_area.height);
        match std::mem::replace(&mut self.reveal, Reveal::None) {
            Reveal::None => {}
            Reveal::Ensure => self.scroll.ensure_visible(self.focused as u16),
            Reveal::Center => self.center_on_focused(),
        }

        let scroll = self.scroll.scroll as usize;
        let height = list_area.height as usize;
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
        let has_items = self
            .visible
            .iter()
            .any(|r| matches!(r, VisibleRow::Item(_)));
        if !has_items {
            lines.push(Line::from(Span::styled(
                chrome.empty_text.to_owned(),
                theme.modal_item,
            )));
        } else {
            for (offset, row) in self.visible.iter().skip(scroll).take(height).enumerate() {
                let abs = offset + scroll;
                match row {
                    VisibleRow::Header(title) => lines.push(fmt(RowCtx::Header {
                        title,
                        width: list_area.width,
                    })),
                    VisibleRow::Item(i) => lines.push(fmt(RowCtx::Item {
                        item: &self.items[*i],
                        focused: abs == self.focused,
                        width: list_area.width,
                    })),
                }
            }
        }
        Paragraph::new(lines)
            .style(theme.modal_bg)
            .render(list_area, buf);

        if self.scroll.max_scroll() > 0 {
            let bar = Rect {
                x: chrome.scrollbar_col,
                y: list_area.y,
                width: 1,
                height: list_area.height,
            };
            crate::ui::scrollbar::render_for_scroll_state(bar, &self.scroll, theme, buf);
        }
    }
}

/// Paint the `› query` input row, with a muted `placeholder` when the query is
/// empty.  The block cursor is shown when `cursor_on` (blink phase ∧ field
/// focused) and is constant-width across blink phases.
fn render_search_input_row(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    query: &str,
    cursor_on: bool,
    placeholder: &str,
) {
    let mut spans = vec![Span::styled("› ", theme.modal_item)];
    spans.extend(crate::ui::cursor::text_field_spans(
        query,
        query.chars().count(),
        cursor_on,
        theme.modal_item,
        theme.cursor,
    ));
    if query.is_empty() && !placeholder.is_empty() {
        let muted = Style::default()
            .fg(theme.palette.text_muted)
            .bg(theme.palette.surface_elevated);
        spans.push(Span::styled(placeholder.to_owned(), muted));
    }
    Paragraph::new(Line::from(spans))
        .style(theme.modal_bg)
        .render(area, buf);
}

/// Geometry for a searchable-list modal whose body is `pinned_top` rows of
/// fixed chrome, a scrolling list, and `pinned_bottom` rows of fixed chrome.
///
/// Returns the centred modal rect using the top-anchor trick (positioned as if
/// the list were at its full `max_list_rows` height so the input row doesn't
/// jump as the match count shrinks while typing), plus the [`ContentSize`] to
/// hand to [`draw_frame`].
pub struct SearchableModalGeometry {
    pub modal_area: Rect,
    pub content: ContentSize,
}

#[allow(clippy::too_many_arguments)]
pub fn anchor_searchable_modal(
    area: Rect,
    content_width: u16,
    total_rows: u16,
    max_list_rows: u16,
    pinned_top: u16,
    pinned_bottom: u16,
    vertical_pad: u16,
) -> SearchableModalGeometry {
    let chrome_rows = pinned_top + pinned_bottom + VERTICAL_CHROME_ROWS;
    let height_budget = area
        .height
        .saturating_sub(2 * vertical_pad)
        .saturating_sub(chrome_rows)
        .max(1);
    let max_rows = max_list_rows.min(height_budget);
    let scrolling_height = total_rows.max(1).min(max_rows);
    let content = ContentSize {
        width: content_width,
        height: scrolling_height,
        pinned_top,
        pinned_bottom,
        ..Default::default()
    };
    let anchor = centered_rect_for_content(
        ContentSize {
            height: max_rows,
            ..content
        },
        area,
    );
    let actual = centered_rect_for_content(content, area);
    let max_y = area.y + area.height.saturating_sub(actual.height);
    let modal_area = Rect {
        x: actual.x,
        y: anchor.y.min(max_y),
        width: actual.width,
        height: actual.height,
    };
    SearchableModalGeometry {
        modal_area,
        content,
    }
}

/// Options for [`draw_searchable_list_modal`] — the "input + list fills the
/// whole modal" composition used by the command palette and section picker.
pub struct ListModalOpts<'a> {
    pub title: &'a str,
    /// Body width in columns (longest row).
    pub content_width: u16,
    /// Cap on the scrolling list height ([`MAX_LIST_ROWS`], or `u16::MAX` to
    /// fill the available height).
    pub max_list_rows: u16,
    /// Blank rows kept above and below the modal.
    pub vertical_pad: u16,
    pub theme: &'a Theme,
    pub cursor_visible: bool,
    pub placeholder: &'a str,
    pub empty_text: &'a str,
}

/// Draw a centred, top-anchored searchable-list modal: frame + input row +
/// divider + scrollable list.  Returns the `esc` close-hint rect (for click
/// hit-testing).  The list body is styled by `fmt`.
pub fn draw_searchable_list_modal<T, F>(
    list: &mut SearchableList<T>,
    area: Rect,
    buf: &mut Buffer,
    opts: ListModalOpts<'_>,
    fmt: F,
) -> Option<Rect>
where
    F: Fn(RowCtx<'_, T>) -> Line<'static>,
{
    let total_rows = list.visible_len() as u16;
    let geom = anchor_searchable_modal(
        area,
        opts.content_width,
        total_rows,
        opts.max_list_rows,
        PINNED_TOP,
        0,
        opts.vertical_pad,
    );
    let frame = draw_frame(
        geom.modal_area,
        buf,
        FrameOpts {
            title: opts.title,
            kind: ModalKind::Normal,
            show_close_hint: true,
            content: geom.content,
            theme: opts.theme,
        },
    );
    list.render(
        frame.body,
        buf,
        ListChrome {
            theme: opts.theme,
            cursor_visible: opts.cursor_visible,
            field_focused: true,
            placeholder: opts.placeholder,
            empty_text: opts.empty_text,
            scrollbar_col: frame.scrollbar_col,
        },
        fmt,
    );
    frame.esc_hit_rect
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_filter_empty_query_matches_every_item_in_order() {
        let items = vec!["alpha", "bravo", "charlie"];
        let result = fuzzy_filter(&items, "", |s| s);
        assert_eq!(result, vec![0, 1, 2]);
    }

    #[test]
    fn fuzzy_filter_keeps_matches_and_drops_non_matches() {
        // Two items contain the query characters; one is unrelated.
        // Only assert the membership and exclusion contract — leave
        // the relative ranking of two matches to nucleo's scorer (its
        // tie-breaking heuristics are an implementation detail).
        let items = vec!["preview", "copy save", "save it"];
        let result = fuzzy_filter(&items, "save", |s| s);
        assert!(!result.contains(&0), "non-matches must be excluded");
        assert!(result.contains(&1) && result.contains(&2));
    }

    #[test]
    fn fuzzy_filter_excludes_non_matches() {
        let items = vec!["alpha", "bravo"];
        let result = fuzzy_filter(&items, "zzzz", |s| s);
        assert!(result.is_empty());
    }

    #[test]
    fn fuzzy_filter_breaks_score_ties_by_input_order() {
        // Two strings that fuzzy-match identically; the first wins.
        let items = vec!["foo", "foo"];
        let result = fuzzy_filter(&items, "foo", |s| s);
        assert_eq!(result, vec![0, 1]);
    }

    // ── SearchableList component ─────────────────────────────────────────────

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn list() -> SearchableList<String> {
        let items: Vec<String> = (0..30).map(|i| format!("item-{i:02}")).collect();
        SearchableList::new(items, |s: &String| s.as_str())
    }

    /// Render once so the list observes its visible-window size; return the
    /// rendered buffer as a flat string and the modal's top-edge row.
    fn render(l: &mut SearchableList<String>, w: u16, h: u16) -> (String, u16) {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_searchable_list_modal(
                    l,
                    area,
                    frame.buffer_mut(),
                    ListModalOpts {
                        title: "T",
                        content_width: 20,
                        max_list_rows: MAX_LIST_ROWS,
                        vertical_pad: 0,
                        theme,
                        cursor_visible: true,
                        placeholder: "Search…",
                        empty_text: "(none)",
                    },
                    |ctx| match ctx {
                        RowCtx::Item { item, .. } => Line::from(Span::raw(item.clone())),
                        RowCtx::Header { title, .. } => Line::from(Span::raw(title.to_owned())),
                    },
                );
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let flat: String = buf
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        let top = (0..h)
            .find(|&y| (0..w).any(|x| buf[(x, y)].symbol() == "─"))
            .unwrap_or(0);
        (flat, top)
    }

    #[test]
    fn click_on_a_row_submits_that_item() {
        let mut l = list();
        let area = l.list_area_after_render(80, 24);
        // First visible row is the top of the list (scroll 0).
        let resp = l.handle_click(area.x + 1, area.y);
        assert_eq!(resp, ListEvent::Submitted(0));
        // A click on the third visible row submits the third item.
        let resp = l.handle_click(area.x + 1, area.y + 2);
        assert_eq!(resp, ListEvent::Submitted(2));
    }

    #[test]
    fn click_outside_the_list_is_continue() {
        let mut l = list();
        let area = l.list_area_after_render(80, 24);
        // Above the list.
        assert_eq!(
            l.handle_click(area.x + 1, area.y.saturating_sub(2)),
            ListEvent::Continue
        );
    }

    #[test]
    fn focus_changed_only_fires_when_the_item_actually_changes() {
        let mut l = list();
        render(&mut l, 80, 24);
        // Down moves to a new item → FocusChanged.
        assert_eq!(
            l.handle_key(&key(KeyCode::Down)),
            ListEvent::FocusChanged(1)
        );
        // Up back to item 0 → FocusChanged again (different item).
        assert_eq!(l.handle_key(&key(KeyCode::Up)), ListEvent::FocusChanged(0));
        // Up at the top can't move → Continue (no spurious preview).
        assert_eq!(l.handle_key(&key(KeyCode::Up)), ListEvent::Continue);
    }

    #[test]
    fn placeholder_shows_only_when_query_empty() {
        let mut l = list();
        let (flat, _) = render(&mut l, 80, 24);
        assert!(
            flat.contains("Search…"),
            "placeholder shown for empty query"
        );
        l.handle_key(&key(KeyCode::Char('x')));
        let (flat, _) = render(&mut l, 80, 24);
        assert!(!flat.contains("Search…"), "placeholder hidden once typing");
    }

    #[test]
    fn modal_top_edge_is_stable_while_filtering() {
        // The top-anchor trick keeps the input row from jumping as the match
        // count shrinks.  Both the full list and a broad query hit the row cap.
        let mut l = list();
        let (_, top_empty) = render(&mut l, 80, 40);
        l.handle_key(&key(KeyCode::Char('i'))); // matches every "item-*"
        let (_, top_typed) = render(&mut l, 80, 40);
        assert_eq!(
            top_empty, top_typed,
            "modal top must not move when filtering"
        );
    }

    impl SearchableList<String> {
        /// Test helper: render once and return the cached list rect.
        fn list_area_after_render(&mut self, w: u16, h: u16) -> Rect {
            render(self, w, h);
            self.list_area.expect("list rendered")
        }
    }
}
