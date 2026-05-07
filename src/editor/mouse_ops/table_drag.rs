use crate::document::{EditDelta, VisualSelection};
use crate::editor::table_edit;
use crate::editor::EditorState;
use crate::input::MouseAction;
use crate::markdown::table_layout::{self, MIN_COL_WIDTH, PER_COL_OVERHEAD, ROW_END_OVERHEAD};
use crate::ui::line_render;

use super::links::follow_link_at_click;
use super::DragTarget;

/// Recompute the currently-displayed column widths and the user-override
/// snapshot for `info`'s table.
///
/// Returns `(rendered_widths, user_widths)` where `rendered_widths` is the
/// actual `Vec<usize>` that's on screen for this table right now, and
/// `user_widths` is the per-column `Option<usize>` override vector
/// (`Some(w)` pinned, `None` auto) to carry into the drag.
///
/// Preference order for the user-widths snapshot:
///   1. The `live_table_widths` drag preview (if one's already in flight for
///      this table — e.g. the user resizes column A, releases, then grabs
///      column B's border without the drag being reset).
///   2. The persisted `<!-- tui-columns: [..] -->` comment immediately after
///      the table, parsed out of the buffer.
///   3. No overrides (`Vec` of all `None`) — every column auto-sizes.
///
/// Without (1) and (2), resizing a second column would revert the first
/// column's prior resize because the override would snap back to naturals.
pub(super) fn current_widths_for_table(
    state: &EditorState,
    info: &table_edit::TableInfo,
) -> (Vec<usize>, Vec<Option<usize>>) {
    let natural = natural_widths(info);

    // (1) Live preview in progress?
    if let Some((start, widths)) = state.live_table_widths.as_ref() {
        if *start == info.start && widths.len() == info.col_count {
            let rendered = apply_user_widths(&natural, widths);
            return (rendered, widths.clone());
        }
    }

    // (2) Persisted `tui-columns` comment on the line immediately after.
    let source = state.buffer.contents();
    if info.end < source.len() {
        let comment_line_end = source[info.end..]
            .find('\n')
            .map(|i| info.end + i)
            .unwrap_or(source.len());
        let comment_line = &source[info.end..comment_line_end];
        if let Some(persisted) = table_layout::parse_column_widths_comment(comment_line) {
            if persisted.len() == info.col_count {
                let rendered = apply_user_widths(&natural, &persisted);
                return (rendered, persisted);
            }
        }
    }

    // (3) Natural widths only — no overrides.
    (natural, vec![None; info.col_count])
}

fn natural_widths(info: &table_edit::TableInfo) -> Vec<usize> {
    let col_count = info.col_count;
    let mut cell_widths: Vec<Vec<usize>> = Vec::with_capacity(info.rows.len());
    let mut cell_min_widths: Vec<Vec<usize>> = Vec::with_capacity(info.rows.len());
    for row in &info.rows {
        let mut row_widths = Vec::with_capacity(col_count);
        let mut row_min_widths = Vec::with_capacity(col_count);
        for cell in row.cells.iter().take(col_count) {
            let trimmed = cell.raw.trim();
            row_widths.push(trimmed.chars().count());
            row_min_widths.push(longest_word_chars(trimmed));
        }
        while row_widths.len() < col_count {
            row_widths.push(0);
            row_min_widths.push(0);
        }
        cell_widths.push(row_widths);
        cell_min_widths.push(row_min_widths);
    }
    // `usize::MAX` viewport disables the proportional path; we want
    // natural / max widths here for drag-anchor purposes.
    table_layout::compute_widths(&cell_widths, &cell_min_widths, col_count, usize::MAX, None)
}

/// Number of characters in the longest whitespace-delimited word in `text`.
/// Returns 0 when `text` is empty or all whitespace.  Used to compute the
/// per-cell `min` (the floor that the column-width algorithm must respect
/// to avoid breaking a word across rendered rows).
fn longest_word_chars(text: &str) -> usize {
    text.split_whitespace()
        .map(|w| w.chars().count())
        .max()
        .unwrap_or(0)
}

fn apply_user_widths(natural: &[usize], user: &[Option<usize>]) -> Vec<usize> {
    let mut out = natural.to_vec();
    for (i, u) in user.iter().take(out.len()).enumerate() {
        if let Some(w) = u {
            out[i] = (*w).max(MIN_COL_WIDTH);
        }
    }
    out
}

/// Resize the left column of the border at `col_idx` by `delta` cells.
///
/// Unlike a spreadsheet-style drag (where widening one column shrinks the
/// next), this pins ONLY the left column of the border.  The right column
/// retains whatever pin it already had (if any) or stays auto — so widening
/// a column lets the table grow up to `viewport_width` instead of squeezing
/// neighbouring cells.
///
/// Returns the new `user_widths` vector (each entry `Some(w)` pinned,
/// `None` auto).  Other columns' pins carry over from
/// `start_user_widths` unchanged.
pub(super) fn resize_widths(
    start_widths: &[usize],
    start_user_widths: &[Option<usize>],
    col_idx: usize,
    delta: i32,
    viewport_width: usize,
) -> Option<Vec<Option<usize>>> {
    let n = start_widths.len();
    if col_idx == 0 || col_idx > n {
        return None;
    }
    let left = col_idx - 1;

    // Border + padding overhead: PER_COL_OVERHEAD per column + ROW_END once.
    let border_budget = PER_COL_OVERHEAD * n + ROW_END_OVERHEAD;
    let other_total: usize = start_widths
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != left)
        .map(|(_, w)| *w)
        .sum();

    // Upper bound: grow up to the viewport edge, leaving room for other
    // columns + borders.  Lower bound: MIN_COL_WIDTH.
    let max_left = viewport_width
        .saturating_sub(border_budget + other_total)
        .max(MIN_COL_WIDTH);
    let target = (start_widths[left] as i32 + delta).max(MIN_COL_WIDTH as i32) as usize;
    let new_left = target.min(max_left);

    let mut out = start_user_widths.to_vec();
    while out.len() < n {
        out.push(None);
    }
    out[left] = Some(new_left);
    Some(out)
}

/// Commit a row-drag release: swap the source row to the hover destination
/// via a chain of adjacent-swap `EditDelta`s.  Note that Phase 2's
/// `table_edit::swap_rows` only supports adjacent swaps, so we apply
/// `|row_idx - hover_row_idx|` of them in the right direction.  Each swap
/// lands in history as its own undo step — acceptable for Phase 6; a future
/// pass can coalesce them into one delta.
pub(super) fn commit_row_drag(
    state: &mut EditorState,
    table_byte_start: usize,
    src_idx: usize,
    dst_idx: usize,
) {
    if src_idx == dst_idx || src_idx < 2 || dst_idx < 2 {
        return;
    }
    // Preserve the pre-drag cursor offset.  `apply_delta` unconditionally
    // moves the cursor to `delta.redo_cursor()` — for a structural swap
    // that lands at `info.end`, which after the trailing-comment merge
    // sits on the `<!-- tui-columns: ... -->` line.  The raw-reveal in
    // `RenderedView` then paints the comment text into the last data
    // row until the user moves the cursor somewhere else.  Since
    // swap_rows preserves total buffer length the saved offset remains
    // a valid char offset after every intermediate swap.
    let saved_cursor = state.cursor.offset;
    let mut cur = src_idx;
    while cur != dst_idx {
        let step = if cur < dst_idx { cur + 1 } else { cur - 1 };
        let source = state.buffer.contents();
        let Some(info) = table_edit::find_table_at(&source, table_byte_start) else {
            break;
        };
        let Some(delta) = table_edit::swap_rows(&info, cur, step) else {
            break;
        };
        let rope = state.buffer.rope();
        let char_delta = EditDelta {
            offset: rope.byte_to_char(delta.offset),
            removed: delta.removed,
            inserted: delta.inserted,
        };
        state.apply_delta(char_delta);
        cur = step;
    }
    state.cursor.offset = saved_cursor.min(state.buffer.len_chars());
    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
    state.update_cursor_block();
}

/// Hand the pending column-resize off to the App by setting the
/// pending-commit flag.  The App then decides — on the next loop
/// iteration — whether to call [`EditorState::commit_pending_column_widths`]
/// directly or stage a width-injection warning modal first.
///
/// Phase 13 split this out of `mouse_ops::apply` so `config.table.warn_on_width_injection`
/// can intercept the commit without dragging config plumbing into the mouse layer.
pub(super) fn commit_column_border_drag(state: &mut EditorState, table_byte_start: usize) {
    // Only flag a pending commit when the live preview actually targets
    // this table — otherwise the drag was a no-op (no width change) and
    // the live state is already clean.
    if state
        .live_table_widths
        .as_ref()
        .is_some_and(|(start, _)| *start == table_byte_start)
    {
        state.pending_column_widths_commit = Some(table_byte_start);
    } else {
        state.live_table_widths = None;
        state.refresh_parsed();
    }
}

/// Click-driven delete of a single table row.  Looks up the table at
/// `table_byte_start`, builds a `delete_row` `EditDelta`, converts it
/// to char-offsets, and applies it through the editor's history so
/// undo restores the row.  No-op when the table has scrolled off-screen
/// between snapshot and click, or when `row_idx` is the header /
/// alignment row (`< 2`) — `table_edit::delete_row` already guards both.
pub(super) fn delete_table_row_at(
    state: &mut EditorState,
    table_byte_start: usize,
    row_idx: usize,
    viewport_height: usize,
    viewport_width: usize,
) {
    let source = state.buffer.contents();
    let Some(info) = table_edit::find_table_at(&source, table_byte_start) else {
        return;
    };
    let Some(delta) = table_edit::delete_row(&info, row_idx) else {
        return;
    };
    let rope = state.buffer.rope();
    let char_delta = EditDelta {
        offset: rope.byte_to_char(delta.offset),
        removed: delta.removed,
        inserted: delta.inserted,
    };
    state.selection = None;
    state.apply_delta(char_delta);
    state.update_cursor_block();
    state.ensure_cursor_visible(viewport_height, viewport_width);
}

/// Click-driven delete of a single table column.  Mirrors
/// `delete_table_row_at` for the column axis.  No-op when the table
/// scrolled off-screen between snapshot and click, when the table only
/// has one column, or when `col_idx` is out of range — all guarded by
/// `table_edit::delete_column`.
pub(super) fn delete_table_column_at(
    state: &mut EditorState,
    table_byte_start: usize,
    col_idx: usize,
    viewport_height: usize,
    viewport_width: usize,
) {
    let source = state.buffer.contents();
    let Some(info) = table_edit::find_table_at(&source, table_byte_start) else {
        return;
    };
    let Some(delta) = table_edit::delete_column(&info, col_idx) else {
        return;
    };
    let rope = state.buffer.rope();
    let char_delta = EditDelta {
        offset: rope.byte_to_char(delta.offset),
        removed: delta.removed,
        inserted: delta.inserted,
    };
    state.selection = None;
    state.apply_delta(char_delta);
    state.update_cursor_block();
    state.ensure_cursor_visible(viewport_height, viewport_width);
}

/// Commit a column-drag release: swap the source column to the hover
/// destination via a chain of adjacent-swap `EditDelta`s.  Mirrors
/// `commit_row_drag` but for columns.
pub(super) fn commit_column_drag(
    state: &mut EditorState,
    table_byte_start: usize,
    src_idx: usize,
    dst_idx: usize,
) {
    if src_idx == dst_idx {
        return;
    }
    // See `commit_row_drag` for why the pre-drag cursor offset must be
    // restored after each swap: otherwise the cursor ends up at
    // `info.end`, which after the trailing-`tui-columns`-comment merge
    // points onto the comment line and the raw-reveal then overlays the
    // comment's raw text into the table's last data row.
    let saved_cursor = state.cursor.offset;
    let mut cur = src_idx;
    while cur != dst_idx {
        let step = if cur < dst_idx { cur + 1 } else { cur - 1 };
        let source = state.buffer.contents();
        let Some(info) = table_edit::find_table_at(&source, table_byte_start) else {
            break;
        };
        let Some(delta) = table_edit::swap_columns(&info, cur, step) else {
            break;
        };
        let rope = state.buffer.rope();
        let char_delta = EditDelta {
            offset: rope.byte_to_char(delta.offset),
            removed: delta.removed,
            inserted: delta.inserted,
        };
        state.apply_delta(char_delta);
        cur = step;
    }
    state.cursor.offset = saved_cursor.min(state.buffer.len_chars());
    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
    state.update_cursor_block();
}

/// Preview-mode mouse handling.  Selections are tracked in rendered
/// coordinates (`(line_idx, char_col)`) so that `Copy` can produce the
/// exact rendered text the user saw — a raw-buffer selection would include
/// the Markdown markers (`*`, `` ` ``, link URLs, etc.) the user didn't.
pub(super) fn apply_preview(
    state: &mut EditorState,
    action: MouseAction,
    drag_target: &mut Option<DragTarget>,
    viewport_width: usize,
) {
    match action {
        MouseAction::Click {
            col,
            row,
            modifiers: _,
        } => {
            // Phase 8: Preview is read-only, so any click on a link
            // (plain OR Ctrl) follows the link.  Checkboxes aren't
            // interactive in Preview — users must be in Rendered mode
            // for editing semantics.
            if follow_link_at_click(state, col, row, viewport_width) {
                *drag_target = None;
                state.drag_in_progress = false;
                return;
            }
            let Some((line_idx, char_col)) = preview_pos(state, col as usize, row as usize) else {
                state.visual_selection = None;
                return;
            };
            state.visual_selection = Some(VisualSelection {
                anchor: (line_idx, char_col),
                active: (line_idx, char_col),
            });
            *drag_target = Some(DragTarget::TextSelection { anchor: 0 });
            state.drag_in_progress = true;
        }
        MouseAction::DoubleClick {
            col,
            row,
            modifiers: _,
        } => {
            if let Some((line_idx, char_col)) = preview_pos(state, col as usize, row as usize) {
                if let Some(range) = preview_word_range(state, line_idx, char_col) {
                    state.visual_selection = Some(VisualSelection {
                        anchor: (line_idx, range.0),
                        active: (line_idx, range.1),
                    });
                }
            }
            *drag_target = None;
            state.drag_in_progress = false;
        }
        MouseAction::TripleClick {
            col,
            row,
            modifiers: _,
        } => {
            if let Some((line_idx, _)) = preview_pos(state, col as usize, row as usize) {
                let end_col = state
                    .parsed
                    .lines
                    .get(line_idx)
                    .map(|l| {
                        l.spans
                            .iter()
                            .map(|s| s.content.chars().count())
                            .sum::<usize>()
                    })
                    .unwrap_or(0);
                state.visual_selection = Some(VisualSelection {
                    anchor: (line_idx, 0),
                    active: (line_idx, end_col),
                });
            }
            *drag_target = None;
            state.drag_in_progress = false;
        }
        MouseAction::Drag { col, row } => {
            if drag_target.is_none() {
                return;
            }
            if let Some(active) = preview_pos(state, col as usize, row as usize) {
                if let Some(sel) = state.visual_selection.as_mut() {
                    sel.active = active;
                } else {
                    state.visual_selection = Some(VisualSelection {
                        anchor: active,
                        active,
                    });
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
            super::scroll_by_mouse(state, delta, viewport_width);
        }
    }
}

/// Resolve `(col, row)` in document-area coords to `(rendered_line_idx,
/// char_col)` within that line.  Returns `None` when the click falls on an
/// empty line (past the last content row) or above the scroll region.
fn preview_pos(state: &EditorState, col: usize, row: usize) -> Option<(usize, usize)> {
    let lines = &state.parsed.lines;
    if lines.is_empty() {
        return None;
    }
    let mut y = 0usize;
    for (idx, line) in lines.iter().enumerate().skip(state.scroll) {
        let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        let rows_used = line_render::visual_rows_for_line(line, usize::MAX).max(1);
        if row < y + rows_used {
            return Some((idx, col.min(total)));
        }
        y += rows_used;
    }
    None
}

/// Return the `[start_col, end_col)` char range of the word surrounding
/// `char_col` on rendered line `line_idx`, using alphanumeric + '_' as the
/// word-char predicate (same as keyboard word navigation).
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
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut start = clamped;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = clamped;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    if start == end {
        // On whitespace or punctuation — no meaningful word.  Fall back to a
        // single-char selection so the user still sees a response.
        if clamped < chars.len() {
            return Some((clamped, clamped + 1));
        }
        return None;
    }
    Some((start, end))
}
