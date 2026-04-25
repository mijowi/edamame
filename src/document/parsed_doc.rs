use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;

use ratatui::text::Line;

use crate::config::Theme;
use crate::diagram::DiagramSource;
use crate::document::SourceMap;
use crate::markdown::{
    inlines_to_plain, parse_offsets, parse_raw, promote_diagram_code_blocks,
    promote_image_paragraphs, Block, ImageRowOverride, Renderer,
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
    /// `Some` when this block was promoted from a ```mermaid (or other
    /// diagram backend) code block by `promote_diagram_code_blocks`.
    /// The App decode worker branches on this field to pick between
    /// `crate::image::resolve` (for `None`) and
    /// `crate::diagram::resolve_mermaid` (for `Some(Mermaid(_))`) —
    /// both funnel to the same `AppEvent::ImageReady` / `ImageCache`
    /// path.
    pub source: Option<DiagramSource>,
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
    /// Post-processed block AST, parallel with `real_ranges`.  Stashed
    /// here so per-frame consumers (`ui::link_view`, inline-link
    /// resolution) don't have to re-run the full pulldown-cmark parse
    /// on every draw.  Reflects all post-parse passes
    /// (`merge_trailing_tui_columns_comments`,
    /// `promote_image_paragraphs`, and any live table-width overrides)
    /// so callers see exactly what the renderer rendered.
    pub blocks: Vec<crate::markdown::Block>,
    /// Byte ranges of the real (non-blank) source blocks, 1:1 with
    /// `blocks`.  Blank-line virtual blocks synthesised by
    /// `build_with_overrides` are NOT present here — look them up via
    /// `source_map` instead.
    pub real_ranges: Vec<Range<usize>>,
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
    /// GFM-slug → rendered-line-index map for every `Block::Heading`
    /// in the document.  Consumed by Phase 8's `#anchor` navigation —
    /// `LinkTarget::Anchor(slug)` dispatches against this table.
    ///
    /// Slugs follow the GitHub Flavored Markdown algorithm: lowercase,
    /// strip characters not in `[a-z0-9 -]`, replace runs of whitespace
    /// with `-`, uniquify with a `-N` suffix on collisions.
    pub heading_anchors: HashMap<String, usize>,
    /// Lazy per-(ParsedDoc, viewport-width) cache of `visual_rows_for_line`
    /// results.  Populated on first query per frame and reused across
    /// scroll-only frames so the snapshot builders (`link_view`,
    /// `image_view`, `table_view`) and scroll arithmetic
    /// (`EditorState::scroll_for_last_visible`, `visual_rows_between`)
    /// don't re-walk and re-allocate per call.  Width-keyed: a terminal
    /// resize triggers a single rebuild (resize is debounced in `App`,
    /// so this is rare).  `RefCell` because `&EditorState` callers
    /// need shared access; `ParsedDoc` is single-threaded.
    visual_rows: RefCell<Option<VisualRowCache>>,
}

/// Per-(ParsedDoc, width) prefix-sum table over `lines` so the snapshot
/// builders can answer "how many visual rows do lines [0..i) consume?"
/// in O(1).  Replaces the per-block walks that dominated scroll-path
/// CPU on large documents prior to Phase 15.
#[derive(Debug, Clone)]
struct VisualRowCache {
    /// Viewport width this cache was built for.  A mismatch with the
    /// caller's width forces a refill.
    width: usize,
    /// `visual_rows_per_line[i]` = `visual_rows_for_line(&lines[i], width).max(1)`.
    visual_rows_per_line: Vec<usize>,
    /// `visual_row_prefix_sum[i]` = sum of `visual_rows_per_line[0..i]`.
    /// Length is `lines.len() + 1`; `[0] == 0`, `[lines.len()]` is the
    /// total visual row count.
    visual_row_prefix_sum: Vec<usize>,
}

impl ParsedDoc {
    /// Number of visual rows rendered line `idx` occupies at `width`.
    /// Returns 1 for out-of-range indices (matches the `.max(1)` clamp
    /// the snapshot builders applied historically).  O(1) after the
    /// cache is populated; first call at a given width is O(lines).
    pub fn visual_rows_for_line_at(&self, idx: usize, width: usize) -> usize {
        self.ensure_visual_rows(width);
        self.visual_rows
            .borrow()
            .as_ref()
            .and_then(|c| c.visual_rows_per_line.get(idx).copied())
            .unwrap_or(1)
    }

    /// Sum of visual rows occupied by rendered lines `[0..idx)` at
    /// `width`.  O(1) after the cache is populated.  Replaces the
    /// per-block `for idx in 0..start` loop in
    /// `link_view::extract_block_links` and the
    /// `for idx in scroll..end` loop in `image_view::build_snapshots`.
    pub fn visual_rows_before(&self, idx: usize, width: usize) -> usize {
        self.ensure_visual_rows(width);
        let clamped = idx.min(self.lines.len());
        self.visual_rows
            .borrow()
            .as_ref()
            .and_then(|c| c.visual_row_prefix_sum.get(clamped).copied())
            .unwrap_or(0)
    }

    /// Sum of visual rows occupied by rendered lines `[first..=last]`
    /// at `width`.  O(1) after the cache is populated.  Used by
    /// `EditorState::visual_rows_between`.
    pub fn visual_rows_between(&self, first: usize, last: usize, width: usize) -> usize {
        if first > last || self.lines.is_empty() {
            return 0;
        }
        let last = last.min(self.lines.len() - 1);
        self.visual_rows_before(last + 1, width)
            .saturating_sub(self.visual_rows_before(first, width))
    }

    /// Populate or refresh the visual-row cache for `width`.  Cheap
    /// when the cached width already matches; otherwise walks every
    /// line once and stores per-line counts plus a prefix sum.
    /// Two-phase borrow: the immutable check releases before the
    /// `borrow_mut` so we don't alias the `RefCell`.
    fn ensure_visual_rows(&self, width: usize) {
        {
            let borrow = self.visual_rows.borrow();
            if let Some(c) = borrow.as_ref() {
                if c.width == width {
                    return;
                }
            }
        }
        let len = self.lines.len();
        let mut per_line = Vec::with_capacity(len);
        let mut prefix = Vec::with_capacity(len + 1);
        prefix.push(0usize);
        let mut acc = 0usize;
        for line in &self.lines {
            // Reuse the canonical wrap algorithm — never duplicate it.
            let rows = crate::ui::line_render::visual_rows_for_line(line, width).max(1);
            per_line.push(rows);
            acc = acc.saturating_add(rows);
            prefix.push(acc);
        }
        *self.visual_rows.borrow_mut() = Some(VisualRowCache {
            width,
            visual_rows_per_line: per_line,
            visual_row_prefix_sum: prefix,
        });
    }
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
    /// `Block::ImageBlock`; propagated from `ImagesConfig::max_height` via
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
            false,
            80,
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
        row_striping: bool,
        viewport_width: usize,
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
        // Promote fenced ```mermaid blocks to synthetic `Block::ImageBlock`
        // so the same renderer path reserves overlay rows for them.  Returns
        // a `url → DiagramSource` map — attached to `ImageBlockInfo.source`
        // below so the App decode worker can find the mermaid text without
        // re-walking `blocks`.
        let diagram_sources = promote_diagram_code_blocks(&mut blocks);
        if let Some((override_start, widths)) = live_table_widths {
            apply_live_table_widths(&mut blocks, &real_ranges, *override_start, widths);
        }

        // 3. Render, tracking per-block rendered line counts.  The
        // viewport width feeds the table-column min-max distribution so
        // wide tables wrap proportionally rather than overflow.
        let mut renderer = Renderer::new(theme)
            .with_viewport_width(viewport_width.max(1))
            .with_image_max_height(image_max_height)
            .with_row_striping(row_striping);
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
        // Consume `rendered_lines` by move via an iterator: prior to
        // Phase 15 this loop indexed into the Vec and `.clone()`'d each
        // `Line<'static>`, which deep-copies every span's Cow and is
        // measurable on large documents.  Sequential consumption means
        // we can drain the source vector directly.
        let mut rendered_iter = rendered_lines.into_iter();
        let mut image_blocks = Vec::new();
        let mut heading_anchors: HashMap<String, usize> = HashMap::new();
        let mut anchor_counts: HashMap<String, usize> = HashMap::new();
        for (i, &count) in real_per_block_counts.iter().enumerate() {
            let idx = all_original.len();
            all_original.push(real_ranges[i].clone());
            all_per_block_own.push(count);
            // Record image-block metadata keyed by the virtual block index
            // we just allocated, so the decode-dispatch scan and the paint
            // pass don't need to walk `blocks` again.
            if let Block::ImageBlock { alt, url } = &blocks[i] {
                let source = diagram_sources.get(url).cloned();
                image_blocks.push(ImageBlockInfo {
                    block_idx: idx,
                    alt: alt.clone(),
                    url: url.clone(),
                    source,
                });
            }
            // Record heading anchors (GFM slug → first rendered line of
            // the heading).  Collisions get a `-N` suffix so later
            // headings don't clobber earlier ones.
            if let Block::Heading { inlines, .. } = &blocks[i] {
                let plain = inlines_to_plain(inlines);
                let base_slug = gfm_slug(&plain);
                let slug = uniquify_slug(&base_slug, &mut anchor_counts);
                // lines.len() here is the rendered-line index where this
                // heading's first line will land (before we push the
                // block's own lines below).
                heading_anchors.insert(slug, lines.len());
            }
            for _ in 0..count {
                if let Some(line) = rendered_iter.next() {
                    lines.push(line);
                    rendered_to_block.push(idx);
                }
            }

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
        for line in rendered_iter {
            lines.push(line);
            let last = all_original.len().saturating_sub(1);
            rendered_to_block.push(last);
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
            blocks,
            real_ranges,
            per_block_own: all_per_block_own,
            image_blocks,
            heading_anchors,
            visual_rows: RefCell::new(None),
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

/// Produce a GitHub Flavored Markdown slug for `text`.
///
/// Algorithm: lowercase every character, drop characters that aren't
/// `[a-z0-9 -]`, then replace runs of whitespace with a single `-`.
/// Leading / trailing dashes are preserved (matching GFM's behaviour)
/// but a heading that produces an empty slug is returned as an empty
/// string — callers that want to skip empty slugs should do so
/// explicitly.
pub fn gfm_slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_ws = false;
    for ch in text.chars().flat_map(|c| c.to_lowercase()) {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push('-');
            }
            prev_ws = true;
            continue;
        }
        prev_ws = false;
        if ch.is_ascii_alphanumeric() || ch == '-' {
            out.push(ch);
        }
        // Other characters (punctuation, non-ASCII) are stripped.
    }
    out
}

/// Append a `-N` suffix to `base` when it collides with a previously
/// issued slug.  `counts` tracks how many times each base slug has
/// appeared so far — first use returns `base` unchanged, second use
/// `base-1`, third `base-2`, etc., matching GitHub's behaviour.
fn uniquify_slug(base: &str, counts: &mut HashMap<String, usize>) -> String {
    let entry = counts.entry(base.to_owned()).or_insert(0);
    let slug = if *entry == 0 {
        base.to_owned()
    } else {
        format!("{base}-{}", *entry)
    };
    *entry += 1;
    slug
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
        let doc =
            ParsedDoc::build_with_overrides(src, theme(), true, 24, Some(&live), None, false, 80);
        for line in &doc.lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                !text.contains("tui-columns"),
                "comment leaked into rendered output: {text:?}"
            );
        }
    }

    #[test]
    fn gfm_slug_basic_cases() {
        assert_eq!(gfm_slug("Hello, World!"), "hello-world");
        assert_eq!(gfm_slug("Foo"), "foo");
        assert_eq!(gfm_slug("  spaces   here  "), "-spaces-here-");
        // Non-ASCII chars are stripped; surrounding spaces collapse.  The
        // three letters here are removed, leaving "  " (two runs of
        // whitespace separated by nothing), which produces two dashes.
        assert_eq!(gfm_slug("α β γ"), "--");
        assert_eq!(gfm_slug("API v2 — Release Notes"), "api-v2--release-notes");
        assert_eq!(gfm_slug("Hello World"), "hello-world");
    }

    #[test]
    fn uniquify_slug_appends_suffix_on_collision() {
        let mut counts = HashMap::new();
        assert_eq!(uniquify_slug("foo", &mut counts), "foo");
        assert_eq!(uniquify_slug("foo", &mut counts), "foo-1");
        assert_eq!(uniquify_slug("foo", &mut counts), "foo-2");
        assert_eq!(uniquify_slug("bar", &mut counts), "bar");
    }

    #[test]
    fn heading_anchors_has_one_entry_per_heading() {
        let src = "# First\n\nPara.\n\n## Second Heading\n\nMore.\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);
        assert!(doc.heading_anchors.contains_key("first"));
        assert!(doc.heading_anchors.contains_key("second-heading"));
        // Stable across a reparse of an identical source.
        let doc2 = ParsedDoc::build(src, theme(), true, 24);
        assert_eq!(doc.heading_anchors, doc2.heading_anchors);
    }

    #[test]
    fn heading_anchors_uniquify_on_collision() {
        let src = "# Foo\n\n## Foo\n\n### Foo\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);
        assert!(doc.heading_anchors.contains_key("foo"));
        assert!(doc.heading_anchors.contains_key("foo-1"));
        assert!(doc.heading_anchors.contains_key("foo-2"));
    }

    #[test]
    fn heading_anchor_indexes_point_to_rendered_heading_line() {
        let src = "Intro paragraph.\n\n# Target\n\nBody.\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);
        let &idx = doc
            .heading_anchors
            .get("target")
            .expect("heading anchor present");
        // The rendered line at `idx` should belong to the heading's block,
        // which is the block that contains the `Target` bytes.
        let target_byte = src.find("Target").unwrap();
        let heading_lines = doc.source_map.rendered_lines_for_byte(target_byte);
        assert!(
            heading_lines.contains(&idx),
            "anchor {} points to line {} but heading spans {:?}",
            "target",
            idx,
            heading_lines
        );
    }

    // ── Visual-row cache ────────────────────────────────────────────────

    /// The cached per-line count must match the canonical
    /// `line_render::visual_rows_for_line` answer for every line.
    #[test]
    fn visual_rows_cache_matches_line_render() {
        // Mix of short paragraphs, a wrapped long line, and a heading.
        let long = "x".repeat(120);
        let src = format!("# Title\n\nshort\n\n{long}\n\nfinal\n");
        let doc = ParsedDoc::build(&src, theme(), true, 24);
        let width = 40;
        for (i, line) in doc.lines.iter().enumerate() {
            let canonical = crate::ui::line_render::visual_rows_for_line(line, width).max(1);
            assert_eq!(
                doc.visual_rows_for_line_at(i, width),
                canonical,
                "cache mismatch at line {i}",
            );
        }
    }

    /// `visual_rows_before(i + 1) == visual_rows_before(i) + visual_rows_for_line_at(i)`
    /// for every line — the prefix-sum invariant the snapshot builders rely
    /// on.
    #[test]
    fn visual_rows_before_is_prefix_sum() {
        let src = "Para one.\n\n```\ncode\nblock\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);
        let width = 30;
        for i in 0..doc.lines.len() {
            assert_eq!(
                doc.visual_rows_before(i + 1, width),
                doc.visual_rows_before(i, width) + doc.visual_rows_for_line_at(i, width),
                "prefix-sum invariant broken at line {i}",
            );
        }
    }

    /// Querying the cache at width=A, then width=B, then width=A again must
    /// return correct answers each time.  Validates the width-mismatch
    /// rebuild path (terminal-resize case).
    #[test]
    fn visual_rows_cache_invalidates_on_width_change() {
        let long = "y".repeat(80);
        let src = format!("Hello\n\n{long}\n");
        let doc = ParsedDoc::build(&src, theme(), true, 24);
        // Sanity-check both widths against `line_render` directly.
        let expect = |w: usize| -> Vec<usize> {
            doc.lines
                .iter()
                .map(|l| crate::ui::line_render::visual_rows_for_line(l, w).max(1))
                .collect()
        };
        let at_40 = expect(40);
        let at_60 = expect(60);
        for (i, want) in at_40.iter().enumerate() {
            assert_eq!(doc.visual_rows_for_line_at(i, 40), *want);
        }
        for (i, want) in at_60.iter().enumerate() {
            assert_eq!(doc.visual_rows_for_line_at(i, 60), *want);
        }
        // Switching back to 40 rebuilds again.
        for (i, want) in at_40.iter().enumerate() {
            assert_eq!(doc.visual_rows_for_line_at(i, 40), *want);
        }
    }

    /// `visual_rows_between(first, last)` must equal the manual sum over
    /// the per-line counts in that inclusive range.
    #[test]
    fn visual_rows_between_matches_manual_sum() {
        let src = "alpha\n\nbeta\n\ngamma\n\ndelta\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);
        let width = 50;
        let n = doc.lines.len();
        for first in 0..n {
            for last in first..n {
                let expected: usize = (first..=last)
                    .map(|i| doc.visual_rows_for_line_at(i, width))
                    .sum();
                assert_eq!(
                    doc.visual_rows_between(first, last, width),
                    expected,
                    "between({first}, {last}) mismatch",
                );
            }
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
