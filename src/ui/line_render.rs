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
/// Hanging-indent: when the line begins with a recognized list marker (or
/// leading whitespace from a list-item continuation paragraph), wrapped
/// continuation rows are left-padded so their text aligns with the column
/// where the first row's text begins — the marker hangs off on the left.
/// Detection lives in `compute_hanging_indent`; an indent of 0 (the default
/// for non-list lines) preserves the legacy zero-padding wrap.
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

    // Hanging indent — only meaningful when the indent leaves at least one
    // cell of room on continuation rows.  When the viewport is too narrow
    // for the indent to fit (or the line has no marker), fall back to flat
    // wrap.
    let indent = compute_hanging_indent(line);
    let indent = if indent + 1 >= width { 0 } else { indent };

    // Word-aware wrapping. See module docstring.
    let mut cur_visual = visual_y;
    let mut start = 0;
    let mut char_col_base = 0usize;
    let mut row_idx = 0usize;

    while start < chars.len() {
        if cur_visual >= area.height {
            break;
        }
        let cur_abs_y = area.y + cur_visual;
        let row_indent = if row_idx == 0 { 0 } else { indent };
        let row_width = width.saturating_sub(row_indent).max(1);

        let remaining = chars.len() - start;
        let (row_end, next_start) = if remaining <= row_width {
            (chars.len(), chars.len())
        } else {
            let window = &chars[start..start + row_width];
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
                    let end = start + row_width;
                    (end, end)
                }
            }
        };

        // Pre-pad the hanging-indent cells with the line's base style so the
        // continuation rows show as flush-left blank columns rather than the
        // previous frame's leftover content.
        let mut x = area.x;
        for _ in 0..row_indent {
            if x >= area.x + area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, cur_abs_y)) {
                cell.set_char(' ');
                cell.set_style(line_style);
            }
            x += 1;
        }
        for (rel_idx, (ch, style)) in chars[start..row_end].iter().enumerate() {
            if x >= area.x + area.width {
                break;
            }
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
        row_idx += 1;
    }

    (cur_visual - visual_y).max(1)
}

/// Compute the list of visual rows produced by wrapping `chars` at `width`
/// with a hanging `indent`.  When `indent > 0`, the first row uses the full
/// `width` and every continuation row uses `width - indent`.
///
/// Returns a list of `(start, end, next_start)` tuples, where:
/// - `chars[start..end]` is the content placed on that visual row
/// - `next_start` is the index at which the next visual row begins (may be
///   `> end` when trailing spaces are consumed at the wrap point)
///
/// The algorithm mirrors `render_line` exactly so that visual-line navigation
/// lands the cursor on the same visual row as the renderer draws.
pub fn visual_rows_of_chars(
    chars: &[(char, Style)],
    width: usize,
    indent: usize,
) -> Vec<(usize, usize, usize)> {
    let mut rows = Vec::new();
    if width == 0 {
        rows.push((0, chars.len(), chars.len()));
        return rows;
    }
    // If the indent leaves no room on continuation rows, ignore it — matches
    // `render_line_with_cursor`'s fallback so wrap-row counts stay in sync.
    let indent = if indent + 1 >= width { 0 } else { indent };

    let mut start = 0;
    let mut row_idx = 0usize;
    loop {
        if start >= chars.len() {
            if rows.is_empty() {
                rows.push((0, 0, 0));
            }
            break;
        }
        let row_width = if row_idx == 0 {
            width
        } else {
            width.saturating_sub(indent).max(1)
        };
        let remaining = chars.len() - start;
        let (row_end, next_start) = if remaining <= row_width {
            (chars.len(), chars.len())
        } else {
            let window = &chars[start..start + row_width];
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
                None => (start + row_width, start + row_width),
            }
        };

        rows.push((start, row_end, next_start));
        if next_start >= chars.len() || next_start == start {
            break;
        }
        start = next_start;
        row_idx += 1;
    }

    if rows.is_empty() {
        rows.push((0, 0, 0));
    }

    rows
}

/// Compute the list of visual rows produced by wrapping a plain string `text`
/// at `width`.  Convenience wrapper around `visual_rows_of_chars` used by the
/// cursor/navigation code which operates on RAW buffer text — no hanging
/// indent applies (the cursor's raw line in Rendered mode and every line in
/// Raw mode follow the source layout, not the rendered layout).
pub fn visual_rows_of_str(text: &str, width: usize) -> Vec<(usize, usize, usize)> {
    let chars: Vec<(char, Style)> = text.chars().map(|c| (c, Style::default())).collect();
    visual_rows_of_chars(&chars, width, 0)
}

/// Number of visual rows a styled `Line` occupies when wrapped at `width`.
///
/// Mirrors the wrap algorithm used by `render_line`, so scroll-bound
/// calculations can match what the viewport actually draws.  Empty lines
/// always consume a single row.  Detects the line's hanging indent the same
/// way `render_line` does — list-item continuations consume more rows when
/// indented than they would under flat wrap.
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
    let indent = compute_hanging_indent(line);
    visual_rows_of_chars(&chars, width, indent).len().max(1)
}

/// Hanging-indent (in cells) for the given rendered `Line`.
///
/// The indent is the column where the first character after the list marker
/// begins on the first visual row — wrapped continuation rows are then
/// left-padded by this amount so the wrapped text aligns with the first
/// line's text column and the marker visually hangs off to its left.
///
/// Detected shapes:
///
/// 1. Rendered bullet:           `• text`           → indent = leading_ws + 2
/// 2. Raw bullet (raw-revealed): `- text`           → indent = leading_ws + 2
/// 3. Task without bullet:       `[ ] text`         → indent = leading_ws + 4
/// 4. Bullet + task:             `- [ ] text`       → indent = leading_ws + 6
/// 5. Ordered (rendered/raw):    `1. text`/` 1. `   → indent = leading_ws + digit_width + 2
/// 6. Continuation paragraph:    `   text`          → indent = leading_ws
///
/// Returns 0 for lines that don't match any of these shapes (plain
/// paragraphs, blockquoted content, table rows, code blocks, etc.).
pub fn compute_hanging_indent(line: &Line<'_>) -> usize {
    let chars: Vec<char> = line.spans.iter().flat_map(|s| s.content.chars()).collect();
    let mut i = 0;
    while i < chars.len() && chars[i] == ' ' {
        i += 1;
    }
    let leading = i;

    // Rendered bullet glyph (`•`) — only emitted by the renderer.
    if chars.get(i) == Some(&'•') && chars.get(i + 1) == Some(&' ') {
        return text_start_after_optional_task_prefix(&chars, i + 2);
    }
    // Raw bullet glyph (`-`, `*`, `+`) — used when the cursor's list-item
    // line is shown raw inside the otherwise-rendered `RenderedView`.  We
    // hang-indent it too so the cursor's row stays visually aligned with
    // the surrounding rendered list.
    if matches!(chars.get(i), Some('-') | Some('*') | Some('+')) && chars.get(i + 1) == Some(&' ') {
        return text_start_after_optional_task_prefix(&chars, i + 2);
    }
    // Task without bullet (the renderer drops the bullet for task items —
    // the checkbox is the visual anchor).
    if is_task_marker(&chars, i) {
        return i + 4;
    }
    // Ordered marker: digits + `.`/`)` + space.  Matches both the raw form
    // (`1. `) and the rendered right-aligned form (` 1. `).
    let digit_count = chars[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digit_count > 0
        && matches!(chars.get(i + digit_count), Some('.') | Some(')'))
        && chars.get(i + digit_count + 1) == Some(&' ')
    {
        return text_start_after_optional_task_prefix(&chars, i + digit_count + 2);
    }

    // Continuation paragraph or otherwise-indented text (e.g. list-item
    // child block, indented heading).  Hanging-indent at the leading-space
    // count keeps wrapped continuations flush with the indented body.
    if leading > 0 {
        return leading;
    }
    0
}

fn is_task_marker(chars: &[char], i: usize) -> bool {
    chars.get(i) == Some(&'[')
        && matches!(chars.get(i + 1), Some(' ') | Some('x') | Some('X'))
        && chars.get(i + 2) == Some(&']')
        && chars.get(i + 3) == Some(&' ')
}

fn text_start_after_optional_task_prefix(chars: &[char], pos: usize) -> usize {
    if is_task_marker(chars, pos) {
        pos + 4
    } else {
        pos
    }
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
    use ratatui::text::Span;

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

    #[test]
    fn hanging_indent_bullet() {
        let line = Line::from(vec![Span::raw("• "), Span::raw("foo bar")]);
        assert_eq!(compute_hanging_indent(&line), 2);
    }

    #[test]
    fn hanging_indent_raw_bullet() {
        let line = Line::from(vec![Span::raw("- foo bar")]);
        assert_eq!(compute_hanging_indent(&line), 2);
    }

    #[test]
    fn hanging_indent_ordered_single_digit() {
        let line = Line::from(vec![Span::raw("1. "), Span::raw("foo")]);
        assert_eq!(compute_hanging_indent(&line), 3);
    }

    #[test]
    fn hanging_indent_ordered_padded() {
        // ` 1. foo` — right-aligned single-digit when list reaches 10+.
        let line = Line::from(vec![Span::raw(" 1. "), Span::raw("foo")]);
        assert_eq!(compute_hanging_indent(&line), 4);
    }

    #[test]
    fn hanging_indent_ordered_double_digit() {
        let line = Line::from(vec![Span::raw("10. "), Span::raw("foo")]);
        assert_eq!(compute_hanging_indent(&line), 4);
    }

    #[test]
    fn hanging_indent_task_no_bullet() {
        // Renderer drops `- ` for task items; the checkbox is the marker.
        let line = Line::from(vec![Span::raw("[ ] "), Span::raw("foo")]);
        assert_eq!(compute_hanging_indent(&line), 4);
    }

    #[test]
    fn hanging_indent_task_raw_revealed() {
        // Cursor's raw line in Rendered view: `- [ ] foo`.
        let line = Line::from(vec![Span::raw("- [ ] foo")]);
        assert_eq!(compute_hanging_indent(&line), 6);
    }

    #[test]
    fn hanging_indent_nested_bullet() {
        // Outer bullet → child indent of 2 spaces, then nested bullet.
        let line = Line::from(vec![Span::raw("  • "), Span::raw("inner")]);
        assert_eq!(compute_hanging_indent(&line), 4);
    }

    #[test]
    fn hanging_indent_continuation_paragraph() {
        // List-item continuation paragraph: just leading spaces, no marker.
        let line = Line::from(vec![Span::raw("   "), Span::raw("more text")]);
        assert_eq!(compute_hanging_indent(&line), 3);
    }

    #[test]
    fn hanging_indent_plain_paragraph() {
        let line = Line::from(vec![Span::raw("Hello world")]);
        assert_eq!(compute_hanging_indent(&line), 0);
    }

    #[test]
    fn hanging_indent_blockquote() {
        // Blockquote bar must not register as a marker.
        let line = Line::from(vec![Span::raw("▎ "), Span::raw("quoted")]);
        assert_eq!(compute_hanging_indent(&line), 0);
    }

    #[test]
    fn visual_rows_with_indent_word_aligned() {
        // First row holds "• hello " (8 chars), continuation row has width
        // 10 - 2 = 8 for indented body.  "world foo" fits in 9 chars, so
        // the wrap places "world " on row 2 and "foo" on row 3.
        let chars: Vec<(char, Style)> = "• hello world foo"
            .chars()
            .map(|c| (c, Style::default()))
            .collect();
        let rows = visual_rows_of_chars(&chars, 10, 2);
        // Row 0 width 10: breaks at the space after "hello", giving
        // "• hello " (8 chars) before consuming the trailing space.
        assert_eq!(rows[0].0, 0);
        // Row 1 width 8: "world " then break.
        assert!(rows.len() >= 2);
        assert_eq!(rows.last().map(|r| r.1), Some(17));
    }

    #[test]
    fn visual_rows_with_indent_zero_matches_flat() {
        let s = "hello world foo";
        let chars: Vec<(char, Style)> = s.chars().map(|c| (c, Style::default())).collect();
        let with_indent = visual_rows_of_chars(&chars, 10, 0);
        let flat = visual_rows_of_str(s, 10);
        assert_eq!(with_indent, flat);
    }

    #[test]
    fn visual_rows_for_line_counts_indent_extra_rows() {
        // Bullet line wrapped at width 10 — indent = 2, so continuation rows
        // are 8 chars wide.  Total visual rows must be ≥ flat count.
        let line = Line::from(vec![Span::raw("• "), Span::raw("hello world foo bar baz")]);
        let with_marker = visual_rows_for_line(&line, 10);
        let line_flat = Line::from(vec![Span::raw("hello world foo bar baz")]);
        let flat = visual_rows_for_line(&line_flat, 10);
        assert!(with_marker >= flat);
    }
}
