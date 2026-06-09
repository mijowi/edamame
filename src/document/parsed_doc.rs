use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::ops::Range;

use ratatui::text::Line;

use crate::config::Theme;
use crate::diagram::DiagramSource;
use crate::document::visual_cache::VisualRowCache;
use crate::document::SourceMap;
use crate::markdown::{
    inlines_to_plain, parse_offsets, parse_raw, promote_diagram_code_blocks, promote_html_comments,
    promote_image_paragraphs, split_lists_on_blank_lines, Block, ImageRowOverride, InlineColMap,
    Renderer,
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
    /// `blocks`.  Blank-line virtual blocks synthesized by
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
    /// in the document.  Consumed by `#anchor` navigation —
    /// `LinkTarget::Anchor(slug)` dispatches against this table.
    ///
    /// Slugs follow the GitHub Flavored Markdown algorithm: lowercase,
    /// strip characters not in `[a-z0-9 -]`, replace runs of whitespace
    /// with `-`, uniquify with a `-N` suffix on collisions.
    pub heading_anchors: HashMap<String, usize>,
    /// Footnote-label → rendered-line-index map for every
    /// `Block::FootnoteDefinition` in the document.  Consumed by
    /// footnote navigation — following a `[^label]` reference scrolls to
    /// the definition's first rendered line via this table (the footnote
    /// analogue of [`heading_anchors`](Self::heading_anchors)).  The key
    /// is the raw label as written (`"1"`, `"note"`), not the rendered
    /// number.
    pub footnote_anchors: HashMap<String, usize>,
    /// Lazy per-(ParsedDoc, viewport-width) cache of `visual_rows_for_line`
    /// results.  Populated on first query per frame and reused across
    /// scroll-only frames so the snapshot builders (`link_view`,
    /// `image_view`, `table_view`) and scroll arithmetic
    /// (`EditorState::scroll_for_last_visible`, `visual_rows_between`)
    /// don't re-walk and re-allocate per call.  Width-keyed.
    ///
    /// A small LRU (most-recent-first) rather than a single slot because
    /// each frame queries at two widths in lockstep: the editor view
    /// computes total rows at the full doc-area width to decide whether
    /// a scrollbar gutter is needed, and again at the post-gutter width
    /// for the scrollbar's own metrics.  A single-slot cache would
    /// rebuild twice per frame on long documents — visible as scroll
    /// lag — so we keep both widths warm.  `RefCell` because
    /// `&EditorState` callers need shared access; `ParsedDoc` is
    /// single-threaded.
    pub(super) visual_rows: RefCell<Vec<VisualRowCache>>,
    /// Lazy per-buffer-line cache of `InlineColMap` — maps between raw char
    /// columns and rendered (inline-markup-collapsed) char columns.  Used by
    /// the selection painter and cursor-indicator overlay.
    inline_maps: Vec<OnceCell<InlineColMap>>,
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
            false,
            true,
        )
    }

    /// Like [`build`], but applies a live `user_widths` override to the
    /// table whose first row begins at `live_table_widths.0`.  Used by
    /// the column-resize drag to preview widths without writing the
    /// `tui-columns` comment to the buffer on every mouse-move event.
    ///
    /// `image_row_override` is an optional URL → row-count callback used
    /// to reserve exactly the rows each decoded image will occupy
    /// (aspect-aware).  When the callback returns `None` for a URL (or is
    /// itself `None`), the renderer falls back to `image_max_height`.
    // Argument count is high because the parsed-doc build needs every
    // input the renderer cares about; bundling them into a struct is a
    // Phase-C task, not a Phase-A cleanup.
    #[allow(clippy::too_many_arguments)]
    pub fn build_with_overrides(
        source: &str,
        theme: &Theme,
        preserve_blank_lines: bool,
        image_max_height: usize,
        live_table_widths: Option<&(usize, Vec<Option<usize>>)>,
        image_row_override: Option<ImageRowOverride>,
        row_striping: bool,
        viewport_width: usize,
        big_h1: bool,
        // When false, fenced ```mermaid blocks stay as regular code
        // blocks — the renderer shows the diagram source verbatim
        // instead of substituting a synthetic image placeholder.  Set
        // by `EditorState::refresh_parsed` to `self.diagrams_enabled`
        // so a user who declined the diagrams prompt (or set
        // `[diagrams].enabled = "never"`) sees the original code.
        promote_diagrams: bool,
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
        // Split top-level lists across blank-line gaps so `1. a\n\n1. b\n`
        // renders as two ordered lists rather than one merged list (and so
        // `Enter`-twice on a list item produces a clean visual split).
        // Mutates both vectors so they stay 1:1.
        split_lists_on_blank_lines(&mut blocks, &mut real_ranges, source);
        // Promote pure-comment `Block::Html` entries to `Block::HtmlComment`
        // FIRST — the tui-columns merge below looks for `Block::HtmlComment`
        // adjacent to a `Block::Table` and must run against the promoted
        // variant.  Promotion preserves block order and count, so
        // `real_ranges` stays 1:1 with `blocks`.
        promote_html_comments(&mut blocks);
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
        let diagram_sources = if promote_diagrams {
            promote_diagram_code_blocks(&mut blocks)
        } else {
            HashMap::new()
        };
        if let Some((override_start, widths)) = live_table_widths {
            apply_live_table_widths(&mut blocks, &real_ranges, *override_start, widths);
        }

        // 3. Render, tracking per-block rendered line counts.  The
        // viewport width feeds the table-column min-max distribution so
        // wide tables wrap proportionally rather than overflow.
        let mut renderer = Renderer::new(theme)
            .with_viewport_width(viewport_width.max(1))
            .with_image_max_height(image_max_height)
            .with_row_striping(row_striping)
            .with_big_h1(big_h1);
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
        // Consume `rendered_lines` by move via an iterator: this loop
        // previously indexed into the Vec and `.clone()`'d each
        // `Line<'static>`, which deep-copies every span's Cow and is
        // measurable on large documents.  Sequential consumption means
        // we can drain the source vector directly.
        let mut rendered_iter = rendered_lines.into_iter();
        let mut image_blocks = Vec::new();
        let mut heading_anchors: HashMap<String, usize> = HashMap::new();
        let mut footnote_anchors: HashMap<String, usize> = HashMap::new();
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
            // Record footnote anchors (label → first rendered line of the
            // definition) so a `[^label]` reference can scroll here.
            if let Block::FootnoteDefinition { label, .. } = &blocks[i] {
                footnote_anchors.insert(label.clone(), lines.len());
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
                lines.push(Line::styled("─".repeat(viewport_width.max(1)), theme.rule));
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

        let line_count = source.split('\n').count();
        Self {
            lines,
            source_map,
            blocks,
            real_ranges,
            per_block_own: all_per_block_own,
            image_blocks,
            heading_anchors,
            footnote_anchors,
            visual_rows: RefCell::new(Vec::new()),
            inline_maps: (0..line_count).map(|_| OnceCell::new()).collect(),
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

    /// True when `block_idx` is a synthetic `Block::ImageBlock` produced
    /// from a mermaid fenced code block.  Mermaid blocks share the
    /// "reveal the entire raw source on cursor entry" affordance with
    /// fenced code blocks, so several call sites need to special-case
    /// them; this keeps the rule in one place.
    pub fn is_mermaid_block(&self, block_idx: usize) -> bool {
        self.image_blocks.iter().any(|info| {
            info.block_idx == block_idx && matches!(info.source, Some(DiagramSource::Mermaid(_)))
        })
    }

    /// True when `block_idx` is a `Block::ImageBlock` (a real image *or* a
    /// promoted diagram).  Such a block has a single source line — the
    /// `![alt](url)` / fenced-diagram opener — but reserves *many* rendered
    /// rows (`image_max_height`).  Callers that translate a rendered
    /// sub-row back to a raw source line must not use the sub-row index
    /// directly: the block's byte range can also absorb a trailing blank
    /// line, so a naive `sub_idx` lands on a phantom empty raw line and
    /// addresses the wrong buffer line.  `mouse_ops::coord` guards against
    /// this by pinning every reserved row to raw line 0; the selection
    /// overlay in `rendered_view::paint` instead relies on its
    /// `raw_line_idx >= raw_lines.len()` bounds check to skip painting
    /// reserved rows entirely.
    pub fn is_image_block(&self, block_idx: usize) -> bool {
        self.image_blocks
            .iter()
            .any(|info| info.block_idx == block_idx)
    }

    // ── Inline column map cache ────────────────────────────────────────────

    /// Lazily-built bidirectional char-column map for `buffer_line_idx`.
    /// The caller passes the raw line text; on first call for this index
    /// the map is built from `raw_line` and cached.
    ///
    /// **Contract:** `raw_line` must be the canonical content for
    /// `buffer_line_idx` in the *current* `ParsedDoc` generation.  The cache
    /// is keyed only by index, so if a caller passes a different `raw_line`
    /// for an already-initialized index the stale map is returned silently
    /// (the `chars().count()` debug-assert catches differing-length cases
    /// but not equal-length content drift).  `ParsedDoc` is rebuilt on every
    /// buffer mutation, so all live callers satisfy this naturally —
    /// don't reuse a `ParsedDoc` across edits.
    ///
    /// `buffer_line_idx` must be in-bounds.  Callers derive it from a live
    /// `block.range.start` via `Buffer::block_line_to_buffer_line`, which
    /// always produces a valid index for a fresh `ParsedDoc`.
    pub fn inline_map(&self, buffer_line_idx: usize, raw_line: &str) -> &InlineColMap {
        debug_assert!(
            buffer_line_idx < self.inline_maps.len(),
            "InlineColMap: buffer_line_idx {buffer_line_idx} out of bounds ({})",
            self.inline_maps.len()
        );
        let cell = &self.inline_maps[buffer_line_idx];
        let map = cell.get_or_init(|| InlineColMap::build(raw_line));
        debug_assert_eq!(
            raw_line.chars().count(),
            map.raw_len(),
            "InlineColMap: raw_line char count mismatch for buffer line {buffer_line_idx}"
        );
        map
    }

    // ── Visual-row cache (rendered) ───────────────────────────────────────
    //
    // Thin lazy wrappers over `VisualRowCache`.  The cache lives in a
    // `RefCell<Option<_>>` so `&ParsedDoc` callers get shared access, and
    // is populated on first query at a given width — terminal resizes
    // (rare; debounced by App) trigger a single rebuild.

    /// Visual rows occupied by rendered line `idx` at `width`.  O(1) after
    /// the cache is populated; first call at a given width is O(lines).
    pub fn visual_rows_for_line_at(&self, idx: usize, width: usize) -> usize {
        self.with_visual_rows(width, |c| c.for_line(idx))
    }

    /// Sum of visual rows occupied by rendered lines `[0..idx)` at `width`.
    pub fn visual_rows_before(&self, idx: usize, width: usize) -> usize {
        self.with_visual_rows(width, |c| c.before(idx))
    }

    /// Sum of visual rows occupied by rendered lines `[first..=last]`
    /// at `width`.  Used by tests in this crate.
    #[allow(dead_code)]
    pub fn visual_rows_between(&self, first: usize, last: usize, width: usize) -> usize {
        self.with_visual_rows(width, |c| c.between(first, last))
    }

    /// Total visual rows occupied by the rendered document at `width`.
    pub fn total_visual_rows(&self, width: usize) -> usize {
        self.with_visual_rows(width, |c| c.total())
    }

    /// `(rendered_line_idx, sub_row)` for a document-level visual row.
    pub fn line_at_visual_row(&self, visual_row: usize, width: usize) -> (usize, usize) {
        self.with_visual_rows(width, |c| c.find_visual_row(visual_row))
    }

    /// Run `f` against the visual-row cache for `width`, building it
    /// first if no entry for that width is currently warm.
    fn with_visual_rows<R>(&self, width: usize, f: impl FnOnce(&VisualRowCache) -> R) -> R {
        self.ensure_visual_rows(width);
        let borrow = self.visual_rows.borrow();
        let cache = borrow
            .iter()
            .find(|c| c.width() == width)
            .expect("visual-row cache populated above");
        f(cache)
    }

    /// Populate or refresh the visual-row cache for `width`.  When an
    /// entry for `width` is already warm, promote it to the front and
    /// return.  Otherwise build a fresh cache and prepend, evicting any
    /// entries beyond the LRU capacity.  Two-phase borrow: the
    /// immutable check releases before the `borrow_mut` so we don't
    /// alias the `RefCell`.
    fn ensure_visual_rows(&self, width: usize) {
        /// Number of distinct widths we keep warm.  Must be at least 2
        /// to absorb the editor view's per-frame "decide bar / display
        /// bar" two-width query pattern without thrashing.
        const LRU_CAP: usize = 2;
        {
            let borrow = self.visual_rows.borrow();
            if borrow.first().map(|c| c.width()) == Some(width) {
                return;
            }
        }
        let warm_pos = self
            .visual_rows
            .borrow()
            .iter()
            .position(|c| c.width() == width);
        if let Some(pos) = warm_pos {
            let mut entries = self.visual_rows.borrow_mut();
            let entry = entries.remove(pos);
            entries.insert(0, entry);
            return;
        }
        let cache = VisualRowCache::build(self.lines.len(), width, |i| {
            crate::ui::line_render::visual_rows_for_line(&self.lines[i], width)
        });
        let mut entries = self.visual_rows.borrow_mut();
        entries.insert(0, cache);
        entries.truncate(LRU_CAP);
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
/// the column-resize drag to preview widths without buffer mutation.
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
            (Block::Table { user_widths: None, .. }, Block::HtmlComment(body))
                if crate::markdown::table_layout::parse_column_widths_comment(body).is_some()
        );
        if is_pair {
            let body = match &blocks[i + 1] {
                Block::HtmlComment(s) => s.clone(),
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
            #[allow(clippy::single_range_in_vec_init)]
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
        let doc = ParsedDoc::build_with_overrides(
            src,
            theme(),
            true,
            24,
            Some(&live),
            None,
            false,
            80,
            false,
            true,
        );
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
    fn footnote_anchors_map_label_to_definition_line() {
        let src = "Intro.[^1]\n\nMiddle.\n\n[^1]: The note.\n";
        let doc = ParsedDoc::build(src, theme(), true, 40);
        let &line = doc
            .footnote_anchors
            .get("1")
            .expect("footnote_anchors should contain label '1'");
        // The recorded line is the definition's first rendered line.
        let rendered = doc.lines[line]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(
            rendered.contains("The note."),
            "anchor line should be the definition, got: {rendered:?}"
        );
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

    // ── HTML-comment hiding ──────────────────────────────────────────────

    /// A block-level HTML comment contributes zero rendered lines — its
    /// `per_block_own` count must be 0 so navigation can detect the block
    /// as hidden without inspecting the AST variant.
    #[test]
    fn html_comment_block_owns_zero_rendered_lines() {
        let src = "Alpha.\n\n<!-- hidden -->\n\nBeta.\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);
        // The comment block must be present in `blocks`.
        assert!(
            doc.blocks
                .iter()
                .any(|b| matches!(b, Block::HtmlComment(_))),
            "blocks: {:?}",
            doc.blocks
        );
        // Find its byte offset — the `<` of the `<!--`.
        let comment_byte = src.find("<!--").unwrap();
        let block_idx = doc
            .source_map
            .block_for_byte(comment_byte)
            .expect("comment bytes must map to a block");
        assert_eq!(doc.block_own_line_count(block_idx), 0);
        // And no rendered line anywhere contains the marker text.
        for line in &doc.lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                !text.contains("<!--"),
                "comment leaked into rendered output: {text:?}"
            );
        }
    }

    /// The trailing `tui-columns` comment is absorbed into the preceding
    /// table by `merge_trailing_tui_columns_comments`.  Regression guard
    /// after the parser refactor changed the variant the merge
    /// function looks for.
    #[test]
    fn tui_columns_still_absorbed_through_parsed_doc() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n<!-- tui-columns: [10, 20] -->\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);
        assert!(matches!(
            doc.blocks.first(),
            Some(Block::Table {
                user_widths: Some(_),
                ..
            })
        ));
        // The comment must NOT appear as its own surviving block.
        assert!(
            !doc.blocks
                .iter()
                .any(|b| matches!(b, Block::HtmlComment(_))),
            "comment should have been absorbed: {:?}",
            doc.blocks
        );
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
