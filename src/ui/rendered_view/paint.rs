use ratatui::{
    buffer::Buffer as TuiBuf,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::config::Theme;
use crate::editor::table_edit;
use crate::editor::EditorState;
use crate::markdown::table_layout::CellOverlay;

use super::list_marker::{
    list_raw_col_to_rendered_col, raw_list_marker_char_width, rendered_list_marker_char_width,
};
use super::raw_text::raw_line_byte_start;

/// Build a `Line` showing `raw_text` with a block cursor at `cursor_col`.
///
/// If `cursor_col` is `None`, no cursor is drawn (other lines of the block).
#[cfg(test)]
pub(super) fn make_raw_line(raw_text: &str, theme: &Theme) -> Line<'static> {
    make_raw_line_with_selection(raw_text, None, theme)
}

/// Build a `Line` showing `raw_text` (the cursor's block, raw-revealed), with
/// `selection_cols` painted in the theme's selection background.
/// `selection_cols` is a `[start, end)` range in char columns within
/// `raw_text`.
///
/// The cursor itself is NOT embedded here: it is painted onto the resolved
/// cell by `line_render`'s cursor override at render time.  This keeps the
/// wrapped layout computed from the *bare* source text, matching the wrap that
/// the scroll / navigation code (which never sees the cursor glyph) uses — a
/// `▏` bar glyph baked into the line would otherwise shift word-wrap breaks
/// and desync the two.
pub(super) fn make_raw_line_with_selection(
    raw_text: &str,
    selection_cols: Option<(usize, usize)>,
    theme: &Theme,
) -> Line<'static> {
    let sel_style = theme.selection;
    let chars: Vec<char> = raw_text.chars().collect();
    let total = chars.len();

    // Always emit one span per char so per-char styling stays predictable when
    // cursor and selection overlap.  The runs of same-style chars don't need to
    // be coalesced — ratatui's Line works fine with short spans.
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(total);
    for (i, ch) in chars.iter().enumerate() {
        let mut style = theme.normal;
        if matches!(selection_cols, Some((s, e)) if i >= s && i < e) {
            style = style.patch(sel_style);
        }
        spans.push(Span::styled(ch.to_string(), style));
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
    selection_cols: Option<(usize, usize)>,
    theme: &Theme,
) -> Line<'static> {
    let base = theme.code_block_text;
    let sel_style = theme.selection;
    let chars: Vec<char> = raw_text.chars().collect();
    let total = chars.len();

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(total);
    for (i, ch) in chars.iter().enumerate() {
        let mut style = base;
        if matches!(selection_cols, Some((s, e)) if i >= s && i < e) {
            style = style.patch(sel_style);
        }
        spans.push(Span::styled(ch.to_string(), style));
    }
    Line::from(spans).style(base)
}

/// Post-render pass: paint `style` on top of the rendered cells of a
/// given rendered line for the source byte range `[sel_start_byte,
/// sel_end_byte)`, if that range touches the line's block.  Shared by
/// the selection overlay (`theme.selection`) and the search-match
/// overlays (`theme.selection` / `selection_muted`).
///
/// Computes the raw byte range of *this specific rendered line* within its
/// block (by splitting the block's raw text on newlines), intersects with the
/// requested byte range, and highlights only the rendered cols that
/// correspond to covered bytes.  Falls back to "whole line" highlight for
/// blocks where the per-line mapping can't be determined cleanly.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_byte_range_overlay(
    editor: &EditorState,
    buf: &mut TuiBuf,
    area: Rect,
    y_start: u16,
    rows_used: u16,
    skip_rows: usize,
    rendered_line_idx: usize,
    sel_start_byte: usize,
    sel_end_byte: usize,
    style: Style,
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
    // Wrap-chunk index of a table sub-line within its logical row: a
    // wrapped cell shows chunk `table_sub` of its content on this line.
    let mut table_sub = 0usize;
    let raw_line_idx = if is_table {
        // Tables can have multi-line headers / data rows when
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
            Some(crate::ui::table_view::TableSubLineKind::Header { sub }) => {
                table_sub = *sub;
                0
            }
            Some(crate::ui::table_view::TableSubLineKind::DataRow { row, sub }) => {
                table_sub = *sub;
                row + 2
            }
            // Separators and borders don't carry a raw-byte mapping —
            // skip the highlight rather than paint a speculative one.
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

    // Map raw cols to rendered cols.  Tables go cell-by-cell via pipe
    // positions.  List items compose the marker offset with the inline
    // collapse map.  Paragraph lines use the inline collapse map directly.
    let Some(line) = editor.parsed.lines.get(rendered_line_idx) else {
        return;
    };

    let buffer_line_idx = editor
        .buffer
        .block_line_to_buffer_line(block_range.start, raw_line_idx);
    let inline_map = editor.inline_map_for(buffer_line_idx, raw_line);
    let actual_rendered: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();

    // Block-kind-aware prefix adjustments: headings render behind a
    // level-deep space prefix and code blocks behind a single leading
    // pad cell, both invisible to the inline collapse map (which only
    // models inline markup).  Without the shift, the fallback paths
    // below paint one or more cells to the left of the actual text.
    // Looked up via `real_block_for_byte` — a source-map block index
    // must NOT be used against `parsed.blocks` (its index space counts
    // blank-line virtual blocks, so the two diverge in any document
    // with blank lines).  `line_sel_start` is a byte inside the
    // covered text, so it lands inside the block's real range.
    let block_kind = editor.parsed.real_block_for_byte(line_sel_start);

    if is_table {
        // A cell may wrap onto several rendered sub-lines, and the match's
        // raw columns land in at most ONE wrap chunk per cell.  Map the raw
        // range to the per-cell rendered segments visible on *this*
        // wrap-chunk (`table_sub`): a cell whose chunk doesn't overlap the
        // match contributes no segment, so the highlight never bleeds onto a
        // sub-line that doesn't actually show the matched text.  This is the
        // unified path for the first sub-line (`table_sub == 0`, the first
        // chunk) and every continuation alike — the old first-line mapping
        // ignored wrapping and painted a spurious clamped highlight on
        // sub-line 0 for a match that really sits on a later chunk.
        for (rs, re) in crate::markdown::table_layout::table_raw_col_range_to_rendered_segments(
            raw_line,
            line,
            start_raw_col,
            end_raw_col,
            table_sub,
        ) {
            paint_cols_on_line(
                line, buf, area, y_start, rows_used, skip_rows, rs, re, style,
            );
        }
        return;
    }

    let (rend_start, rend_end) =
        if let Some(crate::markdown::Block::CodeBlock { fenced, .. }) = block_kind {
            // Code body rows render the raw text 1:1 behind one leading
            // pad cell.  Indented (non-fenced) blocks additionally drop
            // the up-to-4-space indent that pulldown-cmark strips from the
            // content.
            //
            // Fence rows render unrelated text — the ` lang ` label (or an
            // NBSP placeholder) for the opening fence, and an NBSP
            // placeholder for the closing fence — so a raw→rendered column
            // mapping is meaningless.  When the selection touches a fence
            // row, highlight the whole rendered row so the selection reads
            // as covering it (Visual / V-LINE) rather than leaving a gap at
            // the top and bottom of a selected block.
            if *fenced && (raw_line_idx == 0 || raw_line_idx + 1 == raw_lines.len()) {
                (0, actual_rendered)
            } else {
                let stripped = if *fenced {
                    0
                } else {
                    code_indent_strip_chars(raw_line)
                };
                let map_col = |c: usize| c.saturating_sub(stripped) + 1;
                (map_col(start_raw_col), map_col(end_raw_col))
            }
        } else if let Some(crate::markdown::Block::Heading { level, .. }) = block_kind {
            // Headings render as a level-deep space prefix plus the
            // collapsed inline content; shift the mapped cols right by the
            // prefix.  A length mismatch (big-H1 rows, the setext
            // underline, smart-punctuation collapse) skips the highlight
            // instead of falling back to raw cols, which would be
            // off-by-prefix.
            let prefix = heading_prefix_width(*level);
            let content_rendered = actual_rendered.saturating_sub(prefix);
            match (
                inline_map.raw_to_rendered_checked(start_raw_col, content_rendered),
                inline_map.raw_to_rendered_checked(end_raw_col, content_rendered),
            ) {
                (Some(rs), Some(re)) => (rs + prefix, re + prefix),
                _ => return,
            }
        } else if let (Some(rs), Some(re)) = (
            list_raw_col_to_rendered_col(raw_line, line, start_raw_col),
            list_raw_col_to_rendered_col(raw_line, line, end_raw_col),
        ) {
            // List-item line: the marker-only map handles the `- ` / `1. `
            // prefix shift.  Compose with the inline collapse map so bold /
            // italic / link markup inside the item also lines up.
            let raw_marker = raw_list_marker_char_width(raw_line);
            let rendered_marker = rendered_list_marker_char_width(line);
            let (mut rend_start, rend_end) =
                if let (Some(rmw), Some(rmw_r)) = (raw_marker, rendered_marker) {
                    let content_rendered = actual_rendered.saturating_sub(rmw_r);
                    if inline_map.rendered_len() == content_rendered {
                        let map_col = |raw_col: usize| -> usize {
                            if raw_col < rmw {
                                rmw_r
                            } else {
                                inline_map.raw_to_rendered(raw_col) + rmw_r
                            }
                        };
                        (map_col(start_raw_col), map_col(end_raw_col))
                    } else {
                        (rs, re)
                    }
                } else {
                    (rs, re)
                };
            // A selection that reaches the line's first column covers the rendered
            // marker too — most notably VisualLine, which widens to whole lines, but
            // also any charwise span whose intermediate lines are fully selected.
            // The marker map above snaps such a start forward to the content column,
            // leaving the `1. ` / `• ` prefix unpainted; pull it back to col 0 so the
            // whole rendered row highlights.
            if start_raw_col == 0 {
                rend_start = 0;
            }
            (rend_start, rend_end)
        } else {
            // Paragraph / heading / code block line: use the inline collapse
            // map so selection highlights track rendered glyph positions.
            match (
                inline_map.raw_to_rendered_checked(start_raw_col, actual_rendered),
                inline_map.raw_to_rendered_checked(end_raw_col, actual_rendered),
            ) {
                (Some(rs), Some(re)) => (rs, re),
                _ => (start_raw_col, end_raw_col),
            }
        };
    if rend_start >= rend_end {
        return;
    }
    paint_cols_on_line(
        line, buf, area, y_start, rows_used, skip_rows, rend_start, rend_end, style,
    );
}

/// Rendered-cell width of the space prefix the renderer puts in front
/// of a heading's inline content (one cell per level — see
/// `Renderer::render_heading`).
fn heading_prefix_width(level: pulldown_cmark::HeadingLevel) -> usize {
    use pulldown_cmark::HeadingLevel::*;
    match level {
        H1 => 1,
        H2 => 2,
        H3 => 3,
        H4 => 4,
        H5 => 5,
        H6 => 6,
    }
}

/// Leading chars pulldown-cmark strips from an indented code block's
/// raw line before it lands in `Block::CodeBlock::content`: one tab,
/// or up to four spaces.
fn code_indent_strip_chars(raw_line: &str) -> usize {
    if raw_line.starts_with('\t') {
        return 1;
    }
    raw_line.chars().take(4).take_while(|c| *c == ' ').count()
}

/// Post-render pass: paint every visible search match over the
/// rendered document.  Called by `EditorView` after the Preview /
/// Rendered widget render — both walk `parsed.lines` with the same
/// wrap, so one overlay walk serves both view modes.  The focused
/// match gets the full `theme.selection` treatment; all others recede
/// onto the muted `theme.selection_muted` wash.  No-op outside a
/// search flow.
///
/// Every range is clamped against the live source (`partition_point`
/// bounds + the byte-length guard) so a stale match list — possible
/// for one frame after an external content swap — skips rather than
/// panics.
pub(crate) fn paint_search_overlays(
    editor: &EditorState,
    buf: &mut TuiBuf,
    area: Rect,
    theme: &Theme,
) {
    let Some(search) = editor.search.as_ref() else {
        return;
    };
    // A live `:s` preview may have rewritten the buffer, so any search
    // session's byte ranges are stale against the previewed text (its
    // freshness refresh is paused too).  Suspend the wash — it reappears
    // untouched once the preview reverts.
    if editor.substitute_preview.is_some() {
        return;
    }
    if search.matches.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let source_len = editor.buffer.rope().len_bytes();
    let width = area.width as usize;
    let (mut line_idx, mut first_sub_row) =
        editor.rendered_line_at_visual_row(editor.scroll, width.max(1));
    let mut vis_y: u16 = 0;
    while vis_y < area.height {
        if line_idx >= editor.parsed.lines.len() {
            break;
        }
        let rows = editor
            .parsed
            .visual_rows_for_line_at(line_idx, width)
            .max(1);
        let painted = rows
            .saturating_sub(first_sub_row)
            .min((area.height - vis_y) as usize);
        if painted == 0 {
            break;
        }
        let block_range = editor
            .parsed
            .source_map
            .original_byte_for_rendered_line(line_idx)
            .and_then(|b| editor.parsed.source_map.original_range_for_byte(b));
        if let Some(block_range) = block_range {
            // Matches are sorted; jump to the first that could touch
            // this block and stop at the first past it.
            let start = search
                .matches
                .partition_point(|m| m.end <= block_range.start);
            for (i, m) in search.matches.iter().enumerate().skip(start) {
                if m.start >= block_range.end {
                    break;
                }
                if m.end > source_len {
                    continue;
                }
                let style = if i == search.focused_idx {
                    theme.selection
                } else {
                    theme.selection_muted
                };
                paint_byte_range_overlay(
                    editor,
                    buf,
                    area,
                    vis_y,
                    painted as u16,
                    first_sub_row,
                    line_idx,
                    m.start,
                    m.end,
                    style,
                );
            }
        }
        vis_y += painted as u16;
        line_idx += 1;
        first_sub_row = 0;
    }
}

/// Post-render pass: paint the live `:s` substitution preview's highlight
/// ranges over the rendered document — match ranges while the pattern is
/// being typed, the inserted replacement segments once the replacement
/// field exists (the buffer already shows the substituted text; this wash
/// marks what changed).  Shares the visible-line walk with
/// [`paint_search_overlays`], but with a single style (`theme.selection`)
/// for every range — the preview has no focus concept, matching nvim's
/// one `Substitute` highlight group.  No-op outside a preview session.
pub(crate) fn paint_substitute_preview_overlays(
    editor: &EditorState,
    buf: &mut TuiBuf,
    area: Rect,
    theme: &Theme,
) {
    let Some(preview) = editor.substitute_preview.as_ref() else {
        return;
    };
    if preview.highlights.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let source_len = editor.buffer.rope().len_bytes();
    let width = area.width as usize;
    let (mut line_idx, mut first_sub_row) =
        editor.rendered_line_at_visual_row(editor.scroll, width.max(1));
    let mut vis_y: u16 = 0;
    while vis_y < area.height {
        if line_idx >= editor.parsed.lines.len() {
            break;
        }
        let rows = editor
            .parsed
            .visual_rows_for_line_at(line_idx, width)
            .max(1);
        let painted = rows
            .saturating_sub(first_sub_row)
            .min((area.height - vis_y) as usize);
        if painted == 0 {
            break;
        }
        let block_range = editor
            .parsed
            .source_map
            .original_byte_for_rendered_line(line_idx)
            .and_then(|b| editor.parsed.source_map.original_range_for_byte(b));
        if let Some(block_range) = block_range {
            // Ranges are sorted; jump to the first that could touch
            // this block and stop at the first past it.
            let start = preview
                .highlights
                .partition_point(|r| r.end <= block_range.start);
            for r in preview.highlights.iter().skip(start) {
                if r.start >= block_range.end {
                    break;
                }
                if r.end > source_len {
                    continue;
                }
                paint_byte_range_overlay(
                    editor,
                    buf,
                    area,
                    vis_y,
                    painted as u16,
                    first_sub_row,
                    line_idx,
                    r.start,
                    r.end,
                    theme.selection,
                );
            }
        }
        vis_y += painted as u16;
        line_idx += 1;
        first_sub_row = 0;
    }
}

/// Post-render pass: paint the recently-yanked span as a brief highlight
/// "flash" over the rendered document, confirming a `y` operation the way
/// neovim's yank highlight does.  Shares the same visible-line walk as
/// [`paint_search_overlays`] but for the single [`EditorState::yank_flash`]
/// range, using `theme.selection`.  No-op once the flash window has
/// elapsed (`active_yank_flash` returns `None`).
pub(crate) fn paint_yank_flash(editor: &EditorState, buf: &mut TuiBuf, area: Rect, theme: &Theme) {
    let Some(flash) = editor.active_yank_flash() else {
        return;
    };
    if area.width == 0 || area.height == 0 {
        return;
    }
    let source_len = editor.buffer.rope().len_bytes();
    if flash.start >= flash.end || flash.end > source_len {
        return;
    }
    let width = area.width as usize;
    let (mut line_idx, mut first_sub_row) =
        editor.rendered_line_at_visual_row(editor.scroll, width.max(1));
    let mut vis_y: u16 = 0;
    while vis_y < area.height {
        if line_idx >= editor.parsed.lines.len() {
            break;
        }
        let rows = editor
            .parsed
            .visual_rows_for_line_at(line_idx, width)
            .max(1);
        let painted = rows
            .saturating_sub(first_sub_row)
            .min((area.height - vis_y) as usize);
        if painted == 0 {
            break;
        }
        let block_range = editor
            .parsed
            .source_map
            .original_byte_for_rendered_line(line_idx)
            .and_then(|b| editor.parsed.source_map.original_range_for_byte(b));
        if let Some(block_range) = block_range {
            if block_range.start < flash.end && block_range.end > flash.start {
                paint_byte_range_overlay(
                    editor,
                    buf,
                    area,
                    vis_y,
                    painted as u16,
                    first_sub_row,
                    line_idx,
                    flash.start,
                    flash.end,
                    theme.selection,
                );
            }
        }
        vis_y += painted as u16;
        line_idx += 1;
        first_sub_row = 0;
    }
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
    // The block-cursor style when the cursor is visible this frame, or `None`
    // when it's blinked off / the cursor isn't in this cell's row.
    cursor: Option<Style>,
) {
    if visual_y >= area.height {
        return;
    }
    let abs_y = area.y + visual_y;
    let cell_width = overlay.rendered_end.saturating_sub(overlay.rendered_start);
    let raw_chars: Vec<char> = overlay.raw_text.chars().collect();
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
        // Block cursor: recolor the cell, leaving the char visible.
        if let Some(cursor_style) = cursor.filter(|_| overlay.cursor_in_cell == Some(i)) {
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
    fn make_raw_line_keeps_source_text_verbatim() {
        // The cursor is no longer baked into the line (it is painted onto the
        // resolved cell by the render override), so the line content is exactly
        // the source text — no substituted glyph, no appended cursor cell.
        let theme = Theme::default();
        let line = make_raw_line("hello", &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "hello");
    }

    #[test]
    fn make_raw_line_with_selection_paints_range() {
        let theme = Theme::default();
        let line = make_raw_line_with_selection("hello", Some((1, 3)), &theme);
        // Cols 1..3 carry the selection background; others don't.
        assert_eq!(line.spans[1].style.bg, theme.selection.bg);
        assert_eq!(line.spans[2].style.bg, theme.selection.bg);
        assert_ne!(line.spans[0].style.bg, theme.selection.bg);
    }
}
