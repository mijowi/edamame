use crate::document::wrap;
use crate::document::{row_map, CellBand};
use crate::editor::row_offset::{locate_block, rendered_sub_line_to_offset, source_text};
use crate::editor::table_edit_ops;
use crate::editor::{EditorState, Mode};
use crate::markdown::table_layout;

/// The rendered line under document-area `row` (scroll- and wrap-aware): its index into
/// `parsed.lines`, and the sub-row within it.
pub(super) fn rendered_line_at_row(state: &EditorState, row: usize) -> Option<(usize, usize)> {
    let lines = &state.parsed.lines;
    if lines.is_empty() {
        return None;
    }
    let (mut line_idx, mut first_sub_row) =
        state.rendered_line_at_visual_row(state.scroll.saturating_add(row), state.viewport_width);
    let mut y = 0usize;
    while line_idx < lines.len() {
        let rows_used = state
            .parsed
            .visual_rows_for_line_at(line_idx, state.viewport_width)
            .max(1);
        let visible_rows = rows_used.saturating_sub(first_sub_row).max(1);
        if y < visible_rows {
            return Some((line_idx, first_sub_row));
        }
        y += visible_rows;
        line_idx += 1;
        first_sub_row = 0;
    }
    None
}

/// Translate a document-area click to a buffer char offset (scroll- and wrap-aware).  Clicks
/// past content clamp to the nearest valid position; `None` only in Diff mode, which handles
/// its own clicks via `DiffView`.
pub(super) fn click_to_char_offset(
    state: &EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> Option<usize> {
    match state.mode {
        Mode::Raw => Some(raw_click_to_offset(state, col, row, viewport_width)),
        Mode::Preview | Mode::Rendered => {
            Some(rendered_click_to_offset(state, col, row, viewport_width))
        }
        Mode::Diff => None,
    }
}

/// Raw-mode click: walk buffer lines from `state.scroll` by wrapped visual rows; cell-aware so
/// wide chars align.
fn raw_click_to_offset(
    state: &EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> usize {
    let line_count = state.buffer.line_count();
    let width = viewport_width.max(1);
    let (mut target_line, mut first_sub_row) = state.raw_line_at_visual_row(state.scroll, width);
    let mut y = 0usize;
    while target_line < line_count {
        let text = state
            .buffer
            .line(target_line)
            .map(|s| s.trim_end_matches('\n').to_owned())
            .unwrap_or_default();
        let rows = wrap::visual_rows_of_str(&text, width);
        let used = rows.len().max(1).saturating_sub(first_sub_row).max(1);
        if row < y + used {
            let sub_row = first_sub_row + row - y;
            let line_start = state.buffer.line_to_char(target_line);
            let row_tuple = rows.get(sub_row).copied().unwrap_or((0, 0, 0));
            let raw_col = char_in_row_at_cell(&text, row_tuple, col, 0, sub_row + 1 == rows.len());
            return line_start + raw_col;
        }
        y += used;
        target_line += 1;
        first_sub_row = 0;
    }
    state.buffer.len_chars()
}

/// Cell-aware click cell → char column on the logical line.  Mirrors
/// `state::raw_col_for_visual_cells`; see it for the wide-char snap-past rule and the forbidden
/// indent zone.
fn char_in_row_at_cell(
    text: &str,
    row: (usize, usize, usize),
    target_cell: usize,
    indent: usize,
    is_last_row: bool,
) -> usize {
    let (start, end, _) = row;
    let max_char_in_row = wrap::last_col_in_row(row, is_last_row);
    let row_chars = text.chars().skip(start).take(end - start);
    let in_row = wrap::char_idx_at_cell_col(row_chars, target_cell, indent);
    (start + in_row).min(max_char_in_row)
}

/// Rendered/Preview click: find the rendered line and sub-row, then map back to source.
fn rendered_click_to_offset(
    state: &EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> usize {
    match walk_rendered_rows(state, row, viewport_width) {
        Some((idx, sub_row)) => {
            rendered_sub_line_to_offset(state, idx, sub_row, col, viewport_width)
        }
        None => state.buffer.len_chars(),
    }
}

/// Preview-mode click translator: the `(rendered_line_idx, char_col)` that seeds a
/// `VisualSelection`.  `char_col` is the cumulative position within the flat rendered line,
/// using the same wrap layout as [`patch_char_cols`](crate::ui::line_render::patch_char_cols),
/// so drags across wrapped sub-rows highlight the right range.  `col` is a screen cell: a click on the right half of
/// a wide glyph lands after it, as [`wrap::char_idx_at_cell_col`] snaps.
pub(super) fn rendered_click_to_line_col(
    state: &EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> Option<(usize, usize)> {
    let (idx, char_col, _) =
        rendered_click_to_line_col_with_layout(state, col, row, viewport_width)?;
    Some((idx, char_col))
}

/// [`rendered_click_to_line_col`] plus the [`LineLayout`] it wrapped against (`None` for an
/// empty rendered line), so a caller inverting the mapping doesn't rebuild it.
fn rendered_click_to_line_col_with_layout(
    state: &EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> Option<(usize, usize, Option<LineLayout>)> {
    let (idx, sub_row) = walk_rendered_rows(state, row, viewport_width)?;
    let chars = wrap::styled_chars(state.parsed.lines.get(idx)?);
    if chars.is_empty() {
        return Some((idx, 0, None));
    }
    let width = viewport_width.max(1);
    let stated = state.parsed.row_indent(idx);
    let rows = wrap::visual_rows_of_chars(&chars, width, stated);
    let indent = stated.at(width);
    let sub = sub_row.min(rows.len().saturating_sub(1));
    let row = rows
        .get(sub)
        .copied()
        .unwrap_or((0, chars.len(), chars.len()));
    let (row_start, _, _) = row;
    let max_in_row = wrap::last_col_in_row(row, sub + 1 == rows.len());
    // Rows carry their indent's cells of left padding (as `patch_char_cols`).
    let row_indent = indent.row(sub);
    let chars: Vec<char> = chars.into_iter().map(|(c, _)| c).collect();
    let local_col = wrap::char_idx_at_cell_col(chars[row_start..].iter().copied(), col, row_indent);
    let char_col = (row_start + local_col).min(max_in_row);
    let layout = LineLayout {
        rows,
        indent,
        chars,
    };
    Some((idx, char_col, Some(layout)))
}

/// Like [`rendered_click_to_line_col`], but declines when the cell lies past the last painted
/// character of its visual row instead of clamping onto it.  The clamp is right for cursor
/// placement and wrong for hit-testing: a line ending in a link would otherwise report that
/// link for every blank cell to its right.
pub(super) fn rendered_click_to_line_col_on_text(
    state: &EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> Option<(usize, usize)> {
    let (idx, char_col, layout) =
        rendered_click_to_line_col_with_layout(state, col, row, viewport_width)?;
    let layout = layout?;
    // The click snaps past a wide glyph whose right half it hit; step back onto that glyph.
    let char_col = match cell_col_for_char_col(&layout, char_col) {
        Some(cell) if cell > col => char_col.checked_sub(1)?,
        _ => char_col,
    };
    let ch = *layout.chars.get(char_col)?;
    // A real hit is one whose clamped column covers the clicked cell.
    let cell = cell_col_for_char_col(&layout, char_col)?;
    (cell..cell + wrap::char_cells(ch).max(1))
        .contains(&col)
        .then_some((idx, char_col))
}

/// The wrap layout [`rendered_click_to_line_col`] resolved a click against, returned so the
/// inverse mapping (which runs on every mouse-move) doesn't rebuild it.
pub(super) struct LineLayout {
    /// `(row_start, row_end, next_start)` per visual row, as
    /// [`wrap::visual_rows_of_chars`] produces them.
    rows: Vec<(usize, usize, usize)>,
    /// The indent the rows start behind, as applied at the width.
    indent: wrap::Indent,
    /// The rendered line's chars, for measuring cells.
    chars: Vec<char>,
}

/// Screen cell column at which `char_col` is painted, including a continuation row's hanging
/// indent — the inverse of [`rendered_click_to_line_col`].
fn cell_col_for_char_col(layout: &LineLayout, char_col: usize) -> Option<usize> {
    let (sub, _) = wrap::sub_line_of_col(&layout.rows, char_col);
    let (row_start, _, _) = layout.rows.get(sub).copied()?;
    let row_indent = layout.indent.row(sub);
    let row_chars = layout.chars.get(row_start..)?.iter().copied();
    Some(wrap::cell_col_at_char_idx(
        row_chars,
        char_col.saturating_sub(row_start),
        row_indent,
    ))
}

/// Which `(rendered_line_idx, sub_row_within_line)` document-relative `row` falls on, walking
/// from `state.scroll` with reveal corrections.  Shared by `rendered_click_to_offset` and
/// `rendered_click_to_line_col` so the two cannot drift.
fn walk_rendered_rows(
    state: &EditorState,
    row: usize,
    viewport_width: usize,
) -> Option<(usize, usize)> {
    let lines = &state.parsed.lines;
    if lines.is_empty() {
        return None;
    }
    let (mut idx, mut first_sub_row) =
        state.rendered_line_at_visual_row(state.scroll, viewport_width);
    let reveal = cursor_reveal(state);
    let mut y = 0usize;
    while idx < lines.len() {
        let rows_used = revealed_raw_row_count(state, reveal.as_ref(), idx, viewport_width)
            .unwrap_or_else(|| state.parsed.visual_rows_for_line_at(idx, viewport_width))
            .max(1);
        let used = rows_used.saturating_sub(first_sub_row).max(1);
        if row < y + used {
            return Some((idx, first_sub_row + row - y));
        }
        y += used;
        idx += 1;
        first_sub_row = 0;
    }
    None
}

/// The revealed cursor block, gathered once per click: [`walk_rendered_rows`] asks
/// [`revealed_raw_row_count`] about every row it passes, so the block text is copied and the
/// cursor row looked up once rather than per row.
struct CursorReveal {
    block_idx: usize,
    block_lines: std::ops::Range<usize>,
    text: String,
    /// The revealed cursor row and the raw line it paints (see [`row_offset`](crate::editor::row_offset)).
    cursor_row: usize,
    cursor_line: usize,
}

/// The cursor block's [`CursorReveal`]; `None` while the reveal is off.
fn cursor_reveal(state: &EditorState) -> Option<CursorReveal> {
    if !state.cursor_block_revealed() {
        return None;
    }
    let block_idx = state.cursor_block_idx?;
    let block_lines = state.parsed.source_map.rendered_lines_for_block(block_idx);
    let block_start_byte = state
        .parsed
        .source_map
        .original_byte_for_rendered_line(block_lines.start)?;
    let block_range = state
        .parsed
        .source_map
        .original_range_for_byte(block_start_byte)?;
    Some(CursorReveal {
        block_idx,
        block_lines,
        text: source_text(state, &block_range),
        cursor_row: crate::editor::state::cursor_rendered_line_idx(state),
        cursor_line: crate::editor::state::cursor_raw_line(state),
    })
}

/// When the reveal is active and `rendered_line_idx` is inside the cursor's block, the wrap
/// count of the raw source line the painter actually paints there (raw text carries markers
/// and may wrap to more rows than the rendered form).  `None` otherwise; callers fall back to
/// the per-line cache.  Covers mermaid blocks (every reserved row) and the non-table cursor
/// line.
fn revealed_raw_row_count(
    state: &EditorState,
    reveal: Option<&CursorReveal>,
    rendered_line_idx: usize,
    viewport_width: usize,
) -> Option<usize> {
    let reveal = reveal?;
    if !reveal.block_lines.contains(&rendered_line_idx) {
        return None;
    }
    let block_text = reveal.text.as_str();

    if state.parsed.is_diagram_reveal_block(reveal.block_idx) {
        // The source line the row shows; a math-preview band row or padding past the source
        // paints empty, one row.
        let row_line = row_map::revealed_diagram_line(
            &state.parsed,
            reveal.block_idx,
            rendered_line_idx.saturating_sub(reveal.block_lines.start),
        );
        let raw_line = row_line
            .and_then(|l| block_text.split('\n').nth(l))
            .unwrap_or("");
        let indent = row_line.map_or(wrap::Indent::NONE, |l| {
            row_map::revealed_indent(&state.parsed, reveal.block_idx, l)
        });
        return Some(wrap::revealed_row_count(raw_line, indent, viewport_width));
    }

    // A revealed reflowed paragraph is one rendered line that reveals to its *stacked* raw lines,
    // so its row count is the sum of every raw line's wrap count — not just the first line's.
    // Its block's other rows (an item's siblings, a quote's other paragraphs) stay rendered.
    let effective = state.effective_rows(viewport_width);
    if effective
        .block_rendered()
        .is_some_and(|r| r.contains(&rendered_line_idx))
    {
        let total: usize = effective
            .raw_lines()
            .map(|line| effective.raw_wrap_at(line))
            .sum();
        return Some(total.max(1));
    }
    if rendered_line_idx != reveal.cursor_row {
        return None;
    }
    // A table row keeps its rendered chrome: the reveal shows raw text inside the cell only.
    let cursor_row_in_block = reveal.cursor_row.saturating_sub(reveal.block_lines.start);
    if row_map::table_row(&state.parsed, reveal.block_idx, cursor_row_in_block).is_some() {
        return None;
    }

    let cursor_line = reveal.cursor_line;
    let raw_line = block_text.split('\n').nth(cursor_line).unwrap_or("");
    // A row the view doesn't de-render (a code block's body) still shows its padded rendered
    // line; the raw wrap count would mis-walk every row below it.
    if !row_map::reveals(&state.parsed, reveal.block_idx, cursor_row_in_block) {
        return None;
    }
    let indent = row_map::revealed_indent(&state.parsed, reveal.block_idx, cursor_line);
    Some(wrap::revealed_row_count(raw_line, indent, viewport_width))
}

/// The table-cell band under a Preview click: the inclusive rendered-line range of the cell's
/// logical row plus the half-open column range of its content area.  `None` off a header or
/// data sub-line, where callers keep full-line selection.
pub(super) fn preview_table_cell_band(
    state: &EditorState,
    rendered_line_idx: usize,
    col: usize,
) -> Option<CellBand> {
    let block = locate_block(state, rendered_line_idx)?;
    let hit = row_map::table_row(&state.parsed, block.idx, block.sub_idx).filter(|h| h.cells)?;

    // `col` is a char column on the clicked line, so find the cell by char pipes; the band
    // itself is in cells, which every sub-line of the row shares.  Cell `i`'s content area is
    // `[pipes[i] + 2, pipes[i + 1] - 1)`.
    let line = &state.parsed.lines[rendered_line_idx];
    let pipes = table_layout::rendered_pipe_positions(line);
    let pipe_cells = table_layout::rendered_pipe_cells(line);
    if pipes.len() < 2 {
        return None;
    }
    let col_count = pipes.len() - 1;
    let cell_idx = (0..col_count)
        .find(|&i| col < pipes[i + 1])
        .unwrap_or(col_count - 1);
    let cols = (
        pipe_cells[cell_idx] + 2,
        (pipe_cells[cell_idx + 1]).saturating_sub(1),
    );
    if cols.0 >= cols.1 {
        return None;
    }
    Some(CellBand {
        lines: (
            block.rendered_span.start + hit.rows.start,
            block.rendered_span.start + hit.rows.end - 1,
        ),
        cols,
    })
}

/// Buffer char range of the header/data table cell containing `char_offset`, used to clamp a
/// Rendered-mode drag to the cell it began in.
pub(super) fn table_cell_char_range_at(
    state: &EditorState,
    char_offset: usize,
) -> Option<(usize, usize)> {
    let rope = state.buffer.rope();
    let byte = rope.char_to_byte(char_offset.min(rope.len_chars()));
    let block = state.parsed.source_map.block_for_byte(byte)?;
    let block_start = state
        .parsed
        .source_map
        .original_range_for_block(block)?
        .start;
    let line_idx = rope.byte_to_line(byte);
    let line = line_idx.checked_sub(rope.byte_to_line(block_start.min(rope.len_bytes())))?;
    // A header or data row, found by its origin at any nesting depth; the delimiter line shows
    // only on the heavy rule, which is chrome.
    let row = row_map::row_for_line(&state.parsed, block, line);
    row_map::table_row(&state.parsed, block, row).filter(|h| h.cells && h.line == line)?;
    let info = table_edit_ops::locate_table(state, byte)?;
    // The last row starting at or before `byte`: `byte` is on one of the table's lines, and a
    // last row with no newline ends at the buffer's end, which `start..end` would exclude.
    let row = info.rows.iter().rev().find(|r| r.start <= byte)?;
    let cell = row.cell_at(byte - row.start)?;
    Some((
        rope.byte_to_char(row.start + cell.content_start),
        rope.byte_to_char(row.start + cell.content_end),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::document::Buffer;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    /// The band is built in cells so every sub-line of the row can share it.
    #[test]
    fn preview_table_cell_band_is_in_cells() {
        let st = preview_state("| 日本 | ab |\n|---|---|\n| x | y |\n", 40);
        let header = st
            .parsed
            .lines
            .iter()
            .position(|l| l.spans.iter().any(|s| s.content.contains('日')))
            .expect("header line");
        // Cells: │0 ␠1 日本2-5 ␠6 │7 ␠8 a9 b10 ␠11 ␠12 │13; `b` is char 8.
        let band = preview_table_cell_band(&st, header, 8).expect("inside a cell");
        assert_eq!(band.cols, (9, 12));
    }

    /// A table nested in a list item or a quote gets a cell band too: its rows are found by their
    /// origins, though the block is the list or the quote.  The band is the clicked cell's alone.
    #[test]
    fn preview_table_cell_band_covers_nested_tables() {
        for src in [
            "- item\n\n  | a | b |\n  |---|---|\n  | 1 | 2 |\n",
            "> | a | b |\n> |---|---|\n> | 1 | 2 |\n",
        ] {
            let st = preview_state(src, 40);
            let (row, text) = st
                .parsed
                .lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .enumerate()
                .find(|(_, t)| t.contains('│') && t.contains('2'))
                .expect("the data row renders");
            // Every glyph on the row is one cell wide, so char columns are cell columns.
            let col_of = |ch| text.chars().position(|c| c == ch).unwrap();
            let band = preview_table_cell_band(&st, row, col_of('2'))
                .unwrap_or_else(|| panic!("{src:?}: no band on {text:?}"));
            assert_eq!(band.lines, (row, row), "{src:?}");
            assert!(
                band.cols.0 > col_of('1') && (band.cols.0..band.cols.1).contains(&col_of('2')),
                "{src:?}: band {:?} on {text:?}",
                band.cols
            );
        }
    }

    /// A Preview click is a screen cell, and the selection column it seeds is a char column:
    /// past two wide glyphs the two differ by two, so `b` (cell 10) must be char 8, not the
    /// `│` at char 10, and the pad cell at cell 6 belongs to the first cell, not the second.
    #[test]
    fn preview_click_maps_screen_cells_past_wide_glyphs() {
        let st = preview_state("| 日本 | ab |\n|---|---|\n| x | y |\n", 40);
        let header = st
            .parsed
            .lines
            .iter()
            .position(|l| l.spans.iter().any(|s| s.content.contains('日')))
            .expect("header line");
        // Cells: │0 ␠1 日2-3 本4-5 ␠6 │7 ␠8 a9 b10 ␠11 │12
        // Chars: │0 ␠1 日2   本3   ␠4 │5 ␠6 a7 b8  ␠9  │10
        let click = |col| rendered_click_to_line_col(&st, col, header, 40);
        assert_eq!(click(10), Some((header, 8)));
        assert_eq!(click(2), Some((header, 2)), "left half of 日");
        assert_eq!(
            click(3),
            Some((header, 3)),
            "right half of 日 lands after it"
        );
        let (_, pad) = click(6).unwrap();
        let band = preview_table_cell_band(&st, header, pad).expect("inside a cell");
        assert_eq!(band.cols, (2, 6), "the first cell's content area");
    }

    /// At a width the list marker's hanging indent leaves no room in, continuation rows start at
    /// the left edge, as the painter draws them: `• ` / `abc` / `def` at three cells, so cell 1 of
    /// the second row is `b`.  The click once padded the row by the unclamped indent.
    #[test]
    fn preview_click_on_a_row_too_narrow_for_its_indent() {
        let st = preview_state("- abcdef\n", 3);
        let click = |col| rendered_click_to_line_col(&st, col, 1, 3).map(|(_, c)| c);
        assert_eq!([click(0), click(1), click(2)], [Some(2), Some(3), Some(4)]);
    }

    /// Hit-testing takes the glyph under either half of a wide glyph, and still declines the
    /// blank cells past the end of the row.
    #[test]
    fn preview_hit_test_covers_both_halves_of_a_wide_glyph() {
        let st = preview_state("日本x\n", 40);
        let hit = |col| rendered_click_to_line_col_on_text(&st, col, 0, 40);
        assert_eq!(hit(0), Some((0, 0)));
        assert_eq!(hit(1), Some((0, 0)), "right half of 日");
        assert_eq!(hit(3), Some((0, 1)), "right half of 本");
        assert_eq!(hit(4), Some((0, 2)));
        assert_eq!(hit(5), None, "past the text");
    }

    /// Preview state with paragraph reflow reconciled, as `App::prepare_viewport` does each frame.
    fn preview_state(src: &str, width: usize) -> EditorState {
        let mut st = EditorState::new(Buffer::from_str(src), theme());
        assert_eq!(st.mode, Mode::Preview);
        st.set_viewport_width(width);
        st.sync_reflow_for_mode();
        st
    }

    /// A revealed nested reflowed paragraph stacks only its own lines, past its block's first:
    /// a click on its second stacked row lands on that source line's char, container indent and
    /// all, and the sibling item below it still maps through its rendered row.
    #[test]
    fn click_in_revealed_nested_flow_maps_to_its_stacked_raw_line() {
        let src = "- one\n- two\n  three\n- four\n";
        let mut st = EditorState::new(Buffer::from_str(src), theme());
        st.mode = Mode::Rendered;
        st.set_viewport_width(80);
        st.sync_reflow_for_mode();
        st.cursor.offset = src.find("two").unwrap();
        st.update_cursor_block();
        st.cursor_block_entered_at = None;
        assert!(st.effective_rows(80).has_reveal());
        // Rows: 0 `• one`, 1 the stack (`- two`, `  three`), 2 `• four`.  Col 3 of the stack's
        // second row is the `h` of `three`.
        let off = rendered_sub_line_to_offset(&st, 1, 1, 3, 80);
        assert_eq!(off, src.find("hree").unwrap());
        // The stack's walk counts both raw rows; `four` stays rendered, its `f` at col 2.
        let reveal = cursor_reveal(&st);
        assert_eq!(revealed_raw_row_count(&st, reveal.as_ref(), 1, 80), Some(2));
        assert_eq!(revealed_raw_row_count(&st, reveal.as_ref(), 2, 80), None);
        let off = rendered_sub_line_to_offset(&st, 2, 0, 2, 80);
        assert_eq!(off, src.find("four").unwrap());
    }

    /// A cursor on a line rendering no row of its own (the blank between an item's paragraphs)
    /// shares the next line's row, and the revealed row paints the cursor's line, not that one:
    /// its wrap count and a click on it must both be the blank line's.
    #[test]
    fn revealed_cursor_row_measures_the_cursors_own_line() {
        let src = format!("- a\n\n  {}\n", "word ".repeat(20));
        let mut st = EditorState::new(Buffer::from_str(&src), theme());
        st.mode = Mode::Rendered;
        st.set_viewport_width(20);
        st.sync_reflow_for_mode();
        st.cursor.offset = 4; // the blank line
        st.update_cursor_block();
        st.cursor_block_entered_at = None;
        assert!(st.cursor_block_revealed());
        let row = crate::editor::state::cursor_rendered_line_idx(&st);
        let reveal = cursor_reveal(&st);
        assert_eq!(
            revealed_raw_row_count(&st, reveal.as_ref(), row, 20),
            Some(1)
        );
        assert_eq!(rendered_sub_line_to_offset(&st, row, 0, 5, 20), 4);
    }
}
