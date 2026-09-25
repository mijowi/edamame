//! Apply [`MouseAction`] values to the editor state.
//!
//! Mirrors `edit_ops` for mouse input: click placement, drag selection, word/line chords,
//! wheel scrolling, checkbox toggles, table gestures.  All coordinate-to-offset translation
//! happens here; the `mouse.rs` dispatcher only sees document-area-relative cells.

mod checkbox;
mod coord;
mod footnotes;
mod links;
mod selection;
mod table_drag;

pub use footnotes::footnote_at_offset;
pub use links::{hovered_link_url, link_at_offset};
pub use selection::visual_selection_to_rendered_text;

use std::time::Duration;

use crate::document::{Selection, VisualSelection};
use crate::editor::list_edit;
use crate::editor::table_edit;
use crate::editor::{EditorState, Mode};
use crate::input::MouseAction;
use crate::ui::table_view::{TableHit, TableLayoutSnapshot};

use self::checkbox::toggle_checkbox_at;
use self::coord::{
    click_to_char_offset, preview_table_cell_band, rendered_click_to_line_col,
    rendered_line_at_row, table_cell_char_range_at,
};
use self::links::{follow_footnote_at_click, follow_link_at_click, link_at_rendered_pos};
use self::selection::{
    expand_selection_to_inline_markers, select_line_at_cursor, select_word_at_cursor,
    word_range_around,
};
use self::table_drag::{
    commit_column_border_drag, commit_column_drag, commit_row_drag, current_widths_for_table,
    delete_table_column_at, delete_table_row_at, resize_widths,
};

/// What a mouse-down/drag interaction targets, decided at mouse-down so `Drag` events dispatch
/// on the original intent.  Variants carry only state invariant across the drag; live
/// coordinates arrive with each `Drag` and are folded into the `Release` commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragTarget {
    /// Text selection anchored at a char offset.  `cell` is the char range of the table cell
    /// the drag began in (Rendered mode only); `Drag` clamps the active end into it.
    TextSelection {
        anchor: usize,
        cell: Option<(usize, usize)>,
    },
    /// Row-handle drag.  `row_idx` is a `TableInfo` row index (≥ 2: header and alignment rows
    /// aren't draggable); `Release` swaps it with `hover_row_idx`.
    TableRow {
        table_byte_start: usize,
        row_idx: usize,
        /// Most-recent hover target; drives the drop indicator and the swap destination.
        hover_row_idx: usize,
    },
    /// Column-border resize at border `col_idx` (between columns `col_idx - 1` and `col_idx`).
    /// `start_widths` makes drag deltas additive from mouse-down; `start_user_widths` keeps
    /// prior pins on other columns intact (the width comment may mix `[10, _, 15]` entries).
    TableColumnBorder {
        table_byte_start: usize,
        col_idx: usize,
        start_widths: Vec<usize>,
        start_user_widths: Vec<Option<usize>>,
        anchor_x: u16,
    },
    /// Column-header drag from `col_idx`; `hover_col_idx` is the current drop target.
    TableColumnHeader {
        table_byte_start: usize,
        col_idx: usize,
        hover_col_idx: usize,
    },
    /// Scrollbar thumb drag.  `grab_offset` is the row distance from the thumb's top edge to
    /// the mouse-down row, keeping the thumb anchored under the pointer.
    Scrollbar { grab_offset: u16 },
}

/// Lines a wheel tick may scroll beyond the last document line.  Keyboard scrolling uses the
/// stricter bound in [`EditorState::scroll_down`], which keeps at least one line visible.
pub const MOUSE_SCROLL_OVERSHOOT: usize = 0;

/// Whether `(col, row)` (document-area-relative) is on a clickable element: checkbox, link,
/// footnote marker, one of the table buttons, or a resizable column border.  The leftmost outer
/// border is not clickable (nothing to its left to resize).
///
/// The complete answer, used by tests.  The app's mouse-move handler composes
/// [`hit_test_clickable_non_link`] with the link hover it already resolved for the hint line.
pub fn hit_test_clickable(
    state: &EditorState,
    col: u16,
    row: u16,
    viewport_width: usize,
    snapshots: &[TableLayoutSnapshot],
) -> bool {
    hit_test_clickable_non_link(state, col, row, viewport_width, snapshots)
        || link_at_rendered_pos(state, col as usize, row as usize, viewport_width).is_some()
}

/// [`hit_test_clickable`] minus the link test.
///
/// Exists because `App::dispatch_mouse_event` already resolves the hovered link for the hint
/// line on every `Moved` event; the link resolver is expensive (allocates the line's char vector
/// and wrap table, re-parses the block on a hit), so it must not run twice per pointer report.
/// The substitution is exact only when `hovered_link` was resolved for the same position and
/// viewport width.
pub fn hit_test_clickable_non_link(
    state: &EditorState,
    col: u16,
    row: u16,
    viewport_width: usize,
    snapshots: &[TableLayoutSnapshot],
) -> bool {
    // Checked before the "past end of line" early return: the `⠿` row handle sits in the
    // external gutter, beyond the table's own line width.
    for snap in snapshots {
        match snap.hit_test(col, row) {
            Some(TableHit::RowHandle { .. })
            | Some(TableHit::ColumnHandle { .. })
            | Some(TableHit::DeleteRowHandle { .. })
            | Some(TableHit::DeleteColumnHandle { .. }) => return true,
            // Same predicate as `dispatch_table_click`: the leftmost outer border is inert.
            Some(TableHit::ColumnBorder { col_idx })
                if col_idx > 0 && col_idx <= snap.col_count =>
            {
                return true;
            }
            _ => {}
        }
    }

    let c = col as usize;
    let r = row as usize;
    let Some((line, visual_col)) = rendered_line_at_row(state, r) else {
        return false;
    };
    let total_width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if c >= total_width {
        return false;
    }
    let _ = visual_col;

    // The `↩` back-link glyph is appended chrome with no raw byte, so it needs a
    // rendered-line hit-test; markers and leaders are found by the source scan below.
    if footnotes::back_link_glyph_at_click(state, col, row).is_some() {
        return true;
    }

    // Hover hitbox must match the toggle hitbox in `toggle_checkbox_at`
    // (`item.start..task_box + 3`, so the bullet itself shows the click cursor).
    if let Some(offset) = click_to_char_offset(state, c, r, viewport_width) {
        let source = state.buffer.contents();
        let click_byte = state.buffer.rope().char_to_byte(offset);
        if footnotes::footnote_at_offset(&source, click_byte).is_some() {
            return true;
        }
        if let Some(info) = list_edit::find_list_at(&source, click_byte) {
            if let Some(item_idx) = list_edit::cursor_item_idx(&info, click_byte) {
                let item = &info.items[item_idx];
                if let Some(task_box) = item.task_box {
                    if click_byte >= item.start && click_byte < task_box + 3 {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Preview-mode mouse handler.  Selection lives in `state.visual_selection` (rendered-line
/// coordinates) rather than `state.selection` so `Action::Copy` can extract rendered text
/// without Markdown markers.  Preview is read-only, so a plain click on a link follows it.
fn apply_preview_action(
    state: &mut EditorState,
    action: MouseAction,
    drag_target: &mut Option<DragTarget>,
    viewport_width: usize,
) {
    match action {
        MouseAction::Click { col, row, .. } => {
            if follow_link_at_click(state, col, row, viewport_width) {
                *drag_target = None;
                state.drag_in_progress = false;
                return;
            }
            match rendered_click_to_line_col(state, col as usize, row as usize, viewport_width) {
                Some((line_idx, char_col)) => {
                    // A click inside a table cell pins drag, painting, and copy to that cell.
                    let band = preview_table_cell_band(state, line_idx, char_col);
                    let char_col = match band {
                        Some(b) => {
                            let (lo, hi) = b.char_cols(&state.parsed.lines[line_idx]);
                            char_col.clamp(lo, hi)
                        }
                        None => char_col,
                    };
                    state.visual_selection = Some(VisualSelection {
                        anchor: (line_idx, char_col),
                        active: (line_idx, char_col),
                        band,
                    });
                    // `anchor` is unused here; the Drag arm only checks for `Some(_)`.
                    *drag_target = Some(DragTarget::TextSelection {
                        anchor: 0,
                        cell: None,
                    });
                    state.drag_in_progress = true;
                }
                None => {
                    state.visual_selection = None;
                }
            }
        }
        MouseAction::DoubleClick { col, row, .. } => {
            if let Some((line_idx, char_col)) =
                rendered_click_to_line_col(state, col as usize, row as usize, viewport_width)
            {
                if let Some((s, e)) = preview_word_range(state, line_idx, char_col) {
                    state.visual_selection =
                        Some(VisualSelection::span((line_idx, s), (line_idx, e)));
                }
            }
            *drag_target = None;
            state.drag_in_progress = false;
        }
        MouseAction::TripleClick { col, row, .. } => {
            if let Some((line_idx, char_col)) =
                rendered_click_to_line_col(state, col as usize, row as usize, viewport_width)
            {
                if let Some(b) = preview_table_cell_band(state, line_idx, char_col) {
                    // Whole cell: every wrapped sub-line of the row within the column band,
                    // ending at the last sub-line's trimmed content (as `select_line_at_cursor`).
                    let lines = &state.parsed.lines;
                    let start_col = lines.get(b.lines.0).map_or(0, |l| b.char_cols(l).0);
                    let end_col = lines.get(b.lines.1).map_or(start_col, |l| {
                        let (lo, hi) = b.char_cols(l);
                        let cell: String = l
                            .spans
                            .iter()
                            .flat_map(|s| s.content.chars())
                            .skip(lo)
                            .take(hi - lo)
                            .collect();
                        lo + cell.trim_end().chars().count()
                    });
                    state.visual_selection = Some(VisualSelection {
                        anchor: (b.lines.0, start_col),
                        active: (b.lines.1, end_col),
                        band: Some(b),
                    });
                } else {
                    let end_col = state
                        .parsed
                        .lines
                        .get(line_idx)
                        .map(|l| l.spans.iter().map(|s| s.content.chars().count()).sum())
                        .unwrap_or(0);
                    state.visual_selection =
                        Some(VisualSelection::span((line_idx, 0), (line_idx, end_col)));
                }
            }
            *drag_target = None;
            state.drag_in_progress = false;
        }
        MouseAction::Drag { col, row } => {
            if drag_target.is_none() {
                return;
            }
            if let Some(mut active) =
                rendered_click_to_line_col(state, col as usize, row as usize, viewport_width)
            {
                if let Some(sel) = state.visual_selection.as_mut() {
                    if let Some(b) = sel.band {
                        active.0 = active.0.clamp(b.lines.0, b.lines.1);
                        if let Some(line) = state.parsed.lines.get(active.0) {
                            let (lo, hi) = b.char_cols(line);
                            active.1 = active.1.clamp(lo, hi);
                        }
                    }
                    sel.active = active;
                } else {
                    state.visual_selection = Some(VisualSelection::span(active, active));
                }
            }
        }
        MouseAction::Release => {
            if let Some(sel) = state.visual_selection {
                if sel.is_empty() {
                    state.visual_selection = None;
                }
            }
            state.drag_in_progress = false;
            *drag_target = None;
        }
        MouseAction::Scroll(delta) => {
            scroll_by_mouse(state, delta, viewport_width);
        }
    }
}

/// Word range under `(line_idx, char_col)` in rendered-line coordinates; boundary detection is
/// the shared `word_range_around`.
fn preview_word_range(
    state: &EditorState,
    line_idx: usize,
    char_col: usize,
) -> Option<(usize, usize)> {
    let line = state.parsed.lines.get(line_idx)?;
    let chars: Vec<char> = line.spans.iter().flat_map(|s| s.content.chars()).collect();
    if chars.is_empty() {
        return None;
    }
    let clamped = char_col.min(chars.len());
    if let Some(range) = word_range_around(chars.len(), clamped, |i| chars[i]) {
        return Some(range);
    }
    // On whitespace, select the single char so the double-click still gives visible feedback.
    if clamped < chars.len() {
        Some((clamped, clamped + 1))
    } else {
        None
    }
}

/// Hit-test a mouse-down against every visible table snapshot and arm a drag target or perform
/// a delete.  Returns `true` when the click was consumed; `false` when it should fall through
/// to cursor placement (a `Cell` hit, the inert leftmost outer border, or no table).
///
/// Shared by the single-, double-, and triple-click arms: a quick re-grab of a handle after a
/// release arrives as a `DoubleClick`, and must behave like the first press.  The `✕` guard is
/// a cooldown keyed off the last delete (`table_delete_allowed`), not off the click chord — the
/// multi-click window restarts on every press, so chord-gating would swallow every press of a
/// user clicking steadily.
fn dispatch_table_click(
    state: &mut EditorState,
    snapshots: &[TableLayoutSnapshot],
    col: u16,
    row: u16,
    drag_target: &mut Option<DragTarget>,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    let Some((snap, hit)) = snapshots
        .iter()
        .find_map(|s| s.hit_test(col, row).map(|h| (s, h)))
    else {
        return false;
    };

    // `Cell` and the inert leftmost border fall through to cursor placement, which focuses the
    // table at the clicked position rather than at its start.
    let acts_on_handle = match hit {
        TableHit::Cell { .. } => false,
        TableHit::ColumnBorder { col_idx } => col_idx > 0 && col_idx <= snap.col_count,
        _ => true,
    };
    if acts_on_handle && focus_table_first(state, snap, viewport_height, viewport_width) {
        *drag_target = None;
        state.drag_in_progress = false;
        return true;
    }

    match hit {
        TableHit::RowHandle { row_idx } => {
            *drag_target = Some(DragTarget::TableRow {
                table_byte_start: snap.table_byte_start,
                row_idx,
                hover_row_idx: row_idx,
            });
            state.drag_in_progress = true;
            true
        }
        TableHit::ColumnHandle { col_idx } => {
            *drag_target = Some(DragTarget::TableColumnHeader {
                table_byte_start: snap.table_byte_start,
                col_idx,
                hover_col_idx: col_idx,
            });
            state.drag_in_progress = true;
            true
        }
        TableHit::ColumnBorder { col_idx } => {
            // Interior borders and the rightmost outer border (resizes the last column).
            if col_idx > 0 && col_idx <= snap.col_count {
                let source = state.buffer.contents();
                if let Some(info) = table_edit::find_table_at(&source, snap.table_byte_start) {
                    let (start_widths, start_user_widths) = current_widths_for_table(state, &info);
                    *drag_target = Some(DragTarget::TableColumnBorder {
                        table_byte_start: info.start,
                        col_idx,
                        start_widths,
                        start_user_widths,
                        anchor_x: col,
                    });
                    state.drag_in_progress = true;
                    return true;
                }
            }
            false
        }
        TableHit::DeleteRowHandle { row_idx } => {
            if !table_delete_allowed(state) {
                return true;
            }
            delete_table_row_at(
                state,
                snap.table_byte_start,
                row_idx,
                viewport_height,
                viewport_width,
            );
            *drag_target = None;
            state.drag_in_progress = false;
            true
        }
        TableHit::DeleteColumnHandle { col_idx } => {
            if !table_delete_allowed(state) {
                return true;
            }
            delete_table_column_at(
                state,
                snap.table_byte_start,
                col_idx,
                viewport_height,
                viewport_width,
            );
            *drag_target = None;
            state.drag_in_progress = false;
            true
        }
        TableHit::Cell { .. } => false,
    }
}

/// A `✕` press within this long of the previous delete is a double-click on one button, not a
/// request to delete a second row.  Anchored to the delete so it always expires.
const TABLE_DELETE_COOLDOWN: Duration = Duration::from_millis(250);

fn table_delete_allowed(state: &EditorState) -> bool {
    state
        .last_table_delete_at
        .is_none_or(|t| t.elapsed() >= TABLE_DELETE_COOLDOWN)
}

/// Focus guard for every table handle: when the cursor isn't in `snap`'s table, move it there
/// and return `true` so the caller consumes the click without acting on the handle.
///
/// `paint_handles` draws handles only on the cursor's table, but hit-testing runs against every
/// visible snapshot; without this a press on another table would drive a control the user was
/// never shown.
fn focus_table_first(
    state: &mut EditorState,
    snap: &TableLayoutSnapshot,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    if cursor_byte >= snap.table_byte_start && cursor_byte < snap.table_byte_end {
        return false;
    }
    let offset = state
        .buffer
        .rope()
        .byte_to_char(snap.table_byte_start.min(state.buffer.rope().len_bytes()));
    state.selection = None;
    state.cursor.offset = offset.min(state.buffer.len_chars());
    state.cursor.preferred_col = state.current_visual_col(viewport_width);
    state.update_cursor_block();
    state.ensure_cursor_visible(viewport_height, viewport_width);
    true
}

/// Apply a mouse action to the editor.
///
/// `drag_target` persists across a click → drag → release sequence.  `snapshots` are the table
/// layout snapshots captured by the last render (empty when no tables are visible).
pub fn apply(
    state: &mut EditorState,
    action: MouseAction,
    drag_target: &mut Option<DragTarget>,
    snapshots: &[TableLayoutSnapshot],
    viewport_height: usize,
    viewport_width: usize,
) {
    // Mouse input deliberately never calls `enter_edit_if_preview`: the user may want to copy
    // rendered text without flipping into edit mode and exposing raw markers.
    if state.mode == Mode::Preview {
        apply_preview_action(state, action, drag_target, viewport_width);
        return;
    }

    match action {
        MouseAction::Click {
            col,
            row,
            modifiers,
        } => {
            // Ctrl-click on a link leaves the cursor where it was.
            if modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                && follow_link_at_click(state, col, row, viewport_width)
            {
                return;
            }
            // A plain click follows footnotes in Rendered mode only; in Raw the markers are
            // literal editable text, so a plain click places the cursor.
            if state.mode == Mode::Rendered
                && follow_footnote_at_click(state, col, row, viewport_width)
            {
                *drag_target = None;
                state.drag_in_progress = false;
                return;
            }
            if dispatch_table_click(
                state,
                snapshots,
                col,
                row,
                drag_target,
                viewport_height,
                viewport_width,
            ) {
                return;
            }

            if toggle_checkbox_at(state, col as usize, row as usize, viewport_width) {
                *drag_target = None;
                state.drag_in_progress = false;
                return;
            }
            // Image blocks are hit-tested by rendered line, not via `click_to_char_offset`,
            // whose offset can spill into the next block at the end of a placeholder line.
            // Once the block's raw source is revealed, clicks land on text normally.
            if let Some(block_idx) = image_block_at_click(state, row as usize, viewport_width) {
                let already_revealed =
                    state.cursor_block_idx == Some(block_idx) && state.cursor_block_revealed();
                if !already_revealed {
                    if let Some(target) = image_block_cursor_target(state, block_idx) {
                        state.selection = None;
                        state.cursor.offset = target;
                        state.cursor.preferred_col = state.current_visual_col(viewport_width);
                        state.update_cursor_block();
                        state.cursor_block_entered_at = None;
                        state.ensure_cursor_visible(viewport_height, viewport_width);
                        *drag_target = None;
                        state.drag_in_progress = false;
                        return;
                    }
                }
            }

            if let Some(offset) =
                click_to_char_offset(state, col as usize, row as usize, viewport_width)
            {
                state.selection = None;
                let new_offset = offset.min(state.buffer.len_chars());

                // Setting `drag_in_progress` makes `cursor_block_revealed()` false until
                // mouse-up, so a click on the cursor's own line would flash raw → rendered →
                // raw.  Skip the flag there — except in tables, whose cell-based reveal needs
                // the suppression to track which cell was clicked — and within a mermaid block,
                // which reveals as a unit and would flash its image back in.
                let new_line = state.buffer.char_to_line(new_offset);
                let same_logical_line = state.cursor_line_idx == Some(new_line);
                let cursor_block_is_table = state
                    .cursor_block_idx
                    .and_then(|idx| state.parsed.source_map.original_range_for_block(idx))
                    .map(|range| {
                        let source = state.buffer.contents();
                        let end = range.end.min(source.len());
                        table_edit::is_table_block(&source[range.start..end])
                    })
                    .unwrap_or(false);
                // Diagram blocks (mermaid fences, `$$...$$` math) reveal as a single unit, so a
                // click on another line inside the same block must not drop drag suppression and
                // flash the image back in for the click-to-mouseup window.
                let new_block_idx = state
                    .parsed
                    .source_map
                    .block_for_byte(state.buffer.rope().char_to_byte(new_offset));
                let same_diagram_block = new_block_idx == state.cursor_block_idx
                    && new_block_idx.is_some_and(|idx| state.parsed.is_diagram_reveal_block(idx));
                let suppress_drag_flag =
                    (same_logical_line && !cursor_block_is_table) || same_diagram_block;

                state.cursor.offset = new_offset;
                // `preferred_col` must be the screen cell column, not the line-relative one:
                // on a wrapped continuation row the latter is far past the content's right
                // edge and makes vertical nav clamp every row to its end.
                state.cursor.preferred_col = state.current_visual_col(viewport_width);
                state.update_cursor_block();
                state.ensure_cursor_visible(viewport_height, viewport_width);
                *drag_target = Some(DragTarget::TextSelection {
                    anchor: state.cursor.offset,
                    // Raw shows the pipes, so free selection is correct there.
                    cell: if state.mode == Mode::Rendered {
                        table_cell_char_range_at(state, state.cursor.offset)
                    } else {
                        None
                    },
                });
                if !suppress_drag_flag {
                    state.drag_in_progress = true;
                }

                let source = state.buffer.contents();
                let click_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
                if let Some(url) = link_at_offset(&source, click_byte) {
                    tracing::info!(target: "mouse", url = %url, "link clicked");
                }
            }
        }
        MouseAction::DoubleClick {
            col,
            row,
            modifiers: _,
        } => {
            if dispatch_table_click(
                state,
                snapshots,
                col,
                row,
                drag_target,
                viewport_height,
                viewport_width,
            ) {
                return;
            }
            if let Some(offset) =
                click_to_char_offset(state, col as usize, row as usize, viewport_width)
            {
                state.cursor.offset = offset.min(state.buffer.len_chars());
                select_word_at_cursor(state);
                state.update_cursor_block();
                state.ensure_cursor_visible(viewport_height, viewport_width);
                *drag_target = None;
            }
        }
        MouseAction::TripleClick {
            col,
            row,
            modifiers: _,
        } => {
            if dispatch_table_click(
                state,
                snapshots,
                col,
                row,
                drag_target,
                viewport_height,
                viewport_width,
            ) {
                return;
            }
            if let Some(offset) =
                click_to_char_offset(state, col as usize, row as usize, viewport_width)
            {
                state.cursor.offset = offset.min(state.buffer.len_chars());
                select_line_at_cursor(state);
                state.update_cursor_block();
                state.ensure_cursor_visible(viewport_height, viewport_width);
                *drag_target = None;
            }
        }
        MouseAction::Drag { col, row } => match drag_target {
            Some(DragTarget::TextSelection { anchor, cell }) => {
                let anchor = *anchor;
                let cell = *cell;
                if let Some(offset) =
                    click_to_char_offset(state, col as usize, row as usize, viewport_width)
                {
                    let mut active = offset.min(state.buffer.len_chars());
                    if let Some((lo, hi)) = cell {
                        active = active.clamp(lo, hi);
                    }
                    state.cursor.offset = active;
                    state.cursor.preferred_col = state.current_visual_col(viewport_width);
                    state.selection = Some(Selection { anchor, active });
                    state.update_cursor_block();
                    state.ensure_cursor_visible(viewport_height, viewport_width);
                }
            }
            Some(DragTarget::TableRow {
                table_byte_start,
                hover_row_idx,
                ..
            }) => {
                // Resolved from the pointer's y alone: a full `hit_test` classification stalled
                // the hover whenever the pointer strayed onto a `│` border or a `├─┼─┤`
                // separator, so the drop indicator tracked the pointer only half the time.
                if let Some(snap) = snapshots
                    .iter()
                    .find(|s| s.table_byte_start == *table_byte_start)
                {
                    if let Some(row_idx) = snap.data_row_at_y(row) {
                        *hover_row_idx = row_idx;
                    }
                }
            }
            Some(DragTarget::TableColumnBorder {
                table_byte_start,
                col_idx,
                start_widths,
                start_user_widths,
                anchor_x,
            }) => {
                let delta = col as i32 - *anchor_x as i32;
                if let Some(new_user_widths) = resize_widths(
                    start_widths,
                    start_user_widths,
                    *col_idx,
                    delta,
                    viewport_width,
                ) {
                    state.live_table_widths = Some((*table_byte_start, new_user_widths));
                    state.refresh_parsed();
                }
            }
            Some(DragTarget::TableColumnHeader {
                table_byte_start,
                hover_col_idx,
                ..
            }) => {
                // Same rule as the row case, on the x axis.
                if let Some(snap) = snapshots
                    .iter()
                    .find(|s| s.table_byte_start == *table_byte_start)
                {
                    if let Some(col_idx) = snap.column_at_x(col) {
                        *hover_col_idx = col_idx;
                    }
                }
            }
            // Scrollbar drags are driven by the App layer, which sees the gutter Rect in
            // absolute terminal coordinates.
            Some(DragTarget::Scrollbar { .. }) => {}
            None => {}
        },
        MouseAction::Release => {
            match drag_target.take() {
                Some(DragTarget::TextSelection { .. }) => {
                    if let Some(sel) = state.selection {
                        if sel.is_empty() {
                            state.selection = None;
                        } else {
                            state.selection =
                                Some(expand_selection_to_inline_markers(&state.buffer, sel));
                        }
                    }
                }
                Some(DragTarget::TableRow {
                    table_byte_start,
                    row_idx,
                    hover_row_idx,
                }) => {
                    commit_row_drag(state, table_byte_start, row_idx, hover_row_idx);
                }
                Some(DragTarget::TableColumnBorder {
                    table_byte_start, ..
                }) => {
                    commit_column_border_drag(state, table_byte_start);
                }
                Some(DragTarget::TableColumnHeader {
                    table_byte_start,
                    col_idx,
                    hover_col_idx,
                }) => {
                    commit_column_drag(state, table_byte_start, col_idx, hover_col_idx);
                }
                Some(DragTarget::Scrollbar { .. }) => {}
                None => {}
            }
            state.drag_in_progress = false;
        }
        MouseAction::Scroll(delta) => {
            scroll_by_mouse(state, delta, viewport_width);
        }
    }
}

/// Index of the image block whose reserved rendered lines contain doc-relative `row` (scroll
/// applied), if any.
fn image_block_at_click(state: &EditorState, row: usize, viewport_width: usize) -> Option<usize> {
    if state.parsed.image_blocks.is_empty() {
        return None;
    }
    let (line_idx, _) =
        state.rendered_line_at_visual_row(state.scroll.saturating_add(row), viewport_width);
    state
        .parsed
        .image_blocks
        .iter()
        .find(|info| {
            state
                .parsed
                .source_map
                .rendered_lines_for_block(info.block_idx)
                .contains(&line_idx)
        })
        .map(|info| info.block_idx)
}

/// Cursor offset for a click anywhere on a rendered image block: the end of the `![alt](url)`
/// line, or for diagram-reveal blocks (mermaid fences, `$$...$$` math) the end of the last content
/// line before the closing delimiter.  Landing inside the block's range (not at its trailing
/// boundary) keeps a click on a block at end-of-file resolving back to the same block so the
/// reveal opens.  `None` when the block has no resolvable source range.
fn image_block_cursor_target(state: &EditorState, block_idx: usize) -> Option<usize> {
    let range = state
        .parsed
        .source_map
        .original_range_for_block(block_idx)?;
    let source = state.buffer.contents();
    let end = range.end.min(source.len());
    let block_text = source.get(range.start..end)?;
    let trimmed = block_text.trim_end_matches('\n');

    let target_in_block = if state.parsed.is_mermaid_block(block_idx) {
        trimmed.rfind("\n```").unwrap_or(trimmed.len())
    } else if state.parsed.is_latex_block(block_idx) {
        // Land on the last line of the `$$...$$` source, before the
        // closing delimiter — mirroring the mermaid fence.  Fall back to
        // the last `$$` on a single-line `$$x$$` formula so the target
        // still lands inside the block rather than past its end.
        trimmed
            .rfind("\n$$")
            .or_else(|| trimmed.rfind("$$"))
            .unwrap_or(trimmed.len())
    } else {
        trimmed.len()
    };

    let absolute_byte = range.start + target_in_block;
    Some(
        state
            .buffer
            .rope()
            .byte_to_char(absolute_byte.min(source.len())),
    )
}

/// Set the scroll position from a scrollbar interaction.  Clamped to `total - visible`, the
/// same bound `position_for_click` / `position_for_drag` use, so the thumb's bottom-most
/// position matches the bottom-most reachable scroll; [`scroll_by_mouse`] uses the looser wheel
/// bound.  Does not disturb the cursor.
pub fn set_scroll_absolute(
    state: &mut EditorState,
    position: usize,
    viewport_width: usize,
    viewport_height: usize,
) {
    let total = state.total_visual_rows_for_mode(viewport_width);
    let max_scroll = total.saturating_sub(viewport_height);
    state.scroll = position.min(max_scroll);
}

/// Scroll by `delta` lines, allowing the last rendered line to sit at the top of the viewport.
/// Does not disturb the cursor — re-implemented rather than calling `EditorState::scroll_down`
/// to avoid its companion `clamp_cursor_to_viewport_top`.
pub fn scroll_by_mouse(state: &mut EditorState, delta: i32, _viewport_width: usize) {
    if delta == 0 {
        return;
    }
    let total = state.total_visual_rows_for_mode(state.viewport_width);
    if total == 0 {
        state.scroll = 0;
        return;
    }
    let max_scroll = total.saturating_sub(1) + MOUSE_SCROLL_OVERSHOOT;
    if delta > 0 {
        state.scroll = (state.scroll + delta as usize).min(max_scroll);
    } else {
        state.scroll = state.scroll.saturating_sub((-delta) as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::document::Buffer;
    use crossterm::event::KeyModifiers;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn click_plain(col: u16, row: u16) -> MouseAction {
        MouseAction::Click {
            col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn scroll_down_mouse_respects_max() {
        let mut state = EditorState::new(Buffer::from_str("a\nb\nc\nd\n"), theme());
        state.mode = Mode::Rendered;
        scroll_by_mouse(&mut state, 100, 80);
        assert_eq!(state.scroll, state.parsed.line_count().saturating_sub(1));
    }

    #[test]
    fn scroll_up_clamps_at_zero() {
        let mut state = EditorState::new(Buffer::from_str("hello\nworld\n"), theme());
        state.mode = Mode::Rendered;
        state.scroll = 1;
        scroll_by_mouse(&mut state, -5, 80);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn click_places_cursor_on_paragraph() {
        let text = "Hello world\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        let mut target: Option<DragTarget> = None;
        apply(&mut state, click_plain(6, 0), &mut target, &[], 10, 80);
        assert_eq!(state.cursor.offset, 6);
        assert_eq!(state.selection, None);
        assert_eq!(
            target,
            Some(DragTarget::TextSelection {
                anchor: 6,
                cell: None
            })
        );
    }

    #[test]
    fn double_click_selects_word() {
        let text = "hello world";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        state.cursor.offset = 7; // inside "world"
        select_word_at_cursor(&mut state);
        assert_eq!(
            state.selection,
            Some(Selection {
                anchor: 6,
                active: 11
            })
        );
    }

    #[test]
    fn triple_click_selects_line() {
        let text = "first line\nsecond\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        state.cursor.offset = 3;
        select_line_at_cursor(&mut state);
        let sel = state.selection.expect("selection set");
        assert_eq!(sel.anchor, 0);
        assert_eq!(sel.active, 11); // up to end of "first line\n"
    }

    /// Clicking a promoted `$$...$$` block must drop the cursor *inside*
    /// the block (before the closing `$$`), so it resolves back to the same
    /// block and the raw-source reveal opens — even for a formula at EOF
    /// with no trailing newline, the case where the generic "end of source"
    /// target used to land past the block.
    #[test]
    fn click_target_lands_inside_a_latex_block_at_eof() {
        let text = "intro\n\n$$\nE = mc^2\n$$";
        let state = EditorState::new(Buffer::from_str(text), theme());
        let block_idx = state
            .parsed
            .image_blocks
            .iter()
            .find(|i| state.parsed.is_latex_block(i.block_idx))
            .expect("$$...$$ promotes to a latex image block")
            .block_idx;
        let target = image_block_cursor_target(&state, block_idx).expect("resolvable target");
        let byte = state.buffer.rope().char_to_byte(target);
        assert!(
            byte < text.len(),
            "target must land before the block's end, got byte {byte} of {}",
            text.len()
        );
        assert_eq!(
            state.parsed.source_map.block_for_byte(byte),
            Some(block_idx),
            "target must resolve back to the latex block so the reveal opens"
        );
    }

    #[test]
    fn drag_extends_selection() {
        let text = "hello world";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        let mut target: Option<DragTarget> = None;
        apply(&mut state, click_plain(0, 0), &mut target, &[], 10, 80);
        apply(
            &mut state,
            MouseAction::Drag { col: 5, row: 0 },
            &mut target,
            &[],
            10,
            80,
        );
        let sel = state.selection.expect("drag selects");
        assert_eq!(sel.anchor, 0);
        assert_eq!(sel.active, 5);
    }

    #[test]
    fn click_in_preview_stays_in_preview_and_seeds_visual_selection() {
        let mut state = EditorState::new(Buffer::from_str("hello"), theme());
        assert_eq!(state.mode, Mode::Preview);
        let mut target: Option<DragTarget> = None;
        apply(&mut state, click_plain(1, 0), &mut target, &[], 10, 80);
        assert_eq!(state.mode, Mode::Preview);
        let vs = state.visual_selection.expect("visual selection seeded");
        assert_eq!(vs.anchor, (0, 1));
        assert_eq!(vs.active, (0, 1));
    }

    #[test]
    fn click_on_checkbox_toggles_it() {
        let text = "- [ ] todo\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        let mut target: Option<DragTarget> = None;
        // Clicking the bullet itself (col 0 of `• [ ] todo`) is inside the toggle hitbox.
        apply(&mut state, click_plain(0, 0), &mut target, &[], 10, 80);
        assert!(state.buffer.contents().contains("[x]"));
    }

    #[test]
    fn click_past_end_of_line_clamps_to_line_end() {
        let text = "hi\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        let mut target: Option<DragTarget> = None;
        apply(&mut state, click_plain(50, 0), &mut target, &[], 10, 80);
        assert!(state.cursor.offset <= 2);
    }

    #[test]
    fn link_at_offset_detects_markdown_link() {
        let src = "See [the docs](https://example.com) for more.\n";
        assert_eq!(
            link_at_offset(src, 8),
            Some("https://example.com".to_owned())
        );
        assert_eq!(
            link_at_offset(src, 20),
            Some("https://example.com".to_owned())
        );
    }

    #[test]
    fn link_at_offset_returns_none_outside_link() {
        let src = "See [the docs](https://example.com) for more.\n";
        assert_eq!(link_at_offset(src, 40), None);
        assert_eq!(link_at_offset(src, 1), None);
    }

    #[test]
    fn link_at_offset_ignores_escaped_bracket() {
        let src = r"See \[the docs](https://example.com) for more.";
        assert_eq!(link_at_offset(src, 9), None);
        // A doubled backslash leaves the link live.
        let live = r"See \\[the docs](https://example.com) end";
        assert_eq!(
            link_at_offset(live, 9),
            Some("https://example.com".to_owned())
        );
    }

    #[test]
    fn link_at_offset_handles_nested_brackets() {
        let src = "[one [nested] two](https://ex.com)\n";
        assert_eq!(link_at_offset(src, 7), Some("https://ex.com".to_owned()));
    }

    #[test]
    fn raw_mode_click_places_cursor_on_line() {
        let text = "first\nsecond\nthird\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Raw;
        let mut target: Option<DragTarget> = None;
        apply(&mut state, click_plain(2, 1), &mut target, &[], 10, 80);
        assert_eq!(state.cursor.offset, 8);
    }

    // The click-mapping tests below put the cursor on a spacer line 0 so the clicked line
    // stays rendered (the cursor's own line is revealed raw and would map against raw chars).

    /// `==highlight==` markers must not make the click land off-by-two.
    #[test]
    fn click_in_highlight_places_cursor_correctly() {
        let text = "x\nalpha ==beta== gamma\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        let mut target: Option<DragTarget> = None;
        // Rendered "alpha beta gamma", col 8 is the 't' of "beta": raw col 10.
        apply(&mut state, click_plain(8, 1), &mut target, &[], 10, 80);
        assert_eq!(state.cursor.offset, 2 + 10);
    }

    /// Both the `• ` bullet replacing `- ` and the `**` markers must be accounted for.
    #[test]
    fn click_in_bold_inside_list_item_places_cursor_correctly() {
        let text = "x\n- **bold** text\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        let mut target: Option<DragTarget> = None;
        // Rendered "• bold text", col 3 is the `o`: raw col 5.
        apply(&mut state, click_plain(3, 1), &mut target, &[], 10, 80);
        assert_eq!(state.cursor.offset, 2 + 5);
    }

    /// Ordered-list variant: `1. ` keeps its width, but the `**` markers are still skipped.
    #[test]
    fn click_in_bold_inside_ordered_list_item_places_cursor_correctly() {
        let text = "x\n1. **bold** text\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        let mut target: Option<DragTarget> = None;
        // Rendered "1. bold text", col 4 is the `o`: raw col 6.
        apply(&mut state, click_plain(4, 1), &mut target, &[], 10, 80);
        assert_eq!(state.cursor.offset, 2 + 6);
    }

    /// Blockquote variant: the rendered `▎ ` bar has no Text-event counterpart in the parse.
    #[test]
    fn click_in_bold_inside_blockquote_places_cursor_correctly() {
        let text = "x\n> **bold** text\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        let mut target: Option<DragTarget> = None;
        // Rendered "▎ bold text", col 3 is the `o`: raw col 5.
        apply(&mut state, click_plain(3, 1), &mut target, &[], 10, 80);
        assert_eq!(state.cursor.offset, 2 + 5);
    }
}
