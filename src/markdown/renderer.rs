pub mod list;
pub mod table;
pub mod util;

use std::cell::Cell;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use tui_big_text::{BigText, PixelSize};

use crate::config::Theme;

use self::util::{link_fallback, link_style_for};
use super::ast::{inlines_to_plain, Block, Inline, MetadataKind};
use super::code_layout;
use super::highlight::{self, Token};
use super::render_cache::{is_cache_worthy, RenderCache, RenderSettings};
use super::table_layout::str_cells;

const IMAGE_PREFIX: &str = "Image: ";

/// Aspect-aware row count for an image block, keyed by URL and by **ordinal**
/// — the 0-based index of the block among the document's image blocks, in
/// document order.  See `Renderer::with_image_row_override`.
///
/// The ordinal is what separates two blocks carrying the same URL (one image
/// repeated in a header and a footer).  It matches the index into
/// `ParsedDoc::image_blocks`: both count `Block::ImageBlock`s in document order
/// over the same block list, and the promotions that create one act on
/// top-level blocks only, so no nested image block can shift the count.
pub type ImageRowOverride<'t> = &'t dyn Fn(&str, usize) -> Option<usize>;

/// Converts a `Vec<Block>` AST into a `Vec<Line<'static>>` ready for ratatui.
pub struct Renderer<'t> {
    pub(super) theme: &'t Theme,
    /// Viewport width in terminal columns; used to size code block backgrounds.
    pub(super) viewport_width: usize,
    /// Whether code block lines should wrap at `viewport_width`.
    code_wrap: bool,
    /// Rows reserved per `Block::ImageBlock` absent a row override; from
    /// `ImagesConfig::max_height`, so editor and renderer agree.
    image_max_height: usize,
    /// Per-image row override; `None` from it (pending / failed / uncached
    /// image) falls back to `image_max_height`, so pre-decode layout is stable.
    image_row_override: Option<ImageRowOverride<'t>>,
    /// Ordinal handed to `image_row_override`.  A `Cell` because the render
    /// walk takes `&self`; a `Renderer` is built fresh per `ParsedDoc` build,
    /// so it always starts at 0.  Counted in `render_image_block` rather than
    /// in the render loops so it stays exact under
    /// `render_with_counts_cached`, which skips `render_block` on cache hits.
    image_block_seq: Cell<usize>,
    /// Alternating `Theme::table_row_even` / `table_row_odd` fill for table
    /// data rows; `config.table.row_striping`.
    pub(super) row_striping: bool,
    /// Render H1 headings as big text, falling back to the one-line rendering
    /// when the title is too wide or non-ASCII; `config.editor.big_h1`.
    big_h1: bool,
    /// Per-token colors for fenced code blocks naming a grammar we ship;
    /// `config.editor.syntax_highlighting`.  When false the highlighter is
    /// never called and body rows are single-span lines.
    syntax_highlighting: bool,
    /// Reflow prose paragraphs: a soft break becomes a space and the paragraph
    /// wraps to the viewport as one flow, instead of each source line getting
    /// its own rendered row.  A hard break still forces a row split.  See
    /// `docs/dev/plans/paragraph-reflow.md`.
    reflow_paragraphs: bool,
}

impl<'t> Renderer<'t> {
    pub fn new(theme: &'t Theme) -> Self {
        Self {
            theme,
            viewport_width: 80,
            code_wrap: false,
            image_max_height: 24,
            image_row_override: None,
            image_block_seq: Cell::new(0),
            row_striping: false,
            big_h1: false,
            syntax_highlighting: false,
            reflow_paragraphs: false,
        }
    }

    pub fn with_viewport_width(mut self, width: usize) -> Self {
        self.viewport_width = width;
        self
    }

    /// Used by tests in this module and `ui::preview`.
    #[allow(dead_code)]
    pub fn with_code_wrap(mut self, wrap: bool) -> Self {
        self.code_wrap = wrap;
        self
    }

    pub fn with_image_max_height(mut self, rows: usize) -> Self {
        self.image_max_height = rows.max(1);
        self
    }

    /// Install a `(URL, ordinal)` → row-count callback that overrides
    /// `image_max_height` per image.  Reserves exactly the rows a decoded
    /// image will occupy, and collapses the block whose raw source the cursor
    /// has revealed.
    pub fn with_image_row_override(mut self, override_fn: ImageRowOverride<'t>) -> Self {
        self.image_row_override = Some(override_fn);
        self
    }

    pub fn with_row_striping(mut self, on: bool) -> Self {
        self.row_striping = on;
        self
    }

    pub fn with_big_h1(mut self, on: bool) -> Self {
        self.big_h1 = on;
        self
    }

    pub fn with_syntax_highlighting(mut self, on: bool) -> Self {
        self.syntax_highlighting = on;
        self
    }

    pub fn with_reflow_paragraphs(mut self, on: bool) -> Self {
        self.reflow_paragraphs = on;
        self
    }

    /// Render a list of top-level blocks to styled lines.  Tests and
    /// `ui::preview` only; production uses `render_with_counts`.
    #[allow(dead_code)]
    pub fn render(&self, blocks: &[Block]) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for block in blocks {
            self.render_block(block, &mut lines, "", true);
        }
        lines
    }

    /// Render blocks and also return the number of rendered lines each block produced.
    ///
    /// Returns `(lines, per_block_counts)` where `per_block_counts[i]` is the
    /// number of entries that block `i` appended to `lines`.
    pub fn render_with_counts(&self, blocks: &[Block]) -> (Vec<Line<'static>>, Vec<usize>) {
        let mut lines = Vec::new();
        let mut counts = Vec::with_capacity(blocks.len());

        for block in blocks {
            let before = lines.len();
            self.render_block(block, &mut lines, "", true);
            counts.push(lines.len() - before);
        }

        (lines, counts)
    }

    /// Like [`render_with_counts`](Self::render_with_counts), but memoizes
    /// each block's rendered lines in `cache` so blocks unchanged since the
    /// previous build cost a clone instead of a re-render.  See
    /// [`RenderCache`] for the keying and eviction rules.
    pub fn render_with_counts_cached(
        &self,
        blocks: &[Block],
        cache: &mut RenderCache,
    ) -> (Vec<Line<'static>>, Vec<usize>) {
        let mut prev = cache.begin_build(RenderSettings {
            theme_addr: self.theme as *const Theme as usize,
            viewport_width: self.viewport_width,
            code_wrap: self.code_wrap,
            image_max_height: self.image_max_height,
            row_striping: self.row_striping,
            big_h1: self.big_h1,
            syntax_highlighting: self.syntax_highlighting,
            reflow_paragraphs: self.reflow_paragraphs,
            // Read here rather than threaded in from `EditorState`: the
            // renderer consults the warm grammars, so this is the one place
            // that can't fall out of step with them.  Pinned to 0 when off so
            // toggling can't leave a stale generation in the fingerprint.
            highlight_generation: if self.syntax_highlighting {
                highlight::warm_generation()
            } else {
                0
            },
            // A grammar refused for want of budget warms nothing, so the
            // generation above cannot move for it; without this field the
            // retry `App::tick_syntax_warm` drives would reparse into a fully
            // warm cache and never call the highlighter again.
            highlight_retry_epoch: if self.syntax_highlighting {
                highlight::retry_epoch()
            } else {
                0
            },
        });

        let mut lines = Vec::new();
        let mut counts = Vec::with_capacity(blocks.len());
        for block in blocks {
            let before = lines.len();
            // ImageBlock row counts track the decode cache, which changes
            // without the AST changing — never cache them.  Cheap-to-render
            // blocks bypass the cache too: a hash + line-clone costs more than
            // re-rendering them (#35 §2, `is_cache_worthy`).
            if matches!(block, Block::ImageBlock { .. }) || !is_cache_worthy(block) {
                self.render_block(block, &mut lines, "", true);
            } else if let Some(hit) = cache.entries.get(block) {
                lines.extend(hit.iter().cloned());
            } else if let Some((key, hit)) = prev.remove_entry(block) {
                lines.extend(hit.iter().cloned());
                cache.entries.insert(key, hit);
            } else {
                self.render_block(block, &mut lines, "", true);
                cache
                    .entries
                    .insert(block.clone(), lines[before..].to_vec());
            }
            counts.push(lines.len() - before);
        }

        // `prev` drops here, evicting entries whose block is gone.
        (lines, counts)
    }

    // ── Block rendering ───────────────────────────────────────────

    pub(super) fn render_block(
        &self,
        block: &Block,
        out: &mut Vec<Line<'static>>,
        indent_prefix: &str,
        // Whether `block` is a top-level document block.  Nested calls (blockquote children,
        // list-item blocks, footnote-definition bodies) pass `false` so their paragraphs never
        // reflow — see `render_paragraph`.
        top_level: bool,
    ) {
        match block {
            Block::Heading { level, inlines } => {
                self.render_heading(*level, inlines, out);
            }
            Block::Paragraph { inlines } => {
                // A `$$...$$`-only paragraph that survived promotion (figures disabled) renders
                // as a fenced-style ` math ` code block — the source counterpart of the
                // display-math reveal — matching how a `` ```mermaid `` fence stays a code block
                // when figures are off.
                if let Some(body) =
                    crate::markdown::parser::post_pass::display_math_block_body(block)
                {
                    self.render_code_block(Some("math"), &body, true, out);
                } else {
                    self.render_paragraph(
                        inlines,
                        out,
                        indent_prefix,
                        self.reflow_paragraphs && top_level,
                    );
                }
            }
            Block::CodeBlock {
                language,
                content,
                fenced,
            } => {
                self.render_code_block(language.as_deref(), content, *fenced, out);
            }
            Block::BlockQuote { blocks } => {
                self.render_blockquote(blocks, out);
            }
            Block::List {
                ordered,
                start,
                items,
            } => {
                self.render_list(*ordered, *start, items, out, indent_prefix);
            }
            Block::HorizontalRule => {
                out.push(Line::styled(
                    "─".repeat(self.viewport_width.max(1)),
                    self.theme.rule,
                ));
            }
            Block::Table {
                col_count,
                headers,
                rows,
                user_widths,
            } => {
                self.render_table(*col_count, headers, rows, user_widths.as_deref(), out);
            }
            Block::Html(html) => {
                for line in html.lines() {
                    out.push(Line::styled(
                        format!("{indent_prefix}{line}"),
                        self.theme.code_block_text,
                    ));
                }
            }
            Block::HtmlComment(_) => {
                // Annotation, not content: zero rendered lines (raw mode reads
                // the rope directly, so the source stays visible there).
            }
            Block::ImageBlock { alt, url } => {
                self.render_image_block(alt, url, out);
            }
            Block::MetadataBlock { kind, content } => {
                self.render_metadata_block(*kind, content, out);
            }
            Block::FootnoteDefinition { label, blocks } => {
                self.render_footnote_definition(label, blocks, out);
            }
        }
    }

    // ── Frontmatter ───────────────────────────────────────────────
    //
    // A metadata block renders *verbatim* — one rendered row per source line,
    // every character in its source column — so the raw↔rendered column map
    // stays the identity function and the row count stays 1:1 with the source
    // lines (what the raw reveal requires).  Rendering only adds color.

    fn render_metadata_block(
        &self,
        kind: MetadataKind,
        content: &str,
        out: &mut Vec<Line<'static>>,
    ) {
        let delim = kind.delimiter();
        out.push(Line::styled(
            delim.to_string(),
            self.theme.frontmatter_delimiter,
        ));
        for line in content.lines() {
            out.push(self.metadata_line(kind, line));
        }
        out.push(Line::styled(
            delim.to_string(),
            self.theme.frontmatter_delimiter,
        ));
    }

    /// Split one frontmatter line into a `key`-styled head and a
    /// `value`-styled tail.  Cosmetic — a shallow separator scan, not a
    /// YAML/TOML parse — so an unreadable line renders whole in the value
    /// style.  The spans concatenate back to `line` byte for byte either way.
    fn metadata_line(&self, kind: MetadataKind, line: &str) -> Line<'static> {
        let sep = match kind {
            MetadataKind::Yaml => ':',
            MetadataKind::Toml => '=',
        };
        if let Some(idx) = metadata_key_end(line, sep) {
            let (key, rest) = line.split_at(idx);
            return Line::from(vec![
                Span::styled(key.to_string(), self.theme.frontmatter_key),
                Span::styled(rest.to_string(), self.theme.frontmatter_value),
            ]);
        }
        Line::styled(line.to_string(), self.theme.frontmatter_value)
    }

    // ── Footnote definition ───────────────────────────────────────
    //
    // Rendered in place as `  <label>.  definition body text… ↩`.
    //
    // The leader is column-width-matched to the raw `[^<label>]: ` it replaces,
    // so the 1:1 rendered↔raw column mapping holds across the body.  The
    // trailing `↩` is appended chrome backed by no raw byte, so the mouse layer
    // hit-tests it on the rendered line
    // (`mouse_ops::footnotes::back_link_glyph_at_click`).

    fn render_footnote_definition(
        &self,
        label: &str,
        blocks: &[Block],
        out: &mut Vec<Line<'static>>,
    ) {
        let mut body: Vec<Line<'static>> = Vec::new();
        for b in blocks {
            self.render_block(b, &mut body, "", false);
        }
        let leader = format!("  {label}.  ");
        let cont_indent = " ".repeat(leader.chars().count());
        let back = " ↩";

        if body.is_empty() {
            out.push(Line::from(vec![
                Span::styled(leader, self.theme.footnote),
                Span::styled(back.to_string(), self.theme.footnote),
            ]));
            return;
        }

        let last = body.len() - 1;
        for (i, line) in body.into_iter().enumerate() {
            let mut spans: Vec<Span<'static>> = Vec::new();
            if i == 0 {
                spans.push(Span::styled(leader.clone(), self.theme.footnote));
            } else {
                spans.push(Span::styled(cont_indent.clone(), self.theme.footnote));
            }
            spans.extend(line.spans);
            if i == last {
                spans.push(Span::styled(back.to_string(), self.theme.footnote));
            }
            out.push(Line::from(spans));
        }
    }

    // ── Image block ───────────────────────────────────────────────
    //
    // Emits N rows: an `[Image: alt]` placeholder (what unsupported terminals
    // and raw-reveal show) followed by empty lines reserving space for the
    // overlay `ui::image_view::paint_images` paints afterwards.
    //
    // N comes from `image_row_override` once decoded, else `image_max_height`
    // — which keeps `per_block_own` stable while pending or failed, so
    // navigation doesn't depend on decode order.

    fn render_image_block(&self, alt: &str, url: &str, out: &mut Vec<Line<'static>>) {
        let name = if alt.trim().is_empty() {
            link_fallback(url)
        } else {
            alt.to_owned()
        };
        let placeholder = Line::from(vec![
            Span::styled(format!("[{}", IMAGE_PREFIX), self.theme.image_placeholder),
            Span::styled(
                name,
                self.theme
                    .image_placeholder
                    .add_modifier(Modifier::UNDERLINED),
            ),
            Span::styled("]", self.theme.image_placeholder),
        ]);
        out.push(placeholder);
        let ordinal = self.image_block_seq.get();
        self.image_block_seq.set(ordinal + 1);
        let rows = self
            .image_row_override
            .and_then(|f| f(url, ordinal))
            .unwrap_or(self.image_max_height)
            .max(1);
        for _ in 1..rows {
            out.push(Line::raw(""));
        }
    }

    // ── Heading ───────────────────────────────────────────────────

    fn render_heading(
        &self,
        level: pulldown_cmark::HeadingLevel,
        inlines: &[Inline],
        out: &mut Vec<Line<'static>>,
    ) {
        use pulldown_cmark::HeadingLevel::*;

        if level == H1 && self.big_h1 && self.try_render_h1_big(inlines, out) {
            return;
        }

        let prefix = match level {
            H1 => " ",
            H2 => "  ",
            H3 => "   ",
            H4 => "    ",
            H5 => "     ",
            H6 => "      ",
        };

        let style = self.theme.heading_style(level);
        let prefix_style = style.remove_modifier(Modifier::UNDERLINED | Modifier::CROSSED_OUT);
        let mut spans = vec![Span::styled(prefix, prefix_style)];
        spans.extend(self.render_inlines(inlines, style));

        out.push(Line::from(spans));

        if level == H1 {
            out.push(Line::styled(
                "─".repeat(self.viewport_width.max(1)),
                self.theme.h1_rule,
            ));
        }
    }

    // ── Big H1 ────────────────────────────────────────────────────
    //
    // Renders an H1's text as one or two rows of `tui_big_text::BigText` at
    // `PixelSize::Octant` (4 × 2 cells per glyph), emitting
    // `2 * chunks.len() + 1` lines.  The temporary buffers are pre-filled with
    // the palette background so the surrounding empty cells carry the real
    // editor background — `Color::Reset` would render as terminal-default.
    //
    // Returns `false`, emitting nothing, when the title is non-ASCII (font8x8
    // covers ASCII only), when one word is wider than the viewport, or when it
    // needs 3+ wrapped lines — past two, an H1 reads as a poster.  The caller
    // then falls back to the one-line styled rendering.
    fn try_render_h1_big(&self, inlines: &[Inline], out: &mut Vec<Line<'static>>) -> bool {
        const GLYPH_W_PER_CHAR: usize = 4;
        const GLYPH_H: u16 = 2;
        const MAX_WRAPPED_LINES: usize = 2;

        let plain = inlines_to_plain(inlines);
        let normalized = normalise_for_big_text(plain.trim());
        if normalized.is_empty() || !normalized.is_ascii() {
            return false;
        }
        let viewport = self.viewport_width.max(1);
        let max_chars = viewport / GLYPH_W_PER_CHAR;
        if max_chars == 0 {
            return false;
        }
        let chunks = match word_wrap_for_big_text(&normalized, max_chars) {
            Some(c) if c.len() <= MAX_WRAPPED_LINES => c,
            _ => return false,
        };

        let bg_style = Style::default().bg(self.theme.palette.bg);
        let text_style = self
            .theme
            .h1
            .remove_modifier(Modifier::UNDERLINED)
            .bg(self.theme.palette.bg);
        let shadow_color = self.theme.palette.bg_muted;
        let blank_spacer = Line::styled(" ".repeat(viewport), bg_style);

        for (chunk_idx, chunk) in chunks.iter().enumerate() {
            // Gap between wrapped chunks, so they don't merge into one
            // 4-row block.
            if chunk_idx > 0 {
                out.push(blank_spacer.clone());
            }
            let chunk_glyph_w = chunk.len() * GLYPH_W_PER_CHAR;
            let area = Rect::new(0, 0, viewport as u16, GLYPH_H);
            let mut buf = Buffer::empty(area);
            buf.set_style(area, bg_style);
            let big = BigText::builder()
                .pixel_size(PixelSize::Octant)
                .style(text_style)
                .centered()
                .lines(vec![Line::from(chunk.clone())])
                .build();
            big.render(area, &mut buf);
            let bottom = GLYPH_H - 1;
            let glyph_start_x = (viewport.saturating_sub(chunk_glyph_w)) / 2;
            for (i, ch) in chunk.chars().enumerate() {
                if ch == ' ' {
                    continue;
                }
                let cell_start = glyph_start_x + i * GLYPH_W_PER_CHAR;
                for x in cell_start..cell_start + GLYPH_W_PER_CHAR {
                    buf[(x as u16, bottom)].set_bg(shadow_color);
                }
            }
            for y in 0..GLYPH_H {
                out.push(buffer_row_to_line(&buf, y));
            }
        }
        out.push(Line::styled("─".repeat(viewport), self.theme.h1_rule));
        true
    }

    // ── Paragraph ─────────────────────────────────────────────────

    fn render_paragraph(
        &self,
        inlines: &[Inline],
        out: &mut Vec<Line<'static>>,
        indent_prefix: &str,
        // Reflow this paragraph.  Only true for a genuinely top-level `Block::Paragraph`: the
        // rendered-row ↔ source-line consumers (gutter, mouse, overlay, `EffectiveRows`) key on
        // a top-level `Block::Paragraph` via `real_block_for_byte`.
        reflow: bool,
    ) {
        // A paragraph with a hard break renders as several logical lines, each spanning several
        // source lines — a shape the consumers can't map (they assume a reflowed paragraph is one
        // rendered line).  Fall back to one row per source line for it, exactly like reflow-off.
        let reflow = reflow && !inlines.iter().any(|i| matches!(i, Inline::HardBreak));

        let prefix = indent_prefix.to_string();
        // Reflow joins a paragraph's soft breaks into one flow that `line_render` wraps to the
        // viewport; without it, each break gets its own row (CommonMark collapses soft breaks to
        // spaces, but the rendered form then mirrors the source line-for-line).  Split via
        // `render_inlines`, not inline-by-inline, so adjacent footnote references still fuse.
        let segments: Vec<&[Inline]> = inlines
            .split(|i| !reflow && matches!(i, Inline::HardBreak | Inline::SoftBreak))
            .collect();
        let last = segments.len() - 1;

        for (i, segment) in segments.iter().enumerate() {
            let mut spans: Vec<Span<'static>> = Vec::new();
            if !prefix.is_empty() {
                spans.push(Span::raw(prefix.clone()));
            }
            spans.extend(self.render_inlines(segment, Style::default()));

            // Every break emits its line, blank ones included; only a trailing
            // segment holding nothing but the indent prefix is suppressed.
            #[allow(clippy::nonminimal_bool)]
            let keep = i < last
                || (!spans.is_empty() && !(spans.len() == 1 && spans[0].content.trim().is_empty()));
            if keep {
                out.push(Line::from(spans));
            }
        }
    }

    // ── Code block ────────────────────────────────────────────────

    /// Build one code-block body row: the pad cell, the line's text, and the
    /// background fill out to the viewport edge.
    ///
    /// `tokens` are char ranges **into `text`** — already re-based by the
    /// caller for a wrapped segment.  With none, this reproduces the
    /// single-span pre-highlighting line character for character, which is what
    /// lets an unknown language, a switched-off setting and an over-cap block
    /// share the pre-feature snapshots.
    ///
    /// The leading space is [`code_layout::CODE_PAD_COLS`]; the cursor
    /// indicator, overlays and the mouse hit-test map columns through that
    /// module, so prefix and constant must agree
    /// (`code_block_render_agrees_with_code_layout_column_map` catches drift).
    /// Extra spans don't disturb the mapping: `line_render` flattens spans to
    /// `(char, style)` pairs.
    fn code_body_row(&self, text: &str, tokens: &[Token], block_width: usize) -> Line<'static> {
        let base = self.theme.code_block_text;
        let pad_to = block_width.saturating_sub(code_layout::CODE_PAD_COLS);
        if tokens.is_empty() {
            return Line::styled(format!(" {text:<pad_to$}"), base);
        }

        let chars: Vec<char> = text.chars().collect();
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(tokens.len() * 2 + 2);
        spans.push(Span::styled(" ".repeat(code_layout::CODE_PAD_COLS), base));

        let run = |from: usize, to: usize| -> String { chars[from..to].iter().collect() };
        let mut col = 0usize;
        for token in tokens {
            // Clamp: a grammar reporting past end of line should mis-color,
            // never panic on a slice.
            let start = token.range.start.min(chars.len()).max(col);
            let end = token.range.end.min(chars.len()).max(start);
            if col < start {
                spans.push(Span::styled(run(col, start), base));
            }
            if start < end {
                let style = base.patch(highlight::style_for(self.theme, token.class));
                spans.push(Span::styled(run(start, end), style));
            }
            col = end;
        }
        if col < chars.len() {
            spans.push(Span::styled(run(col, chars.len()), base));
        }

        // Fill to the viewport edge, matching the `{:<pad_to$}` above.
        if let Some(pad) = pad_to.checked_sub(chars.len()).filter(|p| *p > 0) {
            spans.push(Span::styled(" ".repeat(pad), base));
        }
        Line::from(spans).style(base)
    }

    fn render_code_block(
        &self,
        language: Option<&str>,
        content: &str,
        fenced: bool,
        out: &mut Vec<Line<'static>>,
    ) {
        // Strip exactly one trailing empty string (pulldown-cmark always ends
        // code content with '\n'), so a genuine blank line inside the block
        // survives but the final newline adds no spurious row.
        let mut raw_lines: Vec<&str> = content.split('\n').collect();
        if raw_lines.last() == Some(&"") {
            raw_lines.pop();
        }

        // Capped at the viewport so short lines are never over-padded, which
        // would wrap in the terminal and add a blank line after every code row.
        let block_width = self.viewport_width.max(1);

        // Opening-fence row: a ` lang ` label when tagged, else an NBSP-padded
        // placeholder.  The ``` glyphs appear only when the cursor enters the
        // row and `RenderedView` reveals its raw source.
        if fenced {
            if let Some(lang) = language {
                out.push(Line::styled(
                    format!(" {} ", lang),
                    self.theme.code_block_lang,
                ));
            } else {
                let padded = "\u{00A0}".repeat(block_width);
                out.push(Line::styled(padded, self.theme.code_block_text));
            }
        }

        // Token runs per body line; empty when the feature is off, no/unknown
        // language, or over `highlight`'s size caps — all of which collapse to
        // `code_body_row` with no tokens.  Asked once for the whole block, not
        // per line: the grammar's parser state runs across lines, which is what
        // classifies a block comment or multi-line string past its first row.
        let tokens = if self.syntax_highlighting {
            highlight::highlight_block(language, &raw_lines)
        } else {
            Vec::new()
        };
        let row_tokens = |i: usize| tokens.get(i).map(Vec::as_slice).unwrap_or(&[]);

        if self.code_wrap {
            let wrap_at = self.viewport_width.max(1);
            for (i, line) in raw_lines.iter().enumerate() {
                let chars: Vec<char> = line.chars().collect();
                if chars.is_empty() {
                    // NBSP, not spaces: ratatui's WordWrapper treats it as
                    // non-whitespace and so emits no extra blank line.
                    let padded = "\u{00A0}".repeat(block_width);
                    out.push(Line::styled(padded, self.theme.code_block_text));
                    continue;
                }
                let mut start = 0;
                while start < chars.len() {
                    let end = (start + wrap_at - 1).min(chars.len());
                    let slice: String = chars[start..end].iter().collect();
                    // Tokens address the whole source line, so each segment
                    // takes the overlapping part re-based to its own column 0.
                    let seg = highlight::slice_tokens(row_tokens(i), start, end);
                    out.push(self.code_body_row(&slice, &seg, block_width));
                    start = end;
                }
            }
        } else {
            // One display line per source line, padded to `block_width` so the
            // surface reaches the viewport edge.  Over-long lines are clipped by
            // the terminal, never truncated here; padding never exceeds the
            // viewport, so short lines do not wrap.
            for (i, line) in raw_lines.iter().enumerate() {
                if line.is_empty() {
                    // NBSP, as in the wrapped path above.
                    let padded = "\u{00A0}".repeat(block_width);
                    out.push(Line::styled(padded, self.theme.code_block_text));
                } else {
                    out.push(self.code_body_row(line, row_tokens(i), block_width));
                }
            }
        }

        // Closing-fence placeholder, revealed the same way as the opening one.
        if fenced {
            let padded = "\u{00A0}".repeat(block_width);
            out.push(Line::styled(padded, self.theme.code_block_text));
        }
    }

    // ── Blockquote ────────────────────────────────────────────────

    fn render_blockquote(&self, blocks: &[Block], out: &mut Vec<Line<'static>>) {
        // A blank line between consecutive child blocks keeps a bare `>` in the
        // source visible as a quoted blank row.
        let mut inner_lines: Vec<Line<'static>> = Vec::new();
        for (i, block) in blocks.iter().enumerate() {
            if i > 0 {
                inner_lines.push(Line::from(""));
            }
            self.render_block(block, &mut inner_lines, "", false);
        }

        for line in inner_lines {
            // The quote style is the *base*, not a replacement: each inner span
            // keeps its own resolved style and inherits the wash underneath.
            // Overwriting wholesale silenced every inline style inside a quote
            // (issue #33).  An inner block's line style layers on first, so a
            // nested code block's surface still wins over the wash.
            let base = self.theme.blockquote_text.patch(line.style);
            let bar = Span::styled("▎ ", base.patch(self.theme.blockquote_bar));
            let mut spans = vec![bar];
            for span in line.spans {
                let content = span.content.into_owned();
                spans.push(Span::styled(content, base.patch(span.style)));
            }
            // `line_render` fills trailing cells and wrapped-row indents with
            // the line-level style, so the wash reaches the viewport edge.
            out.push(Line::from(spans).style(base));
        }
    }

    // ── Inline width helpers ──────────────────────────────────────

    /// Terminal-cell width of a single inline as it would appear when rendered.  Used for table
    /// column width calculation so borders align with content: a CJK glyph is two cells.
    fn rendered_inline_width(&self, inline: &Inline) -> usize {
        match inline {
            Inline::Text(t) => str_cells(t),
            Inline::Bold(inner)
            | Inline::Italic(inner)
            | Inline::Strikethrough(inner)
            | Inline::Highlight(inner) => self.rendered_inlines_width(inner),
            // Content only; the backticks are dropped, with no pad cells.
            Inline::Code(c) => str_cells(c),
            // The visible text, or a URL/filename fallback when empty.
            Inline::Link { text, url, .. } => {
                let text_width = self.rendered_inlines_width(text);
                if text_width == 0 {
                    str_cells(&link_fallback(url))
                } else {
                    text_width
                }
            }
            // Image renders as "[Image: <alt-or-filename>]".
            Inline::Image { alt, url } => {
                let name_width = if alt.trim().is_empty() {
                    str_cells(&link_fallback(url))
                } else {
                    str_cells(alt)
                };
                str_cells(IMAGE_PREFIX) + name_width + 2
            }
            Inline::HtmlComment(_) => 0,
            // Unreachable: `footnote_run_at` matches a run of one as readily as
            // a run of three, so the only caller measures every reference before
            // this arm is consulted.  Kept for exhaustiveness, and built from
            // `reference_marker` so it can't state a second format.
            Inline::FootnoteReference { label } => {
                let marker = reference_marker(std::iter::once(label.as_str()));
                str_cells(&marker)
            }
            // Math renders as its delimited source — width equals the raw
            // text width, so table borders and cursor columns stay aligned.
            Inline::Math { source, display } => {
                let delim = if *display { "$$" } else { "$" };
                str_cells(delim) + str_cells(source) + str_cells(delim)
            }
            Inline::SoftBreak | Inline::HardBreak => 1,
        }
    }

    pub(super) fn rendered_inlines_width(&self, inlines: &[Inline]) -> usize {
        let mut total = 0;
        let mut i = 0;
        while i < inlines.len() {
            // Adjacent references fuse, so measure the run through the same
            // helper that renders it.
            if let Some((marker, run_len)) = footnote_run_at(inlines, i) {
                total += str_cells(&marker);
                i += run_len;
                continue;
            }
            total += self.rendered_inline_width(&inlines[i]);
            i += 1;
        }
        total
    }

    // ── Inline rendering ──────────────────────────────────────────

    pub(super) fn render_inlines(&self, inlines: &[Inline], base: Style) -> Vec<Span<'static>> {
        let mut out: Vec<Span<'static>> = Vec::new();
        let mut i = 0;
        while i < inlines.len() {
            // Adjacent references collapse into one marker (`[^1][^2]` →
            // `[1,2]`), so the run is consumed as a group.  This is the only
            // rendering entry point, so the fusion can't be bypassed.
            if let Some((marker, run_len)) = footnote_run_at(inlines, i) {
                out.push(Span::styled(marker, base.patch(self.theme.footnote)));
                i += run_len;
                continue;
            }
            out.extend(self.render_inline(&inlines[i], base));
            i += 1;
        }
        out
    }

    fn render_inline(&self, inline: &Inline, base: Style) -> Vec<Span<'static>> {
        match inline {
            Inline::Text(text) => vec![Span::styled(text.clone(), base)],

            Inline::Bold(inner) => {
                let style = base.patch(self.theme.bold);
                self.render_inlines(inner, style)
            }

            Inline::Italic(inner) => {
                let style = base.patch(self.theme.italic);
                self.render_inlines(inner, style)
            }

            Inline::Strikethrough(inner) => {
                let style = base.patch(self.theme.strikethrough);
                self.render_inlines(inner, style)
            }

            Inline::Highlight(inner) => {
                let style = base.patch(self.theme.highlight);
                self.render_inlines(inner, style)
            }

            Inline::Code(code) => {
                // Under strikethrough (explicit `~~…~~` or a checked task
                // item), take the dim style and keep CROSSED_OUT so the snippet
                // still reads as struck through.
                let style = if base.add_modifier.contains(Modifier::CROSSED_OUT) {
                    self.theme.code_span_dim.add_modifier(Modifier::CROSSED_OUT)
                } else {
                    self.theme.code_span
                };
                vec![Span::styled(code.clone(), style)]
            }

            Inline::Link { text, url, .. } => {
                // Per-link style by URL kind — see docs/dev/theming.md.
                let style = link_style_for(url, self.theme);
                if inlines_to_plain(text).trim().is_empty() {
                    vec![Span::styled(link_fallback(url), style)]
                } else {
                    self.render_inlines(text, style)
                }
            }

            Inline::Image { alt, url } => {
                let name = if alt.trim().is_empty() {
                    link_fallback(url)
                } else {
                    alt.clone()
                };
                vec![
                    Span::styled(format!("[{}", IMAGE_PREFIX), self.theme.image_placeholder),
                    Span::styled(
                        name,
                        self.theme
                            .image_placeholder
                            .add_modifier(Modifier::UNDERLINED),
                    ),
                    Span::styled("]", self.theme.image_placeholder),
                ]
            }

            Inline::HtmlComment(_) => {
                // Annotation, not visible content.
                Vec::new()
            }

            Inline::FootnoteReference { label } => {
                // Unreachable, as in the width arm above: `footnote_run_at`
                // intercepts a lone reference too.  `InlineColMap` accounts for
                // the `[^label]` → `[label]` width difference.
                vec![Span::styled(
                    reference_marker(std::iter::once(label.as_str())),
                    base.patch(self.theme.footnote),
                )]
            }

            // Math renders as its delimited source text in phase 1 —
            // width-equivalent to the raw source, so wrap, cursor columns
            // and the inline column map need no adjustment.  A paragraph
            // holding exactly one display-math inline is promoted to a
            // `Block::ImageBlock` by the post-pass before it ever reaches
            // this arm, so the display form here is the mixed-paragraph
            // fallback only.
            Inline::Math { source, display } => {
                let delim = if *display { "$$" } else { "$" };
                vec![Span::styled(
                    format!("{delim}{source}{delim}"),
                    base.patch(self.theme.code_span),
                )]
            }

            Inline::SoftBreak => vec![Span::raw(" ")],

            Inline::HardBreak => {
                // A space here; `render_paragraph` handles the line split.
                vec![Span::raw(" ")]
            }
        }
    }
}

/// The inline footnote-reference marker: the raw labels of one run of
/// adjacent references, comma-joined inside square brackets (`1` → `[1]`,
/// `note` → `[note]`, `[^1][^2][^3]` → `[1,2,3]`).  Labels are never
/// renumbered for display, so the marker never diverges from the source.
///
/// Matches the `[N]` convention of the bundled export stylesheet
/// (`config/export/default.css`).
///
/// Deliberately plain ASCII: the superscript form (U+207D/U+207E) is absent
/// from most monospace fonts, and a terminal falling back to a proportional
/// face draws it wider than the cell, overlapping the digit.  Do not
/// reintroduce a codepoint outside Basic Latin without re-checking that.
pub(crate) fn reference_marker<'a>(labels: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::from("[");
    for (i, label) in labels.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(label);
    }
    out.push(']');
    out
}

/// If a run of adjacent `Inline::FootnoteReference` starts at `start`,
/// return its fused marker and the number of inlines it consumed.
///
/// "Adjacent" means adjacent *inlines* — `[^1][^2]` fuses, `[^1] [^2]` does
/// not, the space being its own `Inline::Text`.  Rendering and width
/// measurement both route through here so their markers can't drift apart.
fn footnote_run_at(inlines: &[Inline], start: usize) -> Option<(String, usize)> {
    if !matches!(inlines.get(start), Some(Inline::FootnoteReference { .. })) {
        return None;
    }
    let labels: Vec<&str> = inlines[start..]
        .iter()
        .map_while(|inline| match inline {
            Inline::FootnoteReference { label } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    let run_len = labels.len();
    Some((reference_marker(labels), run_len))
}

/// Substitute common Unicode typography with ASCII so the big-H1 renderer can
/// show it: font8x8's `basic_latin` table covers exactly U+0020..=U+007E and
/// renders anything else as a blank square.  Rendering-only — the document text
/// is untouched, and whatever is still non-ASCII afterwards makes the caller
/// fall back to the plain render.
fn normalise_for_big_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\u{2014}' | '\u{2013}' => out.push('-'), // em dash, en dash
            '\u{2026}' => out.push_str("..."),        // ellipsis
            '\u{2018}' | '\u{2019}' => out.push('\''), // curly single quotes
            '\u{201C}' | '\u{201D}' => out.push('"'), // curly double quotes
            '\u{00A0}' => out.push(' '),              // non-breaking space
            other => out.push(other),
        }
    }
    out
}

/// Greedy word-wrap for the big-H1 renderer, packing words into lines no wider
/// than `max_chars`.  `None` if any single word exceeds `max_chars`: the caller
/// then falls back to the plain render rather than hard-breaking a word.
fn word_wrap_for_big_text(text: &str, max_chars: usize) -> Option<Vec<String>> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if word.chars().count() > max_chars {
            return None;
        }
        let needed = if current.is_empty() {
            word.chars().count()
        } else {
            current.chars().count() + 1 + word.chars().count()
        };
        if needed <= max_chars {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines)
    }
}

fn buffer_row_to_line(buf: &Buffer, y: u16) -> Line<'static> {
    let width = buf.area.width;
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current = String::new();
    let mut current_style: Option<Style> = None;
    for x in 0..width {
        let cell = &buf[(x, y)];
        let style = Style::default()
            .fg(cell.fg)
            .bg(cell.bg)
            .add_modifier(cell.modifier);
        match current_style {
            Some(prev) if prev == style => current.push_str(cell.symbol()),
            _ => {
                if let Some(prev) = current_style.take() {
                    spans.push(Span::styled(std::mem::take(&mut current), prev));
                }
                current.push_str(cell.symbol());
                current_style = Some(style);
            }
        }
    }
    if let Some(prev) = current_style {
        spans.push(Span::styled(current, prev));
    }
    Line::from(spans)
}

/// Byte index just past the `key` + separator run of a frontmatter line, or
/// `None` when the line has no readable key.
///
/// Deliberately conservative — first `sep` on the line, non-empty key, and the
/// separator followed by a space or end of line — so `url: https://…` splits at
/// the separator rather than the scheme's colon, and `  - tag` doesn't split.
fn metadata_key_end(line: &str, sep: char) -> Option<usize> {
    let idx = line.find(sep)?;
    if line[..idx]
        .trim_start()
        .trim_start_matches("- ")
        .trim()
        .is_empty()
    {
        return None;
    }
    let end = idx + sep.len_utf8();
    let rest = &line[end..];
    if rest.is_empty() || rest.starts_with(' ') {
        Some(end)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::markdown::parser::parse;

    fn renderer() -> Renderer<'static> {
        // SAFETY: Theme::default() is 'static (no borrows)
        let theme = Box::leak(Box::new(Theme::default()));
        Renderer::new(theme)
    }

    fn render(md: &str) -> Vec<Line<'static>> {
        let blocks = parse(md);
        renderer().render(&blocks)
    }

    // ── Render cache ──────────────────────────────────────────────────

    /// Output-identical to the uncached path on both a cold and a warm cache.
    #[test]
    fn cached_render_matches_uncached() {
        let src = "# Title\n\nSome **bold** prose.\n\n- a\n- b\n\n\
                   | x | y |\n|---|---|\n| 1 | 2 |\n\n```\ncode\n```\n";
        let blocks = parse(src);
        let r = renderer().with_viewport_width(60).with_row_striping(true);
        let (plain_lines, plain_counts) = r.render_with_counts(&blocks);

        let mut cache = RenderCache::default();
        let cold = r.render_with_counts_cached(&blocks, &mut cache);
        let warm = r.render_with_counts_cached(&blocks, &mut cache);
        assert_eq!(cold.0, plain_lines);
        assert_eq!(cold.1, plain_counts);
        assert_eq!(warm.0, plain_lines);
        assert_eq!(warm.1, plain_counts);
    }

    #[test]
    fn cache_evicts_dropped_blocks_and_shares_duplicates() {
        let r = renderer();
        let mut cache = RenderCache::default();

        // Code blocks, not paragraphs: only cache-worthy blocks land in the map
        // now (paragraphs bypass it), so eviction/dedup is only observable on them.
        let first = parse("```\nalpha\n```\n\n```\nbeta\n```\n\n```\nalpha\n```\n");
        assert_eq!(first.len(), 3, "two duplicates plus one distinct block");
        r.render_with_counts_cached(&first, &mut cache);
        assert_eq!(cache.entries.len(), 2, "duplicates share one entry");

        let second = parse("```\nbeta\n```\n\n```\ngamma\n```\n");
        r.render_with_counts_cached(&second, &mut cache);
        assert_eq!(cache.entries.len(), 2);
        assert!(!cache
            .entries
            .keys()
            .any(|b| matches!(b, Block::CodeBlock { content, .. } if content.trim() == "alpha")));
    }

    /// A settings change must invalidate the cache — a stale-width hit would
    /// render tables and code blocks at the wrong width.  Uses a code block: it
    /// is cache-worthy (so the entry actually persists to be invalidated), and
    /// its background fill runs to the viewport edge, so width changes its lines.
    #[test]
    fn cache_cleared_on_settings_change() {
        let blocks = parse("```\ncode\n```\n");
        let mut cache = RenderCache::default();

        let narrow = renderer().with_viewport_width(40);
        let (narrow_lines, _) = narrow.render_with_counts_cached(&blocks, &mut cache);

        let wide = renderer().with_viewport_width(120);
        let (wide_lines, _) = wide.render_with_counts_cached(&blocks, &mut cache);

        assert_ne!(
            narrow_lines, wide_lines,
            "block must re-render at new width"
        );
        assert_eq!(wide_lines, wide.render(&blocks));
    }

    /// The `Block` value is unchanged by the toggle, so without the
    /// `RenderSettings` field a hit would keep painting the old setting.
    #[test]
    fn cache_cleared_when_syntax_highlighting_toggles() {
        let src = "```rust\nfn main() {}\n```\n";
        warm_fence_languages(src);
        let blocks = parse(src);
        let mut cache = RenderCache::default();

        let off = renderer().with_syntax_highlighting(false);
        let (off_lines, _) = off.render_with_counts_cached(&blocks, &mut cache);

        let on = renderer().with_syntax_highlighting(true);
        let (on_lines, _) = on.render_with_counts_cached(&blocks, &mut cache);

        assert_ne!(off_lines, on_lines, "the toggle must re-render the block");
        assert_eq!(on_lines, on.render(&blocks));

        // ...and back again, so the invalidation is not one-way.
        let (off_again, _) = off.render_with_counts_cached(&blocks, &mut cache);
        assert_eq!(off_again, off_lines);
    }

    /// Image row counts track the decode cache, not the AST.
    #[test]
    fn image_blocks_bypass_cache() {
        let blocks = vec![Block::ImageBlock {
            alt: "a".into(),
            url: "img.png".into(),
        }];
        let mut cache = RenderCache::default();
        renderer().render_with_counts_cached(&blocks, &mut cache);
        assert!(cache.entries.is_empty());
    }

    /// Cheap-to-render blocks bypass the cache (#35 §2): a hash + line-clone
    /// costs more than re-rendering them, so nothing lands in the map.
    #[test]
    fn cheap_blocks_bypass_cache() {
        let blocks = parse("# Heading\n\nplain **prose** here.\n\n- a\n- b\n\n---\n");
        let mut cache = RenderCache::default();
        renderer().render_with_counts_cached(&blocks, &mut cache);
        assert!(
            cache.entries.is_empty(),
            "no Table or CodeBlock present, so nothing is cache-worthy: {:?}",
            cache.entries.keys().collect::<Vec<_>>()
        );
    }

    /// The gate follows nesting: a `List`/`BlockQuote` wrapping a `Table` or
    /// `CodeBlock` stays cache-worthy, or the expensive nested render would run
    /// every keystroke.  A container of only cheap content does not.
    #[test]
    fn is_cache_worthy_follows_nested_expensive_content() {
        use crate::markdown::ast::ListItem;

        let code = || Block::CodeBlock {
            language: None,
            content: "x\n".into(),
            fenced: true,
        };
        let table = || Block::Table {
            col_count: 1,
            headers: vec![vec![]],
            rows: vec![],
            user_widths: None,
        };
        let item = |blocks| ListItem {
            blocks,
            task: None,
            blank_lines_before: 0,
        };
        let list = |items| Block::List {
            ordered: false,
            start: None,
            items,
        };

        assert!(is_cache_worthy(&code()));
        assert!(is_cache_worthy(&table()));
        assert!(is_cache_worthy(&Block::BlockQuote {
            blocks: vec![table()]
        }));
        assert!(is_cache_worthy(&list(vec![item(vec![code()])])));

        assert!(!is_cache_worthy(&Block::Paragraph { inlines: vec![] }));
        assert!(!is_cache_worthy(&Block::HorizontalRule));
        assert!(!is_cache_worthy(&Block::BlockQuote {
            blocks: vec![Block::Paragraph { inlines: vec![] }]
        }));
        assert!(!is_cache_worthy(&list(vec![item(vec![
            Block::Paragraph { inlines: vec![] }
        ])])));
    }

    #[test]
    fn heading_produces_lines() {
        let lines = render("# Hello\n");
        assert!(!lines.is_empty());
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("Hello"));
    }

    // ── Paragraph reflow ──────────────────────────────────────────────

    /// Without reflow (the default), each soft break gets its own rendered row.
    #[test]
    fn soft_breaks_split_rows_by_default() {
        let lines = renderer()
            .with_viewport_width(80)
            .render(&parse("one\ntwo\nthree\n"));
        assert_eq!(lines.len(), 3);
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(texts, vec!["one", "two", "three"]);
    }

    /// With reflow on, soft breaks become spaces and the paragraph flows to one
    /// row when it fits the viewport.
    #[test]
    fn reflow_joins_soft_breaks_into_one_flow() {
        let lines = renderer()
            .with_viewport_width(80)
            .with_reflow_paragraphs(true)
            .render(&parse("one\ntwo\nthree\n"));
        assert_eq!(lines.len(), 1, "soft-broken lines should reflow to one row");
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.trim_end(), "one two three");
    }

    /// A paragraph containing a hard break does not reflow at all: it would render as several
    /// logical lines each spanning several source lines, which the reflow-aware consumers can't
    /// map, so it falls back to one row per source line (soft breaks no longer collapse either).
    #[test]
    fn hard_break_paragraph_falls_back_to_one_row_per_line() {
        let lines = renderer()
            .with_viewport_width(80)
            .with_reflow_paragraphs(true)
            .render(&parse("one\ntwo  \nthree\n"));
        let texts: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        assert_eq!(texts, vec!["one", "two", "three"]);
    }

    /// Reflow joins soft breaks into one *logical* line; wrapping to the
    /// viewport is `line_render`'s job downstream, so the renderer emits a single
    /// row here even when the flow is wider than the viewport.
    #[test]
    fn reflow_emits_one_logical_line_wider_than_viewport() {
        let lines = renderer()
            .with_viewport_width(10)
            .with_reflow_paragraphs(true)
            .render(&parse("alpha\nbeta\ngamma\ndelta\n"));
        assert_eq!(
            lines.len(),
            1,
            "the renderer joins soft breaks into one line; wrapping is downstream"
        );
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.trim_end(), "alpha beta gamma delta");
    }

    #[test]
    fn big_h1_emits_two_glyph_rows_plus_rule() {
        let theme = Box::leak(Box::new(Theme::default()));
        let r = Renderer::new(theme).with_big_h1(true);
        let lines = r.render(&parse("# Hi\n"));
        assert_eq!(
            lines.len(),
            3,
            "expected 2 glyph rows + rule, got {}",
            lines.len()
        );
        for (i, line) in lines.iter().take(2).enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.chars().any(|c| !c.is_ascii() && c != ' '),
                "row {i} had no block glyph: {text:?}"
            );
        }
        let rule: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            rule.contains('─'),
            "expected rule glyph in last line, got {rule:?}"
        );
    }

    #[test]
    fn big_h1_falls_back_for_non_ascii_title() {
        let theme = Box::leak(Box::new(Theme::default()));
        let r = Renderer::new(theme).with_big_h1(true);
        let lines = r.render(&parse("# Héllo\n"));
        assert_eq!(lines.len(), 2, "expected plain 2-line H1 fallback");
    }

    #[test]
    fn big_h1_falls_back_for_unbreakable_word_wider_than_viewport() {
        let theme = Box::leak(Box::new(Theme::default()));
        // 21 chars × 4 = 84 cells, over the 80-col viewport, and unbreakable.
        let r = Renderer::new(theme)
            .with_big_h1(true)
            .with_viewport_width(80);
        let lines = r.render(&parse("# AAAAAAAAAAAAAAAAAAAAA\n"));
        assert_eq!(lines.len(), 2, "expected plain 2-line H1 fallback");
    }

    #[test]
    fn big_h1_falls_back_when_more_than_two_wrapped_lines_needed() {
        let theme = Box::leak(Box::new(Theme::default()));
        // Max 10 chars per line; three 9-char words need 3, over the 2-line cap.
        let r = Renderer::new(theme)
            .with_big_h1(true)
            .with_viewport_width(40);
        let lines = r.render(&parse("# alphabet beanbags carriers\n"));
        assert_eq!(
            lines.len(),
            2,
            "expected plain fallback for 3-line wrap, got {}",
            lines.len()
        );
    }

    #[test]
    fn big_h1_word_wraps_to_two_big_lines_with_blank_spacer() {
        let theme = Box::leak(Box::new(Theme::default()));
        // Max 10 chars per line, so this wraps into two chunks:
        // 2 + 1 spacer + 2 + 1 rule = 6 lines.
        let r = Renderer::new(theme)
            .with_big_h1(true)
            .with_viewport_width(40);
        let lines = r.render(&parse("# hello world!\n"));
        assert_eq!(
            lines.len(),
            6,
            "expected 2 chunks × 2 glyphs + spacer + rule, got {}",
            lines.len()
        );
        for &i in &[0usize, 1, 3, 4] {
            let text: String = lines[i].spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.chars().any(|c| !c.is_ascii() && c != ' '),
                "glyph row {i} had no block glyph: {text:?}"
            );
        }
        let spacer: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            !spacer.chars().any(|c| !c.is_ascii() && c != ' '),
            "spacer row should be blank, got {spacer:?}"
        );
        let rule: String = lines[5].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(rule.contains('─'), "expected rule on last line");
    }

    #[test]
    fn big_h1_renders_em_dash_via_ascii_substitution() {
        let theme = Box::leak(Box::new(Theme::default()));
        let r = Renderer::new(theme).with_big_h1(true);
        // Em dash would fail the ASCII check but is substituted with a hyphen,
        // so this is 2 glyph rows + rule, not the 2-line plain fallback.
        let lines = r.render(&parse("# A — B\n"));
        assert_eq!(
            lines.len(),
            3,
            "em dash should transliterate; got plain fallback ({} lines)",
            lines.len()
        );
    }

    #[test]
    fn normalise_substitutes_common_typography() {
        assert_eq!(normalise_for_big_text("hello — world"), "hello - world");
        assert_eq!(normalise_for_big_text("a–b"), "a-b");
        assert_eq!(normalise_for_big_text("yes…"), "yes...");
        assert_eq!(normalise_for_big_text("‘x’"), "'x'");
        assert_eq!(normalise_for_big_text("“x”"), "\"x\"");
        assert_eq!(normalise_for_big_text("a\u{00A0}b"), "a b");
        // Untabled characters stay put and fail the caller's ASCII check.
        assert_eq!(normalise_for_big_text("café"), "café");
    }

    #[test]
    fn big_h1_off_by_default() {
        let theme = Box::leak(Box::new(Theme::default()));
        let r = Renderer::new(theme);
        let lines = r.render(&parse("# Hi\n"));
        assert_eq!(lines.len(), 2, "default Renderer should not produce big H1");
    }

    #[test]
    fn paragraph_produces_lines() {
        let lines = render("Hello world\n");
        assert!(!lines.is_empty());
    }

    #[test]
    fn horizontal_rule_produces_dashes() {
        let lines = render("---\n");
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('─'));
    }

    #[test]
    fn code_block_has_content() {
        let lines = render("```\nfoo\n```\n");
        // Line 0 is the opening-fence placeholder.
        let body_text: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(body_text.contains("foo"));
    }

    // ── Syntax highlighting ───────────────────────────────────────────

    /// Opt this thread into inline grammar compilation for every language
    /// the source's fences name.
    ///
    /// Compilation is asynchronous in production, so without this a render test
    /// asserts on whichever grammars an unrelated test happened to warm first
    /// and passes or fails by test order.  See `highlight::warm_inline`.
    fn warm_fence_languages(src: &str) {
        for line in src.lines() {
            let Some(info) = line.trim_start().strip_prefix("```") else {
                continue;
            };
            if !info.trim().is_empty() {
                highlight::warm_inline(Some(info.trim()));
            }
        }
    }

    /// Render with highlighting on, which the plain `render` helper leaves off.
    fn render_highlighted(src: &str) -> Vec<Line<'static>> {
        warm_fence_languages(src);
        let blocks = parse(src);
        renderer().with_syntax_highlighting(true).render(&blocks)
    }

    /// The flattened `(text, style)` pairs of one rendered line.
    fn spans_of(line: &Line<'static>) -> Vec<(String, Style)> {
        line.spans
            .iter()
            .map(|s| (s.content.to_string(), s.style))
            .collect()
    }

    fn plain_text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_highlighted_code_block_splits_its_body_into_styled_spans() {
        let theme = Theme::default();
        let lines = render_highlighted("```rust\nfn main() {}\n```\n");
        // Line 0 is the ` rust ` label row, line 1 the body.
        let spans = spans_of(&lines[1]);
        assert!(spans.len() > 1, "body should be tokenized, got {spans:?}");
        let keyword = spans.iter().find(|(t, _)| t == "fn").expect("an `fn` span");
        assert_eq!(keyword.1.fg, theme.syntax_keyword.fg);
        let func = spans
            .iter()
            .find(|(t, _)| t == "main")
            .expect("a `main` span");
        assert_eq!(func.1.fg, theme.syntax_function.fg);
    }

    #[test]
    fn token_styles_are_patched_over_the_code_surface() {
        // A token sets a foreground only; the code block's background has
        // to survive, or highlighting punches holes in the surface.
        let theme = Theme::default();
        let lines = render_highlighted("```rust\nfn main() {}\n```\n");
        for (text, style) in spans_of(&lines[1]) {
            assert_eq!(
                style.bg, theme.code_block_text.bg,
                "span {text:?} lost the code surface background"
            );
        }
    }

    #[test]
    fn highlighting_does_not_change_the_text_or_the_row_count() {
        // `code_layout`'s column geometry is a property of the characters, not
        // the spans, so text and row count must survive highlighting.
        let src = "```rust\nfn main() {}\nlet x = 1;\n```\n";
        let plain = render(src);
        let lit = render_highlighted(src);
        assert_eq!(plain.len(), lit.len());
        for (a, b) in plain.iter().zip(&lit) {
            assert_eq!(plain_text(a), plain_text(b));
        }
    }

    #[test]
    fn an_unknown_language_renders_exactly_like_highlighting_off() {
        // Unknown language, no language and feature-off are one path.
        for src in [
            "```frobnicate\nfn main() {}\n```\n",
            "```\nfn main() {}\n```\n",
            "    indented code\n",
        ] {
            let plain = render(src);
            let lit = render_highlighted(src);
            assert_eq!(
                plain.iter().map(spans_of).collect::<Vec<_>>(),
                lit.iter().map(spans_of).collect::<Vec<_>>(),
                "{src:?} should be untouched by highlighting"
            );
        }
    }

    #[test]
    fn the_language_label_row_keeps_the_whole_info_string() {
        // Only the grammar lookup takes the first token.
        let lines = render_highlighted("```rust,ignore\nfn main() {}\n```\n");
        assert!(plain_text(&lines[0]).contains("rust,ignore"));
        // ...and the block is still highlighted, via the `rust` prefix.
        let spans = spans_of(&lines[1]);
        assert!(spans.iter().any(|(t, _)| t == "fn"));
    }

    #[test]
    fn a_wrapped_token_keeps_its_style_on_both_rows() {
        // Wrapping splits a source line into rows, so tokens are clipped and
        // re-based per segment.
        let theme = Theme::default();
        let long = format!("let s = \"{}\";", "x".repeat(60));
        let src = format!("```rust\n{long}\n```\n");
        warm_fence_languages(&src);
        let blocks = parse(&src);
        let lines = renderer()
            .with_viewport_width(20)
            .with_code_wrap(true)
            .with_syntax_highlighting(true)
            .render(&blocks);
        let string_rows = lines
            .iter()
            .filter(|l| {
                spans_of(l)
                    .iter()
                    .any(|(t, s)| t.contains('x') && s.fg == theme.syntax_string.fg)
            })
            .count();
        assert!(
            string_rows > 1,
            "the literal should stay styled across every wrapped row"
        );
    }

    #[test]
    fn blockquote_has_bar() {
        let lines = render("> quote\n");
        let first_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first_text.contains('▎'));
    }

    /// A bare `>` stays visible as a quoted blank row.
    #[test]
    fn blockquote_blank_line_rendered() {
        let lines = render("> first\n>\n> third\n");
        assert_eq!(lines.len(), 3, "got {} lines", lines.len());
        for line in &lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.starts_with("▎"),
                "line did not start with bar: {text:?}"
            );
        }
        let middle: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            middle.trim_start_matches('▎').trim().is_empty(),
            "middle line not blank: {middle:?}"
        );
    }

    /// Soft breaks produce a new visual line rather than collapsing to spaces.
    #[test]
    fn soft_break_produces_new_line() {
        let lines = render("alpha\nbeta\ngamma\n");
        assert_eq!(lines.len(), 3);
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(texts, vec!["alpha", "beta", "gamma"]);
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn web_link_shows_only_text() {
        let lines = render("[Google](https://google.com)\n");
        assert_eq!(line_text(&lines[0]), "Google");
    }

    #[test]
    fn file_link_shows_only_text() {
        let lines = render("[Plan](./plan.md)\n");
        assert_eq!(line_text(&lines[0]), "Plan");
    }

    #[test]
    fn web_link_without_text_shows_url() {
        let lines = render("[](https://google.com)\n");
        assert_eq!(line_text(&lines[0]), "https://google.com");
    }

    #[test]
    fn file_link_without_text_shows_filename_only() {
        let lines = render("[](/home/mjw/Work/plan.md)\n");
        assert_eq!(line_text(&lines[0]), "plan.md");
    }

    #[test]
    fn image_with_alt_shows_alt_prefixed() {
        let lines = render("![Cat](/home/mjw/Pictures/me.jpg)\n");
        assert_eq!(line_text(&lines[0]), "[Image: Cat]");
    }

    #[test]
    fn image_without_alt_shows_filename_prefixed() {
        let lines = render("![](/home/mjw/Pictures/me.jpg)\n");
        assert_eq!(line_text(&lines[0]), "[Image: me.jpg]");
    }

    #[test]
    fn inline_code_inside_strikethrough_uses_dim_code_style() {
        let theme = Box::leak(Box::new(Theme::default()));
        let r = Renderer::new(theme);
        let blocks = parse("~~before `snippet` after~~\n");
        let lines = r.render(&blocks);
        let span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.trim() == "snippet")
            .expect("code-span span");
        assert_eq!(span.style.fg, theme.code_span_dim.fg);
        assert!(
            span.style.add_modifier.contains(Modifier::CROSSED_OUT),
            "code span inside strikethrough should still be struck through"
        );
        let plain_lines = r.render(&parse("alpha `snippet` beta\n"));
        let plain_span = plain_lines[0]
            .spans
            .iter()
            .find(|s| s.content.trim() == "snippet")
            .expect("code-span span");
        assert_eq!(plain_span.style.fg, theme.code_span.fg);
    }

    #[test]
    fn inline_code_inside_checked_task_item_uses_dim_code_style() {
        // `task_strikethrough` defaults on, so checked items propagate
        // CROSSED_OUT through `base` into the code span.
        let theme = Box::leak(Box::new(Theme::default()));
        let r = Renderer::new(theme);
        let blocks = parse("- [x] do `thing` now\n");
        let lines = r.render(&blocks);
        let span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.trim() == "thing")
            .expect("code-span span");
        assert_eq!(span.style.fg, theme.code_span_dim.fg);
        assert!(span.style.add_modifier.contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn link_text_is_underlined() {
        let lines = render("[Google](https://google.com)\n");
        let span = lines[0]
            .spans
            .iter()
            .find(|s| s.content == "Google")
            .expect("link text span");
        assert!(span.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn table_has_thick_header_separator_and_inter_row_borders() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
        let lines = render(src);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        // Layout: top, header, thick, data1, thin, data2, bottom.
        assert_eq!(texts.len(), 7, "got {texts:#?}");
        assert!(texts[0].starts_with('┌'), "top: {:?}", texts[0]);
        assert!(
            texts[2].starts_with('┝') && texts[2].contains('━') && texts[2].contains('┿'),
            "thick sep: {:?}",
            texts[2]
        );
        assert!(
            texts[4].starts_with('├') && texts[4].contains('┼'),
            "thin sep: {:?}",
            texts[4]
        );
        assert!(texts[6].starts_with('└'), "bottom: {:?}", texts[6]);
    }

    /// Plain-text rendering would drop a wrapped cell's bold/code spans; the
    /// inline-aware wrap keeps them.
    #[test]
    fn table_multirow_cell_preserves_inline_styles() {
        let theme = Box::leak(Box::new(Theme::default()));
        let r = Renderer::new(theme).with_viewport_width(28);

        let blocks = parse(
            "| Name | Notes |\n\
             |---|---|\n\
             | a | This row has a **really** long note |\n",
        );
        let lines = r.render(&blocks);

        let mut found_bold = false;
        for line in &lines {
            for span in &line.spans {
                if span.style.add_modifier.contains(Modifier::BOLD)
                    && span.content.contains("really")
                {
                    found_bold = true;
                }
            }
        }
        assert!(
            found_bold,
            "wrapped cell lost the **really** bold styling — multi-row \
             rendering must preserve inline formatting (lines: {lines:#?})",
        );
    }

    /// A long code span doesn't pin its column wide: the cell's `min` is the
    /// breakable floor, so the table compresses and the span hard-splits.
    #[test]
    fn table_breaks_long_inline_code_to_fit_viewport() {
        let src = "| id | code |\n\
                   |----|------|\n\
                   | 1 | `some_extremely_long_identifier_name` |\n";
        let lines = renderer().with_viewport_width(30).render(&parse(src));
        for line in &lines {
            assert!(
                line.width() <= 30,
                "table must compress to the viewport; overflowing line: {:?}",
                line_text(line)
            );
        }
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(
            !texts
                .iter()
                .any(|t| t.contains("some_extremely_long_identifier_name")),
            "code span should hard-split across rows: {texts:#?}"
        );
        let squashed: String = texts
            .join("")
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '│')
            .collect();
        assert!(
            squashed.contains("some_extremely_long_identifier_name"),
            "split must preserve every character in order: {texts:#?}"
        );
    }

    #[test]
    fn table_code_span_wraps_without_pads() {
        let src = "| intro `breakable_code_name` | x |\n\
                   |---|---|\n\
                   | a | b |\n";
        let lines = renderer().with_viewport_width(25).render(&parse(src));
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        let joined = texts.join("\n");
        assert!(
            joined.contains("breakable_") && !joined.contains("breakable_code_name"),
            "code span must hard-split across wrap rows: {texts:#?}"
        );
        assert!(
            !joined.contains('\u{00A0}'),
            "code spans render without pad cells: {texts:#?}"
        );
    }

    /// Long link labels are breakable the same way code spans are.
    #[test]
    fn table_breaks_long_link_to_fit_viewport() {
        let src = "| id | link |\n\
                   |----|------|\n\
                   | 1 | [see-the-full-reference-document-here](https://example.com) |\n";
        let lines = renderer().with_viewport_width(30).render(&parse(src));
        for line in &lines {
            assert!(
                line.width() <= 30,
                "table must compress to the viewport; overflowing line: {:?}",
                line_text(line)
            );
        }
    }

    /// Prose policy is unchanged: a long plain word is never broken — the
    /// table overflows the viewport horizontally instead.
    #[test]
    fn table_never_breaks_long_prose_word() {
        let src = "| id | word |\n\
                   |----|------|\n\
                   | 1 | someextremelylongunbrokenword |\n";
        let lines = renderer().with_viewport_width(30).render(&parse(src));
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("someextremelylongunbrokenword")),
            "prose word must stay intact on one row: {texts:#?}"
        );
        assert!(
            lines.iter().any(|l| l.width() > 30),
            "table should overflow rather than break prose"
        );
    }

    #[test]
    fn ordered_list_right_aligns_numbers_when_double_digit() {
        let mut src = String::new();
        for i in 1..=12u32 {
            src.push_str(&format!("{i}. item {i}\n"));
        }
        let lines = render(&src);
        // Single-digit items get a leading space to align under two-digit ones.
        assert!(
            line_text(&lines[0]).starts_with(" 1. "),
            "got {:?}",
            line_text(&lines[0])
        );
        assert!(
            line_text(&lines[8]).starts_with(" 9. "),
            "got {:?}",
            line_text(&lines[8])
        );
        assert!(
            line_text(&lines[9]).starts_with("10. "),
            "got {:?}",
            line_text(&lines[9])
        );
    }

    #[test]
    fn nested_ordered_list_aligns_with_source_indent() {
        // Render matches the source's 4-space nesting, so switching to raw view
        // doesn't shift the nested marker.
        let lines = render("1. outer\n    1. inner\n2. next\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "1. outer");
        assert_eq!(line_text(&lines[1]), "    1. inner");
        assert_eq!(line_text(&lines[2]), "2. next");
    }

    #[test]
    fn nested_bullet_list_uses_four_space_indent() {
        // Same `INDENT_WIDTH` as the raw source, so de-rendering doesn't shift.
        let lines = render("- outer\n    - inner\n- next\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "• outer");
        assert_eq!(line_text(&lines[1]), "    • inner");
        assert_eq!(line_text(&lines[2]), "• next");
    }

    #[test]
    fn task_items_render_as_bullet_plus_checkbox() {
        // Tasks are decorated bullets: bullet, then checkbox.
        let lines = render("- [ ] outer\n    - [ ] inner\n- [ ] next\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "• [ ] outer");
        assert_eq!(line_text(&lines[1]), "    • [ ] inner");
        assert_eq!(line_text(&lines[2]), "• [ ] next");
    }

    #[test]
    fn task_and_plain_bullets_coexist_in_one_list() {
        let lines = render("- regular\n- [ ] task\n- [x] done\n");
        assert_eq!(line_text(&lines[0]), "• regular");
        assert_eq!(line_text(&lines[1]), "• [ ] task");
        assert_eq!(line_text(&lines[2]), "• [✓] done");
    }

    /// Regression: a blank-line-separated list is "loose", so pulldown-cmark
    /// wraps each item in a `Paragraph` and the `TaskListMarker` sits inside it
    /// rather than under `Item`.  The parser must find it in both positions.
    #[test]
    fn loose_task_list_still_renders_checkboxes() {
        let lines = render("- [ ] parent\n\n    - [ ] nested\n- [ ] sibling\n");
        assert!(
            line_text(&lines[0]).starts_with("• [ ] parent"),
            "parent should render as a task item with bullet, got {:?}",
            line_text(&lines[0])
        );
        assert!(
            line_text(&lines[lines.len() - 1]).starts_with("• [ ] sibling"),
            "sibling should render as a task item with bullet, got {:?}",
            line_text(&lines[lines.len() - 1])
        );
    }

    #[test]
    fn image_name_is_underlined_but_prefix_is_not() {
        let lines = render("![Cat](/tmp/x.jpg)\n");
        let prefix = lines[0]
            .spans
            .iter()
            .find(|s| s.content == "[Image: ")
            .expect("prefix span");
        let name = lines[0]
            .spans
            .iter()
            .find(|s| s.content == "Cat")
            .expect("name span");
        assert!(!prefix.style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(name.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    // ── HTML comment hiding ───────────────────────────────────────────────

    #[test]
    fn block_level_html_comment_renders_zero_lines() {
        let lines = render("<!-- hidden -->\n");
        assert_eq!(lines.len(), 0, "got {} lines: {lines:?}", lines.len());
    }

    #[test]
    fn block_level_html_comment_between_paragraphs_is_invisible() {
        let lines = render("alpha\n\n<!-- hidden -->\n\nbeta\n");
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(
            !texts.iter().any(|t| t.contains("<!--")),
            "comment leaked: {texts:?}"
        );
        assert!(texts.iter().any(|t| t.contains("alpha")));
        assert!(texts.iter().any(|t| t.contains("beta")));
    }

    #[test]
    fn inline_html_comment_is_hidden_from_paragraph() {
        let lines = render("before <!-- inline --> after\n");
        assert_eq!(lines.len(), 1);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains("<!--"), "got {text:?}");
        assert!(text.contains("before"));
        assert!(text.contains("after"));
    }

    #[test]
    fn paragraph_containing_only_inline_comments_renders_zero_lines() {
        let lines = render("<!-- only --><!-- comments -->\n");
        assert_eq!(lines.len(), 0, "got {lines:?}");
    }

    #[test]
    fn setext_h2_renders_same_as_atx_h2() {
        let atx_lines = render("## H2 text\n");
        let setext_lines = render("H2 text\n---\n");
        eprintln!(
            "ATX H2 lines: {:?}",
            atx_lines.iter().map(line_text).collect::<Vec<_>>()
        );
        eprintln!(
            "Setext H2 lines: {:?}",
            setext_lines.iter().map(line_text).collect::<Vec<_>>()
        );
        assert_eq!(
            atx_lines.len(),
            setext_lines.len(),
            "ATX: {:?}, Setext: {:?}",
            atx_lines.iter().map(line_text).collect::<Vec<_>>(),
            setext_lines.iter().map(line_text).collect::<Vec<_>>()
        );
        assert!(
            !setext_lines.iter().map(line_text).any(|t| t.contains('─')),
            "Setext H2 should not have a horizontal rule: {:?}",
            setext_lines.iter().map(line_text).collect::<Vec<_>>()
        );
    }
}
