//! `TableView` — per-frame layout snapshot plus the drag-handle rendering
//! needed for Phase 6's mouse-driven row/column drag and column resize.
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

use crate::config::Theme;
use crate::editor::{table_edit, EditorState};
use crate::markdown::table_layout;

/// Reorder-handle glyph — `⠿` (U+283F, braille dots 1-2-3-4-5-6).  Used for
/// BOTH row-reorder (painted in the external left-side gutter at the `│` of
/// each data row) and column-reorder (painted at the centre of each column's
/// top `─` border cell).  The "dot grip" convention reads as "drag me".
pub const REORDER_HANDLE_GLYPH: char = '⠿';
/// Column-resize glyph — `⇔` (U+21D4, left-right arrow).  Painted on each
/// interior `│` of the header row so the user has a visible, hoverable
/// resize target — but clicks on any part of the interior border (the pipe
/// and the two columns adjacent to it, within the Phase 6 `±1` tolerance)
/// still drive a resize.
pub const COLUMN_RESIZE_GLYPH: char = '⇔';

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
}

impl TableLayoutSnapshot {
    /// Hit-test `(col, row)` — both in document-area-relative coordinates —
    /// against this snapshot.  Returns `None` when the click falls outside
    /// any tracked region.
    ///
    /// Precedence: row handle → column handle → column border → cell.
    /// Borders are hit within `±1` of their `│` col so the user doesn't have
    /// to land on the single character of the rendered pipe exactly.
    pub fn hit_test(&self, col: u16, row: u16) -> Option<TableHit> {
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
/// .show_drag_handles` from the caller.
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
        // row_info: Some((info row_idx, info)) when this rendered line maps
        // to a *navigable* table row (header or data); None for borders
        // and separators.
        let mut row_info: Option<(usize, table_edit::TableInfo)> = None;
        if let Some(bb) = block_byte {
            if let Some(range) = state.parsed.source_map.original_range_for_byte(bb) {
                let end = range.end.min(source.len());
                let block_text = &source[range.start..end];
                if table_edit::is_table_block(block_text) {
                    current_block = Some(range.start);
                    if let Some(info) = table_edit::find_table_at(&source, range.start) {
                        let own = state.parsed.source_map.rendered_lines_for_byte(range.start);
                        let sub_in_block = virtual_idx.saturating_sub(own.start);
                        let own_len = own.end.saturating_sub(own.start);
                        if let Some(ri) = table_sub_to_row_idx(sub_in_block, own_len) {
                            row_info = Some((ri, info));
                        }
                    }
                }
            }
        }

        // Close the open snapshot if we've moved into a different block.
        if current_block != open_table_block {
            if let Some(prev) = open_table.take() {
                out.push(prev);
            }
            open_table_block = None;
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
                    });
                    open_table_block = Some(table_start);
                }
            }

            if let Some(snap) = open_table.as_mut() {
                let own = state.parsed.source_map.rendered_lines_for_byte(table_start);
                let sub_in_block = virtual_idx.saturating_sub(own.start);

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

                // Track the top border row (sub 0) — handle painter draws
                // the column-reorder `⠿` glyphs there.
                if show_handles && sub_in_block == 0 && snap.top_border_row.is_none() {
                    snap.top_border_row = Some(area.y + vis_y as u16);
                }

                // Track the header row (sub 1) for the resize `⇔` glyphs.
                if show_handles && sub_in_block == 1 && snap.header_row.is_none() {
                    snap.header_row = Some(area.y + vis_y as u16);
                }

                // Row-reorder gutter column — one cell left of the outer `│`.
                if show_handles && snap.row_handle_col.is_none() && !snap.col_ranges.is_empty() {
                    let outer_left = snap.col_ranges[0].start.saturating_sub(1);
                    snap.row_handle_col = Some(outer_left.saturating_sub(1));
                }

                // Accumulate data-row y-ranges for hit-testing.
                if let Some((row_idx, _)) = &row_info {
                    if *row_idx >= 2 {
                        snap.row_ranges.push(Range {
                            start: area.y + vis_y as u16,
                            end: area.y + vis_y as u16 + rows_used as u16,
                        });
                    }
                }
            }
        } else {
            // Left the table block — any open snapshot was closed above.
        }

        vis_y += rows_used;
        virtual_idx += 1;
    }

    if let Some(prev) = open_table.take() {
        out.push(prev);
    }
    out
}

/// Map a sub-line index inside a rendered table (0 = top border, 1 = header,
/// 2 = thick separator, …) back to the TableInfo row index.  Border and
/// separator lines return `None`.  `own` is the total number of rendered
/// lines in this table block.
fn table_sub_to_row_idx(sub_idx: usize, own: usize) -> Option<usize> {
    // Layout: 0 = top ┌─┐, 1 = header, 2 = thick ┝━┥,
    // then for each data row: one data line + one thin ├─┤ separator,
    // except after the final data row: no separator (own-1 = bottom border).
    if sub_idx == 0 || sub_idx + 1 >= own {
        return None; // top or bottom border
    }
    match sub_idx {
        1 => Some(0),                         // header
        2 => None,                            // thick separator stands in for alignment row
        n if n % 2 == 1 => Some((n + 1) / 2), // data row
        _ => None,                            // thin separator between data rows
    }
}

// ── Handle rendering ────────────────────────────────────────────────────────

/// Paint the drag-handle glyphs on top of each snapshot's table.  The
/// underlying rendered lines have already been drawn; this layer overlays:
///   * `⠿` in the external left gutter for each data row (row-reorder),
///   * `⠿` on the centre of each column's top-border cell (column-reorder),
///   * `⇔` on each interior `│` in the header row (column-resize).
pub fn paint_handles(
    snapshots: &[TableLayoutSnapshot],
    area: Rect,
    buf: &mut TuiBuf,
    theme: &Theme,
) {
    let handle_style: Style = theme.table_border;
    for snap in snapshots {
        // Row-reorder glyphs in the external gutter.
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
    fn table_sub_to_row_idx_handles_layout() {
        // own = 7: sub 0 top, 1 header, 2 thick, 3 data0, 4 thin, 5 data1, 6 bottom
        assert_eq!(table_sub_to_row_idx(0, 7), None);
        assert_eq!(table_sub_to_row_idx(1, 7), Some(0)); // header
        assert_eq!(table_sub_to_row_idx(2, 7), None);
        assert_eq!(table_sub_to_row_idx(3, 7), Some(2)); // first data row
        assert_eq!(table_sub_to_row_idx(4, 7), None);
        assert_eq!(table_sub_to_row_idx(5, 7), Some(3));
        assert_eq!(table_sub_to_row_idx(6, 7), None); // bottom
    }
}
