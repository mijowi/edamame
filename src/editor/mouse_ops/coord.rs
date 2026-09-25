use ratatui::text::Line;

use crate::document::CellBand;
use crate::editor::table_edit;
use crate::editor::{EditorState, Mode};
use crate::markdown::list_layout::{
    list_rendered_col_to_raw_col_marker, raw_list_marker_char_width,
    rendered_list_marker_char_width,
};
use crate::markdown::table_layout;
use crate::ui::line_render;
use crate::ui::table_view::HEADER_ROWS;

/// The rendered `Line` under document-area `row` (scroll- and wrap-aware) and the sub-row
/// within it.
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
    let max_char_in_row = line_render::last_col_in_row(row, is_last_row);
    let row_chars = text.chars().skip(start).take(end - start);
    let in_row = line_render::char_idx_at_cell_col(row_chars, target_cell, indent);
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
/// using the same wrap layout as [`line_render::patch_char_cols`], so drags across wrapped
/// sub-rows highlight the right range.  `col` is a screen cell: a click on the right half of
/// a wide glyph lands after it, as [`line_render::char_idx_at_cell_col`] snaps.
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
        return Some((idx, 0, None));
    }
    let indent = line_render::compute_hanging_indent(line);
    let rows = line_render::visual_rows_of_chars(&chars, viewport_width.max(1), indent);
    let sub = sub_row.min(rows.len().saturating_sub(1));
    let row = rows
        .get(sub)
        .copied()
        .unwrap_or((0, chars.len(), chars.len()));
    let (row_start, _, _) = row;
    let max_in_row = line_render::last_col_in_row(row, sub + 1 == rows.len());
    // Continuation rows carry `indent` cells of left padding (as `patch_char_cols`).
    let row_indent = if sub == 0 { 0 } else { indent };
    let chars: Vec<char> = chars.into_iter().map(|(c, _)| c).collect();
    let local_col =
        line_render::char_idx_at_cell_col(chars[row_start..].iter().copied(), col, row_indent);
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
    (cell..cell + line_render::char_cells(ch).max(1))
        .contains(&col)
        .then_some((idx, char_col))
}

/// The wrap layout [`rendered_click_to_line_col`] resolved a click against, returned so the
/// inverse mapping (which runs on every mouse-move) doesn't rebuild it.
pub(super) struct LineLayout {
    /// `(row_start, row_end, next_start)` per visual row, as
    /// [`line_render::visual_rows_of_chars`] produces them.
    rows: Vec<(usize, usize, usize)>,
    /// Hanging indent applied to every row past the first.
    indent: usize,
    /// The rendered line's chars, for measuring cells.
    chars: Vec<char>,
}

/// Screen cell column at which `char_col` is painted, including a continuation row's hanging
/// indent — the inverse of [`rendered_click_to_line_col`].
fn cell_col_for_char_col(layout: &LineLayout, char_col: usize) -> Option<usize> {
    let (sub, _) = line_render::sub_line_of_col(&layout.rows, char_col);
    let (row_start, _, _) = layout.rows.get(sub).copied()?;
    let row_indent = if sub == 0 { 0 } else { layout.indent };
    let row_chars = layout.chars.get(row_start..)?.iter().copied();
    Some(line_render::cell_col_at_char_idx(
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

/// Map `(rendered_line_idx, sub_row_within_line, col)` to a buffer char offset: locate the
/// block, pick the raw source line within it, then map the rendered column to a raw column.
///
/// For inline-formatted text the rendered column may diverge slightly from the raw column; the
/// click lands approximately and the reveal lets the user refine with a second click.
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
    // For tables, the wrap-chunk index of the clicked sub-line within its logical row.
    let mut table_sub = 0usize;
    let raw_line_idx = if is_table {
        let (raw_idx, sub) = table_raw_line_idx(state, &block, block_text);
        table_sub = sub;
        raw_idx
    } else if state.parsed.is_image_block(block.idx)
        && !state.parsed.is_diagram_reveal_block(block.idx)
    {
        // An image reserves many rendered rows for one source line; mapping a reserved row through
        // `sub_idx` would index a phantom raw line and poison the inline-map cache for an unrelated
        // buffer line.  Diagram-reveal blocks (mermaid fences, `$$...$$` math) are excluded: their
        // reveal overlay paints raw source 1:1 onto the reserved rows, so `sub_idx` is the correct
        // source line there.
        0
    } else {
        // Diagram-reveal blocks map the rendered sub-row to a raw source
        // line 1:1, minus the math-preview band a `$$...$$` reveal reserves
        // above the source (0 for mermaid, a preview-off reveal, or any
        // ordinary block).  A click on the band rows themselves resolves to
        // the first source line.
        block
            .sub_idx
            .saturating_sub(state.parsed.latex_source_offset(block.idx))
    };

    // Virtual blank blocks: place the cursor at block start.
    if block_text.is_empty() {
        return state.buffer.rope().byte_to_char(block.range.start);
    }

    // A reflowed paragraph's several source lines collapse into one flow, so `sub_idx` no longer
    // names a raw source line.  Two shapes: revealed (raw lines shown stacked) and not (one
    // wrapped rendered flow) — handled separately below.
    if !is_table && state.parsed.is_reflowed_paragraph_at(block.range.start) {
        // The stacked-raw reveal only happens in Rendered mode; Preview always shows the flow.
        let revealed = state.mode == Mode::Rendered
            && state.cursor_block_revealed()
            && rendered_line_idx == crate::editor::state::cursor_rendered_line_idx(state);
        if revealed {
            // Revealed: the block shows its raw source lines *stacked*, so `sub_row_within_line`
            // walks those lines' wrap rows.  Find the raw line and wrap sub it lands on, map the
            // cell column within that raw line, and resolve to a buffer offset via the raw line's
            // buffer position — the same shape as the mermaid / setext reveal paths.
            let raw_lines = crate::ui::rendered_view::revealed_source_lines(block_text);
            let mut remaining = sub_row_within_line;
            let mut chosen = raw_lines.len().saturating_sub(1);
            let mut wrap_sub = 0usize;
            for (i, rl) in raw_lines.iter().enumerate() {
                let n = revealed_raw_rows(rl, viewport_width).0.len().max(1);
                if remaining < n {
                    chosen = i;
                    wrap_sub = remaining;
                    break;
                }
                remaining -= n;
            }
            let raw_line_text = raw_lines.get(chosen).copied().unwrap_or("");
            let (rows, indent) = revealed_raw_rows(raw_line_text, viewport_width);
            let sub = wrap_sub.min(rows.len().saturating_sub(1));
            let rowr = rows.get(sub).copied().unwrap_or((0, 0, 0));
            let (start, end, _) = rowr;
            let is_last_row = sub + 1 == rows.len();
            let max_in_row = line_render::last_col_in_row(rowr, is_last_row);
            let row_indent = if sub == 0 { 0 } else { indent };
            let row_chars = raw_line_text.chars().skip(start).take(end - start);
            let in_row = line_render::char_idx_at_cell_col(row_chars, col, row_indent);
            let raw_col = (start + in_row).min(max_in_row);
            let first_buf_line = state.buffer.rope().byte_to_line(block.range.start);
            let target = (first_buf_line + chosen).min(state.buffer.line_count().saturating_sub(1));
            return (state.buffer.line_to_char(target) + raw_col).min(buffer_len);
        }
        // Not revealed: the block is one wrapped rendered flow.  Resolve the wrap with
        // `click_to_rendered_char_idx`, then turn that rendered column into a raw char through the
        // block-wide inline collapse map (soft breaks → spaces, the collapse owned in
        // `InlineColMap`); the raw char is a char offset into the block.  `col` is a cell column,
        // which `click_to_rendered_char_idx` folds in, so this stays correct across wide glyphs.
        let content = block_text.strip_suffix('\n').unwrap_or(block_text);
        let rendered_line = &state.parsed.lines[rendered_line_idx];
        let rendered_chars: Vec<(char, ratatui::style::Style)> = rendered_line
            .spans
            .iter()
            .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
            .collect();
        let rendered_idx = click_to_rendered_char_idx(
            rendered_line,
            &rendered_chars,
            col,
            sub_row_within_line,
            viewport_width,
        );
        let map = crate::markdown::InlineColMap::build(content);
        let raw_char = map.rendered_to_raw_vec()[rendered_idx.min(map.rendered_len())];
        let block_start_char = state.buffer.rope().byte_to_char(block.range.start);
        return (block_start_char + raw_char).min(buffer_len);
    }

    let (line_byte_start, line_byte_end) = raw_line_byte_range(block_text, raw_line_idx);
    let line_text = &block_text[line_byte_start..line_byte_end];
    let rendered_line = &state.parsed.lines[rendered_line_idx];

    // Rows the view paints as raw source (a mermaid block's reserved rows, the cursor's own
    // revealed line) map `col` against the raw line's own wrap layout, since the rendered
    // `Line` isn't what the user sees.  Tables are excluded (their chrome stays painted, so
    // the pipe-aware branch below applies), as are code-block body rows: `cursor_block_revealed`
    // is block-level and time-based, but only a code block's fence rows de-render, so without
    // `line_allows_raw_reveal` a body click would land one char past the target glyph.
    let block_kind = state.parsed.real_block_for_byte(block.range.start);
    let content_lines = crate::ui::rendered_view::raw_source_lines(block_text);
    let line_reveals = crate::markdown::code_layout::line_allows_raw_reveal(
        block_kind,
        raw_line_idx,
        &content_lines,
    );
    let revealed_cursor_line = !is_table
        && line_reveals
        && state.cursor_block_revealed()
        && rendered_line_idx == crate::editor::state::cursor_rendered_line_idx(state);
    if state.parsed.is_diagram_reveal_block(block.idx) || revealed_cursor_line {
        let (rows, indent) = revealed_raw_rows(line_text, viewport_width);
        let sub = sub_row_within_line.min(rows.len().saturating_sub(1));
        let row = rows.get(sub).copied().unwrap_or((0, 0, 0));
        let (start, end, _) = row;
        let is_last_row = sub + 1 == rows.len();
        let max_in_row = line_render::last_col_in_row(row, is_last_row);
        let row_indent = if sub == 0 { 0 } else { indent };
        let row_chars = line_text.chars().skip(start).take(end - start);
        let in_row = line_render::char_idx_at_cell_col(row_chars, col, row_indent);
        let raw_col = (start + in_row).min(max_in_row);
        return raw_col_to_buffer_char(state, &block, line_byte_start, line_text, raw_col);
    }

    let raw_col = if is_table && rendered_line.spans.iter().any(|s| s.content.contains('│')) {
        // Rendered cells are padded to layout width; map through the pipe positions so the
        // click stays inside the clicked cell.
        let row_width = line_row_width(rendered_line, sub_row_within_line);
        let clamped_col = col.min(row_width);
        table_click_to_raw_col(line_text, rendered_line, clamped_col, table_sub)
            .unwrap_or(clamped_col)
    } else if let Some(crate::markdown::Block::CodeBlock { fenced, .. }) =
        block_kind.filter(|_| !line_reveals)
    {
        // A code body row is one pad cell (and, for indented blocks, the stripped indent) off
        // from its raw column; the generic mapping would land one char late (issue #28).
        let rendered_chars: Vec<(char, ratatui::style::Style)> = rendered_line
            .spans
            .iter()
            .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
            .collect();
        let rendered_idx = click_to_rendered_char_idx(
            rendered_line,
            &rendered_chars,
            col,
            sub_row_within_line,
            viewport_width,
        );
        let stripped = line_text.strip_suffix('\n').unwrap_or(line_text);
        crate::markdown::code_layout::code_rendered_col_to_raw_col(stripped, *fenced, rendered_idx)
    } else {
        let buffer_line_idx = state
            .buffer
            .block_line_to_buffer_line(block.range.start, raw_line_idx);
        let stripped = line_text.strip_suffix('\n').unwrap_or(line_text);
        let inline_map = state.inline_map_for(buffer_line_idx, stripped);
        // Resolved via the AST so a paragraph line that merely looks like `2. ...` stays
        // generic, and a YAML sequence entry in frontmatter isn't taken for a list marker.
        let line_block = state
            .parsed
            .real_block_for_byte(block.range.start + line_byte_start);
        let mapping = match line_block {
            Some(crate::markdown::Block::List { .. }) => LineMapping::List,
            Some(crate::markdown::Block::MetadataBlock { .. }) => LineMapping::Verbatim,
            _ => LineMapping::Generic,
        };
        non_table_click_to_raw_col(
            rendered_line,
            line_text,
            col,
            sub_row_within_line,
            viewport_width,
            &inline_map,
            mapping,
        )
    };

    raw_col_to_buffer_char(state, &block, line_byte_start, line_text, raw_col)
}

/// The block that produced a rendered line: its source byte range, its rendered-line range, and
/// the clicked line's offset within that range.
struct BlockLocation {
    idx: usize,
    range: std::ops::Range<usize>,
    rendered_span: std::ops::Range<usize>,
    sub_idx: usize,
}

/// `None` when the rendered line is past the source map (e.g. during a pending edit).
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

/// Raw `info.rows[..]` index plus wrap-chunk index for a click on a table block's rendered
/// sub-line.  Classified by leading box-drawing glyph, not by line alternation, because
/// wrapped data rows span several rendered lines.
fn table_raw_line_idx(
    state: &EditorState,
    block: &BlockLocation,
    block_text: &str,
) -> (usize, usize) {
    use crate::ui::table_view::TableSubLineKind;
    let block_lines = state
        .parsed
        .lines
        .get(block.rendered_span.start..block.rendered_span.end.min(state.parsed.lines.len()))
        .unwrap_or(&[]);
    let kinds = crate::ui::table_view::classify_table_sub_lines(block_lines);
    match kinds.get(block.sub_idx) {
        Some(TableSubLineKind::TopBorder) => (0, 0),
        Some(TableSubLineKind::Header { sub }) => (0, *sub),
        Some(TableSubLineKind::ThickSeparator) => (HEADER_ROWS, 0),
        Some(TableSubLineKind::DataRow { row, sub }) => (row + HEADER_ROWS, *sub),
        Some(TableSubLineKind::ThinSeparator) => {
            // Snap to the preceding data row.
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
            (row + HEADER_ROWS, 0)
        }
        Some(TableSubLineKind::BottomBorder) | None => {
            // Snap to the last data row (`is_table_block` guarantees at least one).
            let last_data = block_text.split('\n').count().saturating_sub(HEADER_ROWS);
            (last_data.max(HEADER_ROWS), 0)
        }
    }
}

/// When the reveal is active and `rendered_line_idx` is inside the cursor's block, the wrap
/// count of the raw source line the painter actually paints there (raw text carries markers
/// and may wrap to more rows than the rendered form).  `None` otherwise; callers fall back to
/// the per-line cache.  Covers mermaid blocks (every reserved row) and the non-table cursor
/// line.
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

    if state.parsed.is_diagram_reveal_block(cursor_block_idx) {
        // Shift past the math-preview band (0 unless this is a `$$...$$`
        // reveal with the preview on) so the rendered row maps to its raw
        // source line; band rows clamp to the first line.
        let band = state.parsed.latex_source_offset(cursor_block_idx);
        let sub = (rendered_line_idx - block_lines.start).saturating_sub(band);
        let raw_line = block_text.split('\n').nth(sub).unwrap_or("");
        return Some(revealed_raw_rows(raw_line, viewport_width).0.len().max(1));
    }

    // Tables keep their rendered chrome, so skip them.
    let is_table = table_edit::is_table_block(block_text);
    if is_table {
        return None;
    }
    // A revealed reflowed paragraph is one rendered line that reveals to its *stacked* raw lines,
    // so its row count is the sum of every raw line's wrap count — not just the first line's.
    if state.parsed.is_reflowed_paragraph_at(block_range.start) {
        let total: usize = crate::ui::rendered_view::revealed_source_lines(block_text)
            .iter()
            .map(|rl| revealed_raw_rows(rl, viewport_width).0.len().max(1))
            .sum();
        return Some(total.max(1));
    }
    let cursor_line = crate::editor::state::cursor_rendered_line_idx(state);
    if rendered_line_idx != cursor_line {
        return None;
    }

    let sub = rendered_line_idx - block_lines.start;
    let raw_line = block_text.split('\n').nth(sub).unwrap_or("");
    // A row the view doesn't de-render (a code block's body) still shows its padded rendered
    // line; the raw wrap count would mis-walk every row below it.
    if !crate::markdown::code_layout::line_allows_raw_reveal(
        state.parsed.real_block_for_byte(block_range.start),
        sub,
        &crate::ui::rendered_view::raw_source_lines(block_text),
    ) {
        return None;
    }
    Some(revealed_raw_rows(raw_line, viewport_width).0.len().max(1))
}

/// Wrap layout of a raw source line exactly as the reveal painter lays it out, plus the
/// hanging indent it used.  `render_line` derives that indent from the line's leading marker,
/// so `visual_rows_of_str` (indent 0) would disagree on both row count and continuation start
/// columns.  Callers must shift `col` by the indent on sub-rows past the first.
///
/// The indent is the *effective* one: when `indent + 1 >= width` the painter falls back to a
/// flat layout, and reporting the raw marker width would push every continuation row into
/// `char_idx_at_cell_col`'s forbidden-indent zone.
fn revealed_raw_rows(raw_line: &str, viewport_width: usize) -> (Vec<(usize, usize, usize)>, usize) {
    let width = viewport_width.max(1);
    let indent = line_render::compute_hanging_indent_str(raw_line);
    let indent = if indent + 1 >= width { 0 } else { indent };
    let chars: Vec<(char, ratatui::style::Style)> = raw_line
        .chars()
        .map(|c| (c, ratatui::style::Style::default()))
        .collect();
    (
        line_render::visual_rows_of_chars(&chars, width, indent),
        indent,
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
/// landed on.  The one shared walk over `line_render`'s wrap geometry for every rendered-line
/// mapping (generic and code-block branches alike).
fn click_to_rendered_char_idx(
    rendered_line: &Line<'_>,
    rendered_chars: &[(char, ratatui::style::Style)],
    col: usize,
    sub_row_within_line: usize,
    viewport_width: usize,
) -> usize {
    let indent = line_render::compute_hanging_indent(rendered_line);
    let viewport = viewport_width.max(1);
    let rows = line_render::visual_rows_of_chars(rendered_chars, viewport, indent);
    let sub = sub_row_within_line.min(rows.len().saturating_sub(1));
    let row = rows.get(sub).copied().unwrap_or((0, 0, 0));
    let (start, end, _) = row;
    let row_indent = if sub == 0 { 0 } else { indent };
    let is_last_row = sub + 1 == rows.len();
    let max_in_row = line_render::last_col_in_row(row, is_last_row);
    let row_chars = rendered_chars
        .iter()
        .skip(start)
        .take(end - start)
        .map(|(c, _)| *c);
    let in_row = line_render::char_idx_at_cell_col(row_chars, col, row_indent);
    (start + in_row).min(max_in_row)
}

/// Which raw↔rendered column relation a non-table line takes, resolved from the line's AST
/// block rather than from what its text looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineMapping {
    /// The marker map composes with the inline collapse map.
    List,
    /// Painted exactly as written (frontmatter): identity.
    Verbatim,
    /// The inline collapse map alone.
    Generic,
}

/// Non-table click → raw char column on `line_text`.
///
/// The renderer's leading prefix (`• ` / `1. ` / `[ ] ` / `▎ ` / heading indent) has no
/// counterpart in pulldown-cmark's `Text` events; its width is recovered by comparing the
/// rendered char count against the inline map's content count, so clicks on the prefix route
/// to the raw prefix area and clicks past it go through the map.  Falls back to 1:1 when the
/// prefix width isn't trustworthy.  Code blocks never reach here (their padding defeats the
/// inference); the caller routes them through
/// [`code_layout::code_rendered_col_to_raw_col`](crate::markdown::code_layout::code_rendered_col_to_raw_col).
fn non_table_click_to_raw_col(
    rendered_line: &Line<'_>,
    line_text: &str,
    col: usize,
    sub_row_within_line: usize,
    viewport_width: usize,
    inline_map: &crate::markdown::InlineColMap,
    mapping: LineMapping,
) -> usize {
    let rendered_chars: Vec<(char, ratatui::style::Style)> = rendered_line
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
        .collect();
    let rendered_idx = click_to_rendered_char_idx(
        rendered_line,
        &rendered_chars,
        col,
        sub_row_within_line,
        viewport_width,
    );

    if mapping == LineMapping::Verbatim {
        return rendered_idx;
    }

    let actual_rendered_count = rendered_chars.len();
    let map = inline_map.rendered_to_raw_vec();
    let map_content_count = inline_map.rendered_len();
    let raw_content_start = map.first().copied().unwrap_or(0);

    // Mirror of the forward marker map in `markdown::list_layout`: the rendered marker can be
    // wider than the raw one (` 1. ` vs `1. `), which the prefix inference below can't
    // represent.  A continuation line without its own marker falls through to the generic path.
    if mapping == LineMapping::List {
        if let (Some(rmw), Some(rmw_r)) = (
            raw_list_marker_char_width(line_text),
            rendered_list_marker_char_width(rendered_line),
        ) {
            if rendered_idx < rmw_r {
                return list_rendered_col_to_raw_col_marker(rmw, rmw_r, rendered_idx);
            }
            let content_idx = rendered_idx - rmw_r;
            let content_rendered = actual_rendered_count.saturating_sub(rmw_r);
            if map_content_count == content_rendered {
                // The map's raw columns are absolute, marker included.
                return map
                    .get(content_idx)
                    .copied()
                    .unwrap_or_else(|| line_text.chars().count());
            }
            // Marker shift only: exact for unformatted content, approximate otherwise.
            return (rendered_idx + rmw).saturating_sub(rmw_r);
        }
    }

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
    let source_len = state.buffer.contents().len();
    state
        .buffer
        .rope()
        .byte_to_char(absolute_byte.min(source_len))
        .min(state.buffer.len_chars())
}

/// Rendered column → raw column for a table row, keyed by the cell the click falls in (found
/// by pipe positions).  Leading padding lands on the first content char; trailing padding
/// clamps just past the chunk's last char so the cursor never jumps into the next cell.
///
/// Content columns are routed through the cell's [`InlineColMap`](crate::markdown::InlineColMap)
/// so a click inside a cell with hidden inline markers (`` `code` ``, `**bold**`, a link)
/// lands on the glyph under the cursor rather than the raw position the same *count* of chars
/// in — mirroring [`non_table_click_to_raw_col`].  The wrap chunks are computed over the
/// marker-collapsed (rendered) content, matching what `render_table_row` actually wraps, so the
/// two agree even on continuation sub-lines.
///
/// `sub` is the wrap-chunk index of the clicked sub-line within its logical row.  `None` when
/// the line isn't a table row (separator, border); the caller falls back to the char-by-char
/// map.
fn table_click_to_raw_col(
    raw_line: &str,
    rendered_line: &Line<'_>,
    rendered_col: usize,
    sub: usize,
) -> Option<usize> {
    let raw_pipes = table_layout::raw_pipe_positions(raw_line);
    let rendered_pipes = table_layout::rendered_pipe_cells(rendered_line);
    if raw_pipes.len() < 2 || rendered_pipes.len() != raw_pipes.len() {
        return None;
    }
    let col_count = rendered_pipes.len() - 1;

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

    let clicked = rendered_col.max(rend_cell_start);
    let rend_offset_in_cell = clicked.saturating_sub(rend_cell_start);

    let raw_chars: Vec<char> = raw_cell_text.chars().collect();
    let raw_leading = raw_chars.iter().take_while(|c| c.is_whitespace()).count();
    let raw_trailing = raw_chars
        .iter()
        .rev()
        .take_while(|c| c.is_whitespace())
        .count();
    let content_chars = raw_chars.len().saturating_sub(raw_leading + raw_trailing);

    // A rendered cell is `│` + space + content + space (see `render_table_row`).
    let cell_width = rend_cell_end.saturating_sub(rend_cell_start + 2).max(1);
    let trimmed: String = raw_chars[raw_leading..raw_leading + content_chars]
        .iter()
        .collect();

    // Compose with the cell's inline collapse map: markers (backtick delimiters, `**`, a link's
    // URL) are hidden in the rendered cell, so a rendered content column is fewer chars in than
    // the raw column it addresses.  `rendered_to_raw` skips exactly those markers.
    let map = crate::markdown::InlineColMap::build(&trimmed);
    let rendered_to_raw = map.rendered_to_raw_vec();
    let raw_content_col = |rendered: usize| rendered_to_raw[rendered.min(map.rendered_len())];

    // Wrap the marker-collapsed content the renderer paints, so chunk offsets are in the same
    // (rendered) coordinate space as `rend_offset_in_cell`.
    let trimmed_chars: Vec<char> = trimmed.chars().collect();
    let rendered_content: String = (0..map.rendered_len())
        .map(|r| {
            trimmed_chars
                .get(rendered_to_raw[r])
                .copied()
                .unwrap_or(' ')
        })
        .collect();
    let chunks = table_layout::wrap_cell_with_indices(&rendered_content, cell_width);
    // Blank padding sub-lines of a short cell map to the end of its content.
    let (chunk_start, chunk_text) = chunks
        .get(sub)
        .map(|(start, text)| (*start, text.as_str()))
        .unwrap_or((map.rendered_len(), ""));

    let raw_offset_in_cell = if rend_offset_in_cell <= 1 {
        raw_leading + raw_content_col(chunk_start)
    } else {
        // The click is a screen cell; a wide glyph before it spans two.
        let content_col =
            line_render::char_idx_at_cell_col(chunk_text.chars(), rend_offset_in_cell - 1, 0);
        raw_leading + raw_content_col(chunk_start + content_col.min(chunk_text.chars().count()))
    };

    Some(raw_cell_start + raw_offset_in_cell.min(raw_chars.len()))
}

/// The table-cell band under a Preview click: the inclusive rendered-line range of the cell's
/// logical row plus the half-open column range of its content area.  `None` off a header or
/// data sub-line, where callers keep full-line selection.
pub(super) fn preview_table_cell_band(
    state: &EditorState,
    rendered_line_idx: usize,
    col: usize,
) -> Option<CellBand> {
    use crate::ui::table_view::TableSubLineKind;
    let block = locate_block(state, rendered_line_idx)?;
    let source = state.buffer.contents();
    let block_text = source.get(block.range.start..block.range.end.min(source.len()))?;
    if !table_edit::is_table_block(block_text) {
        return None;
    }
    let block_lines = state
        .parsed
        .lines
        .get(block.rendered_span.start..block.rendered_span.end.min(state.parsed.lines.len()))?;
    let kinds = crate::ui::table_view::classify_table_sub_lines(block_lines);

    let same_row = |k: &TableSubLineKind| match (kinds.get(block.sub_idx), k) {
        (Some(TableSubLineKind::Header { .. }), TableSubLineKind::Header { .. }) => true,
        (
            Some(TableSubLineKind::DataRow { row: clicked, .. }),
            TableSubLineKind::DataRow { row, .. },
        ) => row == clicked,
        _ => false,
    };
    if !kinds.get(block.sub_idx).is_some_and(&same_row) {
        return None;
    }

    let mut first = block.sub_idx;
    while first > 0 && same_row(&kinds[first - 1]) {
        first -= 1;
    }
    let mut last = block.sub_idx;
    while last + 1 < kinds.len() && same_row(&kinds[last + 1]) {
        last += 1;
    }

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
            block.rendered_span.start + first,
            block.rendered_span.start + last,
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
    let source = state.buffer.contents();
    let byte = state
        .buffer
        .rope()
        .char_to_byte(char_offset.min(state.buffer.len_chars()));
    let info = table_edit::find_table_at(&source, byte)?;
    let (row_idx, col_idx) = table_edit::cursor_cell(&info, byte)?;
    let row = info.rows.get(row_idx)?;
    if row.kind == table_edit::RowKind::Alignment {
        return None;
    }
    let cell = row.cells.get(col_idx)?;
    let start_byte = row.start + cell.content_start;
    let end_byte = row.start + cell.content_end;
    let rope = state.buffer.rope();
    Some((
        rope.byte_to_char(start_byte.min(source.len())),
        rope.byte_to_char(end_byte.min(source.len())),
    ))
}

/// Upper bound for clamping a click past the end of a rendered line.  Returns the full width
/// in cells regardless of sub-row: conservative (keeps clicks off the next line) and only loses
/// precision deep in the padding of wrapped lines.
fn line_row_width(line: &Line<'_>, _sub_row: usize) -> usize {
    line.spans
        .iter()
        .map(|s| table_layout::str_cells(&s.content))
        .sum()
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

    /// A click is a screen cell: a wide glyph in the cell to the left moves the later cells two
    /// columns per glyph, and a click on a glyph's right half lands after it.
    #[test]
    fn table_click_maps_screen_cells_past_wide_glyphs() {
        let raw = "| 日本 | ab |";
        // Cells: │0 ␠1 日2-3 本4-5 ␠6 │7 ␠8 a9 b10 ␠11 │12
        let line = Line::from("│ 日本 │ ab │");
        let raw_col = |c: char| raw.chars().position(|x| x == c).unwrap();
        assert_eq!(
            table_click_to_raw_col(raw, &line, 10, 0),
            Some(raw_col('b'))
        );
        assert_eq!(table_click_to_raw_col(raw, &line, 9, 0), Some(raw_col('a')));
        assert_eq!(
            table_click_to_raw_col(raw, &line, 2, 0),
            Some(raw_col('日'))
        );
        assert_eq!(
            table_click_to_raw_col(raw, &line, 3, 0),
            Some(raw_col('本'))
        );
    }

    /// Preview state with paragraph reflow reconciled, as `App::prepare_viewport` does each frame.
    fn preview_state(src: &str, width: usize) -> EditorState {
        let mut st = EditorState::new(Buffer::from_str(src), theme());
        assert_eq!(st.mode, Mode::Preview);
        st.set_viewport_width(width);
        st.sync_reflow_for_mode();
        st
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
        assert!(
            !state.parsed.is_reflowed_paragraph_at(0),
            "a hard-break paragraph is multi-line and must not take the reflow path",
        );
        let off = rendered_sub_line_to_offset(&state, 1, 0, 0, 80);
        assert_eq!(
            state.buffer.contents().chars().nth(off),
            Some('t'),
            "click on the second segment must land on 'three', got offset {off}",
        );
    }
}
