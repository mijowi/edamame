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
//! # Width calculation strategy — min-max proportional
//!
//! Mirrors the algorithm browsers use for `table-layout: auto`, which is
//! also what `rich` and `tabulate` converge on:
//!
//! 1. **Per-column metrics**: `min = longest word` (cells can never wrap
//!    below this without breaking a word), `max = longest cell`.
//!
//! 2. **Fits naturally**: when the sum of every column's `max` (plus
//!    borders) fits the viewport budget, use the `max` widths as-is.
//!
//! 3. **Below max but above min**: distribute the remaining slack
//!    (`viewport - borders - sum(min)`) across unpinned columns weighted
//!    by `(max - min)`.  Wide-prose columns absorb most of the slack;
//!    short / numeric columns stay at their `max` since their `min == max`.
//!
//! 4. **Below sum(min)**: every column drops to its `min` and the table
//!    overflows the viewport horizontally — never break a *prose* word to
//!    fit.  Code spans and links are the exception: the renderer reports a
//!    reduced `min` for cells containing them (see
//!    `renderer::table::cell_min_width`), so those cells hard-split across
//!    rows instead of forcing the column wide.
//!
//! 5. **User overrides**: any column with a `Some(w)` entry in
//!    `user_widths` is pinned to `w` (clamped to `MIN_COL_WIDTH`); pinned
//!    columns are excluded from the proportional distribution so the
//!    user's drag-set widths survive viewport pressure.
//!
//! # Cell wrapping
//!
//! Cells whose natural width exceeds their allocated column width are
//! word-wrapped onto multiple visual rows.  The row height is the maximum
//! wrap count across all cells in that row.
//!
//! # Status
//!
//! This module is fully implemented and unit-tested.  Some items below may be
//! intentionally unreferenced by production code, hence the module-level
//! `allow(dead_code)`.

use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

/// Minimum column width in character cells.  Narrower than this leaves no
/// room for `...` truncation indicators.
pub const MIN_COL_WIDTH: usize = 3;

/// Overhead per column for the leading `│` separator and one space of
/// padding on each side of the cell content: `│ content ` — 3 cells.  The
/// trailing `│` at row end adds one more, accounted for separately.
pub const PER_COL_OVERHEAD: usize = 3;
pub const ROW_END_OVERHEAD: usize = 1;

/// Compute column widths for a table using min-max proportional distribution.
///
/// Inputs:
/// - `cell_max_widths[row][col]` — the longest cell in that position
///   (the column's `max`).
/// - `cell_min_widths[row][col]` — the longest single *word* in that cell
///   (the column's `min`; never wraps below this without breaking a word).
///   When the cell text contains no spaces `min == max` for that cell.
/// - `viewport_width` is the total character-cell budget including borders
///   and padding.  Pass `usize::MAX` to disable proportional distribution
///   and always return `max` widths.
/// - `user_widths`, if `Some`, pins specific columns: `Some(w)` per entry
///   sets that column's width (clamped to `MIN_COL_WIDTH`), `None` lets the
///   column participate in the proportional distribution.  Length must
///   match `col_count`.
///
/// Returns a `Vec<usize>` of length `col_count`.  When the viewport can
/// fit every column at `max`, returns `max`s; when it can fit at least the
/// `min`s, distributes slack weighted by `(max - min)`; otherwise returns
/// `min`s and lets the caller decide whether to truncate or accept overflow.
pub fn compute_widths(
    cell_max_widths: &[Vec<usize>],
    cell_min_widths: &[Vec<usize>],
    col_count: usize,
    viewport_width: usize,
    user_widths: Option<&[Option<usize>]>,
) -> Vec<usize> {
    if col_count == 0 {
        return Vec::new();
    }

    // Per-column max (longest cell) and min (longest word), each clamped to
    // MIN_COL_WIDTH so a single-cell column never collapses below the room
    // needed for a `...` ellipsis indicator.
    let mut col_max = vec![MIN_COL_WIDTH; col_count];
    let mut col_min = vec![MIN_COL_WIDTH; col_count];
    for row in cell_max_widths {
        for (i, w) in row.iter().take(col_count).enumerate() {
            col_max[i] = col_max[i].max(*w);
        }
    }
    for row in cell_min_widths {
        for (i, w) in row.iter().take(col_count).enumerate() {
            col_min[i] = col_min[i].max(*w);
        }
    }
    // Min can never exceed max (e.g. a long single word forces both equal).
    for i in 0..col_count {
        if col_min[i] > col_max[i] {
            col_max[i] = col_min[i];
        }
    }

    // Apply user overrides as pinned widths.
    let mut widths = col_max.clone();
    let mut pinned = vec![false; col_count];
    if let Some(uw) = user_widths {
        for (i, w) in uw.iter().take(col_count).enumerate() {
            if let Some(val) = w {
                widths[i] = (*val).max(MIN_COL_WIDTH);
                pinned[i] = true;
            }
        }
    }

    let border_budget = PER_COL_OVERHEAD * col_count + ROW_END_OVERHEAD;
    let pinned_total: usize = widths
        .iter()
        .enumerate()
        .filter(|(i, _)| pinned[*i])
        .map(|(_, w)| *w)
        .sum();

    // Remaining cells available for unpinned columns.  When viewport_width
    // is `usize::MAX` (callers that disable proportional distribution),
    // this is also `usize::MAX` and the natural-fit branch always wins.
    let available = viewport_width
        .saturating_sub(border_budget)
        .saturating_sub(pinned_total);

    let unpinned: Vec<usize> = (0..col_count).filter(|i| !pinned[*i]).collect();
    let unpinned_max_total: usize = unpinned.iter().map(|i| col_max[*i]).sum();
    let unpinned_min_total: usize = unpinned.iter().map(|i| col_min[*i]).sum();

    if unpinned_max_total <= available {
        // Every unpinned column fits at `max` — no compression needed.
        for &i in &unpinned {
            widths[i] = col_max[i];
        }
    } else if unpinned_min_total <= available {
        // Slack distribution: assign `min` to every column, then divvy up
        // the leftover space weighted by each column's prose flexibility
        // (`max - min`).  Columns whose `min == max` get nothing — they
        // already render fully at their floor.
        let slack = available - unpinned_min_total;
        let total_weight: usize = unpinned.iter().map(|i| col_max[*i] - col_min[*i]).sum();
        if total_weight == 0 {
            // No flexibility anywhere (every cell is one long word).  Use
            // mins as-is and accept that this fits exactly.
            for &i in &unpinned {
                widths[i] = col_min[i];
            }
        } else {
            // Integer-weighted division with remainder distribution: assign
            // floor(slack * weight / total_weight) to each column and hand
            // out the leftover cells one at a time to the columns with the
            // largest fractional residuals so we use every available cell.
            let mut residuals: Vec<(usize, usize)> = Vec::with_capacity(unpinned.len());
            let mut assigned = 0usize;
            for &i in &unpinned {
                let weight = col_max[i] - col_min[i];
                let numer = slack * weight;
                let extra = numer / total_weight;
                let remainder = numer % total_weight;
                widths[i] = col_min[i] + extra;
                assigned += extra;
                residuals.push((i, remainder));
            }
            // Hand out the remaining cells to the columns whose integer
            // truncation lost the most.  Sort by residual (descending),
            // breaking ties on column index so the result is deterministic.
            residuals.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let mut leftover = slack.saturating_sub(assigned);
            for (i, _) in residuals {
                if leftover == 0 {
                    break;
                }
                if widths[i] < col_max[i] {
                    widths[i] += 1;
                    leftover -= 1;
                }
            }
        }
    } else {
        // Even the mins don't fit — assign `min` to every column.  The
        // caller may truncate or accept horizontal overflow.
        for &i in &unpinned {
            widths[i] = col_min[i];
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

/// Wrap `text` like [`wrap_cell`], but also report the char index in
/// `text` where each output row begins.  Used by the wrapped-cell
/// editing path in `RenderedView` to map a cursor's char offset
/// inside the cell back to a (sub-line, column) coordinate.
///
/// Word-wrap drops the whitespace that sits exactly at a break point
/// (the space between the last word of row N and the first word of
/// row N+1 isn't drawn on either row).  Continuation rows therefore
/// start at the first non-whitespace char *after* the previous row's
/// last char.  A cursor that lands on a dropped whitespace char maps
/// to the start of the next row, since that's where the next visible
/// character actually shows up.
pub fn wrap_cell_with_indices(text: &str, width: usize) -> Vec<(usize, String)> {
    let rows = wrap_cell(text, width);
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<(usize, String)> = Vec::with_capacity(rows.len());
    let mut idx = 0;
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            // Continuation rows start after any whitespace dropped at
            // the wrap point.
            while idx < chars.len() && chars[idx].is_whitespace() {
                idx += 1;
            }
        }
        let row_start = idx;
        idx += row.chars().count();
        out.push((row_start, row));
    }
    out
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
/// width.  Prefers to break just after a punctuation character
/// ([`is_break_after`]) in the trailing half of each row, so identifiers,
/// paths, and URLs split at `_` / `.` / `/` / … instead of mid-word.
/// Never returns an empty `Vec`.
fn hard_split(word: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut cur: Vec<char> = Vec::new();
    let mut cur_w = 0usize;
    for ch in word.chars() {
        let cw = UnicodeWidthStr::width(ch.to_string().as_str());
        if cur_w + cw > width && !cur.is_empty() {
            let cut = preferred_cut(cur.len(), |i| cur[i]);
            rows.push(cur[..cut].iter().collect());
            cur.drain(..cut);
            cur_w = cur
                .iter()
                .map(|c| UnicodeWidthStr::width(c.to_string().as_str()))
                .sum();
        }
        cur.push(ch);
        cur_w += cw;
    }
    if !cur.is_empty() {
        rows.push(cur.iter().collect());
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// Characters a hard-split prefers to break *after*.  Tuned for the token
/// shapes that actually force narrow tables — code identifiers
/// (`snake_case`, `kebab-case`, `method().chains`), file paths, and URLs.
pub fn is_break_after(ch: char) -> bool {
    matches!(
        ch,
        '_' | '-'
            | '.'
            | ','
            | ';'
            | ':'
            | '/'
            | '\\'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '='
            | '&'
            | '?'
            | '#'
            | '@'
    )
}

/// Pick the cut index for a hard-split chunk of `len` elements whose chars
/// are exposed via `char_at`.  Scans the trailing half of the chunk for the
/// last break-friendly character ([`is_break_after`]) and cuts just after
/// it; falls back to a full-width cut (`len`) when none exists.  Limiting
/// the scan to the trailing half guarantees every emitted row keeps at
/// least half its width, so a punctuation-dense token can't degenerate
/// into confetti rows.  Shared by [`hard_split`] and the styled-char
/// counterpart in `renderer::util`.
pub fn preferred_cut(len: usize, char_at: impl Fn(usize) -> char) -> usize {
    let lookback = len / 2;
    for back in 0..lookback {
        let i = len - 1 - back;
        if is_break_after(char_at(i)) {
            return i + 1;
        }
    }
    len
}

// ── Column-width comment parsing ────────────────────────────────────────────

/// Parse a `<!-- tui-columns: [20, _, 30] -->` comment out of `text`.
///
/// Returns a vector whose entries are `Some(w)` for each pinned column and
/// `None` for each `_` (auto-size) placeholder.  Returns `None` if no
/// comment exists, the body doesn't match the expected format, or every
/// entry is `_` (in which case the comment carries no information and the
/// caller should treat it as absent).
pub fn parse_column_widths_comment(text: &str) -> Option<Vec<Option<usize>>> {
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
        if s == "_" {
            widths.push(None);
        } else {
            widths.push(Some(s.parse::<usize>().ok()?));
        }
    }
    if widths.is_empty() || widths.iter().all(Option::is_none) {
        None
    } else {
        Some(widths)
    }
}

/// Emit a `<!-- tui-columns: [20, _, 30] -->` comment for persistence.  A
/// `None` entry becomes `_`, signalling an auto-sized column.
pub fn format_column_widths_comment(widths: &[Option<usize>]) -> String {
    let body: Vec<String> = widths
        .iter()
        .map(|w| match w {
            Some(v) => v.to_string(),
            None => "_".to_owned(),
        })
        .collect();
    format!("<!-- tui-columns: [{}] -->", body.join(", "))
}

// ── Pipe position / cell-range helpers ──────────────────────────────────────
//
// These were originally private to `src/ui/rendered_view.rs`; they were lifted
// here so `TableView`'s mouse hit-testing and `RenderedView`'s cell-
// scoped raw reveal share one implementation.  They operate purely on the
// raw table row text and the rendered `Line` produced by the markdown
// renderer — no editor-state coupling.

/// Char positions of unescaped `|` characters in a raw table row.  Preceding
/// `\` escapes the pipe per GFM rules; `\\|` is a literal backslash followed
/// by an unescaped pipe.
pub fn raw_pipe_positions(row: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut escaped = false;
    for (i, ch) in row.chars().enumerate() {
        if ch == '|' && !escaped {
            positions.push(i);
            escaped = false;
        } else if ch == '\\' {
            escaped = !escaped;
        } else {
            escaped = false;
        }
    }
    positions
}

/// Char positions of `│` box-drawing pipe characters in a rendered line.
pub fn rendered_pipe_positions(line: &Line<'_>) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut col = 0usize;
    for span in &line.spans {
        for ch in span.content.chars() {
            if ch == '│' {
                positions.push(col);
            }
            col += 1;
        }
    }
    positions
}

/// For a table row, map a raw char column in `raw_row` to the matching
/// rendered column in `rendered_line`, using both pipe-position sequences.
/// Returns `None` when pipe counts don't match (e.g. alignment row, border).
pub fn table_raw_col_to_rendered_col(
    raw_row: &str,
    rendered_line: &Line<'_>,
    raw_col: usize,
) -> Option<usize> {
    let raw_pipes = raw_pipe_positions(raw_row);
    let rendered_pipes = rendered_pipe_positions(rendered_line);
    if raw_pipes.len() < 2 || rendered_pipes.len() != raw_pipes.len() {
        return None;
    }
    let col_count = raw_pipes.len() - 1;

    // Which raw cell does `raw_col` fall in?  Cell `i` spans
    // (raw_pipes[i] + 1) .. raw_pipes[i + 1].
    let cell_idx = (0..col_count)
        .find(|&i| raw_col < raw_pipes[i + 1])
        .unwrap_or(col_count - 1);
    let raw_cell_start = raw_pipes[cell_idx] + 1;
    let rend_cell_start = rendered_pipes[cell_idx] + 1;
    let rend_cell_end = rendered_pipes[cell_idx + 1];

    // Align on the one-space leading padding the renderer always emits.
    let raw_offset_in_cell = raw_col.saturating_sub(raw_cell_start);
    let raw_cell_text: String = raw_row
        .chars()
        .skip(raw_cell_start)
        .take(raw_pipes[cell_idx + 1].saturating_sub(raw_cell_start))
        .collect();
    let raw_leading = raw_cell_text
        .chars()
        .take_while(|c| c.is_whitespace())
        .count();
    // Rendered cell = `<space><content><pad_spaces><space>`.  Map a click
    // inside the raw content region to 1 + (offset past raw leading).
    let rend_offset_in_cell = if raw_offset_in_cell <= raw_leading {
        0
    } else {
        1 + (raw_offset_in_cell - raw_leading)
    };
    let rend_cell_width = rend_cell_end.saturating_sub(rend_cell_start);
    Some(rend_cell_start + rend_offset_in_cell.min(rend_cell_width))
}

/// Metadata for overlaying a raw cell on top of a rendered table row.
///
/// The `rendered_start..rendered_end` char range spans the cell's content area
/// between the two surrounding `│` box-drawing characters (exclusive of both
/// pipes).  `raw_text` is padded/clamped to that width when painted, so the
/// surrounding borders and neighbouring cells remain intact.
pub struct CellOverlay {
    pub rendered_start: usize,
    pub rendered_end: usize,
    pub raw_text: String,
    /// Cursor offset within `raw_text` in chars; `None` if the cursor sits
    /// outside the cell's overlay area (fallback path should be taken).
    pub cursor_in_cell: Option<usize>,
    /// Byte offset within the raw row at which this cell's content starts
    /// (the byte immediately after the cell's opening `|`).  Used by the
    /// caller to align an absolute selection byte range onto `raw_text` so
    /// the overlay can repaint selection highlighting over the raw chars.
    pub raw_cell_byte_start: usize,
}

/// Try to compute a cell-scoped overlay for the cursor's active cell.
///
/// Returns `None` when the row doesn't parse as a table row, when the rendered
/// and raw pipe counts disagree (e.g. the cursor row is the alignment row,
/// which renders as a `├─┼─┤` separator), or when the raw cell text is wider
/// than the rendered cell area (in which case the caller falls back to the
/// full row-reveal so the user can still see the content they're editing).
pub fn compute_cell_overlay(
    raw_row: &str,
    rendered_line: &Line<'_>,
    cursor_col: usize,
) -> Option<CellOverlay> {
    let raw_pipes = raw_pipe_positions(raw_row);
    let rendered_pipes = rendered_pipe_positions(rendered_line);
    if raw_pipes.len() < 2 || rendered_pipes.len() != raw_pipes.len() {
        return None;
    }

    // Cell index: the number of raw pipes at or before the cursor, minus one
    // (pipe 0 begins cell 0).  Clamp to [0, col_count-1].
    let col_count = raw_pipes.len() - 1;
    let preceding = raw_pipes.iter().take_while(|&&p| p < cursor_col).count();
    let cell_idx = preceding.saturating_sub(1).min(col_count - 1);

    let raw_cell_start = raw_pipes[cell_idx] + 1;
    let raw_cell_end = raw_pipes[cell_idx + 1];
    let raw_text: String = raw_row
        .chars()
        .skip(raw_cell_start)
        .take(raw_cell_end - raw_cell_start)
        .collect();

    // Byte offset of the cell's content within the raw row — needed so the
    // caller can intersect an absolute-byte selection range with this cell.
    let raw_cell_byte_start = raw_row
        .char_indices()
        .nth(raw_cell_start)
        .map(|(b, _)| b)
        .unwrap_or(raw_row.len());

    let rendered_start = rendered_pipes[cell_idx] + 1;
    let rendered_end = rendered_pipes[cell_idx + 1];
    let rendered_width = rendered_end.saturating_sub(rendered_start);

    if raw_text.chars().count() > rendered_width {
        return None;
    }

    let cursor_offset = cursor_col.saturating_sub(raw_cell_start);
    let cursor_in_cell = if cursor_offset < rendered_width {
        Some(cursor_offset)
    } else if cursor_offset == rendered_width {
        Some(rendered_width.saturating_sub(1))
    } else {
        None
    };

    Some(CellOverlay {
        rendered_start,
        rendered_end,
        raw_text,
        cursor_in_cell,
        raw_cell_byte_start,
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build mins=maxes (single-word cells) so the existing tests
    /// keep exercising the "no-wrap" path before they get a separate
    /// proportional-distribution test below.
    fn maxes_eq_mins(cells: Vec<Vec<usize>>) -> (Vec<Vec<usize>>, Vec<Vec<usize>>) {
        let mins = cells.clone();
        (cells, mins)
    }

    #[test]
    fn compute_widths_uses_natural_when_room_available() {
        let (maxes, mins) = maxes_eq_mins(vec![vec![2, 4, 3], vec![3, 5, 2]]);
        let widths = compute_widths(&maxes, &mins, 3, 80, None);
        assert_eq!(widths, vec![3, 5, 3]); // clamped to MIN where natural was 2
    }

    #[test]
    fn compute_widths_returns_mins_when_max_exceeds_budget_and_no_slack() {
        // Cells all 10 chars; cells are single words so min == max == 10.
        // No slack possible — every column drops to its `min` and the table
        // overflows the viewport horizontally rather than truncate.
        let (maxes, mins) = maxes_eq_mins(vec![vec![10, 10, 10]]);
        let widths = compute_widths(&maxes, &mins, 3, 20, None);
        assert_eq!(widths, vec![10, 10, 10]);
        // All columns are at their floor — never below MIN.
        assert!(widths.iter().all(|w| *w >= MIN_COL_WIDTH));
    }

    #[test]
    fn compute_widths_distributes_slack_proportionally_to_max_minus_min() {
        // Col 0: prose, max=20, min=4 (longest word).  Col 1: short label,
        // max=5, min=5.  Viewport is wider than the mins but tight on max.
        // Col 1 should stay at its `max` (no flexibility); col 0 absorbs
        // all the available slack.
        let maxes = vec![vec![20, 5]];
        let mins = vec![vec![4, 5]];
        // border = 3*2 + 1 = 7.  Pick viewport = 7 + 4 + 5 + 6 = 22:
        //   slack = 22 - 7 - 4 - 5 = 6.
        //   total_weight = (20-4) + (5-5) = 16.
        //   col0 extra = 6*16/16 = 6 → width = 4 + 6 = 10.
        //   col1 extra = 0 → width = 5.
        let widths = compute_widths(&maxes, &mins, 2, 22, None);
        assert_eq!(widths, vec![10, 5]);
    }

    #[test]
    fn compute_widths_caps_each_column_at_max_during_distribution() {
        // Two prose columns; viewport big enough that the proportional
        // formula would overshoot col 0's max.  Width must clamp at max.
        let maxes = vec![vec![8, 8]];
        let mins = vec![vec![3, 3]];
        // border = 3*2 + 1 = 7.  Viewport = 7 + 8 + 8 = 23 → fits at max.
        let widths = compute_widths(&maxes, &mins, 2, 23, None);
        assert_eq!(widths, vec![8, 8]);
    }

    #[test]
    fn compute_widths_respects_user_override() {
        let (maxes, mins) = maxes_eq_mins(vec![vec![1, 1, 1]]);
        let widths = compute_widths(&maxes, &mins, 3, 80, Some(&[Some(15), Some(7), Some(20)]));
        assert_eq!(widths, vec![15, 7, 20]);
    }

    #[test]
    fn compute_widths_clamps_user_override_to_min() {
        let (maxes, mins) = maxes_eq_mins(vec![vec![1, 1]]);
        let widths = compute_widths(&maxes, &mins, 2, 80, Some(&[Some(1), Some(0)])); // both below MIN
        assert_eq!(widths, vec![MIN_COL_WIDTH, MIN_COL_WIDTH]);
    }

    #[test]
    fn compute_widths_mixes_pinned_and_auto_columns() {
        // Col 0: natural 3, user-set to 5.  Col 1: natural 6, auto.
        let (maxes, mins) = maxes_eq_mins(vec![vec![3, 6]]);
        let widths = compute_widths(&maxes, &mins, 2, 80, Some(&[Some(5), None]));
        assert_eq!(widths, vec![5, 6]);
    }

    #[test]
    fn compute_widths_shrink_leaves_pinned_columns_alone() {
        // Tight viewport; pinned col 0 stays at its width, auto col 1
        // distributes from whatever's left.  Col 1 has slack room (max 10,
        // min 4) so it lands somewhere in `[4, 10]`.
        let maxes = vec![vec![10, 10]];
        let mins = vec![vec![10, 4]];
        let widths = compute_widths(&maxes, &mins, 2, 17, Some(&[Some(8), None]));
        assert_eq!(widths[0], 8); // pinned
        assert!(widths[1] >= MIN_COL_WIDTH);
        assert!(widths[1] <= 10);
    }

    #[test]
    fn compute_widths_narrow_prose_column_stays_at_min_with_no_slack() {
        // Single column, prose with max=12, min=3.  Viewport with no slack:
        // border = 3 + 1 = 4; if viewport = 4 + 3 = 7, the column gets
        // exactly its min.
        let maxes = vec![vec![12]];
        let mins = vec![vec![3]];
        let widths = compute_widths(&maxes, &mins, 1, 7, None);
        assert_eq!(widths, vec![3]);
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
    fn wrap_cell_hard_split_prefers_punctuation_break() {
        // Width 10; "really_long_name" would naively cut at char 10
        // ("really_lon"), but the `_` at index 6 sits inside the trailing
        // half, so the cut lands just after it.
        let rows = wrap_cell("really_long_name", 10);
        assert_eq!(rows, vec!["really_", "long_name"]);
    }

    #[test]
    fn wrap_cell_hard_split_falls_back_to_full_width_without_punctuation() {
        let rows = wrap_cell("abcdefghijklmnop", 10);
        assert_eq!(rows, vec!["abcdefghij", "klmnop"]);
    }

    #[test]
    fn preferred_cut_ignores_punctuation_in_leading_half() {
        // `_` at index 1 is outside the trailing-half scan window of a
        // 10-char chunk — full-width cut.
        let chunk: Vec<char> = "a_bcdefghi".chars().collect();
        assert_eq!(preferred_cut(chunk.len(), |i| chunk[i]), chunk.len());
    }

    #[test]
    fn parse_column_widths_comment_roundtrip() {
        let widths = vec![Some(20), Some(15), Some(30)];
        let s = format_column_widths_comment(&widths);
        assert_eq!(s, "<!-- tui-columns: [20, 15, 30] -->");
        let parsed = parse_column_widths_comment(&s).unwrap();
        assert_eq!(parsed, widths);
    }

    #[test]
    fn parse_column_widths_comment_handles_embedded_text() {
        let src = "some doc text\n<!-- tui-columns: [5, 7] -->\nrest of doc";
        let parsed = parse_column_widths_comment(src).unwrap();
        assert_eq!(parsed, vec![Some(5), Some(7)]);
    }

    #[test]
    fn parse_column_widths_comment_returns_none_when_absent() {
        assert!(parse_column_widths_comment("no widths here").is_none());
        assert!(parse_column_widths_comment("<!-- tui-columns: [a, b] -->").is_none());
    }

    #[test]
    fn parse_column_widths_comment_supports_auto_placeholders() {
        // Mixed pinned + auto columns.
        let parsed = parse_column_widths_comment("<!-- tui-columns: [10, _, 30] -->").unwrap();
        assert_eq!(parsed, vec![Some(10), None, Some(30)]);
    }

    #[test]
    fn parse_column_widths_comment_all_auto_returns_none() {
        // `[_, _]` is equivalent to no comment at all.
        assert!(parse_column_widths_comment("<!-- tui-columns: [_, _] -->").is_none());
    }

    #[test]
    fn format_column_widths_comment_emits_underscore_for_auto() {
        let widths = vec![Some(5), None, Some(12)];
        assert_eq!(
            format_column_widths_comment(&widths),
            "<!-- tui-columns: [5, _, 12] -->"
        );
    }

    // ── Pipe / cell helpers ────────────────────────────────────────────────

    use ratatui::text::{Line, Span};

    fn line_with(s: &str) -> Line<'static> {
        Line::from(vec![Span::raw(s.to_owned())])
    }

    #[test]
    fn raw_pipe_positions_basic() {
        assert_eq!(raw_pipe_positions("| a | b |"), vec![0, 4, 8]);
    }

    #[test]
    fn raw_pipe_positions_skips_escaped_pipes() {
        // The `\|` in the middle must NOT count as a separator.
        let row = r"| a \| x | b |";
        let pipes = raw_pipe_positions(row);
        assert_eq!(pipes, vec![0, 9, 13]);
    }

    #[test]
    fn rendered_pipe_positions_counts_box_drawing_pipes() {
        let line = line_with("│ a │ bb │");
        let pipes = rendered_pipe_positions(&line);
        assert_eq!(pipes, vec![0, 4, 9]);
    }

    #[test]
    fn table_raw_col_to_rendered_col_maps_first_cell() {
        // raw row:      | a | b |
        //               0 123 4
        // rendered:     │ a │ b │
        //               0 123 4
        // raw col 2 (on 'a') in the first cell.  The mapping aligns on
        // the leading-space padding, so both sides have a shared leading
        // space: the raw-content char 'a' (raw col 2) maps to rendered
        // col 2 ('a').
        let raw = "| a | b |";
        let rendered = line_with("│ a │ b │");
        assert_eq!(table_raw_col_to_rendered_col(raw, &rendered, 2), Some(1));
    }

    #[test]
    fn table_raw_col_to_rendered_col_returns_none_on_pipe_mismatch() {
        let raw = "| a | b |";
        // Alignment row: renders as `├─┼─┤`, no `│` so zero pipes.
        let rendered = line_with("├───┼───┤");
        assert!(table_raw_col_to_rendered_col(raw, &rendered, 2).is_none());
    }

    #[test]
    fn compute_cell_overlay_none_when_raw_exceeds_rendered_width() {
        let raw = "| supercalifragilistic | b |";
        let rendered = line_with("│ a │ b │");
        assert!(compute_cell_overlay(raw, &rendered, 3).is_none());
    }

    #[test]
    fn compute_cell_overlay_returns_metadata_when_fits() {
        let raw = "| a | b |";
        let rendered = line_with("│ a │ b │");
        let overlay = compute_cell_overlay(raw, &rendered, 2).expect("overlay fits");
        assert_eq!(overlay.raw_text, " a ");
        assert_eq!(overlay.rendered_start, 1);
        assert_eq!(overlay.rendered_end, 4);
    }
}
