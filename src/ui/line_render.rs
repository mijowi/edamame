use ratatui::{buffer::Buffer as TuiBuf, layout::Rect, style::Style, text::Line};

/// Write a styled `Line` to the TUI buffer, wrapping at `area.width` when
/// `wrap` is true. Returns the number of visual rows consumed (≥ 1).
///
/// Trailing cells in every visual row are filled with the line's base style so
/// styled blocks (e.g. code blocks) extend to the full width of `area`.  The
/// wrap algorithm is word-aware: breaks prefer the last non-alphanumeric
/// character within the row, falling back to a hard break when a single word
/// exceeds the row width.
///
/// `cursor_col_override`: when `Some((col, style))`, the character at visual
/// column `col` on the first output row is rendered with `style` (used to
/// show a cursor indicator during the jitter-suppression delay in hybrid
/// rendered mode).
pub fn render_line(
    line: &Line<'static>,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
) -> u16 {
    render_line_with_cursor(line, area, buf, visual_y, wrap, None)
}

pub fn render_line_with_cursor(
    line: &Line<'static>,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
    cursor_col_override: Option<(usize, Style)>,
) -> u16 {
    if visual_y >= area.height {
        return 0;
    }
    let width = area.width as usize;
    if width == 0 {
        return 1;
    }
    let abs_y = area.y + visual_y;

    // Collect all (char, style) pairs from the spans, resolving line-level style.
    let line_style = line.style;
    let mut chars: Vec<(char, Style)> = Vec::new();
    for span in &line.spans {
        let style = line_style.patch(span.style);
        for ch in span.content.chars() {
            chars.push((ch, style));
        }
    }

    if !wrap {
        // No wrapping: write up to `width` chars on a single row.
        let mut x = area.x;
        for (idx, (ch, style)) in chars.iter().enumerate() {
            if x >= area.x + area.width {
                break;
            }
            let effective_style = cursor_col_override
                .filter(|(col, _)| *col == idx)
                .map(|(_, s)| s)
                .unwrap_or(*style);
            if let Some(cell) = buf.cell_mut((x, abs_y)) {
                cell.set_char(*ch);
                cell.set_style(effective_style);
            }
            x += 1;
        }
        while x < area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, abs_y)) {
                cell.set_style(line_style);
            }
            x += 1;
        }
        return 1;
    }

    // Word-aware wrapping. See module docstring.
    let mut cur_visual = visual_y;
    let mut start = 0;
    let mut char_col_base = 0usize;

    while start < chars.len() {
        if cur_visual >= area.height {
            break;
        }
        let cur_abs_y = area.y + cur_visual;

        let remaining = chars.len() - start;
        let (row_end, next_start) = if remaining <= width {
            (chars.len(), chars.len())
        } else {
            let window = &chars[start..start + width];
            let break_rel = window
                .iter()
                .enumerate()
                .rev()
                .find(|(_, (ch, _))| !ch.is_alphanumeric())
                .map(|(i, _)| i);

            match break_rel {
                Some(bp) => {
                    let end = start + bp + 1;
                    let mut next = end;
                    while next < chars.len() && chars[next].0 == ' ' {
                        next += 1;
                    }
                    (end, next)
                }
                None => {
                    let end = start + width;
                    (end, end)
                }
            }
        };

        let mut x = area.x;
        for (rel_idx, (ch, style)) in chars[start..row_end].iter().enumerate() {
            let abs_col = char_col_base + rel_idx;
            let effective_style = cursor_col_override
                .filter(|(col, _)| *col == abs_col)
                .map(|(_, s)| s)
                .unwrap_or(*style);
            if let Some(cell) = buf.cell_mut((x, cur_abs_y)) {
                cell.set_char(*ch);
                cell.set_style(effective_style);
            }
            x += 1;
        }
        while x < area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, cur_abs_y)) {
                cell.set_style(line_style);
            }
            x += 1;
        }

        char_col_base += next_start - start;
        start = next_start;
        cur_visual += 1;
    }

    (cur_visual - visual_y).max(1)
}

/// Compute the list of visual rows produced by wrapping `chars` at `width`.
///
/// Returns a list of `(start, end, next_start)` tuples, where:
/// - `chars[start..end]` is the content placed on that visual row
/// - `next_start` is the index at which the next visual row begins (may be
///   `> end` when trailing spaces are consumed at the wrap point)
///
/// The algorithm mirrors `render_line` exactly so that visual-line navigation
/// lands the cursor on the same visual row as the renderer draws.
pub fn visual_rows_of_chars(chars: &[(char, Style)], width: usize) -> Vec<(usize, usize, usize)> {
    let mut rows = Vec::new();
    if width == 0 {
        rows.push((0, chars.len(), chars.len()));
        return rows;
    }

    let mut start = 0;
    loop {
        if start >= chars.len() {
            if rows.is_empty() {
                rows.push((0, 0, 0));
            }
            break;
        }
        let remaining = chars.len() - start;
        let (row_end, next_start) = if remaining <= width {
            (chars.len(), chars.len())
        } else {
            let window = &chars[start..start + width];
            let break_rel = window
                .iter()
                .enumerate()
                .rev()
                .find(|(_, (ch, _))| !ch.is_alphanumeric())
                .map(|(i, _)| i);

            match break_rel {
                Some(bp) => {
                    let end = start + bp + 1;
                    let mut next = end;
                    while next < chars.len() && chars[next].0 == ' ' {
                        next += 1;
                    }
                    (end, next)
                }
                None => (start + width, start + width),
            }
        };

        rows.push((start, row_end, next_start));
        if next_start >= chars.len() || next_start == start {
            break;
        }
        start = next_start;
    }

    if rows.is_empty() {
        rows.push((0, 0, 0));
    }

    rows
}

/// Compute the list of visual rows produced by wrapping a plain string `text`
/// at `width`.  Convenience wrapper around `visual_rows_of_chars` used by the
/// cursor/navigation code which doesn't care about per-char styling.
pub fn visual_rows_of_str(text: &str, width: usize) -> Vec<(usize, usize, usize)> {
    let chars: Vec<(char, Style)> = text.chars().map(|c| (c, Style::default())).collect();
    visual_rows_of_chars(&chars, width)
}

/// Number of visual rows a styled `Line` occupies when wrapped at `width`.
///
/// Mirrors the wrap algorithm used by `render_line`, so scroll-bound
/// calculations can match what the viewport actually draws.  Empty lines
/// always consume a single row.
pub fn visual_rows_for_line(line: &Line<'_>, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let chars: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
        .collect();
    if chars.is_empty() {
        return 1;
    }
    visual_rows_of_chars(&chars, width).len().max(1)
}

/// Given the visual-row layout of a line and a raw char column, return
/// `(sub_line_idx, visual_col)` — which visual row the char is on, and its
/// visual column within that row (0-based).
///
/// Cursor positions at end-of-line or within a wrap-point skip zone are mapped
/// to the nearest visible row at its end column.
pub fn sub_line_of_col(rows: &[(usize, usize, usize)], raw_col: usize) -> (usize, usize) {
    for (i, &(s, e, n)) in rows.iter().enumerate() {
        if raw_col < n {
            let row_width = e - s;
            let visual_col = raw_col.saturating_sub(s).min(row_width);
            return (i, visual_col);
        }
    }
    if let Some(&(s, e, _)) = rows.last() {
        let last_idx = rows.len() - 1;
        let row_width = e - s;
        let visual_col = raw_col.saturating_sub(s).min(row_width);
        return (last_idx, visual_col);
    }
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_rows_short_line() {
        let rows = visual_rows_of_str("hello", 10);
        assert_eq!(rows, vec![(0, 5, 5)]);
    }

    #[test]
    fn visual_rows_wraps_at_space() {
        // "hello world" wraps at space (col 5).
        let rows = visual_rows_of_str("hello world foo", 10);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (0, 6, 6)); // "hello " on row 0
        assert_eq!(rows[1], (6, 15, 15)); // "world foo" on row 1
    }

    #[test]
    fn visual_rows_force_break_on_long_word() {
        // Single long word exceeds width — force break.
        let rows = visual_rows_of_str("abcdefghijklmnop", 8);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (0, 8, 8));
        assert_eq!(rows[1], (8, 16, 16));
    }

    #[test]
    fn visual_rows_empty_string() {
        let rows = visual_rows_of_str("", 10);
        assert_eq!(rows, vec![(0, 0, 0)]);
    }
}
