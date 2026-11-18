//! Rendered row → source position: which buffer offset (or line) a cell on a row of the
//! Preview / Rendered layout shows.  The one mapping behind a mouse click
//! ([`mouse_ops`](crate::editor::mouse_ops) finds the row, then asks here), the Preview scroll
//! anchor (breadcrumb, Go to section, the cursor sync on leaving Preview), and the Rendered
//! scroll clamp, so all of them land on the same position for the same row.

use ratatui::text::Line;

use crate::document::row_map;
use crate::document::wrap;
use crate::editor::{EditorState, Mode};
use crate::markdown::cell_overlay::{self, CellOverlay};
use crate::markdown::table_layout;

/// Map `(rendered_line_idx, sub_row_within_line, col)` to a buffer char offset: locate the
/// block, resolve the wrap to a rendered char of the row, then ask `row_map` which source
/// position that char shows.  Rows painted as raw source (the revealed cursor row, a revealed
/// reflowed paragraph's stacked lines, a diagram's rows) map against the raw line's own wrap
/// instead, and a table row maps its columns through its pipes.  Past the last rendered line:
/// the end of the buffer.
///
/// [`EditorState::char_offset_at_visual_row`] resolves a viewport row as a click at its first
/// cell, so the Preview anchor and the Rendered scroll clamp land exactly where a click would.
pub fn rendered_sub_line_to_offset(
    state: &EditorState,
    rendered_line_idx: usize,
    sub_row_within_line: usize,
    col: usize,
    viewport_width: usize,
) -> usize {
    let buffer_len = state.buffer.len_chars();
    let Some(block) = locate_block(state, rendered_line_idx) else {
        return buffer_len;
    };
    let block_text = source_text(state, &block.range);
    let block_text = block_text.as_str();

    // Virtual blank blocks: place the cursor at block start.
    if block_text.is_empty() {
        return state.buffer.rope().byte_to_char(block.range.start);
    }

    if let Some(hit) = row_map::table_row(&state.parsed, block.idx, block.sub_idx) {
        return table_click_to_offset(
            state,
            &block,
            block_text,
            &hit,
            sub_row_within_line,
            col,
            viewport_width,
        );
    }

    // A reflowed paragraph revealed in Rendered mode shows its raw source lines *stacked*, so
    // `sub_row_within_line` walks those lines' wrap rows.  Find the raw line and wrap sub it
    // lands on, map the cell column within that raw line, and resolve to a buffer offset via the
    // raw line's buffer position — the same shape as the diagram reveal below.  The paragraph
    // may be nested, so the stack is the block's lines `EffectiveRows` names, not all of them.
    // (Preview always shows the flow, which `row_map` maps like any other row.)
    let effective = state.effective_rows(viewport_width);
    if effective
        .block_rendered()
        .is_some_and(|r| r.contains(&rendered_line_idx))
    {
        let stack = effective.raw_lines();
        let block_lines = crate::ui::rendered_view::raw_source_lines(block_text);
        let mut remaining = sub_row_within_line;
        let mut chosen = stack.end.saturating_sub(1).max(stack.start);
        let mut wrap_sub = 0usize;
        for line in stack.clone() {
            let n = effective.raw_wrap_at(line);
            if remaining < n {
                chosen = line;
                wrap_sub = remaining;
                break;
            }
            remaining -= n;
        }
        let raw_line_text = block_lines.get(chosen).copied().unwrap_or("");
        let indent = row_map::revealed_indent(&state.parsed, block.idx, chosen);
        let raw_col = raw_click_col(raw_line_text, indent, wrap_sub, col, viewport_width);
        let first_buf_line = state.buffer.rope().byte_to_line(block.range.start);
        let target = (first_buf_line + chosen).min(state.buffer.line_count().saturating_sub(1));
        return (state.buffer.line_to_char(target) + raw_col).min(buffer_len);
    }

    // Rows the view paints as raw source (a diagram's reserved rows, the cursor's own revealed
    // row) map `col` against the raw line's own wrap layout, since the rendered `Line` isn't
    // what the user sees.  The revealed row paints the cursor's line; a diagram's rows show
    // their source lines 1:1 below any math-preview band, whose own rows resolve to the first
    // line, and above any padding past the source, whose rows resolve to the last.
    let revealed_row = is_revealed_cursor_row(state, block.idx, rendered_line_idx);
    if revealed_row || state.parsed.is_diagram_reveal_block(block.idx) {
        let raw_line_idx = if revealed_row {
            crate::editor::state::cursor_raw_line(state)
        } else {
            row_map::revealed_diagram_line(&state.parsed, block.idx, block.sub_idx)
                .unwrap_or_else(|| row_map::line_for_row(&state.parsed, block.idx, block.sub_idx))
        };
        let (line_byte_start, line_byte_end) = raw_line_byte_range(block_text, raw_line_idx);
        let line_text = &block_text[line_byte_start..line_byte_end];
        let indent = row_map::revealed_indent(&state.parsed, block.idx, raw_line_idx);
        let raw_col = raw_click_col(line_text, indent, sub_row_within_line, col, viewport_width);
        return raw_col_to_buffer_char(state, &block, line_byte_start, line_text, raw_col);
    }

    let pos = rendered_row_raw_pos(
        state,
        &block,
        rendered_line_idx,
        sub_row_within_line,
        col,
        viewport_width,
    );
    let (line_byte_start, line_byte_end) = raw_line_byte_range(block_text, pos.line);
    let line_text = &block_text[line_byte_start..line_byte_end];
    raw_col_to_buffer_char(state, &block, line_byte_start, line_text, pos.col)
}

/// The buffer line [`rendered_sub_line_to_offset`] lands on at `col` 0, without resolving the
/// column.  That needs the block's text, a copy and a line scan costing O(block) for a row deep
/// inside a long block; this costs O(row) and runs on every Preview redraw (the breadcrumb).
/// Rows painted as raw source fall back to the full mapping: they appear only while the cursor
/// block is revealed, or in a diagram's few rows.
pub fn rendered_sub_line_to_line(
    state: &EditorState,
    rendered_line_idx: usize,
    sub_row_within_line: usize,
    viewport_width: usize,
) -> usize {
    let rope = state.buffer.rope();
    let Some(block) = locate_block(state, rendered_line_idx) else {
        return rope.len_lines().saturating_sub(1);
    };
    let start = block.range.start.min(rope.len_bytes());
    let first_line = rope.byte_to_line(start);
    if block.range.is_empty() {
        return first_line;
    }
    let last_line = rope.byte_to_line(
        block
            .range
            .end
            .saturating_sub(1)
            .clamp(start, rope.len_bytes()),
    );
    let line = if let Some(hit) = row_map::table_row(&state.parsed, block.idx, block.sub_idx) {
        hit.line
    } else if shows_raw_source(state, &block, rendered_line_idx, viewport_width) {
        let offset = rendered_sub_line_to_offset(
            state,
            rendered_line_idx,
            sub_row_within_line,
            0,
            viewport_width,
        );
        return rope.char_to_line(offset);
    } else {
        rendered_row_raw_pos(
            state,
            &block,
            rendered_line_idx,
            sub_row_within_line,
            0,
            viewport_width,
        )
        .line
    };
    (first_line + line).min(last_line)
}

/// Whether [`rendered_sub_line_to_offset`] maps this row against raw source rather than through
/// `row_map`: a revealed reflowed paragraph's stacked lines, the revealed cursor row, a diagram.
fn shows_raw_source(
    state: &EditorState,
    block: &BlockLocation,
    rendered_line_idx: usize,
    viewport_width: usize,
) -> bool {
    state
        .effective_rows(viewport_width)
        .block_rendered()
        .is_some_and(|r| r.contains(&rendered_line_idx))
        || is_revealed_cursor_row(state, block.idx, rendered_line_idx)
        || state.parsed.is_diagram_reveal_block(block.idx)
}

/// The block-relative source position a click at `col` on wrap row `sub_row_within_line` of
/// rendered line `rendered_line_idx` shows: the wrap resolved to a rendered char, then `row_map`.
fn rendered_row_raw_pos(
    state: &EditorState,
    block: &BlockLocation,
    rendered_line_idx: usize,
    sub_row_within_line: usize,
    col: usize,
    viewport_width: usize,
) -> row_map::RawPos {
    let rendered_chars = wrap::styled_chars(&state.parsed.lines[rendered_line_idx]);
    let rendered_idx = click_to_rendered_char_idx(
        state.parsed.row_indent(rendered_line_idx),
        &rendered_chars,
        col,
        sub_row_within_line,
        viewport_width,
    );
    row_map::rendered_to_raw_col(&state.parsed, block.idx, block.sub_idx, rendered_idx)
}

/// A click on a table's row: the table row and wrap chunk come from the row's origin
/// ([`row_map::table_row`], which snaps a border or separator onto a table row), the column from
/// the pipe positions.  A border or separator shares its table row's column geometry, so a click
/// on one lands in the cell below or above it, where the cursor then shows.  A click on the
/// cursor's revealed cell maps through the raw text the view paints there
/// ([`overlay_click_to_raw_col`]).  A row whose pipes don't match its source line's (a pipe-less
/// row, a mid-edit line) maps its char 1:1, clamped to the raw line.
fn table_click_to_offset(
    state: &EditorState,
    block: &BlockLocation,
    block_text: &str,
    hit: &row_map::TableRowHit,
    sub_row_within_line: usize,
    col: usize,
    viewport_width: usize,
) -> usize {
    let table_sub = hit.sub;
    let (line_byte_start, line_byte_end) = raw_line_byte_range(block_text, hit.line);
    let line_text = &block_text[line_byte_start..line_byte_end];
    let rendered_line_idx = block.rendered_span.start + block.sub_idx;
    let Some(rendered_line) = state.parsed.lines.get(rendered_line_idx) else {
        return state.buffer.len_chars();
    };
    // Resolve the wrap first (a table wider than the viewport, behind a quote's bar, wraps
    // like any row), then work in the row's cells, which its pipes are measured in.
    let rendered_chars = wrap::styled_chars(rendered_line);
    let rendered_idx = click_to_rendered_char_idx(
        state.parsed.row_indent(rendered_line_idx),
        &rendered_chars,
        col,
        sub_row_within_line,
        viewport_width,
    );
    let clamped_col: usize = rendered_chars[..rendered_idx]
        .iter()
        .map(|&(c, _)| table_layout::char_cells(c))
        .sum();
    let cells_line = if hit.cells {
        Some(rendered_line)
    } else {
        state
            .parsed
            .lines
            .get(block.rendered_span.start + hit.rows.start)
    };
    // Rendered cells are padded to layout width; map through the pipe positions so the click
    // stays inside the clicked cell.
    let raw_col = revealed_cursor_col(state, block, hit)
        .and_then(|cursor_col| {
            overlay_click_to_raw_col(line_text, cursor_col, clamped_col, |c| {
                revealed_cell_overlay(state, block, hit, line_text, rendered_line_idx, c)
            })
        })
        .or_else(|| {
            cells_line.and_then(|line| {
                table_click_to_raw_col(
                    line_text,
                    hit.raw_col,
                    line,
                    clamped_col,
                    table_sub,
                    state.parsed.ref_labels(),
                )
            })
        })
        .unwrap_or(clamped_col);
    raw_col_to_buffer_char(state, block, line_byte_start, line_text, raw_col)
}

/// The cursor's char column on the source line of `hit`, a row of cells, when the view shows
/// that row revealed: [`reveal_shown`], and the cursor on that line of this block.
fn revealed_cursor_col(
    state: &EditorState,
    block: &BlockLocation,
    hit: &row_map::TableRowHit,
) -> Option<usize> {
    if !reveal_shown(state) || !hit.cells {
        return None;
    }
    let (cursor_block, pos) = crate::ui::rendered_view::cursor_block_pos(state)?;
    (cursor_block == block.idx && pos.line == hit.line).then_some(pos.col)
}

/// The overlay the view paints on `rendered_line_idx`, a line of the revealed table row `hit`
/// whose source line is `line_text`, with the cursor at char column `cursor_col` there.  Asked
/// as `RenderedView` asks it: a row that wraps inside its cells takes the wrapped overlay on
/// each of its lines, a one-line row the single-line overlay on the revealed cursor row.
/// `None` where the view reveals the line whole, or paints nothing over it.  Only reached past
/// [`revealed_cursor_col`], which checked [`reveal_shown`].
fn revealed_cell_overlay(
    state: &EditorState,
    block: &BlockLocation,
    hit: &row_map::TableRowHit,
    line_text: &str,
    rendered_line_idx: usize,
    cursor_col: usize,
) -> Option<CellOverlay> {
    let span_start = block.rendered_span.start;
    if let Some(wrapped) = cell_overlay::compute_wrapped_cell_overlay(
        &state.parsed.lines,
        span_start + hit.rows.start..span_start + hit.rows.end,
        line_text,
        hit.raw_col,
        cursor_col,
    ) {
        let sub = rendered_line_idx.checked_sub(wrapped.row_first_line_idx)?;
        return wrapped.subs.into_iter().nth(sub);
    }
    if !is_cursor_reveal_row(state, block.idx, rendered_line_idx) {
        return None;
    }
    let rendered_line = state.parsed.lines.get(rendered_line_idx)?;
    cell_overlay::single_line_cell_overlay(line_text, hit.raw_col, rendered_line, cursor_col)
}

/// Cell column `col` on a revealed table row → char column on `raw_line`, through the overlay
/// `overlay_at(cursor_col)` paints in the cursor's cell: the click lands on the raw char drawn
/// under it, hidden markers and all.  Clamped to the cell's content, so a click on its padding
/// lands on the first char or past the last, as on a formatted cell.  A click past a chunk's
/// text lands just past it only where the overlay then shows the cursor on that chunk (a wrap
/// dropped the space there); where the next chunk would take the cursor, it lands on the last
/// glyph instead.  `None` off the overlay's cell, and on a blank sub-line padding a short
/// wrapped cell: the caller's map through [`table_layout::CellContent`] covers both.
fn overlay_click_to_raw_col(
    raw_line: &str,
    cursor_col: usize,
    col: usize,
    overlay_at: impl Fn(usize) -> Option<CellOverlay>,
) -> Option<usize> {
    let overlay = overlay_at(cursor_col)?;
    if overlay.raw_text.is_empty() || !(overlay.left_pipe..overlay.rendered_end).contains(&col) {
        return None;
    }
    let start = raw_line[..overlay.raw_cell_byte_start.min(raw_line.len())]
        .chars()
        .count();
    let chars: Vec<char> = overlay.raw_text.chars().collect();
    let in_cell = col.saturating_sub(overlay.rendered_start);
    let mut at = wrap::char_idx_at_cell_col(chars.iter().copied(), in_cell, 0).min(chars.len());
    if at == chars.len() {
        let shows_here = overlay_at(start + at).is_some_and(|o| {
            o.raw_cell_byte_start == overlay.raw_cell_byte_start && o.cursor_in_cell.is_some()
        });
        if !shows_here {
            at = table_layout::last_cluster_start(&chars);
        }
    }
    let content = overlay.raw_content;
    Some((start + at).clamp(content.start, content.end))
}

/// The char of `line_text` under cell `col` on wrap row `sub` of the line, laid out as the
/// reveal painter lays out raw source: behind `indent`, the line's `row_map::revealed_indent`.
fn raw_click_col(
    line_text: &str,
    indent: wrap::Indent,
    sub: usize,
    col: usize,
    viewport_width: usize,
) -> usize {
    let (rows, indent) = wrap::revealed_rows_of_str(line_text, indent, viewport_width);
    let sub = sub.min(rows.len().saturating_sub(1));
    let row = rows.get(sub).copied().unwrap_or((0, 0, 0));
    let (start, end, _) = row;
    let is_last_row = sub + 1 == rows.len();
    let max_in_row = wrap::last_col_in_row(row, is_last_row);
    let row_indent = indent.row(sub);
    let row_chars = line_text.chars().skip(start).take(end - start);
    let in_row = wrap::char_idx_at_cell_col(row_chars, col, row_indent);
    (start + in_row).min(max_in_row)
}

/// The block that produced a rendered line: its source byte range, its rendered-line range, and
/// the clicked line's offset within that range.
pub(crate) struct BlockLocation {
    pub(crate) idx: usize,
    pub(crate) range: std::ops::Range<usize>,
    pub(crate) rendered_span: std::ops::Range<usize>,
    pub(crate) sub_idx: usize,
}

/// `None` when the rendered line is past the source map (e.g. during a pending edit).
pub(crate) fn locate_block(state: &EditorState, rendered_line_idx: usize) -> Option<BlockLocation> {
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

/// Whether `rendered_line_idx` is the revealed cursor row of block `block_idx`, which paints
/// the cursor's raw line rather than the line its origin names.  Diagram blocks paint every row
/// 1:1 from its origin, and a row that never de-renders (a code body) stays rendered, so
/// neither counts; nor does any row in Preview, which never reveals.
fn is_revealed_cursor_row(state: &EditorState, block_idx: usize, rendered_line_idx: usize) -> bool {
    reveal_shown(state) && is_cursor_reveal_row(state, block_idx, rendered_line_idx)
}

/// Whether the view shows the cursor block revealed: Rendered mode, the reveal on.
fn reveal_shown(state: &EditorState) -> bool {
    state.mode == Mode::Rendered && state.cursor_block_revealed()
}

/// [`is_revealed_cursor_row`] without the [`reveal_shown`] gate, for a caller that checked it.
fn is_cursor_reveal_row(state: &EditorState, block_idx: usize, rendered_line_idx: usize) -> bool {
    let block_start = state
        .parsed
        .source_map
        .rendered_lines_for_block(block_idx)
        .start;
    !state.parsed.is_diagram_reveal_block(block_idx)
        && rendered_line_idx == crate::editor::state::cursor_rendered_line_idx(state)
        && row_map::reveals(
            &state.parsed,
            block_idx,
            rendered_line_idx.saturating_sub(block_start),
        )
}

/// Byte range within `block_text` of raw line `raw_line_idx`, clamped to the block's last line.
fn raw_line_byte_range(block_text: &str, raw_line_idx: usize) -> (usize, usize) {
    let mut byte_cursor = 0usize;
    for (i, line) in block_text.split('\n').enumerate() {
        if i == raw_line_idx {
            return (byte_cursor, byte_cursor + line.len());
        }
        byte_cursor += line.len() + 1;
        if byte_cursor >= block_text.len() {
            return (byte_cursor.saturating_sub(line.len() + 1), block_text.len());
        }
    }
    (0, block_text.len())
}

/// Which char of the *rendered* line the click at cell `col` on wrap row `sub_row_within_line`
/// landed on.  The one shared walk over [`wrap`]'s geometry for every row `row_map`
/// maps.
fn click_to_rendered_char_idx(
    indent: wrap::Indent,
    rendered_chars: &[(char, ratatui::style::Style)],
    col: usize,
    sub_row_within_line: usize,
    viewport_width: usize,
) -> usize {
    let viewport = viewport_width.max(1);
    let rows = wrap::visual_rows_of_chars(rendered_chars, viewport, indent);
    let sub = sub_row_within_line.min(rows.len().saturating_sub(1));
    let row = rows.get(sub).copied().unwrap_or((0, 0, 0));
    let (start, end, _) = row;
    let row_indent = indent.at(viewport).row(sub);
    let is_last_row = sub + 1 == rows.len();
    let max_in_row = wrap::last_col_in_row(row, is_last_row);
    let row_chars = rendered_chars
        .iter()
        .skip(start)
        .take(end - start)
        .map(|(c, _)| *c);
    let in_row = wrap::char_idx_at_cell_col(row_chars, col, row_indent);
    (start + in_row).min(max_in_row)
}

/// Char column on `line_text` → buffer-wide char offset, clamped to the buffer.
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
    let rope = state.buffer.rope();
    rope.byte_to_char(absolute_byte.min(rope.len_bytes()))
        .min(state.buffer.len_chars())
}

/// The buffer's text over byte `range`, clamped to the buffer; empty when the range doesn't fall
/// on char boundaries (a stale range during a pending edit).  Copies only the range, not the
/// whole document.
pub(crate) fn source_text(state: &EditorState, range: &std::ops::Range<usize>) -> String {
    let rope = state.buffer.rope();
    let end = range.end.min(rope.len_bytes());
    rope.get_byte_slice(range.start.min(end)..end)
        .map(|slice| slice.to_string())
        .unwrap_or_default()
}

/// Rendered column → raw column for a table row, keyed by the cell the click falls in (found
/// by pipe positions).  Leading padding lands on the first content char; trailing padding
/// clamps just past the chunk's last char so the cursor never jumps into the next cell.
///
/// Content columns go through the cell's [`CellContent`](table_layout::CellContent), so a click
/// inside a cell with hidden inline markers (`` `code` ``, `**bold**`, a link) lands on the glyph
/// under the cursor, and wrap chunks are those `render_table_row` drew, even on continuation
/// sub-lines.  `labels` are the document's, so a reference link collapses as it renders.
///
/// Raw cells are [`table_layout::paired_raw_cells`] from `content_col`, the row's content
/// column, so a container prefix, missing edge pipes and a row with more cells than the header
/// all pair cell for cell.  A click on an empty cell the renderer pads a short row with lands at
/// the row's end (trailing whitespace excluded): past a closing pipe, where typing adds that
/// cell, and the cursor shows in the clicked one; without one, typing there extends the last
/// cell, where the cursor shows ([`table_layout::drawn_cell_at`]).
///
/// `sub` is the wrap-chunk index of the clicked sub-line within its logical row.  `None` when
/// the line isn't a row of cells (separator, border); the caller falls back to the char-by-char
/// map.
fn table_click_to_raw_col(
    raw_line: &str,
    content_col: usize,
    rendered_line: &Line<'_>,
    rendered_col: usize,
    sub: usize,
    labels: &crate::markdown::RefLabels,
) -> Option<usize> {
    let rendered_pipes = table_layout::rendered_pipe_cells(rendered_line);
    let col_count = rendered_pipes.len().saturating_sub(1);
    let raw_cells = table_layout::paired_raw_cells(raw_line, content_col, col_count)?;

    let cell_idx = (0..col_count)
        .find(|&i| rendered_col < rendered_pipes[i + 1])
        .unwrap_or(col_count - 1);
    let Some(raw_range) = raw_cells.get(cell_idx) else {
        return Some(raw_line.trim_end().chars().count());
    };
    let rend_cell_start = rendered_pipes[cell_idx] + 1;
    let rend_cell_end = rendered_pipes[cell_idx + 1];
    let raw_cell_start = raw_range.start;
    let raw_chars: Vec<char> = raw_line.chars().collect();
    let raw_cell = &raw_chars[raw_range.clone()];

    let rend_offset_in_cell = rendered_col
        .max(rend_cell_start)
        .saturating_sub(rend_cell_start);
    // A rendered cell is `│` + space + content + space (see `render_table_row`).
    let cell_width = rend_cell_end.saturating_sub(rend_cell_start + 2);
    let cell = table_layout::CellContent::new(raw_cell, cell_width, labels);
    let rendered_to_raw = cell.map.rendered_to_raw_vec();
    let raw_content_col = |rendered: usize| rendered_to_raw[rendered.min(cell.map.rendered_len())];

    // Blank padding sub-lines of a short cell map to the end of its content.
    let (chunk_start, chunk_text) = cell
        .chunks
        .get(sub)
        .map(|(start, text)| (*start, text.as_str()))
        .unwrap_or((cell.map.rendered_len(), ""));

    let raw_offset_in_cell = if rend_offset_in_cell <= 1 {
        cell.leading + raw_content_col(chunk_start)
    } else {
        // The click is a screen cell; a wide glyph before it spans two.
        let content_col =
            wrap::char_idx_at_cell_col(chunk_text.chars(), rend_offset_in_cell - 1, 0);
        cell.leading + raw_content_col(chunk_start + content_col.min(chunk_text.chars().count()))
    };

    Some(raw_cell_start + raw_offset_in_cell.min(raw_cell.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::document::Buffer;
    use crate::markdown::RefLabels;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    /// Preview state with paragraph reflow reconciled, as `App::prepare_viewport` does each frame.
    fn preview_state(src: &str, width: usize) -> EditorState {
        let mut st = EditorState::new(Buffer::from_str(src), theme());
        assert_eq!(st.mode, Mode::Preview);
        st.set_viewport_width(width);
        st.sync_reflow_for_mode();
        st
    }

    /// A click is a screen cell: a wide glyph in the cell to the left moves the later cells two
    /// columns per glyph, and a click on a glyph's right half lands after it.
    #[test]
    fn table_click_maps_screen_cells_past_wide_glyphs() {
        let raw = "| 日本 | ab |";
        // Cells: │0 ␠1 日2-3 本4-5 ␠6 │7 ␠8 a9 b10 ␠11 │12
        let line = Line::from("│ 日本 │ ab │");
        let raw_col = |c: char| raw.chars().position(|x| x == c).unwrap();
        assert_eq!(
            table_click_to_raw_col(raw, 0, &line, 10, 0, &RefLabels::default()),
            Some(raw_col('b'))
        );
        assert_eq!(
            table_click_to_raw_col(raw, 0, &line, 9, 0, &RefLabels::default()),
            Some(raw_col('a'))
        );
        assert_eq!(
            table_click_to_raw_col(raw, 0, &line, 2, 0, &RefLabels::default()),
            Some(raw_col('日'))
        );
        assert_eq!(
            table_click_to_raw_col(raw, 0, &line, 3, 0, &RefLabels::default()),
            Some(raw_col('本'))
        );
    }

    /// A click on a reflowed paragraph maps through the block-wide flow, not a single raw line:
    /// "one\ntwo\nthree" renders as "one two three" on one row, and a click on the third word
    /// must resolve to that word's offset in the source, not somewhere inside the first line.
    #[test]
    fn click_in_reflowed_paragraph_maps_across_source_lines() {
        let state = preview_state("one\ntwo\nthree\n", 80);
        // Rendered "one two three": col 8 is the 't' of "three".  In the source
        // (o0 n1 e2 \n3 t4 w5 o6 \n7 t8 …) that same 't' is char 8.
        let off = rendered_sub_line_to_offset(&state, 0, 0, 8, 80);
        assert_eq!(off, 8);
        assert_eq!(state.buffer.contents().chars().nth(off), Some('t'));

        // The 'w' of "two" (rendered col 5) is source char 5.
        let off_two = rendered_sub_line_to_offset(&state, 0, 0, 5, 80);
        assert_eq!(off_two, 5);
        assert_eq!(state.buffer.contents().chars().nth(off_two), Some('w'));
    }

    /// In Rendered mode with the block revealed, the reflowed paragraph shows its raw lines
    /// stacked, so a click on the third stacked row must resolve to that source line — not through
    /// the collapsed flow.
    #[test]
    fn click_in_revealed_reflowed_block_maps_to_stacked_raw_line() {
        let mut st = EditorState::new(Buffer::from_str("one\ntwo\nthree\n"), theme());
        st.mode = Mode::Rendered;
        st.set_viewport_width(80);
        st.sync_reflow_for_mode(); // reflow is default-on; reconcile the parse as a frame would
        st.cursor.offset = 0;
        st.update_cursor_block();
        st.cursor_block_entered_at = None; // skip the reveal delay
        assert!(
            st.cursor_block_revealed(),
            "block must reveal (no delay pending)"
        );
        // Stacked rows: 0 = "one", 1 = "two", 2 = "three".  Row 2, col 0 → start of "three".
        let off = rendered_sub_line_to_offset(&st, 0, 2, 0, 80);
        assert_eq!(off, 8);
        assert!(st.buffer.contents()[st.buffer.rope().char_to_byte(off)..].starts_with("three"));
        // Col 2 within that row → the second 'r' of "three".
        let off_col = rendered_sub_line_to_offset(&st, 0, 2, 2, 80);
        assert_eq!(off_col, 10);
    }

    /// A hard break makes a paragraph render one row per source line (it no longer reflows), so
    /// the reflow branch must not fire and a click on a later line resolves to that line's source.
    /// "one two  \nthree" renders as "one two" / "three"; a click on the second line's 't' must
    /// land on 'three' (source char 10), not char 0.
    #[test]
    fn click_in_hard_break_paragraph_stays_per_source_line() {
        let state = preview_state("one two  \nthree\n", 80);
        assert_eq!(
            row_map::stacked_lines(&state.parsed, 0, 0),
            None,
            "a hard-break paragraph is multi-line and must not take the reflow path",
        );
        let off = rendered_sub_line_to_offset(&state, 1, 0, 0, 80);
        assert_eq!(
            state.buffer.contents().chars().nth(off),
            Some('t'),
            "click on the second segment must land on 'three', got offset {off}",
        );
    }

    /// A click past a chunk's text in a revealed cell lands just past it where the cursor
    /// would still show on that chunk (the last one), and on its last glyph where the next
    /// chunk would take the cursor (a wide glyph wrapped early).  A click on the closing
    /// pipe is off the overlay's cell.
    #[test]
    fn overlay_click_past_a_chunk_lands_where_the_cursor_shows() {
        // Content area of six cells, from cell 1 to the closing pipe at 7.
        let line = Line::from("│ abcd │");
        let click = |raw: &str, cursor: usize, col: usize| {
            overlay_click_to_raw_col(raw, cursor, col, |c| {
                cell_overlay::single_line_cell_overlay(raw, 0, &line, c)
            })
        };

        // Cell ` abcdefghi` chunks as ` abcde` | `fghi`; the cursor is on `g`.
        let raw = "| abcdefghi|";
        let g = raw.find('g').unwrap();
        let past_i = raw.find('i').unwrap() + 1;
        assert_eq!(click(raw, g, 5), Some(past_i));
        assert_eq!(click(raw, g, 6), Some(past_i));
        assert_eq!(click(raw, g, 7), None);

        // Cell ` abcd日fg` chunks as ` abcd` | `日fg`: `日` doesn't fit beside ` abcd`.  The
        // cursor is on `b`; the cursor past `d` would show in the second chunk, so a click past
        // `d` lands on it, as a click on it does.
        let raw = "| abcd日fg|";
        let b = raw.find('b').unwrap();
        let d = raw.find('d').unwrap();
        assert_eq!(click(raw, b, 5), Some(d));
        assert_eq!(click(raw, b, 6), Some(d));
    }
}
