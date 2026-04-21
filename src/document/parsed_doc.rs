use std::ops::Range;

use ratatui::text::Line;

use crate::config::Theme;
use crate::document::SourceMap;
use crate::markdown::{
    parse_offsets, parse_raw, promote_image_paragraphs, ImageRowOverride, Renderer,
};

/// Setext heading style detected from raw block source.  `None` for ATX
/// headings or non-heading blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetextKind {
    H1,
    H2,
}

/// Return the setext variant for `source` if it matches the pattern:
/// a non-blank line that does not begin with `#`, followed by a second line
/// consisting entirely of `=` (H1) or `-` (H2).  Trailing spaces on the
/// underline are allowed per the CommonMark spec.
pub fn detect_setext(source: &str) -> Option<SetextKind> {
    let mut lines = source.lines();
    let first = lines.next()?;
    let second = lines.next()?;
    if first.trim().is_empty() {
        return None;
    }
    if first.trim_start().starts_with('#') {
        return None;
    }
    let second = second.trim_end();
    if second.is_empty() {
        return None;
    }
    if second.chars().all(|c| c == '=') {
        return Some(SetextKind::H1);
    }
    if second.chars().all(|c| c == '-') {
        return Some(SetextKind::H2);
    }
    None
}

/// Metadata for a single `Block::ImageBlock` — the block index in
/// `source_map`'s space (so callers can look up its rendered-line range),
/// plus the alt text and URL for the image loader + placeholder.
#[derive(Debug, Clone)]
pub struct ImageBlockInfo {
    /// Index in the `SourceMap`'s virtual-block space.  Use
    /// `source_map.rendered_lines_for_block(block_idx)` to get the
    /// rendered-line range this image reserves.
    pub block_idx: usize,
    pub alt: String,
    pub url: String,
}

/// The parsed and rendered state of the document.
///
/// Holds the fully-rendered lines (for use by PreviewView and RenderedView),
/// the SourceMap (for cursor ↔ rendered-line mapping), and the raw source map
/// entries (for extracting raw text when entering edit mode on a block).
///
/// Rebuilt from scratch after every edit (debouncing is deferred to a future
/// phase; for typical document sizes this is fast enough).
#[derive(Debug, Clone)]
pub struct ParsedDoc {
    /// Rendered styled lines.
    pub lines: Vec<Line<'static>>,
    /// Source map linking rendered lines to source byte ranges.
    pub source_map: SourceMap,
    /// Per-block own rendered line count (from the renderer, BEFORE
    /// `preserve_blank_lines` inserts inter-block gap lines).
    ///
    /// `per_block_own[i]` is the number of rendered lines produced by block `i`
    /// itself. The gap blank lines inserted after block `i` are NOT counted here.
    /// `RenderedView` uses this to avoid treating gap blank lines as part of the
    /// cursor block's raw replacement region.
    per_block_own: Vec<usize>,
    /// Metadata for every `Block::ImageBlock` in the document, in the
    /// order they appear.  Populated during `build_with_overrides` so
    /// downstream code (the decode-dispatch scan in `App`, the paint
    /// pass in `ui::image_view`) doesn't have to walk the block list
    /// independently.
    pub image_blocks: Vec<ImageBlockInfo>,
}

impl ParsedDoc {
    /// Parse `source` and render it using `theme`.
    ///
    /// When `preserve_blank_lines` is true, consecutive blank lines between
    /// blocks in the source are reflected in the output: if two blocks are
    /// separated by N blank lines, N-1 extra `Line::raw("")` entries are
    /// inserted after the first block's rendered lines.  This overrides
    /// Markdown's default behaviour of collapsing multiple blank lines to one.
    ///
    /// `image_max_height` is the ceiling (in rendered rows) used for each
    /// `Block::ImageBlock`; propagated from `ImageConfig::max_height` via
    /// `EditorState`.
    pub fn build(
        source: &str,
        theme: &Theme,
        preserve_blank_lines: bool,
        image_max_height: usize,
    ) -> Self {
        Self::build_with_overrides(
            source,
            theme,
            preserve_blank_lines,
            image_max_height,
            None,
            None,
        )
    }

    /// Like [`build`], but applies a live `user_widths` override to the
    /// table whose first row begins at `live_table_widths.0`.  Used by
    /// Phase 6's column-resize drag to preview widths without writing the
    /// `tui-columns` comment to the buffer on every mouse-move event.
    ///
    /// `image_row_override` is an optional URL → row-count callback used
    /// to reserve exactly the rows each decoded image will occupy
    /// (aspect-aware).  When the callback returns `None` for a URL (or is
    /// itself `None`), the renderer falls back to `image_max_height`.
    pub fn build_with_overrides(
        source: &str,
        theme: &Theme,
        preserve_blank_lines: bool,
        image_max_height: usize,
        live_table_widths: Option<&(usize, Vec<Option<usize>>)>,
        image_row_override: Option<ImageRowOverride>,
    ) -> Self {
        // 1. Extract top-level block byte ranges.
        let mut real_ranges = parse_offsets::top_level_block_ranges(source);
        let total_bytes = source.len();

        // 2. Parse into raw AST (blocks here match `real_ranges` 1:1 — the
        //    `tui-columns` comment post-pass hasn't run yet).  The merge pass
        //    MUST run before the live-widths override: merging checks for
        //    `user_widths: None`, so applying the override first would
        //    prevent the Html comment block from being absorbed and the
        //    comment would flash into the rendered view between drag events.
        let mut blocks = parse_raw(source);
        merge_trailing_tui_columns_comments(&mut blocks, &mut real_ranges);
        // Promote paragraphs that contain only an image inline into
        // `Block::ImageBlock` so the renderer can reserve multi-row space
        // for terminal-graphics overlay.  Promotion happens in-place and
        // keeps blocks:real_ranges alignment 1:1 (no blocks are removed).
        promote_image_paragraphs(&mut blocks, Some(&mut real_ranges));
        if let Some((override_start, widths)) = live_table_widths {
            apply_live_table_widths(&mut blocks, &real_ranges, *override_start, widths);
        }

        // 3. Render, tracking per-block rendered line counts.
        let mut renderer = Renderer::new(theme).with_image_max_height(image_max_height);
        if let Some(override_fn) = image_row_override {
            renderer = renderer.with_image_row_override(override_fn);
        }
        let (rendered_lines, real_per_block_counts) = renderer.render_with_counts(&blocks);

        // 4. Build the final line list and source-map structures.  Each blank
        //    line in the source becomes its own "virtual block" owning a single
        //    `\n` byte, so the cursor can land on it independently of the
        //    surrounding content.  This keeps cursor navigation 1:1 with buffer
        //    lines — the cursor never silently jumps over a blank line.
        //
        //    pulldown-cmark's block ranges absorb a variable number of trailing
        //    newlines, so for each real block we back up past any trailing `\n`
        //    (via `content_end_of_block`) and then count newlines forward.  The
        //    first `\n` is the natural line break that ends the block; each
        //    additional `\n` is a blank line.
        let src_bytes = source.as_bytes();
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(rendered_lines.len());
        let mut rendered_to_block: Vec<usize> = Vec::with_capacity(rendered_lines.len());
        let mut all_original: Vec<Range<usize>> = Vec::new();
        let mut all_per_block_own: Vec<usize> = Vec::new();

        let push_blank = |lines: &mut Vec<Line<'static>>,
                          r2b: &mut Vec<usize>,
                          origs: &mut Vec<Range<usize>>,
                          owns: &mut Vec<usize>,
                          original: Range<usize>,
                          emit: bool| {
            let idx = origs.len();
            origs.push(original);
            owns.push(if emit { 1 } else { 0 });
            if emit {
                lines.push(Line::raw(""));
                r2b.push(idx);
            }
        };

        // Leading blank lines (before the first real block, or the whole
        // document if there are no real blocks).
        let leading_end = real_ranges
            .first()
            .map(|r| r.start.min(total_bytes))
            .unwrap_or(total_bytes);
        let mut bp = 0usize;
        while bp < leading_end {
            if src_bytes[bp] == b'\n' {
                push_blank(
                    &mut lines,
                    &mut rendered_to_block,
                    &mut all_original,
                    &mut all_per_block_own,
                    bp..bp + 1,
                    preserve_blank_lines,
                );
            }
            bp += 1;
        }

        // Real blocks, each followed by any blank lines in the gap after it.
        let mut rendered_src = 0usize;
        let mut image_blocks = Vec::new();
        for (i, &count) in real_per_block_counts.iter().enumerate() {
            let idx = all_original.len();
            all_original.push(real_ranges[i].clone());
            all_per_block_own.push(count);
            // Record image-block metadata keyed by the virtual block index
            // we just allocated, so the decode-dispatch scan and the paint
            // pass don't need to walk `blocks` again.
            if let crate::markdown::ast::Block::ImageBlock { alt, url } = &blocks[i] {
                image_blocks.push(ImageBlockInfo {
                    block_idx: idx,
                    alt: alt.clone(),
                    url: url.clone(),
                });
            }
            for j in 0..count {
                if let Some(line) = rendered_lines.get(rendered_src + j) {
                    lines.push(line.clone());
                    rendered_to_block.push(idx);
                }
            }
            rendered_src += count;

            // Setext H2 headings have two raw lines (the title and the `---`
            // underline) but the renderer produces only one styled line.  To
            // make the reveal logic in `RenderedView` line up 1:1 with the
            // raw source, append a thin rule here so the block owns two
            // rendered lines — matching what setext H1 already does via
            // `Renderer::render_heading`.
            let block_source = &source[real_ranges[i].start..real_ranges[i].end.min(total_bytes)];
            if count == 1 && matches!(detect_setext(block_source), Some(SetextKind::H2)) {
                lines.push(Line::styled("─".repeat(80), theme.rule));
                rendered_to_block.push(idx);
                if let Some(n) = all_per_block_own.last_mut() {
                    *n += 1;
                }
            }

            let content_end = content_end_of_block(source, &real_ranges[i]);
            let gap_end = if i + 1 < real_ranges.len() {
                real_ranges[i + 1].start.min(total_bytes)
            } else {
                total_bytes
            };

            // First `\n` in the gap is the natural line break that ends the
            // block; subsequent `\n`s are blank lines.
            let mut newline_count = 0usize;
            let mut emitted_in_gap = 0usize;
            let mut gp = content_end;
            while gp < gap_end {
                if src_bytes[gp] == b'\n' {
                    newline_count += 1;
                    if newline_count > 1 {
                        let emit = preserve_blank_lines || emitted_in_gap == 0;
                        push_blank(
                            &mut lines,
                            &mut rendered_to_block,
                            &mut all_original,
                            &mut all_per_block_own,
                            gp..gp + 1,
                            emit,
                        );
                        if emit {
                            emitted_in_gap += 1;
                        }
                    }
                }
                gp += 1;
            }
        }

        // Attribute any stray rendered lines not accounted for above to the
        // most recently pushed block (defensive — shouldn't happen in practice).
        while rendered_src < rendered_lines.len() {
            if let Some(line) = rendered_lines.get(rendered_src) {
                lines.push(line.clone());
                let last = all_original.len().saturating_sub(1);
                rendered_to_block.push(last);
            }
            rendered_src += 1;
        }

        let extended_ranges = build_extended_ranges(&all_original, total_bytes);

        let source_map = SourceMap::new(
            rendered_to_block,
            extended_ranges,
            all_original,
            total_bytes,
        );

        Self {
            lines,
            source_map,
            per_block_own: all_per_block_own,
            image_blocks,
        }
    }

    /// Number of rendered lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Number of rendered lines produced by the renderer for block `block_idx`,
    /// NOT counting any inter-block gap blank lines inserted by `preserve_blank_lines`.
    pub fn block_own_line_count(&self, block_idx: usize) -> usize {
        self.per_block_own.get(block_idx).copied().unwrap_or(0)
    }
}

/// Pair AST blocks with their byte ranges and splice a `user_widths` override
/// onto the `Block::Table` whose range starts at `override_start`.  Used by
/// Phase 6's column-resize drag to preview widths without buffer mutation.
fn apply_live_table_widths(
    blocks: &mut [crate::markdown::ast::Block],
    real_ranges: &[Range<usize>],
    override_start: usize,
    widths: &[Option<usize>],
) {
    use crate::markdown::ast::Block;
    // `parse_raw` and `parse_offsets::top_level_block_ranges` emit blocks
    // and ranges in the same order, so `blocks[i]` pairs with
    // `real_ranges[i]` before the trailing-comment merge runs.
    let mut block_i = 0usize;
    while block_i < blocks.len() && block_i < real_ranges.len() {
        if real_ranges[block_i].start == override_start {
            if let Block::Table { user_widths, .. } = &mut blocks[block_i] {
                *user_widths = Some(widths.to_vec());
            }
        }
        block_i += 1;
    }
}

/// Merge trailing `<!-- tui-columns: [..] -->` HTML blocks into their
/// preceding tables, AND update `real_ranges` so the (block, range) pairing
/// remains 1:1 after the merge.  Mirrors `markdown::parser::
/// attach_trailing_tui_columns_comments` but also rewrites the range vector
/// so the ParsedDoc downstream can keep using index alignment.
fn merge_trailing_tui_columns_comments(
    blocks: &mut Vec<crate::markdown::ast::Block>,
    real_ranges: &mut Vec<Range<usize>>,
) {
    use crate::markdown::ast::Block;
    let mut i = 0;
    while i + 1 < blocks.len() {
        let is_pair = matches!(
            (&blocks[i], &blocks[i + 1]),
            (Block::Table { user_widths: None, .. }, Block::Html(body))
                if crate::markdown::table_layout::parse_column_widths_comment(body).is_some()
        );
        if is_pair {
            let body = match &blocks[i + 1] {
                Block::Html(s) => s.clone(),
                _ => unreachable!(),
            };
            let widths = crate::markdown::table_layout::parse_column_widths_comment(&body).unwrap();
            if let Block::Table { user_widths, .. } = &mut blocks[i] {
                *user_widths = Some(widths);
            }
            blocks.remove(i + 1);
            // Absorb the comment's range into the preceding table's range
            // so the remaining ranges stay 1:1 with blocks.  The table's
            // extended range already ends at the next block's start, so
            // we can just drop `real_ranges[i + 1]` and let the downstream
            // covering-ranges pass stretch `real_ranges[i]` to fill the gap.
            if i + 1 < real_ranges.len() {
                let absorbed_end = real_ranges[i + 1].end;
                real_ranges[i] = real_ranges[i].start..absorbed_end;
                real_ranges.remove(i + 1);
            }
            continue;
        }
        i += 1;
    }
}

/// Return the byte position just after the block's last non-newline character.
/// `block.end` may include zero, one, or two trailing `\n` characters depending
/// on how pulldown-cmark reported the event range, so we normalise here.
fn content_end_of_block(source: &str, block: &Range<usize>) -> usize {
    let bytes = source.as_bytes();
    let mut end = block.end.min(bytes.len());
    while end > block.start && bytes[end - 1] == b'\n' {
        end -= 1;
    }
    end
}

/// Build extended byte ranges such that they non-overlappingly cover
/// `0..total_bytes`.  Each block's extended range starts at its original start
/// (except the first, which starts at 0 to absorb any leading bytes that the
/// block-walk missed) and ends at the next block's start (or `total_bytes`
/// for the last block).
fn build_extended_ranges(original: &[Range<usize>], total_bytes: usize) -> Vec<Range<usize>> {
    if original.is_empty() {
        if total_bytes > 0 {
            return vec![0..total_bytes];
        }
        return Vec::new();
    }
    let n = original.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let start = if i == 0 { 0 } else { original[i].start };
        let end = if i + 1 < n {
            original[i + 1].start
        } else {
            total_bytes.max(original[i].end)
        };
        let start = start.min(end);
        out.push(start..end);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    #[test]
    fn build_single_paragraph() {
        let doc = ParsedDoc::build("Hello world\n", theme(), false, 24);
        assert!(!doc.lines.is_empty());
        assert!(doc.source_map.block_count() >= 1);
    }

    #[test]
    fn build_heading_and_paragraph() {
        let src = "# Heading\n\nParagraph text\n";
        let doc = ParsedDoc::build(src, theme(), false, 24);
        // Should have at least as many rendered lines as there are blocks.
        assert!(doc.line_count() >= 2);
        // Both bytes of the heading and paragraph should map to some line.
        let heading_range = doc.source_map.rendered_lines_for_byte(2);
        assert!(!heading_range.is_empty());
        let para_range = doc.source_map.rendered_lines_for_byte(src.len() - 3);
        assert!(!para_range.is_empty());
    }

    #[test]
    fn every_byte_maps_to_some_line() {
        let src = "# Hello\n\nWorld\n\n---\n";
        let doc = ParsedDoc::build(src, theme(), false, 24);
        for b in 0..src.len() {
            let range = doc.source_map.rendered_lines_for_byte(b);
            assert!(
                !range.is_empty(),
                "byte {} ('{:?}') did not map to any rendered line",
                b,
                src.as_bytes().get(b)
            );
        }
    }

    #[test]
    fn empty_doc_builds_without_panic() {
        let doc = ParsedDoc::build("", theme(), false, 24);
        assert_eq!(doc.line_count(), 0);
    }

    #[test]
    fn detect_setext_recognises_h1_and_h2() {
        assert_eq!(detect_setext("Title\n=====\n"), Some(SetextKind::H1));
        assert_eq!(detect_setext("Title\n-----\n"), Some(SetextKind::H2));
    }

    #[test]
    fn detect_setext_rejects_atx() {
        assert_eq!(detect_setext("# Title\n"), None);
        assert_eq!(detect_setext("## Title\n"), None);
    }

    #[test]
    fn detect_setext_requires_underline() {
        assert_eq!(detect_setext("Only one line"), None);
        assert_eq!(detect_setext("Title\nbody\n"), None);
    }

    /// Setext H2 headings are given an extra rule line by `ParsedDoc::build`
    /// so the block owns two rendered lines — matching the two raw lines
    /// (title + `---` underline).  This parity lets `RenderedView` show both
    /// lines of raw source simultaneously when the cursor lands on either.
    /// A paragraph consisting solely of an image inline should promote to a
    /// `Block::ImageBlock` that reserves `image_max_height` rendered rows,
    /// so `move_up_visual` / `move_down_visual` traverse the reserved area
    /// consistently with other multi-line blocks.
    #[test]
    fn image_paragraph_promotes_and_reserves_rows() {
        let src = "Above.\n\n![cat](local.png)\n\nBelow.\n";
        let doc = ParsedDoc::build(src, theme(), true, 10);
        // The byte offset of `![cat]...` — find the '!' after the first blank.
        let image_byte = src.find('!').expect("image exists");
        let image_block = doc.source_map.block_for_byte(image_byte).unwrap();
        assert_eq!(doc.block_own_line_count(image_block), 10);
    }

    #[test]
    fn setext_h2_has_two_rendered_lines() {
        let src = "Heading\n-------\n";
        let doc = ParsedDoc::build(src, theme(), false, 24);
        let block = doc.source_map.block_for_byte(0).unwrap();
        assert_eq!(doc.block_own_line_count(block), 2);
    }

    #[test]
    fn setext_h1_has_two_rendered_lines() {
        let src = "Heading\n=======\n";
        let doc = ParsedDoc::build(src, theme(), false, 24);
        let block = doc.source_map.block_for_byte(0).unwrap();
        assert_eq!(doc.block_own_line_count(block), 2);
    }

    /// A blank line between two paragraphs must own its own virtual block so
    /// the cursor can land on it (instead of being silently absorbed into the
    /// preceding paragraph's extended range).
    #[test]
    fn blank_line_is_its_own_block() {
        let src = "First\n\nSecond\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);

        // Byte layout: F i r s t \n \n S e c o n d \n
        //              0 1 2 3 4 5  6  7 8 9 ...
        // The '\n' at byte 6 is the blank line between the paragraphs.
        let first_block = doc.source_map.block_for_byte(2); // inside "First"
        let blank_block = doc.source_map.block_for_byte(6); // the blank line
        let second_block = doc.source_map.block_for_byte(9); // inside "Second"

        assert!(first_block.is_some() && blank_block.is_some() && second_block.is_some());
        assert_ne!(
            first_block, blank_block,
            "blank line must not share a block with preceding paragraph"
        );
        assert_ne!(
            blank_block, second_block,
            "blank line must not share a block with following paragraph"
        );
    }

    /// Multiple consecutive blank lines each get their own virtual block so
    /// navigating through them lands on each blank line in turn.
    #[test]
    fn multiple_blank_lines_each_own_block() {
        let src = "A\n\n\n\nB\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);

        // Bytes: A \n \n \n \n B \n
        //        0 1  2  3  4  5 6
        // Blank-line '\n's are at bytes 2, 3, 4.
        let b2 = doc.source_map.block_for_byte(2).unwrap();
        let b3 = doc.source_map.block_for_byte(3).unwrap();
        let b4 = doc.source_map.block_for_byte(4).unwrap();
        assert_ne!(b2, b3);
        assert_ne!(b3, b4);
    }

    /// Regression: during a column-resize drag, `live_table_widths` stuffs
    /// `user_widths = Some(...)` onto the table.  If the merge pass then
    /// checked `user_widths: None` before running, the trailing
    /// `tui-columns` HTML comment would NOT get absorbed and would flash
    /// into the rendered view below the table on every drag event.
    #[test]
    fn live_widths_preview_still_hides_trailing_tui_columns_comment() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n<!-- tui-columns: [5, 6] -->\n";
        let live = (0usize, vec![Some(7), None]);
        let doc = ParsedDoc::build_with_overrides(src, theme(), true, 24, Some(&live), None);
        for line in &doc.lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                !text.contains("tui-columns"),
                "comment leaked into rendered output: {text:?}"
            );
        }
    }

    /// Each blank line's rendered-line range must map back to that same line
    /// (not the preceding block's last line).
    #[test]
    fn blank_line_rendered_range_is_blank_line() {
        let src = "First\n\nSecond\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);

        let first_range = doc.source_map.rendered_lines_for_byte(2);
        let blank_range = doc.source_map.rendered_lines_for_byte(6);
        let second_range = doc.source_map.rendered_lines_for_byte(9);

        // All non-empty.
        assert!(!first_range.is_empty());
        assert!(!blank_range.is_empty());
        assert!(!second_range.is_empty());
        // Ranges are disjoint.
        assert!(first_range.end <= blank_range.start);
        assert!(blank_range.end <= second_range.start);
        // The blank range points at an actually-blank rendered line.
        for idx in blank_range.clone() {
            let line = &doc.lines[idx];
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.is_empty(),
                "expected blank rendered line at index {idx}, got {text:?}"
            );
        }
    }
}
