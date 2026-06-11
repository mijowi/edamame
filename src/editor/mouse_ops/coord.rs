use ratatui::text::Line;

use crate::editor::table_edit;
use crate::editor::{EditorState, Mode};
use crate::markdown::table_layout;
use crate::ui::line_render;
use crate::ui::table_view::HEADER_ROWS;

/// Look up the rendered `Line` and the visual column within it that
/// correspond to document-area row `row`.  Accounts for scroll and wrap; the
/// returned visual col is `row - y_of_first_row_of_line`.
pub(super) fn rendered_line_at_row(
    state: &EditorState,
    row: usize,
) -> Option<(Line<'static>, usize)> {
    let lines = &state.parsed.lines;
    if lines.is_empty() {
        return None;
    }
    let (mut line_idx, mut first_sub_row) =
        state.rendered_line_at_visual_row(state.scroll.saturating_add(row), state.viewport_width);
    let mut y = 0usize;
    while let Some(line) = lines.get(line_idx) {
        let rows_used = line_render::visual_rows_for_line(line, state.viewport_width).max(1);
        let visible_rows = rows_used.saturating_sub(first_sub_row).max(1);
        if y < visible_rows {
            return Some((line.clone(), first_sub_row));
        }
        y += visible_rows;
        line_idx += 1;
        first_sub_row = 0;
    }
    None
}

/// True if the span covering char-col `col` in `line` has `modifier` set.
pub(super) fn span_at_col_has_modifier(
    line: &Line<'_>,
    col: usize,
    modifier: ratatui::style::Modifier,
) -> bool {
    let mut walk = 0usize;
    for span in &line.spans {
        let span_len = span.content.chars().count();
        if col < walk + span_len {
            return span.style.add_modifier.contains(modifier);
        }
        walk += span_len;
    }
    false
}

/// Translate a click at `(col, row)` in the document area to a buffer char
/// offset.  Accounts for current scroll and visual-row wrap.
///
/// Returns `None` only when the buffer is empty — all clicks are clamped to
/// the nearest valid position (end of line / end of buffer) so the caller
/// never has to handle "click landed in whitespace past the document".
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
        // Diff mode handles its own clicks via `DiffView` — `mouse_ops`
        // doesn't reach this helper while `Mode::Diff` is active (a
        // future diff-Edit mode would wire cursor placement
        // separately).  Return None so any inadvertent call no-ops
        // cleanly.
        Mode::Diff => None,
    }
}

/// Raw-mode click: walk buffer lines from `state.scroll`, accumulating each
/// line's wrapped visual-row count, and translate the click into a char
/// offset on the appropriate visual sub-row.  Cell-aware so wide chars
/// align and the cursor lands where the user sees it.
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
        let rows = line_render::visual_rows_of_str(&text, width);
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

/// Cell-aware "click landed in this visual row" → char column on the
/// logical line.  Mirrors `state::raw_col_for_visual_cells` but lives here
/// because the public copy belongs to mouse hit-testing.  See that
/// function for the wide-char snap-past rule and forbidden indent zone.
fn char_in_row_at_cell(
    text: &str,
    row: (usize, usize, usize),
    target_cell: usize,
    indent: usize,
    is_last_row: bool,
) -> usize {
    let (start, end, next_start) = row;
    let max_char_in_row = if is_last_row {
        end
    } else {
        next_start.saturating_sub(1).max(start)
    };
    let row_chars = text.chars().skip(start).take(end - start);
    let in_row = line_render::char_idx_at_cell_col(row_chars, target_cell, indent);
    (start + in_row).min(max_char_in_row)
}

/// Rendered/Preview click: walk through rendered lines from `state.scroll`,
/// accumulating each line's visual-row count, and find which rendered line
/// and sub-row the click landed on.  Then map that rendered sub-line back to
/// a source byte using the source map.
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

/// Preview-mode click translator.  Returns the `(rendered_line_idx,
/// char_col)` pair used to seed Preview's `VisualSelection`, applying the
/// same scroll-aware visual-row walk as `rendered_click_to_offset` so
/// wrapped lines (long list items, paragraphs) map clicks to the correct
/// line, and the same per-sub-row wrap layout as
/// `paint_preview_selection` so `char_col` is the cumulative position
/// within the flat rendered line — required for drag-selection across
/// wrapped sub-rows to highlight the correct range.
pub(super) fn rendered_click_to_line_col(
    state: &EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> Option<(usize, usize)> {
    let (idx, sub_row) = walk_rendered_rows(state, row, viewport_width)?;
    let line = state.parsed.lines.get(idx)?;
    let chars: Vec<(char, ratatui::style::Style)> = line
        .spans
        .iter()
        .flat_map(|span| {
            let style = span.style;
            span.content.chars().map(move |c| (c, style))
        })
        .collect();
    if chars.is_empty() {
        return Some((idx, 0));
    }
    let indent = line_render::compute_hanging_indent(line);
    let rows = line_render::visual_rows_of_chars(&chars, viewport_width.max(1), indent);
    let sub = sub_row.min(rows.len().saturating_sub(1));
    let (row_start, row_end, _) = rows
        .get(sub)
        .copied()
        .unwrap_or((0, chars.len(), chars.len()));
    // Continuation rows render with `indent` blank cells of left-padding;
    // subtract them so `col=indent` maps to the first content char of the
    // sub-row, matching `paint_preview_selection`'s x_off calculation.
    let row_indent = if sub == 0 { 0 } else { indent };
    let local_col = col.saturating_sub(row_indent);
    let char_col = (row_start + local_col).min(row_end);
    Some((idx, char_col))
}

/// Walk rendered lines from `state.scroll`, accumulating per-line wrapped
/// visual-row counts (with reveal corrections), and find which
/// `(rendered_line_idx, sub_row_within_line)` the document-relative `row`
/// falls on.  Shared by `rendered_click_to_offset` and
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
    let mut y = 0usize;
    while idx < lines.len() {
        let rows_used = revealed_raw_row_count(state, idx, viewport_width)
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

/// Map `(rendered_line_idx, sub_row_within_line, col)` to a buffer char
/// offset.
///
/// Strategy:
/// 1. Look up the block that produced the rendered line.
/// 2. Compute which raw source line within the block corresponds to this
///    rendered line (skipping the table-top border row when relevant).
/// 3. Within that raw source line, advance `col` chars and convert to a char
///    offset on the rope.
///
/// For blocks with inline formatting (`**bold**` rendering as `bold`), the
/// rendered column may diverge slightly from the raw column.  Given the
/// reveal semantics turn the cursor's line into raw text within
/// `RAW_REVEAL_DELAY`, the click lands at an approximate position that the
/// user can refine with a second click if needed.
pub fn rendered_sub_line_to_offset(
    state: &EditorState,
    rendered_line_idx: usize,
    sub_row_within_line: usize,
    col: usize,
    viewport_width: usize,
) -> usize {
    let buffer_len = state.buffer.len_chars();
    let source = state.buffer.contents();
    let Some(block) = locate_block(state, rendered_line_idx) else {
        return buffer_len;
    };
    let block_text = source
        .get(block.range.start..block.range.end.min(source.len()))
        .unwrap_or("");

    let is_table = table_edit::is_table_block(block_text);
    let raw_line_idx = if is_table {
        table_raw_line_idx(state, &block, block_text)
    } else if state.parsed.is_image_block(block.idx) && !state.parsed.is_mermaid_block(block.idx) {
        // A real `![alt](url)` image reserves many rendered rows for its
        // single source line, but only the placeholder row carries text.
        // Mapping a reserved row through `sub_idx` would index a phantom
        // empty raw line (the block range can absorb a trailing blank) and
        // poison the inline-map cache for an unrelated buffer line, so pin
        // every reserved row to raw line 0.  Mermaid blocks are excluded:
        // their reveal overlay paints the raw source 1:1 onto the reserved
        // rows, so `sub_idx` IS the correct source line there — and the
        // mermaid branch below consumes `line_text` derived from it.
        0
    } else {
        block.sub_idx
    };

    // Blank-line "virtual blocks" have no content.  The renderer produces
    // a single empty line for them; place the cursor at block start.
    if block_text.is_empty() {
        return state.buffer.rope().byte_to_char(block.range.start);
    }

    let (line_byte_start, line_byte_end) = raw_line_byte_range(block_text, raw_line_idx);
    let line_text = &block_text[line_byte_start..line_byte_end];
    let rendered_line = &state.parsed.lines[rendered_line_idx];

    // Mermaid diagram blocks render as an image placeholder + blank
    // reserved rows, but `RenderedView` overlays the raw mermaid source
    // 1:1 on those rows when the cursor is inside the block.  The
    // rendered `Line`s therefore don't reflect what the user clicks
    // on — walk the raw line's own wrap layout and map `col` directly
    // against it.
    //
    // The same shortcut applies to the cursor's own line in any block
    // when the hybrid-edit reveal is active: `RenderedView` paints raw
    // source over that one rendered row, so a click on dropped markers
    // like the `](url)` portion of a link must map against the raw
    // chars the user sees, not the rendered span set.
    //
    // Tables are excluded: even when the cursor's row is "revealed",
    // `RenderedView` keeps the table chrome (pipes and padding) painted
    // for layout, so clicks must continue to use the pipe-aware mapping
    // in the table branch below.
    let revealed_cursor_line = !is_table
        && state.cursor_block_revealed()
        && rendered_line_idx == crate::editor::state::cursor_rendered_line_idx(state);
    if state.parsed.is_mermaid_block(block.idx) || revealed_cursor_line {
        let raw_chars: Vec<(char, ratatui::style::Style)> = line_text
            .chars()
            .map(|c| (c, ratatui::style::Style::default()))
            .collect();
        let viewport = viewport_width.max(1);
        let rows = line_render::visual_rows_of_chars(&raw_chars, viewport, 0);
        let sub = sub_row_within_line.min(rows.len().saturating_sub(1));
        let (start, end, next_start) = rows.get(sub).copied().unwrap_or((0, 0, 0));
        let is_last_row = sub + 1 == rows.len();
        let max_in_row = if is_last_row {
            end
        } else {
            next_start.saturating_sub(1).max(start)
        };
        let row_chars = raw_chars
            .iter()
            .skip(start)
            .take(end - start)
            .map(|(c, _)| *c);
        let in_row = line_render::char_idx_at_cell_col(row_chars, col, 0);
        let raw_col = (start + in_row).min(max_in_row);
        return raw_col_to_buffer_char(state, &block, line_byte_start, line_text, raw_col);
    }

    let raw_col = if is_table && rendered_line.spans.iter().any(|s| s.content.contains('│')) {
        // Tables: rendered cells are padded to layout width, so a simple col
        // → char mapping lands clicks on the wrong cell whenever the
        // rendered cell is wider than its raw counterpart.  Map through the
        // pipe positions instead so the click stays inside the cell the
        // user clicked on.
        let row_width = line_row_width(rendered_line, sub_row_within_line);
        let clamped_col = col.min(row_width);
        table_click_to_raw_col(line_text, rendered_line, clamped_col).unwrap_or(clamped_col)
    } else {
        let buffer_line_idx = state
            .buffer
            .block_line_to_buffer_line(block.range.start, raw_line_idx);
        let stripped = line_text.strip_suffix('\n').unwrap_or(line_text);
        let inline_map = state.inline_map_for(buffer_line_idx, stripped);
        non_table_click_to_raw_col(
            rendered_line,
            line_text,
            col,
            sub_row_within_line,
            viewport_width,
            &inline_map,
        )
    };

    raw_col_to_buffer_char(state, &block, line_byte_start, line_text, raw_col)
}

/// Resolved location of the block that produced a rendered line: byte
/// range in the source, the rendered-line range the block occupies, and
/// the click's offset within those rendered lines.
struct BlockLocation {
    idx: usize,
    range: std::ops::Range<usize>,
    rendered_span: std::ops::Range<usize>,
    sub_idx: usize,
}

/// Look up the source block that owns `rendered_line_idx`.  Returns `None`
/// when the rendered line is past the document's source map (off-by-one
/// during a pending edit, etc.).
fn locate_block(state: &EditorState, rendered_line_idx: usize) -> Option<BlockLocation> {
    let block_start_byte = state
        .parsed
        .source_map
        .original_byte_for_rendered_line(rendered_line_idx)?;
    let idx = state.parsed.source_map.block_for_byte(block_start_byte)?;
    let range = state
        .parsed
        .source_map
        .original_range_for_byte(block_start_byte)?;
    let rendered_span = state
        .parsed
        .source_map
        .rendered_lines_for_byte(block_start_byte);
    let sub_idx = rendered_line_idx.saturating_sub(rendered_span.start);
    Some(BlockLocation {
        idx,
        range,
        rendered_span,
        sub_idx,
    })
}

/// Translate a click on a table block's rendered sub-line to the raw
/// `info.rows[..]` row index.  Classifies the rendered line by its
/// leading box-drawing glyph rather than the alternating-line pattern,
/// because data rows may span multiple rendered lines after cell-wrap.
fn table_raw_line_idx(state: &EditorState, block: &BlockLocation, block_text: &str) -> usize {
    use crate::ui::table_view::TableSubLineKind;
    let block_lines = state
        .parsed
        .lines
        .get(block.rendered_span.start..block.rendered_span.end.min(state.parsed.lines.len()))
        .unwrap_or(&[]);
    let kinds = crate::ui::table_view::classify_table_sub_lines(block_lines);
    match kinds.get(block.sub_idx) {
        Some(TableSubLineKind::TopBorder) | Some(TableSubLineKind::Header { .. }) => 0,
        Some(TableSubLineKind::ThickSeparator) => HEADER_ROWS,
        Some(TableSubLineKind::DataRow { row, .. }) => row + HEADER_ROWS,
        Some(TableSubLineKind::ThinSeparator) => {
            // A separator click snaps to the data row immediately
            // preceding it.  Walk back through `kinds` to find it.
            let row = kinds[..block.sub_idx]
                .iter()
                .rev()
                .find_map(|k| {
                    if let TableSubLineKind::DataRow { row, .. } = k {
                        Some(*row)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            row + HEADER_ROWS
        }
        Some(TableSubLineKind::BottomBorder) | None => {
            // Bottom border or out-of-range — snap to the last data
            // row.  Total data rows = info.rows.len() - HEADER_ROWS
            // (header + alignment).  Tables always have at least one
            // data row for `is_table_block` to be true.
            let last_data = block_text.split('\n').count().saturating_sub(HEADER_ROWS);
            last_data.max(HEADER_ROWS)
        }
    }
}

/// When the reveal is active and `rendered_line_idx` lies inside the
/// cursor's block, returns the wrap count of the raw source line the
/// painter actually paints onto that rendered row.  Returns `None`
/// otherwise — callers fall back to the regular per-line cache.
///
/// Two cases need this correction:
/// - **Mermaid blocks**: `RenderedView` reserves placeholder rendered lines
///   that each report 1 visual row; during reveal the painter overlays raw
///   mermaid source lines that may themselves wrap.
/// - **Non-table cursor line**: `RenderedView` replaces the cursor's
///   rendered line with raw source text.  Inline formatting markers
///   (`**`, `_`, backticks) make the raw line longer than the rendered
///   form, so it can wrap to more visual rows at the same viewport width.
fn revealed_raw_row_count(
    state: &EditorState,
    rendered_line_idx: usize,
    viewport_width: usize,
) -> Option<usize> {
    if !state.cursor_block_revealed() {
        return None;
    }
    let cursor_block_idx = state.cursor_block_idx?;
    let block_lines = state
        .parsed
        .source_map
        .rendered_lines_for_block(cursor_block_idx);
    if !block_lines.contains(&rendered_line_idx) {
        return None;
    }

    let block_start_byte = state
        .parsed
        .source_map
        .original_byte_for_rendered_line(block_lines.start)?;
    let block_range = state
        .parsed
        .source_map
        .original_range_for_byte(block_start_byte)?;
    let source = state.buffer.contents();
    let block_text = source
        .get(block_range.start..block_range.end.min(source.len()))
        .unwrap_or("");

    if state.parsed.is_mermaid_block(cursor_block_idx) {
        let sub = rendered_line_idx - block_lines.start;
        let raw_line = block_text.split('\n').nth(sub).unwrap_or("");
        return Some(
            line_render::visual_rows_of_str(raw_line, viewport_width.max(1))
                .len()
                .max(1),
        );
    }

    // Non-mermaid: only the cursor's own rendered line gets replaced with
    // raw text.  Tables keep their rendered chrome, so skip them.
    let is_table = table_edit::is_table_block(block_text);
    if is_table {
        return None;
    }
    let cursor_line = crate::editor::state::cursor_rendered_line_idx(state);
    if rendered_line_idx != cursor_line {
        return None;
    }

    // Find the raw source line within the block that corresponds to the
    // cursor's rendered line, matching the render loop's logic.
    let sub = rendered_line_idx - block_lines.start;
    let raw_line = block_text.split('\n').nth(sub).unwrap_or("");
    Some(
        line_render::visual_rows_of_str(raw_line, viewport_width.max(1))
            .len()
            .max(1),
    )
}

/// Byte range `[start..end)` within `block_text` of raw line `raw_line_idx`,
/// clamped to the block's last line if `raw_line_idx` exceeds the block.
fn raw_line_byte_range(block_text: &str, raw_line_idx: usize) -> (usize, usize) {
    let mut byte_cursor = 0usize;
    for (i, line) in block_text.split('\n').enumerate() {
        if i == raw_line_idx {
            return (byte_cursor, byte_cursor + line.len());
        }
        byte_cursor += line.len() + 1;
        if byte_cursor >= block_text.len() {
            // Clamp when raw_line_idx points past the block's last line.
            return (byte_cursor.saturating_sub(line.len() + 1), block_text.len());
        }
    }
    (0, block_text.len())
}

/// Non-table click: walk the rendered line's wrap layout to find which
/// sub-row the click landed on, translate the click's cell column into a
/// char position using the cell-aware mapping (wide-char snap-past,
/// hanging-indent forbidden zone), then map that rendered char back to a
/// raw char column on `line_text`.
///
/// The renderer emits a leading prefix (`• ` / `1. ` / `[ ] ` / `▎ ` /
/// heading indent) on lists, tasks, blockquotes and headings; that prefix
/// has no counterpart in pulldown-cmark's `Text` events.  Compare the
/// rendered char count against the map's content count to recover the
/// prefix width, then route clicks on the prefix region into the raw
/// prefix area and clicks past it through the map — so a click on a
/// `**bold**` span inside a list item lands correctly even though the
/// raw `**` markers must be skipped.  Falls back to 1:1 when the prefix
/// width isn't trustworthy (e.g. code blocks pad their lines with
/// trailing spaces).
fn non_table_click_to_raw_col(
    rendered_line: &Line<'_>,
    line_text: &str,
    col: usize,
    sub_row_within_line: usize,
    viewport_width: usize,
    inline_map: &crate::markdown::InlineColMap,
) -> usize {
    let indent = line_render::compute_hanging_indent(rendered_line);
    let rendered_chars: Vec<(char, ratatui::style::Style)> = rendered_line
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
        .collect();
    let viewport = viewport_width.max(1);
    let rows = line_render::visual_rows_of_chars(&rendered_chars, viewport, indent);
    let sub = sub_row_within_line.min(rows.len().saturating_sub(1));
    let (start, end, next_start) = rows.get(sub).copied().unwrap_or((0, 0, 0));
    let row_indent = if sub == 0 { 0 } else { indent };
    let is_last_row = sub + 1 == rows.len();
    let max_in_row = if is_last_row {
        end
    } else {
        next_start.saturating_sub(1).max(start)
    };
    let row_chars = rendered_chars
        .iter()
        .skip(start)
        .take(end - start)
        .map(|(c, _)| *c);
    let in_row = line_render::char_idx_at_cell_col(row_chars, col, row_indent);
    let rendered_idx = (start + in_row).min(max_in_row);

    let actual_rendered_count = rendered_chars.len();
    let map = inline_map.rendered_to_raw_vec();
    let map_content_count = inline_map.rendered_len();
    let raw_content_start = map.first().copied().unwrap_or(0);

    if actual_rendered_count >= map_content_count {
        let prefix_len = actual_rendered_count - map_content_count;
        if prefix_len <= raw_content_start {
            return if rendered_idx < prefix_len {
                rendered_idx.min(raw_content_start)
            } else {
                let content_idx = rendered_idx - prefix_len;
                map.get(content_idx)
                    .copied()
                    .unwrap_or_else(|| line_text.chars().count())
            };
        }
    }
    rendered_idx
}

/// Convert `raw_col` (a char column on `line_text`) to a buffer-wide char
/// offset, clamped to the buffer length.
fn raw_col_to_buffer_char(
    state: &EditorState,
    block: &BlockLocation,
    line_byte_start: usize,
    line_text: &str,
    raw_col: usize,
) -> usize {
    let line_char_count = line_text.chars().count();
    let raw_col = raw_col.min(line_char_count);
    let byte_offset_in_line: usize = line_text.chars().take(raw_col).map(char::len_utf8).sum();
    let byte_in_block = line_byte_start + byte_offset_in_line.min(line_text.len());
    let block_text_len = block.range.end.saturating_sub(block.range.start);
    let absolute_byte = block.range.start + byte_in_block.min(block_text_len);
    let source_len = state.buffer.contents().len();
    state
        .buffer
        .rope()
        .byte_to_char(absolute_byte.min(source_len))
        .min(state.buffer.len_chars())
}

/// Cell-aware mapping from a rendered column to a raw column for table rows.
///
/// Locates the cell the click falls in by walking the rendered line's `│`
/// positions, then maps the click's position *within* the rendered cell to
/// the matching raw cell:
/// - clicks on actual content chars map 1:1 to the raw content char,
/// - clicks on leading padding land on the first raw content char,
/// - clicks on trailing padding land just past the last non-whitespace char
///   in the raw cell so the cursor never jumps into the next cell.
///
/// Returns `None` when the line doesn't parse as a table row (alignment
/// separator, border) — caller falls back to the default char-by-char map.
fn table_click_to_raw_col(
    raw_line: &str,
    rendered_line: &Line<'_>,
    rendered_col: usize,
) -> Option<usize> {
    let raw_pipes = table_layout::raw_pipe_positions(raw_line);
    let rendered_pipes = table_layout::rendered_pipe_positions(rendered_line);
    if raw_pipes.len() < 2 || rendered_pipes.len() != raw_pipes.len() {
        return None;
    }
    let col_count = rendered_pipes.len() - 1;

    // Which cell does `rendered_col` fall in?  Cell `i` spans
    // (rendered_pipes[i] + 1) .. rendered_pipes[i + 1] (content area).
    let cell_idx = (0..col_count)
        .find(|&i| rendered_col < rendered_pipes[i + 1])
        .unwrap_or(col_count - 1);
    let rend_cell_start = rendered_pipes[cell_idx] + 1;
    let rend_cell_end = rendered_pipes[cell_idx + 1];
    let raw_cell_start = raw_pipes[cell_idx] + 1;
    let raw_cell_end = raw_pipes[cell_idx + 1];

    let raw_cell_text: String = raw_line
        .chars()
        .skip(raw_cell_start)
        .take(raw_cell_end - raw_cell_start)
        .collect();

    // Clamp the click into the rendered cell's span so clicks on the opening
    // pipe land at the start of the cell's content.
    let _ = rend_cell_end;
    let clicked = rendered_col.max(rend_cell_start);
    let rend_offset_in_cell = clicked.saturating_sub(rend_cell_start);

    // Partition the raw cell into leading-ws / content / trailing-ws.
    let raw_chars: Vec<char> = raw_cell_text.chars().collect();
    let raw_leading = raw_chars.iter().take_while(|c| c.is_whitespace()).count();
    let raw_trailing = raw_chars
        .iter()
        .rev()
        .take_while(|c| c.is_whitespace())
        .count();
    let content_chars = raw_chars.len().saturating_sub(raw_leading + raw_trailing);

    // The renderer always emits exactly one leading space before the cell
    // content (see `render_table_row`).  A click on that leading space should
    // land on the first raw content char; clicks past the content's last
    // non-whitespace char clamp to "just past last content char" so the
    // cursor never jumps into the next cell via trailing padding.
    let raw_offset_in_cell = if rend_offset_in_cell <= 1 {
        raw_leading
    } else {
        let content_col = rend_offset_in_cell - 1;
        raw_leading + content_col.min(content_chars)
    };

    Some(raw_cell_start + raw_offset_in_cell)
}

/// Width in cells of visual sub-row `sub_row` of the rendered line.  Clicks
/// past the line's content are clamped to this bound before being mapped into
/// the raw source, so the user can click "past the end" and still land at the
/// line's last valid cursor position.
///
/// Currently returns the full character count of the line regardless of
/// sub-row — a conservative upper bound that keeps clicks off the next line.
/// A precise per-row bound would require re-running the line-wrap algorithm
/// here; the conservative bound is correct at the character level and only
/// loses precision for clicks deep in the padding of wrapped lines.
fn line_row_width(line: &Line<'_>, _sub_row: usize) -> usize {
    line.spans.iter().map(|s| s.content.chars().count()).sum()
}
