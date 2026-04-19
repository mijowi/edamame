use ratatui::{
    buffer::Buffer as TuiBuf,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::StatefulWidget,
};

use crate::config::Theme;
use crate::document::detect_setext;
use crate::editor::table_edit;
use crate::editor::EditorState;

use super::line_render::{render_line, render_line_with_cursor};

/// State for the `RenderedView` widget.
///
/// Owned by `EditorViewState`; updated every frame from `EditorState`.
#[derive(Debug, Default)]
pub struct RenderedViewState {
    /// First visible rendered line (scroll offset).
    pub scroll: usize,
}

/// Hybrid rendered/raw editing view.
///
/// Every rendered block is shown as styled Markdown EXCEPT the block that
/// contains the cursor, which is replaced by the raw source text with an
/// inline cursor.
pub struct RenderedView<'a> {
    pub state: &'a EditorState,
    pub theme: &'a Theme,
}

impl<'a> StatefulWidget for RenderedView<'a> {
    type State = RenderedViewState;

    fn render(self, area: Rect, buf: &mut TuiBuf, view_state: &mut Self::State) {
        if area.height == 0 {
            return;
        }

        let height = area.height as usize;
        let editor = self.state;
        let cursor_offset = editor.cursor.offset;
        let cursor_byte = editor.buffer.rope().char_to_byte(cursor_offset);

        // Find which rendered lines belong to the cursor's source block.
        let cursor_block_lines = editor
            .parsed
            .source_map
            .rendered_lines_for_byte(cursor_byte);

        // The cursor block's OWN rendered lines (before gap blanks).
        let cursor_block_idx = editor
            .parsed
            .source_map
            .block_for_byte(cursor_byte)
            .unwrap_or(0);
        let cursor_block_own = editor.parsed.block_own_line_count(cursor_block_idx);

        // Get the raw source text for the cursor's block.
        let raw_block_source: String = editor
            .parsed
            .source_map
            .original_range_for_byte(cursor_byte)
            .map(|r| {
                let source = editor.buffer.contents();
                let end = r.end.min(source.len());
                source[r.start..end].to_owned()
            })
            .unwrap_or_default();

        // Split raw source into lines.
        let raw_lines: Vec<&str> = raw_source_lines(&raw_block_source);

        // Find where the cursor is within the raw block.
        let (cursor_raw_line, cursor_col) =
            cursor_position_in_block(editor, cursor_byte, &raw_block_source);

        // Map the cursor's raw source line to a rendered line within the block.
        // For tables the rendered layout is: top border, header, thick
        // separator (alignment row), then (data row, thin separator)* ending
        // with a data row and finally the bottom border.  Raw line 0 (header)
        // → sub 1; raw line 1 (alignment) → sub 2 (the thick separator);
        // raw line r ≥ 2 (data) → sub 2r − 1.  We must never replace a border
        // or separator line with raw text.
        let is_table = table_edit::is_table_block(&raw_block_source);
        let is_setext = detect_setext(&raw_block_source).is_some();
        let cursor_in_block = if is_table && cursor_block_own >= 3 {
            let last_replaceable = cursor_block_own.saturating_sub(2);
            let sub = match cursor_raw_line {
                0 => 1,
                1 => 2,
                r => 2 * r - 1,
            };
            sub.min(last_replaceable)
        } else {
            cursor_raw_line.min(cursor_block_own.saturating_sub(1))
        };
        let cursor_rendered_line = cursor_block_lines.start + cursor_in_block;

        // Determine the scroll offset; sync from editor state.
        view_state.scroll = editor.scroll;
        let scroll = view_state.scroll;

        // Jitter suppression: if the cursor only recently moved to this line,
        // keep showing the block as rendered until the reveal delay has elapsed.
        let reveal_raw = editor.cursor_block_revealed();

        let cursor_indicator_style = Style::default().add_modifier(Modifier::REVERSED);

        let total_rendered = editor.parsed.lines.len();
        // Long-line wrapping is enabled in rendered-edit mode.
        let wrap = true;

        // Selection: compute the selected raw byte range once; per-line overlay
        // logic will intersect it with each line's byte range.
        let selection_bytes = editor.selection.map(|s| {
            let (sa, sb) = s.range();
            let rope = editor.buffer.rope();
            (rope.char_to_byte(sa), rope.char_to_byte(sb))
        });
        let block_range_for_cursor = editor
            .parsed
            .source_map
            .original_range_for_byte(cursor_byte);

        // Walk rendered lines from scroll offset. For each line, render it
        // normally EXCEPT cursor_rendered_line, which is shown as raw text.
        let mut virtual_idx = scroll;
        let mut vis_y: usize = 0;
        while vis_y < height {
            if virtual_idx >= total_rendered {
                break;
            }

            let rows_used;
            // Setext headings reveal all of their raw lines (the title and
            // the `===` / `---` underline) at once, on their corresponding
            // rendered positions — not just the single line the cursor is on.
            let in_cursor_block =
                virtual_idx >= cursor_block_lines.start && virtual_idx < cursor_block_lines.end;
            if reveal_raw && is_setext && in_cursor_block {
                let sub = virtual_idx - cursor_block_lines.start;
                let raw_text = raw_lines.get(sub).copied().unwrap_or("");
                let cursor_on_this = cursor_raw_line == sub;
                let sel_cols = selection_bytes.and_then(|(sa, sb)| {
                    let block_start = block_range_for_cursor.as_ref()?.start;
                    let raw_line_start_in_block = raw_line_byte_start(&raw_block_source, sub);
                    let raw_line_start_abs = block_start + raw_line_start_in_block;
                    let raw_line_end_abs = raw_line_start_abs + raw_text.len();
                    let start_byte = sa.max(raw_line_start_abs).min(raw_line_end_abs);
                    let end_byte = sb.max(raw_line_start_abs).min(raw_line_end_abs);
                    if start_byte >= end_byte {
                        return None;
                    }
                    let start_col = raw_text[..start_byte - raw_line_start_abs].chars().count();
                    let end_col = raw_text[..end_byte - raw_line_start_abs].chars().count();
                    Some((start_col, end_col))
                });
                let styled = make_raw_line_with_selection(
                    raw_text,
                    if cursor_on_this {
                        Some(cursor_col)
                    } else {
                        None
                    },
                    sel_cols,
                    self.theme,
                );
                rows_used = render_line(&styled, area, buf, vis_y as u16, wrap) as usize;
            } else if reveal_raw && virtual_idx == cursor_rendered_line {
                let raw_text = raw_lines.get(cursor_raw_line).copied().unwrap_or("");
                // Prefer cell-scoped reveal for table rows — replace only the
                // active cell's content area with raw text, keeping the box-
                // drawing borders and neighbouring cells rendered.
                let cell_overlay = if is_table {
                    editor
                        .parsed
                        .lines
                        .get(virtual_idx)
                        .and_then(|line| compute_cell_overlay(raw_text, line, cursor_col))
                } else {
                    None
                };
                if let Some(overlay) = cell_overlay {
                    let line = &editor.parsed.lines[virtual_idx];
                    rows_used = render_line(line, area, buf, vis_y as u16, wrap) as usize;

                    // Compute selection highlight inside this cell.  The cell's
                    // absolute byte range is [cell_byte_start, cell_byte_end);
                    // intersect with the selection and map back to char cols
                    // within `overlay.raw_text`.
                    let sel_in_cell = selection_bytes.and_then(|(sa, sb)| {
                        let block_start = block_range_for_cursor.as_ref()?.start;
                        let raw_line_start_in_block =
                            raw_line_byte_start(&raw_block_source, cursor_raw_line);
                        let cell_byte_start =
                            block_start + raw_line_start_in_block + overlay.raw_cell_byte_start;
                        let cell_byte_end = cell_byte_start + overlay.raw_text.len();
                        let lo = sa.max(cell_byte_start).min(cell_byte_end);
                        let hi = sb.max(cell_byte_start).min(cell_byte_end);
                        if lo >= hi {
                            return None;
                        }
                        let start_col = overlay.raw_text[..lo - cell_byte_start].chars().count();
                        let end_col = overlay.raw_text[..hi - cell_byte_start].chars().count();
                        Some((start_col, end_col))
                    });
                    overlay_raw_cell(buf, area, vis_y as u16, &overlay, sel_in_cell, self.theme);
                } else {
                    // Fall back to full row-reveal (non-table blocks, or when
                    // raw cell content won't fit in the rendered cell width).
                    // Compute selection cols within the raw line (if any).
                    let sel_cols = selection_bytes.and_then(|(sa, sb)| {
                        let block_start = block_range_for_cursor.as_ref()?.start;
                        let raw_line_start_in_block =
                            raw_line_byte_start(&raw_block_source, cursor_raw_line);
                        let raw_line_start_abs = block_start + raw_line_start_in_block;
                        let raw_line_end_abs = raw_line_start_abs + raw_text.len();
                        let start_byte = sa.max(raw_line_start_abs).min(raw_line_end_abs);
                        let end_byte = sb.max(raw_line_start_abs).min(raw_line_end_abs);
                        if start_byte >= end_byte {
                            return None;
                        }
                        let start_col = raw_text[..start_byte - raw_line_start_abs].chars().count();
                        let end_col = raw_text[..end_byte - raw_line_start_abs].chars().count();
                        Some((start_col, end_col))
                    });
                    let styled = make_raw_line_with_selection(
                        raw_text,
                        Some(cursor_col),
                        sel_cols,
                        self.theme,
                    );
                    rows_used = render_line(&styled, area, buf, vis_y as u16, wrap) as usize;
                }
            } else if !reveal_raw && virtual_idx == cursor_rendered_line {
                // Still in jitter delay: show the rendered version with a cursor indicator
                // at the cursor's column so there is no visible column-jump when it reveals.
                if let Some(line) = editor.parsed.lines.get(virtual_idx) {
                    // Raw col → rendered col isn't 1:1 for table rows: padded
                    // cells mean the cursor column shifts.  Walk the pipe
                    // positions to place the jitter-delay indicator at the
                    // same visual col the cell overlay will use on reveal, so
                    // the cursor doesn't jump when the delay elapses.
                    let raw_text = raw_lines.get(cursor_raw_line).copied().unwrap_or("");
                    let visual_col = if is_table {
                        table_raw_col_to_rendered_col(raw_text, line, cursor_col)
                            .unwrap_or(cursor_col)
                    } else {
                        cursor_col
                    };
                    rows_used = render_line_with_cursor(
                        line,
                        area,
                        buf,
                        vis_y as u16,
                        wrap,
                        Some((visual_col, cursor_indicator_style)),
                    ) as usize;
                } else {
                    rows_used = 1;
                }
            } else {
                // Normal rendered line.
                if let Some(line) = editor.parsed.lines.get(virtual_idx) {
                    rows_used = render_line(line, area, buf, vis_y as u16, wrap) as usize;
                } else {
                    break;
                }
            }

            // Paint the selection overlay across the line's visual rows if
            // the line's block is part of the active selection and this is
            // NOT the cursor's raw-displayed line (that line was painted by
            // `make_raw_line_with_selection` and must not be re-painted).
            if let Some((sa, sb)) = selection_bytes {
                let setext_revealed = reveal_raw && is_setext && in_cursor_block;
                if !(reveal_raw && virtual_idx == cursor_rendered_line) && !setext_revealed {
                    paint_selection_overlay(
                        editor,
                        buf,
                        area,
                        vis_y as u16,
                        rows_used as u16,
                        virtual_idx,
                        sa,
                        sb,
                        self.theme,
                    );
                }
            }

            vis_y += rows_used.max(1);
            virtual_idx += 1;
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a `Line` showing `raw_text` with a block cursor at `cursor_col`.
///
/// If `cursor_col` is `None`, no cursor is drawn (other lines of the block).
#[cfg(test)]
fn make_raw_line(raw_text: &str, cursor_col: Option<usize>, theme: &Theme) -> Line<'static> {
    make_raw_line_with_selection(raw_text, cursor_col, None, theme)
}

/// Variant of [`make_raw_line`] that also paints `selection_cols` with the
/// theme's selection background.  `selection_cols` is a `[start, end)` range
/// in char columns within `raw_text`.
fn make_raw_line_with_selection(
    raw_text: &str,
    cursor_col: Option<usize>,
    selection_cols: Option<(usize, usize)>,
    theme: &Theme,
) -> Line<'static> {
    let cursor_style = Style::default().add_modifier(Modifier::REVERSED);
    let sel_style = theme.selection;
    let chars: Vec<char> = raw_text.chars().collect();
    let total = chars.len();

    // Always emit one span per char so per-char styling stays predictable when
    // cursor and selection overlap.  The runs of same-style chars don't need to
    // be coalesced — ratatui's Line works fine with short spans.
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(total + 1);
    for (i, ch) in chars.iter().enumerate() {
        let mut style = theme.normal;
        if matches!(selection_cols, Some((s, e)) if i >= s && i < e) {
            style = style.patch(sel_style);
        }
        if cursor_col == Some(i) {
            style = cursor_style;
        }
        spans.push(Span::styled(ch.to_string(), style));
    }

    // Cursor past end-of-line: append a styled space so the cursor still shows.
    if let Some(col) = cursor_col {
        if col >= total {
            spans.push(Span::styled(" ".to_string(), cursor_style));
        }
    }
    Line::from(spans)
}

/// Byte offset within `block_source` where raw line `line_idx` starts.
fn raw_line_byte_start(block_source: &str, line_idx: usize) -> usize {
    let mut byte = 0usize;
    for (i, line) in block_source.split('\n').enumerate() {
        if i == line_idx {
            return byte;
        }
        byte += line.len() + 1;
    }
    block_source.len()
}

/// Post-render pass: paint the theme's selection background on top of the
/// rendered cells for a given rendered line, if that line's block is part of
/// the active selection.
///
/// Computes the raw byte range of *this specific rendered line* within its
/// block (by splitting the block's raw text on newlines), intersects with the
/// selection byte range, and highlights only the rendered cols that
/// correspond to selected bytes.  Falls back to "whole line" highlight for
/// blocks where the per-line mapping can't be determined cleanly.
fn paint_selection_overlay(
    editor: &EditorState,
    buf: &mut TuiBuf,
    area: Rect,
    y_start: u16,
    rows_used: u16,
    rendered_line_idx: usize,
    sel_start_byte: usize,
    sel_end_byte: usize,
    theme: &Theme,
) {
    let Some(block_byte) = editor
        .parsed
        .source_map
        .original_byte_for_rendered_line(rendered_line_idx)
    else {
        return;
    };
    let Some(block_range) = editor.parsed.source_map.original_range_for_byte(block_byte) else {
        return;
    };
    // Does the selection touch this block at all?
    if block_range.end <= sel_start_byte || block_range.start >= sel_end_byte {
        return;
    }

    // Figure out which RAW line within the block this rendered line maps to.
    // For tables, the renderer prepends a top border and interleaves the
    // alignment row as a box-drawing separator, so the mapping shifts.  For
    // other blocks that produce one rendered line per raw line (code blocks,
    // lists where each item is a single-line paragraph), it's 1:1.
    let source = editor.buffer.contents();
    let block_text = &source[block_range.start..block_range.end.min(source.len())];
    let rendered_span = editor
        .parsed
        .source_map
        .rendered_lines_for_byte(block_range.start);
    let sub_idx_in_block = rendered_line_idx.saturating_sub(rendered_span.start);
    let is_table = table_edit::is_table_block(block_text);
    let raw_line_idx = if is_table {
        // Table rendered layout: sub 0 = top border, sub 1 = header,
        // sub 2 = thick separator (= alignment raw row), then data rows at
        // odd sub indexes ≥ 3, thin separators at even sub indexes between
        // them, and the final sub = bottom border.  Only header and data
        // rows map to raw lines that can carry selection highlighting.
        let own_count = editor.parsed.block_own_line_count(
            editor
                .parsed
                .source_map
                .block_for_byte(block_range.start)
                .unwrap_or(0),
        );
        if sub_idx_in_block == 0 || sub_idx_in_block + 1 >= own_count {
            return;
        }
        match sub_idx_in_block {
            1 => 0,
            n if n >= 3 && n % 2 == 1 => (n + 1) / 2,
            _ => return,
        }
    } else {
        sub_idx_in_block
    };

    // Byte range of the raw line within the block's source text.
    let raw_lines: Vec<&str> = block_text.split('\n').collect();
    if raw_line_idx >= raw_lines.len() {
        // Out-of-range raw line — no highlight rather than a speculative one.
        return;
    }
    let raw_line = raw_lines[raw_line_idx];
    let raw_line_start = raw_line_byte_start(block_text, raw_line_idx);
    let raw_line_start_abs = block_range.start + raw_line_start;
    let raw_line_end_abs = raw_line_start_abs + raw_line.len();

    // Selection's intersection with this raw line (in absolute bytes).
    let line_sel_start = sel_start_byte.max(raw_line_start_abs);
    let line_sel_end = sel_end_byte.min(raw_line_end_abs);
    if line_sel_start >= line_sel_end {
        // Selection doesn't actually cover any bytes on THIS rendered line,
        // even though it covers the block — nothing to paint.
        return;
    }

    // Raw col range within the raw line.
    let start_raw_col = raw_line[..line_sel_start - raw_line_start_abs]
        .chars()
        .count();
    let end_raw_col = raw_line[..line_sel_end - raw_line_start_abs]
        .chars()
        .count();

    // Map raw cols to rendered cols.  Best-effort: 1:1 for non-table
    // non-task-list lines (paragraph, heading, code block line).  Task items
    // shift left by the list-marker length.  Tables go cell-by-cell via
    // pipe positions.
    let Some(line) = editor.parsed.lines.get(rendered_line_idx) else {
        return;
    };
    let (rend_start, rend_end) = if is_table {
        // Pipe counts disagreeing usually means the "raw" line is the
        // alignment row (`|---|`) — the renderer drew it as a `├─┼─┤`
        // separator, which has no `│` chars to map to.  Skip rather than
        // flood-fill the separator.
        let Some(rs) = table_raw_col_to_rendered_col(raw_line, line, start_raw_col) else {
            return;
        };
        let Some(re) = table_raw_col_to_rendered_col(raw_line, line, end_raw_col) else {
            return;
        };
        (rs, re)
    } else {
        let shift = task_marker_shift(raw_line);
        (
            start_raw_col.saturating_sub(shift),
            end_raw_col.saturating_sub(shift),
        )
    };
    if rend_start >= rend_end {
        return;
    }
    paint_cols_on_line(
        line,
        buf,
        area,
        y_start,
        rows_used,
        rend_start,
        rend_end,
        theme.selection,
    );
}

/// Paint `sel_bg` onto the rendered cells for rendered char cols in
/// `[start_col, end_col)`, walking each visual row of the wrapped line.
fn paint_cols_on_line(
    line: &Line<'_>,
    buf: &mut TuiBuf,
    area: Rect,
    y_start: u16,
    rows_used: u16,
    start_col: usize,
    end_col: usize,
    sel_bg: Style,
) {
    let width = area.width as usize;
    if width == 0 || end_col <= start_col {
        return;
    }
    let chars: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| {
            let style = span.style;
            span.content.chars().map(move |c| (c, style))
        })
        .collect();
    let rows = super::line_render::visual_rows_of_chars(&chars, width);
    for (row_off, &(row_start, row_end, _)) in rows.iter().enumerate() {
        if row_off as u16 >= rows_used {
            break;
        }
        let y = area.y + y_start + row_off as u16;
        if y >= area.y + area.height {
            break;
        }
        let row_sel_start = start_col.max(row_start);
        let row_sel_end = end_col.min(row_end);
        if row_sel_start >= row_sel_end {
            continue;
        }
        for i in row_sel_start..row_sel_end {
            let x_off = (i - row_start) as u16;
            let x = area.x + x_off;
            if x >= area.x + area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(cell.style().patch(sel_bg));
            }
        }
    }
}

/// Raw→rendered column shift for task-list items — the renderer strips the
/// `- ` / `N. ` prefix before the `[ ]` checkbox.  Non-task lines return 0.
fn task_marker_shift(raw_line: &str) -> usize {
    let bytes = raw_line.as_bytes();
    let indent_len = bytes
        .iter()
        .take_while(|&&b| b == b' ' || b == b'\t')
        .count();
    let rest = &raw_line[indent_len..];
    let rb = rest.as_bytes();
    let marker_len = match rb.first() {
        Some(&b) if b == b'-' || b == b'*' || b == b'+' => {
            if rb.get(1) == Some(&b' ') {
                2
            } else {
                return 0;
            }
        }
        Some(&b) if b.is_ascii_digit() => {
            let digits = rb.iter().take_while(|b| b.is_ascii_digit()).count();
            match rb.get(digits) {
                Some(&b'.') | Some(&b')') if rb.get(digits + 1) == Some(&b' ') => digits + 2,
                _ => return 0,
            }
        }
        _ => return 0,
    };
    let after_marker = &rest[marker_len..];
    if after_marker.starts_with("[ ] ")
        || after_marker.starts_with("[x] ")
        || after_marker.starts_with("[X] ")
    {
        marker_len
    } else {
        0
    }
}

/// For a table row, map a raw char column in `raw_row` to the matching
/// rendered column in `rendered_line`, using both pipe-position sequences.
/// Returns `None` when pipe counts don't match (e.g. alignment row, border).
fn table_raw_col_to_rendered_col(
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
struct CellOverlay {
    rendered_start: usize,
    rendered_end: usize,
    raw_text: String,
    /// Cursor offset within `raw_text` in chars; `None` if the cursor sits
    /// outside the cell's overlay area (fallback path should be taken).
    cursor_in_cell: Option<usize>,
    /// Byte offset within the raw row at which this cell's content starts
    /// (the byte immediately after the cell's opening `|`).  Used by the
    /// caller to align an absolute selection byte range onto `raw_text` so
    /// the overlay can repaint selection highlighting over the raw chars.
    raw_cell_byte_start: usize,
}

/// Try to compute a cell-scoped overlay for the cursor's active cell.
///
/// Returns `None` when the row doesn't parse as a table row, when the rendered
/// and raw pipe counts disagree (e.g. the cursor row is the alignment row,
/// which renders as a `├─┼─┤` separator), or when the raw cell text is wider
/// than the rendered cell area (in which case the caller falls back to the
/// full row-reveal so the user can still see the content they're editing).
fn compute_cell_overlay(
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

/// Char positions of unescaped `|` characters in a raw table row.  Preceding
/// `\` escapes the pipe per GFM rules; `\\|` is a literal backslash followed
/// by an unescaped pipe.
fn raw_pipe_positions(row: &str) -> Vec<usize> {
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
fn rendered_pipe_positions(line: &Line<'_>) -> Vec<usize> {
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

/// Paint `overlay.raw_text` into the cell's rendered column range, inverting
/// the character at `overlay.cursor_in_cell` to draw the cursor.  Writes
/// directly to the `TuiBuf` — the caller must have already rendered the
/// underlying row so the pipes and neighbouring cells are intact.
///
/// `selection_cols` is the `[start, end)` char range within `overlay.raw_text`
/// that should carry the theme's selection background.  Painting selection here
/// (rather than relying on the generic `paint_selection_overlay`) is necessary
/// because the cell overlay replaces whatever was already in those cells — any
/// earlier selection highlight would be clobbered.
fn overlay_raw_cell(
    buf: &mut TuiBuf,
    area: Rect,
    visual_y: u16,
    overlay: &CellOverlay,
    selection_cols: Option<(usize, usize)>,
    theme: &Theme,
) {
    if visual_y >= area.height {
        return;
    }
    let abs_y = area.y + visual_y;
    let cell_width = overlay.rendered_end.saturating_sub(overlay.rendered_start);
    let raw_chars: Vec<char> = overlay.raw_text.chars().collect();
    let cursor_style = Style::default().add_modifier(Modifier::REVERSED);
    let base_style = theme.normal;

    for i in 0..cell_width {
        let col = overlay.rendered_start + i;
        let abs_x = area.x.saturating_add(col as u16);
        if abs_x >= area.x.saturating_add(area.width) {
            break;
        }
        let ch = raw_chars.get(i).copied().unwrap_or(' ');
        let mut style = base_style;
        if matches!(selection_cols, Some((s, e)) if i >= s && i < e) {
            style = style.patch(theme.selection);
        }
        if overlay.cursor_in_cell == Some(i) {
            style = cursor_style;
        }
        if let Some(cell) = buf.cell_mut((abs_x, abs_y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

/// Split raw block source into lines, keeping any content before the final
/// trailing newline (which ropey line indexing includes).
fn raw_source_lines(source: &str) -> Vec<&str> {
    if source.is_empty() {
        return vec![""];
    }
    // Split on newlines. If source ends with '\n', the last element would be
    // empty — we include it as an empty line so cursor positioning still works.
    let mut lines: Vec<&str> = source.split('\n').collect();
    // Remove the trailing empty string only if there are multiple lines and
    // the source ends with '\n' (the split always produces an extra empty entry
    // at the end for trailing newlines, which we don't want to display as an
    // extra blank).
    if lines.last() == Some(&"") && lines.len() > 1 {
        lines.pop();
    }
    lines
}

/// Find which raw line of the block the cursor is on, and its column offset.
///
/// Returns `(raw_line_index, col)` where col is the char count from the start
/// of the raw line.
fn cursor_position_in_block(
    state: &EditorState,
    cursor_byte: usize,
    raw_source: &str,
) -> (usize, usize) {
    if raw_source.is_empty() {
        return (0, 0);
    }

    // Get the original byte range of the block to find where cursor_byte falls
    // within the raw source text.
    let block_start_byte = state
        .parsed
        .source_map
        .original_range_for_byte(cursor_byte)
        .map(|r| r.start)
        .unwrap_or(0);

    let cursor_offset_in_block = cursor_byte.saturating_sub(block_start_byte);

    // Walk through the raw source in bytes to find which line and col.
    let mut byte_pos = 0usize;
    for (line_idx, line) in raw_source.split('\n').enumerate() {
        let line_end = byte_pos + line.len();
        if cursor_offset_in_block <= line_end {
            // Cursor is on this line. Convert byte offset within line to char count.
            let col_bytes = cursor_offset_in_block.saturating_sub(byte_pos);
            let col = line[..col_bytes.min(line.len())].chars().count();
            return (line_idx, col);
        }
        byte_pos = line_end + 1; // +1 for the '\n'
    }

    // Cursor is at or past the end.
    let last_line_idx = raw_source.split('\n').count().saturating_sub(1);
    let last_line = raw_source.split('\n').last().unwrap_or("");
    (last_line_idx, last_line.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_source_lines_no_trailing_newline() {
        let lines = raw_source_lines("hello\nworld");
        assert_eq!(lines, vec!["hello", "world"]);
    }

    #[test]
    fn raw_source_lines_trailing_newline() {
        let lines = raw_source_lines("hello\nworld\n");
        assert_eq!(lines, vec!["hello", "world"]);
    }

    #[test]
    fn raw_source_lines_single() {
        let lines = raw_source_lines("hello");
        assert_eq!(lines, vec!["hello"]);
    }

    #[test]
    fn raw_source_lines_empty() {
        let lines = raw_source_lines("");
        assert_eq!(lines, vec![""]);
    }

    #[test]
    fn make_raw_line_with_cursor_at_start() {
        let theme = Theme::default();
        let line = make_raw_line("hello", Some(0), &theme);
        // First span should be empty (before cursor), second should be 'h'.
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "hello");
    }

    #[test]
    fn make_raw_line_with_cursor_at_end() {
        let theme = Theme::default();
        let line = make_raw_line("hi", Some(2), &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "hi "); // space added for end-of-line cursor
    }

    #[test]
    fn make_raw_line_without_cursor() {
        let theme = Theme::default();
        let line = make_raw_line("hello", None, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "hello");
    }
}
