//! Apply [`MouseAction`] values to the editor state.
//!
//! Mirrors `edit_ops` but for mouse input: click placement, drag selection,
//! word/line selection chords, wheel scrolling, and checkbox toggles.  All
//! coordinate-to-buffer-offset translation happens here; the `mouse.rs`
//! dispatcher only sees document-area-relative cells.

mod checkbox;
mod coord;
mod footnotes;
mod links;
mod selection;
mod table_drag;

pub use footnotes::footnote_at_offset;
pub use links::{hovered_link_url, link_at_offset};
pub use selection::visual_selection_to_rendered_text;

use crate::document::{Selection, VisualSelection};
use crate::editor::list_edit;
use crate::editor::table_edit;
use crate::editor::{EditorState, Mode};
use crate::input::MouseAction;
use crate::ui::table_view::{TableHit, TableLayoutSnapshot};

use self::checkbox::toggle_checkbox_at;
use self::coord::{
    click_to_char_offset, rendered_click_to_line_col, rendered_line_at_row,
    span_at_col_has_modifier,
};
use self::links::{follow_footnote_at_click, follow_link_at_click};
use self::selection::{
    expand_selection_to_inline_markers, select_line_at_cursor, select_word_at_cursor,
    word_range_around,
};
use self::table_drag::{
    commit_column_border_drag, commit_column_drag, commit_row_drag, current_widths_for_table,
    delete_table_column_at, delete_table_row_at, resize_widths,
};

/// What a mouse-down/drag interaction currently targets.
///
/// Replaced the old `Option<usize>` drag anchor with this enum so
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
    /// Scrollbar thumb drag.  `grab_offset` is the row offset from the
    /// thumb's top edge to the initial mouse-down row, so the thumb
    /// stays anchored under the pointer for the duration of the drag.
    Scrollbar { grab_offset: u16 },
}

/// Number of lines a wheel tick scrolls beyond the last document line.
///
/// Mouse scrolling is allowed to park the last line near the top of the
/// viewport so the user can comfortably read and edit the tail of a document.
/// Keyboard scrolling uses the stricter bound in [`EditorState::scroll_down`]
/// which keeps at least one line visible.
pub const MOUSE_SCROLL_OVERSHOOT: usize = 0;

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

    // Footnote definition's trailing `↩` back-link glyph — appended chrome
    // with no raw byte, so it needs the rendered-line hit-test (the leader
    // and reference markers are covered by the source scan below).
    if footnotes::back_link_glyph_at_click(state, col, row).is_some() {
        return true;
    }

    // Link: check whether the rendered span at `c` was emitted with the
    // `link_text` style.  The renderer is the only producer of UNDERLINED +
    // Cyan spans, so the presence of the UNDERLINED modifier is a reliable
    // marker that the hover landed on a link glyph.
    if span_at_col_has_modifier(&line, c, ratatui::style::Modifier::UNDERLINED) {
        return true;
    }

    // Checkbox: reuse the full click-to-offset translation, since the glyph is
    // rendered as plain `[ ]` / `[✓] ` text without a distinguishing style.
    // Hover hitbox matches the toggle hitbox in `toggle_checkbox_at` —
    // `item.start..task_box + 3` so the bullet itself shows the click cursor.
    if let Some(offset) = click_to_char_offset(state, c, r, viewport_width) {
        let source = state.buffer.contents();
        let click_byte = state.buffer.rope().char_to_byte(offset);
        // Footnote reference marker (the superscript) or a definition's
        // back-link leader.  Both are styled but not underlined, so they
        // need the same source-scan the click-follow path uses — keeping
        // hover and click consistent.
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

/// Preview-mode mouse handler.  Selection lives in
/// `state.visual_selection` (rendered-line `(line_idx, char_col)` pairs)
/// rather than `state.selection` (rope offsets) so the Copy action can
/// extract rendered text without Markdown markers — see
/// `Action::Copy` in `edit_ops.rs`.
///
/// All coordinate math and word-boundary logic is shared with the
/// Rendered/Raw path: this function only owns the per-mode storage and
/// the Preview-specific "plain click follows links" policy.
fn apply_preview_action(
    state: &mut EditorState,
    action: MouseAction,
    drag_target: &mut Option<DragTarget>,
    viewport_width: usize,
) {
    match action {
        MouseAction::Click { col, row, .. } => {
            // Preview is read-only: any click on a link (plain or Ctrl)
            // follows it; there's no cursor placement to disambiguate.
            if follow_link_at_click(state, col, row, viewport_width) {
                *drag_target = None;
                state.drag_in_progress = false;
                return;
            }
            match rendered_click_to_line_col(state, col as usize, row as usize, viewport_width) {
                Some((line_idx, char_col)) => {
                    state.visual_selection = Some(VisualSelection {
                        anchor: (line_idx, char_col),
                        active: (line_idx, char_col),
                    });
                    // The `anchor: 0` is unused by Preview's Drag arm
                    // (which extends the visual selection, not a rope
                    // selection) — it only needs to be `Some(_)` so the
                    // Drag arm below doesn't no-op.
                    *drag_target = Some(DragTarget::TextSelection { anchor: 0 });
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
                    state.visual_selection = Some(VisualSelection {
                        anchor: (line_idx, s),
                        active: (line_idx, e),
                    });
                }
            }
            *drag_target = None;
            state.drag_in_progress = false;
        }
        MouseAction::TripleClick { col, row, .. } => {
            if let Some((line_idx, _)) =
                rendered_click_to_line_col(state, col as usize, row as usize, viewport_width)
            {
                let end_col = state
                    .parsed
                    .lines
                    .get(line_idx)
                    .map(|l| l.spans.iter().map(|s| s.content.chars().count()).sum())
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
            if let Some(active) =
                rendered_click_to_line_col(state, col as usize, row as usize, viewport_width)
            {
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

/// Word range under `(line_idx, char_col)` in the Preview rendered-line
/// coordinate system.  Defers boundary detection to the shared
/// `word_range_around`; only the char-source closure is Preview-specific
/// (chars come from the rendered `Line`'s span sequence).
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
    // On whitespace — fall back to a single-char selection so the user
    // still gets visible feedback from the double-click.
    if clamped < chars.len() {
        Some((clamped, clamped + 1))
    } else {
        None
    }
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
    // Preview-mode clicks store their selection in rendered-line
    // coordinates (`state.visual_selection`) rather than rope offsets
    // (`state.selection`), so the copy path can extract the rendered text
    // verbatim — no Markdown markers.  Preview also intentionally does NOT
    // trigger `enter_edit_if_preview` on mouse input: the user may want to
    // copy without flipping into edit mode (which would expose raw markers
    // under the pointer).  Keyboard actions still flip via
    // `enter_edit_if_preview` in `edit_ops`.
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
            // Ctrl-click on a link bypasses cursor placement
            // entirely — we return early after firing the link-open
            // side effect so the cursor stays where it was.
            if modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                && follow_link_at_click(state, col, row, viewport_width)
            {
                return;
            }
            // Rendered mode: a plain click on a footnote marker or a
            // definition back-link follows it (matching Preview), without
            // needing Ctrl.  Scoped to Rendered — in Raw mode the markers
            // are literal editable text, so a plain click there places the
            // cursor (Ctrl-click still follows via the block above).
            if state.mode == Mode::Rendered
                && follow_footnote_at_click(state, col, row, viewport_width)
            {
                *drag_target = None;
                state.drag_in_progress = false;
                return;
            }
            // hit-test the click against every visible table's
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
            // Image block click: hit-test the click's rendered line
            // directly against every image block's reserved rendered-
            // line range — avoids `click_to_char_offset`'s boundary
            // ambiguity at the very end of a placeholder line (where
            // the offset can spill into the next block).  Skip
            // interception when the cursor is already inside the
            // matched image block AND the reveal has elapsed (the raw
            // source is showing; the click should land on text
            // normally).
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
                // Mermaid blocks reveal as a single unit — every line of
                // the block already shows raw source — so a click that
                // lands on another line inside the same mermaid block
                // shouldn't drop drag suppression and let the image
                // flash back in for the click-to-mouseup window.
                let new_block_idx = state
                    .parsed
                    .source_map
                    .block_for_byte(state.buffer.rope().char_to_byte(new_offset));
                let same_mermaid_block = new_block_idx == state.cursor_block_idx
                    && new_block_idx.is_some_and(|idx| state.parsed.is_mermaid_block(idx));
                let suppress_drag_flag =
                    (same_logical_line && !cursor_block_is_table) || same_mermaid_block;

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

                // detect clicks on Markdown link syntax so we can wire up URL opening
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
            // Scrollbar drags are driven by the App layer (which sees
            // the gutter Rect in absolute terminal coords); ignore the
            // dispatcher's doc-relative drag stream while one is in
            // flight.
            Some(DragTarget::Scrollbar { .. }) => {}
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

/// Return the block index of the image block whose reserved rendered
/// lines contain the rendered line under doc-relative `row` (with the
/// current scroll applied).  Returns `None` when no image block covers
/// that row, or when the buffer/parsed view has no image blocks at all.
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

/// Compute the cursor offset to use when a click lands anywhere on a
/// rendered image block.  Returns the buffer char offset at the end of
/// the block's source text — for regular images, the end of the
/// `![alt](url)` line; for diagram (`mermaid`) blocks, the end of the
/// last code line inside the fence (i.e. just before the closing
/// ```` ``` ````).  Returns `None` when the block has no resolvable
/// source range.
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
        // Strip the closing fence line.  `\n```` ``` ```` ` is the
        // newline immediately before the closing fence; the char before
        // it is the last char of the last code line.
        trimmed.rfind("\n```").unwrap_or(trimmed.len())
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

/// Set the scroll position absolutely from a scrollbar interaction.
/// Clamped to `total - visible` so the thumb's bottom-most rendered
/// position corresponds to the bottom-most reachable scroll value —
/// matches the bound `position_for_click` / `position_for_drag` use,
/// avoiding a one-frame drift between "clicked at gutter bottom" and
/// "thumb is at gutter bottom".  Distinct from [`scroll_by_mouse`]
/// which uses the looser `total - 1 + OVERSHOOT` bound for wheel
/// kinetic feel.  Does not disturb the cursor.
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
    fn link_at_offset_ignores_escaped_bracket() {
        // `\[text](url)` is escaped literal text — not a clickable link.
        let src = r"See \[the docs](https://example.com) for more.";
        // Click inside what would have been the link text.
        assert_eq!(link_at_offset(src, 9), None);
        // A doubled backslash leaves the link live (`\\` then a real `[`).
        let live = r"See \\[the docs](https://example.com) end";
        assert_eq!(
            link_at_offset(live, 9),
            Some("https://example.com".to_owned())
        );
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

    /// Clicking inside a `==highlight==` span should land on the correct
    /// raw character rather than being off-by-two because of the markers.
    #[test]
    fn click_in_highlight_places_cursor_correctly() {
        // Cursor on the first line so the highlighted line stays
        // rendered (the cursor's own line is de-rendered by reveal and
        // would map clicks against raw chars instead).
        let text = "x\nalpha ==beta== gamma\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        let mut target: Option<DragTarget> = None;
        // Rendered line 1: "alpha beta gamma"
        // Click on the 't' in "beta" — rendered col 8.
        apply(&mut state, click_plain(8, 1), &mut target, &[], 10, 80);
        // Line 1 starts at raw offset 2 (after "x\n").  Raw char 10 in
        // line 1 is 't': 0 a,1 l,2 p,3 h,4 a,5 space,6 =,7 =,8 b,9 e,10 t.
        assert_eq!(state.cursor.offset, 2 + 10);
    }

    /// Clicking inside a `**bold**` span of a list item lands on the
    /// correct raw character: the rendered `• ` bullet replaces the raw
    /// `- ` marker, and the `**` markers around `bold` have no rendered
    /// counterpart — both must be accounted for when mapping the click's
    /// rendered column back to a raw char.
    #[test]
    fn click_in_bold_inside_list_item_places_cursor_correctly() {
        // Two lines so the second (the list item) keeps its formatted
        // line rendered: cursor begins on line 0 (the spacer), so the
        // list item's rendered line is what the click sees.
        let text = "x\n- **bold** text\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        let mut target: Option<DragTarget> = None;
        // Rendered line 1: "• bold text".  Click the `o` in `bold` —
        // rendered col 3 (0:• 1:space 2:b 3:o).
        apply(&mut state, click_plain(3, 1), &mut target, &[], 10, 80);
        // Line 1 starts at raw offset 2.  Raw "- **bold** text": 0:- 1:space
        // 2:* 3:* 4:b 5:o 6:l 7:d 8:* 9:* 10:space 11:t … — `o` is raw col 5.
        assert_eq!(state.cursor.offset, 2 + 5);
    }

    /// Ordered-list variant: rendered `1. ` vs raw `1. ` happen to be
    /// the same width, but the `**` markers inside the content still
    /// must be skipped when mapping the click column.
    #[test]
    fn click_in_bold_inside_ordered_list_item_places_cursor_correctly() {
        let text = "x\n1. **bold** text\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        let mut target: Option<DragTarget> = None;
        // Rendered line 1: "1. bold text".  Click `o` — col 4 (0:1 1:.
        // 2:space 3:b 4:o).
        apply(&mut state, click_plain(4, 1), &mut target, &[], 10, 80);
        // Raw "1. **bold** text": 0:1 1:. 2:space 3:* 4:* 5:b 6:o …
        assert_eq!(state.cursor.offset, 2 + 6);
    }

    /// Blockquote variant — the rendered `▎ ` bar is a renderer-emitted
    /// prefix that has no Text-event counterpart in pulldown's parse of
    /// the raw `> ` line.
    #[test]
    fn click_in_bold_inside_blockquote_places_cursor_correctly() {
        let text = "x\n> **bold** text\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        let mut target: Option<DragTarget> = None;
        // Rendered line 1: "▎ bold text".  Click `o` — col 3.
        apply(&mut state, click_plain(3, 1), &mut target, &[], 10, 80);
        // Raw "> **bold** text": 0:> 1:space 2:* 3:* 4:b 5:o …
        assert_eq!(state.cursor.offset, 2 + 5);
    }
}
