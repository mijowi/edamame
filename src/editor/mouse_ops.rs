//! Apply [`MouseAction`] values to the editor state.
//!
//! Mirrors `edit_ops` but for mouse input: click placement, drag selection,
//! word/line selection chords, wheel scrolling, and checkbox toggles.  All
//! coordinate-to-buffer-offset translation happens here; the `mouse.rs`
//! dispatcher only sees document-area-relative cells.

use crate::document::{EditDelta, Selection, VisualSelection};
use crate::editor::link::LinkTarget;
use crate::editor::list_edit;
use crate::editor::table_edit;
use crate::editor::{EditorState, Mode};
use crate::input::MouseAction;
use crate::markdown::table_layout::{self, MIN_COL_WIDTH, PER_COL_OVERHEAD, ROW_END_OVERHEAD};
use crate::ui::line_render;
use crate::ui::table_view::{TableHit, TableLayoutSnapshot};
use ratatui::text::Line;

/// What a mouse-down/drag interaction currently targets.
///
/// Phase 6 replaced the old `Option<usize>` drag anchor with this enum so
/// `MouseAction::Drag` events can dispatch on the user's original intent —
/// text selection remains the fallback when the click didn't land on any
/// table-specific region.
///
/// All variants carry only the state that's invariant across a single drag;
/// the live mouse-event coordinates arrive with each `Drag` and get folded
/// into whatever commit the `Release` produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragTarget {
    /// Plain text selection anchored at a buffer char offset.
    TextSelection { anchor: usize },
    /// Row-handle drag in the table that begins at `table_byte_start`.
    /// `row_idx` is a `TableInfo` row index (≥ 2 — header and alignment
    /// aren't draggable).  Tracked across mouse-move events; the release
    /// handler swaps rows between this starting index and the final drop.
    TableRow {
        table_byte_start: usize,
        row_idx: usize,
        /// Most-recent hover target (updated by `Drag`).  Used by rendering
        /// to highlight the drop indicator and by `Release` to pick the
        /// swap destination.
        hover_row_idx: usize,
    },
    /// Column-border resize drag at border `col_idx` (the border between
    /// columns `col_idx - 1` and `col_idx`; `col_idx == 0` is the left
    /// outer border, unused here; `col_idx == col_count` is the right
    /// outer border, also unused).  `start_widths` captures the rendered
    /// widths at mouse-down so drag deltas are additive from the starting
    /// point; `start_user_widths` captures which columns were already
    /// user-pinned so the drag preserves prior pins on other columns
    /// (partial-pin support — the comment can mix `[10, _, 15]` entries).
    TableColumnBorder {
        table_byte_start: usize,
        col_idx: usize,
        start_widths: Vec<usize>,
        start_user_widths: Vec<Option<usize>>,
        anchor_x: u16,
    },
    /// Column-header drag in the table at `table_byte_start`, starting at
    /// column `col_idx`.  `hover_col_idx` is the current drop target.
    TableColumnHeader {
        table_byte_start: usize,
        col_idx: usize,
        hover_col_idx: usize,
    },
}

/// Number of lines a wheel tick scrolls beyond the last document line.
///
/// Mouse scrolling is allowed to park the last line near the top of the
/// viewport so the user can comfortably read and edit the tail of a document.
/// Keyboard scrolling uses the stricter bound in [`EditorState::scroll_down`]
/// which keeps at least one line visible.
pub const MOUSE_SCROLL_OVERSHOOT: usize = 0;

/// If `(col, row)` falls on a Markdown link, return its classified
/// [`LinkTarget`].  Used by the App to stash the currently hovered
/// link on `App::hovered_link` so Phase 9 can surface the target on
/// the hint line.
pub fn hovered_link_target(
    state: &EditorState,
    col: u16,
    row: u16,
    viewport_width: usize,
) -> Option<LinkTarget> {
    let (line, _) = rendered_line_at_row(state, row as usize)?;
    if !span_at_col_has_modifier(&line, col as usize, ratatui::style::Modifier::UNDERLINED) {
        return None;
    }
    let url = link_url_for_click(state, col as usize, row as usize, viewport_width)?;
    let base_dir = state
        .buffer
        .path()
        .and_then(|p| p.parent())
        .map(|p| p.to_owned());
    Some(LinkTarget::parse(&url, base_dir.as_deref()))
}

/// Hit-test the position `(col, row)` (in document-area-relative coords) to
/// determine whether it falls on a clickable element: a task-list checkbox
/// glyph, a Markdown link, one of the four table buttons (row-reorder
/// `⠿`, column-reorder `⠿`, row-delete `✕`, column-delete `✕`), or a
/// resizable column border (the `⇔` glyph and the surrounding `±1`
/// resize-tolerance window).
///
/// The leftmost outer column border is intentionally NOT classified —
/// there's no column to its left to resize, so a click there falls
/// through to cell placement and the cursor stays as text.
///
/// Used by the app's mouse-move handler to update the terminal pointer shape
/// so the mouse cursor renders as a pointing hand over clickable regions.
pub fn hit_test_clickable(
    state: &EditorState,
    col: u16,
    row: u16,
    viewport_width: usize,
    snapshots: &[TableLayoutSnapshot],
) -> bool {
    // Table buttons + resize borders — snapshot hit-test is independent
    // of rendered-line content (the `⠿` row-reorder glyph sits in the
    // external gutter, beyond the table's own line width).  Checked
    // first so the "past end of line" early-return below doesn't
    // suppress the hand cursor for gutter clicks.
    for snap in snapshots {
        match snap.hit_test(col, row) {
            Some(TableHit::RowHandle { .. })
            | Some(TableHit::ColumnHandle { .. })
            | Some(TableHit::DeleteRowHandle { .. })
            | Some(TableHit::DeleteColumnHandle { .. }) => return true,
            // Match the click dispatcher's predicate: only borders that
            // actually drive a resize (interior + the rightmost outer)
            // turn the cursor into a hand.  `col_idx == 0` is the
            // leftmost outer border with no column to its left, so a
            // click there falls through to cell placement.
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
        // Past the end of the rendered line — nothing visible to hover.
        return false;
    }
    let _ = visual_col;

    // Link: check whether the rendered span at `c` was emitted with the
    // `link_text` style.  The renderer is the only producer of UNDERLINED +
    // Cyan spans, so the presence of the UNDERLINED modifier is a reliable
    // marker that the hover landed on a link glyph.
    if span_at_col_has_modifier(&line, c, ratatui::style::Modifier::UNDERLINED) {
        return true;
    }

    // Checkbox: reuse the full click-to-offset translation, since the glyph is
    // rendered as plain `[ ]` / `[x] ` text without a distinguishing style.
    // Hover hitbox matches the toggle hitbox in `toggle_checkbox_at` —
    // `item.start..task_box + 3` so the bullet itself shows the click cursor.
    if let Some(offset) = click_to_char_offset(state, c, r, viewport_width) {
        let source = state.buffer.contents();
        let click_byte = state.buffer.rope().char_to_byte(offset);
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

/// Look up the rendered `Line` and the visual column within it that
/// correspond to document-area row `row`.  Accounts for scroll and wrap; the
/// returned visual col is `row - y_of_first_row_of_line`.
fn rendered_line_at_row(
    state: &EditorState,
    row: usize,
) -> Option<(ratatui::text::Line<'static>, usize)> {
    let lines = &state.parsed.lines;
    if lines.is_empty() {
        return None;
    }
    let (mut line_idx, mut first_sub_row) =
        state.rendered_line_at_visual_row(state.scroll.saturating_add(row), state.viewport_width);
    let mut y = 0usize;
    while let Some(line) = lines.get(line_idx) {
        let rows_used = line_render::visual_rows_for_line(line, state.viewport_width).max(1);
        let visible_rows = rows_used.saturating_sub(first_sub_row).max(1);
        if y < visible_rows {
            return Some((line.clone(), first_sub_row));
        }
        y += visible_rows;
        line_idx += 1;
        first_sub_row = 0;
    }
    None
}

/// True if the span covering char-col `col` in `line` has `modifier` set.
fn span_at_col_has_modifier(
    line: &ratatui::text::Line<'_>,
    col: usize,
    modifier: ratatui::style::Modifier,
) -> bool {
    let mut walk = 0usize;
    for span in &line.spans {
        let span_len = span.content.chars().count();
        if col < walk + span_len {
            return span.style.add_modifier.contains(modifier);
        }
        walk += span_len;
    }
    false
}

/// Apply a mouse action to the editor.
///
/// `drag_target` persists state across a click → drag → release sequence.
/// `snapshots` are the per-frame table layout snapshots captured at the end
/// of the last render; they drive hit-testing for table-specific drag
/// classification (row handles, column borders, column handles).  Pass an
/// empty slice when no tables are visible.
pub fn apply(
    state: &mut EditorState,
    action: MouseAction,
    drag_target: &mut Option<DragTarget>,
    snapshots: &[TableLayoutSnapshot],
    viewport_height: usize,
    viewport_width: usize,
) {
    // Preview-mode clicks work over rendered coordinates and stay in
    // Preview — the user may want to copy rendered text without entering
    // edit mode (entering edit mode would expose raw Markdown markers and
    // change the text under the pointer).  Keyboard actions still transition
    // Preview → Rendered via `enter_edit_if_preview` in `edit_ops`.
    if state.mode == Mode::Preview {
        apply_preview(state, action, drag_target, viewport_width);
        return;
    }

    match action {
        MouseAction::Click {
            col,
            row,
            modifiers,
        } => {
            // Phase 8: Ctrl-click on a link bypasses cursor placement
            // entirely — we return early after firing the link-open
            // side effect so the cursor stays where it was.
            if modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                && follow_link_at_click(state, col, row, viewport_width)
            {
                return;
            }
            // Phase 6: hit-test the click against every visible table's
            // layout snapshot.  Row-handle / column-handle / column-border
            // hits set a table-specific `DragTarget`; Cell hits fall through
            // to normal cursor placement (the cursor lands inside the cell).
            if let Some((snap, hit)) = snapshots
                .iter()
                .find_map(|s| s.hit_test(col, row).map(|h| (s, h)))
            {
                match hit {
                    TableHit::RowHandle { row_idx } => {
                        *drag_target = Some(DragTarget::TableRow {
                            table_byte_start: snap.table_byte_start,
                            row_idx,
                            hover_row_idx: row_idx,
                        });
                        state.drag_in_progress = true;
                        return;
                    }
                    TableHit::ColumnHandle { col_idx } => {
                        *drag_target = Some(DragTarget::TableColumnHeader {
                            table_byte_start: snap.table_byte_start,
                            col_idx,
                            hover_col_idx: col_idx,
                        });
                        state.drag_in_progress = true;
                        return;
                    }
                    TableHit::ColumnBorder { col_idx } => {
                        // Resize targets: every interior border AND the
                        // rightmost outer border (the latter resizes the
                        // last column).  The leftmost outer border
                        // (`col_idx == 0`) has no column to its left, so it
                        // stays inert.
                        if col_idx > 0 && col_idx <= snap.col_count {
                            let source = state.buffer.contents();
                            if let Some(info) =
                                table_edit::find_table_at(&source, snap.table_byte_start)
                            {
                                let (start_widths, start_user_widths) =
                                    current_widths_for_table(state, &info);
                                *drag_target = Some(DragTarget::TableColumnBorder {
                                    table_byte_start: info.start,
                                    col_idx,
                                    start_widths,
                                    start_user_widths,
                                    anchor_x: col,
                                });
                                state.drag_in_progress = true;
                                return;
                            }
                        }
                        // Outer border — fall through to cell placement.
                    }
                    TableHit::DeleteRowHandle { row_idx } => {
                        delete_table_row_at(
                            state,
                            snap.table_byte_start,
                            row_idx,
                            viewport_height,
                            viewport_width,
                        );
                        *drag_target = None;
                        state.drag_in_progress = false;
                        return;
                    }
                    TableHit::DeleteColumnHandle { col_idx } => {
                        delete_table_column_at(
                            state,
                            snap.table_byte_start,
                            col_idx,
                            viewport_height,
                            viewport_width,
                        );
                        *drag_target = None;
                        state.drag_in_progress = false;
                        return;
                    }
                    TableHit::Cell { .. } => {
                        // Fall through to normal cell placement below.
                    }
                }
            }

            if toggle_checkbox_at(state, col as usize, row as usize, viewport_width) {
                *drag_target = None;
                state.drag_in_progress = false;
                return;
            }
            if let Some(offset) =
                click_to_char_offset(state, col as usize, row as usize, viewport_width)
            {
                state.selection = None;
                let new_offset = offset.min(state.buffer.len_chars());
                // A click landing on the same logical line as the cursor
                // already occupies should not flip the cursor block out of
                // its raw view — without this guard, setting
                // `drag_in_progress` below makes `cursor_block_revealed()`
                // return false until the mouse-up arrives, so the line
                // re-renders raw → rendered → raw across the click.
                // Tables are excluded: their cell-based reveal needs the
                // suppression to track which cell the click landed in.
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
                let suppress_drag_flag = same_logical_line && !cursor_block_is_table;

                state.cursor.offset = new_offset;
                // Click target is a screen position — `preferred_col` must
                // be the screen cell column (cell within visual sub-row +
                // any hanging indent).  Plain `cell_col` would store the
                // line-relative cell column, which on a wrapped continuation
                // row is far past the line content's right edge and makes
                // subsequent vertical nav clamp every row to its end.
                state.cursor.preferred_col = state.current_visual_col(viewport_width);
                state.update_cursor_block();
                state.ensure_cursor_visible(viewport_height, viewport_width);
                *drag_target = Some(DragTarget::TextSelection {
                    anchor: state.cursor.offset,
                });
                // Mouse button is down — suppress raw reveal for the block
                // under the cursor so the user's click anchor stays visually
                // aligned during any subsequent drag.
                if !suppress_drag_flag {
                    state.drag_in_progress = true;
                }

                // Phase 5 prerequisite for Phase 8: detect clicks on Markdown
                // link syntax so Phase 8 can wire up URL opening without
                // reworking the mouse-dispatch plumbing.  For now we merely
                // surface the detected URL in the tracing log.
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
            Some(DragTarget::TextSelection { anchor }) => {
                let anchor = *anchor;
                if let Some(offset) =
                    click_to_char_offset(state, col as usize, row as usize, viewport_width)
                {
                    let active = offset.min(state.buffer.len_chars());
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
                // Update `hover_row_idx` to the data-row under the pointer
                // (if any) so Release has a destination to swap toward.
                if let Some(snap) = snapshots
                    .iter()
                    .find(|s| s.table_byte_start == *table_byte_start)
                {
                    if let Some(TableHit::RowHandle { row_idx })
                    | Some(TableHit::Cell { row_idx, .. }) = snap.hit_test(col, row)
                    {
                        if row_idx >= 2 {
                            *hover_row_idx = row_idx;
                        }
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
                if let Some(snap) = snapshots
                    .iter()
                    .find(|s| s.table_byte_start == *table_byte_start)
                {
                    if let Some(TableHit::ColumnHandle { col_idx })
                    | Some(TableHit::Cell { col_idx, .. }) = snap.hit_test(col, row)
                    {
                        *hover_col_idx = col_idx;
                    }
                }
            }
            None => {}
        },
        MouseAction::Release => {
            // Commit per-target semantics, then clear the drag.
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
                None => {}
            }
            state.drag_in_progress = false;
        }
        MouseAction::Scroll(delta) => {
            scroll_by_mouse(state, delta, viewport_width);
        }
    }
}

// ── Phase 6 drag commit helpers ──────────────────────────────────────────────

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
fn current_widths_for_table(
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
fn resize_widths(
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
fn commit_row_drag(
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
        let char_delta = crate::document::EditDelta {
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
fn commit_column_border_drag(state: &mut EditorState, table_byte_start: usize) {
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
fn delete_table_row_at(
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
fn delete_table_column_at(
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
fn commit_column_drag(
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
        let char_delta = crate::document::EditDelta {
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
fn apply_preview(
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
            scroll_by_mouse(state, delta, viewport_width);
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

/// Extract the rendered text covered by `sel` from `lines`.  Lines between
/// the anchor and active endpoints are fully included; the first and last
/// lines are clipped to the selection's char columns.  A newline separates
/// each rendered line so multi-line copies preserve structure.
pub fn visual_selection_to_rendered_text(sel: VisualSelection, lines: &[Line<'_>]) -> String {
    let (start, end) = sel.range();
    let (start_line, start_col) = start;
    let (end_line, end_col) = end;
    if lines.is_empty() || start_line >= lines.len() {
        return String::new();
    }
    let end_line = end_line.min(lines.len() - 1);

    let mut out = String::new();
    for idx in start_line..=end_line {
        let line = &lines[idx];
        let chars: Vec<char> = line.spans.iter().flat_map(|s| s.content.chars()).collect();
        let lo = if idx == start_line { start_col } else { 0 };
        let hi = if idx == end_line {
            end_col
        } else {
            chars.len()
        };
        let lo = lo.min(chars.len());
        let hi = hi.min(chars.len());
        if lo < hi {
            out.extend(chars[lo..hi].iter());
        }
        if idx < end_line {
            out.push('\n');
        }
    }
    out
}

// ── Mode transitions ─────────────────────────────────────────────────────────

fn enter_edit_from_preview(state: &mut EditorState) {
    if state.mode == Mode::Preview {
        state.mode = Mode::Rendered;
    }
}

// ── Scrolling ────────────────────────────────────────────────────────────────

/// Scroll by `delta` lines using the mouse-specific bound that allows the
/// last rendered line to sit at the very top of the viewport.  Does not
/// disturb the cursor.
pub fn scroll_by_mouse(state: &mut EditorState, delta: i32, _viewport_width: usize) {
    if delta == 0 {
        return;
    }
    let total = state.total_visual_rows_for_mode(state.viewport_width);
    if total == 0 {
        state.scroll = 0;
        return;
    }
    // Mouse scroll allows the last line to sit at the TOP of the viewport:
    // max_scroll = total - 1.  `EditorState::scroll_down` already uses the
    // same bound, but we re-implement it here to avoid triggering keyboard's
    // companion `clamp_cursor_to_viewport_top`.
    let max_scroll = total.saturating_sub(1) + MOUSE_SCROLL_OVERSHOOT;
    if delta > 0 {
        state.scroll = (state.scroll + delta as usize).min(max_scroll);
    } else {
        state.scroll = state.scroll.saturating_sub((-delta) as usize);
    }
}

// ── Hit-testing ──────────────────────────────────────────────────────────────

/// Translate a click at `(col, row)` in the document area to a buffer char
/// offset.  Accounts for current scroll and visual-row wrap.
///
/// Returns `None` only when the buffer is empty — all clicks are clamped to
/// the nearest valid position (end of line / end of buffer) so the caller
/// never has to handle "click landed in whitespace past the document".
pub fn click_to_char_offset(
    state: &EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> Option<usize> {
    match state.mode {
        Mode::Raw => Some(raw_click_to_offset(state, col, row, viewport_width)),
        Mode::Preview | Mode::Rendered => {
            Some(rendered_click_to_offset(state, col, row, viewport_width))
        }
    }
}

/// Raw-mode click: walk buffer lines from `state.scroll`, accumulating each
/// line's wrapped visual-row count, and translate the click into a char
/// offset on the appropriate visual sub-row.  Cell-aware so wide chars
/// align and the cursor lands where the user sees it.
fn raw_click_to_offset(
    state: &EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> usize {
    let line_count = state.buffer.line_count();
    let width = viewport_width.max(1);
    let (mut target_line, mut first_sub_row) = state.raw_line_at_visual_row(state.scroll, width);
    let mut y = 0usize;
    while target_line < line_count {
        let text = state
            .buffer
            .line(target_line)
            .map(|s| s.trim_end_matches('\n').to_owned())
            .unwrap_or_default();
        let rows = crate::ui::line_render::visual_rows_of_str(&text, width);
        let used = rows.len().max(1).saturating_sub(first_sub_row).max(1);
        if row < y + used {
            let sub_row = first_sub_row + row - y;
            let line_start = state.buffer.line_to_char(target_line);
            let row_tuple = rows.get(sub_row).copied().unwrap_or((0, 0, 0));
            let raw_col = char_in_row_at_cell(&text, row_tuple, col, 0, sub_row + 1 == rows.len());
            return line_start + raw_col;
        }
        y += used;
        target_line += 1;
        first_sub_row = 0;
    }
    state.buffer.len_chars()
}

/// Cell-aware "click landed in this visual row" → char column on the
/// logical line.  Mirrors `state::raw_col_for_visual_cells` but lives here
/// because the public copy belongs to mouse hit-testing.  See that
/// function for the wide-char snap-past rule and forbidden indent zone.
fn char_in_row_at_cell(
    text: &str,
    row: (usize, usize, usize),
    target_cell: usize,
    indent: usize,
    is_last_row: bool,
) -> usize {
    let (start, end, next_start) = row;
    let max_char_in_row = if is_last_row {
        end
    } else {
        next_start.saturating_sub(1).max(start)
    };
    let row_chars = text.chars().skip(start).take(end - start);
    let in_row = crate::ui::line_render::char_idx_at_cell_col(row_chars, target_cell, indent);
    (start + in_row).min(max_char_in_row)
}

/// Rendered/Preview click: walk through rendered lines from `state.scroll`,
/// accumulating each line's visual-row count, and find which rendered line
/// and sub-row the click landed on.  Then map that rendered sub-line back to
/// a source byte using the source map.
fn rendered_click_to_offset(
    state: &EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> usize {
    let lines = &state.parsed.lines;
    if lines.is_empty() {
        return 0;
    }
    let (mut idx, mut first_sub_row) =
        state.rendered_line_at_visual_row(state.scroll, viewport_width);
    let mut y = 0usize;
    while idx < lines.len() {
        // Per-line lookup against `ParsedDoc`'s O(1) visual-row cache —
        // historically this called `visual_rows_for_line` directly, which
        // adds up on rapid mouse-move events over a long document.
        let rows_used = state
            .parsed
            .visual_rows_for_line_at(idx, viewport_width)
            .max(1);
        let used = rows_used.saturating_sub(first_sub_row).max(1);
        if row < y + used {
            let sub_row = first_sub_row + row - y;
            return rendered_sub_line_to_offset(state, idx, sub_row, col, viewport_width);
        }
        y += used;
        idx += 1;
        first_sub_row = 0;
    }
    state.buffer.len_chars()
}

/// Map `(rendered_line_idx, sub_row_within_line, col)` to a buffer char
/// offset.
///
/// Strategy:
/// 1. Look up the block that produced the rendered line.
/// 2. Compute which raw source line within the block corresponds to this
///    rendered line (skipping the table-top border row when relevant).
/// 3. Within that raw source line, advance `col` chars and convert to a char
///    offset on the rope.
///
/// For blocks with inline formatting (`**bold**` rendering as `bold`), the
/// rendered column may diverge slightly from the raw column.  Given the
/// Phase 1 reveal semantics turn the cursor's line into raw text within
/// `RAW_REVEAL_DELAY`, the click lands at an approximate position that the
/// user can refine with a second click if needed.
fn rendered_sub_line_to_offset(
    state: &EditorState,
    rendered_line_idx: usize,
    sub_row_within_line: usize,
    col: usize,
    viewport_width: usize,
) -> usize {
    let buffer_len = state.buffer.len_chars();
    let source = state.buffer.contents();
    let Some(block_start_byte) = state
        .parsed
        .source_map
        .original_byte_for_rendered_line(rendered_line_idx)
    else {
        return buffer_len;
    };
    let Some(block_range) = state
        .parsed
        .source_map
        .original_range_for_byte(block_start_byte)
    else {
        return buffer_len;
    };
    let block_end = block_range.end.min(source.len());
    // Tolerate stale source-map ranges that land mid-grapheme: when a
    // pending in-line edit has shifted byte offsets after the cursor,
    // direct slicing would panic at the char-boundary check.  Mouse
    // dispatch flushes the parse before reaching here so the empty-
    // string fallback is defence-in-depth, not the routine path.
    let block_text = source.get(block_range.start..block_end).unwrap_or("");

    // How deep into the block's rendered lines did we click?
    let rendered_span = state
        .parsed
        .source_map
        .rendered_lines_for_byte(block_start_byte);
    let sub_idx_in_block = rendered_line_idx.saturating_sub(rendered_span.start);

    // Table click → raw-row index.  Phase 13: classify the rendered
    // sub-line by leading box-drawing glyph instead of relying on a
    // fixed alternating-line pattern, since data rows may now span
    // multiple rendered lines after cell-wrap.
    let is_table = table_edit::is_table_block(block_text);
    let raw_line_idx = if is_table {
        let block_lines = state
            .parsed
            .lines
            .get(rendered_span.start..rendered_span.end.min(state.parsed.lines.len()))
            .unwrap_or(&[]);
        let kinds = crate::ui::table_view::classify_table_sub_lines(block_lines);
        match kinds.get(sub_idx_in_block) {
            Some(crate::ui::table_view::TableSubLineKind::TopBorder)
            | Some(crate::ui::table_view::TableSubLineKind::Header { .. }) => 0, // header line
            Some(crate::ui::table_view::TableSubLineKind::ThickSeparator) => 2, // alignment-row → first data row
            Some(crate::ui::table_view::TableSubLineKind::DataRow { row, .. }) => row + 2,
            Some(crate::ui::table_view::TableSubLineKind::ThinSeparator) => {
                // A separator click snaps to the data row immediately
                // preceding it.  Walk back through `kinds` to find it.
                let mut row = 0usize;
                for k in &kinds[..sub_idx_in_block] {
                    if let crate::ui::table_view::TableSubLineKind::DataRow { row: r, .. } = k {
                        row = *r;
                    }
                }
                row + 2
            }
            Some(crate::ui::table_view::TableSubLineKind::BottomBorder) | None => {
                // Bottom border or out-of-range — snap to the last data
                // row.  Total data rows = info.rows.len() - 2 (header +
                // alignment).  Tables always have at least one data row
                // for `is_table_block` to be true.
                let last_data = block_text.split('\n').count().saturating_sub(2);
                last_data.max(2)
            }
        }
    } else {
        sub_idx_in_block
    };

    // Blank-line "virtual blocks" have no content.  The renderer produces
    // a single empty line for them; place the cursor at block start.
    if block_text.is_empty() {
        return state.buffer.rope().byte_to_char(block_range.start);
    }

    // Walk raw source lines to find the byte start of the target raw line.
    let mut byte_cursor = 0usize;
    let mut line_byte_start = 0usize;
    let mut line_byte_end = block_text.len();
    let mut found_idx = 0usize;
    for (i, line) in block_text.split('\n').enumerate() {
        if i == raw_line_idx {
            line_byte_start = byte_cursor;
            line_byte_end = byte_cursor + line.len();
            found_idx = i;
            break;
        }
        byte_cursor += line.len() + 1;
        if byte_cursor >= block_text.len() {
            // Clamp when raw_line_idx points past the block's last line.
            line_byte_start = byte_cursor.saturating_sub(line.len() + 1);
            line_byte_end = block_text.len();
            found_idx = i;
            break;
        }
    }
    let line_text = &block_text[line_byte_start..line_byte_end];
    let rendered_line = &state.parsed.lines[rendered_line_idx];

    // Tables: rendered cells are padded to layout width, so a simple col →
    // char mapping lands clicks on the wrong cell whenever the rendered cell
    // is wider than its raw counterpart.  Map through the pipe positions
    // instead so the click stays inside the cell the user clicked on.
    let raw_col = if is_table && rendered_line.spans.iter().any(|s| s.content.contains('│')) {
        let row_width = line_row_width(rendered_line, sub_row_within_line);
        let clamped_col = col.min(row_width);
        if let Some(c) = table_click_to_raw_col(line_text, rendered_line, clamped_col) {
            c
        } else {
            clamped_col
        }
    } else {
        // Non-table click: walk the rendered line's wrap layout to find
        // which sub-row the click landed on, then translate the click's
        // cell column into a char position using the cell-aware mapping
        // (wide-char snap-past, hanging-indent forbidden zone).  Falls
        // back to row 0 if the rendered line had fewer wrap rows than the
        // sub_row_within_line we were told.
        let indent = crate::ui::line_render::compute_hanging_indent(rendered_line);
        let rendered_chars: Vec<(char, ratatui::style::Style)> = rendered_line
            .spans
            .iter()
            .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
            .collect();
        let viewport = viewport_width.max(1);
        let rows = crate::ui::line_render::visual_rows_of_chars(&rendered_chars, viewport, indent);
        let sub = sub_row_within_line.min(rows.len().saturating_sub(1));
        let (start, end, next_start) = rows.get(sub).copied().unwrap_or((0, 0, 0));
        let row_indent = if sub == 0 { 0 } else { indent };
        let is_last_row = sub + 1 == rows.len();
        let max_in_row = if is_last_row {
            end
        } else {
            next_start.saturating_sub(1).max(start)
        };
        let row_chars = rendered_chars
            .iter()
            .skip(start)
            .take(end - start)
            .map(|(c, _)| *c);
        let in_row = crate::ui::line_render::char_idx_at_cell_col(row_chars, col, row_indent);
        let rendered_idx = (start + in_row).min(max_in_row);

        // Translate the rendered char index back to a raw char column on
        // `line_text`.  For lines whose rendered form drops or transforms
        // syntax characters (links, code spans), the rendered→raw map
        // makes the cursor land where the user clicked rather than at the
        // matching rendered char's *position* in the raw text.  When the
        // map's rendered length doesn't match the line's actual rendered
        // length (headings/lists/blockquotes have prefix glyphs the map
        // doesn't model) we fall back to the 1:1 column mapping that's
        // been in use since Phase 5.
        let actual_rendered_count = rendered_chars.len();
        let map = rendered_to_raw_char_map(line_text);
        if map.len().saturating_sub(1) == actual_rendered_count {
            map.get(rendered_idx)
                .copied()
                .unwrap_or_else(|| line_text.chars().count())
        } else {
            rendered_idx
        }
    };

    // Advance `raw_col` chars into the raw line.
    let line_char_count = line_text.chars().count();
    let raw_col = raw_col.min(line_char_count);
    let mut byte_offset_in_line = 0usize;
    for (char_idx, ch) in line_text.chars().enumerate() {
        if char_idx == raw_col {
            break;
        }
        byte_offset_in_line += ch.len_utf8();
    }
    let max_byte_in_line = line_text.len();
    let byte_in_block = line_byte_start + byte_offset_in_line.min(max_byte_in_line);
    let absolute_byte = block_range.start + byte_in_block.min(block_text.len());

    let _ = found_idx;
    state
        .buffer
        .rope()
        .byte_to_char(absolute_byte.min(source.len()))
        .min(buffer_len)
}

/// Cell-aware mapping from a rendered column to a raw column for table rows.
///
/// Locates the cell the click falls in by walking the rendered line's `│`
/// positions, then maps the click's position *within* the rendered cell to
/// the matching raw cell:
/// - clicks on actual content chars map 1:1 to the raw content char,
/// - clicks on leading padding land on the first raw content char,
/// - clicks on trailing padding land just past the last non-whitespace char
///   in the raw cell so the cursor never jumps into the next cell.
///
/// Returns `None` when the line doesn't parse as a table row (alignment
/// separator, border) — caller falls back to the default char-by-char map.
fn table_click_to_raw_col(
    raw_line: &str,
    rendered_line: &Line<'_>,
    rendered_col: usize,
) -> Option<usize> {
    let raw_pipes = table_layout::raw_pipe_positions(raw_line);
    let rendered_pipes = table_layout::rendered_pipe_positions(rendered_line);
    if raw_pipes.len() < 2 || rendered_pipes.len() != raw_pipes.len() {
        return None;
    }
    let col_count = rendered_pipes.len() - 1;

    // Which cell does `rendered_col` fall in?  Cell `i` spans
    // (rendered_pipes[i] + 1) .. rendered_pipes[i + 1] (content area).
    let cell_idx = (0..col_count)
        .find(|&i| rendered_col < rendered_pipes[i + 1])
        .unwrap_or(col_count - 1);
    let rend_cell_start = rendered_pipes[cell_idx] + 1;
    let rend_cell_end = rendered_pipes[cell_idx + 1];
    let raw_cell_start = raw_pipes[cell_idx] + 1;
    let raw_cell_end = raw_pipes[cell_idx + 1];

    let raw_cell_text: String = raw_line
        .chars()
        .skip(raw_cell_start)
        .take(raw_cell_end - raw_cell_start)
        .collect();

    // Clamp the click into the rendered cell's span so clicks on the opening
    // pipe land at the start of the cell's content.
    let _ = rend_cell_end;
    let clicked = rendered_col.max(rend_cell_start);
    let rend_offset_in_cell = clicked.saturating_sub(rend_cell_start);

    // Partition the raw cell into leading-ws / content / trailing-ws.
    let raw_chars: Vec<char> = raw_cell_text.chars().collect();
    let raw_leading = raw_chars.iter().take_while(|c| c.is_whitespace()).count();
    let raw_trailing = raw_chars
        .iter()
        .rev()
        .take_while(|c| c.is_whitespace())
        .count();
    let content_chars = raw_chars.len().saturating_sub(raw_leading + raw_trailing);

    // The renderer always emits exactly one leading space before the cell
    // content (see `render_table_row`).  A click on that leading space should
    // land on the first raw content char; clicks past the content's last
    // non-whitespace char clamp to "just past last content char" so the
    // cursor never jumps into the next cell via trailing padding.
    let raw_offset_in_cell = if rend_offset_in_cell <= 1 {
        raw_leading
    } else {
        let content_col = rend_offset_in_cell - 1;
        raw_leading + content_col.min(content_chars)
    };

    Some(raw_cell_start + raw_offset_in_cell)
}

/// Width in cells of visual sub-row `sub_row` of the rendered line.  Clicks
/// past the line's content are clamped to this bound before being mapped into
/// the raw source, so the user can click "past the end" and still land at the
/// line's last valid cursor position.
///
/// Currently returns the full character count of the line regardless of
/// sub-row — a conservative upper bound that keeps clicks off the next line.
/// A precise per-row bound would require re-running the line-wrap algorithm
/// here; the conservative bound is correct at the character level and only
/// loses precision for clicks deep in the padding of wrapped lines.
fn line_row_width(line: &ratatui::text::Line<'_>, _sub_row: usize) -> usize {
    line.spans.iter().map(|s| s.content.chars().count()).sum()
}

// ── Selection helpers ────────────────────────────────────────────────────────

/// If the raw bytes immediately before `sel.start` and immediately after
/// `sel.end` form a matching pair of inline formatting markers (`*…*`,
/// `**…**`, `_…_`, `__…__`, `` `…` ``, `~~…~~`), expand the selection to
/// include both markers so the highlight matches what the user sees when
/// the element de-renders after the click-and-drag completes.
///
/// Only expands when the selection is entirely on a single source line —
/// inline formatting doesn't span newlines in CommonMark.
fn expand_selection_to_inline_markers(
    buffer: &crate::document::Buffer,
    sel: Selection,
) -> Selection {
    let (start_char, end_char) = sel.range();
    if end_char <= start_char {
        return sel;
    }
    let rope = buffer.rope();
    let start_byte = rope.char_to_byte(start_char);
    let end_byte = rope.char_to_byte(end_char);
    let source = buffer.contents();
    if end_byte > source.len() {
        return sel;
    }

    // Same-line constraint.
    if source[start_byte..end_byte].contains('\n') {
        return sel;
    }

    // Try double-char markers first so `**foo**` doesn't get reduced to `*foo*`.
    const DOUBLE_MARKERS: &[&str] = &["**", "__", "~~"];
    const SINGLE_MARKERS: &[&str] = &["*", "_", "`"];

    for m in DOUBLE_MARKERS.iter().chain(SINGLE_MARKERS.iter()) {
        let len = m.len();
        if start_byte < len || end_byte + len > source.len() {
            continue;
        }
        let before = &source[start_byte - len..start_byte];
        let after = &source[end_byte..end_byte + len];
        if before == *m && after == *m {
            // Don't cross a line boundary when expanding — redundant given
            // the check above but cheap to verify.
            if before.contains('\n') || after.contains('\n') {
                continue;
            }
            let new_start_byte = start_byte - len;
            let new_end_byte = end_byte + len;
            let new_start = rope.byte_to_char(new_start_byte);
            let new_end = rope.byte_to_char(new_end_byte);
            // Preserve anchor/active direction.
            let (anchor, active) = if sel.anchor <= sel.active {
                (new_start, new_end)
            } else {
                (new_end, new_start)
            };
            return Selection { anchor, active };
        }
    }
    sel
}

/// Expand the selection to the word under the cursor (double-click).
fn select_word_at_cursor(state: &mut EditorState) {
    let buf = &state.buffer;
    let len = buf.len_chars();
    let rope = buf.rope();
    let offset = state.cursor.offset.min(len);

    if len == 0 {
        state.selection = None;
        return;
    }

    let is_word_char = |c: char| c.is_alphanumeric() || c == '_';

    // If the cursor is on whitespace, fall back to selecting the run of
    // whitespace instead of an empty selection.
    let mut start = offset;
    while start > 0 && is_word_char(rope.char(start - 1)) {
        start -= 1;
    }
    let mut end = offset;
    while end < len && is_word_char(rope.char(end)) {
        end += 1;
    }
    if start == end {
        // Not on a word — try expanding across non-alphanumeric chars (e.g.
        // punctuation).  If there's no such run either, leave unchanged.
        let mut s2 = offset;
        while s2 > 0 {
            let c = rope.char(s2 - 1);
            if c.is_whitespace() || is_word_char(c) {
                break;
            }
            s2 -= 1;
        }
        let mut e2 = offset;
        while e2 < len {
            let c = rope.char(e2);
            if c.is_whitespace() || is_word_char(c) {
                break;
            }
            e2 += 1;
        }
        if s2 != e2 {
            state.selection = Some(Selection {
                anchor: s2,
                active: e2,
            });
            state.cursor.offset = e2;
            return;
        }
        state.selection = None;
        return;
    }
    state.selection = Some(Selection {
        anchor: start,
        active: end,
    });
    state.cursor.offset = end;
    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
}

/// Expand the selection to the whole line (triple-click).
///
/// Inside a table the whole buffer line is `| cell | cell | cell |` — selecting
/// that pulls in the borders and neighbouring cells, which almost never matches
/// what the user wants.  When the cursor is in a table cell, select just the
/// trimmed content of that cell instead.
fn select_line_at_cursor(state: &mut EditorState) {
    let source = state.buffer.contents();
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    if let Some(info) = table_edit::find_table_at(&source, cursor_byte) {
        if let Some((row_idx, col_idx)) = table_edit::cursor_cell(&info, cursor_byte) {
            if let Some(row) = info.rows.get(row_idx) {
                if let Some(cell) = row.cells.get(col_idx) {
                    let raw_bytes = cell.raw.as_bytes();
                    let leading = raw_bytes
                        .iter()
                        .take_while(|b| matches!(**b, b' ' | b'\t'))
                        .count();
                    let trailing = raw_bytes
                        .iter()
                        .rev()
                        .take_while(|b| matches!(**b, b' ' | b'\t'))
                        .count();
                    let content_len = cell.raw.len().saturating_sub(leading + trailing);
                    let start_byte = row.start + cell.content_start + leading;
                    let end_byte = start_byte + content_len;
                    let rope = state.buffer.rope();
                    let anchor = rope.byte_to_char(start_byte);
                    let active = rope.byte_to_char(end_byte);
                    state.selection = Some(Selection { anchor, active });
                    state.cursor.offset = active;
                    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
                    return;
                }
            }
        }
    }

    let (line, _) = state.cursor.line_col(&state.buffer);
    let start = state.buffer.line_to_char(line);
    let end = if line + 1 < state.buffer.line_count() {
        state.buffer.line_to_char(line + 1)
    } else {
        state.buffer.len_chars()
    };
    state.selection = Some(Selection {
        anchor: start,
        active: end,
    });
    state.cursor.offset = end;
    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
}

// ── Phase 8 link-follow dispatch ────────────────────────────────────────────

/// If `(col, row)` lands on a Markdown link, set
/// `state.pending_link_follow` to the classified target and return
/// `true`.  Otherwise return `false` so the caller falls through to
/// normal cursor placement.
///
/// Walks the rendered line's UNDERLINED spans first (the AST-backed
/// path, matching what `link_view::build_snapshots` exposes), falling
/// back to a raw-source scan via `link_at_offset` so the raw-reveal
/// window of a cursor block still detects `[text](url)` clicks.
pub fn follow_link_at_click(
    state: &mut EditorState,
    col: u16,
    row: u16,
    viewport_width: usize,
) -> bool {
    // Try AST-backed path via underlined-span hit-test on the rendered line
    // directly.  Works for Preview and Rendered when the line isn't being
    // revealed as raw.  We intentionally do NOT consult an external
    // snapshot slice here — `hit_test_clickable` already shows the span
    // marker is sufficient, and this keeps `mouse_ops::apply`'s signature
    // small.
    if let Some((line, _)) = rendered_line_at_row(state, row as usize) {
        if span_at_col_has_modifier(&line, col as usize, ratatui::style::Modifier::UNDERLINED) {
            // The rendered line has a link span at this col — resolve the
            // URL by matching the N-th link in the block's AST with the
            // N-th underlined span on this line.
            if let Some(url) = link_url_for_click(state, col as usize, row as usize, viewport_width)
            {
                let base_dir = state
                    .buffer
                    .path()
                    .and_then(|p| p.parent())
                    .map(|p| p.to_owned());
                state.pending_link_follow = Some(LinkTarget::parse(&url, base_dir.as_deref()));
                return true;
            }
        }
    }

    // Raw fallback — click on the revealed raw `[text](url)` syntax of the
    // cursor block, or a raw-mode click, also triggers link-follow.
    let Some(offset) = click_to_char_offset(state, col as usize, row as usize, viewport_width)
    else {
        return false;
    };
    let source = state.buffer.contents();
    let click_byte = state.buffer.rope().char_to_byte(offset);
    if let Some(url) = link_at_offset(&source, click_byte) {
        let base_dir = state
            .buffer
            .path()
            .and_then(|p| p.parent())
            .map(|p| p.to_owned());
        state.pending_link_follow = Some(LinkTarget::parse(&url, base_dir.as_deref()));
        return true;
    }
    false
}

/// Best-effort: determine which URL was clicked by matching the
/// underlined-span index at `(col, row)` against the N-th
/// `Inline::Link` in the clicked rendered line's block.
///
/// Returns `None` when the click doesn't land on an underlined span or
/// we can't associate it with an AST link (which falls back to the raw
/// scan).
fn link_url_for_click(
    state: &EditorState,
    col: usize,
    row: usize,
    _viewport_width: usize,
) -> Option<String> {
    let (line, _sub_row) = rendered_line_at_row(state, row)?;
    // Index of the underlined run at `col` within this line.
    let mut walk = 0usize;
    let mut run_index: Option<usize> = None;
    let mut link_count = 0usize;
    let mut in_run = false;
    for span in &line.spans {
        let span_len = span.content.chars().count();
        let under = span
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::UNDERLINED);
        if under {
            if !in_run {
                // Entering a new underlined run — record its index.
                if col >= walk && col < walk + span_len {
                    run_index = Some(link_count);
                }
                link_count += 1;
                in_run = true;
            } else if col >= walk && col < walk + span_len {
                // Still in the same run — run_index already set.
                run_index.get_or_insert(link_count - 1);
            }
        } else {
            in_run = false;
        }
        walk += span_len;
    }
    let target_idx = run_index?;

    // Walk the block's AST to find the `target_idx`-th link.
    let cursor_byte = state
        .parsed
        .source_map
        .original_byte_for_rendered_line(index_for_row(state, row)?)?;
    let block_range = state
        .parsed
        .source_map
        .original_range_for_byte(cursor_byte)?;
    let source = state.buffer.contents();
    // Char-boundary defensive fallback — see `rendered_sub_line_to_offset`
    // for the rationale; the App flushes the parse before mouse dispatch
    // so the unwrap_or path is for safety, not correctness.
    let block_src = source
        .get(block_range.start..block_range.end.min(source.len()))
        .unwrap_or("");
    let blocks = crate::markdown::parse(block_src);
    let mut urls: Vec<(String, Option<String>)> = Vec::new();
    for block in &blocks {
        crate::ui::link_view::collect_links_from_block_public(block, &mut urls);
    }
    urls.into_iter().nth(target_idx).map(|(u, _)| u)
}

/// Resolve the rendered-line index that corresponds to document-area
/// row `row`, accounting for scroll.  Mirrors the inner loop of
/// `rendered_line_at_row` but returns the index rather than the line.
fn index_for_row(state: &EditorState, row: usize) -> Option<usize> {
    let lines = &state.parsed.lines;
    let mut y = 0usize;
    for (idx, line) in lines.iter().enumerate().skip(state.scroll) {
        let rows_used = line_render::visual_rows_for_line(line, usize::MAX).max(1);
        if row < y + rows_used {
            return Some(idx);
        }
        y += rows_used;
    }
    None
}

// ── Link hit-testing (Phase 8 prerequisite) ─────────────────────────────────

/// Scan the source line containing `click_byte` for Markdown link syntax
/// `[text](url)` and return the URL when the click falls inside such a span.
///
/// Kept deliberately simple: operates on the raw line (no AST re-parse), so
/// autolinks (`<url>`), reference links (`[text][id]`), and nested link
/// constructs are not detected.  Phase 8 may upgrade this to a proper
/// per-block hit-test registry once link opening is implemented.
pub fn link_at_offset(source: &str, click_byte: usize) -> Option<String> {
    let click_byte = click_byte.min(source.len());
    let line_start = source[..click_byte].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let rel_after = source[click_byte..]
        .find('\n')
        .map(|i| click_byte + i)
        .unwrap_or(source.len());
    let line = &source[line_start..rel_after];
    let col = click_byte.saturating_sub(line_start);

    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            // Find matching `]`.  Brackets are balanced to support nested
            // `[text containing [inner]]` constructs.
            let mut depth = 1usize;
            let mut j = i + 1;
            while j < bytes.len() {
                match bytes[j] {
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    b'\\' => {
                        j += 1;
                    }
                    _ => {}
                }
                j += 1;
            }
            if depth != 0 || j >= bytes.len() {
                return None;
            }
            let close_bracket = j;
            if close_bracket + 1 >= bytes.len() || bytes[close_bracket + 1] != b'(' {
                i = close_bracket + 1;
                continue;
            }
            let url_start = close_bracket + 2;
            let mut pdepth = 1usize;
            let mut k = url_start;
            while k < bytes.len() {
                match bytes[k] {
                    b'(' => pdepth += 1,
                    b')' => {
                        pdepth -= 1;
                        if pdepth == 0 {
                            break;
                        }
                    }
                    b'\\' => {
                        k += 1;
                    }
                    _ => {}
                }
                k += 1;
            }
            if pdepth != 0 || k >= bytes.len() {
                return None;
            }
            let url_end = k;
            if col >= i && col <= url_end {
                let url_bytes = &bytes[url_start..url_end];
                let url = String::from_utf8_lossy(url_bytes).trim().to_owned();
                return if url.is_empty() { None } else { Some(url) };
            }
            i = url_end + 1;
        } else {
            i += 1;
        }
    }
    None
}

/// Inverse of [`rendered_to_raw_char_map`] for a paragraph-style line:
/// given a raw char column on `raw_line`, return the rendered char
/// column it corresponds to on `rendered_line`.  Used by the jitter-
/// delay cursor overlay (`RenderedView`) so the cursor indicator lands
/// at the same visual column the click handler placed it — without
/// this, the indicator briefly draws at the raw column (e.g. col 1 of
/// the rendered "File link", on `i`) before the raw reveal switches
/// the line to its raw form (col 1 of `[File link]`, on `F`), and the
/// cursor visibly jumps.
///
/// Returns `None` when the rendered count of `rendered_line` doesn't
/// match the rendered count produced by `rendered_to_raw_char_map`
/// (headings/lists/blockquotes/highlights — caller falls back to a
/// 1:1 mapping, matching the click-handler's fallback).
pub fn paragraph_raw_col_to_rendered_col(
    raw_line: &str,
    rendered_line: &Line<'_>,
    raw_col: usize,
) -> Option<usize> {
    let actual_rendered_count: usize = rendered_line
        .spans
        .iter()
        .map(|s| s.content.chars().count())
        .sum();
    let map = rendered_to_raw_char_map(raw_line);
    if map.len().saturating_sub(1) != actual_rendered_count {
        return None;
    }
    // Map entries are non-decreasing (each rendered char's raw position
    // strictly advances).  Find the smallest rendered idx whose raw
    // position is `>= raw_col`.  When `raw_col` lands on a non-rendered
    // marker (e.g. the `[` of `[link]`) this returns the rendered idx
    // immediately after the marker — the same place the click handler
    // would have parked the cursor.
    let pos = map
        .iter()
        .position(|&r| r >= raw_col)
        .unwrap_or(map.len() - 1);
    Some(pos.min(actual_rendered_count))
}

/// Build a map from rendered character index → raw character index on a
/// single source line.
///
/// The renderer drops or transforms certain syntax characters: a link's
/// `[`, `](url)` markers leave only the bracket text on screen; a code
/// span's backticks become surrounding spaces.  As a result, the rendered
/// column the user clicked at doesn't correspond directly to the same
/// column in the raw text — clicks inside `File link` (rendered) are off
/// by one against `[File link](./plan.md)` (raw), and clicks past the
/// rendered end of the line land mid-URL instead of at the raw line's
/// end.
///
/// This map is built by re-parsing `raw_line` with `pulldown-cmark` and
/// recording the raw byte position of every rendered character emitted
/// by inline `Text`, `Code`, and `SoftBreak`/`HardBreak` events.  Marker
/// bytes (asterisks, brackets, the URL portion of a link) sit in the
/// gaps between events and are correctly skipped.
///
/// The returned vector has length `rendered_char_count + 1`: entry `i`
/// is the raw char index that produced rendered char `i`, and the final
/// entry is the raw char index just past the last rendered char (so a
/// click past the rendered end maps to the line's raw end).
///
/// Caller is responsible for falling back to a 1:1 mapping when the
/// returned length doesn't match the actual rendered char count of the
/// line (e.g. for headings/list items/blockquotes whose rendered prefix
/// glyphs aren't represented in the raw text, or for `==highlight==`
/// spans which are post-processed by our parser and not visible to
/// `pulldown-cmark`).
fn rendered_to_raw_char_map(raw_line: &str) -> Vec<usize> {
    use pulldown_cmark::{Event, Options, Parser};

    // Build a byte→char index lookup so events can report their offsets
    // in raw bytes (pulldown-cmark's native unit) and we can translate
    // those back to char indices that our caller and `line_text` work
    // in.  The trailing `byte_to_char[raw_line.len()] = total_chars`
    // entry covers the past-end sentinel.
    let mut byte_to_char = vec![0usize; raw_line.len() + 1];
    let mut char_idx = 0usize;
    for (byte_idx, _) in raw_line.char_indices() {
        byte_to_char[byte_idx] = char_idx;
        char_idx += 1;
    }
    byte_to_char[raw_line.len()] = char_idx;
    let total_chars = char_idx;

    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_SMART_PUNCTUATION;

    let mut map: Vec<usize> = Vec::new();

    for (event, range) in Parser::new_ext(raw_line, opts).into_offset_iter() {
        let lookup = |byte: usize| {
            byte_to_char
                .get(byte.min(byte_to_char.len().saturating_sub(1)))
                .copied()
                .unwrap_or(total_chars)
        };

        match event {
            Event::Text(s) => {
                let mut byte = range.start;
                for c in s.chars() {
                    map.push(lookup(byte));
                    byte += c.len_utf8();
                }
            }
            // Code spans render as `" <inner> "` — the opening and closing
            // backticks become surrounding spaces.  Map the leading space
            // to the opening backtick, the inner text 1:1, and the
            // trailing space to the closing backtick.
            Event::Code(s) => {
                map.push(lookup(range.start));
                let mut byte = range.start + 1;
                for c in s.chars() {
                    map.push(lookup(byte));
                    byte += c.len_utf8();
                }
                map.push(lookup(range.end.saturating_sub(1)));
            }
            // Soft- and hard-breaks render as a single space character.
            Event::SoftBreak | Event::HardBreak => {
                map.push(lookup(range.start));
            }
            // Inline tags (`Strong`, `Emphasis`, `Strikethrough`, `Link`)
            // are handled implicitly: their inner `Text` events walk the
            // content, while the marker bytes (`**`, `*`, `~~`, `[`,
            // `](url)`) sit in the gaps that no `Text` event covers and
            // never get pushed.
            _ => {}
        }
    }

    map.push(total_chars);
    map
}

// ── Checkbox toggling on click ──────────────────────────────────────────────

/// If `(col, row)` falls on a task-list checkbox glyph, toggle it and return
/// `true`.  Otherwise returns `false` so the caller can fall through to
/// cursor-placement behaviour.
fn toggle_checkbox_at(
    state: &mut EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> bool {
    if state.mode == Mode::Raw {
        return false;
    }
    // Locate the source line under the click by reusing the click-to-offset
    // translation, then ask the list machinery whether the click sits inside
    // a checkbox glyph.
    let Some(offset) = click_to_char_offset(state, col, row, viewport_width) else {
        return false;
    };
    let source = state.buffer.contents();
    let click_byte = state.buffer.rope().char_to_byte(offset);
    let Some(info) = list_edit::find_list_at(&source, click_byte) else {
        return false;
    };
    // Find which item this is.  `cursor_item_idx` does the work.
    let Some(item_idx) = list_edit::cursor_item_idx(&info, click_byte) else {
        return false;
    };
    let item = &info.items[item_idx];
    let Some(task_box) = item.task_box else {
        return false;
    };

    // Toggle hitbox spans the entire bullet+checkbox prefix — `• [ ]`
    // (i.e. `item.start..task_box + 3` in source bytes).  Clicks anywhere
    // on the bullet, the leading marker space, or the `[x]` glyph itself
    // toggle the checkbox; clicks on the trailing space after `]` fall
    // through to normal cursor placement so the user can put the caret
    // immediately before the task's text.
    let hit_start = item.start;
    let hit_end = task_box + 3;
    if click_byte >= hit_start && click_byte < hit_end {
        if let Some(res) = list_edit::toggle_checkbox(&info, &source, click_byte) {
            // Checkbox toggle is a 1-for-1 char replacement, so existing
            // offsets stay valid.  Apply the edit without touching cursor
            // tracking state (`update_cursor_block` would reset the reveal
            // timer, causing the current cursor block to briefly re-render
            // as "rendered" before snapping back to "raw").
            let offset_char = state.buffer.rope().byte_to_char(res.delta.offset);
            let delta = EditDelta {
                offset: offset_char,
                removed: res.delta.removed,
                inserted: res.delta.inserted,
            };
            let saved_offset = state.cursor.offset;
            let saved_preferred = state.cursor.preferred_col;
            let saved_block_idx = state.cursor_block_idx;
            let saved_line_idx = state.cursor_line_idx;
            let saved_entered_at = state.cursor_block_entered_at;
            state.apply_delta(delta);
            state.cursor.offset = saved_offset.min(state.buffer.len_chars());
            state.cursor.preferred_col = saved_preferred;
            state.cursor_block_idx = saved_block_idx;
            state.cursor_line_idx = saved_line_idx;
            state.cursor_block_entered_at = saved_entered_at;
            return true;
        }
    }
    false
}

/// Apply a byte-offset `EditDelta` to the editor (mirrors the helper in
/// `edit_ops`; duplicated here because `apply_byte_delta` there is private to
/// that module).
fn apply_byte_delta(state: &mut EditorState, byte_delta: EditDelta, cursor_byte_target: usize) {
    let offset_char = state.buffer.rope().byte_to_char(byte_delta.offset);
    let delta = EditDelta {
        offset: offset_char,
        removed: byte_delta.removed,
        inserted: byte_delta.inserted,
    };
    state.apply_delta(delta);
    let source = state.buffer.contents();
    let clamped_byte = cursor_byte_target.min(source.len());
    let char_off = state.buffer.rope().byte_to_char(clamped_byte);
    state.cursor.offset = char_off.min(state.buffer.len_chars());
    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
    state.update_cursor_block();
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

    /// Convenience for tests: plain Click with no modifiers.
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
        // Max scroll lands the last rendered line at the top.
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
        // "Hello world" — clicking col 6 lands on 'w'.
        assert_eq!(state.cursor.offset, 6);
        assert_eq!(state.selection, None);
        assert_eq!(target, Some(DragTarget::TextSelection { anchor: 6 }));
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
        // Preview clicks must NOT transition to Rendered any more — users
        // copy rendered text from Preview mode.  A zero-width visual
        // selection is seeded as the drag anchor.
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
        // Task items render as `• [ ] todo` — bullet at col 0, checkbox at
        // cols 2-4.  The whole prefix is a toggle hitbox; clicking on the
        // bullet itself toggles the checkbox.
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
        // Should land at end of "hi" (char 2) — clamped by line length.
        assert!(state.cursor.offset <= 2);
    }

    #[test]
    fn link_at_offset_detects_markdown_link() {
        let src = "See [the docs](https://example.com) for more.\n";
        // Click inside the bracket text.
        assert_eq!(
            link_at_offset(src, 8),
            Some("https://example.com".to_owned())
        );
        // Click inside the URL.
        assert_eq!(
            link_at_offset(src, 20),
            Some("https://example.com".to_owned())
        );
    }

    #[test]
    fn link_at_offset_returns_none_outside_link() {
        let src = "See [the docs](https://example.com) for more.\n";
        // Click past the closing paren.
        assert_eq!(link_at_offset(src, 40), None);
        // Click before the opening bracket.
        assert_eq!(link_at_offset(src, 1), None);
    }

    #[test]
    fn link_at_offset_handles_nested_brackets() {
        let src = "[one [nested] two](https://ex.com)\n";
        // Click inside the nested brackets still resolves to the outer URL.
        assert_eq!(link_at_offset(src, 7), Some("https://ex.com".to_owned()));
    }

    #[test]
    fn raw_mode_click_places_cursor_on_line() {
        let text = "first\nsecond\nthird\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Raw;
        let mut target: Option<DragTarget> = None;
        apply(&mut state, click_plain(2, 1), &mut target, &[], 10, 80);
        // Line 1 = "second" starting at char 6, col 2 → char 8.
        assert_eq!(state.cursor.offset, 8);
    }

    /// The forward map covers exactly one entry per rendered char plus a
    /// trailing past-end sentinel — that's the contract `rendered_sub_line_
    /// to_offset` and `paragraph_raw_col_to_rendered_col` rely on to detect
    /// "rendered count matches" and use the map instead of a 1:1 fallback.
    #[test]
    fn rendered_to_raw_map_link_has_one_entry_per_visible_char() {
        let map = rendered_to_raw_char_map("[File link](./plan.md)");
        // Rendered: "File link" = 9 chars; +1 sentinel = 10 entries.
        assert_eq!(map.len(), 10);
        // Each rendered char maps to its position inside the brackets.
        assert_eq!(&map[..9], &[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        // Sentinel points past the closing `)`.
        assert_eq!(map[9], 22);
    }

    /// Round-trip: clicking at a rendered col → raw byte (via the forward
    /// map), then asking for the rendered col of that raw byte (via the
    /// inverse) should return the original rendered col.  This is what
    /// keeps the jitter-delay cursor indicator at the same visual column
    /// the user clicked at, eliminating the "jump" when the raw reveal
    /// fires.
    #[test]
    fn paragraph_raw_col_round_trips_through_map() {
        use crate::markdown::{parse, Renderer};
        let raw = "[File link](./plan.md)";
        let blocks = parse(&format!("{raw}\n"));
        let renderer = Renderer::new(theme()).with_viewport_width(80);
        let lines = renderer.render(&blocks);
        let rendered = &lines[0];

        // Probe rendered cols 0..=9 (every visible char + the past-end
        // position).  The forward map is `rendered_to_raw_char_map`; its
        // `i`-th entry is the raw byte that `paragraph_raw_col_to_rendered_col`
        // should round-trip back to `i`.
        let forward = rendered_to_raw_char_map(raw);
        for rendered_col in 0..=9 {
            let raw_col = forward[rendered_col];
            let round_tripped = paragraph_raw_col_to_rendered_col(raw, rendered, raw_col);
            assert_eq!(
                round_tripped,
                Some(rendered_col),
                "round-trip failed at rendered col {rendered_col}: raw {raw_col} \
                 → {round_tripped:?} (expected Some({rendered_col}))",
            );
        }
    }

    /// Headings have a 2-char rendered prefix (`  `) the parser doesn't
    /// produce — the inverse helper should detect the count mismatch and
    /// fall back to `None`, letting callers use a 1:1 mapping for the
    /// indicator (the same fallback path the click handler takes).
    #[test]
    fn paragraph_raw_col_returns_none_for_headings() {
        use crate::markdown::{parse, Renderer};
        let raw = "## Heading";
        let blocks = parse(&format!("{raw}\n"));
        let renderer = Renderer::new(theme()).with_viewport_width(80);
        let lines = renderer.render(&blocks);
        // For `##`, raw and rendered widths happen to coincide (2 vs 2),
        // but the rendered prefix is `  ` and the raw is `##` — the
        // pulldown-cmark map only covers "Heading" (7 chars) while the
        // rendered line has 9 chars.  Mismatch → None.
        assert_eq!(paragraph_raw_col_to_rendered_col(raw, &lines[0], 5), None,);
    }
}
