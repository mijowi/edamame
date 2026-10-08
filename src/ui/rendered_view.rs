mod cell_overlay;
mod paint;
mod raw_text;

use ratatui::{buffer::Buffer as TuiBuf, layout::Rect, style::Style, widgets::StatefulWidget};

use crate::config::Theme;
use crate::editor::vim_ops::VisualKind;
use crate::editor::EditorState;
use crate::markdown::table_layout::{compute_cell_overlay, table_raw_col_to_rendered};

use super::image_view::{self, ImageLayoutSnapshot};
use super::line_render::{
    render_line_from_visual, render_line_reporting_cursor, render_line_with_cursor_from_visual,
};
use super::link_view::{self, LinkLayoutSnapshot};
use super::table_view::{self, TableLayoutSnapshot};

use self::cell_overlay::{compute_cell_chunk_overlay, compute_wrapped_cell_overlay};
use self::paint::{
    make_code_styled_body_line, make_raw_line_over, make_raw_line_with_selection, overlay_raw_cell,
    paint_byte_range_overlay, Overlay,
};

pub(crate) use self::paint::{
    paint_search_overlays, paint_substitute_preview_overlays, paint_yank_flash,
};
pub(crate) use self::raw_text::{
    block_source, cursor_block_pos, raw_block_cursor, raw_source_lines, revealed_source_line_count,
};
use self::raw_text::{raw_line_byte_start, raw_line_sel_cols, text_sel_cols};

/// State for the `RenderedView` widget; owned by `EditorViewState`, updated every frame.
#[derive(Debug, Default)]
pub struct RenderedViewState {
    /// First visible rendered line (scroll offset).
    pub scroll: usize,
    /// Every visible table, captured at the end of the last render, for mouse hit-testing.
    pub table_snapshots: Vec<TableLayoutSnapshot>,
    /// Every visible `Block::ImageBlock`, captured at the end of the last render, for
    /// mouse hit-testing.
    pub image_snapshots: Vec<ImageLayoutSnapshot>,
    /// Cache key for `image_snapshots`: `(scroll, area, parsed_version)`. A match reuses the
    /// vector instead of repeating the O(lines × images) geometry scan.
    pub image_snapshots_key: Option<(usize, Rect, u64)>,
    /// Every visible Markdown link, captured at the end of the last render, for mouse
    /// hit-testing (plain click in Preview, Ctrl-click in Rendered/Raw).
    pub link_snapshots: Vec<LinkLayoutSnapshot>,
    /// Cache key for `link_snapshots`, as `image_snapshots_key`. The uncached build calls
    /// `visual_rows_for_line` for every visible line, which dominated idle CPU.
    pub link_snapshots_key: Option<(usize, Rect, u64)>,
    /// Cache key for `table_snapshots`: `(scroll, area, parsed_version, show_handles)`.
    pub table_snapshots_key: Option<(usize, Rect, u64, bool)>,
    /// Absolute `(x, y)` cell where the cursor indicator was painted this frame, or `None`
    /// when a raw-reveal / cell-overlay path composited the cursor itself. `EditorView`
    /// re-stamps this cell after the search / selection overlays so they can't bury it.
    pub cursor_screen: Option<(u16, u16)>,
}

/// Hybrid rendered/raw editing view: every block is styled Markdown except the cursor's,
/// which shows raw source with an inline cursor.
pub struct RenderedView<'a> {
    pub state: &'a EditorState,
    pub theme: &'a Theme,
    /// Paint the `⠿` / `⇔` / `✕` table buttons. Reflects `config.table.show_buttons` AND
    /// `capabilities.mouse` (the App zeros the first when the second is false). Buttons paint
    /// only on the cursor's table (`paint_handles_for_cursor_table` in `table_view`).
    pub show_table_buttons: bool,
    /// In-progress table drag to highlight after the handles are painted.
    pub drop_indicator: Option<crate::ui::table_view::DropIndicator>,
    /// Active vim Visual flavor: the half-open `selection` is widened for the overlay paint
    /// via `vim_ops::visual_span` (inclusive of the cursor char in charwise, whole rows in
    /// VisualLine); `selection` itself is never snapped.
    pub visual_kind: Option<VisualKind>,
    /// Block-cursor style for this frame, already resolved for view mode and vim sub-mode
    /// (`app::cursor_style`).
    pub cursor_style: Style,
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

        // With `parsed_dirty` set, `source_map` ranges are stale, so use the cached
        // `cursor_block_idx` / `cursor_block_line_range` (in-line edits don't cross a block
        // boundary). When fresh, consult `source_map` directly so tests that set
        // `cursor.offset` without `update_cursor_block` see the real block.
        let use_cache = editor.parsed_dirty;
        let cursor_block_idx = if use_cache {
            editor.cursor_block_idx.unwrap_or_else(|| {
                editor
                    .parsed
                    .source_map
                    .block_for_byte(cursor_byte)
                    .unwrap_or(0)
            })
        } else {
            editor
                .parsed
                .source_map
                .block_for_byte(cursor_byte)
                .unwrap_or(0)
        };
        let cursor_block_lines = editor
            .parsed
            .source_map
            .rendered_lines_for_block(cursor_block_idx);
        let cursor_block_own = editor.parsed.block_own_line_count(cursor_block_idx);

        // The raw line index is an index *into* this source, so derive both together. A stale
        // parse rebuilds from the cached buffer-line range so unparsed typing is visible;
        // otherwise `raw_block_cursor` reads `cursor_block_pos`, the same derivation
        // `editor::state::cursor_raw_line` (and so `cursor_rendered_line_idx` and the click
        // mapping) reads.
        let (raw_block_source, cursor_raw_line, cursor_col) =
            match (use_cache, editor.cursor_block_line_range.clone()) {
                (true, Some(range)) => {
                    let mut out = String::new();
                    for line in range.clone() {
                        if let Some(text) = editor.buffer.line(line) {
                            out.push_str(&text);
                        }
                    }
                    while out.ends_with('\n') {
                        out.pop();
                    }
                    let (buffer_line, col) = editor.cursor.line_col(&editor.buffer);
                    (out, buffer_line.saturating_sub(range.start), col)
                }
                (true, None) => (String::new(), 0, 0),
                _ => {
                    let raw = raw_block_cursor(editor);
                    (raw.source, raw.raw_line, raw.col)
                }
            };

        let raw_lines: Vec<&str> = raw_source_lines(&raw_block_source);

        // `None` on a blank line, including one the block above's range absorbs, so a blank
        // after a quote doesn't take its wash.
        let cursor_block_ast = editor.parsed.real_block_for_byte(cursor_byte);
        let is_setext = cursor_block_ast.is_some_and(crate::markdown::Block::is_setext_heading);
        // Diagram blocks (mermaid fences and `$$...$$` math) are synthetic `Block::ImageBlock`s;
        // with the cursor inside, every reserved row shows the corresponding raw line, like a
        // fenced code block.
        let is_diagram_block = editor.parsed.is_diagram_reveal_block(cursor_block_idx);
        let is_mermaid_block = editor.parsed.is_mermaid_block(cursor_block_idx);
        let is_latex_block = editor.parsed.is_latex_block(cursor_block_idx);
        // Big-text H1 (4 big-text rows + rule vs. the plain 2-line H1) collapses to the raw
        // `# Title` line plus the rendered rule while the cursor is inside.
        let is_big_h1_block = matches!(
            cursor_block_ast,
            Some(crate::markdown::Block::Heading {
                level: pulldown_cmark::HeadingLevel::H1,
                ..
            })
        ) && cursor_block_own > 2;
        // A revealed row inside a blockquote keeps the quote's wash, or the line being edited
        // drops out of the quote it visibly belongs to.
        let reveal_base = if matches!(
            cursor_block_ast,
            Some(crate::markdown::Block::BlockQuote { .. })
        ) {
            self.theme.blockquote_text
        } else {
            self.theme.normal
        };
        // The row showing the cursor's source position, asked the way `cursor_rendered_line_idx`
        // (and the mouse hit-test's revealed-line shortcut) ask it, so all agree on which row
        // shows raw source.
        let cursor_in_block = crate::document::row_map::row_for_pos(
            &editor.parsed,
            cursor_block_idx,
            crate::document::row_map::RawPos {
                line: cursor_raw_line,
                col: cursor_col,
            },
        );
        // Whether that row de-renders: every row but one shown verbatim (a code body), asked of
        // its origin exactly as the mouse hit-test asks, or a click mapped against raw text on a
        // row the view never revealed lands on the wrong character.
        let cursor_row_reveals =
            crate::document::row_map::reveals(&editor.parsed, cursor_block_idx, cursor_in_block);
        // Whether the cursor's row is a table's (its cells, or a border or separator), from
        // its origin, as the click asks it: a table row reveals cell by cell, keeping its chrome.
        let cursor_table =
            crate::document::row_map::table_row(&editor.parsed, cursor_block_idx, cursor_in_block);
        let is_table = cursor_table.is_some();
        // Data-row cell in a row that wraps: one raw chunk per rendered sub. `None` for
        // non-data and single-sub rows, which the single-line overlays handle.  The row must
        // show the cursor's own line, which `cursor_col` counts along (a stale parse can name
        // another).
        let wrapped_cell = cursor_table
            .as_ref()
            .filter(|t| t.cells && t.index >= 1 && t.line == cursor_raw_line)
            .and_then(|t| {
                compute_wrapped_cell_overlay(
                    editor,
                    cursor_block_lines.start + t.rows.start..cursor_block_lines.start + t.rows.end,
                    raw_lines.get(cursor_raw_line).copied().unwrap_or(""),
                    cursor_col,
                )
            });

        let cursor_rendered_line = match &wrapped_cell {
            Some(w) => w.row_first_line_idx + w.cursor_sub,
            None => cursor_block_lines.start + cursor_in_block,
        };

        // Recorded by the indicator path below so `EditorView` can re-stamp it over overlays.
        view_state.cursor_screen = None;

        view_state.scroll = editor.scroll;
        let scroll = view_state.scroll;
        // Stack-aware start: `EffectiveRows` is the identity unless the cursor rests on a revealed
        // stacked row (a reflowed paragraph, any row over several lines), in which case its one
        // rendered line is replaced by its `M` raw source lines.  A viewport that opens *inside*
        // that row then starts on one of those raw lines (`start_raw`, block-relative), not the
        // rendered line.
        let effective = editor.effective_rows(area.width as usize);
        let stacked_range = effective.block_rendered();
        let (mut virtual_idx, mut first_sub_row, mut start_raw): (
            usize,
            usize,
            Option<(usize, usize)>,
        ) = match effective.line_at_visual_row(scroll) {
            crate::editor::effective_rows::RowHit::Rendered { line, sub } => (line, sub, None),
            crate::editor::effective_rows::RowHit::Raw { raw_line, sub } => (
                stacked_range.as_ref().map(|r| r.start).unwrap_or(0),
                0,
                Some((raw_line, sub)),
            ),
        };

        // Jitter suppression: keep the block rendered until the reveal delay elapses.
        let reveal_raw = editor.cursor_block_revealed();
        // Where the cursor shows on a table row drawn formatted (before the reveal fires, or while
        // search, a `:s` preview or a drag holds it off): on the glyph it is on, the row line and
        // char column of `table_raw_col_to_rendered`, which is also where a click there puts it.
        // Hidden markers make that differ from where the reveal then draws it, in raw text, and in
        // a wrapped cell the sub-line can differ too.
        let table_indicator = cursor_table
            .as_ref()
            .filter(|t| t.cells && t.line == cursor_raw_line)
            .and_then(|t| {
                let rows =
                    cursor_block_lines.start + t.rows.start..cursor_block_lines.start + t.rows.end;
                let (sub, col) = table_raw_col_to_rendered(
                    raw_lines.get(cursor_raw_line).copied().unwrap_or(""),
                    editor.parsed.lines.get(rows.clone())?,
                    cursor_col,
                    editor.parsed.ref_labels(),
                )?;
                Some((rows.start + sub, col))
            });
        let indicator_line = match table_indicator {
            Some((line, _)) if !reveal_raw => line,
            _ => cursor_rendered_line,
        };
        let cursor_visible = editor.cursor_visible();

        let cursor_indicator_style = self.cursor_style;

        let total_rendered = editor.parsed.lines.len();
        let wrap = true;

        // The highlighted byte range, intersected per line below: the selection, else the yank
        // flash.  Both are painted here, not in a post-pass, because a revealed row shows raw text
        // and a stacked reveal changes row heights, which only this loop knows: each reveal branch
        // highlights its own raw text, and the overlay below takes every other row.  Yanking ends
        // a visual selection, so the two rarely coexist.
        let highlight_bytes = editor
            .selection
            .map(|s| {
                let r = crate::editor::vim_ops::visual_span(&s, &editor.buffer, self.visual_kind);
                let rope = editor.buffer.rope();
                (rope.char_to_byte(r.start), rope.char_to_byte(r.end))
            })
            .or_else(|| editor.active_yank_flash().map(|f| (f.start, f.end)));
        let block_range_for_cursor = editor
            .parsed
            .source_map
            .original_range_for_byte(cursor_byte);
        // A revealed diagram's (mermaid, `$$…$$`) row `row` in block: the source line it paints
        // (`None` for a math-preview band row or padding past the source), that line's raw text,
        // and its highlighted columns.
        let diagram_row = |row: usize| {
            let src_idx = crate::document::row_map::revealed_diagram_line(
                &editor.parsed,
                cursor_block_idx,
                row,
            )
            .filter(|&s| s < raw_lines.len());
            let raw_text = src_idx.map_or("", |s| raw_lines[s]);
            let sel_cols = src_idx
                .zip(highlight_bytes)
                .zip(block_range_for_cursor.as_ref())
                .and_then(|((s, sel), block)| {
                    raw_line_sel_cols(&raw_block_source, block.start, s, raw_text, sel)
                });
            (src_idx, raw_text, sel_cols)
        };

        let mut vis_y: usize = 0;
        while vis_y < height {
            if virtual_idx >= total_rendered {
                break;
            }

            let skip_rows = first_sub_row;
            let rows_used;
            let in_cursor_block =
                virtual_idx >= cursor_block_lines.start && virtual_idx < cursor_block_lines.end;
            // Index into `wrapped_cell.subs` when `virtual_idx` is one of the wrapped cell's
            // chunks.
            let wrapped_sub_idx_opt: Option<usize> = wrapped_cell.as_ref().and_then(|w| {
                let end = w.row_first_line_idx + w.subs.len();
                if virtual_idx >= w.row_first_line_idx && virtual_idx < end {
                    Some(virtual_idx - w.row_first_line_idx)
                } else {
                    None
                }
            });
            let stacked_here = reveal_raw
                && stacked_range
                    .as_ref()
                    .is_some_and(|r| r.contains(&virtual_idx));
            if stacked_here {
                // A stacked row (a reflowed paragraph, any row over several lines) reveals as its
                // raw source lines: the rendered form was one wrapped row, the raw form is `M`
                // source lines, so paint them all in this one iteration (the row is a single
                // rendered line, so `virtual_idx` then advances straight past it).  Each is the
                // whole source line, container prefix (`> `, `- `, indent) included, as every
                // revealed row is.  `EffectiveRows` already made scroll, gutter, and mouse count
                // these rows, and names the lines (block-relative: a nested row's start past its
                // block's first line).  Only the first painted raw line honors `skip_rows`, for a
                // viewport opening mid-row.
                let stack = effective.raw_lines();
                let (first_raw, first_sub) = start_raw.take().unwrap_or((stack.start, 0));
                let block_start = block_range_for_cursor.as_ref().map(|r| r.start);
                let mut used = 0usize;
                for raw_idx in stack.filter(|&l| l >= first_raw) {
                    if vis_y + used >= height {
                        break;
                    }
                    let raw_text = raw_lines.get(raw_idx).copied().unwrap_or("");
                    let sub_skip = if raw_idx == first_raw { first_sub } else { 0 };
                    let sel_cols = highlight_bytes.zip(block_start).and_then(|(sel, bs)| {
                        raw_line_sel_cols(&raw_block_source, bs, raw_idx, raw_text, sel)
                    });
                    let styled = make_raw_line_over(raw_text, sel_cols, self.theme, reveal_base);
                    let cursor_override = (cursor_visible && raw_idx == cursor_raw_line)
                        .then_some((cursor_col, cursor_indicator_style));
                    let rows = render_line_with_cursor_from_visual(
                        &styled,
                        area,
                        buf,
                        (vis_y + used) as u16,
                        wrap,
                        cursor_override,
                        sub_skip,
                    ) as usize;
                    used += rows;
                }
                rows_used = used;
            } else if reveal_raw && is_big_h1_block && in_cursor_block {
                // `# Title` on the first sub-line, the other big-text rows blanked, the last
                // sub-line keeps the rendered rule.
                let sub = virtual_idx - cursor_block_lines.start;
                let last_sub = cursor_block_own.saturating_sub(1);
                if sub == last_sub {
                    if let Some(line) = editor.parsed.lines.get(virtual_idx) {
                        rows_used =
                            render_line_from_visual(line, area, buf, vis_y as u16, wrap, skip_rows)
                                as usize;
                    } else {
                        rows_used = 1;
                    }
                } else {
                    let raw_text = if sub == 0 {
                        raw_lines.first().copied().unwrap_or("")
                    } else {
                        ""
                    };
                    let cursor_on_this = sub == 0 && cursor_raw_line == 0;
                    let sel_cols = highlight_bytes
                        .filter(|_| sub == 0)
                        .zip(block_range_for_cursor.as_ref())
                        .and_then(|(sel, block)| {
                            raw_line_sel_cols(&raw_block_source, block.start, 0, raw_text, sel)
                        });
                    let styled = make_raw_line_with_selection(raw_text, sel_cols, self.theme);
                    let cursor_override = (cursor_on_this && cursor_visible)
                        .then_some((cursor_col, cursor_indicator_style));
                    rows_used = render_line_with_cursor_from_visual(
                        &styled,
                        area,
                        buf,
                        vis_y as u16,
                        wrap,
                        cursor_override,
                        skip_rows,
                    ) as usize;
                }
            } else if reveal_raw && is_setext && in_cursor_block {
                // Every rendered row of the block reveals the raw line it shows: the rule its
                // underline, a one-line text row its line.  A multi-line text row is stacked
                // (above), with the cursor anywhere in the heading (`row_map::cursor_stack`).
                let row = virtual_idx - cursor_block_lines.start;
                let sub =
                    crate::document::row_map::line_for_row(&editor.parsed, cursor_block_idx, row);
                let raw_text = raw_lines.get(sub).copied().unwrap_or("");
                let cursor_on_this = cursor_raw_line == sub;
                let sel_cols = highlight_bytes.and_then(|sel| {
                    let block_start = block_range_for_cursor.as_ref()?.start;
                    raw_line_sel_cols(&raw_block_source, block_start, sub, raw_text, sel)
                });
                let styled = make_raw_line_with_selection(raw_text, sel_cols, self.theme);
                let cursor_override = (cursor_on_this && cursor_visible)
                    .then_some((cursor_col, cursor_indicator_style));
                rows_used = render_line_with_cursor_from_visual(
                    &styled,
                    area,
                    buf,
                    vis_y as u16,
                    wrap,
                    cursor_override,
                    skip_rows,
                ) as usize;
            } else if reveal_raw && is_mermaid_block && in_cursor_block {
                // Reveal as a regular fenced code block: row 0 shows the language label (or
                // the raw fence when the cursor is on it), body rows raw source on the code
                // background, the closing fence a padded placeholder (or the raw fence with
                // cursor), and rows past the source padded so the reservation reads as one
                // block.
                let (src_idx, raw_text, sel_cols) =
                    diagram_row(virtual_idx - cursor_block_lines.start);
                let cursor_on_this = src_idx == Some(cursor_raw_line);
                let last_raw_idx = raw_lines.len().saturating_sub(1);
                let is_opening_fence_row = src_idx == Some(0) && raw_lines.len() >= 2;
                let is_closing_fence_row = src_idx == Some(last_raw_idx)
                    && raw_lines.len() >= 2
                    && raw_text.trim() == "```";
                let in_source = src_idx.is_some();
                let width = area.width as usize;

                let styled = if cursor_on_this && (is_opening_fence_row || is_closing_fence_row) {
                    make_raw_line_with_selection(raw_text, sel_cols, self.theme)
                } else if is_opening_fence_row {
                    // Falls back to raw source for a non-`mermaid` language.
                    let lang = raw_text.trim_start_matches(['`', '~']);
                    let lang = if lang.is_empty() { "mermaid" } else { lang };
                    ratatui::text::Line::styled(format!(" {} ", lang), self.theme.code_block_lang)
                } else if is_closing_fence_row {
                    ratatui::text::Line::styled(
                        "\u{00A0}".repeat(width.max(1)),
                        self.theme.code_block_text,
                    )
                } else if in_source {
                    make_code_styled_body_line(raw_text, sel_cols, self.theme)
                } else {
                    ratatui::text::Line::styled(
                        "\u{00A0}".repeat(width.max(1)),
                        self.theme.code_block_text,
                    )
                };

                // The cursor is painted onto the resolved cell so the wrap comes from the
                // bare source text.
                let cursor_override = (cursor_on_this && cursor_visible)
                    .then_some((cursor_col, cursor_indicator_style));
                rows_used = render_line_with_cursor_from_visual(
                    &styled,
                    area,
                    buf,
                    vis_y as u16,
                    wrap,
                    cursor_override,
                    skip_rows,
                ) as usize;
            } else if reveal_raw && is_latex_block && in_cursor_block {
                // `$$...$$` math blocks reveal as a code block, styled like
                // the mermaid fence: the opening `$$` becomes a ` math `
                // language header (`code_block_lang`), the body rows carry
                // the code surface (`code_block_text`), and the closing `$$`
                // is a padded blank row on that same surface.  The opening /
                // closing rows reveal their literal `$$` only when the
                // cursor lands on them (like a fence's edges).
                //
                // With the math preview on, the reveal reserves a top band
                // for the rendered formula and paints the source *below* it
                // (so the image keeps the block's top edge and doesn't
                // jump): band rows paint empty — `image_view` overlays the
                // formula there — and the source rows shift down by `band`
                // (`row_map::revealed_diagram_line`).  With the preview
                // off there is no band and the source paints from row 0.
                // Either way the formula's URL hashes its source, so moving
                // the cursor out collapses the block back to a freshly
                // rendered image.
                let (src_idx, raw_text, sel_cols) =
                    diagram_row(virtual_idx - cursor_block_lines.start);
                let cursor_on_this = src_idx == Some(cursor_raw_line);
                // Delimiter rows only when the block has separate opening /
                // closing lines (a one-line `$$x$$` is neither).  The
                // closing `$$` is matched by its text, not by `len - 1`: a
                // math paragraph's byte range can absorb the blank line that
                // follows it, so `raw_lines` may carry a trailing empty
                // entry past the real closing delimiter — indexing the last
                // entry would miss it and leave the closing `$$` styled as a
                // body row.
                let is_opening = src_idx == Some(0) && raw_lines.len() >= 2;
                let is_closing = !is_opening
                    && src_idx.is_some()
                    && raw_lines.len() >= 2
                    && raw_text.trim() == "$$";
                let width = area.width as usize;
                let styled = if src_idx.is_none() {
                    // Band row: transparent so the formula image shows.
                    make_raw_line_with_selection("", None, self.theme)
                } else if cursor_on_this && (is_opening || is_closing) {
                    // Cursor on a delimiter row: reveal the literal `$$`.
                    make_raw_line_with_selection(raw_text, sel_cols, self.theme)
                } else if is_opening {
                    // No cursor: ` math ` header, code-block language surface.
                    ratatui::text::Line::styled(" math ", self.theme.code_block_lang)
                } else if is_closing {
                    // No cursor: padded blank row on the code surface.
                    ratatui::text::Line::styled(
                        "\u{00A0}".repeat(width.max(1)),
                        self.theme.code_block_text,
                    )
                } else {
                    // Body row: code surface, cursor / selection per char.
                    make_code_styled_body_line(raw_text, sel_cols, self.theme)
                };
                let cursor_override = (cursor_on_this && cursor_visible)
                    .then_some((cursor_col, cursor_indicator_style));
                rows_used = render_line_with_cursor_from_visual(
                    &styled,
                    area,
                    buf,
                    vis_y as u16,
                    wrap,
                    cursor_override,
                    skip_rows,
                ) as usize;
            } else if let (true, Some(sub_idx)) = (reveal_raw, wrapped_sub_idx_opt) {
                // Paint the rendered row first (neighboring cells and borders stay), then
                // overlay this sub's raw chunk into the active cell.
                let w = wrapped_cell
                    .as_ref()
                    .expect("wrapped_sub_idx implies wrapped_cell");
                let overlay = &w.subs[sub_idx];
                if let Some(line) = editor.parsed.lines.get(virtual_idx) {
                    rows_used =
                        render_line_from_visual(line, area, buf, vis_y as u16, wrap, skip_rows)
                            as usize;
                    let sel_in_cell = highlight_bytes.and_then(|sel| {
                        let block_start = block_range_for_cursor.as_ref()?.start;
                        // Every chunk is a slice of the single raw row `cursor_raw_line`.
                        let cell_start = block_start
                            + raw_line_byte_start(&raw_block_source, cursor_raw_line)
                            + overlay.raw_cell_byte_start;
                        text_sel_cols(&overlay.raw_text, cell_start, sel)
                    });
                    overlay_raw_cell(
                        buf,
                        area,
                        vis_y as u16,
                        overlay,
                        sel_in_cell,
                        self.theme,
                        cursor_visible.then_some(self.cursor_style),
                    );
                } else {
                    rows_used = 1;
                }
            } else if reveal_raw && virtual_idx == cursor_rendered_line && cursor_row_reveals {
                let raw_text = raw_lines.get(cursor_raw_line).copied().unwrap_or("");
                // Table rows prefer a cell-scoped reveal, keeping borders and neighboring
                // cells rendered: `compute_cell_overlay` when the raw text fits, else
                // `compute_cell_chunk_overlay`, which scrolls the cell horizontally.
                let line_opt = editor.parsed.lines.get(virtual_idx);
                let cell_overlay = if is_table {
                    line_opt.and_then(|line| compute_cell_overlay(raw_text, line, cursor_col))
                } else {
                    None
                };
                let chunk_overlay = if is_table && cell_overlay.is_none() {
                    line_opt.and_then(|line| compute_cell_chunk_overlay(raw_text, line, cursor_col))
                } else {
                    None
                };
                if let Some(overlay) = cell_overlay.or(chunk_overlay) {
                    let line = &editor.parsed.lines[virtual_idx];
                    rows_used =
                        render_line_from_visual(line, area, buf, vis_y as u16, wrap, skip_rows)
                            as usize;

                    let sel_in_cell = highlight_bytes.and_then(|sel| {
                        let block_start = block_range_for_cursor.as_ref()?.start;
                        let cell_start = block_start
                            + raw_line_byte_start(&raw_block_source, cursor_raw_line)
                            + overlay.raw_cell_byte_start;
                        text_sel_cols(&overlay.raw_text, cell_start, sel)
                    });
                    overlay_raw_cell(
                        buf,
                        area,
                        vis_y as u16,
                        &overlay,
                        sel_in_cell,
                        self.theme,
                        cursor_visible.then_some(self.cursor_style),
                    );
                } else {
                    // Non-table block, or a pipe-mismatched table line (mid-edit alignment row).
                    let sel_cols = highlight_bytes.and_then(|sel| {
                        let block_start = block_range_for_cursor.as_ref()?.start;
                        raw_line_sel_cols(
                            &raw_block_source,
                            block_start,
                            cursor_raw_line,
                            raw_text,
                            sel,
                        )
                    });
                    let styled = make_raw_line_over(raw_text, sel_cols, self.theme, reveal_base);
                    let cursor_override =
                        cursor_visible.then_some((cursor_col, cursor_indicator_style));
                    rows_used = render_line_with_cursor_from_visual(
                        &styled,
                        area,
                        buf,
                        vis_y as u16,
                        wrap,
                        cursor_override,
                        skip_rows,
                    ) as usize;
                }
            } else if virtual_idx == indicator_line && (!reveal_raw || !cursor_row_reveals) {
                // Rendered line plus a cursor indicator: the jitter-delay window before
                // `reveal_raw` (drawing it now avoids a column jump when the reveal fires),
                // or a row that never de-renders (a code body, frontmatter).
                if let Some(line) = editor.parsed.lines.get(virtual_idx) {
                    // Where a click on that char would have put the cursor, inverted: the row's
                    // origin says how its columns relate to the cursor's line.
                    let visual_col = if is_table {
                        table_indicator.map_or(cursor_col, |(_, col)| col)
                    } else {
                        crate::document::row_map::raw_to_rendered_col_near(
                            &editor.parsed,
                            cursor_block_idx,
                            cursor_in_block,
                            crate::document::row_map::RawPos {
                                line: cursor_raw_line,
                                col: cursor_col,
                            },
                        )
                    };
                    let (rows, cursor_cell) = render_line_reporting_cursor(
                        line,
                        area,
                        buf,
                        vis_y as u16,
                        wrap,
                        if cursor_visible {
                            Some((visual_col, cursor_indicator_style))
                        } else {
                            None
                        },
                        skip_rows,
                    );
                    rows_used = rows as usize;
                    view_state.cursor_screen = cursor_cell;
                } else {
                    rows_used = 1;
                }
            } else {
                if let Some(line) = editor.parsed.lines.get(virtual_idx) {
                    rows_used =
                        render_line_from_visual(line, area, buf, vis_y as u16, wrap, skip_rows)
                            as usize;
                } else {
                    break;
                }
            }

            // Lines that painted their own highlight (the revealed line, big-H1 / setext / diagram
            // rows, wrapped-cell subs, the stacked reflow) are skipped.  A big H1's rule stays
            // rendered, so it takes the overlay.
            if let Some((sa, sb)) = highlight_bytes {
                let big_h1_revealed = reveal_raw
                    && is_big_h1_block
                    && in_cursor_block
                    && virtual_idx - cursor_block_lines.start < cursor_block_own.saturating_sub(1);
                let setext_revealed = reveal_raw && is_setext && in_cursor_block;
                let diagram_revealed = reveal_raw && is_diagram_block && in_cursor_block;
                let wrapped_revealed = reveal_raw && wrapped_sub_idx_opt.is_some();
                // Separate suppression cases; clippy's collapse hides which is which.
                #[allow(clippy::nonminimal_bool)]
                if !(reveal_raw && virtual_idx == cursor_rendered_line && cursor_row_reveals)
                    && !big_h1_revealed
                    && !setext_revealed
                    && !diagram_revealed
                    && !wrapped_revealed
                    && !stacked_here
                {
                    paint_byte_range_overlay(
                        editor,
                        buf,
                        area,
                        vis_y as u16,
                        rows_used as u16,
                        skip_rows,
                        virtual_idx,
                        sa,
                        sb,
                        self.theme.selection,
                        Overlay::Selection,
                    );
                }
            }

            if rows_used == 0 {
                break;
            }
            vis_y += rows_used;
            virtual_idx += 1;
            first_sub_row = 0;
        }

        // Snapshots are captured for every visible table (mouse hit-testing on adjacent
        // tables), but handles paint only on the cursor's table.
        table_view::build_snapshots_cached(
            self.state,
            area,
            self.show_table_buttons,
            &mut view_state.table_snapshots,
            &mut view_state.table_snapshots_key,
        );
        let cursor_table_start = if self.show_table_buttons {
            cursor_table_block_start(self.state, &view_state.table_snapshots)
        } else {
            None
        };
        table_view::paint_handles(
            &view_state.table_snapshots,
            area,
            buf,
            self.theme,
            cursor_table_start,
        );
        if let Some(indicator) = self.drop_indicator {
            table_view::paint_drop_indicator(
                &view_state.table_snapshots,
                &indicator,
                area,
                buf,
                self.theme,
            );
        }

        // Image painting itself happens in `EditorView::render`, which has the cache.
        image_view::build_snapshots_cached(
            self.state,
            area,
            self.state.scroll,
            &mut view_state.image_snapshots,
            &mut view_state.image_snapshots_key,
        );

        link_view::build_snapshots_cached(
            self.state,
            area,
            self.state.scroll,
            &mut view_state.link_snapshots,
            &mut view_state.link_snapshots_key,
        );
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Byte offset of the table block the cursor is inside, or `None`; gates the drag-handle
/// painter. Walks the snapshots rather than reparsing.
fn cursor_table_block_start(
    state: &EditorState,
    snapshots: &[crate::ui::table_view::TableLayoutSnapshot],
) -> Option<usize> {
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    snapshots
        .iter()
        .find(|s| cursor_byte >= s.table_byte_start && cursor_byte < s.table_byte_end)
        .map(|s| s.table_byte_start)
}
