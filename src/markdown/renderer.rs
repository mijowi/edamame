use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::config::Theme;

use super::ast::{Block, Inline, ListItem};

/// Converts a `Vec<Block>` AST into a `Vec<Line<'static>>` ready for ratatui.
pub struct Renderer<'t> {
    theme: &'t Theme,
}

impl<'t> Renderer<'t> {
    pub fn new(theme: &'t Theme) -> Self {
        Self { theme }
    }

    /// Render a list of top-level blocks to styled lines.
    pub fn render(&self, blocks: &[Block]) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for block in blocks {
            self.render_block(block, &mut lines, 0);
        }
        lines
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
            Block::List { ordered, start, items } => {
                self.render_list(*ordered, *start, items, out, indent);
            }
            Block::HorizontalRule => {
                out.push(Line::styled(
                    "─".repeat(80),
                    self.theme.rule,
                ));
                out.push(Line::raw(""));
            }
            Block::Table { col_count, headers, rows } => {
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
                out.push(Line::raw(""));
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

        out.push(Line::raw(""));
    }

    // ── Paragraph ─────────────────────────────────────────────────

    fn render_paragraph(
        &self,
        inlines: &[Inline],
        out: &mut Vec<Line<'static>>,
        indent: usize,
    ) {
        let prefix = "  ".repeat(indent);
        // Split at HardBreaks so each hard-break produces a new visual line.
        let mut current_spans: Vec<Span<'static>> = Vec::new();

        if !prefix.is_empty() {
            current_spans.push(Span::raw(prefix.clone()));
        }

        for inline in inlines {
            if matches!(inline, Inline::HardBreak) {
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

        out.push(Line::raw(""));
    }

    // ── Code block ────────────────────────────────────────────────

    fn render_code_block(
        &self,
        language: Option<&str>,
        content: &str,
        out: &mut Vec<Line<'static>>,
    ) {
        // Top border with optional language tag
        let lang_label: String = match language {
            Some(lang) => format!(" {} ", lang),
            None => String::new(),
        };

        let border_right_len = 80usize.saturating_sub(4 + lang_label.len());
        let top_border = format!("╭─{}{} ╮", lang_label, "─".repeat(border_right_len));

        out.push(Line::styled(top_border, self.theme.code_block_border));

        for line in content.lines() {
            let display = format!("│ {:<78} │", line);
            out.push(Line::styled(display, self.theme.code_block_text));
        }

        let bottom_border = format!("╰{}╯", "─".repeat(80 - 2));
        out.push(Line::styled(bottom_border, self.theme.code_block_border));
        out.push(Line::raw(""));
    }

    // ── Blockquote ────────────────────────────────────────────────

    fn render_blockquote(&self, blocks: &[Block], out: &mut Vec<Line<'static>>) {
        // Render inner blocks to a temporary buffer, then prefix each with ▎
        let mut inner_lines: Vec<Line<'static>> = Vec::new();
        for block in blocks {
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

        out.push(Line::raw(""));
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
            let marker: String = if ordered {
                let s = format!("{}{}. ", indent_str, counter);
                counter += 1;
                s
            } else {
                format!("{}• ", indent_str)
            };

            // Task list prefix
            let task_prefix: Option<Span<'static>> = item.task.map(|checked| {
                if checked {
                    Span::styled("[x] ", self.theme.task_checked)
                } else {
                    Span::styled("[ ] ", self.theme.task_unchecked)
                }
            });

            let marker_style = if ordered {
                self.theme.list_number
            } else {
                self.theme.list_bullet
            };

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
                            let inline_style = if item.task == Some(true) {
                                self.theme.task_checked
                            } else {
                                Style::default()
                            };
                            spans.extend(self.render_inlines(inlines, inline_style));
                            out.push(Line::from(spans));
                            out.push(Line::raw(""));
                        }
                        other => {
                            // Non-paragraph first block: render marker alone, then block
                            out.push(Line::from(vec![Span::styled(
                                marker.clone(),
                                marker_style,
                            )]));
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

        // Calculate column widths from content
        let mut widths: Vec<usize> = vec![3; col_count];
        for (i, cell) in headers.iter().enumerate() {
            if i < col_count {
                let len = super::ast::inlines_to_plain(cell).len();
                widths[i] = widths[i].max(len);
            }
        }
        for row in rows {
            for (i, cell) in row.iter().enumerate() {
                if i < col_count {
                    let len = super::ast::inlines_to_plain(cell).len();
                    widths[i] = widths[i].max(len);
                }
            }
        }

        let border_style = self.theme.table_border;
        let header_style = self.theme.table_header;

        // Top border: ┌─────┬─────┐
        let top: String = std::iter::once("┌".to_string())
            .chain(widths.iter().enumerate().map(|(i, &w)| {
                let sep = if i + 1 < col_count { "┬" } else { "┐" };
                format!("{}{}",  "─".repeat(w + 2), sep)
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
        out.push(Line::raw(""));
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
            let plain = super::ast::inlines_to_plain(cell_inlines);
            let width = widths.get(i).copied().unwrap_or(3);
            let padded = format!(" {:<width$} ", plain, width = width);

            let cell_spans = if cell_inlines.is_empty() {
                vec![Span::styled(padded, default_style)]
            } else {
                let mut s = vec![Span::styled(" ", default_style)];
                s.extend(self.render_inlines(cell_inlines, default_style));
                // Pad to width
                let rendered_len = super::ast::inlines_to_plain(cell_inlines).len();
                let pad = width.saturating_sub(rendered_len);
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

            Inline::Code(code) => {
                vec![Span::styled(format!(" {} ", code), self.theme.code_span)]
            }

            Inline::Link { text, url, .. } => {
                let mut spans = self.render_inlines(text, self.theme.link_text);
                spans.push(Span::styled(format!(" ({})", url), self.theme.link_url));
                spans
            }

            Inline::Image { alt, url: _ } => {
                vec![Span::styled(
                    format!("[img: {}]", alt),
                    self.theme.image_placeholder,
                )]
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
    fn code_block_has_borders() {
        let lines = render("```\nfoo\n```\n");
        let first_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first_text.contains('╭'));
    }

    #[test]
    fn blockquote_has_bar() {
        let lines = render("> quote\n");
        let first_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first_text.contains('▎'));
    }
}
