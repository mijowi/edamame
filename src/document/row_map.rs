//! Rendered row ↔ source line, answered from the [`RowOrigin`]s the renderer recorded.
//!
//! Works in **logical rows** — entries of `ParsedDoc::lines`, before wrap — and in source lines
//! relative to a block's first line (the line holding its original range's start).  Wrapping and
//! the reflow reveal stay with `line_render` and `EffectiveRows`.  `block` is always an index in
//! the source map's space (blank-line virtual blocks included), and a row is an offset from the
//! block's first rendered row.  The rules are in `docs/dev/editing-model.md`.

use std::ops::Range;

use crate::document::ParsedDoc;
use crate::markdown::ast::to_u32;
use crate::markdown::RowOrigin;

/// `block`'s own rows' origins, and how many rows a `$$…$$` reveal's math-preview band holds
/// above them.  The band is editor state (the cursor's reveal), not the renderer's, so the
/// origins don't know it: a band row shows no source line, and the rows below it are the
/// origins' rows shifted down.
fn own_origins(parsed: &ParsedDoc, block: usize) -> (&[RowOrigin], usize) {
    let own = parsed.block_own_line_count(block);
    let first = parsed.source_map.rendered_lines_for_block(block).start;
    let origins = parsed
        .row_origins()
        .get(first..first.saturating_add(own))
        .unwrap_or(&[]);
    (origins, parsed.latex_source_offset(block))
}

/// The row of `block` that shows block-relative source `line`: the first row whose lines reach
/// it.  A line that renders no row of its own (an interior blank, a setext underline below a
/// one-row heading, a bare `-`) shares the next line's row; a line past every row clamps to the
/// last row that shows a line, so never onto trailing chrome no line owns (a table's bottom
/// border, an unclosed fence's placeholder).  A block with no rows answers 0.
pub fn row_for_line(parsed: &ParsedDoc, block: usize, line: usize) -> usize {
    let (origins, band) = own_origins(parsed, block);
    if origins.is_empty() {
        return 0;
    }
    let line = to_u32(line);
    let last = origins.len() - 1;
    let row = origins
        .iter()
        .position(|o| o.lines.as_ref().is_some_and(|l| l.end > line))
        .or_else(|| origins.iter().rposition(|o| o.lines.is_some()))
        .unwrap_or(last);
    (row + band).min(last)
}

/// The source lines row `row` of `block` shows; `None` for a row no line owns (a table's top
/// border, an unclosed fence's placeholder) and for a math-preview band row.
pub fn lines_of_row(parsed: &ParsedDoc, block: usize, row: usize) -> Option<Range<u32>> {
    let (origins, band) = own_origins(parsed, block);
    origins.get(row.checked_sub(band)?)?.lines.clone()
}

/// The block-relative source line row `row` of `block` belongs to: the first line it shows, or,
/// for a row no line owns, the nearest owned line above it (0 when there is none).
pub fn line_for_row(parsed: &ParsedDoc, block: usize, row: usize) -> usize {
    let (origins, band) = own_origins(parsed, block);
    let Some(row) = row.checked_sub(band) else {
        return 0;
    };
    let row = row.min(origins.len().saturating_sub(1));
    origins
        .get(..=row)
        .unwrap_or(&[])
        .iter()
        .rev()
        .find_map(RowOrigin::first_line)
        .map_or(0, |l| l as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn doc(src: &str) -> ParsedDoc {
        ParsedDoc::build(src, theme(), true, 4)
    }

    /// Rows and lines of the block holding `byte`.
    fn block_at(doc: &ParsedDoc, byte: usize) -> usize {
        doc.source_map.block_for_byte(byte).unwrap()
    }

    #[test]
    fn a_line_with_no_row_shares_the_next_lines_row() {
        // An interior blank between an item's paragraphs renders no row.
        let d = doc("- a\n\n  b\n");
        let b = block_at(&d, 0);
        assert_eq!(
            (0..3).map(|l| row_for_line(&d, b, l)).collect::<Vec<_>>(),
            [0, 1, 1]
        );
        assert_eq!(line_for_row(&d, b, 1), 2);
    }

    #[test]
    fn a_marker_on_a_row_of_its_own_keeps_the_rows_below_on_their_lines() {
        // Rows: `•`, ` bash ` label, body, closing fence, `• next`.
        let d = doc("- ```bash\n  code\n  ```\n- next\n");
        let b = block_at(&d, 0);
        assert_eq!(
            (0..4).map(|l| row_for_line(&d, b, l)).collect::<Vec<_>>(),
            [0, 2, 3, 4]
        );
        assert_eq!(
            (0..5).map(|r| line_for_row(&d, b, r)).collect::<Vec<_>>(),
            [0, 0, 1, 2, 3]
        );
    }

    #[test]
    fn a_row_no_line_owns_belongs_to_the_line_above() {
        // An unclosed fence's closing placeholder.
        let d = doc("- a\n  ```\n  x\n- b\n");
        let b = block_at(&d, 0);
        assert_eq!(lines_of_row(&d, b, 3), None);
        assert_eq!(line_for_row(&d, b, 3), 2);
        assert_eq!(row_for_line(&d, b, 3), 4);
    }

    /// A line past every row (a cursor on the blank line a block's range absorbs) lands on the
    /// last row that shows a line, not on a trailing placeholder.
    #[test]
    fn a_line_past_every_row_skips_trailing_chrome() {
        let d = doc("- a\n  ```\n  x\n");
        let b = block_at(&d, 0);
        let rows = d.block_own_line_count(b);
        assert_eq!(
            lines_of_row(&d, b, rows - 1),
            None,
            "the placeholder closes the block"
        );
        assert_eq!(row_for_line(&d, b, 9), rows - 2);
    }

    #[test]
    fn a_table_line_lands_on_its_first_content_row() {
        // Rows: top border, header, heavy rule, data row, bottom border.
        let d = doc("| a | b |\n|---|---|\n| 1 | 2 |\n");
        let b = block_at(&d, 0);
        assert_eq!(
            (0..3).map(|l| row_for_line(&d, b, l)).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(line_for_row(&d, b, 0), 0);
        assert_eq!(line_for_row(&d, b, 4), 2);
    }

    /// A row showing several lines (a multi-line heading's text) is the row of each of them.
    #[test]
    fn every_line_of_a_multi_line_row_lands_on_it() {
        // Rows: the heading's text (lines 0–1), its rule (line 2).
        let d = doc("Title\nmore\n=====\n");
        let b = block_at(&d, 0);
        assert_eq!(lines_of_row(&d, b, 0), Some(0..2));
        assert_eq!(
            (0..3).map(|l| row_for_line(&d, b, l)).collect::<Vec<_>>(),
            [0, 0, 1]
        );
        assert_eq!(line_for_row(&d, b, 1), 2);
    }

    /// The math-preview band sits above a revealed formula's source rows: its rows show no
    /// line, and every line moves down past it.
    #[test]
    fn a_math_preview_band_shifts_the_source_rows_down() {
        let mut d = doc("$$\nx\n$$\n");
        let b = block_at(&d, 0);
        d.math_source_offset = Some((b, 1));
        assert_eq!(lines_of_row(&d, b, 0), None);
        assert_eq!(
            line_for_row(&d, b, 0),
            0,
            "a band row resolves to the first line"
        );
        assert_eq!(
            (0..3).map(|l| row_for_line(&d, b, l)).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            (1..4).map(|r| line_for_row(&d, b, r)).collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }

    #[test]
    fn an_image_pins_every_reserved_row_to_its_line() {
        let d = doc("![alt](missing.png)\n");
        let b = block_at(&d, 0);
        assert_eq!(d.block_own_line_count(b), 4);
        assert!((0..4).all(|r| line_for_row(&d, b, r) == 0));
        assert_eq!(row_for_line(&d, b, 0), 0);
    }
}
