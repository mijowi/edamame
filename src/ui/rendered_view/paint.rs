use ratatui::{
    buffer::Buffer as TuiBuf,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::config::Theme;
use crate::editor::table_edit;
use crate::editor::EditorState;
use crate::markdown::table_layout::{table_raw_col_to_rendered_col, CellOverlay};

use super::list_marker::list_raw_col_to_rendered_col;
use super::raw_text::raw_line_byte_start;

/// Build a `Line` showing `raw_text` with a block cursor at `cursor_col`.
///
/// If `cursor_col` is `None`, no cursor is drawn (other lines of the block).
#[cfg(test)]
pub(super) fn make_raw_line(
    raw_text: &str,
    cursor_col: Option<usize>,
    theme: &Theme,
) -> Line<'static> {
    make_raw_line_with_selection(raw_text, cursor_col, None, theme)
}

/// Variant of [`make_raw_line`] that also paints `selection_cols` with the
/// theme's selection background.  `selection_cols` is a `[start, end)` range
/// in char columns within `raw_text`.
pub(super) fn make_raw_line_with_selection(
    raw_text: &str,
    cursor_col: Option<usize>,
    selection_cols: Option<(usize, usize)>,
    theme: &Theme,
) -> Line<'static> {
    let cursor_style = theme.cursor_rendered;
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

/// Build a `Line` for one body row of a mermaid block revealed as a code
/// block: `raw_text` carries the source line with optional cursor and
/// selection overlays, and the line's base style is `code_block_text` so
/// `render_line_from_visual`'s trailing-cell fill extends the code
/// background to the full viewport width.
///
/// Char positions are kept 1:1 with `raw_text` (no leading-pad column),
/// so mouse click → raw col mapping in `rendered_sub_line_to_offset`
/// continues to work without offset adjustments.
pub(super) fn make_code_styled_body_line(
    raw_text: &str,
    cursor_col: Option<usize>,
    selection_cols: Option<(usize, usize)>,
    theme: &Theme,
) -> Line<'static> {
    let base = theme.code_block_text;
    let cursor_style = theme.cursor_rendered;
    let sel_style = theme.selection;
    let chars: Vec<char> = raw_text.chars().collect();
    let total = chars.len();

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(total + 1);
    for (i, ch) in chars.iter().enumerate() {
        let mut style = base;
        if matches!(selection_cols, Some((s, e)) if i >= s && i < e) {
            style = style.patch(sel_style);
        }
        if cursor_col == Some(i) {
            style = cursor_style;
        }
        spans.push(Span::styled(ch.to_string(), style));
    }
    if let Some(col) = cursor_col {
        if col >= total {
            spans.push(Span::styled(" ".to_string(), cursor_style));
        }
    }
    Line::from(spans).style(base)
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
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_selection_overlay(
    editor: &EditorState,
    buf: &mut TuiBuf,
    area: Rect,
    y_start: u16,
    rows_used: u16,
    skip_rows: usize,
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
    // `source.get(..)` rather than direct indexing — when `parsed_dirty` is
    // set, an in-line edit (e.g. an emoji insertion) has shifted byte
    // offsets after the cursor, so `block_range` may now end inside a
    // multi-byte UTF-8 sequence in the live buffer.  Empty-string fallback
    // skips selection painting on this block for one frame; the next parse
    // refresh restores correct ranges.
    let block_text = source
        .get(block_range.start..block_range.end.min(source.len()))
        .unwrap_or("");
    let rendered_span = editor
        .parsed
        .source_map
        .rendered_lines_for_byte(block_range.start);
    let sub_idx_in_block = rendered_line_idx.saturating_sub(rendered_span.start);
    let is_table = table_edit::is_table_block(block_text);
    let raw_line_idx = if is_table {
        // Phase 13: tables can have multi-line headers / data rows when
        // cell content wraps.  Use the box-drawing-glyph classifier
        // instead of a fixed alternating-line pattern so the selection
        // highlight maps onto the right raw row regardless of wrap.
        let own_end = rendered_span.end.min(editor.parsed.lines.len());
        let block_lines = editor
            .parsed
            .lines
            .get(rendered_span.start..own_end)
            .unwrap_or(&[]);
        let kinds = crate::ui::table_view::classify_table_sub_lines(block_lines);
        match kinds.get(sub_idx_in_block) {
            Some(crate::ui::table_view::TableSubLineKind::Header { sub: 0 }) => 0,
            Some(crate::ui::table_view::TableSubLineKind::DataRow { row, sub: 0 }) => row + 2,
            // Continuation sub-lines, separators, and borders don't carry
            // a 1:1 raw-byte mapping, so we skip the highlight rather
            // than paint a speculative one that would look wrong against
            // the wrapped text.
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
    } else if let (Some(rs), Some(re)) = (
        list_raw_col_to_rendered_col(raw_line, line, start_raw_col),
        list_raw_col_to_rendered_col(raw_line, line, end_raw_col),
    ) {
        // List-item lines may shift the content column when the rendered
        // marker width differs from the raw one (e.g. ordered lists with
        // 10+ items render numbers right-aligned, adding leading padding).
        // Use the same map the cursor indicator uses so selection paint
        // and cursor stay coherent.
        (rs, re)
    } else {
        // Non-list line: rendered cells align 1:1 with raw chars.
        (start_raw_col, end_raw_col)
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
        skip_rows,
        rend_start,
        rend_end,
        theme.selection,
    );
}

/// Paint `sel_bg` onto the rendered cells for rendered char cols in
/// `[start_col, end_col)`, walking each visual row of the wrapped line.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_cols_on_line(
    line: &Line<'_>,
    buf: &mut TuiBuf,
    area: Rect,
    y_start: u16,
    rows_used: u16,
    skip_rows: usize,
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
    let indent = crate::ui::line_render::compute_hanging_indent(line);
    let rows = crate::ui::line_render::visual_rows_of_chars(&chars, width, indent);
    for (painted_off, (row_off, &(row_start, row_end, _))) in
        rows.iter().enumerate().skip(skip_rows).enumerate()
    {
        if painted_off as u16 >= rows_used {
            break;
        }
        let y = area.y + y_start + painted_off as u16;
        if y >= area.y + area.height {
            break;
        }
        let row_sel_start = start_col.max(row_start);
        let row_sel_end = end_col.min(row_end);
        if row_sel_start >= row_sel_end {
            continue;
        }
        // Continuation rows are pre-padded with `indent` blank cells so the
        // wrapped text aligns with the first row's text column; the
        // selection background must shift by the same amount.
        let row_indent = if row_off == 0 { 0 } else { indent };
        for i in row_sel_start..row_sel_end {
            let x_off = row_indent + (i - row_start);
            let x = area.x + x_off as u16;
            if x >= area.x + area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(cell.style().patch(sel_bg));
            }
        }
    }
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
pub(super) fn overlay_raw_cell(
    buf: &mut TuiBuf,
    area: Rect,
    visual_y: u16,
    overlay: &CellOverlay,
    selection_cols: Option<(usize, usize)>,
    theme: &Theme,
    cursor_visible: bool,
) {
    if visual_y >= area.height {
        return;
    }
    let abs_y = area.y + visual_y;
    let cell_width = overlay.rendered_end.saturating_sub(overlay.rendered_start);
    let raw_chars: Vec<char> = overlay.raw_text.chars().collect();
    let cursor_style = theme.cursor_rendered;
    // `theme.normal` carries the theme's `default_bg`; letting it
    // through here would clobber the table-row stripe painted under
    // the cell.  Strip the bg so the underlying cell's bg is
    // preserved — selection/cursor styles bring their own bg back
    // when applied on top.
    let base_style = Style {
        bg: None,
        ..theme.normal
    };

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
        if cursor_visible && overlay.cursor_in_cell == Some(i) {
            style = cursor_style;
        }
        if let Some(cell) = buf.cell_mut((abs_x, abs_y)) {
            // `Cell::set_style` only inserts/removes modifiers via
            // `add_modifier` / `sub_modifier`; without an explicit clear,
            // modifiers from the underlying rendered cell — e.g. `BOLD`
            // painted for `**TUI framework**` — survive the overlay and
            // bleed through.  Zero them by hand so the raw markdown chars
            // render in plain weight, while leaving fg/bg untouched so the
            // row's stripe color shows through.
            cell.modifier = Modifier::empty();
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::Theme;

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
