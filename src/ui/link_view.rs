//! `LinkView` — per-frame layout snapshot for clickable-link hit
//! testing.
//!
//! Analogous to `ui::table_view` and `ui::image_view`: the renderer still
//! emits one `Line<'static>` per rendered line (styling underlined link
//! spans with `Theme::link_text`), and this module walks the AST + the
//! corresponding rendered lines to record the screen rect each link
//! occupies.  The snapshots are stored on `RenderedViewState` /
//! `PreviewState` so the next mouse event can hit-test against them —
//! the same pattern used for table handles and images.
//!
//! The snapshots are AST-backed: we walk `Block::Heading`, `Paragraph`,
//! `List`, `Table`, and `BlockQuote` to extract every `Inline::Link` in
//! document order and pair it with the rendered line(s) for its block.
//! The rendered-column range is then read back from the styled
//! `Line<'static>`'s UNDERLINED + `link_text` span(s).
//!
//! For the raw-reveal fallback path (when the cursor block is being
//! shown as raw text and the AST-styled spans aren't present), callers
//! should fall back to `mouse_ops::link_at_offset` against the raw
//! block source.

use std::path::Path;

use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Line;

use crate::editor::link::LinkTarget;
use crate::editor::EditorState;
use crate::markdown::{Block, Inline};

/// Per-frame geometry for one visible link.
///
/// `rect` is in terminal cells, relative to the document area's origin.
/// Width is in char columns on a single visual row — links that wrap
/// across multiple visual rows produce one snapshot per row so the
/// per-row hit-test stays simple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkLayoutSnapshot {
    /// Screen rect occupied by the link's rendered text span.
    pub rect: Rect,
    /// Resolved `LinkTarget` (classification is done once at snapshot
    /// build time so every click path consults the same answer).
    pub target: LinkTarget,
    /// Raw URL from the Markdown source, preserved for hover tooltips
    /// and for error messages on open failure.
    pub url: String,
    /// The optional link title (Markdown: `[text](url "title")`).
    /// Surfaced as a hover tooltip.
    pub title: Option<String>,
}

impl LinkLayoutSnapshot {
    /// Return the hovered snapshot if `(col, row)` falls within its
    /// rect.  Callers typically walk a `&[LinkLayoutSnapshot]` with
    /// `iter().find_map(|s| s.hit_test(col, row))` to get the active
    /// link, since multiple snapshots can exist for wrapped / stacked
    /// links. Used by tests in this module.
    #[allow(dead_code)]
    pub fn hit_test(&self, col: u16, row: u16) -> Option<&Self> {
        if col >= self.rect.x
            && col < self.rect.x + self.rect.width
            && row >= self.rect.y
            && row < self.rect.y + self.rect.height
        {
            Some(self)
        } else {
            None
        }
    }
}

/// Refresh `snapshots` in place when the cache key (`scroll`, `area`,
/// `parsed_version`) differs from the previous frame's, otherwise
/// leave the vector untouched.  Mirrors `image_view::build_snapshots_cached`.
/// The underlying `build_snapshots` walks `parsed.blocks` and calls
/// `visual_rows_for_line` for every visible line, so skipping it when
/// layout hasn't changed is a major per-frame win on large
/// link-heavy documents.
pub fn build_snapshots_cached(
    state: &EditorState,
    area: Rect,
    scroll: usize,
    snapshots: &mut Vec<LinkLayoutSnapshot>,
    cache_key: &mut Option<(usize, Rect, u64)>,
) {
    let key = (scroll, area, state.parsed_version);
    if *cache_key == Some(key) {
        return;
    }
    *snapshots = build_snapshots(state, area, scroll);
    *cache_key = Some(key);
}

/// Build snapshots for every visible link in the rendered document.
///
/// `scroll` is the first visual row index on screen.  `area` is the
/// document area rect.  The returned vector preserves document order —
/// earlier links appear earlier — so hit-testing (which uses `find_map`)
/// naturally favours the first matching snapshot when two overlap.
pub fn build_snapshots(state: &EditorState, area: Rect, scroll: usize) -> Vec<LinkLayoutSnapshot> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let base_dir = state
        .buffer
        .path()
        .and_then(|p| p.parent())
        .map(Path::to_owned);

    // `ParsedDoc` retains the post-processed AST (`blocks`) and the
    // matching byte ranges (`real_ranges`) from the last re-parse, so
    // we pair AST link occurrences with rendered-line geometry without
    // a per-frame parse of the full buffer.  Previously this
    // function called `buffer.contents()` + `markdown::parse()` +
    // `top_level_block_ranges()` on every draw — a dominant hotspot on
    // large documents.
    let mut out = Vec::new();
    for (block, range) in state
        .parsed
        .blocks
        .iter()
        .zip(state.parsed.real_ranges.iter())
    {
        let rendered_range = state.parsed.source_map.rendered_lines_for_byte(range.start);
        if rendered_range.is_empty() {
            continue;
        }
        let block_end_rows = state
            .parsed
            .visual_rows_before(rendered_range.end, area.width as usize);
        if block_end_rows <= scroll {
            continue;
        }
        // y_offset: visual rows of the block's first line above `scroll`.
        // For blocks already in the visible area this is positive; for
        // blocks that start off the bottom of the viewport we still need
        // to walk them so wrapped links on later lines are caught.
        extract_block_links(
            block,
            &rendered_range,
            state,
            area,
            scroll,
            base_dir.as_deref(),
            &mut out,
        );
    }
    out
}

/// Recursive walk: extract links from a block's inlines and pair each
/// with a rendered-line rect.  `rendered_range` is the rendered-line
/// range the block occupies.
fn extract_block_links(
    block: &Block,
    rendered_range: &std::ops::Range<usize>,
    state: &EditorState,
    area: Rect,
    scroll: usize,
    base_dir: Option<&Path>,
    out: &mut Vec<LinkLayoutSnapshot>,
) {
    // Early-exit: most blocks (tables, code, plain paragraphs) carry no
    // links.  Collecting URLs first lets us skip the per-line geometry
    // walk for those blocks entirely — the dominant win on link-sparse
    // documents like plan.md.
    let mut link_urls: Vec<(String, Option<String>)> = Vec::new();
    collect_links_from_block(block, &mut link_urls);
    if link_urls.is_empty() {
        return;
    }

    // Compute the block's first line's screen-y in O(1) via the
    // ParsedDoc's prefix-sum cache.  Replaces the historical
    // `for idx in 0..rendered_range.start` walk that called
    // `visual_rows_for_line` per line — quadratic on large documents.
    let width = area.width as usize;
    let total = state.parsed.lines.len();
    let block_start_rows = state
        .parsed
        .visual_rows_before(rendered_range.start.min(total), width);
    let mut y_cursor: isize = block_start_rows as isize - scroll as isize;

    let mut line_positions: Vec<(usize, u16, u16)> = Vec::new(); // (line_idx, y_start, rows_used)
    for idx in rendered_range.start..rendered_range.end.min(total) {
        let rows_used = state.parsed.visual_rows_for_line_at(idx, width).max(1);
        let y_start = y_cursor;
        y_cursor += rows_used as isize;
        // Skip lines that are entirely above or below the visible
        // viewport — no snapshots to emit for them.
        if y_cursor <= 0 {
            continue;
        }
        if y_start >= area.height as isize {
            break;
        }
        line_positions.push((idx, y_start.max(0) as u16, rows_used as u16));
    }

    // Walk each visible line of the block, extracting UNDERLINED spans
    // in order.  For each UNDERLINED-span slice we pop the next URL
    // from `link_urls` and produce one snapshot per visual row.
    let mut link_iter = link_urls.into_iter().peekable();
    for (line_idx, y_start, rows_used) in line_positions {
        let Some(line) = state.parsed.lines.get(line_idx) else {
            continue;
        };
        let underlined_ranges = underlined_char_ranges(line);
        for (start_col, end_col) in underlined_ranges {
            let Some((url, title)) = link_iter.next() else {
                return;
            };
            let target = LinkTarget::parse(&url, base_dir);
            // For wrap-aware placement, split the char range across the
            // line's visual rows.  `rows_used` is the line's total
            // rows; we synthesise a single snapshot covering the flat
            // char range and let hit-testing use the full height —
            // acceptable until we need per-row precision.
            let width = end_col.saturating_sub(start_col);
            if width == 0 {
                continue;
            }
            let rect = Rect {
                x: area.x + start_col as u16,
                y: area.y + y_start,
                width: width as u16,
                height: rows_used,
            };
            out.push(LinkLayoutSnapshot {
                rect,
                target,
                url,
                title,
            });
        }
    }
}

/// Return `[(start_col, end_col)]` char-column ranges for every run of
/// consecutive `UNDERLINED` spans in `line`.  Adjacent underlined spans
/// are coalesced so a link text that includes bold / italic substyling
/// still produces a single run.
fn underlined_char_ranges(line: &Line<'_>) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut col = 0usize;
    let mut in_link: Option<usize> = None;
    for span in &line.spans {
        let span_len = span.content.chars().count();
        let underlined = span.style.add_modifier.contains(Modifier::UNDERLINED);
        if underlined {
            if in_link.is_none() {
                in_link = Some(col);
            }
        } else if let Some(start) = in_link.take() {
            out.push((start, col));
        }
        col += span_len;
    }
    if let Some(start) = in_link {
        out.push((start, col));
    }
    out
}

/// Public wrapper around [`collect_links_from_block`] for callers
/// outside this module (notably `mouse_ops::link_url_for_click`).  The
/// private function is kept so the build-pass call-site doesn't need
/// to shuffle through the public API.
pub fn collect_links_from_block_public(block: &Block, out: &mut Vec<(String, Option<String>)>) {
    collect_links_from_block(block, out);
}

/// Collect every `Inline::Link` in `block`, walking nested block / inline
/// structures (list items, table cells, block quotes).  Preserves
/// document order so the N-th link emitted here matches the N-th
/// UNDERLINED span the renderer produces.
fn collect_links_from_block(block: &Block, out: &mut Vec<(String, Option<String>)>) {
    match block {
        Block::Heading { inlines, .. } | Block::Paragraph { inlines } => {
            collect_links_from_inlines(inlines, out);
        }
        Block::BlockQuote { blocks } => {
            for inner in blocks {
                collect_links_from_block(inner, out);
            }
        }
        Block::List { items, .. } => {
            for item in items {
                for inner in &item.blocks {
                    collect_links_from_block(inner, out);
                }
            }
        }
        Block::Table { headers, rows, .. } => {
            for cell in headers {
                collect_links_from_inlines(cell, out);
            }
            for row in rows {
                for cell in row {
                    collect_links_from_inlines(cell, out);
                }
            }
        }
        Block::CodeBlock { .. }
        | Block::HorizontalRule
        | Block::Html(_)
        | Block::HtmlComment(_)
        | Block::ImageBlock { .. } => {}
    }
}

fn collect_links_from_inlines(inlines: &[Inline], out: &mut Vec<(String, Option<String>)>) {
    for inline in inlines {
        match inline {
            Inline::Link { url, title, .. } => {
                out.push((url.clone(), title.clone()));
            }
            Inline::Bold(inner)
            | Inline::Italic(inner)
            | Inline::Strikethrough(inner)
            | Inline::Highlight(inner) => {
                collect_links_from_inlines(inner, out);
            }
            Inline::Text(_)
            | Inline::Code(_)
            | Inline::Image { .. }
            | Inline::HtmlComment(_)
            | Inline::SoftBreak
            | Inline::HardBreak => {}
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::document::Buffer;
    use crate::editor::EditorState;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn state(src: &str) -> EditorState {
        EditorState::new(Buffer::from_str(src), theme())
    }

    #[test]
    fn one_link_in_a_paragraph_produces_one_snapshot() {
        let src = "See [docs](https://example.com) for more.\n";
        let st = state(src);
        let area = Rect::new(0, 0, 80, 10);
        let snaps = build_snapshots(&st, area, 0);
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].url, "https://example.com");
        assert!(matches!(&snaps[0].target, LinkTarget::Url(u) if u == "https://example.com"));
    }

    #[test]
    fn two_links_in_document_order_produce_two_snapshots() {
        let src = "[first](a.md) and [second](b.md)\n";
        let st = state(src);
        let area = Rect::new(0, 0, 80, 10);
        let snaps = build_snapshots(&st, area, 0);
        assert_eq!(snaps.len(), 2);
        assert_eq!(snaps[0].url, "a.md");
        assert_eq!(snaps[1].url, "b.md");
    }

    #[test]
    fn no_snapshots_for_plain_paragraph() {
        let src = "no links here, just text\n";
        let st = state(src);
        let area = Rect::new(0, 0, 80, 10);
        assert!(build_snapshots(&st, area, 0).is_empty());
    }

    /// A link below the initial scroll offset must produce a snapshot
    /// whose `rect.y` equals the exact screen row of the link's
    /// rendered line.  Validates that the `ParsedDoc` prefix-sum
    /// math (which replaced the historical per-block 0..start walk)
    /// produces the same y-coordinates as the explicit walk did.
    #[test]
    fn link_below_scroll_lands_on_correct_row() {
        // 12 short paragraphs followed by a link — the link sits at
        // rendered line 24 (each paragraph emits one line plus one
        // blank-line gap line, so paragraph k starts at row 2k).
        let mut src = String::new();
        for i in 0..12 {
            src.push_str(&format!("Paragraph {i}.\n\n"));
        }
        src.push_str("Read [this](https://example.com)\n");
        let st = state(&src);
        let area = Rect::new(0, 0, 80, 6);
        // Scroll past the first 10 paragraphs.  visual_rows_before(20)
        // is 20 (each line is single-row at width 80), so the link
        // line — at rendered index 24 — should land on screen row 4.
        let snaps = build_snapshots(&st, area, 20);
        assert_eq!(snaps.len(), 1, "exactly one link snapshot expected");
        assert_eq!(snaps[0].rect.y, 4, "link y must equal line_index - scroll");
    }

    #[test]
    fn underlined_char_ranges_merge_adjacent_spans() {
        use ratatui::style::{Modifier, Style};
        use ratatui::text::{Line, Span};
        let line = Line::from(vec![
            Span::raw("plain "),
            Span::styled("link", Style::default().add_modifier(Modifier::UNDERLINED)),
            Span::styled(" text", Style::default().add_modifier(Modifier::UNDERLINED)),
            Span::raw(" after"),
        ]);
        let ranges = underlined_char_ranges(&line);
        assert_eq!(ranges, vec![(6, 15)]);
    }
}
