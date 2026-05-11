pub mod list;
pub mod table;
pub mod util;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use tui_big_text::{BigText, PixelSize};

use crate::config::Theme;

use self::util::{link_fallback, link_style_for};
use super::ast::{inlines_to_plain, Block, Inline};

const IMAGE_PREFIX: &str = "Image: ";

/// Callback used by the renderer to look up the aspect-aware row count
/// for an image block.  See `Renderer::with_image_row_override`.
pub type ImageRowOverride<'t> = &'t dyn Fn(&str) -> Option<usize>;

/// Converts a `Vec<Block>` AST into a `Vec<Line<'static>>` ready for ratatui.
pub struct Renderer<'t> {
    pub(super) theme: &'t Theme,
    /// Viewport width in terminal columns; used to size code block backgrounds.
    pub(super) viewport_width: usize,
    /// Whether code block lines should wrap at viewport_width.
    code_wrap: bool,
    /// Maximum reserved rows per `Block::ImageBlock`; fed through from
    /// `ImagesConfig::max_height` so the editor and renderer agree on the
    /// row count.  Ignored when a block isn't an image block.
    image_max_height: usize,
    /// Optional per-image row override keyed by URL.  Returns the aspect-
    /// aware row count when the image has been decoded; `None` when the
    /// image is still pending / failed / absent from the cache.  The
    /// renderer falls back to `image_max_height` whenever this returns
    /// `None`, so pre-decode layout is stable.
    image_row_override: Option<ImageRowOverride<'t>>,
    /// Phase 13 — when true, alternating data rows in tables are filled
    /// with `Theme::table_row_even` / `Theme::table_row_odd`.  Off by
    /// default; opt-in via `config.table.row_striping`.
    pub(super) row_striping: bool,
    /// When true, H1 headings render as 4 rows of "big text" via the
    /// `tui-big-text` widget (Quadrant pixel size).  Falls back to the
    /// regular one-line rendering when the title is too wide for the
    /// viewport or contains non-ASCII characters.  Wired to
    /// `config.editor.big_h1`.
    big_h1: bool,
}

impl<'t> Renderer<'t> {
    pub fn new(theme: &'t Theme) -> Self {
        Self {
            theme,
            viewport_width: 80,
            code_wrap: false,
            image_max_height: 24,
            image_row_override: None,
            row_striping: false,
            big_h1: false,
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

    /// Install a URL → row-count callback that overrides `image_max_height`
    /// per image whenever the callback returns `Some(n)`.  Used to reserve
    /// exactly the rows a decoded image will occupy so wide images don't
    /// leave blank padding rows beneath them.
    pub fn with_image_row_override(mut self, override_fn: ImageRowOverride<'t>) -> Self {
        self.image_row_override = Some(override_fn);
        self
    }

    /// Toggle alternating-row background fill for table data rows.
    /// Wired to `config.table.row_striping`.
    pub fn with_row_striping(mut self, on: bool) -> Self {
        self.row_striping = on;
        self
    }

    /// Enable big-text rendering for H1 headings.  Wired to
    /// `config.editor.big_h1`.
    pub fn with_big_h1(mut self, on: bool) -> Self {
        self.big_h1 = on;
        self
    }

    /// Render a list of top-level blocks to styled lines. Used by tests
    /// in this module and `ui::preview`; production code uses
    /// `render_with_counts` so it also gets per-block line counts.
    #[allow(dead_code)]
    pub fn render(&self, blocks: &[Block]) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for block in blocks {
            self.render_block(block, &mut lines, "");
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
            self.render_block(block, &mut lines, "");
            counts.push(lines.len() - before);
        }

        (lines, counts)
    }

    // ── Block rendering ───────────────────────────────────────────

    pub(super) fn render_block(
        &self,
        block: &Block,
        out: &mut Vec<Line<'static>>,
        indent_prefix: &str,
    ) {
        match block {
            Block::Heading { level, inlines } => {
                self.render_heading(*level, inlines, out);
            }
            Block::Paragraph { inlines } => {
                self.render_paragraph(inlines, out, indent_prefix);
            }
            Block::CodeBlock { language, content } => {
                self.render_code_block(language.as_deref(), content, out);
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
                // Render raw HTML as a muted code-like block.
                for line in html.lines() {
                    out.push(Line::styled(
                        format!("{indent_prefix}{line}"),
                        self.theme.code_block_text,
                    ));
                }
            }
            Block::HtmlComment(_) => {
                // Comments are annotation, not content — emit zero lines in
                // Preview and Rendered modes.  Raw mode reads the rope
                // directly, so the source text stays visible there.
                // `per_block_own` records 0 for this block so navigation
                // and source-map coverage stay consistent.
            }
            Block::ImageBlock { alt, url } => {
                self.render_image_block(alt, url, out);
            }
        }
    }

    // ── Image block ───────────────────────────────────────────────
    //
    // Emits N rows: the first carries the `[Image: alt]` placeholder (so
    // unsupported terminals and raw-reveal still have something textual
    // to show), the remaining rows are empty `Line::raw` entries so the
    // block reserves vertical space for a graphics-capable terminal's
    // image overlay (painted by `ui::image_view::paint_images` after the
    // line-render pass).
    //
    // `N` is:
    //   * The `image_row_override` callback's value when the image has
    //     been decoded, so wide images reserve exactly their aspect
    //     height and don't leave blank rows underneath.
    //   * `image_max_height` otherwise — keeps `per_block_own` stable
    //     during the pending / failed states so navigation doesn't
    //     depend on decode order.

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
        let rows = self
            .image_row_override
            .and_then(|f| f(url))
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
    // Render an H1's inline text as one or two rows of "big text"
    // using `tui_big_text::BigText` with `PixelSize::Octant` (each
    // glyph 4 cells × 2 cells).  Long titles word-wrap onto a second
    // big-text line if they don't fit in the viewport at full size;
    // titles that need a third or more wrapped lines fall back to the
    // regular one-line styled rendering — past two big-text lines the
    // H1 starts dominating the viewport like a poster instead of a
    // heading.
    //
    // Each wrapped chunk is rendered into its own 2-row buffer with
    // `.centered()` alignment so each line centres independently in
    // the viewport.  A subtle `palette.muted` shadow is painted under
    // each word's bottom glyph row, breaking at inter-word spaces.
    //
    // The temporary buffers are pre-filled with `palette.default_bg`
    // so both the cells the BigText widget paints AND the surrounding
    // empty cells carry the real editor background — `Color::Reset`
    // would render as terminal-default (typically the wrong shade).
    //
    // Total emission: `2 * chunks.len() + 1` rendered lines (2 glyph
    // rows per chunk + 1 rule line).
    //
    // Returns `false` and emits nothing when:
    //   * the title contains a non-ASCII character (font8x8 only covers
    //     ASCII and would render the rest as blank squares),
    //   * a single word is wider than the viewport (would need
    //     mid-word breaking that looks worse than the plain fallback),
    //   * or the title needs 3+ wrapped lines to fit.
    // The caller falls back to the regular one-line styled rendering.
    fn try_render_h1_big(&self, inlines: &[Inline], out: &mut Vec<Line<'static>>) -> bool {
        const GLYPH_W_PER_CHAR: usize = 4;
        const GLYPH_H: u16 = 2;
        const MAX_WRAPPED_LINES: usize = 2;

        let plain = inlines_to_plain(inlines);
        // Transliterate common Unicode typography to ASCII equivalents
        // (em/en dash, ellipsis, curly quotes, nbsp).  font8x8's
        // basic_latin glyph set only covers ASCII; anything else would
        // render as a blank square.  After substitution, anything still
        // non-ASCII (accented letters, arrows, emoji, math symbols) is
        // a hard fall back to the regular one-line render.
        let normalised = normalise_for_big_text(plain.trim());
        if normalised.is_empty() || !normalised.is_ascii() {
            return false;
        }
        let viewport = self.viewport_width.max(1);
        let max_chars = viewport / GLYPH_W_PER_CHAR;
        if max_chars == 0 {
            return false;
        }
        let chunks = match word_wrap_for_big_text(&normalised, max_chars) {
            Some(c) if c.len() <= MAX_WRAPPED_LINES => c,
            _ => return false,
        };

        let bg_style = Style::default().bg(self.theme.palette.bg);
        let text_style = self
            .theme
            .h1
            .remove_modifier(Modifier::UNDERLINED)
            .bg(self.theme.palette.bg);
        let muted = self.theme.palette.bg_muted;
        let blank_spacer = Line::styled(" ".repeat(viewport), bg_style);

        for (chunk_idx, chunk) in chunks.iter().enumerate() {
            // Blank spacer row between wrapped chunks so the two big-
            // text lines have a clear gap and don't visually merge into
            // a 4-row block.
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
            // Per-word shadow on the bottom glyph row of THIS chunk.
            let bottom = GLYPH_H - 1;
            let glyph_start_x = (viewport.saturating_sub(chunk_glyph_w)) / 2;
            for (i, ch) in chunk.chars().enumerate() {
                if ch == ' ' {
                    continue;
                }
                let cell_start = glyph_start_x + i * GLYPH_W_PER_CHAR;
                for x in cell_start..cell_start + GLYPH_W_PER_CHAR {
                    buf[(x as u16, bottom)].set_bg(muted);
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
    ) {
        let prefix = indent_prefix.to_string();
        // Split at both HardBreaks and SoftBreaks so every source-level line
        // break produces its own visual line.  CommonMark collapses soft breaks
        // into spaces, but in a TUI editor we preserve the author's line layout
        // so rendered content mirrors the source line-for-line.
        let mut current_spans: Vec<Span<'static>> = Vec::new();

        if !prefix.is_empty() {
            current_spans.push(Span::raw(prefix.clone()));
        }

        for inline in inlines {
            if matches!(inline, Inline::HardBreak | Inline::SoftBreak) {
                out.push(Line::from(std::mem::take(&mut current_spans)));
                if !prefix.is_empty() {
                    current_spans.push(Span::raw(prefix.clone()));
                }
            } else {
                current_spans.extend(self.render_inline(inline, Style::default()));
            }
        }

        // Reads more clearly as "non-empty and not just a single whitespace span";
        // collapsing into a single negation hides the intent.
        #[allow(clippy::nonminimal_bool)]
        if !current_spans.is_empty()
            && !(current_spans.len() == 1 && current_spans[0].content.trim().is_empty())
        {
            out.push(Line::from(current_spans));
        }
    }

    // ── Code block ────────────────────────────────────────────────

    fn render_code_block(
        &self,
        language: Option<&str>,
        content: &str,
        out: &mut Vec<Line<'static>>,
    ) {
        // Split on '\n' and strip exactly one trailing empty string (the artifact
        // of pulldown-cmark always ending code content with '\n').  This ensures
        // that a genuine blank line *within* the code block is preserved while
        // the final newline does not produce a spurious extra blank line.
        let mut raw_lines: Vec<&str> = content.split('\n').collect();
        if raw_lines.last() == Some(&"") {
            raw_lines.pop();
        }

        // Display width: capped at viewport_width so that short lines are never
        // over-padded (which would cause them to wrap in the terminal and produce
        // blank lines after every row of code).
        let block_width = self.viewport_width.max(1);

        // Optional language label shown above the block.
        if let Some(lang) = language {
            out.push(Line::styled(
                format!(" {} ", lang),
                self.theme.code_block_lang,
            ));
        }

        if self.code_wrap {
            // Wrap long lines at viewport_width.
            let wrap_at = self.viewport_width.max(1);
            for line in &raw_lines {
                let chars: Vec<char> = line.chars().collect();
                if chars.is_empty() {
                    // Use NBSP (U+00A0) instead of regular spaces: ratatui's WordWrapper
                    // treats NBSP as non-whitespace and won't produce a spurious extra
                    // blank line for all-whitespace input.
                    let padded = "\u{00A0}".repeat(block_width);
                    out.push(Line::styled(padded, self.theme.code_block_text));
                    continue;
                }
                let mut start = 0;
                while start < chars.len() {
                    let end = (start + wrap_at - 1).min(chars.len());
                    let slice: String = chars[start..end].iter().collect();
                    let padded = format!(" {:<width$}", slice, width = block_width - 1);
                    out.push(Line::styled(padded, self.theme.code_block_text));
                    start = end;
                }
            }
        } else {
            // No wrapping: each source line becomes one display line, padded to
            // block_width with the code background so the coloured block fills
            // the viewport edge.  Lines longer than viewport_width are not
            // truncated here — the terminal clips them — but we never pad
            // beyond viewport_width, so short lines do not wrap.
            for line in &raw_lines {
                if line.is_empty() {
                    // Use NBSP (U+00A0) instead of regular spaces: ratatui's WordWrapper
                    // treats NBSP as non-whitespace and won't produce a spurious extra
                    // blank line for all-whitespace input (preview mode uses Paragraph::wrap).
                    let padded = "\u{00A0}".repeat(block_width);
                    out.push(Line::styled(padded, self.theme.code_block_text));
                } else {
                    let padded = format!(" {:<width$}", line, width = block_width - 1);
                    out.push(Line::styled(padded, self.theme.code_block_text));
                }
            }
        }
    }

    // ── Blockquote ────────────────────────────────────────────────

    fn render_blockquote(&self, blocks: &[Block], out: &mut Vec<Line<'static>>) {
        // Render inner blocks to a temporary buffer, inserting a blank line
        // between consecutive child blocks so blank lines inside the source
        // blockquote (e.g. `>` on its own between paragraphs) remain visible.
        // Each inner line is then prefixed with the ▎ bar.
        let mut inner_lines: Vec<Line<'static>> = Vec::new();
        for (i, block) in blocks.iter().enumerate() {
            if i > 0 {
                inner_lines.push(Line::from(""));
            }
            self.render_block(block, &mut inner_lines, "");
        }

        for line in inner_lines {
            let bar = Span::styled("▎ ", self.theme.blockquote_bar);
            let mut spans = vec![bar];
            // Re-style the existing spans with blockquote text style
            for span in line.spans {
                let content = span.content.into_owned();
                spans.push(Span::styled(content, self.theme.blockquote_text));
            }
            out.push(Line::from(spans));
        }
    }

    // ── Inline width helpers ──────────────────────────────────────

    /// Character width of a single inline as it would appear when rendered.
    /// Used for table column width calculation so borders align with content.
    fn rendered_inline_char_width(&self, inline: &Inline) -> usize {
        match inline {
            Inline::Text(t) => t.chars().count(),
            Inline::Bold(inner)
            | Inline::Italic(inner)
            | Inline::Strikethrough(inner)
            | Inline::Highlight(inner) => self.rendered_inlines_char_width(inner),
            // Code span adds " code " (2 extra chars for leading/trailing spaces).
            Inline::Code(c) => c.chars().count() + 2,
            // Link renders as just the visible text (bracket contents, or a
            // URL/filename fallback when empty).
            Inline::Link { text, url, .. } => {
                let text_width = self.rendered_inlines_char_width(text);
                if text_width == 0 {
                    link_fallback(url).chars().count()
                } else {
                    text_width
                }
            }
            // Image renders as "[Image: <alt-or-filename>]".
            Inline::Image { alt, url } => {
                let name_width = if alt.trim().is_empty() {
                    link_fallback(url).chars().count()
                } else {
                    alt.chars().count()
                };
                IMAGE_PREFIX.chars().count() + name_width + 2
            }
            Inline::HtmlComment(_) => 0,
            Inline::SoftBreak | Inline::HardBreak => 1,
        }
    }

    pub(super) fn rendered_inlines_char_width(&self, inlines: &[Inline]) -> usize {
        inlines
            .iter()
            .map(|i| self.rendered_inline_char_width(i))
            .sum()
    }

    // ── Inline rendering ──────────────────────────────────────────

    pub(super) fn render_inlines(&self, inlines: &[Inline], base: Style) -> Vec<Span<'static>> {
        inlines
            .iter()
            .flat_map(|i| self.render_inline(i, base))
            .collect()
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
                // When the surrounding inline scope carries strikethrough
                // (either an explicit `~~…~~` or a checked task item's
                // muted text), pick the dim code-span style and preserve
                // the CROSSED_OUT modifier so the snippet still reads as
                // struck-through alongside the rest of the run.
                let style = if base.add_modifier.contains(Modifier::CROSSED_OUT) {
                    self.theme.code_span_dim.add_modifier(Modifier::CROSSED_OUT)
                } else {
                    self.theme.code_span
                };
                vec![Span::styled(format!(" {} ", code), style)]
            }

            Inline::Link { text, url, .. } => {
                // Pick a per-link style by URL kind: in-document
                // heading anchors and local files read as more
                // peripheral than full web links per theming.md.
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
                // Zero spans — inline HTML comments are annotation, not
                // visible content.  The surrounding paragraph's other
                // inlines render normally.
                Vec::new()
            }

            Inline::SoftBreak => vec![Span::raw(" ")],

            Inline::HardBreak => {
                // Hard breaks in inline contexts just become a space; the
                // caller (render_paragraph) handles them as line splits.
                vec![Span::raw(" ")]
            }
        }
    }
}

/// Convert one row of a freshly-painted ratatui `Buffer` into a styled
/// `Line<'static>`, coalescing consecutive cells with identical styles
/// into a single `Span`.  Used to lift the output of in-memory widget
/// rendering (e.g. `tui_big_text::BigText`) back into the
/// `Vec<Line<'static>>` model the rest of the renderer pipeline expects.
/// Substitute common Unicode typography characters with their ASCII
/// equivalents so the big-H1 renderer can show them.  font8x8's
/// `basic_latin` glyph table — the only set tui-big-text consults by
/// default — covers exactly U+0020..=U+007E; anything outside that
/// range renders as a blank square.  Substituting `—` → `-`, `…` →
/// `...`, curly quotes → straight, etc. preserves the visual intent
/// of the title without changing what text we're rendering at the
/// document level (the substitution is rendering-only).
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

/// Greedy word-wrap for the big-H1 renderer.  Splits `text` on
/// whitespace and packs words into lines no wider than `max_chars`.
/// Returns `None` if any single word exceeds `max_chars` — in that
/// case the caller falls back to the regular one-line render rather
/// than emitting a hard-broken word that would look worse than no
/// big-text at all.
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

    #[test]
    fn heading_produces_lines() {
        let lines = render("# Hello\n");
        assert!(!lines.is_empty());
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("Hello"));
    }

    #[test]
    fn big_h1_emits_two_glyph_rows_plus_rule() {
        // Octant pixel-size: 4 cells per glyph horizontally, 2 rows tall.
        let theme = Box::leak(Box::new(Theme::default()));
        let r = Renderer::new(theme).with_big_h1(true);
        let lines = r.render(&parse("# Hi\n"));
        // 2 glyph rows + 1 rule line.
        assert_eq!(
            lines.len(),
            3,
            "expected 2 glyph rows + rule, got {}",
            lines.len()
        );
        // The first 2 rows should each contain at least one block glyph.
        for (i, line) in lines.iter().take(2).enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.chars().any(|c| !c.is_ascii() && c != ' '),
                "row {i} had no block glyph: {text:?}"
            );
        }
        // Last row is the H1 rule.
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
        // Non-ASCII title — font8x8 doesn't cover it, so we fall back.
        let lines = r.render(&parse("# Héllo\n"));
        assert_eq!(lines.len(), 2, "expected plain 2-line H1 fallback");
    }

    #[test]
    fn big_h1_falls_back_for_unbreakable_word_wider_than_viewport() {
        let theme = Box::leak(Box::new(Theme::default()));
        // 21 chars × 4 = 84 cells — exceeds the 80-col viewport AND is
        // a single unbreakable word, so word-wrap can't help.
        let r = Renderer::new(theme)
            .with_big_h1(true)
            .with_viewport_width(80);
        let lines = r.render(&parse("# AAAAAAAAAAAAAAAAAAAAA\n"));
        assert_eq!(lines.len(), 2, "expected plain 2-line H1 fallback");
    }

    #[test]
    fn big_h1_falls_back_when_more_than_two_wrapped_lines_needed() {
        let theme = Box::leak(Box::new(Theme::default()));
        // viewport=40 → max 10 chars per big-text line.  Three 9-char
        // words need 3 lines to wrap, which exceeds the 2-line cap.
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
        // viewport=40 → max 10 chars per line.  "hello world!" wraps
        // into ["hello", "world!"].  Emission: chunk1 (2 rows) + blank
        // spacer (1 row) + chunk2 (2 rows) + rule (1 row) = 6 lines.
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
        // Rows 0,1 = chunk 1 glyphs; row 2 = spacer; rows 3,4 = chunk 2
        // glyphs; row 5 = rule.  Spacer row should have NO block glyph
        // characters; chunk rows should each have at least one.
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
        // Em dash would normally fail the ASCII check; the renderer
        // substitutes it with a hyphen so the title still renders big.
        let lines = r.render(&parse("# A — B\n"));
        // 1 chunk × 2 glyph rows + rule = 3 lines, NOT the 2-line plain
        // fallback.  (5 chars × 4 cells = 20, fits in 80.)
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
        // Anything not in the substitution table stays put — accented
        // letters fall through to the ASCII check at the call site,
        // which then triggers the plain-render fallback.
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
        let first_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first_text.contains("foo"));
    }

    #[test]
    fn blockquote_has_bar() {
        let lines = render("> quote\n");
        let first_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first_text.contains('▎'));
    }

    /// A blank line inside a blockquote (`>` with nothing else) must remain
    /// visible as a quoted blank row between the surrounding paragraphs.
    #[test]
    fn blockquote_blank_line_rendered() {
        let lines = render("> first\n>\n> third\n");
        // Expect three lines, all starting with the blockquote bar.
        assert_eq!(lines.len(), 3, "got {} lines", lines.len());
        for line in &lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.starts_with("▎"),
                "line did not start with bar: {text:?}"
            );
        }
        // Middle line's content (after the bar) should be empty / whitespace.
        let middle: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            middle.trim_start_matches('▎').trim().is_empty(),
            "middle line not blank: {middle:?}"
        );
    }

    /// Soft breaks in the source should produce a new visual line — the TUI
    /// editor preserves the author's line layout instead of collapsing to spaces.
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
        // And a plain (non-struck) code span keeps the bright variant.
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
        // task_strikethrough is true by default, so checked items
        // propagate CROSSED_OUT through `base` into the code span.
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

    /// Tables render with a thick double-line separator beneath the header
    /// and a thin separator between successive data rows — so every row
    /// carries a visible bottom border.
    #[test]
    fn table_has_thick_header_separator_and_inter_row_borders() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
        let lines = render(src);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        // Expected layout: top, header, thick, data1, thin, data2, bottom.
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

    /// A multi-row data row whose cell contains styled inlines
    /// (e.g. `**bold**` or `` `code` ``) must preserve the inline
    /// styling on every wrapped sub-line.  Plain-text rendering would
    /// drop the bold/code spans — Phase 13's inline-aware wrap keeps
    /// them.
    #[test]
    fn table_multirow_cell_preserves_inline_styles() {
        // Force a wrap: narrow viewport so the prose cell breaks.
        // The bold word in cell 1 should come out as a styled span
        // (BOLD modifier set) on whichever sub-line it lands on.
        let theme = Box::leak(Box::new(Theme::default()));
        let r = Renderer::new(theme).with_viewport_width(28);

        let blocks = parse(
            "| Name | Notes |\n\
             |---|---|\n\
             | a | This row has a **really** long note |\n",
        );
        let lines = r.render(&blocks);

        // Walk every line of the rendered table and assert that at
        // least one span carries the BOLD modifier with content
        // matching `really` (possibly trimmed by wrap).
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

    #[test]
    fn ordered_list_right_aligns_numbers_when_double_digit() {
        let mut src = String::new();
        for i in 1..=12u32 {
            src.push_str(&format!("{i}. item {i}\n"));
        }
        let lines = render(&src);
        // Single-digit items get a leading space so they align under the
        // two-digit items ("10. "/"11. "/"12. ").  First line should start
        // with " 1. ", not "1. ".
        assert!(
            line_text(&lines[0]).starts_with(" 1. "),
            "got {:?}",
            line_text(&lines[0])
        );
        // Line 9 is " 9. "; line 10 is "10. " (no leading space).
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
        // Source nests at 4 spaces (CommonMark / GFM convention).  Render
        // matches so that switching to raw view doesn't visually shift the
        // nested marker.
        let lines = render("1. outer\n    1. inner\n2. next\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "1. outer");
        assert_eq!(line_text(&lines[1]), "    1. inner");
        assert_eq!(line_text(&lines[2]), "2. next");
    }

    #[test]
    fn nested_bullet_list_uses_two_space_indent() {
        let lines = render("- outer\n    - inner\n- next\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "• outer");
        assert_eq!(line_text(&lines[1]), "  • inner");
        assert_eq!(line_text(&lines[2]), "• next");
    }

    #[test]
    fn task_items_render_as_bullet_plus_checkbox() {
        // Tasks are decorated bullets — the bullet always renders, with
        // the checkbox immediately after.
        let lines = render("- [ ] outer\n    - [ ] inner\n- [ ] next\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "• [ ] outer");
        assert_eq!(line_text(&lines[1]), "  • [ ] inner");
        assert_eq!(line_text(&lines[2]), "• [ ] next");
    }

    #[test]
    fn task_and_plain_bullets_coexist_in_one_list() {
        let lines = render("- regular\n- [ ] task\n- [x] done\n");
        assert_eq!(line_text(&lines[0]), "• regular");
        assert_eq!(line_text(&lines[1]), "• [ ] task");
        assert_eq!(line_text(&lines[2]), "• [✓] done");
    }

    /// Nested-checklist regression: an empty item Tab-indent produces a
    /// blank-line-separated list.  That forces pulldown-cmark into
    /// "loose-list" mode, which wraps each item's content in a
    /// `Paragraph` — the `TaskListMarker` then sits *inside* the
    /// paragraph instead of directly under `Item`.  The parser must pick
    /// up the marker in both positions so the parent items keep their
    /// checkbox rendering instead of regressing to bullets.
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

    // ── HTML comment hiding (Phase 12) ────────────────────────────────────

    #[test]
    fn block_level_html_comment_renders_zero_lines() {
        let lines = render("<!-- hidden -->\n");
        assert_eq!(lines.len(), 0, "got {} lines: {lines:?}", lines.len());
    }

    #[test]
    fn block_level_html_comment_between_paragraphs_is_invisible() {
        // The surrounding paragraphs render normally; the comment contributes
        // zero rendered lines.  Blank-line gap bytes on either side are still
        // tracked by `ParsedDoc` (not tested here — parser/renderer level
        // only).
        let lines = render("alpha\n\n<!-- hidden -->\n\nbeta\n");
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        // No rendered line contains the comment marker.
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
        // The comment markers themselves must not render.
        assert!(!text.contains("<!--"), "got {text:?}");
        // The surrounding words still render.
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
