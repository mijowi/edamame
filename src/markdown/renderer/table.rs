//! `Block::Table` rendering: per-cell width metrics → `compute_widths` →
//! per-row inline-aware wrap → bordered output.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::markdown::ast::Inline;
use crate::markdown::renderer::util::{
    extend_with_styled_chars, link_fallback, styled_cells, truncate_to_width, wrap_styled_chars,
    StyledChar,
};
use crate::markdown::renderer::Renderer;
use crate::markdown::table_layout::{self, char_cells, str_cells, MIN_COL_WIDTH};

/// Floor contribution of a cell token containing *breakable* content (inline code or link
/// text).  Such tokens hard-split across rendered rows, so they don't pin the column to
/// their full length the way a prose word does; `compute_widths` still widens them when
/// the viewport has room.
const BREAKABLE_MIN_WIDTH: usize = 8;

/// Per-cell `min` width for `compute_widths`: the widest run of characters that cannot
/// be broken across rendered rows, in terminal cells.
///
/// Words come from [`table_layout::word_ranges`] — the same split the wrap uses — so a wide
/// glyph is a word to itself and a CJK run floors at its widest glyph.  Prose words are
/// unbreakable ("never break a prose word to fit").  A word containing inline-code or link
/// content contributes at most [`BREAKABLE_MIN_WIDTH`] — but never less than its longest
/// contiguous prose run, so prose glued to a code span keeps its word intact.
fn cell_min_width(inlines: &[Inline]) -> usize {
    let mut chars: Vec<(char, bool)> = Vec::new();
    flatten_breakable_chars(inlines, false, &mut chars);
    let plain: Vec<char> = chars.iter().map(|&(ch, _)| ch).collect();

    let run_cells = |run: &[(char, bool)]| run.iter().map(|&(ch, _)| char_cells(ch)).sum();

    let mut best = 0usize;
    for word in table_layout::word_ranges(&plain) {
        let token = &chars[word];
        let cells = run_cells(token);
        let contribution = if token.iter().any(|&(_, breakable)| breakable) {
            let longest_prose_run = token
                .split(|&(_, breakable)| breakable)
                .map(run_cells)
                .max()
                .unwrap_or(0);
            longest_prose_run.max(cells.min(BREAKABLE_MIN_WIDTH))
        } else {
            cells
        };
        best = best.max(contribution);
    }
    best
}

/// Flatten a cell's inline tree to `(char, breakable)` pairs, mirroring
/// `inlines_to_plain`'s traversal.  Code content and link text are breakable; everything
/// else inherits `breakable` from its enclosing context.
fn flatten_breakable_chars(inlines: &[Inline], breakable: bool, out: &mut Vec<(char, bool)>) {
    for inline in inlines {
        match inline {
            Inline::Text(t) => out.extend(t.chars().map(|c| (c, breakable))),
            Inline::Bold(inner)
            | Inline::Italic(inner)
            | Inline::Strikethrough(inner)
            | Inline::Highlight(inner) => flatten_breakable_chars(inner, breakable, out),
            Inline::Code(c) => out.extend(c.chars().map(|c| (c, true))),
            Inline::Link { text, url, .. } => {
                let before = out.len();
                flatten_breakable_chars(text, true, out);
                if out.len() == before {
                    // Empty bracket text paints the URL / filename fallback.
                    out.extend(link_fallback(url).chars().map(|c| (c, true)));
                }
            }
            Inline::Image { alt, .. } => out.extend(alt.chars().map(|c| (c, false))),
            Inline::HtmlComment(_) | Inline::FootnoteReference { .. } => {}
            // Math content is not breakable — splitting a formula across
            // cell lines would mislead.  Render width = source width
            // (delimiters included), mirroring inlines_to_plain.
            Inline::Math { source, display } => {
                let delim = if *display { "$$" } else { "$" };
                out.extend(delim.chars().map(|c| (c, false)));
                out.extend(source.chars().map(|c| (c, false)));
                out.extend(delim.chars().map(|c| (c, false)));
            }
            Inline::SoftBreak => out.push((' ', false)),
            Inline::HardBreak => out.push(('\n', false)),
        }
    }
}

impl<'t> Renderer<'t> {
    pub(super) fn render_table(
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

        // Headers participate in the column metrics alongside data rows.
        let mut cell_max_widths: Vec<Vec<usize>> = Vec::with_capacity(rows.len() + 1);
        let mut cell_min_widths: Vec<Vec<usize>> = Vec::with_capacity(rows.len() + 1);
        let header_max: Vec<usize> = headers
            .iter()
            .take(col_count)
            .map(|c| self.rendered_inlines_width(c))
            .collect();
        let header_min: Vec<usize> = headers
            .iter()
            .take(col_count)
            .map(|c| cell_min_width(c))
            .collect();
        cell_max_widths.push(header_max);
        cell_min_widths.push(header_min);
        for row in rows {
            cell_max_widths.push(
                row.iter()
                    .take(col_count)
                    .map(|c| self.rendered_inlines_width(c))
                    .collect(),
            );
            cell_min_widths.push(
                row.iter()
                    .take(col_count)
                    .map(|c| cell_min_width(c))
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
        let header_border_style = self.theme.table_header_border;

        let top: String = std::iter::once("┌".to_string())
            .chain(widths.iter().enumerate().map(|(i, &w)| {
                let sep = if i + 1 < col_count { "┬" } else { "┐" };
                format!("{}{}", "─".repeat(w + 2), sep)
            }))
            .collect();
        out.push(Line::styled(top, border_style));

        self.render_table_row(headers, &widths, col_count, header_style, out);

        // Heavy horizontals (`━`) with light-vertical joins, so the header rule reads
        // thicker than the inter-row `─` while the side pipes still match `│`.
        let header_border: String = std::iter::once("┝".to_string())
            .chain(widths.iter().enumerate().map(|(i, &w)| {
                let corner = if i + 1 < col_count { "┿" } else { "┥" };
                format!("{}{}", "━".repeat(w + 2), corner)
            }))
            .collect();
        out.push(Line::styled(header_border, header_border_style));

        // Inter-row separator: a thin `├─┼─┤` rule, or — under `row_striping`, where the
        // rule would clash with the alternating fill — a blank line carrying the row
        // above's background, so each data row reads as a 2-row band.
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

        let bottom: String = std::iter::once("└".to_string())
            .chain(widths.iter().enumerate().map(|(i, &w)| {
                let corner = if i + 1 < col_count { "┴" } else { "┘" };
                format!("{}{}", "─".repeat(w + 2), corner)
            }))
            .collect();
        out.push(Line::styled(bottom, border_style));
    }

    /// Stripe-aware blank separator: a `│ … │ … │` row whose cells carry `cell_style`'s
    /// background while the outer `│`s stay at the border style.
    ///
    /// The fill is NBSP (U+00A0), not a space, so
    /// `ui::table_view::classify_table_sub_lines` can tell a stripe separator from the
    /// ASCII-space wrap-continuation line `render_table_row` emits.  Visually identical.
    fn blank_table_separator(
        &self,
        widths: &[usize],
        col_count: usize,
        cell_style: Style,
    ) -> Line<'static> {
        let outer_border = self.theme.table_border;
        let inner_border = match cell_style.bg {
            Some(bg) => self.theme.table_border.bg(bg),
            None => self.theme.table_border,
        };
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(col_count * 2 + 1);
        spans.push(Span::styled("│", outer_border));
        for i in 0..col_count {
            let width = widths.get(i).copied().unwrap_or(MIN_COL_WIDTH);
            spans.push(Span::styled("\u{00A0}".repeat(width + 2), cell_style));
            let is_last = i + 1 == col_count;
            spans.push(Span::styled(
                "│",
                if is_last { outer_border } else { inner_border },
            ));
        }
        Line::from(spans)
    }

    /// Render one logical table row.  All cells align onto the same number of rendered
    /// lines, shorter ones blank-padded, so the `│` borders stay vertically aligned.
    ///
    /// Wrap is inline-aware: cells flatten to per-char `(char, style)` pairs and re-group
    /// into spans per sub-line, so bold / italic / code survive a line break.
    fn render_table_row(
        &self,
        cells: &[Vec<Inline>],
        widths: &[usize],
        col_count: usize,
        default_style: Style,
        out: &mut Vec<Line<'static>>,
    ) {
        let outer_border = self.theme.table_border;
        let inner_border = match default_style.bg {
            Some(bg) => self.theme.table_border.bg(bg),
            None => self.theme.table_border,
        };

        // `cell_rows[c]` is that cell's wrapped rows; always at least one.
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
            spans.push(Span::styled("│", outer_border));
            // Indexes both `widths` and `cell_rows`, so `enumerate()` doesn't help.
            #[allow(clippy::needless_range_loop)]
            for i in 0..col_count {
                let width = widths.get(i).copied().unwrap_or(MIN_COL_WIDTH);
                let row: &[StyledChar] = cell_rows[i].get(sub).map(|v| v.as_slice()).unwrap_or(&[]);
                let row_w = styled_cells(row);
                // Overflow truncates with `…` — rare: only a single grapheme cluster wider
                // than a pinned column.  Plain text there, to avoid painting a partial
                // styled run.
                spans.push(Span::styled(" ", default_style));
                if row_w <= width {
                    extend_with_styled_chars(&mut spans, row);
                    let pad = width.saturating_sub(row_w);
                    spans.push(Span::styled(format!("{} ", " ".repeat(pad)), default_style));
                } else {
                    let plain: String = row.iter().map(|c| c.ch).collect();
                    let truncated = truncate_to_width(&plain, width.saturating_sub(1));
                    // A wide glyph dropped at the limit leaves a cell to fill.
                    let pad = width.saturating_sub(str_cells(&truncated) + 1);
                    spans.push(Span::styled(
                        format!("{truncated}…{} ", " ".repeat(pad)),
                        default_style,
                    ));
                }
                let is_last = i + 1 == col_count;
                spans.push(Span::styled(
                    "│",
                    if is_last { outer_border } else { inner_border },
                ));
            }
            out.push(Line::from(spans));
        }
    }

    /// Flatten a cell into per-char styled pairs, each remembering its source span's
    /// style, so the inline-aware wrap preserves formatting across line breaks.
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
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Inline {
        Inline::Text(s.to_owned())
    }

    #[test]
    fn cell_min_width_prose_uses_longest_word() {
        let cell = vec![text("a couple of ordinary words")];
        assert_eq!(cell_min_width(&cell), "ordinary".chars().count());
    }

    #[test]
    fn cell_min_width_long_code_span_is_breakable() {
        let cell = vec![Inline::Code("really_long_function_name()".to_owned())];
        assert_eq!(cell_min_width(&cell), BREAKABLE_MIN_WIDTH);
    }

    #[test]
    fn cell_min_width_short_code_span_counts_content_only() {
        let cell = vec![Inline::Code("ok".to_owned())];
        assert_eq!(cell_min_width(&cell), 2);
    }

    #[test]
    fn cell_min_width_prose_word_beside_code_token_wins() {
        let cell = vec![
            text("unbreakableprose "),
            Inline::Code("very_long_identifier_here".to_owned()),
        ];
        assert_eq!(cell_min_width(&cell), "unbreakableprose".chars().count());
    }

    #[test]
    fn cell_min_width_mixed_token_keeps_prose_run_intact() {
        // One token; the floor must not drop below the prose run or a split shreds it.
        let cell = vec![text("unbreakableprose"), Inline::Code("x".to_owned())];
        assert_eq!(cell_min_width(&cell), "unbreakableprose".chars().count());
    }

    #[test]
    fn cell_min_width_link_text_is_breakable() {
        let cell = vec![Inline::Link {
            text: vec![text("a-very-long-link-label-indeed")],
            url: "https://example.com".to_owned(),
            title: None,
        }];
        assert_eq!(cell_min_width(&cell), BREAKABLE_MIN_WIDTH);
    }

    #[test]
    fn cell_min_width_empty_link_text_measures_url_fallback() {
        let cell = vec![Inline::Link {
            text: vec![],
            url: "https://example.com/some/long/path".to_owned(),
            title: None,
        }];
        assert_eq!(cell_min_width(&cell), BREAKABLE_MIN_WIDTH);
    }

    /// CJK breaks between glyphs, so a run floors at its widest glyph, not its full width.
    #[test]
    fn cell_min_width_cjk_run_floors_at_one_glyph() {
        assert_eq!(cell_min_width(&[text("日本語日本語")]), 2);
    }

    #[test]
    fn cell_min_width_prose_glued_to_cjk_keeps_its_word() {
        assert_eq!(cell_min_width(&[text("日本語abc")]), 3);
    }

    #[test]
    fn cell_min_width_cjk_code_span_floors_at_one_glyph() {
        assert_eq!(
            cell_min_width(&[Inline::Code("日本語日本語".to_owned())]),
            2
        );
    }

    /// A cluster is never split, so a ZWJ family (six painted cells) is its own floor.
    #[test]
    fn cell_min_width_zwj_family_is_one_unbreakable_glyph() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(cell_min_width(&[text(&format!("ab {family}"))]), 6);
    }

    #[test]
    fn cell_min_width_empty_cell_is_zero() {
        assert_eq!(cell_min_width(&[]), 0);
    }
}
