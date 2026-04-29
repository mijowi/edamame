use ratatui::{
    buffer::Buffer as TuiBuf,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::StatefulWidget,
};

use crate::config::Theme;
use crate::document::detect_setext;
use crate::editor::mouse_ops;
use crate::editor::table_edit;
use crate::editor::EditorState;
use crate::markdown::table_layout::{
    compute_cell_overlay, raw_pipe_positions, rendered_pipe_positions,
    table_raw_col_to_rendered_col, wrap_cell_with_indices, CellOverlay,
};

use super::image_view::{self, ImageLayoutSnapshot};
use super::line_render::{render_line, render_line_with_cursor};
use super::link_view::{self, LinkLayoutSnapshot};
use super::table_view::{self, TableLayoutSnapshot};

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
    /// of the last render.  Phase 7 uses them as the hit-test surface
    /// for images (click detection); future phases may add expand /
    /// open UX.
    pub image_snapshots: Vec<ImageLayoutSnapshot>,
    /// Cache key for `image_snapshots`: `(scroll, area, parsed_version)`.
    /// When the tuple matches on the next frame, the snapshot vector is
    /// reused instead of recomputed — avoids the O(lines × images)
    /// geometry scan when nothing that affects image layout has changed.
    pub image_snapshots_key: Option<(usize, Rect, u64)>,
    /// Snapshots of every visible Markdown link, captured at the end of
    /// the last render.  Used by Phase 8's mouse dispatch to hit-test
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
    /// Phase 13: buttons only paint on the table that contains the
    /// cursor — moving the cursor out of the table hides them so they
    /// never compete with the rendered content during navigation.  The
    /// gating is enforced by `paint_handles_for_cursor_table` in
    /// `table_view`.
    pub show_table_buttons: bool,
    /// Phase 13 — when `Some`, an in-progress table drag is highlighted
    /// after the handles are painted.  `None` when no relevant drag is
    /// active.
    pub drop_indicator: Option<crate::ui::table_view::DropIndicator>,
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

        // Raw source text for the cursor's block.  When the parse is
        // stale, extract it via the cached buffer-line range so we see
        // the typed characters that haven't been re-parsed yet.
        let raw_block_source: String = if use_cache {
            match editor.cursor_block_line_range.clone() {
                Some(range) => {
                    let mut out = String::new();
                    for line in range {
                        if let Some(text) = editor.buffer.line(line) {
                            out.push_str(&text);
                        }
                    }
                    while out.ends_with('\n') {
                        out.pop();
                    }
                    out
                }
                None => String::new(),
            }
        } else {
            editor
                .parsed
                .source_map
                .original_range_for_byte(cursor_byte)
                .map(|r| {
                    let source = editor.buffer.contents();
                    let end = r.end.min(source.len());
                    source[r.start..end].to_owned()
                })
                .unwrap_or_default()
        };

        // Split raw source into lines.
        let raw_lines: Vec<&str> = raw_source_lines(&raw_block_source);

        // Find where the cursor is within the raw block.
        let (cursor_raw_line, cursor_col) =
            match (use_cache, editor.cursor_block_line_range.as_ref()) {
                (true, Some(range)) => {
                    let (buffer_line, col) = editor.cursor.line_col(&editor.buffer);
                    let raw_line = buffer_line.saturating_sub(range.start);
                    (raw_line, col)
                }
                _ => cursor_position_in_block(editor, cursor_byte, &raw_block_source),
            };

        // Map the cursor's raw source line to a rendered line within the
        // block.  For tables the rendered layout is: top border, header
        // (one or more lines), thick separator (alignment row), then
        // (data row(s), thin separator)*, and finally the bottom border.
        // Phase 13: cells may now wrap, so any single TableInfo row can
        // span multiple rendered sub-lines.  Use the box-drawing-glyph
        // classifier to find the FIRST sub-line of the target row — the
        // raw-text replacement always lands on that line.  We must
        // never replace a border or separator line with raw text.
        let is_table = table_edit::is_table_block(&raw_block_source);
        let is_setext = detect_setext(&raw_block_source).is_some();
        let cursor_in_block = if is_table && cursor_block_own >= 3 {
            let last_replaceable = cursor_block_own.saturating_sub(2);
            let block_lines = editor
                .parsed
                .lines
                .get(cursor_block_lines.clone())
                .unwrap_or(&[]);
            let kinds = crate::ui::table_view::classify_table_sub_lines(block_lines);
            let sub = match cursor_raw_line {
                0 => kinds
                    .iter()
                    .position(|k| {
                        matches!(
                            k,
                            crate::ui::table_view::TableSubLineKind::Header { sub: 0 }
                        )
                    })
                    .unwrap_or(1),
                1 => kinds
                    .iter()
                    .position(|k| {
                        matches!(k, crate::ui::table_view::TableSubLineKind::ThickSeparator)
                    })
                    .unwrap_or(2),
                r => {
                    let target = r - 2;
                    kinds
                        .iter()
                        .position(|k| {
                            matches!(
                                k,
                                crate::ui::table_view::TableSubLineKind::DataRow { row, sub: 0 }
                                    if *row == target
                            )
                        })
                        .unwrap_or(2 * r - 1)
                }
            };
            sub.min(last_replaceable)
        } else {
            // Raw-to-rendered line mapping is 1:1 for simple blocks, but a
            // list that contains a blank-line separator (e.g. the form
            // required for an empty nested item to parse correctly) has
            // fewer rendered lines than raw lines.  Compress by counting
            // non-blank raw lines preceding the cursor's raw line so the
            // replaced rendered row corresponds to the actual item the
            // cursor sits on.
            let preceding_non_blank = raw_lines
                .iter()
                .take(cursor_raw_line)
                .filter(|l| !l.trim().is_empty())
                .count();
            preceding_non_blank.min(cursor_block_own.saturating_sub(1))
        };
        // Wrapped-cell case: when the cursor sits in a data-row cell that
        // wraps onto multiple rendered sub-lines (or is in a row whose
        // *other* cells wrap), build a per-chunk overlay so each
        // rendered sub of the cell can be painted with its own raw
        // chunk.  Returns `None` for non-data rows, for cells that fit
        // in a single sub of a single-sub row (existing
        // `compute_cell_overlay` path), and for cells whose raw text
        // wraps wider than the row's rendered height (existing
        // `compute_cell_chunk_overlay` path keeps the cursor's chunk
        // visible via horizontal scroll).
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

        // Determine the scroll offset; sync from editor state.
        view_state.scroll = editor.scroll;
        let scroll = view_state.scroll;

        // Jitter suppression: if the cursor only recently moved to this line,
        // keep showing the block as rendered until the reveal delay has elapsed.
        let reveal_raw = editor.cursor_block_revealed();

        let cursor_indicator_style = self.theme.cursor;

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
            } else if reveal_raw && wrapped_sub_idx_opt.is_some() {
                // Multi-sub wrapped-cell overlay: paint the rendered row
                // first (so neighbouring cells and borders stay), then
                // overlay the appropriate raw wrap chunk into the
                // active cell's column range.  Each sub of the cell
                // gets its own chunk so the cell's natural wrap is
                // preserved while the cursor edits inside it.
                let w = wrapped_cell
                    .as_ref()
                    .expect("wrapped_sub_idx implies wrapped_cell");
                let sub_idx = wrapped_sub_idx_opt.unwrap();
                let overlay = &w.subs[sub_idx];
                if let Some(line) = editor.parsed.lines.get(virtual_idx) {
                    rows_used = render_line(line, area, buf, vis_y as u16, wrap) as usize;
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
                    overlay_raw_cell(buf, area, vis_y as u16, overlay, sel_in_cell, self.theme);
                } else {
                    rows_used = 1;
                }
            } else if reveal_raw && virtual_idx == cursor_rendered_line {
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
                    let styled = make_raw_line_with_selection(
                        raw_text,
                        Some(cursor_col),
                        sel_cols,
                        self.theme,
                    );
                    rows_used = render_line(&styled, area, buf, vis_y as u16, wrap) as usize;
                }
            } else if !reveal_raw && virtual_idx == cursor_rendered_line {
                // Still in jitter delay: show the rendered version with
                // a cursor indicator at the cursor's column so there's
                // no visible column-jump when the reveal fires.
                if let Some(line) = editor.parsed.lines.get(virtual_idx) {
                    let raw_text = raw_lines.get(cursor_raw_line).copied().unwrap_or("");
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
                    } else if let Some(col) = mouse_ops::paragraph_raw_col_to_rendered_col(
                        raw_text, line, cursor_col,
                    ) {
                        // Paragraph lines with inline links / code spans
                        // shift the cursor's rendered column relative to its
                        // raw column.  Use the inverse of the click handler's
                        // map so the indicator sits where the click landed,
                        // avoiding a visible jump when the raw reveal fires.
                        col
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
            // NOT a line that already painted its own selection (cursor's
            // raw line, setext-revealed lines, or any sub of the active
            // wrapped cell — the cell-overlay paths above handle their own
            // selection highlighting per chunk).
            if let Some((sa, sb)) = selection_bytes {
                let setext_revealed = reveal_raw && is_setext && in_cursor_block;
                let wrapped_revealed = reveal_raw && wrapped_sub_idx_opt.is_some();
                if !(reveal_raw && virtual_idx == cursor_rendered_line)
                    && !setext_revealed
                    && !wrapped_revealed
                {
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

        // Phase 6: build per-frame snapshots of every visible table, then
        // paint the row/column-button glyphs over the rendered content.
        // The snapshots are retained on `RenderedViewState` so the next
        // mouse event can hit-test against them.  The cached variant
        // skips the visible-line walk when scroll, area, parsed-doc
        // version, AND
        // the show-handles flag all match the previous frame.
        //
        // Phase 13: handles paint only on the table the cursor is
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

        // Phase 7: build per-frame snapshots of every visible image block.
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

        // Phase 8: build link snapshots for mouse hit-testing.  Cached
        // by `(scroll, area, parsed_version)` — rebuilt only when
        // something that affects link layout actually changed.  The
        // uncached walk calls `visual_rows_for_line` for every visible
        // line, which is O(chars) per line and dominated idle CPU on
        // large documents prior to Phase 15.
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

/// Cell-scoped overlay for cells whose raw markdown is too wide to fit
/// in the rendered cell.  Wraps the cell's source bytes onto chunks of
/// the cell's full width, picks the chunk the cursor is on, and
/// returns it as a normal `CellOverlay` so the existing painter can
/// stamp it directly onto the rendered table row.
///
/// Effect: while the cursor is in the cell, the cell horizontally
/// scrolls (per character typed) — the chunk the cursor is on stays
/// visible, with the rest of the source paged off-screen.  Switching
/// to Raw mode is the canonical way to see the entire raw cell at
/// once.
///
/// Hard-wrap (one-char-per-step) rather than word-aware wrap because
/// (a) cursor → chunk mapping is then trivial (`offset / cell_width`),
/// (b) the chunks are predictable as the user types, and
/// (c) word-boundary breaks would force the cursor to jump to a new
/// chunk mid-word, which is jarring during line editing.
fn compute_cell_chunk_overlay(
    raw_row: &str,
    rendered_line: &Line<'_>,
    cursor_col_raw: usize,
) -> Option<CellOverlay> {
    let raw_pipes = raw_pipe_positions(raw_row);
    let rendered_pipes = rendered_pipe_positions(rendered_line);
    if raw_pipes.len() < 2 || rendered_pipes.len() != raw_pipes.len() {
        return None;
    }

    let col_count = raw_pipes.len() - 1;
    let preceding = raw_pipes
        .iter()
        .take_while(|&&p| p < cursor_col_raw)
        .count();
    let cell_idx = preceding.saturating_sub(1).min(col_count - 1);

    let raw_cell_start = raw_pipes[cell_idx] + 1;
    let raw_cell_end = raw_pipes[cell_idx + 1];
    let raw_cell_text: String = raw_row
        .chars()
        .skip(raw_cell_start)
        .take(raw_cell_end - raw_cell_start)
        .collect();
    let rendered_start = rendered_pipes[cell_idx] + 1;
    let rendered_end = rendered_pipes[cell_idx + 1];
    let cell_width = rendered_end.saturating_sub(rendered_start);
    if cell_width == 0 {
        return None;
    }

    let raw_chars: Vec<char> = raw_cell_text.chars().collect();
    if raw_chars.len() <= cell_width {
        // Cell content fits — `compute_cell_overlay` should have been
        // chosen instead.  Return None so the caller falls through.
        return None;
    }

    // Hard-wrap by `cell_width`.  Cursor's chunk + col-in-chunk are
    // straight integer division / modulo of the cursor's offset
    // within the cell.
    let cursor_in_cell = cursor_col_raw.saturating_sub(raw_cell_start);
    let total_chunks = raw_chars.len().div_ceil(cell_width);
    let max_chunk_idx = total_chunks.saturating_sub(1);
    let chunk_idx = (cursor_in_cell / cell_width).min(max_chunk_idx);
    let col_in_chunk = (cursor_in_cell - chunk_idx * cell_width).min(cell_width.saturating_sub(1));

    let chunk_start_chars = chunk_idx * cell_width;
    let chunk_end_chars = (chunk_start_chars + cell_width).min(raw_chars.len());
    let chunk: String = raw_chars[chunk_start_chars..chunk_end_chars]
        .iter()
        .collect();

    // Selection mapping: byte offset of the chunk's first char inside
    // `raw_row`.  Selection bytes are then intersected with
    // [chunk_byte_start, chunk_byte_start + chunk.len()) and mapped to
    // chars within the chunk.
    let chunk_byte_start = raw_row
        .char_indices()
        .nth(raw_cell_start + chunk_start_chars)
        .map(|(b, _)| b)
        .unwrap_or(raw_row.len());

    Some(CellOverlay {
        rendered_start,
        rendered_end,
        raw_text: chunk,
        cursor_in_cell: Some(col_in_chunk),
        raw_cell_byte_start: chunk_byte_start,
    })
}

/// Information about the cursor's position inside a *wrapped* table
/// cell — i.e. one whose content broke onto multiple rendered
/// sub-lines because it overflowed the column's allocated width.
///
/// Used by `RenderedView::render` to:
/// 1. Push `cursor_rendered_line` from the row's first sub onto the
///    sub the cursor actually occupies (`sub_offset`).
/// 2. Place the cursor indicator at the right rendered column
///    (`visual_col`) on that sub.
struct WrappedCellOverlay {
    /// Sub-line index in `editor.parsed.lines` of the cell's row's
    /// first rendered sub.
    row_first_line_idx: usize,
    /// Per-chunk overlay info — one entry per wrap chunk that fits
    /// within the row's rendered height.  Index `i` is painted on
    /// `editor.parsed.lines[row_first_line_idx + i]`.  Each entry is
    /// already shaped for `overlay_raw_cell` (rendered_start shifted
    /// for continuation chunks, cursor_in_cell only on the cursor's
    /// chunk).
    subs: Vec<CellOverlay>,
    /// Index within `subs` that contains the cursor.
    cursor_sub: usize,
    /// Document-area-relative rendered column for the cursor.  Used by
    /// the jitter-delay branch to draw the cursor indicator at the
    /// same column the reveal-time overlay will use, so there's no
    /// jump when the reveal fires.
    visual_col: usize,
}

/// Resolve the cursor's wrapped-cell layout — one `CellOverlay` per
/// rendered sub-line of the row, mapping the wrap chunks of the raw
/// cell text onto the rendered sub-lines.  Returns `None` for single-
/// sub-line cells in single-sub-line rows (existing single-sub
/// `compute_cell_overlay` / `compute_cell_chunk_overlay` paths handle
/// those), and also when the raw cell wraps to *more* chunks than the
/// rendered row has sub-lines (existing `compute_cell_chunk_overlay`
/// keeps the cursor's chunk visible by horizontally scrolling — more
/// useful than truncating raw chunks).
fn compute_wrapped_cell_overlay(
    editor: &EditorState,
    block_lines_range: std::ops::Range<usize>,
    data_row_idx: usize,
    cursor_col_raw: usize,
    raw_block_source: &str,
) -> Option<WrappedCellOverlay> {
    use crate::ui::table_view::{classify_table_sub_lines, TableSubLineKind};

    let block_lines = editor.parsed.lines.get(block_lines_range.clone())?;
    let kinds = classify_table_sub_lines(block_lines);

    // Find the row's first sub and how many sub-lines it spans.
    let row_start_local = kinds.iter().position(|k| {
        matches!(
            k,
            TableSubLineKind::DataRow { row, sub: 0 } if *row == data_row_idx
        )
    })?;
    let row_height = kinds[row_start_local..]
        .iter()
        .take_while(|k| matches!(k, TableSubLineKind::DataRow { row, .. } if *row == data_row_idx))
        .count();

    // Pipe geometry: the row's first sub-line carries the column ranges
    // (every wrap sub-line of the same row has identical pipe positions
    // by construction in `render_table_row`).
    let first_line = block_lines.get(row_start_local)?;
    let rendered_pipes = rendered_pipe_positions(first_line);
    let raw_row = raw_block_source.split('\n').nth(data_row_idx + 2)?;
    let raw_pipes = raw_pipe_positions(raw_row);
    if raw_pipes.len() < 2 || rendered_pipes.len() != raw_pipes.len() {
        return None;
    }

    let col_count = raw_pipes.len() - 1;
    let preceding = raw_pipes
        .iter()
        .take_while(|&&p| p < cursor_col_raw)
        .count();
    let cell_idx = preceding.saturating_sub(1).min(col_count - 1);

    // Cell's raw + rendered ranges.
    let raw_cell_start_char = raw_pipes[cell_idx] + 1;
    let raw_cell_end_char = raw_pipes[cell_idx + 1];
    let raw_cell_text: String = raw_row
        .chars()
        .skip(raw_cell_start_char)
        .take(raw_cell_end_char - raw_cell_start_char)
        .collect();
    let cell_rendered_start = rendered_pipes[cell_idx] + 1;
    let cell_rendered_end = rendered_pipes[cell_idx + 1];
    // Effective content width = rendered cell width − 2 leading/trailing
    // padding spaces the renderer always emits around cell content.
    let content_width = cell_rendered_end
        .saturating_sub(cell_rendered_start)
        .saturating_sub(2);
    if content_width == 0 {
        return None;
    }

    // Re-run the renderer's word-wrap so we know which sub-line + col
    // the cursor's char index lands on.  Word-wrap drops whitespace at
    // break points, so a cursor on dropped whitespace maps to the start
    // of the next visible row.
    let wrapped = wrap_cell_with_indices(&raw_cell_text, content_width);
    if wrapped.is_empty() {
        return None;
    }

    // Single-sub cell in a single-sub row: leave it to the existing
    // `compute_cell_overlay` path.  Multi-sub raw beyond the row's
    // rendered height: leave it to `compute_cell_chunk_overlay` so the
    // cursor's chunk stays visible via horizontal scroll.
    if wrapped.len() <= 1 && row_height <= 1 {
        return None;
    }
    if wrapped.len() > row_height {
        return None;
    }

    // Locate cursor: which chunk + col within that chunk.
    let cursor_in_cell = cursor_col_raw.saturating_sub(raw_cell_start_char);
    let last_idx = wrapped.len() - 1;
    let mut cursor_sub = last_idx;
    let mut cursor_col_in_chunk = wrapped[last_idx].1.chars().count();
    for (i, (start_idx, row_text)) in wrapped.iter().enumerate() {
        let next_start = wrapped.get(i + 1).map(|(s, _)| *s).unwrap_or(usize::MAX);
        if cursor_in_cell < next_start {
            cursor_sub = i;
            let row_chars = row_text.chars().count();
            let pos_in_row = cursor_in_cell.saturating_sub(*start_idx);
            cursor_col_in_chunk = pos_in_row.min(row_chars);
            break;
        }
    }

    // raw_row char index → byte offset.  +1 sentinel so we can index
    // past the last char without panicking.
    let raw_row_byte_at: Vec<usize> = raw_row
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(raw_row.len()))
        .collect();

    let mut subs: Vec<CellOverlay> = Vec::with_capacity(wrapped.len());
    for (i, (start_in_cell, chunk_text)) in wrapped.iter().enumerate() {
        // First chunk inherits the cell's leading-pad space directly
        // from the raw cell text — paint it from `cell_rendered_start`
        // so the pad lines up.  Continuation chunks have the leading
        // pad dropped at the wrap point, so paint them one column to
        // the right and let the rendered ' ' that the renderer
        // already drew in the leading-pad column show through.
        let has_leading_pad = chunk_text.starts_with(' ');
        let painted_start = if has_leading_pad {
            cell_rendered_start
        } else {
            cell_rendered_start + 1
        };
        let chunk_first_char_in_row = raw_cell_start_char + start_in_cell;
        let raw_cell_byte_start = raw_row_byte_at
            .get(chunk_first_char_in_row)
            .copied()
            .unwrap_or(raw_row.len());
        let cursor_in_cell = if i == cursor_sub {
            Some(cursor_col_in_chunk.min(chunk_text.chars().count()))
        } else {
            None
        };
        subs.push(CellOverlay {
            rendered_start: painted_start,
            rendered_end: cell_rendered_end,
            raw_text: chunk_text.clone(),
            cursor_in_cell,
            raw_cell_byte_start,
        });
    }

    let visual_col = subs[cursor_sub].rendered_start + cursor_col_in_chunk;

    Some(WrappedCellOverlay {
        row_first_line_idx: block_lines_range.start + row_start_local,
        subs,
        cursor_sub,
        visual_col,
    })
}

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
    let cursor_style = theme.cursor;
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
    let indent = super::line_render::compute_hanging_indent(line);
    let rows = super::line_render::visual_rows_of_chars(&chars, width, indent);
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
    let cursor_style = theme.cursor;
    // `theme.normal` carries `bg(Color::Reset)` to anchor unstyled text against
    // the terminal default; if we let that bg through here it would clobber the
    // table-row stripe painted under the cell.  Strip the bg so the underlying
    // cell's bg is preserved — selection/cursor styles bring their own bg back
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
        if overlay.cursor_in_cell == Some(i) {
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

/// Map a raw-column on a list-item line to its rendered column.  Returns
/// `None` when `raw_text` isn't recognized as a list-item line — callers
/// fall back to treating raw-col as visual-col.
///
/// Needed because the rendered marker width can differ from the raw
/// marker width:
///
///   - task items: raw `- [ ] foo` → rendered `[ ] foo` (the `- ` prefix
///     is dropped; the checkbox is the visual anchor instead).
///   - ordered items with 10+ items: raw `1. foo` → rendered ` 1. foo`
///     (numbers are right-aligned in a max-digit-wide slot).
///
/// Both cases shift the content column, so the jitter-delay cursor
/// indicator in Rendered mode must be drawn at the correct rendered
/// column — not the raw column.
fn list_raw_col_to_rendered_col(
    raw_text: &str,
    line: &ratatui::text::Line<'_>,
    raw_col: usize,
) -> Option<usize> {
    let raw_total = raw_list_marker_char_width(raw_text)?;
    let rendered_total = rendered_list_marker_char_width(line)?;
    if raw_col <= raw_total {
        Some(rendered_total)
    } else {
        Some(raw_col - raw_total + rendered_total)
    }
}

/// Width (in chars) of the raw list-item prefix — leading whitespace +
/// marker (`- ` / `N. ` / `N) `) + optional task-prefix (`[ ] ` etc.).
/// Returns `None` when `raw_text` doesn't start with a list marker.
fn raw_list_marker_char_width(raw_text: &str) -> Option<usize> {
    let indent_chars = raw_text
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    let after_indent: String = raw_text.chars().skip(indent_chars).collect();
    let rb = after_indent.as_bytes();
    let marker_len = match rb.first() {
        Some(b'-') | Some(b'*') | Some(b'+') if rb.get(1) == Some(&b' ') => 2,
        _ => {
            let digits = rb.iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 0
                && matches!(rb.get(digits), Some(b'.') | Some(b')'))
                && rb.get(digits + 1) == Some(&b' ')
            {
                digits + 2
            } else {
                return None;
            }
        }
    };
    let after_marker = &after_indent[marker_len..];
    let task_len = if after_marker.starts_with("[ ] ")
        || after_marker.starts_with("[x] ")
        || after_marker.starts_with("[X] ")
    {
        4
    } else {
        0
    };
    Some(indent_chars + marker_len + task_len)
}

/// Width (in chars) of the rendered list-item marker — leading whitespace
/// + `• ` / padded digits + `. ` plus an optional trailing `[ ] ` task
/// prefix.  Returns `None` when the rendered line doesn't start with a
/// recognizable list marker.
fn rendered_list_marker_char_width(line: &ratatui::text::Line<'_>) -> Option<usize> {
    let text: String = line.spans.iter().flat_map(|s| s.content.chars()).collect();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() && chars[i] == ' ' {
        i += 1;
    }
    let after_bullet = if chars.get(i) == Some(&'•') && chars.get(i + 1) == Some(&' ') {
        Some(i + 2)
    } else {
        let digits = chars[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0
            && matches!(chars.get(i + digits), Some('.') | Some(')'))
            && chars.get(i + digits + 1) == Some(&' ')
        {
            Some(i + digits + 2)
        } else {
            None
        }
    }?;
    // Tasks are decorated bullets — `• ` (or the ordered marker) is followed
    // by a `[ ] ` / `[x] ` checkbox.  Include those four cells in the marker
    // width so cursor / selection mapping covers the whole forbidden zone.
    if chars.get(after_bullet) == Some(&'[')
        && matches!(chars.get(after_bullet + 1), Some(' ') | Some('x') | Some('X'))
        && chars.get(after_bullet + 2) == Some(&']')
        && chars.get(after_bullet + 3) == Some(&' ')
    {
        Some(after_bullet + 4)
    } else {
        Some(after_bullet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::style::Style;
    use ratatui::text::{Line, Span};

    #[test]
    fn raw_list_marker_width_bullet() {
        assert_eq!(raw_list_marker_char_width("- foo"), Some(2));
        assert_eq!(raw_list_marker_char_width("  - foo"), Some(4));
    }

    #[test]
    fn raw_list_marker_width_ordered() {
        assert_eq!(raw_list_marker_char_width("1. foo"), Some(3));
        assert_eq!(raw_list_marker_char_width("10. foo"), Some(4));
    }

    #[test]
    fn raw_list_marker_width_task() {
        assert_eq!(raw_list_marker_char_width("- [ ] foo"), Some(6));
        assert_eq!(raw_list_marker_char_width("- [x] foo"), Some(6));
    }

    #[test]
    fn rendered_marker_width_bullet() {
        let line = Line::from(vec![Span::styled("• ", Style::default()), Span::raw("foo")]);
        assert_eq!(rendered_list_marker_char_width(&line), Some(2));
    }

    #[test]
    fn rendered_marker_width_ordered_padded() {
        let line = Line::from(vec![
            Span::styled(" 1. ", Style::default()),
            Span::raw("foo"),
        ]);
        assert_eq!(rendered_list_marker_char_width(&line), Some(4));
    }

    #[test]
    fn rendered_marker_width_task() {
        // Tasks now render with the bullet kept — `• [ ] foo` — so the
        // full marker width is 6 (bullet + space + checkbox + space).
        let line = Line::from(vec![
            Span::styled("• ", Style::default()),
            Span::styled("[ ] ", Style::default()),
            Span::raw("foo"),
        ]);
        assert_eq!(rendered_list_marker_char_width(&line), Some(6));
    }

    #[test]
    fn list_col_map_bullet_unchanged() {
        // Raw `- foo`, rendered `• foo`.  Both have 2-char markers, so
        // raw col 2 (start of 'foo') maps to rendered col 2.
        let line = Line::from(vec![Span::styled("• ", Style::default()), Span::raw("foo")]);
        assert_eq!(list_raw_col_to_rendered_col("- foo", &line, 2), Some(2));
        assert_eq!(list_raw_col_to_rendered_col("- foo", &line, 4), Some(4));
    }

    #[test]
    fn list_col_map_task_aligns_one_to_one() {
        // Raw `- [ ] foo` (6-char marker), rendered `• [ ] foo` (also 6).
        // Cursor at raw col 6 ('f') stays at rendered col 6.
        let line = Line::from(vec![
            Span::styled("• ", Style::default()),
            Span::styled("[ ] ", Style::default()),
            Span::raw("foo"),
        ]);
        assert_eq!(list_raw_col_to_rendered_col("- [ ] foo", &line, 6), Some(6));
        assert_eq!(list_raw_col_to_rendered_col("- [ ] foo", &line, 7), Some(7));
    }

    #[test]
    fn list_col_map_ordered_padded_shifts_right() {
        // Raw `1. foo` (3-char marker), rendered ` 1. foo` (4-char marker).
        // Raw col 3 ('f') maps to rendered col 4.
        let line = Line::from(vec![
            Span::styled(" 1. ", Style::default()),
            Span::raw("foo"),
        ]);
        assert_eq!(list_raw_col_to_rendered_col("1. foo", &line, 3), Some(4));
        assert_eq!(list_raw_col_to_rendered_col("1. foo", &line, 5), Some(6));
    }

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
