use std::path::Path;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::config::Theme;

use super::ast::{inlines_to_plain, Block, Inline, ListItem};

const IMAGE_PREFIX: &str = "Image: ";

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
}

impl<'t> Renderer<'t> {
    pub fn new(theme: &'t Theme) -> Self {
        Self {
            theme,
            viewport_width: 80,
            code_wrap: false,
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

    /// Render a list of top-level blocks to styled lines.
    pub fn render(&self, blocks: &[Block]) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for block in blocks {
            self.render_block(block, &mut lines, 0);
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
            self.render_block(block, &mut lines, 0);
            counts.push(lines.len() - before);
        }

        (lines, counts)
    }

    // ── Block rendering ───────────────────────────────────────────

    fn render_block(&self, block: &Block, out: &mut Vec<Line<'static>>, indent: usize) {
        match block {
            Block::Heading { level, inlines } => {
                self.render_heading(*level, inlines, out);
            }
            Block::Paragraph { inlines } => {
                self.render_paragraph(inlines, out, indent);
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
                self.render_list(*ordered, *start, items, out, indent);
            }
            Block::HorizontalRule => {
                out.push(Line::styled("─".repeat(80), self.theme.rule));
            }
            Block::Table {
                col_count,
                headers,
                rows,
            } => {
                self.render_table(*col_count, headers, rows, out);
            }
            Block::Html(html) => {
                // Render raw HTML as a muted code-like block.
                for line in html.lines() {
                    out.push(Line::styled(
                        format!("{}{}", "  ".repeat(indent), line),
                        self.theme.code_block_text,
                    ));
                }
            }
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

    fn render_paragraph(&self, inlines: &[Inline], out: &mut Vec<Line<'static>>, indent: usize) {
        let prefix = "  ".repeat(indent);
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
            self.render_block(block, &mut inner_lines, 0);
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
        indent: usize,
    ) {
        let indent_str = "  ".repeat(indent);
        let mut counter = start.unwrap_or(1);

        for item in items {
            let is_task = item.task.is_some();

            // Task items have no bullet/number — the checkbox is the visual anchor.
            let (marker, marker_style) = if is_task {
                (indent_str.clone(), Style::default())
            } else if ordered {
                let s = format!("{}{}. ", indent_str, counter);
                counter += 1;
                (s, self.theme.list_number)
            } else {
                (format!("{}• ", indent_str), self.theme.list_bullet)
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

            // Empty list item: render just the marker so the block produces ≥1 line.
            if item.blocks.is_empty() {
                out.push(Line::from(vec![Span::styled(marker.clone(), marker_style)]));
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
                            self.render_block(other, out, indent + 1);
                        }
                    }
                } else {
                    // Subsequent blocks in the same item: render with extra indent.
                    self.render_block(block, out, indent + 1);
                }
            }
        }
    }

    // ── Table (Phase 0 — basic rendering) ─────────────────────────

    fn render_table(
        &self,
        col_count: usize,
        headers: &[Vec<Inline>],
        rows: &[Vec<Vec<Inline>>],
        out: &mut Vec<Line<'static>>,
    ) {
        if col_count == 0 {
            return;
        }

        // Calculate column widths based on the RENDERED character width of each
        // cell (not plain-text byte length), so that inline decorations like
        // `code spans` and [links](url) don't break border alignment.
        let mut widths: Vec<usize> = vec![3; col_count];
        for (i, cell) in headers.iter().enumerate() {
            if i < col_count {
                widths[i] = widths[i].max(self.rendered_inlines_char_width(cell));
            }
        }
        for row in rows {
            for (i, cell) in row.iter().enumerate() {
                if i < col_count {
                    widths[i] = widths[i].max(self.rendered_inlines_char_width(cell));
                }
            }
        }

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

        // Header row
        let header_line = self.render_table_row(headers, &widths, col_count, header_style);
        out.push(header_line);

        // Separator: ├─────┼─────┤
        let sep: String = std::iter::once("├".to_string())
            .chain(widths.iter().enumerate().map(|(i, &w)| {
                let corner = if i + 1 < col_count { "┼" } else { "┤" };
                format!("{}{}", "─".repeat(w + 2), corner)
            }))
            .collect();
        out.push(Line::styled(sep, border_style));

        // Data rows
        for row in rows {
            let row_line = self.render_table_row(row, &widths, col_count, self.theme.table_cell);
            out.push(row_line);
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

    fn render_table_row(
        &self,
        cells: &[Vec<Inline>],
        widths: &[usize],
        col_count: usize,
        default_style: Style,
    ) -> Line<'static> {
        let border_style = self.theme.table_border;
        let mut spans = vec![Span::styled("│", border_style)];

        for i in 0..col_count {
            let cell_inlines: &[Inline] = cells.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
            let width = widths.get(i).copied().unwrap_or(3);

            let cell_spans = if cell_inlines.is_empty() {
                // Empty cell: pad with spaces to fill the column.
                let padded = format!(" {:width$} ", "", width = width);
                vec![Span::styled(padded, default_style)]
            } else {
                // Render the cell content and measure its char width so the
                // trailing-space padding keeps borders aligned regardless of
                // what inline decorations are present.
                let rendered = self.render_inlines(cell_inlines, default_style);
                let rendered_width: usize =
                    rendered.iter().map(|s| s.content.chars().count()).sum();
                let pad = width.saturating_sub(rendered_width);
                let mut s = vec![Span::styled(" ", default_style)];
                s.extend(rendered);
                s.push(Span::styled(format!("{} ", " ".repeat(pad)), default_style));
                s
            };

            spans.extend(cell_spans);
            spans.push(Span::styled("│", border_style));
        }

        Line::from(spans)
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
}
