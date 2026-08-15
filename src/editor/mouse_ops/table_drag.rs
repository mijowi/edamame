use std::time::Instant;

use crate::document::EditDelta;
use crate::editor::table_edit;
use crate::editor::EditorState;
use crate::markdown::table_layout::{self, MIN_COL_WIDTH, PER_COL_OVERHEAD, ROW_END_OVERHEAD};

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

/// Commit a row-drag release: move the source row to the hover destination
/// as a **single** undo step.
///
/// `table_edit::swap_rows` only supports adjacent swaps, so the move is
/// `|row_idx - hover_row_idx|` of them in the right direction — but they are
/// composed on a `String` copy of the document by [`commit_swap_chain`] and
/// only the net difference reaches the buffer, so one drag is one undo.
pub(super) fn commit_row_drag(
    state: &mut EditorState,
    table_byte_start: usize,
    src_idx: usize,
    dst_idx: usize,
) {
    if src_idx == dst_idx || src_idx < 2 || dst_idx < 2 {
        return;
    }
    commit_swap_chain(
        state,
        table_byte_start,
        src_idx,
        dst_idx,
        table_edit::swap_rows,
    );
}

/// Compose the adjacent-swap chain `src_idx → dst_idx` on a copy of the
/// document, then apply the whole move to the buffer as one `EditDelta`.
///
/// `swap` is the axis-specific adjacent-swap primitive (`swap_rows` /
/// `swap_columns`); both return a delta with *byte* offsets confined to the
/// table, which is why each intermediate step can be folded into the
/// simulated string and the table re-located at the unchanged
/// `table_byte_start` on the next iteration.  A step the primitive refuses
/// ends the chain and commits what has been composed so far — same
/// semantics as the old per-step loop, minus the partial history entries.
///
/// The pre-drag cursor offset is restored afterwards because `apply_delta`
/// unconditionally moves the cursor to `delta.redo_cursor()` — for a
/// structural rewrite that lands at the end of the changed span, which after
/// the trailing-comment merge sits on the `<!-- tui-columns: ... -->` line.
/// The raw-reveal in `RenderedView` would then paint the comment text into
/// the last data row until the user moved the cursor somewhere else.
fn commit_swap_chain(
    state: &mut EditorState,
    table_byte_start: usize,
    src_idx: usize,
    dst_idx: usize,
    swap: fn(&table_edit::TableInfo, usize, usize) -> Option<EditDelta>,
) {
    let original = state.buffer.contents();
    let mut composed = original.clone();
    let mut cur = src_idx;
    while cur != dst_idx {
        let step = if cur < dst_idx { cur + 1 } else { cur - 1 };
        let Some(info) = table_edit::find_table_at(&composed, table_byte_start) else {
            break;
        };
        let Some(delta) = swap(&info, cur, step) else {
            break;
        };
        composed = delta.apply_to_string(&composed);
        cur = step;
    }

    let Some(byte_delta) = EditDelta::diff(&original, &composed) else {
        return; // chain refused every step — nothing to record.
    };
    let saved_cursor = state.cursor.offset;
    let rope = state.buffer.rope();
    let char_delta = EditDelta {
        offset: rope.byte_to_char(byte_delta.offset),
        removed: byte_delta.removed,
        inserted: byte_delta.inserted,
    };
    state.apply_delta(char_delta);
    state.cursor.offset = saved_cursor.min(state.buffer.len_chars());
    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
    state.update_cursor_block();
}

/// Hand the pending column-resize off to the App by setting the
/// pending-commit flag.  The App then decides — on the next loop
/// iteration — whether to call [`EditorState::commit_pending_column_widths`]
/// directly or stage a width-injection warning modal first.
///
/// This was split out of `mouse_ops::apply` so `config.table.warn_on_width_injection`
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
///
/// Stamps `EditorState::last_table_delete_at` on success (and only on
/// success, so a refused delete can't start a cooldown), which is what
/// arms the `✕` double-click guard in `mouse_ops::table_delete_allowed`.
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
    state.last_table_delete_at = Some(Instant::now());
    state.update_cursor_block();
    state.ensure_cursor_visible(viewport_height, viewport_width);
}

/// Click-driven delete of a single table column.  Mirrors
/// `delete_table_row_at` for the column axis.  No-op when the table
/// scrolled off-screen between snapshot and click, when the table only
/// has one column, or when `col_idx` is out of range — all guarded by
/// `table_edit::delete_column`.  Stamps `last_table_delete_at` on
/// success, same as the row axis.
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
    state.last_table_delete_at = Some(Instant::now());
    state.update_cursor_block();
    state.ensure_cursor_visible(viewport_height, viewport_width);
}

/// Commit a column-drag release: move the source column to the hover
/// destination as a single undo step.  Mirrors `commit_row_drag` on the
/// other axis — see [`commit_swap_chain`] for the composition and the
/// cursor restore.
pub(super) fn commit_column_drag(
    state: &mut EditorState,
    table_byte_start: usize,
    src_idx: usize,
    dst_idx: usize,
) {
    if src_idx == dst_idx {
        return;
    }
    commit_swap_chain(
        state,
        table_byte_start,
        src_idx,
        dst_idx,
        table_edit::swap_columns,
    );
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::document::Buffer;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    /// A chain whose destination is out of range commits the steps that *did*
    /// apply and stops — and, because the whole chain is folded before it
    /// reaches the buffer, that partial move is still a single undo entry.
    ///
    /// The drag path can't reach this today (`data_row_at_y` snaps the hover
    /// inside the table's extent, so `dst_idx` is always a real row), which is
    /// exactly why the `break` needs a test of its own: it is the branch that
    /// would silently regress to the old one-entry-per-step behavior, and the
    /// end-to-end tests in `tests/mouse.rs` can't see it.
    #[test]
    fn a_chain_refused_partway_commits_the_partial_move_as_one_undo_step() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n| 5 | 6 |\n";
        let mut state = EditorState::new(Buffer::from_str(src), theme());

        // Data rows are 2, 3, 4.  Ask for 2 → 6: the 2→3 and 3→4 steps apply,
        // then `swap_rows(4, 5)` refuses because row 5 doesn't exist.
        commit_row_drag(&mut state, 0, 2, 6);

        let moved = state.buffer.contents();
        let rows: Vec<&str> = moved.lines().skip(2).collect();
        assert_eq!(
            rows,
            ["| 3 | 4 |", "| 5 | 6 |", "| 1 | 2 |"],
            "the two legal steps should have applied"
        );
        assert_eq!(state.history.undo_depth(), 1, "partial chain is one entry");

        state.history.undo(&mut state.buffer).expect("one undo");
        assert_eq!(state.buffer.contents(), src);
    }

    /// A chain that is refused on its very first step must not record an undo
    /// entry at all — `EditDelta::diff` returns `None` and the commit bails
    /// before touching the buffer.
    #[test]
    fn a_chain_refused_outright_records_nothing() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let mut state = EditorState::new(Buffer::from_str(src), theme());

        // Row 2 is the only data row; row 3 doesn't exist, so the first step
        // is refused.
        commit_row_drag(&mut state, 0, 2, 3);

        assert_eq!(state.buffer.contents(), src);
        assert_eq!(state.history.undo_depth(), 0);
        assert!(!state.dirty, "a refused chain must not dirty the buffer");
    }
}
