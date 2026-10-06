//! Per-frame layout snapshots for clickable-link hit testing, analogous to `ui::table_view`
//! and `ui::image_view`.
//!
//! Snapshots are AST-backed: every link-styled run ([`LinkRun`]) is collected in document order
//! and paired, by index, with the UNDERLINED span runs on the block's rendered lines.  An
//! inline image's placeholder is underlined too, so it gets a `LinkRun` of its own that
//! consumes a run without emitting a snapshot; otherwise every link after an inline image pairs
//! with the wrong URL.  For a raw-revealed cursor block, callers fall back to
//! `mouse_ops::link_at_offset` against the source instead.

use std::path::Path;

use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Line;

use crate::editor::link::LinkTarget;
use crate::editor::EditorState;
use crate::markdown::{Block, Inline};

/// One link-styled run the renderer emits, in document order.  The image-placeholder variant
/// keeps run indices aligned with the AST; see [`collect_link_runs_from_block`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRun {
    Link {
        url: String,
        /// The optional `[text](url "title")` title.
        title: Option<String>,
    },
    /// An `Inline::Image` placeholder: styled like a link, but not one.
    ImagePlaceholder,
}

/// Per-frame geometry for one visible link, in terminal cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkLayoutSnapshot {
    pub rect: Rect,
    /// Classified once at build time so every click path consults the same answer.
    pub target: LinkTarget,
    /// Raw URL, kept for hover tooltips and open-failure messages.
    pub url: String,
    /// Optional link title, surfaced as a hover tooltip.
    pub title: Option<String>,
}

impl LinkLayoutSnapshot {
    /// `Some(self)` if `(col, row)` falls within `rect`.  Used by tests in this module.
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

/// Rebuild `snapshots` only when `(scroll, area, parsed_version)` changed since the previous
/// frame.  Mirrors `image_view::build_snapshots_cached`.
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

/// Build snapshots for every visible link, in document order (so `find_map` hit-testing favors
/// the earlier of two overlapping snapshots).  `scroll` is the first visual row on screen.
pub fn build_snapshots(state: &EditorState, area: Rect, scroll: usize) -> Vec<LinkLayoutSnapshot> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let base_dir = state
        .buffer
        .path()
        .and_then(|p| p.parent())
        .map(Path::to_owned);

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

/// Extract a block's links and pair each with a rendered-line rect.  `rendered_range` is the
/// rendered-line range the block occupies.
fn extract_block_links(
    block: &Block,
    rendered_range: &std::ops::Range<usize>,
    state: &EditorState,
    area: Rect,
    scroll: usize,
    base_dir: Option<&Path>,
    out: &mut Vec<LinkLayoutSnapshot>,
) {
    // Collecting runs first skips the per-line geometry walk for the (common) link-free block.
    let mut link_runs: Vec<LinkRun> = Vec::new();
    collect_link_runs_from_block(block, &mut link_runs);
    if !link_runs
        .iter()
        .any(|run| matches!(run, LinkRun::Link { .. }))
    {
        return;
    }

    // First-line screen y in O(1) via the prefix-sum cache; a per-line walk was quadratic.
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
        if y_cursor <= 0 {
            continue;
        }
        if y_start >= area.height as isize {
            break;
        }
        line_positions.push((idx, y_start.max(0) as u16, rows_used as u16));
    }

    let mut link_iter = link_runs.into_iter().peekable();
    for (line_idx, y_start, rows_used) in line_positions {
        let Some(line) = state.parsed.lines.get(line_idx) else {
            continue;
        };
        let underlined_ranges = underlined_char_ranges(line);
        for (start_col, end_col) in underlined_ranges {
            // Every run consumes an entry, image placeholders included, to keep the pairing
            // aligned past an inline image.
            let Some(run) = link_iter.next() else {
                return;
            };
            let LinkRun::Link { url, title } = run else {
                continue;
            };
            let target = LinkTarget::parse(&url, base_dir);
            // One snapshot covering the flat char range at the line's full row height;
            // per-row precision for wrapped links is not needed yet.
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

/// Char-column ranges of every run of consecutive `UNDERLINED` spans in `line`; adjacent spans
/// coalesce so bold/italic substyling inside a link still yields one run.
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

/// Public wrapper around [`collect_link_runs_from_block`] for `mouse_ops::links`.
pub fn collect_link_runs_from_block_public(block: &Block, out: &mut Vec<LinkRun>) {
    collect_link_runs_from_block(block, out);
}

/// Collect one [`LinkRun`] per link-styled run the renderer will emit for `block`, in document
/// order, so the N-th entry pairs with the N-th underlined run on the rendered lines.
///
/// `Inline::Image` gets an entry because its placeholder is painted with the link fg and an
/// underlined alt text, so consumers see a run there.  An image *inside* a link is not counted
/// twice: the walk does not descend into a link's own text, and the renderer emits one run.
fn collect_link_runs_from_block(block: &Block, out: &mut Vec<LinkRun>) {
    match block {
        Block::Heading { inlines, .. } | Block::Paragraph { inlines, .. } => {
            collect_link_runs_from_inlines(inlines, out);
        }
        Block::BlockQuote { blocks, .. } | Block::FootnoteDefinition { blocks, .. } => {
            for inner in blocks {
                collect_link_runs_from_block(inner, out);
            }
        }
        Block::List { items, .. } => {
            for item in items {
                for inner in &item.blocks {
                    collect_link_runs_from_block(inner, out);
                }
            }
        }
        Block::Table { headers, rows, .. } => {
            for cell in headers {
                collect_link_runs_from_inlines(cell, out);
            }
            for row in rows {
                for cell in row {
                    collect_link_runs_from_inlines(cell, out);
                }
            }
        }
        Block::CodeBlock { .. }
        | Block::HorizontalRule { .. }
        | Block::Html(..)
        | Block::HtmlComment(..)
        | Block::MetadataBlock { .. }
        | Block::ImageBlock { .. } => {}
    }
}

fn collect_link_runs_from_inlines(inlines: &[Inline], out: &mut Vec<LinkRun>) {
    for inline in inlines {
        match inline {
            Inline::Link { url, title, .. } => {
                out.push(LinkRun::Link {
                    url: url.clone(),
                    title: title.clone(),
                });
            }
            Inline::Image { .. } => out.push(LinkRun::ImagePlaceholder),
            Inline::Bold(inner)
            | Inline::Italic(inner)
            | Inline::Strikethrough(inner)
            | Inline::Highlight(inner) => {
                collect_link_runs_from_inlines(inner, out);
            }
            Inline::Text(_)
            | Inline::Code(_)
            | Inline::HtmlComment(_)
            | Inline::FootnoteReference { .. }
            | Inline::Math { .. }
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

    /// Pins the prefix-sum y math: each paragraph is one line plus a gap line, so the link
    /// sits at rendered line 24 and lands on screen row 4 after scrolling by 20.
    #[test]
    fn link_below_scroll_lands_on_correct_row() {
        let mut src = String::new();
        for i in 0..12 {
            src.push_str(&format!("Paragraph {i}.\n\n"));
        }
        src.push_str("Read [this](https://example.com)\n");
        let st = state(&src);
        let area = Rect::new(0, 0, 80, 6);
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
