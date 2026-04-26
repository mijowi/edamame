use std::path::Path;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::config::Theme;

use super::ast::{inlines_to_plain, Block, Inline, ListItem};
use super::table_layout::{self, MIN_COL_WIDTH};

const IMAGE_PREFIX: &str = "Image: ";

/// One character from a styled sequence, tagged with the style its
/// source span carried.  Used by the table renderer's inline-aware
/// wrap pipeline so bold / italic / code-span styling survives a cell
/// breaking across multiple rendered rows.
#[derive(Debug, Clone, Copy)]
struct StyledChar {
    ch: char,
    style: Style,
}

/// Wrap a sequence of styled chars into rows of width ≤ `width`,
/// breaking on whitespace where possible.  A token whose width
/// exceeds `width` is hard-split at character boundaries.  Mirrors
/// the algorithm in `table_layout::wrap_cell` but operates on
/// `StyledChar` so per-char styles are preserved across breaks.
///
/// Returns at least one (possibly empty) row.
fn wrap_styled_chars(chars: &[StyledChar], width: usize) -> Vec<Vec<StyledChar>> {
    if width == 0 {
        return vec![chars.to_vec()];
    }
    if chars.is_empty() {
        return vec![Vec::new()];
    }

    // Tokenize into runs of whitespace+word, mirroring `split_soft`.
    let mut tokens: Vec<Vec<StyledChar>> = Vec::new();
    let mut tok: Vec<StyledChar> = Vec::new();
    let mut in_ws = true;
    for c in chars {
        if c.ch.is_whitespace() {
            if !in_ws && !tok.is_empty() {
                tokens.push(std::mem::take(&mut tok));
            }
            tok.push(*c);
            in_ws = true;
        } else {
            tok.push(*c);
            in_ws = false;
        }
    }
    if !tok.is_empty() {
        tokens.push(tok);
    }

    let mut rows: Vec<Vec<StyledChar>> = Vec::new();
    let mut current: Vec<StyledChar> = Vec::new();
    let mut current_w = 0usize;

    for token in tokens {
        let w = token.len();
        if current.is_empty() {
            if w <= width {
                current.extend(&token);
                current_w = w;
            } else {
                for chunk in hard_split_styled(&token, width) {
                    rows.push(chunk);
                }
                current.clear();
                current_w = 0;
            }
        } else if current_w + w <= width {
            current.extend(&token);
            current_w += w;
        } else {
            rows.push(std::mem::take(&mut current));
            // Drop leading whitespace of the wrapped token before
            // placing it on the new row — matches `wrap_cell`'s
            // `trim_start` behaviour.
            let trimmed: Vec<StyledChar> = token
                .iter()
                .skip_while(|c| c.ch.is_whitespace())
                .copied()
                .collect();
            let tw = trimmed.len();
            if tw <= width {
                current.extend(&trimmed);
                current_w = tw;
            } else {
                for chunk in hard_split_styled(&trimmed, width) {
                    rows.push(chunk);
                }
                current_w = 0;
            }
        }
    }
    if !current.is_empty() {
        rows.push(current);
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows
}

/// Hard-split a token whose char-count exceeds `width` into chunks
/// of size ≤ `width`.  Counterpart of `table_layout::hard_split`
/// for styled sequences.
fn hard_split_styled(token: &[StyledChar], width: usize) -> Vec<Vec<StyledChar>> {
    if width == 0 || token.is_empty() {
        return vec![token.to_vec()];
    }
    let mut rows = Vec::new();
    for chunk in token.chunks(width) {
        rows.push(chunk.to_vec());
    }
    rows
}

/// Append a `StyledChar` slice as a sequence of `Span`s, coalescing
/// runs of consecutive chars that share the same style.  Keeps the
/// output line tight without losing any style transitions.
fn extend_with_styled_chars(out: &mut Vec<Span<'static>>, chars: &[StyledChar]) {
    if chars.is_empty() {
        return;
    }
    let mut current_style = chars[0].style;
    let mut buf = String::new();
    for c in chars {
        if c.style != current_style {
            if !buf.is_empty() {
                out.push(Span::styled(std::mem::take(&mut buf), current_style));
            }
            current_style = c.style;
        }
        buf.push(c.ch);
    }
    if !buf.is_empty() {
        out.push(Span::styled(buf, current_style));
    }
}

/// Number of characters in the longest whitespace-delimited word in `text`.
/// Used by the table renderer to compute a column's `min` — the floor below
/// which `compute_widths` would have to break a word to fit.
fn longest_word_chars(text: &str) -> usize {
    text.split_whitespace()
        .map(|w| w.chars().count())
        .max()
        .unwrap_or(0)
}

/// Truncate `text` to at most `width` character cells.  Used by the table
/// renderer's single-line path when an inline-formatted cell's rendered
/// width exceeds the column allocation: rather than overflowing the
/// trailing border we fall back to plain text and append a `…` to signal
/// the truncation.
fn truncate_to_width(text: &str, width: usize) -> String {
    let mut out = String::with_capacity(width);
    let mut count = 0usize;
    for ch in text.chars() {
        if count >= width {
            break;
        }
        out.push(ch);
        count += 1;
    }
    out
}

/// Callback used by the renderer to look up the aspect-aware row count
/// for an image block.  See `Renderer::with_image_row_override`.
pub type ImageRowOverride<'t> = &'t dyn Fn(&str) -> Option<usize>;

/// Fallback display text for a link/image whose bracket content is empty:
/// the full URL for web-style targets (anything with a scheme or a `#` fragment),
/// otherwise the final path component of the file path.
fn link_fallback(url: &str) -> String {
    if has_url_scheme(url) || url.starts_with('#') {
        return url.to_string();
    }
    Path::new(url)
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| url.to_string())
}

fn has_url_scheme(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Converts a `Vec<Block>` AST into a `Vec<Line<'static>>` ready for ratatui.
pub struct Renderer<'t> {
    theme: &'t Theme,
    /// Viewport width in terminal columns; used to size code block backgrounds.
    viewport_width: usize,
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
    row_striping: bool,
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
        }
    }

    pub fn with_viewport_width(mut self, width: usize) -> Self {
        self.viewport_width = width;
        self
    }

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

    /// Render a list of top-level blocks to styled lines.
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

    fn render_block(&self, block: &Block, out: &mut Vec<Line<'static>>, indent_prefix: &str) {
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
                out.push(Line::styled("─".repeat(80), self.theme.rule));
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

        let prefix = match level {
            H1 => "  ",
            H2 => "  ",
            H3 => "  ",
            H4 => "  ",
            H5 => "  ",
            H6 => "  ",
        };

        let style = self.theme.heading_style(level);
        let mut spans = vec![Span::styled(prefix, style)];
        spans.extend(self.render_inlines(inlines, style));

        out.push(Line::from(spans));

        if level == H1 {
            out.push(Line::styled("─".repeat(80), self.theme.h1_rule));
        }
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

    fn rendered_inlines_char_width(&self, inlines: &[Inline]) -> usize {
        inlines
            .iter()
            .map(|i| self.rendered_inline_char_width(i))
            .sum()
    }

    // ── List ──────────────────────────────────────────────────────

    fn render_list(
        &self,
        ordered: bool,
        start: Option<u64>,
        items: &[ListItem],
        out: &mut Vec<Line<'static>>,
        indent_prefix: &str,
    ) {
        // Per-list-type "gutter" width — the number of cells that the
        // marker (plus its trailing space) occupies, which is also how far
        // nested content / subsequent blocks in each item must be indented
        // so the text column aligns.
        //
        //   unordered:  `• `             → 2 cells
        //   ordered:    ` 1. `           → max(3, max_digits + 2) cells
        //                                   (wider when the list reaches
        //                                    10+ items so two-digit numbers
        //                                    right-align under single-digit
        //                                    ones and every item's text
        //                                    lines up)
        //   task list:  `[ ] `           → 4 cells (no bullet/number; the
        //                                   checkbox is the visual anchor)
        let first_num = start.unwrap_or(1);
        let last_num = first_num + items.len().saturating_sub(1) as u64;
        let digit_width = last_num.to_string().len().max(1);
        let all_task = !items.is_empty() && items.iter().all(|i| i.task.is_some());
        let gutter_width = if all_task {
            4
        } else if ordered {
            digit_width + 2
        } else {
            2
        };
        let child_indent_prefix = format!("{indent_prefix}{}", " ".repeat(gutter_width));

        let mut counter = first_num;
        for item in items {
            let is_task = item.task.is_some();

            // Task items have no bullet/number — the checkbox is the visual anchor.
            let (marker, marker_style) = if is_task {
                (indent_prefix.to_string(), Style::default())
            } else if ordered {
                // Right-align the number inside a `digit_width`-wide slot so
                // multi-digit numbers (10+) don't push their item's text out
                // of alignment with the single-digit items above.
                let s = format!(
                    "{indent_prefix}{counter:>digit_width$}. ",
                    digit_width = digit_width
                );
                counter += 1;
                (s, self.theme.list_number)
            } else {
                (format!("{indent_prefix}• "), self.theme.list_bullet)
            };

            // Task list prefix (checkbox).
            let task_prefix: Option<Span<'static>> = item.task.map(|checked| {
                if checked {
                    Span::styled("[x] ", self.theme.task_checked)
                } else {
                    Span::styled("[ ] ", self.theme.task_unchecked)
                }
            });

            // Checked-item text style: optionally strikethrough.
            let checked_text_style = if item.task == Some(true) {
                if self.theme.task_strikethrough {
                    self.theme
                        .task_checked
                        .add_modifier(ratatui::style::Modifier::CROSSED_OUT)
                } else {
                    self.theme.task_checked
                }
            } else {
                Style::default()
            };

            // Empty list item: render the marker (and the task checkbox, if any)
            // so the block produces ≥1 line.  Without the checkbox branch, an
            // empty task item collapses to an invisible line because the "marker"
            // for task items is just indentation.
            if item.blocks.is_empty() {
                let mut spans = vec![Span::styled(marker.clone(), marker_style)];
                if let Some(tp) = task_prefix.clone() {
                    spans.push(tp);
                }
                out.push(Line::from(spans));
                continue;
            }

            // Render each block in the item.
            for (i, block) in item.blocks.iter().enumerate() {
                if i == 0 {
                    // First block: prepend the marker (and task prefix if any).
                    match block {
                        Block::Paragraph { inlines } => {
                            let mut spans = vec![Span::styled(marker.clone(), marker_style)];
                            if let Some(tp) = task_prefix.clone() {
                                spans.push(tp);
                            }
                            spans.extend(self.render_inlines(inlines, checked_text_style));
                            out.push(Line::from(spans));
                            // No blank line after list items (tight-list style).
                        }
                        other => {
                            // Non-paragraph first block: render marker alone, then block.
                            out.push(Line::from(vec![Span::styled(marker.clone(), marker_style)]));
                            self.render_block(other, out, &child_indent_prefix);
                        }
                    }
                } else {
                    // Subsequent blocks in the same item: render with the
                    // child indent prefix so their text aligns with this
                    // item's text column (hanging-indent layout).
                    self.render_block(block, out, &child_indent_prefix);
                }
            }
        }
    }

    // ── Table ─────────────────────────────────────────────────────
    //
    // Layout pipeline:
    //   1. Per-cell width metrics are computed from the rendered inline
    //      width (`max`) and the longest plain-text word in the cell
    //      (`min`).  Fed to `table_layout::compute_widths` along with the
    //      viewport width so prose columns proportionally absorb slack
    //      while short / numeric columns stay at `max`.
    //   2. Cells whose allocated width is below their natural width get
    //      word-wrapped via `table_layout::wrap_cell` — the row's height
    //      becomes the max wrap count across its cells.
    //   3. Multi-row data rows render as N consecutive lines, each padded
    //      to keep the surrounding `│` borders aligned vertically.

    fn render_table(
        &self,
        col_count: usize,
        headers: &[Vec<Inline>],
        rows: &[Vec<Vec<Inline>>],
        user_widths: Option<&[Option<usize>]>,
        out: &mut Vec<Line<'static>>,
    ) {
        if col_count == 0 {
            return;
        }

        // Per-cell `max` (rendered char width) and `min` (longest word in
        // plain-text form).  Headers participate in the column metrics
        // alongside data rows because a long header word should also keep
        // the column from collapsing past its widest bound.
        let mut cell_max_widths: Vec<Vec<usize>> = Vec::with_capacity(rows.len() + 1);
        let mut cell_min_widths: Vec<Vec<usize>> = Vec::with_capacity(rows.len() + 1);
        let header_max: Vec<usize> = headers
            .iter()
            .take(col_count)
            .map(|c| self.rendered_inlines_char_width(c))
            .collect();
        let header_min: Vec<usize> = headers
            .iter()
            .take(col_count)
            .map(|c| longest_word_chars(&inlines_to_plain(c)))
            .collect();
        cell_max_widths.push(header_max);
        cell_min_widths.push(header_min);
        for row in rows {
            cell_max_widths.push(
                row.iter()
                    .take(col_count)
                    .map(|c| self.rendered_inlines_char_width(c))
                    .collect(),
            );
            cell_min_widths.push(
                row.iter()
                    .take(col_count)
                    .map(|c| longest_word_chars(&inlines_to_plain(c)))
                    .collect(),
            );
        }

        let widths = table_layout::compute_widths(
            &cell_max_widths,
            &cell_min_widths,
            col_count,
            self.viewport_width,
            user_widths,
        );

        let border_style = self.theme.table_border;
        let header_style = self.theme.table_header;

        // Top border: ┌─────┬─────┐
        let top: String = std::iter::once("┌".to_string())
            .chain(widths.iter().enumerate().map(|(i, &w)| {
                let sep = if i + 1 < col_count { "┬" } else { "┐" };
                format!("{}{}", "─".repeat(w + 2), sep)
            }))
            .collect();
        out.push(Line::styled(top, border_style));

        // Header row — may wrap onto multiple lines like data rows do.
        self.render_table_row(headers, &widths, col_count, header_style, out);

        // Thick separator under the header: ┝━━━━━┿━━━━━┥
        // Uses the heavy-horizontal box-drawing glyph (`━`) with light-vertical
        // joins so the stroke renders visibly thicker than the `─` used for
        // inter-row separators while the side pipes stay light to match `│`.
        let thick: String = std::iter::once("┝".to_string())
            .chain(widths.iter().enumerate().map(|(i, &w)| {
                let corner = if i + 1 < col_count { "┿" } else { "┥" };
                format!("{}{}", "━".repeat(w + 2), corner)
            }))
            .collect();
        out.push(Line::styled(thick, border_style));

        // Data rows, each followed by an inter-row separator except the
        // last.  When `row_striping` is off, the separator is a thin
        // box-drawing rule (`├─┼─┤`).  When striping is on, the rule
        // would clash with the alternating background fill — so we
        // emit a *blank* separator whose background matches the row
        // immediately above it.  Visual effect: each data row appears
        // as a 2-row band of its own colour, with no horizontal rule
        // breaking up the stripe.
        let thin: String = std::iter::once("├".to_string())
            .chain(widths.iter().enumerate().map(|(i, &w)| {
                let corner = if i + 1 < col_count { "┼" } else { "┤" };
                format!("{}{}", "─".repeat(w + 2), corner)
            }))
            .collect();
        for (i, row) in rows.iter().enumerate() {
            let cell_style = if self.row_striping {
                if i % 2 == 0 {
                    self.theme.table_cell.patch(self.theme.table_row_even)
                } else {
                    self.theme.table_cell.patch(self.theme.table_row_odd)
                }
            } else {
                self.theme.table_cell
            };
            self.render_table_row(row, &widths, col_count, cell_style, out);
            if i + 1 < rows.len() {
                if self.row_striping {
                    out.push(self.blank_table_separator(&widths, col_count, cell_style));
                } else {
                    out.push(Line::styled(thin.clone(), border_style));
                }
            }
        }

        // Bottom border: └─────┴─────┘
        let bottom: String = std::iter::once("└".to_string())
            .chain(widths.iter().enumerate().map(|(i, &w)| {
                let corner = if i + 1 < col_count { "┴" } else { "┘" };
                format!("{}{}", "─".repeat(w + 2), corner)
            }))
            .collect();
        out.push(Line::styled(bottom, border_style));
    }

    /// Build a stripe-aware blank-separator line — a `│ … │ … │` row
    /// where every cell is filled with NBSP (U+00A0) under the supplied
    /// style.  The leading `│`s remain at the table-border style so the
    /// side edges of the table stay continuous; the cell-padding NBSPs
    /// in between pick up the row-above's bg fill (or no bg for plain
    /// rows).  Replaces the `├─┼─┤` thin rule when `row_striping` is on
    /// so the alternating-band visual rhythm isn't broken by horizontal
    /// rules.
    ///
    /// NBSP rather than regular spaces is the marker that lets
    /// `ui::table_view::classify_table_sub_lines` distinguish a stripe
    /// separator from the empty wrap-continuation line that
    /// `render_table_row` emits for short cells in a multi-row data
    /// row — those use ASCII spaces.  NBSP is visually identical to a
    /// regular space in every terminal we target, so the user never
    /// sees a difference.
    fn blank_table_separator(
        &self,
        widths: &[usize],
        col_count: usize,
        cell_style: Style,
    ) -> Line<'static> {
        let border_style = self.theme.table_border;
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(col_count * 2 + 1);
        spans.push(Span::styled("│", border_style));
        for i in 0..col_count {
            let width = widths.get(i).copied().unwrap_or(MIN_COL_WIDTH);
            spans.push(Span::styled("\u{00A0}".repeat(width + 2), cell_style));
            spans.push(Span::styled("│", border_style));
        }
        Line::from(spans)
    }

    /// Render one logical table row into `out`.  When any cell needs more
    /// than one wrap line, all cells in the row align onto the same number
    /// of rendered lines (shorter cells emit blank-padded continuation
    /// lines so the surrounding `│` borders stay vertically aligned).
    ///
    /// Phase 13: wrap is *inline-aware* — each cell's `Vec<Inline>` is
    /// flattened to a per-char `(char, style)` sequence, wrapped on
    /// whitespace boundaries, then re-grouped into styled spans for
    /// each rendered sub-line.  Bold / italic / code spans preserved
    /// across line breaks.
    fn render_table_row(
        &self,
        cells: &[Vec<Inline>],
        widths: &[usize],
        col_count: usize,
        default_style: Style,
        out: &mut Vec<Line<'static>>,
    ) {
        let border_style = self.theme.table_border;

        // Flatten each cell into a per-char (char, style) sequence and
        // wrap to its column width.  `cell_rows[c]` is `Vec<row>`; each
        // `row` is `Vec<StyledChar>`.  Always returns ≥1 row.
        let mut cell_rows: Vec<Vec<Vec<StyledChar>>> = Vec::with_capacity(col_count);
        for i in 0..col_count {
            let cell_inlines: &[Inline] = cells.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
            let width = widths.get(i).copied().unwrap_or(MIN_COL_WIDTH);
            let chars = self.cell_styled_chars(cell_inlines, default_style);
            let wrapped = wrap_styled_chars(&chars, width);
            cell_rows.push(wrapped);
        }

        let row_height = cell_rows.iter().map(|r| r.len()).max().unwrap_or(1).max(1);

        for sub in 0..row_height {
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(col_count * 4 + 1);
            spans.push(Span::styled("│", border_style));
            for i in 0..col_count {
                let width = widths.get(i).copied().unwrap_or(MIN_COL_WIDTH);
                let row: &[StyledChar] = cell_rows[i].get(sub).map(|v| v.as_slice()).unwrap_or(&[]);
                let row_w: usize = row.iter().map(|c| c.ch.to_string().chars().count()).sum();
                // Cells whose rendered width exceeds the allocated column
                // width truncate with `…` (rare — only fires when a cell
                // is a single un-breakable token that overflows even the
                // hard-split fallback).  Use plain-text fallback for the
                // truncation path so we don't try to paint a partial
                // styled run.
                spans.push(Span::styled(" ", default_style));
                if row_w <= width {
                    extend_with_styled_chars(&mut spans, row);
                    let pad = width.saturating_sub(row_w);
                    spans.push(Span::styled(format!("{} ", " ".repeat(pad)), default_style));
                } else {
                    let plain: String = row.iter().map(|c| c.ch).collect();
                    let truncated = truncate_to_width(&plain, width.saturating_sub(1));
                    spans.push(Span::styled(format!("{truncated}…"), default_style));
                    spans.push(Span::styled(" ", default_style));
                }
                spans.push(Span::styled("│", border_style));
            }
            out.push(Line::from(spans));
        }
    }

    /// Flatten a cell's `Vec<Inline>` into a per-char styled sequence.
    /// Drives the inline-aware wrap pipeline — each emitted character
    /// remembers the style its source span carried (bold / italic / code
    /// span / link / etc.) so the wrapped output preserves formatting
    /// across line breaks.
    fn cell_styled_chars(&self, cell_inlines: &[Inline], default_style: Style) -> Vec<StyledChar> {
        let mut out: Vec<StyledChar> = Vec::new();
        for span in self.render_inlines(cell_inlines, default_style) {
            let style = span.style;
            for ch in span.content.chars() {
                out.push(StyledChar { ch, style });
            }
        }
        out
    }

    // ── Inline rendering ──────────────────────────────────────────

    fn render_inlines(&self, inlines: &[Inline], base: Style) -> Vec<Span<'static>> {
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
                vec![Span::styled(format!(" {} ", code), self.theme.code_span)]
            }

            Inline::Link { text, url, .. } => {
                if inlines_to_plain(text).trim().is_empty() {
                    vec![Span::styled(link_fallback(url), self.theme.link_text)]
                } else {
                    self.render_inlines(text, self.theme.link_text)
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
    fn nested_ordered_list_uses_marker_width_indent() {
        // Outer list is single-digit (gutter = 3), so the nested item's text
        // should start at column 3 (three-space indent).
        let lines = render("1. outer\n    1. inner\n2. next\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "1. outer");
        assert_eq!(line_text(&lines[1]), "   1. inner");
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
    fn nested_task_list_uses_four_space_indent() {
        let lines = render("- [ ] outer\n    - [ ] inner\n- [ ] next\n");
        assert_eq!(lines.len(), 3);
        // Task items render without a bullet marker — the checkbox anchors
        // the item, and nested tasks indent by 4 (the checkbox width).
        assert_eq!(line_text(&lines[0]), "[ ] outer");
        assert_eq!(line_text(&lines[1]), "    [ ] inner");
        assert_eq!(line_text(&lines[2]), "[ ] next");
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
            line_text(&lines[0]).starts_with("[ ] parent"),
            "parent should render as a task item, got {:?}",
            line_text(&lines[0])
        );
        assert!(
            line_text(&lines[lines.len() - 1]).starts_with("[ ] sibling"),
            "sibling should render as a task item, got {:?}",
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
}
