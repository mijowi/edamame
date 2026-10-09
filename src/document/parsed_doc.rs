use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::ops::Range;

use ratatui::text::Line;

use crate::config::Theme;
use crate::diagram::DiagramSource;
use crate::document::row_map::{JoinedMap, RowCache};
use crate::document::visual_cache::VisualRowCache;
use crate::document::wrap::Indent;
use crate::document::SourceMap;
use crate::markdown::{
    attach_nested_tui_columns_comments, inlines_to_plain, parse_document,
    promote_diagram_code_blocks, promote_display_math_paragraphs, promote_html_comments,
    promote_image_paragraphs, reconstruct_broken_display_math, split_display_math_paragraphs,
    Block, ImageRowOverride, RefLabels, RenderCache, Renderer, RowOrigin,
};

/// Metadata for one `Block::ImageBlock`, for the image loader and placeholder.
#[derive(Debug, Clone)]
pub struct ImageBlockInfo {
    /// Index in the `SourceMap`'s virtual-block space — feed it to
    /// `source_map.rendered_lines_for_block` for the rows this image reserves.
    pub block_idx: usize,
    pub alt: String,
    pub url: String,
    /// `Some` when promoted from a fenced diagram code block.  The App decode worker
    /// branches on it to pick between `crate::image::resolve` and
    /// `crate::diagram::resolve_mermaid`; both funnel to the same `ImageReady` path.
    pub source: Option<DiagramSource>,
}

/// The parsed and rendered state of the document: rendered lines, the cursor ↔
/// rendered-line [`SourceMap`], and the block AST.  Rebuilt from scratch after every edit.
#[derive(Debug, Clone)]
pub struct ParsedDoc {
    /// Rendered styled lines.
    pub lines: Vec<Line<'static>>,
    /// Where each of [`lines`](Self::lines) came from, 1:1 with it: the source lines it shows,
    /// relative to its block's first line, and how its columns relate to theirs.  Recorded by
    /// the renderer as it emits each row (and by `build` for the blank rows it adds), never
    /// re-derived.  See `docs/dev/plans/row-provenance.md`.
    row_origins: Vec<RowOrigin>,
    /// The text this parse was built from — the coordinate space of every byte range in
    /// [`source_map`](Self::source_map) and [`real_ranges`](Self::real_ranges).
    ///
    /// The live `Buffer` is *not* that space: a deferred in-line edit advances the buffer
    /// without rebuilding the parse, so slicing the buffer with a parse-time range reads
    /// text shifted by the edit's length — and a shift crossing a line boundary silently
    /// changes how many lines the slice appears to have.  Resolve parse-time ranges here
    /// (see [`byte_to_line`](Self::byte_to_line)); touch the buffer only with a *live*
    /// offset.
    source: Box<str>,
    /// Byte offset of every line start in [`source`](Self::source) (`[0] == 0`), kept from the
    /// parse that indexed it anyway.
    line_starts: Vec<usize>,
    /// The labels the document's reference and footnote definitions define, for the per-row
    /// column maps (`row_map`), which parse one line at a time.
    ref_labels: RefLabels,
    /// Source map linking rendered lines to source byte ranges.
    pub source_map: SourceMap,
    /// Post-processed block AST, 1:1 with `real_ranges`.  Reflects every post-parse pass
    /// and any live table-width override, so callers see exactly what the renderer
    /// rendered — and per-frame consumers need not re-run pulldown-cmark on every draw.
    pub blocks: Vec<crate::markdown::Block>,
    /// Byte ranges of the real (non-blank) source blocks, 1:1 with `blocks`.  The
    /// blank-line virtual blocks are NOT here — look those up via `source_map`.
    pub real_ranges: Vec<Range<usize>>,
    /// Rendered lines produced by block `i` *itself*, before `preserve_blank_lines`
    /// inserts inter-block gap lines.  `RenderedView` uses it to keep gap lines out of the
    /// cursor block's raw replacement region.
    per_block_own: Vec<usize>,
    /// Every `Block::ImageBlock`, in document order, so the decode-dispatch scan and the
    /// paint pass don't each walk the block list.
    pub image_blocks: Vec<ImageBlockInfo>,
    /// GFM-slug → rendered-line index for every heading; `LinkTarget::Anchor` dispatches
    /// against it.  Slugs are lowercased, stripped to `[a-z0-9 -]`, whitespace runs
    /// become `-`, and collisions take a `-N` suffix.
    pub heading_anchors: HashMap<String, usize>,
    /// Label → rendered-line index for every footnote definition, the footnote analogue of
    /// [`heading_anchors`](Self::heading_anchors).  The key is the raw label as written,
    /// not the rendered number.
    pub footnote_anchors: HashMap<String, usize>,
    /// Width-keyed lazy cache of `visual_rows_for_line`, so the snapshot builders and the
    /// scroll arithmetic don't re-walk and re-allocate per call.
    ///
    /// A small LRU rather than one slot because each frame queries two widths in lockstep:
    /// total rows at the full doc-area width to decide whether a scrollbar gutter is
    /// needed, then again at the post-gutter width for the scrollbar's metrics.  One slot
    /// rebuilds twice per frame on long documents — visible as scroll lag.
    pub(super) visual_rows: RefCell<Vec<VisualRowCache>>,
    /// Lazy rendered-line → source-line table for the gutter, filled by
    /// `editor::state_source_lines`.
    ///
    /// Here rather than on `EditorState` so its lifetime is exactly this parse's: a
    /// deferred in-line edit bumps `parsed_version` without rebuilding the parse and keeps
    /// painting *these* rows, so it must keep this table too — and a reparse drops it by
    /// construction.
    source_lines: OnceCell<Vec<Option<usize>>>,
    /// Lazy per-row column data (the row's chars, its block's position, its raw ↔ rendered
    /// column map), 1:1 with [`lines`](Self::lines), built by `row_map` from this parse's own
    /// text on first use.
    row_cache: Vec<OnceCell<RowCache>>,
    /// Lazy joined column maps of the paragraphs whose rows `row_map` maps as parts of the
    /// whole, keyed by `(block, paragraph's first line)`.  See [`with_joined_map`](Self::with_joined_map).
    joined_maps: RefCell<HashMap<(usize, u32), JoinedMap>>,
    /// Whether this parse rendered prose paragraphs with reflow on (soft breaks → spaces,
    /// wrapped as one flow, at any depth).  Row and column questions read the flow from its
    /// `RowOrigin`; what still branches on it (through
    /// [`row_map::stacked_lines`](crate::document::row_map::stacked_lines)) is the reveal's
    /// timing and its stacked raw lines.
    pub reflow_paragraphs: bool,
    /// `(block_idx, band_rows)` for a `$$...$$` block currently revealed with the live math
    /// preview, or `None`.  When set, that block's rendered rows split into a top preview band of
    /// `band_rows` (the rendered formula) followed by the editable raw-source rows, so the formula
    /// keeps the block's top edge.  Every rendered-row ⇄ source-line mapping shifts the source rows
    /// down by `band_rows` through [`latex_source_offset`](Self::latex_source_offset).  Set by
    /// `EditorState::refresh_parsed`; a mermaid or preview-off reveal leaves it `None`.
    pub(crate) math_source_offset: Option<(usize, usize)>,
}

impl ParsedDoc {
    /// Parse `source` and render it using `theme`.
    ///
    /// `preserve_blank_lines` reflects consecutive source blank lines in the output
    /// instead of collapsing them the way Markdown does.  `image_max_height` is the
    /// rendered-row ceiling for each `Block::ImageBlock`.
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
            false,
            true,
            false,
            None,
        )
    }

    /// Like [`Self::build`], but with two live overrides.
    ///
    /// `live_table_widths` splices `user_widths` onto the table starting at its `.0` byte,
    /// so a column-resize drag can preview widths without writing the `tui-columns`
    /// comment to the buffer on every mouse-move.
    ///
    /// `image_row_override` is a `(URL, image-block ordinal)` → row-count callback that
    /// reserves exactly the rows each decoded image will occupy and collapses the one block
    /// whose raw source the cursor revealed.  The ordinal indexes [`Self::image_blocks`],
    /// built from the same document-order walk the renderer counts in.  `None` from (or
    /// for) the callback falls back to `image_max_height`.
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
        // When false, code-block bodies are the single-span lines they were before
        // highlighting existed.
        syntax_highlighting: bool,
        // When false, fenced diagram blocks stay ordinary code blocks and their source
        // shows verbatim — what a user who declined the diagrams prompt should see.
        promote_diagrams: bool,
        // When true, prose paragraphs reflow (soft breaks become spaces, the paragraph
        // wraps to the viewport as one flow) instead of rendering one row per source line.
        reflow_paragraphs: bool,
        // Block-level render memoization, so unchanged blocks reuse their rendered lines.
        // `None` (tests, one-shot builds) renders everything from scratch.
        render_cache: Option<&mut RenderCache>,
    ) -> Self {
        // Blocks and their byte ranges from one pulldown-cmark pass, 1:1 until the
        // `tui-columns` merge below.  That merge MUST run before the live-widths override:
        // it checks for `user_widths: None`, so overriding first would leave the comment
        // unabsorbed and flashing into the rendered view between drag events.
        let parse = parse_document(source);
        let (mut blocks, mut real_ranges) = (parse.blocks, parse.ranges);
        let mut footnote_labels = Vec::new();
        collect_footnote_labels(&blocks, &mut footnote_labels);
        let ref_labels = RefLabels::new(
            parse.link_labels.iter().map(String::as_str),
            footnote_labels,
        );
        let total_bytes = source.len();
        // FIRST: the merge below looks for a `Block::HtmlComment` next to a
        // `Block::Table`, so it must run against the promoted variant.  Order and count
        // are preserved, so `real_ranges` stays 1:1.
        promote_html_comments(&mut blocks);
        merge_trailing_tui_columns_comments(&mut blocks, &mut real_ranges);
        attach_nested_tui_columns_comments(&mut blocks);
        // Image-only paragraphs become `Block::ImageBlock` so the renderer reserves
        // multi-row space for the graphics overlay.  In place, so alignment stays 1:1.
        promote_image_paragraphs(&mut blocks, Some(&mut real_ranges));
        // Rescue a `$$...$$` block whose interior LaTeX pulldown refused to close (an unbalanced
        // brace mid-typing): rebuild the display-math inline so it promotes like any other formula
        // instead of collapsing to prose and losing its reserved rows.  Runs in both branches so
        // the figures-off `math` code-block rendering stays consistent too.  Ranges and block
        // count are untouched, so the 1:1 alignment holds.
        reconstruct_broken_display_math(&mut blocks, &real_ranges, source);
        // Fenced diagram blocks and `$$...$$`-only paragraphs take the same path.  Each returns a
        // `url → DiagramSource` map, merged and attached to `ImageBlockInfo.source` below so the
        // decode worker finds the source text without re-walking `blocks`.  Both are gated on
        // `promote_diagrams`: mermaid and display math share one consent switch, so a user who
        // declined keeps seeing the original source rather than a placeholder they can't render.
        let diagram_sources = if promote_diagrams {
            let mut sources = promote_diagram_code_blocks(&mut blocks);
            sources.extend(promote_display_math_paragraphs(
                &mut blocks,
                &mut real_ranges,
                source,
            ));
            sources
        } else {
            // Figures off: mermaid fences already ARE code blocks, but two
            // `$$...$$` formulas stacked with no blank line between them are
            // one paragraph (pulldown folds them).  Split that paragraph so
            // each formula becomes its own block and renders as a separate
            // fenced-style `math` code block — matching how the figures-on
            // path promotes them to one image block per formula.
            split_display_math_paragraphs(&mut blocks, &mut real_ranges, source);
            HashMap::new()
        };
        if let Some((override_start, widths)) = live_table_widths {
            apply_live_table_widths(&mut blocks, &real_ranges, *override_start, widths);
        }

        // The viewport width feeds the table-column min-max distribution, so wide tables
        // wrap proportionally rather than overflow.
        let mut renderer = Renderer::new(theme)
            .with_viewport_width(viewport_width.max(1))
            .with_image_max_height(image_max_height)
            .with_row_striping(row_striping)
            .with_big_h1(big_h1)
            .with_syntax_highlighting(syntax_highlighting)
            .with_reflow_paragraphs(reflow_paragraphs);
        if let Some(override_fn) = image_row_override {
            renderer = renderer.with_image_row_override(override_fn);
        }
        let (rendered_rows, real_per_block_counts) = match render_cache {
            Some(cache) => renderer.render_with_counts_cached(&blocks, cache),
            None => renderer.render_with_counts(&blocks),
        };

        // Each source blank line becomes its own virtual block owning a single `\n`, so
        // cursor navigation stays 1:1 with buffer lines and never jumps a blank line.
        //
        // pulldown-cmark's ranges absorb a variable number of trailing newlines, so each
        // real block backs up past them (`content_end_of_block`) and then counts forward:
        // the first `\n` ends the block, each additional one is a blank line.
        let src_bytes = source.as_bytes();
        let row_capacity = rendered_rows.lines.len();
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(row_capacity);
        let mut row_origins: Vec<RowOrigin> = Vec::with_capacity(row_capacity);
        let mut rendered_to_block: Vec<usize> = Vec::with_capacity(row_capacity);
        let mut all_original: Vec<Range<usize>> = Vec::new();
        let mut all_per_block_own: Vec<usize> = Vec::new();

        // A blank line's virtual block owns one row: its only line, chrome.
        let push_blank = |lines: &mut Vec<Line<'static>>,
                          origins: &mut Vec<RowOrigin>,
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
                origins.push(RowOrigin::chrome(Some(0)));
                r2b.push(idx);
            }
        };

        // Leading blank lines — the whole document when there are no real blocks.
        let leading_end = real_ranges
            .first()
            .map(|r| r.start.min(total_bytes))
            .unwrap_or(total_bytes);
        let mut bp = 0usize;
        while bp < leading_end {
            if src_bytes[bp] == b'\n' {
                push_blank(
                    &mut lines,
                    &mut row_origins,
                    &mut rendered_to_block,
                    &mut all_original,
                    &mut all_per_block_own,
                    bp..bp + 1,
                    preserve_blank_lines,
                );
            }
            bp += 1;
        }

        // Real blocks, each followed by the blank lines in the gap after it.
        // `rendered_rows` is consumed by move: cloning each `Line<'static>` deep-copies
        // every span's Cow, which is measurable on large documents.
        let mut rendered_iter = rendered_rows.into_rows();
        let mut image_blocks = Vec::new();
        let mut heading_anchors: HashMap<String, usize> = HashMap::new();
        let mut footnote_anchors: HashMap<String, usize> = HashMap::new();
        let mut anchor_counts: HashMap<String, usize> = HashMap::new();
        for (i, &count) in real_per_block_counts.iter().enumerate() {
            let idx = all_original.len();
            all_original.push(real_ranges[i].clone());
            all_per_block_own.push(count);
            if let Block::ImageBlock { alt, url, .. } = &blocks[i] {
                let source = diagram_sources.get(url).cloned();
                image_blocks.push(ImageBlockInfo {
                    block_idx: idx,
                    alt: alt.clone(),
                    url: url.clone(),
                    source,
                });
            }
            if let Block::Heading { inlines, .. } = &blocks[i] {
                let plain = inlines_to_plain(inlines);
                let base_slug = gfm_slug(&plain);
                let slug = uniquify_slug(&base_slug, &mut anchor_counts);
                // `lines.len()` is where this heading's first line will land, since the
                // block's own lines are pushed below.
                heading_anchors.insert(slug, lines.len());
            }
            if let Block::FootnoteDefinition { label, .. } = &blocks[i] {
                footnote_anchors.insert(label.clone(), lines.len());
            }
            for _ in 0..count {
                if let Some((line, origin)) = rendered_iter.next() {
                    lines.push(line);
                    row_origins.push(origin);
                    rendered_to_block.push(idx);
                }
            }

            let content_end = content_end_of_block(source, &real_ranges[i]);
            let gap_end = if i + 1 < real_ranges.len() {
                real_ranges[i + 1].start.min(total_bytes)
            } else {
                total_bytes
            };

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
                            &mut row_origins,
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

        // Defensive: stray rendered lines go to the most recently pushed block.  The per-block
        // counts come from the same render, so this never fires.
        for (line, origin) in rendered_iter {
            debug_assert!(false, "the renderer emitted a row no block counted");
            lines.push(line);
            row_origins.push(origin);
            let last = all_original.len().saturating_sub(1);
            rendered_to_block.push(last);
        }

        // Phantom final line: a source ending in '\n' has one more buffer line than the
        // loops produce blocks for, since ropey puts the cursor on an empty line *after*
        // the last '\n'.  Without its own block, `block_for_byte`'s end-of-source fallback
        // attributes that cursor to the last real block and `RenderedView` swallows the
        // block's text under an empty raw reveal.
        //
        // An *empty* document takes the same branch and must: ropey reports one line for
        // `""` but the loops produce no blocks, so the rendered / preview painters (which
        // iterate rendered lines) would paint nothing at all, cursor included.
        if total_bytes == 0 || src_bytes[total_bytes - 1] == b'\n' {
            push_blank(
                &mut lines,
                &mut row_origins,
                &mut rendered_to_block,
                &mut all_original,
                &mut all_per_block_own,
                total_bytes..total_bytes,
                true,
            );
        }

        let extended_ranges = build_extended_ranges(&all_original, total_bytes);

        let source_map = SourceMap::new(
            rendered_to_block,
            extended_ranges,
            all_original,
            total_bytes,
        );

        debug_assert_eq!(row_origins.len(), lines.len(), "rows and origins drifted");
        let row_count = lines.len();
        Self {
            lines,
            row_origins,
            source: source.into(),
            line_starts: parse.line_starts,
            ref_labels,
            source_map,
            blocks,
            real_ranges,
            per_block_own: all_per_block_own,
            image_blocks,
            heading_anchors,
            footnote_anchors,
            visual_rows: RefCell::new(Vec::new()),
            source_lines: OnceCell::new(),
            row_cache: (0..row_count).map(|_| OnceCell::new()).collect(),
            joined_maps: RefCell::default(),
            reflow_paragraphs,
            // Set by `EditorState::refresh_parsed` once the live reveal is known; a fresh parse
            // starts with no preview split.
            math_source_offset: None,
        }
    }

    /// Every row's origin, 1:1 with [`lines`](Self::lines).
    pub fn row_origins(&self) -> &[RowOrigin] {
        &self.row_origins
    }

    /// The indent rendered line `idx` wraps behind: the hang its origin states
    /// ([`RowOrigin::hang`]).  Every reader of a rendered row's wrap (painters, row counts,
    /// clicks, hit-tests) takes it from here.  [`Indent::NONE`] past the last line.
    pub fn row_indent(&self, idx: usize) -> Indent {
        Indent::of_row(self.row_origins.get(idx))
    }

    /// Number of rendered lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The text this parse was built from.  See [`source`](Self::source) for why a
    /// parse-time range must be resolved here and never against the live `Buffer`.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// 0-based source line containing `byte`, counted in *this parse's* text — the
    /// correct answer when a deferred in-line edit has left the buffer disagreeing.
    ///
    /// Past-the-end answers the last line.
    pub fn byte_to_line(&self, byte: usize) -> usize {
        let upto = byte.min(self.source.len());
        self.line_starts
            .partition_point(|&s| s <= upto)
            .saturating_sub(1)
    }

    /// Byte where source line `line` starts; past the last line, the source's end.
    pub fn line_start_byte(&self, line: usize) -> usize {
        self.line_starts
            .get(line)
            .copied()
            .unwrap_or(self.source.len())
    }

    /// Source line `line`'s text, without its `\n` (a CRLF line keeps its `\r`); `""` past the
    /// last line.
    pub fn source_line(&self, line: usize) -> &str {
        let Some(&start) = self.line_starts.get(line) else {
            return "";
        };
        let end = self
            .line_starts
            .get(line + 1)
            .map_or(self.source.len(), |&next| next - 1);
        self.source.get(start..end).unwrap_or("")
    }

    /// See [`ref_labels`](Self::ref_labels).
    pub fn ref_labels(&self) -> &RefLabels {
        &self.ref_labels
    }

    /// See [`per_block_own`](Self::per_block_own).
    pub fn block_own_line_count(&self, block_idx: usize) -> usize {
        self.per_block_own.get(block_idx).copied().unwrap_or(0)
    }

    /// The post-processed [`Block`] whose content contains `byte`, through its last line's `\n`;
    /// `None` on a blank line, including one pulldown-cmark's range absorbs (`> q⏎⏎` is the
    /// quote's), exactly as `build` splits those into virtual blocks.  Searches `real_ranges`,
    /// not the source map's space — the two diverge by one per preceding blank line, so a
    /// source-map index must never index `blocks`.
    pub fn real_block_for_byte(&self, byte: usize) -> Option<&Block> {
        let idx = self.real_ranges.partition_point(|r| r.end <= byte);
        let range = self.real_ranges.get(idx)?;
        if byte >= range.start && byte <= content_end_of_block(&self.source, range) {
            self.blocks.get(idx)
        } else {
            None
        }
    }

    /// The post-processed [`Block`] holding source `line`, and the line its original range
    /// starts on, which its block-relative positions ([`SrcLines`](crate::markdown::SrcLines))
    /// count from.  `None` on a blank line, as [`Self::real_block_for_byte`].
    pub fn real_block_at_line(&self, line: usize) -> Option<(&Block, usize)> {
        let byte = self.line_start_byte(line);
        let idx = self.real_ranges.partition_point(|r| r.end <= byte);
        let range = self.real_ranges.get(idx)?;
        let first = self.byte_to_line(range.start);
        if first > line || byte > content_end_of_block(&self.source, range) {
            return None;
        }
        Some((self.blocks.get(idx)?, first))
    }

    /// True for a synthetic `Block::ImageBlock` promoted from a mermaid fence.  These
    /// share the fenced-code "reveal the entire raw source on cursor entry" affordance,
    /// which several call sites special-case; the rule lives here.
    pub fn is_mermaid_block(&self, block_idx: usize) -> bool {
        self.image_blocks.iter().any(|info| {
            info.block_idx == block_idx && matches!(info.source, Some(DiagramSource::Mermaid(_)))
        })
    }

    /// True when `block_idx` is a synthetic image block promoted from a `$$...$$` display-math
    /// paragraph (see `promote_display_math_paragraphs`).
    pub fn is_latex_block(&self, block_idx: usize) -> bool {
        self.image_blocks.iter().any(|info| {
            info.block_idx == block_idx && matches!(info.source, Some(DiagramSource::Latex(_)))
        })
    }

    /// Rows of the live math-preview band above the revealed raw source for `block_idx`, or `0`
    /// when the block has no such band (not the revealed math block, mermaid, or preview off).
    /// Every rendered-row ⇄ source-line mapping shifts the source down by this amount, and
    /// `document::row_map` is its only reader: everything else asks it (a diagram's rows through
    /// `row_map::revealed_diagram_line`).  See [`math_source_offset`](Self::math_source_offset).
    pub(crate) fn latex_source_offset(&self, block_idx: usize) -> usize {
        match self.math_source_offset {
            Some((idx, band)) if idx == block_idx => band,
            _ => 0,
        }
    }

    /// True for a *diagram-derived* image block — a mermaid fence or a `$$...$$` math formula,
    /// however many source lines it spans.  Such blocks reveal as a single unit (every reserved
    /// row swaps to its raw-source line 1:1), so the raw-reveal bookkeeping (timer, drag
    /// suppression, click/row mapping) treats them alike.  Ordinary images stay on the generic path.
    pub fn is_diagram_reveal_block(&self, block_idx: usize) -> bool {
        self.image_blocks.iter().any(|info| {
            info.block_idx == block_idx
                && matches!(
                    info.source,
                    Some(DiagramSource::Mermaid(_)) | Some(DiagramSource::Latex(_))
                )
        })
    }

    /// True for a `Block::ImageBlock` (real image or promoted diagram).  Such a block reserves
    /// more rendered rows than it has source lines; their origins pin each to a source line
    /// (see `row_map::line_for_row`), and `rendered_view::paint` skips a real image's rows.
    pub fn is_image_block(&self, block_idx: usize) -> bool {
        self.image_blocks
            .iter()
            .any(|info| info.block_idx == block_idx)
    }

    // ── Row column map cache ──────────────────────────────────────────────

    /// Row `row`'s column data, built with `init` on first use.  See
    /// [`row_cache`](Self::row_cache).  `None` past the last row, or where `init` is.
    pub(crate) fn row_cache_or_init(
        &self,
        row: usize,
        init: impl FnOnce() -> Option<RowCache>,
    ) -> Option<&RowCache> {
        let cell = self.row_cache.get(row)?;
        if let Some(cache) = cell.get() {
            return Some(cache);
        }
        let cache = init()?;
        Some(cell.get_or_init(|| cache))
    }

    /// `read` of the joined map keyed `key`, built with `init` on first use.  See
    /// [`joined_maps`](Self::joined_maps).
    pub(crate) fn with_joined_map<R>(
        &self,
        key: (usize, u32),
        init: impl FnOnce() -> JoinedMap,
        read: impl FnOnce(&JoinedMap) -> R,
    ) -> R {
        if let Some(map) = self.joined_maps.borrow().get(&key) {
            return read(map);
        }
        let map = init();
        let out = read(&map);
        self.joined_maps.borrow_mut().insert(key, map);
        out
    }

    // ── Visual-row cache (rendered) ───────────────────────────────────────
    //
    // Thin lazy wrappers over `VisualRowCache`, populated on first query at a given width.

    /// Visual rows for rendered line `idx`.  O(1) once warm; O(lines) on the first call
    /// at a given width.
    pub fn visual_rows_for_line_at(&self, idx: usize, width: usize) -> usize {
        self.with_visual_rows(width, |c| c.for_line(idx))
    }

    /// Sum of visual rows occupied by rendered lines `[0..idx)` at `width`.
    pub fn visual_rows_before(&self, idx: usize, width: usize) -> usize {
        self.with_visual_rows(width, |c| c.before(idx))
    }

    /// Sum of visual rows over `[first..=last]`.  Used by tests in this crate.
    #[allow(dead_code)]
    pub fn visual_rows_between(&self, first: usize, last: usize, width: usize) -> usize {
        self.with_visual_rows(width, |c| c.between(first, last))
    }

    /// Total visual rows occupied by the rendered document at `width`.
    pub fn total_visual_rows(&self, width: usize) -> usize {
        self.with_visual_rows(width, |c| c.total())
    }

    /// Rendered-line → source-line table, built with `init` on first call.  See
    /// [`source_lines`](Self::source_lines) for why it is cached per parse.
    pub fn source_lines_or_init(
        &self,
        init: impl FnOnce() -> Vec<Option<usize>>,
    ) -> &[Option<usize>] {
        self.source_lines.get_or_init(init)
    }

    /// `(rendered_line_idx, sub_row)` for a document-level visual row.
    pub fn line_at_visual_row(&self, visual_row: usize, width: usize) -> (usize, usize) {
        self.with_visual_rows(width, |c| c.find_visual_row(visual_row))
    }

    /// Run `f` against the visual-row cache for `width`, warming it first if needed.
    fn with_visual_rows<R>(&self, width: usize, f: impl FnOnce(&VisualRowCache) -> R) -> R {
        self.ensure_visual_rows(width);
        let borrow = self.visual_rows.borrow();
        let cache = borrow
            .iter()
            .find(|c| c.width() == width)
            .expect("visual-row cache populated above");
        f(cache)
    }

    /// Promote an already-warm entry for `width` to the front, or build one and evict
    /// past the LRU capacity.  The immutable check releases before the `borrow_mut` so the
    /// `RefCell` is never aliased.
    fn ensure_visual_rows(&self, width: usize) {
        /// At least 2, to absorb the editor view's per-frame two-width query pattern.
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
            crate::document::wrap::visual_rows_for_line(&self.lines[i], self.row_indent(i), width)
        });
        let mut entries = self.visual_rows.borrow_mut();
        entries.insert(0, cache);
        entries.truncate(LRU_CAP);
    }
}

/// GitHub Flavored Markdown slug: lowercase, drop anything outside `[a-z0-9 -]`, collapse
/// whitespace runs to a single `-`.  Leading / trailing dashes are preserved (as GFM
/// does), and an empty slug is returned as-is for the caller to handle.
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
    }
    out
}

/// Append a `-N` suffix on collision, matching GitHub: first use is `base`, then `base-1`,
/// `base-2`.  `counts` carries the per-base tally across calls.
pub(crate) fn uniquify_slug(base: &str, counts: &mut HashMap<String, usize>) -> String {
    let entry = counts.entry(base.to_owned()).or_insert(0);
    let slug = if *entry == 0 {
        base.to_owned()
    } else {
        format!("{base}-{}", *entry)
    };
    *entry += 1;
    slug
}

/// Splice a `user_widths` override onto the `Block::Table` whose range starts at
/// `override_start`, for the column-resize drag's preview.
fn apply_live_table_widths(
    blocks: &mut [crate::markdown::ast::Block],
    real_ranges: &[Range<usize>],
    override_start: usize,
    widths: &[Option<usize>],
) {
    use crate::markdown::ast::Block;
    // Blocks and ranges are emitted in the same order, so they pair by index before the
    // trailing-comment merge runs.
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

/// Merge trailing `<!-- tui-columns: [..] -->` blocks into their preceding tables,
/// rewriting `real_ranges` so the (block, range) pairing stays 1:1.  Mirrors
/// `markdown::parser::attach_trailing_tui_columns_comments`, which does not.
fn merge_trailing_tui_columns_comments(
    blocks: &mut Vec<crate::markdown::ast::Block>,
    real_ranges: &mut Vec<Range<usize>>,
) {
    use crate::markdown::ast::Block;
    let mut i = 0;
    while i + 1 < blocks.len() {
        let is_pair = matches!(
            (&blocks[i], &blocks[i + 1]),
            (Block::Table { user_widths: None, .. }, Block::HtmlComment(body, _))
                if crate::markdown::table_layout::parse_column_widths_comment(body).is_some()
        );
        if is_pair {
            let body = match &blocks[i + 1] {
                Block::HtmlComment(s, _) => s.clone(),
                _ => unreachable!(),
            };
            let widths = crate::markdown::table_layout::parse_column_widths_comment(&body).unwrap();
            if let Block::Table { user_widths, .. } = &mut blocks[i] {
                *user_widths = Some(widths);
            }
            blocks.remove(i + 1);
            // The table's extended range already ends at the next block's start, so
            // dropping the comment's range lets the covering-ranges pass fill the gap.
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

/// Every footnote definition's label in `blocks`, containers included.
fn collect_footnote_labels<'a>(blocks: &'a [Block], out: &mut Vec<&'a str>) {
    for block in blocks {
        match block {
            Block::FootnoteDefinition { label, blocks, .. } => {
                out.push(label);
                collect_footnote_labels(blocks, out);
            }
            Block::BlockQuote { blocks, .. } => collect_footnote_labels(blocks, out),
            Block::List { items, .. } => {
                for item in items {
                    collect_footnote_labels(&item.blocks, out);
                }
            }
            _ => {}
        }
    }
}

/// The byte just after the block's last non-newline character.  `block.end` may include
/// zero, one, or two trailing `\n`s depending on how pulldown-cmark reported the range.
fn content_end_of_block(source: &str, block: &Range<usize>) -> usize {
    let bytes = source.as_bytes();
    let mut end = block.end.min(bytes.len());
    while end > block.start && bytes[end - 1] == b'\n' {
        end -= 1;
    }
    end
}

/// Extend the block ranges so they non-overlappingly cover `0..total_bytes`: each starts
/// at its original start (the first at 0, absorbing anything the block walk missed) and
/// ends at the next block's start.
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

    /// Recording source positions must not cost a second pulldown-cmark pass: one build is one
    /// block parse.
    #[test]
    fn a_build_runs_exactly_one_block_parse() {
        use crate::markdown::parser::BLOCK_PARSE_COUNT;
        let src = "# T\n\n- a\n  - b\n\n> q\n\n```\nx\n```\n\n| a |\n|---|\n| 1 |\n";
        let before = BLOCK_PARSE_COUNT.with(|c| c.get());
        ParsedDoc::build(src, theme(), true, 24);
        assert_eq!(BLOCK_PARSE_COUNT.with(|c| c.get()) - before, 1);
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
        assert!(doc.line_count() >= 2);
        let heading_range = doc.source_map.rendered_lines_for_byte(2);
        assert!(!heading_range.is_empty());
        let para_range = doc.source_map.rendered_lines_for_byte(src.len() - 3);
        assert!(!para_range.is_empty());
    }

    /// A blank line the block above's range absorbs is a blank line, not that block: the quote's
    /// range `> q⏎> r⏎⏎` ends past the blank, but only its own lines (and the last one's `\n`)
    /// answer the quote.
    #[test]
    fn real_block_for_byte_is_none_on_a_blank_line_a_range_absorbs() {
        let src = "para\n\n> q\n> r\n\nafter\n";
        let doc = ParsedDoc::build(src, theme(), false, 24);
        let quote_start = src.find('>').unwrap();
        let quote_last_newline = src.find("r\n").unwrap() + 1;
        let blank_after_quote = quote_last_newline + 1;
        assert!(matches!(
            doc.real_block_for_byte(quote_start),
            Some(Block::BlockQuote { .. })
        ));
        assert!(matches!(
            doc.real_block_for_byte(quote_last_newline),
            Some(Block::BlockQuote { .. })
        ));
        assert!(doc.real_block_for_byte(blank_after_quote).is_none());
        assert!(doc
            .real_block_for_byte(src.find("\n\n").unwrap() + 1)
            .is_none());
        assert!(matches!(
            doc.real_block_for_byte(src.find("after").unwrap()),
            Some(Block::Paragraph { .. })
        ));
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

    /// One blank line matches the count ropey reports for `""`.  With zero lines the
    /// rendered views painted nothing and the cursor was invisible outside raw mode.
    #[test]
    fn empty_doc_owns_one_blank_line_for_the_cursor() {
        let doc = ParsedDoc::build("", theme(), false, 24);
        assert_eq!(doc.line_count(), 1);
        assert_eq!(doc.source_map.block_for_byte(0), Some(0));
        assert!(!doc.source_map.rendered_lines_for_byte(0).is_empty());
    }

    /// An image-only paragraph promotes to a `Block::ImageBlock` reserving
    /// `image_max_height` rows, so visual motion traverses it like any multi-line block.
    #[test]
    fn image_paragraph_promotes_and_reserves_rows() {
        let src = "Above.\n\n![cat](local.png)\n\nBelow.\n";
        let doc = ParsedDoc::build(src, theme(), true, 10);
        let image_byte = src.find('!').expect("image exists");
        let image_block = doc.source_map.block_for_byte(image_byte).unwrap();
        assert_eq!(doc.block_own_line_count(image_block), 10);
    }

    /// A paragraph holding only `$$...$$` display math must be promoted to
    /// an image block (phase 1 block-math design), so the renderer can
    /// reserve multi-row space for the rasterized formula exactly like a
    /// diagram or image block.
    #[test]
    fn display_math_paragraph_promotes_to_image_block() {
        let src = "Above.\n\n$$\nx^2 + y^2 = z^2\n$$\n\nBelow.\n";
        let doc = ParsedDoc::build(src, theme(), true, 10);
        let math_byte = src.find("$$").expect("math exists");
        let math_block = doc
            .source_map
            .block_for_byte(math_byte)
            .expect("math block exists in source map");
        assert!(
            doc.is_image_block(math_block),
            "a $$...$$-only paragraph must promote to an image block (block {}); blocks: {:?}",
            math_block,
            doc.blocks
        );
    }

    /// The promotion must carry the LaTeX source through to
    /// `ImageBlockInfo` (via the diagram-sources map) so the decode worker
    /// can render the formula — same contract mermaid blocks rely on.
    #[test]
    fn promoted_math_block_carries_latex_source() {
        let src = "$$\nE = mc^2\n$$\n";
        let doc = ParsedDoc::build(src, theme(), true, 10);
        let math_blocks: Vec<_> = doc
            .image_blocks
            .iter()
            .filter(|info| matches!(info.source, Some(crate::diagram::DiagramSource::Latex(_))))
            .collect();
        assert_eq!(math_blocks.len(), 1, "image_blocks: {:?}", doc.image_blocks);
        assert!(
            matches!(&math_blocks[0].source, Some(crate::diagram::DiagramSource::Latex(s)) if s.trim() == "E = mc^2"),
            "latex source must be preserved: {:?}",
            math_blocks[0].source
        );
        assert!(
            math_blocks[0].url.starts_with("diagram-math-"),
            "synthetic math url expected: {}",
            math_blocks[0].url
        );
    }

    /// Regression: pulldown-cmark refuses to close a `$$...$$` span whose body has an unbalanced
    /// `{`, so a half-typed `x^{123` degrades to plain text and the block loses its reserved rows
    /// mid-typing.  `reconstruct_broken_display_math` rescues it: the block must still promote to a
    /// Latex image block (whose render then fails cleanly), so the figure stays reserved while the
    /// braces are open.
    #[test]
    fn broken_brace_math_still_promotes_to_a_reserved_image_block() {
        for src in ["$$\nx^2 = z^{123\n$$\n", "$$x^2 = z^{123$$\n"] {
            let doc = ParsedDoc::build(src, theme(), true, 10);
            let math_blocks: Vec<_> = doc
                .image_blocks
                .iter()
                .filter(|info| matches!(info.source, Some(crate::diagram::DiagramSource::Latex(_))))
                .collect();
            assert_eq!(
                math_blocks.len(),
                1,
                "an unbalanced-brace formula must still reserve an image block for {src:?}; blocks: {:?}",
                doc.blocks
            );
            assert!(
                matches!(&math_blocks[0].source, Some(crate::diagram::DiagramSource::Latex(s)) if s.contains("z^{123")),
                "the raw (invalid) latex must be carried through for {src:?}: {:?}",
                math_blocks[0].source
            );
        }
        // Columns shaped like a formula pulldown-cmark parsed itself: only the first line's.
        let src = "$$\nx^2 = z^{123\n$$\n";
        let (mut blocks, ranges) = crate::markdown::parse_raw_with_ranges(src);
        reconstruct_broken_display_math(&mut blocks, &ranges, src);
        let (ok, _) = crate::markdown::parse_raw_with_ranges("$$\nx^2 = z^{123}\n$$\n");
        assert_eq!(blocks[0].src(), ok[0].src());
    }

    /// A paragraph that merely mentions dollar signs — not a lone `$$...$$` block — must NOT be
    /// dragged into display math by the brace-repair pass.
    #[test]
    fn brace_repair_leaves_ordinary_prose_alone() {
        for src in [
            "It cost $$5 and change.\n",            // no closing pair shape
            "$$a$$ then prose then $$b{$$\n",       // interior `$$`: multiple / mixed
            "Prose then $$x^{1$$ trailing words\n", // not delimited end to end
        ] {
            let doc = ParsedDoc::build(src, theme(), true, 10);
            let math_blocks = doc
                .image_blocks
                .iter()
                .filter(|info| matches!(info.source, Some(crate::diagram::DiagramSource::Latex(_))))
                .count();
            assert_eq!(
                math_blocks, 0,
                "prose wrongly promoted for {src:?}: {:?}",
                doc.blocks
            );
        }
    }

    /// Display math is gated on the same consent switch as diagrams. With
    /// `promote_diagrams = false` (the user declined the figures prompt, or
    /// set `[figures].enabled = "never"`) a `$$...$$` paragraph is NOT
    /// promoted to an image block — it stays a paragraph and renders as its
    /// literal source, the way a declined mermaid fence stays a code block.
    /// Regression for the promotion that used to ignore the setting.
    #[test]
    fn display_math_is_not_promoted_when_figures_disabled() {
        let src = "$$\nE = mc^2\n$$\n";
        let doc = ParsedDoc::build_with_overrides(
            src,
            theme(),
            true,
            10,
            None,
            None,
            false,
            80,
            false,
            false,
            /* promote_diagrams */ false,
            /* reflow_paragraphs */ false,
            None,
        );
        assert!(
            doc.image_blocks.is_empty(),
            "no image block should be promoted with figures disabled: {:?}",
            doc.blocks
        );
        // With figures off the `$$...$$` paragraph renders as a
        // fenced-style ` math ` code block (not inline code, not an image):
        // a ` math ` header, the formula body, and a blank closing row.
        // The `$$` delimiters are hidden here and reveal only under the
        // cursor.
        let rendered: String = doc
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(
            rendered.contains("E = mc^2"),
            "the formula body must stay visible when figures are disabled: {rendered:?}"
        );
        assert!(
            rendered.contains("math"),
            "the block must carry a ` math ` header: {rendered:?}"
        );
        assert!(
            !rendered.contains("$$"),
            "the styled block hides the `$$` delimiters (they reveal on cursor): {rendered:?}"
        );
    }

    #[test]
    fn setext_h2_has_two_rendered_lines() {
        let src = "Heading\n-------\n";
        let doc = ParsedDoc::build(src, theme(), false, 24);
        let block = doc.source_map.block_for_byte(0).unwrap();
        assert_eq!(doc.block_own_line_count(block), 2);
    }

    /// The reveal classification: mermaid and `$$...$$` math blocks are
    /// both multi-line diagram images whose raw source must paint 1:1
    /// over the reserved rows on cursor reveal — ordinary `![alt](url)`
    /// images are single-line and stay on the generic image path.
    #[test]
    fn diagram_reveal_classification_covers_latex_and_mermaid_only() {
        let src = "![logo](logo.png)\n\n\
                   ```mermaid\ngraph TD\nA-->B\n```\n\n\
                   $$\nE = mc^2\n$$\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);
        let latex = doc
            .image_blocks
            .iter()
            .find(|i| matches!(i.source, Some(crate::diagram::DiagramSource::Latex(_))))
            .expect("latex block")
            .block_idx;
        let mermaid = doc
            .image_blocks
            .iter()
            .find(|i| matches!(i.source, Some(crate::diagram::DiagramSource::Mermaid(_))))
            .expect("mermaid block")
            .block_idx;
        let plain = doc
            .image_blocks
            .iter()
            .find(|i| i.source.is_none())
            .expect("plain image block")
            .block_idx;

        assert!(doc.is_latex_block(latex));
        assert!(doc.is_diagram_reveal_block(latex));
        assert!(!doc.is_mermaid_block(latex));

        assert!(doc.is_mermaid_block(mermaid));
        assert!(doc.is_diagram_reveal_block(mermaid));
        assert!(!doc.is_latex_block(mermaid));

        assert!(!doc.is_latex_block(plain));
        assert!(!doc.is_mermaid_block(plain));
        assert!(!doc.is_diagram_reveal_block(plain));
    }

    /// Two `$$...$$` blocks stacked with no blank line between them (the
    /// common pattern in math documents) must each promote to their own
    /// image block.  pulldown-cmark may fold them into one paragraph with
    /// two DisplayMath events plus a soft break — the promotion must
    /// handle that shape, not only a paragraph holding a single math.
    #[test]
    fn adjacent_display_math_paragraphs_each_promote() {
        let src = "$$\nX = \\begin{bmatrix} 1 & 2 \\end{bmatrix}\n$$\n\
                   $$\nA = \\begin{bmatrix} 1 & 2 & 3 & 4 \\end{bmatrix}\n$$\n";
        let doc = ParsedDoc::build(src, theme(), true, 10);
        let math_blocks: Vec<_> = doc
            .image_blocks
            .iter()
            .filter(|info| matches!(info.source, Some(crate::diagram::DiagramSource::Latex(_))))
            .collect();
        assert_eq!(
            math_blocks.len(),
            2,
            "each stacked $$...$$ block must promote; blocks: {:?}",
            doc.blocks
        );
        // Sources preserved in document order.
        let sources: Vec<&str> = math_blocks
            .iter()
            .map(|info| match &info.source {
                Some(crate::diagram::DiagramSource::Latex(s)) => s.trim(),
                _ => "",
            })
            .collect();
        assert!(sources[0].contains("X ="), "first source: {sources:?}");
        assert!(sources[1].contains("A ="), "second source: {sources:?}");
    }

    /// With figures OFF the same stacked pair (folded by pulldown into one
    /// paragraph) must still split into two separate blocks, each rendering
    /// as its own fenced-style ` math ` code block — not one merged block,
    /// and not inline code.
    #[test]
    fn adjacent_display_math_paragraphs_split_when_figures_off() {
        let src = "$$\nX = 1\n$$\n$$\nA = 2\n$$\n";
        let doc = ParsedDoc::build_with_overrides(
            src,
            theme(),
            true,
            10,
            None,
            None,
            false,
            80,
            false,
            false,
            /* promote_diagrams */ false,
            /* reflow_paragraphs */ false,
            None,
        );
        // Nothing is promoted to an image with figures off.
        assert!(doc.image_blocks.is_empty(), "blocks: {:?}", doc.blocks);
        // Two separate `math` code blocks: two ` math ` header rows, both
        // formula bodies present, and no literal `$$` in the rendered lines
        // (delimiters reveal only under the cursor).
        let rendered: String = doc
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            rendered.matches("math").count(),
            2,
            "expected two ` math ` headers: {rendered:?}"
        );
        assert!(
            rendered.contains("X = 1") && rendered.contains("A = 2"),
            "{rendered:?}"
        );
        assert!(
            !rendered.contains("$$"),
            "delimiters must be hidden: {rendered:?}"
        );
    }

    #[test]
    fn setext_h1_has_two_rendered_lines() {
        let src = "Heading\n=======\n";
        let doc = ParsedDoc::build(src, theme(), false, 24);
        let block = doc.source_map.block_for_byte(0).unwrap();
        assert_eq!(doc.block_own_line_count(block), 2);
    }

    /// A blank line needs its own virtual block or the cursor cannot land on it.
    #[test]
    fn blank_line_is_its_own_block() {
        let src = "First\n\nSecond\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);

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

    /// Navigating consecutive blank lines must land on each in turn.
    #[test]
    fn multiple_blank_lines_each_own_block() {
        let src = "A\n\n\n\nB\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);

        // The blank-line newlines are at bytes 2, 3, 4.
        let b2 = doc.source_map.block_for_byte(2).unwrap();
        let b3 = doc.source_map.block_for_byte(3).unwrap();
        let b4 = doc.source_map.block_for_byte(4).unwrap();
        assert_ne!(b2, b3);
        assert_ne!(b3, b4);
    }

    /// See the phantom-final-line note in `build_with_overrides`.
    #[test]
    fn phantom_final_line_owns_its_own_block() {
        let src = "Alpha\n\nBeta\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);
        let beta_block = doc.source_map.block_for_byte(src.find("Beta").unwrap());
        let phantom_block = doc.source_map.block_for_byte(src.len());
        assert!(beta_block.is_some() && phantom_block.is_some());
        assert_ne!(
            beta_block, phantom_block,
            "cursor at end of source must not share the last real block"
        );
        let range = doc
            .source_map
            .rendered_lines_for_block(phantom_block.unwrap());
        assert_eq!(range, doc.line_count() - 1..doc.line_count());
        let text: String = doc.lines[range.start]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            text.is_empty(),
            "phantom line must render blank, got {text:?}"
        );
    }

    /// No phantom block without a trailing newline.
    #[test]
    fn no_phantom_block_without_trailing_newline() {
        let src = "Alpha\n\nBeta";
        let doc = ParsedDoc::build(src, theme(), true, 24);
        let beta_block = doc.source_map.block_for_byte(src.find("Beta").unwrap());
        let end_block = doc.source_map.block_for_byte(src.len());
        assert_eq!(
            beta_block, end_block,
            "cursor at end of an unterminated line stays in that line's block"
        );
    }

    /// Regression: the merge pass must run before the live-widths override, or the
    /// `tui-columns` comment flashes into the rendered view on every drag event.
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
            false,
            true,
            false,
            None,
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
        // The three letters are stripped, leaving two whitespace runs and so two dashes.
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

    /// The cache must agree with `wrap::visual_rows_for_line` on every line, behind its row's
    /// stated indent.
    #[test]
    fn visual_rows_cache_matches_line_render() {
        let long = "x".repeat(120);
        let src = format!("# Title\n\nshort\n\n{long}\n\nfinal\n");
        let doc = ParsedDoc::build(&src, theme(), true, 24);
        let width = 40;
        for (i, line) in doc.lines.iter().enumerate() {
            let canonical =
                crate::document::wrap::visual_rows_for_line(line, doc.row_indent(i), width).max(1);
            assert_eq!(
                doc.visual_rows_for_line_at(i, width),
                canonical,
                "cache mismatch at line {i}",
            );
        }
    }

    /// The prefix-sum invariant the snapshot builders rely on.
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

    /// A → B → A must answer correctly each time: the width-mismatch rebuild path.
    #[test]
    fn visual_rows_cache_invalidates_on_width_change() {
        let long = "y".repeat(80);
        let src = format!("Hello\n\n{long}\n");
        let doc = ParsedDoc::build(&src, theme(), true, 24);
        let expect = |w: usize| -> Vec<usize> {
            doc.lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    crate::document::wrap::visual_rows_for_line(l, doc.row_indent(i), w).max(1)
                })
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
        for (i, want) in at_40.iter().enumerate() {
            assert_eq!(doc.visual_rows_for_line_at(i, 40), *want);
        }
    }

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

    /// A comment block's `per_block_own` must be 0, so navigation can detect it as hidden
    /// without inspecting the AST variant.
    #[test]
    fn html_comment_block_owns_zero_rendered_lines() {
        let src = "Alpha.\n\n<!-- hidden -->\n\nBeta.\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);
        assert!(
            doc.blocks
                .iter()
                .any(|b| matches!(b, Block::HtmlComment(..))),
            "blocks: {:?}",
            doc.blocks
        );
        let comment_byte = src.find("<!--").unwrap();
        let block_idx = doc
            .source_map
            .block_for_byte(comment_byte)
            .expect("comment bytes must map to a block");
        assert_eq!(doc.block_own_line_count(block_idx), 0);
        for line in &doc.lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                !text.contains("<!--"),
                "comment leaked into rendered output: {text:?}"
            );
        }
    }

    /// Regression guard: a parser refactor once changed the variant the merge looks for.
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
        assert!(
            !doc.blocks
                .iter()
                .any(|b| matches!(b, Block::HtmlComment(..))),
            "comment should have been absorbed: {:?}",
            doc.blocks
        );
    }

    /// A widths comment after a table in a list item reaches the editor pipeline too: the
    /// table takes the widths, and the comment renders nothing.
    #[test]
    fn nested_tui_columns_comment_attaches_and_stays_hidden() {
        let src =
            "- item\n\n  | a | b |\n  |---|---|\n  | 1 | 2 |\n  <!-- tui-columns: [5, _] -->\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);
        let Some(Block::List { items, .. }) = doc.blocks.first() else {
            panic!("got {:?}", doc.blocks);
        };
        assert!(
            items[0].blocks.iter().any(|b| matches!(
                b,
                Block::Table { user_widths: Some(w), .. } if w == &[Some(5), None]
            )),
            "got {:?}",
            items[0].blocks
        );
        for line in &doc.lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                !text.contains("<!--"),
                "comment leaked into rendered output: {text:?}"
            );
        }
    }

    /// A blank line's rendered range must be that line, not the preceding block's last.
    #[test]
    fn blank_line_rendered_range_is_blank_line() {
        let src = "First\n\nSecond\n";
        let doc = ParsedDoc::build(src, theme(), true, 24);

        let first_range = doc.source_map.rendered_lines_for_byte(2);
        let blank_range = doc.source_map.rendered_lines_for_byte(6);
        let second_range = doc.source_map.rendered_lines_for_byte(9);

        assert!(!first_range.is_empty());
        assert!(!blank_range.is_empty());
        assert!(!second_range.is_empty());
        assert!(first_range.end <= blank_range.start);
        assert!(blank_range.end <= second_range.start);
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
