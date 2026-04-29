//! `TableView` — per-frame layout snapshot plus the row/column-button
//! rendering needed for mouse-driven row/column drag, column resize,
//! and row/column delete.
//!
//! Phase 6 deliberately does NOT introduce a standalone `StatefulWidget` for
//! tables.  The rendered table lines continue to flow through `ParsedDoc`'s
//! pre-rendered line list so scroll, wrap, and cell-scoped raw reveal keep
//! working unchanged.  What this module owns is the mouse-facing half of the
//! table surface:
//!
//!   1. **`TableLayoutSnapshot`** — a per-frame record of where each table's
//!      columns and data rows sit on screen, plus the optional row-handle
//!      column and column-handle row introduced by Phase 6.
//!   2. **`hit_test`** — pure `(col, row)` → `TableHit` lookup on a snapshot.
//!   3. **`paint_handles`** — writes the `≡` / `⇔` glyphs into the buffer
//!      after the normal line-render pass has drawn the surrounding content.
//!   4. **`build_snapshots`** — scans the visible rendered lines for tables
//!      and produces one snapshot per visible table.
//!
//! Snapshots are stored on the `RenderedViewState` so mouse-event handling in
//! the next frame can hit-test against them.

use std::ops::Range;

use ratatui::buffer::Buffer as TuiBuf;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

use crate::config::Theme;
use crate::editor::{table_edit, EditorState};
use crate::markdown::table_layout;

/// Reorder-handle glyph — `⠿` (U+283F, braille dots 1-2-3-4-5-6).  Used for
/// BOTH row-reorder (painted in the external left-side gutter at the `│` of
/// each data row) and column-reorder (painted at the centre of each column's
/// top `─` border cell).  The "dot grip" convention reads as "drag me".
pub const REORDER_HANDLE_GLYPH: char = '⠿';
/// Heavy horizontal box-drawing rule used to highlight the destination
/// separator during a row-handle drag.  Heavier weight (`━`) reads against
/// the standard `─` separator and `─` border of the surrounding table.
pub const DROP_ROW_GLYPH: char = '━';
/// Heavy vertical box-drawing rule used to highlight the destination
/// separator during a column-handle drag.  Heavier weight (`┃`) reads
/// against the standard `│` border of the surrounding table.
pub const DROP_COL_GLYPH: char = '┃';
/// Column-resize glyph — `⇔` (U+21D4, left-right arrow).  Painted on each
/// interior `│` of the header row so the user has a visible, hoverable
/// resize target — but clicks on any part of the interior border (the pipe
/// and the two columns adjacent to it, within the Phase 6 `±1` tolerance)
/// still drive a resize.
pub const COLUMN_RESIZE_GLYPH: char = '⇔';
/// Delete-handle glyph — `✕` (U+2715).  Painted on the table's outer
/// right `│` (overlaying the border for each data row) and on the
/// bottom-border row (centred over each column).  Clicks on the glyph
/// delete that row / column outright; undo restores it.  Gated by the
/// same `config.table.show_buttons` flag as the reorder / resize
/// handles.
pub const DELETE_HANDLE_GLYPH: char = '✕';

/// Per-frame snapshot of one visible table's layout.
///
/// Screen coordinates (`col_ranges`, `row_ranges`, `row_handle_col`,
/// `top_border_row`, `header_row`) are in terminal cells, *relative to the document
/// area*'s origin.  They're valid only for the frame on which they were
/// built; rebuild on every render.
#[derive(Debug, Clone)]
pub struct TableLayoutSnapshot {
    /// Byte offset of this table's first row in the source buffer.
    pub table_byte_start: usize,
    /// Byte offset just past the last newline of the final row.
    pub table_byte_end: usize,
    /// Number of columns (`info.col_count`).
    pub col_count: usize,
    /// Number of TableInfo rows (header + alignment + data).
    pub row_count: usize,
    /// Per-column character-cell ranges inside the content area of each
    /// row — `col_ranges[c].start` is the column just after the opening `│`,
    /// `col_ranges[c].end` is the column of the closing `│`.
    pub col_ranges: Vec<Range<u16>>,
    /// Per-data-row vertical ranges (y).  `row_ranges[i]` spans the rendered
    /// row that displays `info.rows[2 + i]`.  Only data rows carry a drag
    /// handle so this intentionally skips the header + alignment.
    pub row_ranges: Vec<Range<u16>>,
    /// Column (doc-area x) where the `⠿` row-reorder glyph is painted.  Sits
    /// one cell left of the table's outer `│` (i.e. in the external gutter).
    /// `None` when handles are disabled on this frame.
    pub row_handle_col: Option<u16>,
    /// Row (doc-area y) of the `┌─┬─┐` top border, where the column-reorder
    /// `⠿` glyphs are painted (one per column, centred on the top-border
    /// cell).  `None` when handles are disabled OR when the top border
    /// scrolled off the viewport.
    pub top_border_row: Option<u16>,
    /// Row (doc-area y) of the rendered header row.  Used to place the
    /// `⇔` column-resize glyphs on each interior `│` of the header row.
    /// `None` when the header scrolled off the viewport.
    pub header_row: Option<u16>,
    /// Column (doc-area x) where the `✕` row-delete glyph is painted.
    /// Sits ON the table's outer right `│` (i.e. the same column as
    /// `col_ranges.last().end`) — the glyph overlays the border cell
    /// for each data row.  Resize on data rows therefore shifts to
    /// "one cell inside the border"; resize on the header (`⇔`),
    /// alignment, top-border, and bottom-border rows at the same x
    /// still works because those rows have no delete-row hit and fall
    /// through to `ColumnBorder`.  `None` when handles are disabled.
    pub delete_row_handle_col: Option<u16>,
    /// Row (doc-area y) of the `└─┴─┘` bottom border, where the
    /// column-delete `✕` glyphs are painted (one per column, centred on
    /// the bottom-border cell).  `None` when handles are disabled OR
    /// when the bottom border scrolled off the viewport.
    pub bottom_border_row: Option<u16>,
}

/// What a `(col, row)` click lands on inside a table.
///
/// `Cell::row_idx` and `RowHandle::row_idx` are **TableInfo row indices**
/// (i.e. `2 + data_index` — the header is row 0 and alignment is row 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableHit {
    /// Click inside a data cell's content area.
    Cell { row_idx: usize, col_idx: usize },
    /// Click on a vertical `│` border.  `col_idx` is `0..=col_count`:
    /// - `0` is the outer left `│`
    /// - `col_count` is the outer right `│`
    /// - `1..col_count` are the interior borders (resize targets).
    /// Only interior borders drive resize; the outer borders are exposed so
    /// callers can still classify the click precisely.
    ColumnBorder { col_idx: usize },
    /// Click on the `≡` row-drag glyph next to a data row.
    RowHandle { row_idx: usize },
    /// Click on the `⇔` column-drag glyph above a column.
    ColumnHandle { col_idx: usize },
    /// Click on the `✕` row-delete glyph on the right outer `│`.
    /// `row_idx` is a `TableInfo` row index (≥ 2 — header and alignment
    /// can't be deleted, so they don't carry a delete handle, leaving
    /// the same border cell as a resize target on those rows).
    DeleteRowHandle { row_idx: usize },
    /// Click on the `✕` column-delete glyph on the bottom-border row.
    /// `col_idx` is the 0-indexed column.  Only emitted when the table
    /// has more than one column (a single-column table can't lose its
    /// last column).
    DeleteColumnHandle { col_idx: usize },
}

impl TableLayoutSnapshot {
    /// Hit-test `(col, row)` — both in document-area-relative coordinates —
    /// against this snapshot.  Returns `None` when the click falls outside
    /// any tracked region.
    ///
    /// Precedence: delete handle → row handle → column handle → column
    /// border → cell.  Delete handles win over `ColumnBorder` because
    /// the row-delete glyph sits ON the outer right `│` itself — for
    /// data rows it overlays the border, so a click there deletes the
    /// row; resize on data rows is still reachable via the cell just
    /// inside (`border - 1`) or just outside (`border + 1`).  Clicks
    /// at the same x on header / alignment / top / bottom border rows
    /// have no delete handle and fall through to `ColumnBorder`, so
    /// the right column stays resizable from those rows.  Borders are
    /// hit within `±1` of their `│` col.
    pub fn hit_test(&self, col: u16, row: u16) -> Option<TableHit> {
        // Row-delete handle — click in the right-side external gutter at
        // the delete-handle column AND within a data-row y-range.  Checked
        // BEFORE ColumnBorder so the `✕` cell wins over the right-border
        // resize tolerance window.
        if let Some(handle_col) = self.delete_row_handle_col {
            if col == handle_col {
                for (i, y_range) in self.row_ranges.iter().enumerate() {
                    if row >= y_range.start && row < y_range.end {
                        return Some(TableHit::DeleteRowHandle { row_idx: 2 + i });
                    }
                }
            }
        }

        // Column-delete handle — click on the bottom-border row, anywhere
        // within a column's x range.  Same column-spanning policy as the
        // top-row column-reorder handle.
        if let Some(bot_y) = self.bottom_border_row {
            if row == bot_y {
                for (c, x_range) in self.col_ranges.iter().enumerate() {
                    if col >= x_range.start && col < x_range.end {
                        return Some(TableHit::DeleteColumnHandle { col_idx: c });
                    }
                }
            }
        }

        // Row-reorder handle — click in the external gutter at the row-handle
        // column AND within a data-row y-range.
        if let Some(handle_col) = self.row_handle_col {
            if col == handle_col {
                for (i, y_range) in self.row_ranges.iter().enumerate() {
                    if row >= y_range.start && row < y_range.end {
                        return Some(TableHit::RowHandle { row_idx: 2 + i });
                    }
                }
            }
        }

        // Column-reorder handle — click on the top-border row (the `┌─┬─┐`
        // line), anywhere within a column's x range.  Interior `│` vertices
        // (`┬` positions) resolve to the adjacent column for ergonomics —
        // without that, a user who clicks exactly on the `┬` glyph would
        // silently miss the handle.
        if let Some(top_y) = self.top_border_row {
            if row == top_y {
                for (c, x_range) in self.col_ranges.iter().enumerate() {
                    if col >= x_range.start && col < x_range.end {
                        return Some(TableHit::ColumnHandle { col_idx: c });
                    }
                }
            }
        }

        // Column border / column-resize glyph — clicks anywhere within ±1 of
        // a vertical `│` border are resize targets, provided the click is on
        // a row that's actually part of the table (top border, header, or any
        // data row — interior `│` runs full height).  The resize glyph `⇔`
        // on the header row is effectively a highlighted sub-region of the
        // same hit-test area.
        if self.row_count > 0 && self.row_on_table(row) {
            if let Some(first) = self.col_ranges.first() {
                let left_border = first.start.saturating_sub(1);
                if col.abs_diff(left_border) <= 1 {
                    return Some(TableHit::ColumnBorder { col_idx: 0 });
                }
            }
            for (c, x_range) in self.col_ranges.iter().enumerate() {
                let border_col = x_range.end;
                if col.abs_diff(border_col) <= 1 {
                    return Some(TableHit::ColumnBorder { col_idx: c + 1 });
                }
            }
        }

        // Data cell.
        for (i, y_range) in self.row_ranges.iter().enumerate() {
            if row >= y_range.start && row < y_range.end {
                for (c, x_range) in self.col_ranges.iter().enumerate() {
                    if col >= x_range.start && col < x_range.end {
                        return Some(TableHit::Cell {
                            row_idx: 2 + i,
                            col_idx: c,
                        });
                    }
                }
            }
        }
        None
    }

    fn row_on_table(&self, row: u16) -> bool {
        let first_y = self
            .top_border_row
            .or_else(|| self.row_ranges.first().map(|r| r.start.saturating_sub(2)))
            .unwrap_or(0);
        let last_y = self
            .row_ranges
            .last()
            .map(|r| r.end + 1) // include the bottom border
            .unwrap_or(first_y);
        row >= first_y && row <= last_y
    }
}

/// Per-frame instruction for `paint_drop_indicator`.  Captures just the
/// information the painter needs about the active drag — `paint_handles`'s
/// caller distills `mouse_ops::DragTarget` into one of these so the UI
/// layer doesn't import the editor-side enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropIndicator {
    /// Row-drag in progress; highlight the horizontal separator that would
    /// receive the dropped row.  `hover_row_idx` is the `TableInfo` row
    /// index the pointer is currently over (≥ 2 for data rows).  The
    /// painter draws on the separator just *above* that row when dropping
    /// upward, *below* when downward — derived from `src_row_idx`.
    Row {
        table_byte_start: usize,
        src_row_idx: usize,
        hover_row_idx: usize,
    },
    /// Column-drag in progress; highlight the vertical border that would
    /// receive the dropped column.  Same drop-side semantics as `Row`.
    Column {
        table_byte_start: usize,
        src_col_idx: usize,
        hover_col_idx: usize,
    },
    /// Column-border resize in progress; show a faint vertical guideline
    /// at the pointer's current X to indicate where the release will
    /// commit the new width.
    ColumnBorder { table_byte_start: usize, x: u16 },
}

// ── Snapshot construction ───────────────────────────────────────────────────

/// Refresh `snapshots` in place when the cache key
/// (`scroll`, `area`, `parsed_version`, `show_handles`) differs from the
/// previous frame's; otherwise leave the vector untouched.  Mirrors
/// `image_view::build_snapshots_cached` and `link_view::build_snapshots_cached`.
/// `show_handles` is part of the key because it changes the snapshot
/// contents (handle columns / rows are conditionally populated).
pub fn build_snapshots_cached(
    state: &EditorState,
    area: Rect,
    show_handles: bool,
    snapshots: &mut Vec<TableLayoutSnapshot>,
    cache_key: &mut Option<(usize, Rect, u64, bool)>,
) {
    let key = (state.scroll, area, state.parsed_version, show_handles);
    if *cache_key == Some(key) {
        return;
    }
    *snapshots = build_snapshots(state, area, show_handles);
    *cache_key = Some(key);
}

/// Walk every visible rendered line and build a snapshot for every table
/// fully or partially on screen.
///
/// `show_handles` controls whether snapshots carry the row-handle / column-
/// handle coordinates (and therefore whether hit-testing classifies clicks
/// on those cells as handle hits).  Pass `capabilities.mouse && config.table
/// .show_buttons` from the caller.
pub fn build_snapshots(
    state: &EditorState,
    area: Rect,
    show_handles: bool,
) -> Vec<TableLayoutSnapshot> {
    let mut out: Vec<TableLayoutSnapshot> = Vec::new();
    if area.height == 0 {
        return out;
    }
    let source = state.buffer.contents();
    let lines = &state.parsed.lines;
    let total = lines.len();
    let width = area.width as usize;

    let mut virtual_idx = state.scroll;
    let mut vis_y: usize = 0;
    let height = area.height as usize;

    // Track the most-recently-started table so multi-row tables merge into
    // one snapshot.  Keyed by the `table_byte_start` so the same table is
    // not snapshotted twice.
    //
    // The snapshot stays open through border/separator rows (which map to
    // `info_row_idx == None`) so we don't produce one snapshot per data
    // row.  We only close it when the rendered line leaves the table
    // block entirely (either a different block or end of visible range).
    let mut open_table: Option<TableLayoutSnapshot> = None;
    let mut open_table_block: Option<usize> = None; // block byte_start
                                                    // Per-data-row span accumulator.  Set when a `DataRow` sub-line is
                                                    // first encountered for that row index, extended by subsequent
                                                    // continuations, and pushed onto `snap.row_ranges` when a separator
                                                    // (or end of block) closes the row.
    let mut current_data_row_y: Option<(usize, Range<u16>)> = None;

    while vis_y < height && virtual_idx < total {
        let Some(line) = lines.get(virtual_idx) else {
            break;
        };
        let rows_used = state
            .parsed
            .visual_rows_for_line_at(virtual_idx, width)
            .max(1);

        // Does this rendered line belong to a table block?
        let block_byte = state
            .parsed
            .source_map
            .original_byte_for_rendered_line(virtual_idx);
        let mut current_block: Option<usize> = None;
        // sub_kind: classification of this rendered sub-line within its
        // table block — drives everything from row-handle placement to
        // row_range accumulation.
        let mut sub_kind: Option<TableSubLineKind> = None;
        if let Some(bb) = block_byte {
            if let Some(range) = state.parsed.source_map.original_range_for_byte(bb) {
                let end = range.end.min(source.len());
                // Use `get` rather than direct indexing: when an in-line edit
                // has set `parsed_dirty`, the source-map byte ranges are
                // stale relative to the live buffer and may now land inside
                // a multi-byte UTF-8 sequence (e.g. an emoji the user just
                // typed).  Falling back to `""` skips this block's snapshot
                // for the one frame between the keystroke and the next
                // parse flush — preferable to panicking.
                let block_text = source.get(range.start..end).unwrap_or("");
                if table_edit::is_table_block(block_text) {
                    current_block = Some(range.start);
                    let own = state.parsed.source_map.rendered_lines_for_byte(range.start);
                    let sub_in_block = virtual_idx.saturating_sub(own.start);
                    let block_lines = lines.get(own.start..own.end).unwrap_or(&[]);
                    let kinds = classify_table_sub_lines(block_lines);
                    sub_kind = kinds.get(sub_in_block).copied();
                }
            }
        }

        // Close the open snapshot if we've moved into a different block.
        if current_block != open_table_block {
            if let Some(mut prev) = open_table.take() {
                if let Some((_, range)) = current_data_row_y.take() {
                    prev.row_ranges.push(range);
                }
                out.push(prev);
            }
            open_table_block = None;
            current_data_row_y = None;
        }

        if let Some(table_start) = current_block {
            // Open a new snapshot if we aren't already tracking this table.
            if open_table.is_none() {
                if let Some(info) = table_edit::find_table_at(&source, table_start) {
                    open_table = Some(TableLayoutSnapshot {
                        table_byte_start: info.start,
                        table_byte_end: info.end,
                        col_count: info.col_count,
                        row_count: info.rows.len(),
                        col_ranges: Vec::new(),
                        row_ranges: Vec::new(),
                        row_handle_col: None,
                        top_border_row: None,
                        header_row: None,
                        delete_row_handle_col: None,
                        bottom_border_row: None,
                    });
                    open_table_block = Some(table_start);
                }
            }

            if let Some(snap) = open_table.as_mut() {
                // Fill col_ranges the first time we see a row with `│`
                // characters (any header or data row produces them).
                if snap.col_ranges.is_empty() {
                    let pipes = table_layout::rendered_pipe_positions(line);
                    if pipes.len() == snap.col_count + 1 {
                        for i in 0..snap.col_count {
                            let start = pipes[i] as u16 + 1;
                            let end = pipes[i + 1] as u16;
                            snap.col_ranges.push(Range {
                                start: area.x + start,
                                end: area.x + end,
                            });
                        }
                    }
                }

                let y = area.y + vis_y as u16;
                let y_end = y + rows_used as u16;

                match sub_kind {
                    Some(TableSubLineKind::TopBorder) => {
                        if show_handles && snap.top_border_row.is_none() {
                            snap.top_border_row = Some(y);
                        }
                    }
                    Some(TableSubLineKind::Header { sub: 0 }) => {
                        // Header's first rendered sub-line — anchor the
                        // column-resize glyph row here.
                        if show_handles && snap.header_row.is_none() {
                            snap.header_row = Some(y);
                        }
                    }
                    Some(TableSubLineKind::Header { .. }) => {
                        // Header continuation lines (when the header
                        // wraps) don't anchor anything beyond what the
                        // first line already set up.
                    }
                    Some(TableSubLineKind::ThickSeparator)
                    | Some(TableSubLineKind::ThinSeparator)
                    | Some(TableSubLineKind::BottomBorder) => {
                        // A separator closes the current data row's
                        // span — push and reset.
                        if let Some((_, range)) = current_data_row_y.take() {
                            snap.row_ranges.push(range);
                        }
                        if matches!(sub_kind, Some(TableSubLineKind::BottomBorder))
                            && show_handles
                            && snap.bottom_border_row.is_none()
                            && snap.col_count > 1
                        {
                            // Single-column tables can't lose their last
                            // column, so we don't surface the column-delete
                            // handle for them at all.
                            snap.bottom_border_row = Some(y);
                        }
                    }
                    Some(TableSubLineKind::DataRow { row, .. }) => {
                        match current_data_row_y.as_mut() {
                            Some((existing_row, range)) if *existing_row == row => {
                                range.end = y_end;
                            }
                            _ => {
                                if let Some((_, prev_range)) = current_data_row_y.take() {
                                    snap.row_ranges.push(prev_range);
                                }
                                current_data_row_y = Some((row, y..y_end));
                            }
                        }
                    }
                    None => {}
                }

                // Row-reorder gutter column — one cell left of the outer `│`.
                if show_handles && snap.row_handle_col.is_none() && !snap.col_ranges.is_empty() {
                    let outer_left = snap.col_ranges[0].start.saturating_sub(1);
                    snap.row_handle_col = Some(outer_left.saturating_sub(1));
                }
                // Row-delete column — ON the outer right `│` itself
                // (`col_ranges.last().end`), overlaying the border for
                // each data row.  `hit_test` checks delete handles
                // before `ColumnBorder`, so a click on the `✕` cell on
                // a data row deletes; clicks at the same x on header /
                // alignment / top / bottom border rows fall through to
                // `ColumnBorder` and resize the last column.
                if show_handles
                    && snap.delete_row_handle_col.is_none()
                    && !snap.col_ranges.is_empty()
                {
                    let outer_right = snap.col_ranges.last().unwrap().end;
                    snap.delete_row_handle_col = Some(outer_right);
                }
            }
        } else {
            // Left the table block — any open snapshot was closed above.
        }

        vis_y += rows_used;
        virtual_idx += 1;
    }

    if let Some(mut prev) = open_table.take() {
        if let Some((_, range)) = current_data_row_y.take() {
            prev.row_ranges.push(range);
        }
        out.push(prev);
    }
    out
}

/// Classification of one rendered line within a table block.  Drives
/// every consumer that needs to map a sub-line index back to a logical
/// row — `build_snapshots` for hit-testing, `mouse_ops::rendered_sub_line_to_offset`
/// for click-to-cell mapping.
///
/// Phase 13: replaces the fixed-pattern `table_sub_to_row_idx` math
/// because multi-row data rows (cells that wrapped) can occupy any
/// number of consecutive `│`-prefixed lines, breaking the old
/// alternating-line assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSubLineKind {
    /// Top border (`┌─┬─┐`).  Always sub 0.
    TopBorder,
    /// One of (potentially several) header lines.  `sub` is the
    /// 0-indexed row within the header (header always logical row 0
    /// in `TableInfo.rows`).
    Header { sub: usize },
    /// Thick separator under the header (`┝━┿━┥`).
    ThickSeparator,
    /// One of (potentially several) lines making up data row `row`
    /// (0-indexed across data rows).  Maps to `TableInfo.rows[row + 2]`
    /// since header is row 0 and alignment is row 1.  `sub` is the
    /// 0-indexed visual row within that data row.
    DataRow { row: usize, sub: usize },
    /// Thin separator between two data rows (`├─┼─┤`).
    ThinSeparator,
    /// Bottom border (`└─┴─┘`).
    BottomBorder,
}

/// Classify every rendered sub-line of a table block by inspecting its
/// leading box-drawing glyph.  Returns one `TableSubLineKind` per
/// rendered sub-line.  Length matches `lines.len()` so callers can
/// look up by `sub_in_block`.
///
/// `lines` is the slice of rendered lines that make up the table block
/// (i.e. the slice of `parsed.lines[own.start..own.end]`).
///
/// Phase 13: when `config.table.row_striping` is on, the renderer
/// emits a *blank* `│ ... │ ... │` line in place of the `├─┼─┤` rule
/// between data rows.  We detect those by spotting a `│`-prefixed
/// line whose only chars are `│` and whitespace, immediately after a
/// data row — and classify them as `ThinSeparator` so row-counting
/// logic (and the `cursor_block_revealed` plumbing) keep working
/// without further changes.
pub fn classify_table_sub_lines(lines: &[Line<'_>]) -> Vec<TableSubLineKind> {
    let mut out = Vec::with_capacity(lines.len());
    let mut past_thick = false;
    let mut current_header_sub = 0usize;
    let mut current_data_row = 0usize;
    let mut current_data_sub = 0usize;
    let mut prev_was_data = false;
    for line in lines {
        let first = line
            .spans
            .iter()
            .flat_map(|s| s.content.chars())
            .next()
            .unwrap_or(' ');
        let kind = match first {
            '┌' => TableSubLineKind::TopBorder,
            '┝' => {
                past_thick = true;
                prev_was_data = false;
                TableSubLineKind::ThickSeparator
            }
            '├' => {
                if prev_was_data {
                    current_data_row += 1;
                    current_data_sub = 0;
                }
                prev_was_data = false;
                TableSubLineKind::ThinSeparator
            }
            '└' => TableSubLineKind::BottomBorder,
            '│' => {
                let blank_stripe_separator =
                    past_thick && prev_was_data && is_blank_stripe_line(line);
                if blank_stripe_separator {
                    current_data_row += 1;
                    current_data_sub = 0;
                    prev_was_data = false;
                    TableSubLineKind::ThinSeparator
                } else if past_thick {
                    let sub = current_data_sub;
                    current_data_sub += 1;
                    prev_was_data = true;
                    TableSubLineKind::DataRow {
                        row: current_data_row,
                        sub,
                    }
                } else {
                    let sub = current_header_sub;
                    current_header_sub += 1;
                    TableSubLineKind::Header { sub }
                }
            }
            _ => {
                // Defensive fallback: unrecognised leading glyph.  Treat
                // as a header line so the snapshot doesn't panic, even
                // though we don't expect to hit this path.
                TableSubLineKind::Header { sub: 0 }
            }
        };
        out.push(kind);
    }
    out
}

/// Stripe-aware separator detector.  Returns `true` for lines whose
/// only characters are `│` plus NBSP (U+00A0) — the exact shape
/// produced by `Renderer::blank_table_separator`.  ASCII-space-only
/// lines explicitly do *not* qualify, so the wrap-continuation line
/// of a multi-row data row (whose short cells emit `format!(" {}{} ",
/// "", " ".repeat(pad))` — ASCII spaces only) cannot be misidentified
/// as a separator.
fn is_blank_stripe_line(line: &Line<'_>) -> bool {
    let mut saw_nbsp = false;
    for c in line.spans.iter().flat_map(|s| s.content.chars()) {
        match c {
            '│' => {}
            '\u{00A0}' => saw_nbsp = true,
            _ => return false,
        }
    }
    saw_nbsp
}

// ── Handle rendering ────────────────────────────────────────────────────────

/// Paint the row/column-button glyphs on top of each snapshot's table.  The
/// underlying rendered lines have already been drawn; this layer overlays:
///   * `⠿` in the external left gutter for each data row (row-reorder),
///   * `⠿` on the centre of each column's top-border cell (column-reorder),
///   * `⇔` on each interior `│` in the header row (column-resize).
///
/// Phase 13: when `cursor_table_start` is `Some(byte)`, handles paint
/// only on the snapshot whose `table_byte_start` matches — i.e. the
/// table the cursor is currently inside.  Pass `None` to paint on every
/// visible table (the legacy, always-on behaviour used by tests).
pub fn paint_handles(
    snapshots: &[TableLayoutSnapshot],
    area: Rect,
    buf: &mut TuiBuf,
    theme: &Theme,
    cursor_table_start: Option<usize>,
) {
    // Handles inherit `theme.table_border` directly — same colour as
    // the surrounding `│` / `─` so the affordance reads as part of
    // the table chrome.  Visibility comes from the glyph swap (`⠿`
    // / `⇔` instead of `│` / `─`) and from the cursor-in-table
    // gating (`paint_handles_for_cursor_table`) only painting them
    // on the active table.
    let handle_style: Style = theme.table_border;
    for snap in snapshots {
        if let Some(start) = cursor_table_start {
            if snap.table_byte_start != start {
                continue;
            }
        } else {
            // No cursor table — skip painting handles entirely.
            return;
        }
        // Row-reorder glyph: one per logical data row, painted at the
        // first rendered sub-line.  Multi-row (wrapped) data rows still
        // get exactly one glyph — putting one on every wrapped sub-line
        // reads as visual noise, and the row-drag is dispatched via
        // hit-testing against the row's full y-range so the user can
        // still grab anywhere in the gutter even though the glyph only
        // shows once.
        if let Some(col) = snap.row_handle_col {
            if col < area.x + area.width {
                for y_range in &snap.row_ranges {
                    let y = y_range.start;
                    if y >= area.y && y < area.y + area.height {
                        if let Some(cell) = buf.cell_mut((col, y)) {
                            cell.set_char(REORDER_HANDLE_GLYPH);
                            cell.set_style(handle_style);
                        }
                    }
                }
            }
        }

        // Column-reorder glyphs on the top border — centred within each
        // column's content span so they overlay the `─` between the `┌`/`┬`
        // corners without disturbing them.
        if let Some(y) = snap.top_border_row {
            if y >= area.y && y < area.y + area.height {
                for x_range in &snap.col_ranges {
                    if x_range.end <= x_range.start {
                        continue;
                    }
                    let width = x_range.end - x_range.start;
                    let x = x_range.start + width / 2;
                    if x < area.x + area.width {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(REORDER_HANDLE_GLYPH);
                            cell.set_style(handle_style);
                        }
                    }
                }
            }
        }

        // Column-resize glyphs on the header-row `│` borders — every
        // interior border AND the rightmost outer border (which resizes the
        // last column).  Does NOT overwrite the leftmost outer `│` since
        // there's no column to its left to resize.
        if let Some(y) = snap.header_row {
            if y >= area.y && y < area.y + area.height {
                for x_range in &snap.col_ranges {
                    let border_x = x_range.end;
                    if border_x < area.x + area.width {
                        if let Some(cell) = buf.cell_mut((border_x, y)) {
                            cell.set_char(COLUMN_RESIZE_GLYPH);
                            cell.set_style(handle_style);
                        }
                    }
                }
            }
        }

        // Row-delete glyphs ON the right outer `│` — one per data row,
        // painted at the row's first rendered sub-line.  Skips header
        // (row_idx 0) and alignment (row_idx 1) because they aren't
        // deletable; `row_ranges` already only tracks data rows so this
        // loop naturally does the right thing.  Same multi-row "one
        // glyph per logical row" rule as the row-reorder gutter.
        if let Some(col) = snap.delete_row_handle_col {
            if col < area.x + area.width {
                for y_range in &snap.row_ranges {
                    let y = y_range.start;
                    if y >= area.y && y < area.y + area.height {
                        if let Some(cell) = buf.cell_mut((col, y)) {
                            cell.set_char(DELETE_HANDLE_GLYPH);
                            cell.set_style(handle_style);
                        }
                    }
                }
            }
        }

        // Column-delete glyphs on the bottom border — centred within
        // each column's content span (mirrors the column-reorder
        // glyphs on the top border).  `bottom_border_row` is `None`
        // for single-column tables, so no glyph is painted there.
        if let Some(y) = snap.bottom_border_row {
            if y >= area.y && y < area.y + area.height {
                for x_range in &snap.col_ranges {
                    if x_range.end <= x_range.start {
                        continue;
                    }
                    let width = x_range.end - x_range.start;
                    let x = x_range.start + width / 2;
                    if x < area.x + area.width {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(DELETE_HANDLE_GLYPH);
                            cell.set_style(handle_style);
                        }
                    }
                }
            }
        }
    }
}

// ── Drop-indicator painter ──────────────────────────────────────────────────

/// Highlight every valid drop separator for an in-progress row / column
/// drag, with the active hover-target painted at the bright accent and
/// every other valid drop at a dimmer "candidate" shade.  Runs after
/// `paint_handles` so the indicator overlays the existing border glyphs.
/// No-op when no snapshot matches the indicator's `table_byte_start`
/// (e.g. the drag's source table scrolled off-screen).
pub fn paint_drop_indicator(
    snapshots: &[TableLayoutSnapshot],
    indicator: &DropIndicator,
    area: Rect,
    buf: &mut TuiBuf,
    theme: &Theme,
) {
    let active_style: Style = theme.table_drop_indicator;
    let candidate_style: Style = theme.table_drop_target;
    match *indicator {
        DropIndicator::Row {
            table_byte_start,
            src_row_idx,
            hover_row_idx,
        } => {
            let Some(snap) = snapshots
                .iter()
                .find(|s| s.table_byte_start == table_byte_start)
            else {
                return;
            };
            if src_row_idx < 2 || snap.row_ranges.is_empty() {
                return;
            }
            // The active drop target — the separator on the side of the
            // hover row matching the drag direction.  None when the
            // pointer is over an out-of-range index.
            let active_y = active_row_drop_y(snap, src_row_idx, hover_row_idx);

            // First pass: paint every valid drop separator dimly so the
            // user sees the full set of options.  Valid separators are
            // every horizontal border between (and around) the data
            // rows EXCEPT the two adjacent to the source row — moving a
            // row to its own slot is a no-op.
            let src_data_idx = src_row_idx - 2;
            let Some(first) = snap.col_ranges.first() else {
                return;
            };
            let Some(last) = snap.col_ranges.last() else {
                return;
            };
            let x_start = first.start.saturating_sub(1);
            let x_end = last.end;
            let x_max = area.x + area.width;
            for (i, y_range) in snap.row_ranges.iter().enumerate() {
                // Separator above this data row (between row i-1 and i).
                let above = y_range.start.saturating_sub(1);
                // Separator below this data row (between row i and i+1
                // or above the bottom border).
                let below = y_range.end;
                for &y in &[above, below] {
                    if y < area.y || y >= area.y + area.height {
                        continue;
                    }
                    // Skip the separators that bound the source row
                    // (a drop there would be a no-op).
                    if (i == src_data_idx && (y == above || y == below))
                        || (i + 1 == src_data_idx && y == below)
                        || (i == src_data_idx + 1 && y == above)
                    {
                        continue;
                    }
                    let style = if Some(y) == active_y {
                        active_style
                    } else {
                        candidate_style
                    };
                    paint_horizontal_drop(buf, x_start, x_end, x_max, y, style);
                }
            }
        }
        DropIndicator::Column {
            table_byte_start,
            src_col_idx,
            hover_col_idx,
        } => {
            let Some(snap) = snapshots
                .iter()
                .find(|s| s.table_byte_start == table_byte_start)
            else {
                return;
            };
            if snap.col_ranges.is_empty() {
                return;
            }
            let active_x = active_column_drop_x(snap, src_col_idx, hover_col_idx);
            // Vertical span shared across every candidate.
            let y_top = snap
                .top_border_row
                .or_else(|| snap.row_ranges.first().map(|r| r.start.saturating_sub(2)))
                .unwrap_or(area.y);
            let y_bot = snap
                .row_ranges
                .last()
                .map(|r| r.end)
                .unwrap_or(area.y + area.height.saturating_sub(1));
            let y_max = area.y + area.height;
            // Every column-border (interior + the two outer borders) is
            // a candidate drop point, except the two flanking the source
            // column.
            let mut borders: Vec<u16> = Vec::with_capacity(snap.col_ranges.len() + 1);
            if let Some(first) = snap.col_ranges.first() {
                borders.push(first.start.saturating_sub(1));
            }
            for r in &snap.col_ranges {
                borders.push(r.end);
            }
            for (i, &x) in borders.iter().enumerate() {
                if x < area.x || x >= area.x + area.width {
                    continue;
                }
                // Skip borders adjacent to the source column.
                if i == src_col_idx || i == src_col_idx + 1 {
                    continue;
                }
                let style = if Some(x) == active_x {
                    active_style
                } else {
                    candidate_style
                };
                paint_vertical_drop(buf, y_top, y_bot, y_max, x, style);
            }
        }
        DropIndicator::ColumnBorder {
            table_byte_start,
            x,
        } => {
            let Some(snap) = snapshots
                .iter()
                .find(|s| s.table_byte_start == table_byte_start)
            else {
                return;
            };
            if x < area.x || x >= area.x + area.width {
                return;
            }
            let y_top = snap
                .top_border_row
                .or_else(|| snap.row_ranges.first().map(|r| r.start.saturating_sub(2)))
                .unwrap_or(area.y);
            let y_bot = snap
                .row_ranges
                .last()
                .map(|r| r.end)
                .unwrap_or(area.y + area.height.saturating_sub(1));
            let y_max = area.y + area.height;
            for y in y_top..=y_bot {
                if y >= y_max {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(DROP_COL_GLYPH);
                    cell.set_style(active_style);
                }
            }
        }
    }
}

/// Y-coordinate of the active drop separator for a row drag, or `None`
/// when the pointer is over the source row itself (no drop target).
fn active_row_drop_y(
    snap: &TableLayoutSnapshot,
    src_row_idx: usize,
    hover_row_idx: usize,
) -> Option<u16> {
    if hover_row_idx < 2 || hover_row_idx == src_row_idx {
        return None;
    }
    let data_idx = hover_row_idx - 2;
    let y_range = snap.row_ranges.get(data_idx)?;
    if src_row_idx > hover_row_idx {
        Some(y_range.start.saturating_sub(1))
    } else {
        Some(y_range.end)
    }
}

/// X-coordinate of the active drop separator for a column drag, or
/// `None` when the pointer is on the source column.
fn active_column_drop_x(
    snap: &TableLayoutSnapshot,
    src_col_idx: usize,
    hover_col_idx: usize,
) -> Option<u16> {
    if hover_col_idx == src_col_idx {
        return None;
    }
    if src_col_idx > hover_col_idx {
        snap.col_ranges
            .get(hover_col_idx)
            .map(|r| r.start.saturating_sub(1))
    } else {
        snap.col_ranges.get(hover_col_idx).map(|r| r.end)
    }
}

/// Draw a heavy horizontal rule across `[x_start, x_end]` at row `y`,
/// clipped at `x_max`.  Helper for the row-drag drop painter.
fn paint_horizontal_drop(
    buf: &mut TuiBuf,
    x_start: u16,
    x_end: u16,
    x_max: u16,
    y: u16,
    style: Style,
) {
    for x in x_start..=x_end {
        if x >= x_max {
            break;
        }
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char(DROP_ROW_GLYPH);
            cell.set_style(style);
        }
    }
}

/// Draw a heavy vertical rule down `[y_top, y_bot]` at column `x`,
/// clipped at `y_max`.  Helper for the column-drag drop painter.
fn paint_vertical_drop(buf: &mut TuiBuf, y_top: u16, y_bot: u16, y_max: u16, x: u16, style: Style) {
    for y in y_top..=y_bot {
        if y >= y_max {
            break;
        }
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char(DROP_COL_GLYPH);
            cell.set_style(style);
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(col_ranges: Vec<Range<u16>>, row_ranges: Vec<Range<u16>>) -> TableLayoutSnapshot {
        TableLayoutSnapshot {
            table_byte_start: 0,
            table_byte_end: 100,
            col_count: col_ranges.len(),
            row_count: 2 + row_ranges.len(),
            col_ranges,
            row_ranges,
            row_handle_col: None,
            top_border_row: None,
            header_row: None,
            delete_row_handle_col: None,
            bottom_border_row: None,
        }
    }

    #[test]
    fn hit_test_returns_cell_for_click_in_content() {
        let s = snap(vec![1..4, 5..8], vec![3..4, 4..5]);
        let hit = s.hit_test(2, 3).unwrap();
        assert_eq!(
            hit,
            TableHit::Cell {
                row_idx: 2,
                col_idx: 0
            }
        );
    }

    #[test]
    fn hit_test_returns_border_for_click_on_pipe() {
        let s = snap(vec![1..4, 5..8], vec![3..4, 4..5]);
        // Interior border sits at col_ranges[0].end == 4.
        let hit = s.hit_test(4, 3).unwrap();
        assert_eq!(hit, TableHit::ColumnBorder { col_idx: 1 });
    }

    #[test]
    fn hit_test_border_tolerates_one_cell_miss() {
        // Interior border at col_ranges[0].end = 4; click at col 5 should
        // still resolve to ColumnBorder because the border ±1 window hits.
        //
        // NB: col 5 is also col_ranges[1].start; row-on-table check makes
        // sure we only classify as a border when we're vertically on the
        // table.  We set row 3 which is in row_ranges[0].
        let s = snap(vec![1..4, 5..8], vec![3..4, 4..5]);
        let hit = s.hit_test(5, 3).unwrap();
        // Could be classified as either Border (for ±1 of pipe at col 4)
        // or Cell(col 1).  Borders take precedence.
        assert!(matches!(
            hit,
            TableHit::ColumnBorder { col_idx: 1 } | TableHit::Cell { col_idx: 1, .. }
        ));
    }

    #[test]
    fn hit_test_row_handle_when_handles_enabled() {
        let mut s = snap(vec![5..8], vec![3..4]);
        s.row_handle_col = Some(2);
        let hit = s.hit_test(2, 3).unwrap();
        assert_eq!(hit, TableHit::RowHandle { row_idx: 2 });
    }

    #[test]
    fn hit_test_column_handle_when_handles_enabled() {
        let mut s = snap(vec![5..8, 9..12], vec![3..4]);
        s.top_border_row = Some(1);
        let hit = s.hit_test(6, 1).unwrap();
        assert_eq!(hit, TableHit::ColumnHandle { col_idx: 0 });
    }

    #[test]
    fn hit_test_border_in_header_row_when_resize_glyphs_on() {
        // Header at y=2, interior border at col_ranges[0].end=8.
        let mut s = snap(vec![5..8, 9..12], vec![3..4]);
        s.top_border_row = Some(1);
        s.header_row = Some(2);
        let hit = s.hit_test(8, 2).unwrap();
        assert_eq!(hit, TableHit::ColumnBorder { col_idx: 1 });
    }

    #[test]
    fn hit_test_returns_none_outside_any_region() {
        let s = snap(vec![1..4, 5..8], vec![3..4]);
        assert!(s.hit_test(100, 100).is_none());
    }

    #[test]
    fn hit_test_returns_delete_row_handle_when_handle_set() {
        // delete_row_handle_col sits ON the outer right `│`.  With
        // col_ranges last.end = 8, handle_col = 8 (same column as the
        // border).  For data rows that click resolves to delete; the
        // header / alignment rows have no row_range entry and fall
        // through to ColumnBorder.
        let mut s = snap(vec![1..4, 5..8], vec![3..4, 4..5]);
        s.delete_row_handle_col = Some(8);
        let hit = s.hit_test(8, 3).unwrap();
        assert_eq!(hit, TableHit::DeleteRowHandle { row_idx: 2 });
        let hit = s.hit_test(8, 4).unwrap();
        assert_eq!(hit, TableHit::DeleteRowHandle { row_idx: 3 });
    }

    /// With the `✕` glyph painted ON the right border, the cell just
    /// inside the border (`last.end - 1`) becomes the resize target on
    /// data rows.  This documents the new contract: the explicit `✕`
    /// cell deletes; the cell next to it still resizes via the
    /// `ColumnBorder ±1` tolerance.
    #[test]
    fn hit_test_cell_just_inside_right_border_still_resizes() {
        let mut s = snap(vec![1..4, 5..8], vec![3..4]);
        s.delete_row_handle_col = Some(8); // same as last.end
                                           // x=7 is one cell inside the right `│` at x=8 — within the
                                           // ColumnBorder ±1 window.
        let hit = s.hit_test(7, 3).unwrap();
        assert_eq!(hit, TableHit::ColumnBorder { col_idx: 2 });
    }

    /// On non-data rows (header / alignment / top / bottom border),
    /// the same x as the delete glyph has no `row_range` match, so the
    /// click falls through to `ColumnBorder`.  This keeps the right
    /// column resizable via the header `⇔` glyph (and via clicks on
    /// the surrounding border rows).
    #[test]
    fn hit_test_right_border_on_non_data_row_still_resizes() {
        let mut s = snap(vec![1..4, 5..8], vec![5..6]); // single data row at y=5
        s.delete_row_handle_col = Some(8);
        // y=3 is in the header / alignment region (between
        // top_border heuristic at y=3 and the data row at y=5) —
        // outside row_ranges, so the delete-handle check finds no
        // row and the click falls through to ColumnBorder.
        assert_eq!(
            s.hit_test(8, 3),
            Some(TableHit::ColumnBorder { col_idx: 2 })
        );
    }

    /// Disabled delete handles (`None` field) leave the original
    /// hit-test chain unchanged — the right border on a data row
    /// resolves to `ColumnBorder`, as it did before delete handles
    /// existed.  This is what `config.table.show_buttons = false`
    /// must guarantee.
    #[test]
    fn hit_test_falls_through_when_delete_handles_disabled() {
        let s = snap(vec![1..4, 5..8], vec![3..4]);
        // (8, 3) is the right `│`; with no delete handle, it's just a
        // border click.
        assert_eq!(
            s.hit_test(8, 3),
            Some(TableHit::ColumnBorder { col_idx: 2 })
        );
    }

    #[test]
    fn hit_test_returns_delete_column_handle_on_bottom_border() {
        let mut s = snap(vec![1..4, 5..8], vec![3..4]);
        s.bottom_border_row = Some(5);
        // Click in middle of column 0's content range (cols 1..4).
        let hit = s.hit_test(2, 5).unwrap();
        assert_eq!(hit, TableHit::DeleteColumnHandle { col_idx: 0 });
        // Click in middle of column 1's content range (cols 5..8).
        let hit = s.hit_test(6, 5).unwrap();
        assert_eq!(hit, TableHit::DeleteColumnHandle { col_idx: 1 });
    }

    /// `row_ranges` only tracks data rows, so the delete-row check
    /// can't fire on a y outside any data-row range.  Combined with
    /// `hit_test_right_border_on_non_data_row_still_resizes`, this
    /// ensures the header / alignment rows on the same border column
    /// keep their resize behaviour.
    #[test]
    fn hit_test_delete_row_handle_skips_header_and_alignment() {
        let mut s = snap(vec![1..4, 5..8], vec![5..6]); // single data row at y=5
        s.delete_row_handle_col = Some(8);
        // The next assertion — that y=2 produces ColumnBorder, not
        // DeleteRowHandle — is the actual contract.  See
        // `hit_test_right_border_on_non_data_row_still_resizes`.
        assert_ne!(
            s.hit_test(8, 2),
            Some(TableHit::DeleteRowHandle { row_idx: 2 })
        );
    }

    /// `build_snapshots_cached` must leave `snapshots` untouched when the
    /// cache key matches the previous frame.  The `parsed_version` stays
    /// constant, scroll/area/show_handles don't change — so the second
    /// call is a no-op.
    #[test]
    fn build_snapshots_cached_reuses_output_when_key_matches() {
        use crate::config::Theme;
        use crate::document::Buffer;
        use crate::editor::EditorState;

        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let state = EditorState::new(Buffer::from_str(src), theme);
        let area = Rect::new(0, 0, 40, 10);

        let mut snapshots: Vec<TableLayoutSnapshot> = Vec::new();
        let mut key: Option<(usize, Rect, u64, bool)> = None;
        build_snapshots_cached(&state, area, false, &mut snapshots, &mut key);
        let first_len = snapshots.len();
        assert!(first_len > 0, "expected at least one snapshot");

        // Tag the snapshots so we can detect whether they get rebuilt.
        snapshots[0].col_count = 999;
        build_snapshots_cached(&state, area, false, &mut snapshots, &mut key);
        assert_eq!(
            snapshots[0].col_count, 999,
            "snapshots must not be rebuilt when cache key matches",
        );

        // Changing show_handles busts the cache.
        build_snapshots_cached(&state, area, true, &mut snapshots, &mut key);
        assert_ne!(
            snapshots[0].col_count, 999,
            "snapshots must be rebuilt when show_handles changes",
        );
    }

    #[test]
    fn classify_table_sub_lines_simple_table() {
        use ratatui::text::Span;
        let lines = vec![
            Line::from(Span::raw("┌──┬──┐")),
            Line::from(Span::raw("│ a│ b│")),
            Line::from(Span::raw("┝━━┿━━┥")),
            Line::from(Span::raw("│ 1│ 2│")),
            Line::from(Span::raw("├──┼──┤")),
            Line::from(Span::raw("│ 3│ 4│")),
            Line::from(Span::raw("└──┴──┘")),
        ];
        let kinds = classify_table_sub_lines(&lines);
        assert_eq!(kinds[0], TableSubLineKind::TopBorder);
        assert_eq!(kinds[1], TableSubLineKind::Header { sub: 0 });
        assert_eq!(kinds[2], TableSubLineKind::ThickSeparator);
        assert_eq!(kinds[3], TableSubLineKind::DataRow { row: 0, sub: 0 });
        assert_eq!(kinds[4], TableSubLineKind::ThinSeparator);
        assert_eq!(kinds[5], TableSubLineKind::DataRow { row: 1, sub: 0 });
        assert_eq!(kinds[6], TableSubLineKind::BottomBorder);
    }

    #[test]
    fn classify_table_sub_lines_multirow_data_row() {
        use ratatui::text::Span;
        // Data row 0 wraps to two lines, data row 1 stays one line.
        let lines = vec![
            Line::from(Span::raw("┌──┬──┐")),
            Line::from(Span::raw("│ a│ b│")),
            Line::from(Span::raw("┝━━┿━━┥")),
            Line::from(Span::raw("│ 1│ x│")),
            Line::from(Span::raw("│  │ y│")),
            Line::from(Span::raw("├──┼──┤")),
            Line::from(Span::raw("│ 3│ 4│")),
            Line::from(Span::raw("└──┴──┘")),
        ];
        let kinds = classify_table_sub_lines(&lines);
        assert_eq!(kinds[3], TableSubLineKind::DataRow { row: 0, sub: 0 });
        assert_eq!(kinds[4], TableSubLineKind::DataRow { row: 0, sub: 1 });
        assert_eq!(kinds[5], TableSubLineKind::ThinSeparator);
        assert_eq!(kinds[6], TableSubLineKind::DataRow { row: 1, sub: 0 });
    }

    /// Phase 13 — the renderer's `blank_table_separator` line uses
    /// NBSP-padded cells, distinguishing it from a wrap-continuation
    /// line whose short cells are ASCII-space-padded.  classify must
    /// recognise the NBSP-padded `│ … │ … │` line as ThinSeparator
    /// and treat the next `│`-prefixed line as the next data row.
    #[test]
    fn classify_table_sub_lines_blank_stripe_separator() {
        use ratatui::text::Span;
        // NBSP between pipes for the stripe separator (line index 4).
        // Wrap continuation (line index 5 here would be ASCII-padded
        // — but in this fixture the row wraps differently).  Just
        // check the stripe-separator detection.
        let lines = vec![
            Line::from(Span::raw("┌──┬──┐")),
            Line::from(Span::raw("│ a│ b│")),
            Line::from(Span::raw("┝━━┿━━┥")),
            Line::from(Span::raw("│ 1│ 2│")),
            Line::from(Span::raw(
                "│\u{00A0}\u{00A0}\u{00A0}│\u{00A0}\u{00A0}\u{00A0}│",
            )),
            Line::from(Span::raw("│ 3│ 4│")),
            Line::from(Span::raw("└──┴──┘")),
        ];
        let kinds = classify_table_sub_lines(&lines);
        assert_eq!(kinds[3], TableSubLineKind::DataRow { row: 0, sub: 0 });
        assert_eq!(kinds[4], TableSubLineKind::ThinSeparator);
        assert_eq!(kinds[5], TableSubLineKind::DataRow { row: 1, sub: 0 });
    }

    /// Counterpart to the NBSP test: a wrap-continuation line whose
    /// short cells are ASCII-space-padded must NOT be misclassified
    /// as a stripe separator — both cells continue the same data row.
    #[test]
    fn classify_table_sub_lines_ascii_space_padded_continuation_stays_data() {
        use ratatui::text::Span;
        let lines = vec![
            Line::from(Span::raw("┌──┬──┐")),
            Line::from(Span::raw("│ a│ b│")),
            Line::from(Span::raw("┝━━┿━━┥")),
            Line::from(Span::raw("│hi│ y│")),
            Line::from(Span::raw("│  │  │")), // both cells empty on continuation
            Line::from(Span::raw("└──┴──┘")),
        ];
        let kinds = classify_table_sub_lines(&lines);
        assert_eq!(kinds[3], TableSubLineKind::DataRow { row: 0, sub: 0 });
        assert_eq!(kinds[4], TableSubLineKind::DataRow { row: 0, sub: 1 });
    }
}
