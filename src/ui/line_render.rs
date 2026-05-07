use ratatui::{buffer::Buffer as TuiBuf, layout::Rect, style::Style, text::Line};
use unicode_width::UnicodeWidthChar;

/// Display width of `ch` in terminal cells.  Wide chars (CJK, most emoji)
/// return 2; ASCII / BMP narrow chars return 1; control chars return 0.
/// Used by both the renderer and the wrap-row calculator so on-screen
/// geometry agrees with cursor and selection coordinates.
pub fn char_cells(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Char index in `text` (0-based) corresponding to screen cell column
/// `target_cell`, assuming the row's first content cell sits at column
/// `indent` (>0 for hanging-indent continuation rows).  Implements the
/// landing rules used by vertical navigation and mouse clicks alike:
///
/// - **Forbidden indent zone:** when `target_cell <= indent` the cursor
///   lands at char index 0 — the first content char.  The hanging-indent
///   padding on a wrapped list-item continuation row is virtual, not
///   text, so the cursor never sits there.
/// - **Wide-char snap-past:** when `target_cell` falls *inside* a
///   multi-cell glyph (CJK, emoji), the cursor lands *after* that glyph.
///   It never visually sits in the right half of a wide char.
/// - **Past content:** when `target_cell` exceeds the row's total cell
///   width, returns the total char count of `text` (one past the last
///   char) — callers clamp to row end as appropriate.
pub fn char_idx_at_cell_col<I>(iter: I, target_cell: usize, indent: usize) -> usize
where
    I: IntoIterator<Item = char>,
{
    if target_cell <= indent {
        return 0;
    }
    let mut acc = indent;
    let mut count = 0;
    for ch in iter {
        let w = char_cells(ch);
        if acc + w > target_cell {
            return if acc == target_cell { count } else { count + 1 };
        }
        acc += w;
        count += 1;
    }
    count
}

/// Inverse of `char_idx_at_cell_col`: cumulative cell width of the first
/// `char_idx` chars of `iter`, plus `indent`.  Use this to seed
/// `preferred_col` after a horizontal cursor move so subsequent vertical
/// navigation lands at the same screen cell.
pub fn cell_col_at_char_idx<I>(iter: I, char_idx: usize, indent: usize) -> usize
where
    I: IntoIterator<Item = char>,
{
    let mut acc = indent;
    for (i, ch) in iter.into_iter().enumerate() {
        if i >= char_idx {
            break;
        }
        acc += char_cells(ch);
    }
    acc
}

/// Largest `n` such that the cumulative cell width of
/// `chars[start..start + n]` fits within `cell_budget`.  When the very
/// first char is itself wider than `cell_budget` we still return `1` so
/// the wrap loop makes progress at very narrow viewports — the renderer
/// will clip the overflowing right half on draw.
fn chars_within_cell_budget(chars: &[(char, Style)], start: usize, cell_budget: usize) -> usize {
    let mut total = 0usize;
    let mut count = 0usize;
    for (ch, _) in &chars[start..] {
        let w = char_cells(*ch);
        if count > 0 && total + w > cell_budget {
            break;
        }
        total += w;
        count += 1;
        if total >= cell_budget {
            break;
        }
    }
    count
}

/// Write a styled `Line` to the TUI buffer, wrapping at `area.width` when
/// `wrap` is true. Returns the number of visual rows consumed (≥ 1).
///
/// Trailing cells in every visual row are filled with the line's base style so
/// styled blocks (e.g. code blocks) extend to the full width of `area`.  The
/// wrap algorithm is word-aware: breaks prefer the last non-alphanumeric
/// character within the row, falling back to a hard break when a single word
/// exceeds the row width.  `area.width` is interpreted as terminal *cells*,
/// so wide chars (emoji, CJK) consume two columns of budget per char.
///
/// Hanging-indent: when the line begins with a recognized list marker (or
/// leading whitespace from a list-item continuation paragraph), wrapped
/// continuation rows are left-padded so their text aligns with the column
/// where the first row's text begins — the marker hangs off on the left.
/// Detection lives in `compute_hanging_indent`; an indent of 0 (the default
/// for non-list lines) preserves the legacy zero-padding wrap.
///
/// `cursor_col_override`: when `Some((col, style))`, the character at char
/// index `col` (NOT cell column) on the first output row is rendered with
/// `style` (used to show a cursor indicator during the jitter-suppression
/// delay in hybrid rendered mode).  The style applies only to the first cell
/// of a wide char — terminals can't independently style the right half.
/// Used by tests in this module; production code uses
/// `render_line_from_visual` to support sub-row scrolling.
#[allow(dead_code)]
pub fn render_line(
    line: &Line<'static>,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
) -> u16 {
    render_line_with_cursor_from_visual(line, area, buf, visual_y, wrap, None, 0)
}

pub fn render_line_from_visual(
    line: &Line<'static>,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
    skip_rows: usize,
) -> u16 {
    render_line_with_cursor_from_visual(line, area, buf, visual_y, wrap, None, skip_rows)
}

/// Used by tests in this module.
#[allow(dead_code)]
pub fn render_line_with_cursor(
    line: &Line<'static>,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
    cursor_col_override: Option<(usize, Style)>,
) -> u16 {
    render_line_with_cursor_from_visual(line, area, buf, visual_y, wrap, cursor_col_override, 0)
}

pub fn render_line_with_cursor_from_visual(
    line: &Line<'static>,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
    cursor_col_override: Option<(usize, Style)>,
    skip_rows: usize,
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
        if skip_rows > 0 {
            return 0;
        }
        paint_row(
            &chars,
            0,
            chars.len(),
            0,
            0,
            area,
            buf,
            abs_y,
            line_style,
            cursor_col_override,
        );
        return 1;
    }

    // Single source of truth for row breaks — keeps the renderer in lockstep
    // with the navigation/selection helpers below.
    let indent = compute_hanging_indent(line);
    let rows = visual_rows_of_chars(&chars, width, indent);
    let effective_indent = if indent + 1 >= width { 0 } else { indent };

    let mut cur_visual = visual_y;
    for (row_idx, &(start, row_end, _next_start)) in rows.iter().enumerate().skip(skip_rows) {
        if cur_visual >= area.height {
            break;
        }
        let cur_abs_y = area.y + cur_visual;
        let row_indent = if row_idx == 0 { 0 } else { effective_indent };
        paint_row(
            &chars,
            start,
            row_end,
            start,
            row_indent,
            area,
            buf,
            cur_abs_y,
            line_style,
            cursor_col_override,
        );
        cur_visual += 1;
    }

    cur_visual - visual_y
}

/// Paint a single visual row.  `chars[start..end]` are written starting at
/// `area.x + row_indent` (after writing `row_indent` blanks in `line_style`
/// so the indent column shows the surrounding background).  `abs_col_base`
/// is the char-index offset to add to `rel_idx` when matching against
/// `cursor_col_override` — for wrapped continuation rows this is the row's
/// `start`; for the no-wrap fast path it's 0.
#[allow(clippy::too_many_arguments)]
fn paint_row(
    chars: &[(char, Style)],
    start: usize,
    end: usize,
    abs_col_base: usize,
    row_indent: usize,
    area: Rect,
    buf: &mut TuiBuf,
    abs_y: u16,
    line_style: Style,
    cursor_col_override: Option<(usize, Style)>,
) {
    let mut x = area.x;
    let area_end = area.x + area.width;
    for _ in 0..row_indent {
        if x >= area_end {
            break;
        }
        if let Some(cell) = buf.cell_mut((x, abs_y)) {
            cell.set_char(' ');
            cell.set_style(line_style);
        }
        x += 1;
    }
    for (rel_idx, (ch, style)) in chars[start..end].iter().enumerate() {
        let cells = char_cells(*ch) as u16;
        if cells == 0 || x >= area_end {
            // Zero-width chars (e.g. ZWJ, variation selectors, combining
            // marks) are conceptually merged into the preceding grapheme
            // by the terminal — skip without advancing `x` rather than
            // overwriting the previous cell's glyph.
            if cells == 0 {
                continue;
            }
            break;
        }
        let abs_col = abs_col_base + rel_idx;
        let effective_style = cursor_col_override
            .filter(|(col, _)| *col == abs_col)
            .map(|(_, s)| s)
            .unwrap_or(*style);
        if let Some(cell) = buf.cell_mut((x, abs_y)) {
            cell.set_char(*ch);
            cell.set_style(effective_style);
        }
        x += cells;
    }
    while x < area_end {
        if let Some(cell) = buf.cell_mut((x, abs_y)) {
            cell.set_style(line_style);
        }
        x += 1;
    }
}

/// Compute the list of visual rows produced by wrapping `chars` at `width`
/// (in terminal cells) with a hanging `indent`.  When `indent > 0`, the
/// first row uses the full `width` and every continuation row uses
/// `width - indent`.
///
/// Returns a list of `(start, end, next_start)` tuples, where:
/// - `chars[start..end]` is the content placed on that visual row
/// - `next_start` is the index at which the next visual row begins (may be
///   `> end` when trailing spaces are consumed at the wrap point)
///
/// `width` and `indent` are cell counts; the returned indices are char
/// indices.  `render_line_with_cursor` calls this directly to drive its
/// row-by-row painting, so the renderer and visual-line navigation always
/// agree on where rows break.
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
        let n_chars = chars_within_cell_budget(chars, start, row_width);
        let remaining = chars.len() - start;
        let (row_end, next_start) = if n_chars >= remaining {
            (chars.len(), chars.len())
        } else {
            let window_end = start + n_chars;
            let break_rel = chars[start..window_end]
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
                None => (window_end, window_end),
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
/// 3. Bullet + task:             `- [ ] text`       → indent = leading_ws + 6
/// 4. Ordered (rendered/raw):    `1. text`/` 1. `   → indent = leading_ws + digit_width + 2
/// 5. Continuation paragraph:    `   text`          → indent = leading_ws
///
/// Returns 0 for lines that don't match any of these shapes (plain
/// paragraphs, blockquoted content, table rows, code blocks, etc.).
pub fn compute_hanging_indent(line: &Line<'_>) -> usize {
    let chars: Vec<char> = line.spans.iter().flat_map(|s| s.content.chars()).collect();
    compute_hanging_indent_chars(&chars)
}

/// String-based variant of [`compute_hanging_indent`] for use against the
/// raw buffer text (where there are no `Line` spans available, e.g. inside
/// `EditorState::move_up_visual` / `move_down_visual`).  Same detection
/// rules — see that function for the recognized marker shapes.
pub fn compute_hanging_indent_str(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    compute_hanging_indent_chars(&chars)
}

fn compute_hanging_indent_chars(chars: &[char]) -> usize {
    let mut i = 0;
    while i < chars.len() && chars[i] == ' ' {
        i += 1;
    }
    let leading = i;

    // Rendered bullet glyph (`•`) — only emitted by the renderer.
    if chars.get(i) == Some(&'•') && chars.get(i + 1) == Some(&' ') {
        return text_start_after_optional_task_prefix(chars, i + 2);
    }
    // Raw bullet glyph (`-`, `*`, `+`) — used when the cursor's list-item
    // line is shown raw inside the otherwise-rendered `RenderedView`.  We
    // hang-indent it too so the cursor's row stays visually aligned with
    // the surrounding rendered list.
    if matches!(chars.get(i), Some('-') | Some('*') | Some('+')) && chars.get(i + 1) == Some(&' ') {
        return text_start_after_optional_task_prefix(chars, i + 2);
    }
    // Ordered marker: digits + `.`/`)` + space.  Matches both the raw form
    // (`1. `) and the rendered right-aligned form (` 1. `).
    let digit_count = chars[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digit_count > 0
        && matches!(chars.get(i + digit_count), Some('.') | Some(')'))
        && chars.get(i + digit_count + 1) == Some(&' ')
    {
        return text_start_after_optional_task_prefix(chars, i + digit_count + 2);
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
    fn hanging_indent_rendered_task_includes_bullet_and_checkbox() {
        // Tasks render as `• [ ] foo` — bullet + space + checkbox + space
        // = 6 cells of marker before the body text begins.
        let line = Line::from(vec![Span::raw("• [ ] "), Span::raw("foo")]);
        assert_eq!(compute_hanging_indent(&line), 6);
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

    // ── Cell-width awareness ──────────────────────────────────────

    #[test]
    fn wrap_budget_is_cells_not_chars_for_wide_chars() {
        // Each emoji is 1 char / 2 cells.  At width 4, two emoji fill the
        // first row exactly; the third spills onto row 2.
        let chars: Vec<(char, Style)> = "🥇🥇🥇".chars().map(|c| (c, Style::default())).collect();
        let rows = visual_rows_of_chars(&chars, 4, 0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (0, 2, 2));
        assert_eq!(rows[1], (2, 3, 3));
    }

    #[test]
    fn wrap_force_breaks_when_single_wide_char_exceeds_width() {
        // Width 1 can't fit a 2-cell emoji, but the wrap loop must still
        // make progress — emit one char per row even though they overflow.
        let chars: Vec<(char, Style)> = "🥇🥇".chars().map(|c| (c, Style::default())).collect();
        let rows = visual_rows_of_chars(&chars, 1, 0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (0, 1, 1));
        assert_eq!(rows[1], (1, 2, 2));
    }

    #[test]
    fn render_line_paints_wide_char_using_two_cells() {
        // After painting "A🥇B" the cells must read 'A', '🥇', <skipped>, 'B'.
        // The renderer leaves the right-half cell of the wide char unwritten
        // — terminals own that half.  The next char must land at column 3.
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = TuiBuf::empty(area);
        let line = Line::from(vec![Span::raw("A🥇B")]);
        render_line(&line, area, &mut buf, 0, false);
        assert_eq!(
            buf.cell((0, 0)).map(|c| c.symbol().to_string()),
            Some("A".into())
        );
        assert_eq!(
            buf.cell((1, 0)).map(|c| c.symbol().to_string()),
            Some("🥇".into())
        );
        assert_eq!(
            buf.cell((3, 0)).map(|c| c.symbol().to_string()),
            Some("B".into())
        );
    }

    #[test]
    fn char_idx_at_cell_col_forbidden_indent_zone_snaps_to_row_start() {
        // Continuation row of a wrapped list item: indent = 2.  Clicks on
        // cells 0 and 1 land in the virtual padding; cells 2+ are content.
        // All cells in [0..=indent] must snap to char index 0 — the first
        // content char of the row.
        let chars = ['x', 'y', 'z'];
        for cell in 0..=2 {
            assert_eq!(
                char_idx_at_cell_col(chars.iter().copied(), cell, 2),
                0,
                "indent zone cell {cell} did not snap to row start",
            );
        }
        // Cell 3 is the first content cell — lands on 'x' (index 0 in the
        // row, since acc starts at indent=2 and the first char fills cell 2).
        // Wait: walk acc=2, ch='x', w=1, acc+w=3 NOT > 3, acc=3.  Then
        // ch='y', w=1, acc+w=4 > 3, acc==3==target → return count=1.
        assert_eq!(char_idx_at_cell_col(chars.iter().copied(), 3, 2), 1);
    }

    #[test]
    fn char_idx_at_cell_col_snaps_past_wide_char() {
        // 🥇 occupies cells 0–1.  Targeting cell 1 (mid-glyph) must snap
        // past, returning index 1 (cursor *after* the emoji).  Targeting
        // cell 0 (the glyph's start) returns index 0 (cursor before).
        let chars = ['🥇', 'B'];
        assert_eq!(char_idx_at_cell_col(chars.iter().copied(), 0, 0), 0);
        assert_eq!(char_idx_at_cell_col(chars.iter().copied(), 1, 0), 1);
        assert_eq!(char_idx_at_cell_col(chars.iter().copied(), 2, 0), 1);
    }

    #[test]
    fn cell_col_at_char_idx_round_trips_with_wide_chars() {
        let chars = ['A', '🥇', 'B'];
        // Char 0 → cell 0; char 1 → cell 1 (after A); char 2 → cell 3
        // (past A and the wide emoji); char 3 → cell 4.
        assert_eq!(cell_col_at_char_idx(chars.iter().copied(), 0, 0), 0);
        assert_eq!(cell_col_at_char_idx(chars.iter().copied(), 1, 0), 1);
        assert_eq!(cell_col_at_char_idx(chars.iter().copied(), 2, 0), 3);
        assert_eq!(cell_col_at_char_idx(chars.iter().copied(), 3, 0), 4);
    }

    #[test]
    fn zero_width_combining_mark_does_not_advance_cell_cursor() {
        // 'e' + U+0301 ("é") — the combining mark has zero display width;
        // it must not consume a cell on its own.  After painting, column 1
        // is the next character ('!'), not blank.
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = TuiBuf::empty(area);
        let line = Line::from(vec![Span::raw("e\u{0301}!")]);
        render_line(&line, area, &mut buf, 0, false);
        assert_eq!(
            buf.cell((0, 0)).map(|c| c.symbol().to_string()),
            Some("e".into())
        );
        assert_eq!(
            buf.cell((1, 0)).map(|c| c.symbol().to_string()),
            Some("!".into())
        );
    }
}
