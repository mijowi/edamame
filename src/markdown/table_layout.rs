//! Column-width computation and cell text wrapping for GFM tables.
//!
//! This module is responsible for deciding *how* a table's cells fit inside
//! the available viewport — the actual painting of box-drawing characters
//! lives in `renderer::render_table`.  Keeping the layout logic separate
//! makes it easy to unit-test (no ratatui dependency) and lets future
//! features (mouse column-resize, column-width persistence) plug in without
//! disturbing rendering.
//!
//! Responsibilities:
//!   - compute column widths that fit a total viewport budget,
//!   - wrap over-long cell content onto multiple visual rows,
//!   - parse and emit the per-table `<!-- tui-columns: [...] -->` comment
//!     used to persist user-set column widths.
//!
//! Widths are in *terminal columns* (character cells), not bytes.  Cell
//! content width is measured with `unicode-width` to handle CJK / wide
//! characters correctly.
//!
//! # Width calculation strategy
//!
//! 1. **Natural width**: the max rendered width of any cell in a column.
//!    If the sum of natural widths (plus borders/padding) fits within the
//!    budget, use them as-is.
//!
//! 2. **Shrink to fit**: if natural widths exceed the budget, proportionally
//!    shrink the wider columns while respecting a minimum of
//!    `MIN_COL_WIDTH = 3` (enough to render `...`).
//!
//! 3. **User overrides**: if `user_widths` is supplied, those widths are
//!    used verbatim — the caller is responsible for clamping them to a
//!    sensible range.
//!
//! # Cell wrapping
//!
//! Cells whose natural width exceeds their allocated column width are
//! word-wrapped onto multiple visual rows.  The row height is the maximum
//! wrap count across all cells in that row.
//!
//! # Phase 2 status
//!
//! This module is fully implemented and unit-tested in Phase 2, but its
//! consumers (the `TableView` widget, mouse column-resize, and
//! column-width-comment round-tripping) land in Phase 6.  Until then the
//! items below are intentionally unreferenced by production code, hence the
//! module-level `allow(dead_code)`.

#![allow(dead_code)]

use unicode_width::UnicodeWidthStr;

/// Minimum column width in character cells.  Narrower than this leaves no
/// room for `...` truncation indicators.
pub const MIN_COL_WIDTH: usize = 3;

/// Overhead per column for the leading `│` separator and one space of
/// padding on each side of the cell content: `│ content ` — 3 cells.  The
/// trailing `│` at row end adds one more, accounted for separately.
const PER_COL_OVERHEAD: usize = 3;
const ROW_END_OVERHEAD: usize = 1;

/// Compute column widths for a table.
///
/// - `cell_widths[row][col]` is the natural (un-wrapped) rendered width of
///   the cell at that position in character cells.
/// - `viewport_width` is the total character-cell budget for the table row,
///   including borders and padding.  Pass `usize::MAX` to disable shrinking
///   (i.e. always use natural widths).
/// - `user_widths`, if `Some`, forces each column to that width (clamped to
///   `MIN_COL_WIDTH`).  Lengths must match `col_count`.
///
/// Returns a `Vec<usize>` of length `col_count`.
pub fn compute_widths(
    cell_widths: &[Vec<usize>],
    col_count: usize,
    viewport_width: usize,
    user_widths: Option<&[usize]>,
) -> Vec<usize> {
    if col_count == 0 {
        return Vec::new();
    }

    // User override path — trust the caller; only enforce the minimum.
    if let Some(uw) = user_widths {
        let mut out = vec![MIN_COL_WIDTH; col_count];
        for (i, w) in uw.iter().take(col_count).enumerate() {
            out[i] = (*w).max(MIN_COL_WIDTH);
        }
        return out;
    }

    // Natural widths.
    let mut widths = vec![MIN_COL_WIDTH; col_count];
    for row in cell_widths {
        for (i, w) in row.iter().take(col_count).enumerate() {
            widths[i] = widths[i].max(*w);
        }
    }

    // If the table already fits, we're done.
    let border_budget = PER_COL_OVERHEAD * col_count + ROW_END_OVERHEAD;
    let total_natural: usize = widths.iter().sum::<usize>() + border_budget;
    if total_natural <= viewport_width {
        return widths;
    }

    // Shrink: distribute the excess across columns whose width exceeds
    // `MIN_COL_WIDTH`.  Repeatedly remove one cell from the widest column
    // until we fit.  This is O(excess · col_count) but col_count is small
    // and excess is bounded by the viewport width, so it's fine.
    let mut excess = total_natural - viewport_width;
    while excess > 0 {
        // Find the widest column still above MIN.
        let mut best_idx = None;
        let mut best_w = MIN_COL_WIDTH;
        for (i, w) in widths.iter().enumerate() {
            if *w > best_w {
                best_w = *w;
                best_idx = Some(i);
            }
        }
        match best_idx {
            Some(i) => {
                widths[i] -= 1;
                excess -= 1;
            }
            None => break, // every column already at MIN; cannot shrink further
        }
    }
    widths
}

/// Wrap a cell's plain-text content to fit `width` terminal columns.
///
/// Returns one `String` per visual row.  Breaks on spaces where possible;
/// a single word longer than `width` is hard-split at character boundaries.
/// Never returns an empty `Vec` — empty input yields `vec![String::new()]`.
pub fn wrap_cell(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_owned()];
    }
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut rows = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;

    for word in split_soft(text) {
        let w = UnicodeWidthStr::width(word.as_str());
        if current.is_empty() {
            if w <= width {
                current.push_str(&word);
                current_w = w;
            } else {
                // Long word — hard-split across rows.
                for chunk in hard_split(&word, width) {
                    rows.push(chunk);
                }
                current.clear();
                current_w = 0;
            }
        } else {
            // Try to append (with the space already embedded in `word`).
            if current_w + w <= width {
                current.push_str(&word);
                current_w += w;
            } else {
                rows.push(std::mem::take(&mut current));
                let w_trimmed = word.trim_start();
                let w_w = UnicodeWidthStr::width(w_trimmed);
                if w_w <= width {
                    current.push_str(w_trimmed);
                    current_w = w_w;
                } else {
                    for chunk in hard_split(w_trimmed, width) {
                        rows.push(chunk);
                    }
                    current_w = 0;
                }
            }
        }
    }
    if !current.is_empty() {
        rows.push(current);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// Split `text` into tokens, each token being either a single whitespace
/// run merged with the following word, or a single trailing word.  The
/// whitespace is kept attached to the word so `wrap_cell` can re-include
/// spaces exactly as they appeared.
fn split_soft(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_ws = true;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !in_ws && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(ch);
            in_ws = true;
        } else {
            if in_ws && !cur.is_empty() {
                // keep leading whitespace attached to this word
            }
            cur.push(ch);
            in_ws = false;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Hard-split a single word across rows of width `width`, by terminal-cell
/// width.  Never returns an empty `Vec`.
fn hard_split(word: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for ch in word.chars() {
        let cw = UnicodeWidthStr::width(ch.to_string().as_str());
        if cur_w + cw > width && !cur.is_empty() {
            rows.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        cur.push(ch);
        cur_w += cw;
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

// ── Column-width comment parsing ────────────────────────────────────────────

/// Parse a `<!-- tui-columns: [20, 15, 30] -->` comment out of `text`.
///
/// Returns the parsed widths on the first valid occurrence, or `None` if no
/// such comment exists or the body doesn't match the expected format.
pub fn parse_column_widths_comment(text: &str) -> Option<Vec<usize>> {
    let start = text.find("<!-- tui-columns:")?;
    let after = &text[start + "<!-- tui-columns:".len()..];
    let end_rel = after.find("-->")?;
    let body = after[..end_rel].trim();
    let inner = body.strip_prefix('[')?.strip_suffix(']')?;
    let mut widths = Vec::new();
    for part in inner.split(',') {
        let s = part.trim();
        if s.is_empty() {
            continue;
        }
        widths.push(s.parse::<usize>().ok()?);
    }
    if widths.is_empty() {
        None
    } else {
        Some(widths)
    }
}

/// Emit a `<!-- tui-columns: [20, 15, 30] -->` comment for persistence.
pub fn format_column_widths_comment(widths: &[usize]) -> String {
    let body: Vec<String> = widths.iter().map(|w| w.to_string()).collect();
    format!("<!-- tui-columns: [{}] -->", body.join(", "))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_widths_uses_natural_when_room_available() {
        let cells = vec![vec![2, 4, 3], vec![3, 5, 2]];
        let widths = compute_widths(&cells, 3, 80, None);
        assert_eq!(widths, vec![3, 5, 3]); // clamped to MIN where natural was 2
    }

    #[test]
    fn compute_widths_shrinks_to_fit_budget() {
        // cells all 10 wide, 3 cols → natural total = 10*3 + borders = 40
        let cells = vec![vec![10, 10, 10]];
        let widths = compute_widths(&cells, 3, 20, None); // cramped budget
        let border = PER_COL_OVERHEAD * 3 + ROW_END_OVERHEAD;
        assert!(widths.iter().sum::<usize>() + border <= 20);
        // Every column should still be at least MIN wide.
        assert!(widths.iter().all(|w| *w >= MIN_COL_WIDTH));
    }

    #[test]
    fn compute_widths_respects_user_override() {
        let cells = vec![vec![1, 1, 1]];
        let widths = compute_widths(&cells, 3, 80, Some(&[15, 7, 20]));
        assert_eq!(widths, vec![15, 7, 20]);
    }

    #[test]
    fn compute_widths_clamps_user_override_to_min() {
        let cells = vec![vec![1, 1]];
        let widths = compute_widths(&cells, 2, 80, Some(&[1, 0])); // both below MIN
        assert_eq!(widths, vec![MIN_COL_WIDTH, MIN_COL_WIDTH]);
    }

    #[test]
    fn wrap_cell_breaks_on_spaces() {
        let rows = wrap_cell("hello world foo bar", 11);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].trim(), "hello world");
        assert_eq!(rows[1].trim(), "foo bar");
    }

    #[test]
    fn wrap_cell_empty_returns_one_empty_row() {
        let rows = wrap_cell("", 10);
        assert_eq!(rows, vec!["".to_owned()]);
    }

    #[test]
    fn wrap_cell_hard_splits_long_word() {
        let rows = wrap_cell("supercalifragilistic", 6);
        assert!(rows.len() >= 2);
        for r in &rows {
            assert!(UnicodeWidthStr::width(r.as_str()) <= 6);
        }
        let joined: String = rows.join("");
        assert_eq!(joined, "supercalifragilistic");
    }

    #[test]
    fn parse_column_widths_comment_roundtrip() {
        let widths = vec![20, 15, 30];
        let s = format_column_widths_comment(&widths);
        assert_eq!(s, "<!-- tui-columns: [20, 15, 30] -->");
        let parsed = parse_column_widths_comment(&s).unwrap();
        assert_eq!(parsed, widths);
    }

    #[test]
    fn parse_column_widths_comment_handles_embedded_text() {
        let src = "some doc text\n<!-- tui-columns: [5, 7] -->\nrest of doc";
        let parsed = parse_column_widths_comment(src).unwrap();
        assert_eq!(parsed, vec![5, 7]);
    }

    #[test]
    fn parse_column_widths_comment_returns_none_when_absent() {
        assert!(parse_column_widths_comment("no widths here").is_none());
        assert!(parse_column_widths_comment("<!-- tui-columns: [a, b] -->").is_none());
    }
}
