//! Shared chrome and fuzzy-filter helpers for fuzzy-searchable list
//! modals (command palette, section picker, and any future similar
//! widget).
//!
//! Both pickers share the same skeleton: a centred modal with a top-
//! anchored input row, a divider, a scrollable result list, and a
//! right-edge scrollbar.  Only the body content (palette rows with
//! section headers vs heading rows styled by level) and the width-
//! sizing function differ.  This module owns the geometry, the frame
//! drawing, the input-row paint, the divider, the scrollbar render
//! pass, and the nucleo-matcher scoring loop.  Callers fill in body
//! content into the [`SearchableListLayout::list_area`] returned by
//! [`draw_searchable_list_chrome`].

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::config::Theme;
use crate::ui::scroll_container::{
    centered_rect_for_content, draw_frame, ContentSize, FrameOpts, ModalKind, ScrollContainerState,
    VERTICAL_CHROME_ROWS,
};

/// Cap on the scrolling list height shared by every searchable-list
/// modal.  Keeping the cap identical across pickers means the modal
/// height is stable when the user switches between them — no visual
/// reflow.
pub const MAX_LIST_ROWS: u16 = 20;

/// Rows the input + divider occupy.  Pinned above the scrolling list
/// so they don't move as the body scrolls.
const PINNED_TOP: u16 = 2;

/// Chrome inputs.  `content_width` is the body width the caller wants
/// (typically the longest entry row); `row_count` is the unfiltered
/// list length used to size the scrolling region.  Both values are
/// reused as the [`ContentSize`] fed to [`centered_rect_for_content`].
pub struct SearchableListChrome<'a> {
    /// Title shown in the frame top row (e.g. "Command Palette").
    pub title: &'a str,
    /// Current search query — rendered after the `›` prompt glyph.
    pub query: &'a str,
    /// Body content width in display columns.
    pub content_width: u16,
    /// Total list row count for scrollbar sizing.  Pass the number of
    /// display rows the caller actually has, NOT the unfiltered entry
    /// count — the scrollbar needs to track the visible list.
    pub row_count: u16,
    /// Whether the cursor glyph in the input row should currently be
    /// visible (the App's blink phase).
    pub cursor_visible: bool,
    pub theme: &'a Theme,
}

/// Layout produced by [`draw_searchable_list_chrome`].  Callers paint
/// their body content into [`Self::list_area`]; the chrome helper has
/// already drawn the frame, input row, and divider.  The scrollbar is
/// rendered separately — call [`render_searchable_list_scrollbar`]
/// after any post-chrome scroll adjustment so the bar reflects the
/// final scroll value rather than the pre-adjustment one.
pub struct SearchableListLayout {
    /// Rect the caller should render its list rows into.
    pub list_area: Rect,
    /// Absolute terminal rect of the `esc` close affordance — cache
    /// this on the picker state for later click hit-testing.  `None`
    /// when the chrome was too small to render the hint.
    pub esc_hit_rect: Option<Rect>,
    /// Column index where [`render_searchable_list_scrollbar`] should
    /// paint the scrollbar.
    pub scrollbar_col: u16,
}

/// Draw the centred modal frame, input row, divider, and (when the
/// body overflows) the scrollbar.  Returns the list rect the caller
/// should render entries into, plus the cached `esc` hint rect for
/// click hit-testing.
///
/// When the frame body is too small to render usefully (height < 2 or
/// zero width), the returned [`SearchableListLayout::list_area`] has
/// `height == 0` and callers should bail out of their list-rendering
/// pass.  The `esc_hit_rect` and `scrollbar_col` are still populated so
/// the caller can cache them for click hit-testing.
///
/// Side effects:
/// - Mutates `scroll_state` via `observe()` so subsequent
///   `ensure_visible`/`scroll_by` calls are clamped against the new
///   visible-window size.
/// - Paints `area` of `buf`.
pub fn draw_searchable_list_chrome(
    area: Rect,
    buf: &mut Buffer,
    chrome: SearchableListChrome<'_>,
    scroll_state: &mut ScrollContainerState,
) -> Option<SearchableListLayout> {
    let row_count = chrome.row_count.max(1);
    let scrolling_height = row_count.min(MAX_LIST_ROWS);
    let content = ContentSize {
        width: chrome.content_width,
        height: scrolling_height,
        pinned_top: PINNED_TOP,
        pinned_bottom: 0,
        ..Default::default()
    };
    // Top-anchor trick: compute the modal y as if it were at max
    // height so the input row's vertical position doesn't change when
    // the match count shrinks while the user types.  Without this the
    // input row would jitter up and down as filtering changes the
    // body height.
    let max_content = ContentSize {
        height: MAX_LIST_ROWS,
        ..content
    };
    let anchor = centered_rect_for_content(max_content, area);
    let actual = centered_rect_for_content(content, area);
    let max_y = area.y + area.height.saturating_sub(actual.height);
    let modal_area = Rect {
        x: actual.x,
        y: anchor.y.min(max_y),
        width: actual.width,
        height: actual.height,
    };

    let inner_h = modal_area.height.saturating_sub(VERTICAL_CHROME_ROWS);
    let list_height = inner_h.saturating_sub(PINNED_TOP);
    scroll_state.observe(chrome.row_count, list_height);

    let frame = draw_frame(
        modal_area,
        buf,
        FrameOpts {
            title: chrome.title,
            kind: ModalKind::Normal,
            show_close_hint: true,
            content,
            theme: chrome.theme,
        },
    );
    let inner = frame.body;
    if inner.height < 2 || inner.width == 0 {
        return Some(SearchableListLayout {
            list_area: Rect {
                x: inner.x,
                y: inner.y,
                width: 0,
                height: 0,
            },
            esc_hit_rect: frame.esc_hit_rect,
            scrollbar_col: frame.scrollbar_col,
        });
    }

    // Input row — flush against the modal body (no input chip bg).
    let input_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    let prompt = Span::styled("› ", chrome.theme.modal_item);
    let typed = Span::styled(chrome.query.to_owned(), chrome.theme.modal_item);
    let mut spans = vec![prompt, typed];
    if chrome.cursor_visible {
        let cursor_style = ratatui::style::Style::default()
            .fg(chrome.theme.palette.primary)
            .bg(chrome.theme.palette.surface_elevated)
            .add_modifier(Modifier::BOLD);
        spans.push(Span::styled("▏", cursor_style));
    }
    Paragraph::new(Line::from(spans))
        .style(chrome.theme.modal_bg)
        .render(input_area, buf);

    // Divider between input and list.
    let divider_style = ratatui::style::Style::default()
        .fg(chrome.theme.palette.secondary)
        .bg(chrome.theme.palette.surface_elevated);
    let divider_y = inner.y + 1;
    for x in inner.x..(inner.x + inner.width) {
        buf[(x, divider_y)].set_symbol("─").set_style(divider_style);
    }

    let list_area = Rect {
        x: inner.x,
        y: inner.y + PINNED_TOP,
        width: inner.width,
        height: list_height,
    };

    Some(SearchableListLayout {
        list_area,
        esc_hit_rect: frame.esc_hit_rect,
        scrollbar_col: frame.scrollbar_col,
    })
}

/// Paint the scrollbar for a searchable-list modal at the column the
/// chrome helper reserved.  Call this AFTER any post-chrome scroll
/// adjustment (e.g. centring the focused row on first open) so the bar
/// reflects the final scroll value.  No-op when the body fits without
/// scrolling or the list area is empty.
pub fn render_searchable_list_scrollbar(
    layout: &SearchableListLayout,
    scroll_state: &ScrollContainerState,
    theme: &Theme,
    buf: &mut Buffer,
) {
    if scroll_state.max_scroll() == 0 || layout.list_area.height == 0 {
        return;
    }
    let bar_area = Rect {
        x: layout.scrollbar_col,
        y: layout.list_area.y,
        width: 1,
        height: layout.list_area.height,
    };
    crate::ui::scrollbar::render_for_scroll_state(bar_area, scroll_state, theme, buf);
}

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
}
