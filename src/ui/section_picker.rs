//! "Go to section" modal — a fuzzy-searchable list of every heading in the
//! current document.  Built on the shared [`SearchableList`] component
//! ([`crate::ui::searchable_list`]); this module only supplies the
//! per-document heading entries, the heading-styled row formatter, and the
//! modal framing.  Each row renders in its heading-level style (fg + bold,
//! indent by depth) so the list visually resembles the document's outline.
//!
//! Selection is live-previewed: the modal adapter maps the component's
//! [`ListEvent::FocusChanged`](crate::ui::searchable_list::ListEvent::FocusChanged) to a debounced viewport reposition and
//! [`ListEvent::Submitted`](crate::ui::searchable_list::ListEvent::Submitted) to an immediate jump.  All the bookkeeping
//! (`target_scroll` precomputed per entry, current-section preselection)
//! happens in `App::open_section_picker`.

use pulldown_cmark::HeadingLevel;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::config::Theme;
use crate::ui::modal_row::truncate_to_cells;
use crate::ui::searchable_list::{
    draw_searchable_list_modal, FocusPolicy, ListModalOpts, RowCtx, SearchableList,
};

/// One heading in the document.  Constructed by
/// [`App::open_section_picker`](crate::app::App) so the picker doesn't need to
/// know how to walk `ParsedDoc` itself.
///
/// `target_scroll` is the `EditorState::scroll` value that puts the heading's
/// first visual row at the top of the viewport.  It's precomputed at open time
/// using the current mode + viewport width; the picker only echoes it back.
#[derive(Debug, Clone)]
pub struct HeadingEntry {
    pub level: HeadingLevel,
    pub text: String,
    pub buffer_line: usize,
    pub target_scroll: usize,
}

/// Placeholder shown in the empty search field.
const PLACEHOLDER: &str = "Type to filter sections…";

/// Width floor used when the heading list is empty so the modal doesn't snap
/// narrower than `(no headings)`.
const NO_HEADINGS_WIDTH: u16 = 16;

/// Blank rows kept above and below the picker on a terminal tall enough to
/// spare them.
const SECTION_PICKER_VERTICAL_PAD: u16 = 4;

/// Terminal-height threshold below which the picker drops its vertical padding
/// and grows edge-to-edge so the cramped screen isn't wasted.
const SHORT_TERMINAL_ROWS: u16 = 20;

/// Build the picker's list component from a precomputed entry list.  `focused`
/// names the index into `entries` that should be preselected (the nearest
/// preceding heading to the cursor) so the modal opens on the section the user
/// is already reading; the row is centred on first render.
pub fn build_section_list(
    entries: Vec<HeadingEntry>,
    focused: usize,
) -> SearchableList<HeadingEntry> {
    let mut list = SearchableList::new(entries, |e: &HeadingEntry| e.text.as_str())
        .with_focus_policy(FocusPolicy::ResetToTop);
    // Clamp an out-of-range preselect to the last entry (matching the legacy
    // `focused.min(n - 1)`); `focus_item` is otherwise a no-op for a missing
    // index and would leave focus on the first row.
    let clamped = focused.min(list.items().len().saturating_sub(1));
    list.focus_item(clamped);
    list.request_center();
    list
}

/// Render the section-picker modal.  Returns the `esc` close-hint rect for
/// click hit-testing.
pub fn render_section_picker(
    list: &mut SearchableList<HeadingEntry>,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    cursor_visible: bool,
) -> Option<Rect> {
    // The picker grows to fill the available height (the modal adapter has
    // already trimmed the bottom region from `area`).  Padding insets the
    // modal from the edges only when the terminal can spare the rows.
    let vertical_pad = if area.height < SHORT_TERMINAL_ROWS {
        0
    } else {
        SECTION_PICKER_VERTICAL_PAD
    };
    let empty_text = if list.items().is_empty() {
        "(no headings)"
    } else {
        "(no matches)"
    };
    let content_width = picker_content_width(list.items())
        .max(NO_HEADINGS_WIDTH)
        .max(PLACEHOLDER.chars().count() as u16 + 2);
    draw_searchable_list_modal(
        list,
        area,
        buf,
        ListModalOpts {
            title: "Go to Section",
            content_width,
            max_list_rows: u16::MAX,
            vertical_pad,
            theme,
            cursor_visible,
            placeholder: PLACEHOLDER,
            empty_text,
        },
        |ctx| match ctx {
            RowCtx::Item {
                item,
                focused,
                width,
            } => format_heading_row(item, focused, theme, width),
            // The section picker never builds header rows.
            RowCtx::Header { title, .. } => {
                Line::from(Span::styled(title.to_owned(), theme.modal_item))
            }
        },
    )
}

/// Indent (in spaces) used to render `level` in the list.  H1 = 1 space,
/// H2 = 2, …, H6 = 6 — visually mirrors the editor's heading prefix.
/// `HeadingLevel` is `repr(usize)` with H1..H6 = 1..6.
fn indent_for(level: HeadingLevel) -> usize {
    level as usize
}

/// Pre-render width estimate: max over every entry of `indent + text`, +2 for
/// a small right margin so focused-row backgrounds don't read flush.
fn picker_content_width(entries: &[HeadingEntry]) -> u16 {
    let mut max_w: u16 = 0;
    for e in entries {
        let w = indent_for(e.level).saturating_add(UnicodeWidthStr::width(e.text.as_str())) as u16;
        if w > max_w {
            max_w = w;
        }
    }
    max_w.saturating_add(2)
}

/// Format one heading row.  Focused rows match the command palette's selection
/// styling (`theme.modal_item_selected`).  Unfocused rows pick up the
/// heading's per-level color + bold (with `UNDERLINED` stripped) so the list
/// echoes the document's outline.  Long headings are truncated with `…`.
fn format_heading_row(
    entry: &HeadingEntry,
    focused: bool,
    theme: &Theme,
    width: u16,
) -> Line<'static> {
    let indent = " ".repeat(indent_for(entry.level));
    let marker = if focused { "› " } else { "  " };
    let prefix_w = UnicodeWidthStr::width(marker) + UnicodeWidthStr::width(indent.as_str());
    let text_budget = (width as usize).saturating_sub(prefix_w);
    let text = truncate_to_cells(&entry.text, text_budget);
    let label = format!("{marker}{indent}{text}");

    if focused {
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
    use crate::ui::searchable_list::ListEvent;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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
        // Out-of-range preselect clamps to the last entry.
        let list = build_section_list(sample(), 99);
        assert_eq!(list.focused_item_index(), Some(3));
        // An empty list opens with no focused item rather than panicking.
        let list = build_section_list(Vec::new(), 0);
        assert_eq!(list.focused_item_index(), None);
    }

    #[test]
    fn down_emits_focus_changed_for_next_entry() {
        let mut list = build_section_list(sample(), 0);
        let resp = list.handle_key(&key(KeyCode::Down));
        assert_eq!(resp, ListEvent::FocusChanged(1));
        assert_eq!(list.focused_item_index(), Some(1));
    }

    #[test]
    fn enter_submits_focused_entry() {
        let mut list = build_section_list(sample(), 2);
        assert_eq!(
            list.handle_key(&key(KeyCode::Enter)),
            ListEvent::Submitted(2)
        );
    }

    #[test]
    fn typing_filters_via_fuzzy_match() {
        let mut list = build_section_list(sample(), 0);
        for c in "quick".chars() {
            list.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(list.match_count(), 1);
        assert_eq!(
            list.focused_item().map(|e| e.text.as_str()),
            Some("Quick start")
        );
    }

    #[test]
    fn escape_cancels() {
        let mut list = build_section_list(sample(), 0);
        assert_eq!(list.handle_key(&key(KeyCode::Esc)), ListEvent::Cancelled);
    }
}
