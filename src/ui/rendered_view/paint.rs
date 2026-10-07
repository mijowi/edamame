use ratatui::{
    buffer::Buffer as TuiBuf,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::config::Theme;
use crate::editor::EditorState;
use crate::markdown::table_layout::{char_cells, CellOverlay};
use crate::ui::line_render;

use crate::document::row_map::{self, RawPos};
use crate::markdown::{ColOrigin, ContentKind};

/// [`make_raw_line_with_selection`] with no selection.
#[cfg(test)]
pub(super) fn make_raw_line(raw_text: &str, theme: &Theme) -> Line<'static> {
    make_raw_line_with_selection(raw_text, None, theme)
}

/// A `Line` of `raw_text` (the raw-revealed cursor block) with `selection_cols` (a `[start, end)`
/// char range) painted in the selection background.
///
/// The cursor is NOT embedded: `line_render` paints it onto the resolved cell, so the wrap is
/// computed from the bare source and matches the wrap the scroll/navigation code uses.
pub(super) fn make_raw_line_with_selection(
    raw_text: &str,
    selection_cols: Option<(usize, usize)>,
    theme: &Theme,
) -> Line<'static> {
    make_raw_line_over(raw_text, selection_cols, theme, theme.normal)
}

/// [`make_raw_line_with_selection`] over a caller-supplied `base` style, so a revealed line
/// inside a block with its own surface (a blockquote wash) keeps that surface. `base` is also
/// the line-level style, so `line_render`'s trailing-cell fill carries it to the viewport edge.
pub(super) fn make_raw_line_over(
    raw_text: &str,
    selection_cols: Option<(usize, usize)>,
    theme: &Theme,
    base: Style,
) -> Line<'static> {
    let sel_style = theme.selection;
    let chars: Vec<char> = raw_text.chars().collect();
    let total = chars.len();

    // One span per char keeps per-char styling predictable when cursor and selection overlap.
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

/// One body row of a mermaid block revealed as a code block, with `code_block_text` as the
/// line style so the trailing-cell fill extends the code background. Char positions stay 1:1
/// with `raw_text` (no leading pad) so click → raw col mapping needs no adjustment.
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

/// Which overlay [`paint_byte_range_overlay`] paints, which decides the two rows it can't map
/// exactly: a chrome row (a fence label, a rule, a big H1's glyph rows), which has no column
/// relation to the line it shows, and a content row no inline map renders exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Overlay {
    /// A selection or a yank: it covers the line a chrome row stands for, so the row washes
    /// whole; on an unmapped row it paints only what it covers to the row's ends, rather than
    /// a wrong span inside.
    Selection,
    /// A search match or a `:s` preview: it highlights only the text it matched, so a chrome
    /// row is left alone; on an unmapped row it shows at the one-for-one guess, as wide as the
    /// text it matched, since a match the user jumped to must show somewhere.
    Match,
}

/// Post-render pass: paint `style` over the rendered cells of one rendered line for the source
/// byte range `[sel_start_byte, sel_end_byte)`. Shared by the selection, search-match, `:s`
/// preview, and yank-flash overlays; `overlay` says which (see [`Overlay`]).
///
/// Intersects the range with the source lines *this rendered row* shows, then maps the covered
/// raw positions to rendered chars through the row's origin (`row_map`). Keep the per-row
/// clamp: multi-line ranges (a search match containing `\n`, a linewise selection) rely on it.
/// Lines come from the parse's line index (in `\n`s, as the origins count them; the rope also
/// breaks at `\r` and U+2028) and chars from the rope's, so a call costs the same in a block of
/// any size.
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
    overlay: Overlay,
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
    if block_range.end <= sel_start_byte || block_range.start >= sel_end_byte {
        return;
    }

    let rope = editor.buffer.rope();
    // With `parsed_dirty` set, an in-line edit may have shifted offsets so `block_range` runs past
    // the buffer or ends inside a multi-byte sequence: clamp, and let a slice off a char boundary
    // skip the frame.
    let block_end = block_range.end.min(rope.len_bytes());
    if block_range.start >= block_end {
        return;
    }
    let parsed = &editor.parsed;
    let first_line = parsed.byte_to_line(block_range.start);
    let Some(block_idx) = editor.parsed.source_map.block_for_byte(block_range.start) else {
        return;
    };
    // An image's reserved rows show no text to select.  A diagram's map 1:1 onto its source.
    if editor.parsed.is_image_block(block_idx) && !editor.parsed.is_diagram_reveal_block(block_idx)
    {
        return;
    }
    let rendered_span = editor.parsed.source_map.rendered_lines_for_block(block_idx);
    let sub_idx_in_block = rendered_line_idx.saturating_sub(rendered_span.start);
    let Some(line) = editor.parsed.lines.get(rendered_line_idx) else {
        return;
    };
    let actual_rendered: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();

    let line_start = |l: usize| match l {
        0 => block_range.start,
        _ => parsed.line_start_byte(first_line + l),
    };
    // A block line's end, its `\n` aside.
    let line_end = |l: usize| {
        (parsed.line_start_byte(first_line + l) + parsed.source_line(first_line + l).len())
            .min(block_end)
    };
    if let Some(hit) = row_map::table_row(parsed, block_idx, sub_idx_in_block) {
        // Borders and separators carry no raw-byte mapping.
        if !hit.cells {
            return;
        }
        let raw_line_start_abs = line_start(hit.line);
        let raw_line_end_abs = line_end(hit.line);
        let Some(raw_line) = rope
            .get_byte_slice(raw_line_start_abs.min(raw_line_end_abs)..raw_line_end_abs)
            .map(String::from)
        else {
            return;
        };
        let line_sel_start = sel_start_byte.max(raw_line_start_abs);
        let line_sel_end = sel_end_byte.min(raw_line_end_abs);
        if line_sel_start >= line_sel_end {
            return;
        }
        let col_at = |abs: usize| rope.byte_to_char(abs) - rope.byte_to_char(raw_line_start_abs);
        // A match's raw cols land in at most one wrap chunk per cell; mapping per `hit.sub`
        // keeps the highlight off sub-lines that don't show the matched text.
        for (rs, re) in crate::markdown::table_layout::table_raw_col_range_to_rendered_segments(
            &raw_line,
            line,
            col_at(line_sel_start),
            col_at(line_sel_end),
            hit.sub,
        ) {
            paint_cols_on_line(
                line, buf, area, y_start, rows_used, skip_rows, rs, re, style,
            );
        }
        return;
    }

    // The source lines the row shows, and the bytes they cover: from the first one's start (the
    // block's, on its first line) to the last one's end, its `\n` aside.
    let Some(lines) = row_map::lines_of_row(&editor.parsed, block_idx, sub_idx_in_block) else {
        return;
    };
    let (first, last) = (lines.start as usize, lines.end as usize - 1);
    let row_start = line_start(first);
    let row_end = line_end(last);
    let (sel_s, sel_e) = (sel_start_byte.max(row_start), sel_end_byte.min(row_end));
    if sel_s >= sel_e {
        return;
    }

    // A source byte of the block as `(line, col)`, columns on the first line counted from the
    // block's start.
    let pos_at = |abs: usize| {
        let line = parsed.byte_to_line(abs).saturating_sub(first_line);
        let from = line_start(line).min(abs);
        RawPos {
            line,
            col: rope.byte_to_char(abs) - rope.byte_to_char(from),
        }
    };
    // A diagram's rows show its source lines 1:1 when revealed (below any math-preview band,
    // which `lines_of_row` already skipped).
    if editor.parsed.is_diagram_reveal_block(block_idx) {
        let (s, e) = (pos_at(sel_s).col, pos_at(sel_e).col);
        if s < e {
            paint_cols_on_line(
                line,
                buf,
                area,
                y_start,
                rows_used,
                skip_rows,
                s.min(actual_rendered),
                e.min(actual_rendered),
                style,
            );
        }
        return;
    }
    let origin = &editor.parsed.row_origins()[rendered_line_idx];
    let ColOrigin::Content { kind, .. } = origin.cols else {
        // Chrome (a fence label, a rule) has no column relation: the whole row washes, or
        // nothing does.
        if overlay == Overlay::Match {
            return;
        }
        paint_cols_on_line(
            line,
            buf,
            area,
            y_start,
            rows_used,
            skip_rows,
            0,
            actual_rendered,
            style,
        );
        return;
    };
    let rendered =
        |abs: usize| row_map::raw_to_rendered_col(parsed, block_idx, sub_idx_in_block, pos_at(abs));
    // A selection reaching the row's first char (VisualLine, a fully covered line) paints its
    // prefix too: the marker, the bar.  A code row's prefix is its pad cell, which it never
    // painted.  One covering the row's last char paints its whole content even where no map
    // places the end exactly, so a line selection never vanishes.
    let mut rend_start = if sel_start_byte <= row_start && kind != ContentKind::Verbatim {
        Some(0)
    } else {
        rendered(sel_s)
    };
    let mut rend_end = rendered(sel_e).or((sel_end_byte >= row_end).then_some(actual_rendered));
    if overlay == Overlay::Match && (rend_start.is_none() || rend_end.is_none()) {
        // A match the user jumped to must show somewhere: at the one-for-one guess, as wide as
        // the text it matched, kept inside the row.
        let width = rope.byte_to_char(sel_e) - rope.byte_to_char(sel_s);
        let guess =
            row_map::raw_to_rendered_col_near(parsed, block_idx, sub_idx_in_block, pos_at(sel_s));
        let end = (guess + width).min(actual_rendered);
        rend_start = Some(end.saturating_sub(width));
        rend_end = Some(end);
    }
    // A selection with no exact map: skip rather than paint off by N.
    let (Some(rend_start), Some(rend_end)) = (rend_start, rend_end) else {
        return;
    };
    if rend_start >= rend_end {
        return;
    }
    paint_cols_on_line(
        line, buf, area, y_start, rows_used, skip_rows, rend_start, rend_end, style,
    );
}

/// Post-render pass: paint every visible search match; the focused one in `theme.selection`,
/// the rest in `selection_muted`. Called by `EditorView` for both Preview and Rendered, which
/// share the same wrap. Ranges are clamped against the live source so a stale match list
/// (one frame after an external content swap) skips rather than panics.
pub(crate) fn paint_search_overlays(
    editor: &EditorState,
    buf: &mut TuiBuf,
    area: Rect,
    theme: &Theme,
) {
    let Some(search) = editor.search.as_ref() else {
        return;
    };
    // A live `:s` preview rewrites the buffer, so search byte ranges are stale against it.
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
            // Matches are sorted: jump to the first that could touch this block.
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
                    Overlay::Match,
                );
            }
        }
        vis_y += painted as u16;
        line_idx += 1;
        first_sub_row = 0;
    }
}

/// Post-render pass: paint the live `:s` preview's highlight ranges (matches while typing the
/// pattern, inserted replacement segments once one exists). Same walk as
/// [`paint_search_overlays`], single style — the preview has no focus concept.
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
                    Overlay::Match,
                );
            }
        }
        vis_y += painted as u16;
        line_idx += 1;
        first_sub_row = 0;
    }
}

/// Post-render pass: neovim-style yank highlight over [`EditorState::yank_flash`]'s range.
/// Same walk as [`paint_search_overlays`].
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
                    Overlay::Selection,
                );
            }
        }
        vis_y += painted as u16;
        line_idx += 1;
        first_sub_row = 0;
    }
}

/// Paint `sel_bg` onto the rendered cells for rendered char cols in `[start_col, end_col)`,
/// with `y_start` relative to `area` — [`line_render::patch_char_cols`] in this view's
/// coordinates.
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
    line_render::patch_char_cols(
        line,
        buf,
        area,
        area.y + y_start,
        rows_used,
        skip_rows,
        start_col..end_col,
        sel_bg,
    );
}

/// Paint `overlay.raw_text` into the cell's rendered column range, directly into the buffer
/// (the underlying row must already be rendered). `selection_cols` is painted here because
/// the overlay clobbers whatever the generic selection pass already painted.
pub(super) fn overlay_raw_cell(
    buf: &mut TuiBuf,
    area: Rect,
    visual_y: u16,
    overlay: &CellOverlay,
    selection_cols: Option<(usize, usize)>,
    theme: &Theme,
    // Block-cursor style when visible this frame; `None` when blinked off or not in this row.
    cursor: Option<Style>,
) {
    if visual_y >= area.height {
        return;
    }
    let abs_y = area.y + visual_y;
    let area_end = area.x.saturating_add(area.width);
    // Strip `theme.normal`'s bg so the table-row stripe under the cell survives.
    let base_style = Style {
        bg: None,
        ..theme.normal
    };

    // Walk chars (indices address `raw_text`, then one blank cell each past its end) while
    // advancing by cells, so a wide glyph takes two and the right border stays put.
    let mut chars = overlay.raw_text.chars();
    let mut col = overlay.rendered_start;
    let mut i = 0usize;
    while col < overlay.rendered_end {
        let mut ch = chars.next().unwrap_or(' ');
        let mut w = char_cells(ch);
        if w == 0 {
            // Zero-width chars merge into the preceding glyph, as in `line_render::paint_row`.
            i += 1;
            continue;
        }
        if col + w > overlay.rendered_end {
            // Never straddle the border: blank the last cell instead.
            (ch, w) = (' ', 1);
        }
        let abs_x = area.x.saturating_add(col as u16);
        if abs_x >= area_end {
            break;
        }
        let mut style = base_style;
        if matches!(selection_cols, Some((s, e)) if i >= s && i < e) {
            style = style.patch(theme.selection);
        }
        if let Some(cursor_style) = cursor.filter(|_| overlay.cursor_in_cell == Some(i)) {
            style = cursor_style;
        }
        if let Some(cell) = buf.cell_mut((abs_x, abs_y)) {
            // `Cell::set_style` only adds/removes modifiers, so e.g. the BOLD of a rendered
            // `**x**` would bleed through the raw chars; zero them by hand.
            cell.modifier = Modifier::empty();
            cell.set_char(ch);
            cell.set_style(style);
        }
        col += w;
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::Theme;

    fn overlay(start: usize, end: usize, raw: &str, cursor: Option<usize>) -> CellOverlay {
        CellOverlay {
            rendered_start: start,
            rendered_end: end,
            raw_text: raw.to_owned(),
            cursor_in_cell: cursor,
            raw_cell_byte_start: 0,
        }
    }

    fn painted(ov: &CellOverlay, cursor: Style) -> TuiBuf {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = TuiBuf::empty(area);
        for x in 0..10 {
            buf[(x, 0)].set_char('x');
        }
        overlay_raw_cell(&mut buf, area, 0, ov, None, &Theme::default(), Some(cursor));
        buf
    }

    /// A wide raw glyph advances two cells, so the chars after it and the cursor land where
    /// the terminal draws them and the right border is untouched.
    #[test]
    fn overlay_raw_cell_advances_by_cells() {
        let cursor = Style::default().bg(ratatui::style::Color::Magenta);
        let buf = painted(&overlay(1, 6, "日a", Some(1)), cursor);
        assert_eq!(buf[(1, 0)].symbol(), "日");
        assert_eq!(buf[(3, 0)].symbol(), "a");
        assert_eq!(buf[(3, 0)].bg, ratatui::style::Color::Magenta);
        assert_eq!(buf[(4, 0)].symbol(), " ");
        assert_eq!(buf[(5, 0)].symbol(), " ");
        assert_eq!(
            buf[(6, 0)].symbol(),
            "x",
            "the border cell is not the overlay's"
        );
    }

    #[test]
    fn overlay_raw_cell_blanks_a_glyph_that_would_straddle_the_border() {
        let buf = painted(&overlay(1, 4, "ab日", None), Style::default());
        assert_eq!(buf[(3, 0)].symbol(), " ");
        assert_eq!(buf[(4, 0)].symbol(), "x");
    }

    /// A chrome row (here a fence's ` rust ` label) has no column relation to its line: a
    /// selection over the line washes it whole, a match on its text leaves it alone.
    #[test]
    fn a_chrome_row_washes_for_a_selection_and_not_for_a_match() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let src = "```rust\nx\n```\n";
        let editor = EditorState::new(crate::document::Buffer::from_str(src), theme);
        let style = Style::default().bg(ratatui::style::Color::Rgb(9, 8, 7));
        let washed = |overlay: Overlay| {
            let area = Rect::new(0, 0, 20, 1);
            let mut buf = TuiBuf::empty(area);
            paint_byte_range_overlay(&editor, &mut buf, area, 0, 1, 0, 0, 3, 7, style, overlay);
            (0..area.width)
                .filter(|&x| buf[(x, 0)].bg == ratatui::style::Color::Rgb(9, 8, 7))
                .count()
        };
        assert_eq!(washed(Overlay::Selection), " rust ".len());
        assert_eq!(washed(Overlay::Match), 0);
    }

    /// On a row no inline map renders exactly, a match still shows (one for one), while a
    /// selection inside the row skips rather than paint a wrong span.
    #[test]
    fn an_unmapped_row_shows_a_match_and_not_a_partial_selection() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        // pulldown-cmark folds `ss` and `ß` to one label; the maps' lowercasing doesn't, so the
        // row (`x[ss] y`) has no exact map.
        let src = "x[^ss] y\n\n[^ß]: n\n";
        let editor = EditorState::new(crate::document::Buffer::from_str(src), theme);
        let style = Style::default().bg(ratatui::style::Color::Rgb(9, 8, 7));
        let y = src.find('y').unwrap();
        let painted = |overlay: Overlay| {
            let area = Rect::new(0, 0, 20, 1);
            let mut buf = TuiBuf::empty(area);
            paint_byte_range_overlay(
                &editor,
                &mut buf,
                area,
                0,
                1,
                0,
                0,
                y,
                y + 1,
                style,
                overlay,
            );
            (0..area.width)
                .filter(|&x| buf[(x, 0)].bg == ratatui::style::Color::Rgb(9, 8, 7))
                .count()
        };
        assert_eq!(painted(Overlay::Match), 1);
        assert_eq!(painted(Overlay::Selection), 0);
    }

    #[test]
    fn make_raw_line_keeps_source_text_verbatim() {
        // The cursor is painted onto the resolved cell, not baked into the line.
        let theme = Theme::default();
        let line = make_raw_line("hello", &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "hello");
    }

    #[test]
    fn make_raw_line_with_selection_paints_range() {
        let theme = Theme::default();
        let line = make_raw_line_with_selection("hello", Some((1, 3)), &theme);
        assert_eq!(line.spans[1].style.bg, theme.selection.bg);
        assert_eq!(line.spans[2].style.bg, theme.selection.bg);
        assert_ne!(line.spans[0].style.bg, theme.selection.bg);
    }
}
