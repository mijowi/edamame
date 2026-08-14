mod cell_overlay;
mod paint;
mod raw_text;

use ratatui::{buffer::Buffer as TuiBuf, layout::Rect, style::Style, widgets::StatefulWidget};

use crate::config::Theme;
use crate::document::detect_setext;
use crate::editor::table_edit;
use crate::editor::vim_ops::VisualKind;
use crate::editor::EditorState;
use crate::markdown::table_layout::{compute_cell_overlay, table_raw_col_to_rendered_col};

use super::image_view::{self, ImageLayoutSnapshot};
use super::line_render::{
    render_line_from_visual, render_line_reporting_cursor, render_line_with_cursor_from_visual,
};
use super::link_view::{self, LinkLayoutSnapshot};
use super::table_view::{self, TableLayoutSnapshot};

use self::cell_overlay::{compute_cell_chunk_overlay, compute_wrapped_cell_overlay};
use self::paint::{
    make_code_styled_body_line, make_raw_line_with_selection, overlay_raw_cell,
    paint_byte_range_overlay,
};
use crate::markdown::list_layout::list_raw_col_to_rendered_col;

pub(crate) use self::paint::{
    paint_search_overlays, paint_substitute_preview_overlays, paint_yank_flash,
};
use self::raw_text::raw_line_byte_start;
pub(crate) use self::raw_text::{raw_block_cursor, raw_source_lines, revealed_source_line_count};

/// State for the `RenderedView` widget.
///
/// Owned by `EditorViewState`; updated every frame from `EditorState`.
#[derive(Debug, Default)]
pub struct RenderedViewState {
    /// First visible rendered line (scroll offset).
    pub scroll: usize,
    /// Snapshots of every visible table, captured at the end of the last
    /// render.  Used by mouse-event handling to hit-test against the
    /// columns, borders, and buttons of the table under the pointer.
    pub table_snapshots: Vec<TableLayoutSnapshot>,
    /// Snapshots of every visible `Block::ImageBlock`, captured at the end
    /// of the last render.  Used as the hit-test surface
    /// for images (click detection); future work may add expand /
    /// open UX.
    pub image_snapshots: Vec<ImageLayoutSnapshot>,
    /// Cache key for `image_snapshots`: `(scroll, area, parsed_version)`.
    /// When the tuple matches on the next frame, the snapshot vector is
    /// reused instead of recomputed — avoids the O(lines × images)
    /// geometry scan when nothing that affects image layout has changed.
    pub image_snapshots_key: Option<(usize, Rect, u64)>,
    /// Snapshots of every visible Markdown link, captured at the end of
    /// the last render.  Used by the mouse dispatch to hit-test
    /// against link spans — plain click in Preview or Ctrl-click in
    /// Rendered/Raw fires `FollowLink`.
    pub link_snapshots: Vec<LinkLayoutSnapshot>,
    /// Cache key for `link_snapshots`: `(scroll, area, parsed_version)`.
    /// Mirrors `image_snapshots_key`.  The link snapshot build walks
    /// `parsed.blocks` and calls `visual_rows_for_line` for every
    /// visible line — expensive on large documents — so skipping it
    /// when layout inputs haven't changed is a major per-frame win.
    pub link_snapshots_key: Option<(usize, Rect, u64)>,
    /// Cache key for `table_snapshots`:
    /// `(scroll, area, parsed_version, show_handles)`.  Skips the
    /// per-frame visible-line walk when nothing that affects table
    /// layout has changed — the same coalescing strategy used for
    /// images and links.
    pub table_snapshots_key: Option<(usize, Rect, u64, bool)>,
    /// Absolute `(x, y)` terminal cell where the rendered-mode cursor
    /// indicator was painted this frame, or `None` when the cursor was
    /// drawn via a raw-reveal / cell-overlay path (which composite the
    /// cursor themselves) or wasn't visible.  `EditorView` re-stamps this
    /// cell with the resolved cursor style after the search-match / selection
    /// overlays run, so an overlay can't bury the cursor.
    pub cursor_screen: Option<(u16, u16)>,
}

/// Hybrid rendered/raw editing view.
///
/// Every rendered block is shown as styled Markdown EXCEPT the block that
/// contains the cursor, which is replaced by the raw source text with an
/// inline cursor.
pub struct RenderedView<'a> {
    pub state: &'a EditorState,
    pub theme: &'a Theme,
    /// When true, the table renderer paints the row/column buttons —
    /// `⠿` reorder grips, `⇔` resize glyph, `✕` delete glyphs — over
    /// each visible table.  Controlled by `config.table.show_buttons`
    /// AND `capabilities.mouse` (the App zeros the first when the
    /// second is false), so terminals without mouse reporting never
    /// show inert glyphs.
    ///
    /// Buttons only paint on the table that contains the
    /// cursor — moving the cursor out of the table hides them so they
    /// never compete with the rendered content during navigation.  The
    /// gating is enforced by `paint_handles_for_cursor_table` in
    /// `table_view`.
    pub show_table_buttons: bool,
    /// When `Some`, an in-progress table drag is highlighted
    /// after the handles are painted.  `None` when no relevant drag is
    /// active.
    pub drop_indicator: Option<crate::ui::table_view::DropIndicator>,
    /// The active vim Visual flavor, if any: the stored half-open `selection`
    /// is widened for the overlay paint via `vim_ops::visual_span` — inclusive
    /// of the char under the cursor in charwise Visual, whole rows in
    /// VisualLine.  `selection` itself is never snapped, and `None` (default
    /// handler, or vim outside Visual) paints the raw span — see
    /// `docs/vim-implementation-plan.md` §2.6.
    pub visual_kind: Option<VisualKind>,
    /// Resolved block-cursor style for this frame, already accounting for
    /// the view mode and vim sub-mode (`app::cursor_style`).  Used for the
    /// inline cursor indicator and the table-cell cursor overlay.
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

        // When `parsed_dirty` is set, an in-line edit has left `source_map`
        // byte ranges stale — the cursor's live byte offset may no longer
        // fall inside its block's recorded range.  Use the cached
        // `cursor_block_idx` / `cursor_block_line_range` (captured at the
        // last cursor-move's `update_cursor_block`) which remain valid
        // because in-line edits don't cross a block boundary or shift
        // buffer line indices.  When the parse is fresh, consult
        // `source_map` directly so tests that set `cursor.offset`
        // without calling `update_cursor_block` still observe the
        // cursor's real block.
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

        // Raw source text for the cursor's block, and where the cursor sits
        // inside it.  The two must be derived together: the line index is an
        // index *into* this source.  When the parse is stale, rebuild both
        // from the cached buffer-line range so we see the typed characters
        // that haven't been re-parsed yet; otherwise use the shared
        // `raw_block_cursor`, which `cursor_rendered_line_idx` also calls so
        // the view and the cursor-row report can't disagree.
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
                    let raw = raw_block_cursor(editor, cursor_byte);
                    (raw.source, raw.raw_line, raw.col)
                }
            };

        // Split raw source into lines.
        let raw_lines: Vec<&str> = raw_source_lines(&raw_block_source);

        // Map the cursor's raw source line to a rendered line within the
        // block.  For tables the rendered layout is: top border, header
        // (one or more lines), thick separator (alignment row), then
        // (data row(s), thin separator)*, and finally the bottom border.
        // Cells may now wrap, so any single TableInfo row can
        // span multiple rendered sub-lines.  Use the box-drawing-glyph
        // classifier to find the FIRST sub-line of the target row — the
        // raw-text replacement always lands on that line.  We must
        // never replace a border or separator line with raw text.
        let is_table = table_edit::is_table_block(&raw_block_source);
        let is_setext = detect_setext(&raw_block_source).is_some();
        // In a fenced code block only the opening-fence line (raw line 0,
        // when a language tag is present) should de-render — that's the
        // language label the renderer turns into ` rust ` styling.  Body
        // lines render the same as their raw form (just with code-block
        // background) and the closing fence has no rendered row of its
        // own, so de-rendering inside the block is visual churn at best
        // and clobbers the last code line at worst.
        let cursor_block_ast = editor
            .parsed
            .real_ranges
            .iter()
            .position(|r| r.start <= cursor_byte && cursor_byte < r.end)
            .and_then(|i| editor.parsed.blocks.get(i));
        let is_code_block = matches!(
            cursor_block_ast,
            Some(crate::markdown::Block::CodeBlock { .. })
        );
        let is_fenced_code = matches!(
            cursor_block_ast,
            Some(crate::markdown::Block::CodeBlock { fenced: true, .. })
        );
        // Mermaid code blocks are post-processed into synthetic
        // `Block::ImageBlock`s.  When the cursor enters one, every
        // rendered row of the block (where the image placeholder
        // otherwise sits) is replaced with the corresponding raw-source
        // line so the user can see and edit the mermaid source — same
        // affordance as a fenced code block.
        let is_mermaid_block = editor.parsed.is_mermaid_block(cursor_block_idx);
        // Big-text H1: `Renderer::try_render_h1_big` emits 4 big-text rows
        // plus the `─` rule (5 own lines), versus the plain 2-line H1.
        // While the cursor is inside the block we collapse the big-text
        // region back to the raw `# Title` line and leave the bottom rule
        // line rendered so the user edits a stable single line — matches
        // the user's "collapse to raw while editing" preference.
        let is_big_h1_block = matches!(
            cursor_block_ast,
            Some(crate::markdown::Block::Heading {
                level: pulldown_cmark::HeadingLevel::H1,
                ..
            })
        ) && cursor_block_own > 2;
        // True when the cursor's current line is allowed to de-render: any
        // non-code-block line, the opening-fence line of a fenced code
        // block with a language tag, or the closing-fence line of any
        // fenced code block (the renderer reserves a trailing padded row
        // for it; revealing that row shows the ``` glyphs).
        let raw_line_count = raw_lines.len();
        let is_closing_fence_line =
            is_fenced_code && raw_line_count > 0 && cursor_raw_line == raw_line_count - 1;
        let code_block_allows_reveal =
            !is_code_block || (is_fenced_code && cursor_raw_line == 0) || is_closing_fence_line;
        // Which rendered sub-line of the block gets the raw-text
        // replacement.  Shared with `cursor_rendered_line_idx` (and, through
        // it, the mouse hit-test's revealed-line shortcut) so the three can
        // never disagree about which row is showing raw source.
        let cursor_in_block = crate::editor::state::cursor_sub_line_in_block(
            &editor.parsed,
            cursor_byte,
            cursor_block_idx,
            cursor_block_own,
            &raw_block_source,
            &raw_lines,
            cursor_raw_line,
        );
        // Wrapped-cell case: when the cursor sits in a data-row cell that
        // wraps onto multiple rendered sub-lines (or is in a row whose
        // *other* cells wrap), build a per-chunk overlay so each
        // rendered sub of the cell can be painted with its own raw
        // chunk.  Returns `None` for non-data rows and for single-sub
        // rows (existing `compute_cell_overlay` /
        // `compute_cell_chunk_overlay` paths handle those).  Raw text
        // wrapping to more chunks than the row's rendered height
        // scrolls vertically inside the row's sub-lines.
        let wrapped_cell = if is_table && cursor_raw_line >= 2 {
            compute_wrapped_cell_overlay(
                editor,
                cursor_block_lines.clone(),
                cursor_raw_line - 2,
                cursor_col,
                &raw_block_source,
            )
        } else {
            None
        };

        // When in a wrapped cell, the cursor's actual sub-line lives
        // at `row_first_line_idx + cursor_sub`.  For non-wrapped cells
        // it's the row's first sub.
        let cursor_rendered_line = match &wrapped_cell {
            Some(w) => w.row_first_line_idx + w.cursor_sub,
            None => cursor_block_lines.start + cursor_in_block,
        };

        // Cleared each frame; the indicator path below records the cursor's
        // screen cell so `EditorView` can re-stamp it over later overlays.
        view_state.cursor_screen = None;

        // Determine the scroll offset; sync from editor state.
        view_state.scroll = editor.scroll;
        let scroll = view_state.scroll;
        let (mut virtual_idx, mut first_sub_row) =
            editor.rendered_line_at_visual_row(scroll, area.width as usize);

        // Jitter suppression: if the cursor only recently moved to this line,
        // keep showing the block as rendered until the reveal delay has elapsed.
        let reveal_raw = editor.cursor_block_revealed();
        let cursor_visible = editor.cursor_visible();

        let cursor_indicator_style = self.cursor_style;

        let total_rendered = editor.parsed.lines.len();
        // Long-line wrapping is enabled in rendered-edit mode.
        let wrap = true;

        // Selection: compute the selected raw byte range once; per-line overlay
        // logic will intersect it with each line's byte range.
        let selection_bytes = editor.selection.map(|s| {
            let r = crate::editor::vim_ops::visual_span(&s, &editor.buffer, self.visual_kind);
            let rope = editor.buffer.rope();
            (rope.char_to_byte(r.start), rope.char_to_byte(r.end))
        });
        let block_range_for_cursor = editor
            .parsed
            .source_map
            .original_range_for_byte(cursor_byte);

        // Walk rendered lines from scroll offset. For each line, render it
        // normally EXCEPT cursor_rendered_line, which is shown as raw text.
        let mut vis_y: usize = 0;
        while vis_y < height {
            if virtual_idx >= total_rendered {
                break;
            }

            let skip_rows = first_sub_row;
            let rows_used;
            // Setext headings reveal all of their raw lines (the title and
            // the `===` / `---` underline) at once, on their corresponding
            // rendered positions — not just the single line the cursor is on.
            let in_cursor_block =
                virtual_idx >= cursor_block_lines.start && virtual_idx < cursor_block_lines.end;
            // Sub-line index within `wrapped_cell.subs` if `virtual_idx`
            // lands on one of the wrapped cell's chunks — multi-sub
            // overlay paints raw chunks across all those subs so the
            // cell's wrap is preserved when the cursor enters it.
            let wrapped_sub_idx_opt: Option<usize> = wrapped_cell.as_ref().and_then(|w| {
                let end = w.row_first_line_idx + w.subs.len();
                if virtual_idx >= w.row_first_line_idx && virtual_idx < end {
                    Some(virtual_idx - w.row_first_line_idx)
                } else {
                    None
                }
            });
            if reveal_raw && is_big_h1_block && in_cursor_block {
                // Big-H1 collapse: the cursor's raw line (`# Title`) goes on
                // the FIRST sub-line of the block; the remaining big-text
                // rows blank out so we don't see half of a glyph above /
                // below the editable text; the final sub-line keeps the
                // rendered rule (`─` × viewport) so the H1 separator stays
                // visible while editing.
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
                    let styled = make_raw_line_with_selection(raw_text, None, self.theme);
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
                // Setext headings reveal every rendered row of the block
                // to its matching raw-source line in one pass.
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
                // Mermaid blocks reveal as if they were a regular fenced
                // ```mermaid``` code block — the same way they'd appear
                // when the `diagrams` setting is disabled.  Each reserved
                // row gets:
                //
                //   * row 0 (opening fence) → ` mermaid ` language label,
                //     or the raw `` ```mermaid `` line with cursor when
                //     the cursor is on row 0.
                //   * body rows → raw source text painted on the code-
                //     block background; cursor / selection overlays
                //     applied per char.
                //   * the last raw row (closing `` ``` ``) → a padded
                //     placeholder row, or the raw fence with cursor when
                //     the cursor sits on the closing fence.
                //   * rows past the end of the source (reserved height
                //     exceeds the source line count) → padded code-block
                //     background so the entire reservation reads as one
                //     continuous code block.
                let sub = virtual_idx - cursor_block_lines.start;
                let raw_text = raw_lines.get(sub).copied().unwrap_or("");
                let cursor_on_this = cursor_raw_line == sub;
                let last_raw_idx = raw_lines.len().saturating_sub(1);
                let is_opening_fence_row = sub == 0 && raw_lines.len() >= 2;
                let is_closing_fence_row =
                    sub == last_raw_idx && raw_lines.len() >= 2 && raw_text.trim() == "```";
                let in_source = sub < raw_lines.len();
                let width = area.width as usize;

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

                let styled = if cursor_on_this && (is_opening_fence_row || is_closing_fence_row) {
                    // Fence row with cursor on it: reveal the raw fence
                    // text (matches a regular fenced code block's reveal
                    // behaviour for the opening / closing fence rows).
                    make_raw_line_with_selection(raw_text, sel_cols, self.theme)
                } else if is_opening_fence_row {
                    // No cursor: paint the language label as a code block
                    // would emit it.  Falls back to raw source for any
                    // language that isn't `mermaid` so the row still
                    // shows something sensible.
                    let lang = raw_text.trim_start_matches(['`', '~']);
                    let lang = if lang.is_empty() { "mermaid" } else { lang };
                    ratatui::text::Line::styled(format!(" {} ", lang), self.theme.code_block_lang)
                } else if is_closing_fence_row {
                    // No cursor: padded closing-fence row (NBSP-filled,
                    // code-block background) — same as the renderer's
                    // reserved closing row for a fenced code block.
                    ratatui::text::Line::styled(
                        "\u{00A0}".repeat(width.max(1)),
                        self.theme.code_block_text,
                    )
                } else if in_source {
                    // Body row.  Apply the code-block background so the
                    // block reads as code; selection overlaid per char.
                    make_code_styled_body_line(raw_text, sel_cols, self.theme)
                } else {
                    // Past the end of the source: pad with code-block
                    // background so the reservation looks continuous.
                    ratatui::text::Line::styled(
                        "\u{00A0}".repeat(width.max(1)),
                        self.theme.code_block_text,
                    )
                };

                // The cursor (bar/block) is painted onto the resolved cell —
                // on the fence-with-cursor and body rows alike — keeping the
                // wrapped layout computed from the bare source text.
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
                // Multi-sub wrapped-cell overlay: paint the rendered row
                // first (so neighbouring cells and borders stay), then
                // overlay the appropriate raw wrap chunk into the
                // active cell's column range.  Each sub of the cell
                // gets its own chunk so the cell's natural wrap is
                // preserved while the cursor edits inside it.
                let w = wrapped_cell
                    .as_ref()
                    .expect("wrapped_sub_idx implies wrapped_cell");
                let overlay = &w.subs[sub_idx];
                if let Some(line) = editor.parsed.lines.get(virtual_idx) {
                    rows_used =
                        render_line_from_visual(line, area, buf, vis_y as u16, wrap, skip_rows)
                            as usize;
                    let sel_in_cell = selection_bytes.and_then(|(sa, sb)| {
                        let block_start = block_range_for_cursor.as_ref()?.start;
                        // Every chunk of the wrapped cell is a slice of
                        // a single raw row (`cursor_raw_line`), so the
                        // raw-row start byte is the same for every sub.
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
            } else if reveal_raw && virtual_idx == cursor_rendered_line && code_block_allows_reveal
            {
                let raw_text = raw_lines.get(cursor_raw_line).copied().unwrap_or("");
                // Prefer cell-scoped reveal for table rows — replace only the
                // active cell's content area with raw text, keeping the box-
                // drawing borders and neighbouring cells rendered.  Two cell
                // overlays are tried in order: `compute_cell_overlay` for
                // cells whose raw text fits in the rendered cell width, and
                // `compute_cell_chunk_overlay` for wider raw cells (e.g.
                // `**_word_**` in a 5-cell column) — the latter horizontally
                // scrolls the cell, showing the chunk that contains the
                // cursor.
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
                    // Non-table block (or pipe-mismatched table line — e.g.
                    // mid-edit alignment row): full-line raw reveal.
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
                    let styled = make_raw_line_with_selection(raw_text, sel_cols, self.theme);
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
            } else if virtual_idx == cursor_rendered_line
                && (!reveal_raw || !code_block_allows_reveal)
            {
                // Show the rendered version with a cursor indicator at
                // the cursor's column.  Two cases land here:
                //   1. The jitter-delay window before `reveal_raw` flips
                //      to true — drawing the indicator now avoids a
                //      visible column-jump when the reveal fires.
                //   2. A code-block body / closing-fence line where we
                //      intentionally suppress de-render — the cursor
                //      still needs to be visible on top of the rendered
                //      code.
                if let Some(line) = editor.parsed.lines.get(virtual_idx) {
                    let raw_text = raw_lines.get(cursor_raw_line).copied().unwrap_or("");
                    // Paragraph lines with inline links / code spans shift the
                    // cursor's rendered column relative to its raw column.  The
                    // inverse of the click handler's map keeps the indicator
                    // where the click landed, avoiding a visible jump when the
                    // raw reveal fires.  `None` when the line isn't a plain
                    // paragraph (heading/list/blockquote) — caller falls back.
                    let inline_col = block_range_for_cursor.as_ref().and_then(|br| {
                        let actual_rendered: usize =
                            line.spans.iter().map(|s| s.content.chars().count()).sum();
                        let buffer_line_idx = editor
                            .buffer
                            .block_line_to_buffer_line(br.start, cursor_raw_line);
                        editor
                            .inline_map_for(buffer_line_idx, raw_text)
                            .raw_to_rendered_checked(cursor_col, actual_rendered)
                    });
                    let visual_col = if let Some(w) = &wrapped_cell {
                        // Wrapped-cell mapping resolves cursor offset →
                        // (sub-line, col-in-sub) using `wrap_cell_with_indices`,
                        // then converts col-in-sub to a rendered x by adding
                        // the cell's leading-pipe column.
                        w.visual_col
                    } else if is_table {
                        // Raw col → rendered col isn't 1:1 for table rows:
                        // padded cells shift the cursor column.  Walk pipe
                        // positions so the jitter-delay indicator lands at
                        // the same visual col the cell overlay will use on
                        // reveal — avoids a cursor jump at the delay edge.
                        table_raw_col_to_rendered_col(raw_text, line, cursor_col)
                            .unwrap_or(cursor_col)
                    } else if let Some(col) =
                        list_raw_col_to_rendered_col(raw_text, line, cursor_col)
                    {
                        col
                    } else if let Some(col) = inline_col {
                        col
                    } else {
                        cursor_col
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
                    // Record where the indicator landed so `EditorView` can
                    // re-stamp it after the search / selection overlays.
                    view_state.cursor_screen = cursor_cell;
                } else {
                    rows_used = 1;
                }
            } else {
                // Normal rendered line.
                if let Some(line) = editor.parsed.lines.get(virtual_idx) {
                    rows_used =
                        render_line_from_visual(line, area, buf, vis_y as u16, wrap, skip_rows)
                            as usize;
                } else {
                    break;
                }
            }

            // Paint the selection overlay across the line's visual rows if
            // the line's block is part of the active selection and this is
            // NOT a line that already painted its own selection (cursor's
            // raw line, setext-revealed lines, or any sub of the active
            // wrapped cell — the cell-overlay paths above handle their own
            // selection highlighting per chunk).
            if let Some((sa, sb)) = selection_bytes {
                let setext_revealed = reveal_raw && is_setext && in_cursor_block;
                let mermaid_revealed = reveal_raw && is_mermaid_block && in_cursor_block;
                let wrapped_revealed = reveal_raw && wrapped_sub_idx_opt.is_some();
                // Reads as three separate suppression cases; clippy's
                // collapse hides which condition gates which.
                #[allow(clippy::nonminimal_bool)]
                if !(reveal_raw && virtual_idx == cursor_rendered_line && code_block_allows_reveal)
                    && !setext_revealed
                    && !mermaid_revealed
                    && !wrapped_revealed
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

        // Build per-frame snapshots of every visible table, then
        // paint the row/column-button glyphs over the rendered content.
        // The snapshots are retained on `RenderedViewState` so the next
        // mouse event can hit-test against them.  The cached variant
        // skips the visible-line walk when scroll, area, parsed-doc
        // version, AND
        // the show-handles flag all match the previous frame.
        //
        // Handles paint only on the table the cursor is
        // currently inside — keeps the affordance visible during the
        // table-edit interaction without competing with surrounding
        // content during ordinary navigation.  Snapshots are still
        // captured for every visible table so mouse hit-testing on
        // adjacent tables continues to work even though no glyph is
        // painted on them.
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

        // Build per-frame snapshots of every visible image block.
        // Image painting itself happens in `EditorView::render` (after this
        // widget returns) because it needs mutable access to the cache.
        // The `_cached` variant skips the O(lines × images) scan when
        // scroll, area, and parsed-doc version all match the previous
        // frame's.
        image_view::build_snapshots_cached(
            self.state,
            area,
            self.state.scroll,
            &mut view_state.image_snapshots,
            &mut view_state.image_snapshots_key,
        );

        // Build link snapshots for mouse hit-testing.  Cached
        // by `(scroll, area, parsed_version)` — rebuilt only when
        // something that affects link layout actually changed.  The
        // uncached walk calls `visual_rows_for_line` for every visible
        // line, which is O(chars) per line and dominated idle CPU on
        // large documents.
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

/// Byte offset of the table block the cursor is currently inside, or
/// `None` when the cursor isn't in a table.  Used to gate the drag-
/// handle painter so handles only show on the active table.
///
/// Walks the snapshot list rather than reparsing — the snapshots
/// already carry every visible table's `table_byte_start`, and we can
/// match by checking whether the cursor's byte falls in `[start, end)`.
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
