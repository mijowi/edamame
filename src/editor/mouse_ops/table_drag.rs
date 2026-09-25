use std::time::Instant;

use crate::document::EditDelta;
use crate::editor::table_edit;
use crate::editor::EditorState;
use crate::markdown::table_layout::{self, MIN_COL_WIDTH, PER_COL_OVERHEAD, ROW_END_OVERHEAD};

/// Currently-displayed column widths plus the user-override snapshot (`Some(w)` pinned, `None`
/// auto) to carry into a drag.
///
/// Overrides come from the in-flight `live_table_widths` preview, else the persisted
/// `<!-- tui-columns: [..] -->` comment after the table, else nothing.  Without those first two,
/// resizing a second column would revert the first one's resize by snapping back to naturals.
pub(super) fn current_widths_for_table(
    state: &EditorState,
    info: &table_edit::TableInfo,
) -> (Vec<usize>, Vec<Option<usize>>) {
    let natural = natural_widths(info);

    if let Some((start, widths)) = state.live_table_widths.as_ref() {
        if *start == info.start && widths.len() == info.col_count {
            let rendered = apply_user_widths(&natural, widths);
            return (rendered, widths.clone());
        }
    }

    // Persisted `tui-columns` comment on the line immediately after.
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
            row_widths.push(table_layout::str_cells(trimmed));
            row_min_widths.push(longest_word_cells(trimmed));
        }
        while row_widths.len() < col_count {
            row_widths.push(0);
            row_min_widths.push(0);
        }
        cell_widths.push(row_widths);
        cell_min_widths.push(row_min_widths);
    }
    // `usize::MAX` viewport disables the proportional path: drag anchors want natural widths.
    table_layout::compute_widths(&cell_widths, &cell_min_widths, col_count, usize::MAX, None)
}

/// Widest word in `text`, in terminal cells — the per-cell width floor that keeps the
/// column-width algorithm from breaking a word across rendered rows.  Words split as the
/// renderer's do ([`table_layout::word_ranges`]), so a CJK run floors at one glyph.
fn longest_word_cells(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    table_layout::word_ranges(&chars)
        .into_iter()
        .map(|r| table_layout::cells_of(&chars[r]))
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

/// Resize the left column of the border at `col_idx` by `delta` cells, returning the new
/// `user_widths` vector.
///
/// Unlike a spreadsheet drag, this pins ONLY the left column: the right one keeps whatever pin
/// it had, so widening lets the table grow up to `viewport_width` instead of squeezing its
/// neighbor.
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

    let border_budget = PER_COL_OVERHEAD * n + ROW_END_OVERHEAD;
    let other_total: usize = start_widths
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != left)
        .map(|(_, w)| *w)
        .sum();

    // Grow only up to the viewport edge, leaving room for the other columns and borders.
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

/// Commit a row-drag release: move the source row to the hover destination as a **single** undo
/// step.  `table_edit::swap_rows` only swaps adjacent rows, so [`commit_swap_chain`] composes the
/// run on a `String` copy and lets only the net difference reach the buffer.
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

/// Compose the adjacent-swap chain `src_idx → dst_idx` on a copy of the document, then apply the
/// whole move to the buffer as one `EditDelta`.
///
/// `swap` is the axis-specific primitive (`swap_rows` / `swap_columns`); both return byte offsets
/// confined to the table, so each step can be folded into the simulated string and the table
/// re-located at the unchanged `table_byte_start`.  A refused step ends the chain and commits
/// what has been composed so far.
///
/// The pre-drag cursor offset is restored because `apply_delta` parks the cursor at the end of
/// the changed span — after the trailing-comment merge, the `<!-- tui-columns: … -->` line, whose
/// text `RenderedView`'s raw-reveal would then paint into the last data row.
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

/// Hand the pending column-resize off to the App via the pending-commit flag, which lets
/// `config.table.warn_on_width_injection` intercept the commit without dragging config plumbing
/// into the mouse layer.  The App then either calls
/// [`EditorState::commit_pending_column_widths`] or stages the warning modal first.
pub(super) fn commit_column_border_drag(state: &mut EditorState, table_byte_start: usize) {
    // No live preview for this table means the drag changed nothing.
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

/// Click-driven delete of a single table row, applied through history so undo restores it.
/// No-op when the table moved between snapshot and click, or on the header / alignment row —
/// `table_edit::delete_row` guards both.
///
/// Stamps `EditorState::last_table_delete_at` on success only (a refused delete must not start a
/// cooldown), which arms the `✕` double-click guard in `mouse_ops::table_delete_allowed`.
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

/// Click-driven delete of a single table column — [`delete_table_row_at`] on the other axis,
/// with the same refusal and stamping rules (guarded by `table_edit::delete_column`).
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

    /// A partially-refused chain still commits as one undo entry.  The drag path can't reach
    /// this today (`data_row_at_y` snaps the hover inside the table), which is why the `break`
    /// needs its own test: nothing end-to-end would catch it regressing to one entry per step.
    #[test]
    fn a_chain_refused_partway_commits_the_partial_move_as_one_undo_step() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n| 5 | 6 |\n";
        let mut state = EditorState::new(Buffer::from_str(src), theme());

        // Data rows are 2, 3, 4: the 2→3 and 3→4 steps apply, then row 5 doesn't exist.
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

    /// A chain refused on its first step must record no undo entry at all.
    #[test]
    fn a_chain_refused_outright_records_nothing() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let mut state = EditorState::new(Buffer::from_str(src), theme());

        // Row 2 is the only data row, so the first step is refused.
        commit_row_drag(&mut state, 0, 2, 3);

        assert_eq!(state.buffer.contents(), src);
        assert_eq!(state.history.undo_depth(), 0);
        assert!(!state.dirty, "a refused chain must not dirty the buffer");
    }

    /// The drag anchors floor words exactly as the renderer does: a CJK run at one glyph.
    #[test]
    fn longest_word_cells_splits_cjk_like_the_renderer() {
        assert_eq!(longest_word_cells("日本語日本語"), 2);
        assert_eq!(longest_word_cells("日本 hello"), 5);
    }

    /// Ten wide glyphs are twenty cells of content, and the drag anchors must say so: they are
    /// compared against (and persisted into) the renderer's column widths, so a char-counted
    /// anchor would snap a CJK column to half the width it is drawn at.
    #[test]
    fn natural_widths_measure_wide_glyphs_in_cells() {
        let src = format!("| {} |\n| --- |\n| 值 |\n", "哈".repeat(10));
        let info = table_edit::find_table_at(&src, 0).expect("table at offset 0");

        assert_eq!(natural_widths(&info), vec![20]);
    }
}
