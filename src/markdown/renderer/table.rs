//! `Block::Table` rendering: per-cell width metrics → `compute_widths` →
//! per-row inline-aware wrap → bordered output.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::markdown::ast::Inline;
use crate::markdown::renderer::util::{
    extend_with_styled_chars, is_soft_break_space, link_fallback, truncate_to_width,
    wrap_styled_chars, StyledChar,
};
use crate::markdown::renderer::Renderer;
use crate::markdown::table_layout::{self, MIN_COL_WIDTH};

/// Floor contribution of a cell token that contains *breakable* content —
/// inline code or link text.  Those tokens may hard-split across rendered
/// rows (preferring punctuation break points), so they don't pin the
/// column to their full length the way a prose word does.  8 keeps split
/// chunks readable while letting code/link columns shrink well below the
/// token's natural width; `compute_widths`' slack distribution still
/// widens them whenever the viewport has room.
const BREAKABLE_MIN_WIDTH: usize = 8;

/// Per-cell `min` width for `compute_widths`: the longest run of
/// non-whitespace characters that cannot be broken across rendered rows.
///
/// Prose words are unbreakable, matching the layout policy ("never break
/// a prose word to fit").  Tokens containing inline-code or link content
/// are breakable: the wrap stage hard-splits them at character boundaries,
/// so such a token contributes at most [`BREAKABLE_MIN_WIDTH`] — but never
/// less than its longest contiguous *unbreakable* (prose) run, so a prose
/// fragment glued to a code span keeps its word intact.
fn cell_min_width(inlines: &[Inline]) -> usize {
    let mut chars: Vec<(char, bool)> = Vec::new();
    flatten_breakable_chars(inlines, false, &mut chars);

    let mut best = 0usize;
    for token in chars.split(|&(ch, _)| is_soft_break_space(ch)) {
        if token.is_empty() {
            continue;
        }
        let len = token.len();
        let contribution = if token.iter().any(|&(_, breakable)| breakable) {
            let longest_prose_run = token
                .split(|&(_, breakable)| breakable)
                .map(<[(char, bool)]>::len)
                .max()
                .unwrap_or(0);
            longest_prose_run.max(len.min(BREAKABLE_MIN_WIDTH))
        } else {
            len
        };
        best = best.max(contribution);
    }
    best
}

/// Flatten a cell's inline tree to `(char, breakable)` pairs, mirroring
/// `inlines_to_plain`'s traversal.  `Inline::Code` content and link text
/// (or the URL/filename fallback shown for empty bracket text) are marked
/// breakable; everything else inherits `breakable` from its enclosing
/// context (so code nested in a link stays breakable, prose stays not).
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
                    // Empty bracket text renders as the URL / filename
                    // fallback — measure what's actually painted.
                    out.extend(link_fallback(url).chars().map(|c| (c, true)));
                }
            }
            Inline::Image { alt, .. } => out.extend(alt.chars().map(|c| (c, false))),
            Inline::HtmlComment(_) | Inline::FootnoteReference { .. } => {}
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

        // Per-cell `max` (rendered char width) and `min` (longest
        // unbreakable token — see `cell_min_width`).  Headers participate
        // in the column metrics alongside data rows because a long header
        // word should also keep the column from collapsing past its widest
        // bound.
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
            .map(|c| cell_min_width(c))
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
        let header_border: String = std::iter::once("┝".to_string())
            .chain(widths.iter().enumerate().map(|(i, &w)| {
                let corner = if i + 1 < col_count { "┿" } else { "┥" };
                format!("{}{}", "━".repeat(w + 2), corner)
            }))
            .collect();
        out.push(Line::styled(header_border, header_border_style));

        // Data rows, each followed by an inter-row separator except the
        // last.  When `row_striping` is off, the separator is a thin
        // box-drawing rule (`├─┼─┤`).  When striping is on, the rule
        // would clash with the alternating background fill — so we
        // emit a *blank* separator whose background matches the row
        // immediately above it.  Visual effect: each data row appears
        // as a 2-row band of its own color, with no horizontal rule
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

    /// Render one logical table row into `out`.  When any cell needs more
    /// than one wrap line, all cells in the row align onto the same number
    /// of rendered lines (shorter cells emit blank-padded continuation
    /// lines so the surrounding `│` borders stay vertically aligned).
    ///
    /// Wrap is *inline-aware* — each cell's `Vec<Inline>` is
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
        let outer_border = self.theme.table_border;
        let inner_border = match default_style.bg {
            Some(bg) => self.theme.table_border.bg(bg),
            None => self.theme.table_border,
        };

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
            spans.push(Span::styled("│", outer_border));
            // The body indexes both `widths` and `cell_rows` per `i`, so
            // `enumerate()` doesn't simplify it.
            #[allow(clippy::needless_range_loop)]
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
                let is_last = i + 1 == col_count;
                spans.push(Span::styled(
                    "│",
                    if is_last { outer_border } else { inner_border },
                ));
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
        // Rendered form is `ok` — 2 cells, below the breakable floor.
        let cell = vec![Inline::Code("ok".to_owned())];
        assert_eq!(cell_min_width(&cell), 2);
    }

    #[test]
    fn cell_min_width_prose_word_beside_code_token_wins() {
        // The prose word is its own token and longer than the breakable
        // floor — it stays the column's min.
        let cell = vec![
            text("unbreakableprose "),
            Inline::Code("very_long_identifier_here".to_owned()),
        ];
        assert_eq!(cell_min_width(&cell), "unbreakableprose".chars().count());
    }

    #[test]
    fn cell_min_width_mixed_token_keeps_prose_run_intact() {
        // Prose glued to a code span forms one token; the floor must not
        // drop below the prose run so a hard split can't shred the word.
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
        // Empty bracket text renders as the URL fallback — long, so capped
        // at the breakable floor.
        let cell = vec![Inline::Link {
            text: vec![],
            url: "https://example.com/some/long/path".to_owned(),
            title: None,
        }];
        assert_eq!(cell_min_width(&cell), BREAKABLE_MIN_WIDTH);
    }

    #[test]
    fn cell_min_width_empty_cell_is_zero() {
        assert_eq!(cell_min_width(&[]), 0);
    }
}
