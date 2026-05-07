//! Apply [`MouseAction`] values to the editor state.
//!
//! Mirrors `edit_ops` but for mouse input: click placement, drag selection,
//! word/line selection chords, wheel scrolling, and checkbox toggles.  All
//! coordinate-to-buffer-offset translation happens here; the `mouse.rs`
//! dispatcher only sees document-area-relative cells.

mod checkbox;
mod coord;
mod links;
mod selection;
mod table_drag;

// `rendered_sub_line_to_offset` is reachable through the parent module path
// for documentation grep; no production caller imports it directly.
pub use coord::paragraph_raw_col_to_rendered_col;
#[allow(unused_imports)]
pub use coord::rendered_sub_line_to_offset;
pub use links::{hovered_link_target, link_at_offset};
pub use selection::visual_selection_to_rendered_text;

use crate::document::Selection;
use crate::editor::list_edit;
use crate::editor::table_edit;
use crate::editor::{EditorState, Mode};
use crate::input::MouseAction;
use crate::ui::table_view::{TableHit, TableLayoutSnapshot};

use self::checkbox::toggle_checkbox_at;
use self::coord::{click_to_char_offset, rendered_line_at_row, span_at_col_has_modifier};
use self::links::follow_link_at_click;
use self::selection::{
    expand_selection_to_inline_markers, select_line_at_cursor, select_word_at_cursor,
};
use self::table_drag::{
    apply_preview, commit_column_border_drag, commit_column_drag, commit_row_drag,
    current_widths_for_table, delete_table_column_at, delete_table_row_at, resize_widths,
};

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
    // rendered as plain `[ ]` / `[✓] ` text without a distinguishing style.
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
        let map = coord::rendered_to_raw_char_map("[File link](./plan.md)");
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
        let forward = coord::rendered_to_raw_char_map(raw);
        for (rendered_col, &raw_col) in forward.iter().enumerate().take(10) {
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

    /// `==highlight==` markers are stripped by the renderer, so the map
    /// must skip the `==` characters to keep click alignment correct.
    #[test]
    fn rendered_to_raw_map_highlight_skips_markers() {
        let map = coord::rendered_to_raw_char_map("alpha ==beta== gamma");
        // Rendered: "alpha beta gamma" = 16 chars; +1 sentinel = 17 entries.
        assert_eq!(map.len(), 17);
        // "alpha " maps 1:1 (raw chars 0..6).
        assert_eq!(&map[..6], &[0, 1, 2, 3, 4, 5]);
        // "beta" maps to raw chars 8..12 (skipping the opening `==`).
        assert_eq!(&map[6..10], &[8, 9, 10, 11]);
        // " gamma" maps to raw chars 14..20 (skipping the closing `==`).
        assert_eq!(&map[10..16], &[14, 15, 16, 17, 18, 19]);
        // Sentinel points past the last raw char.
        assert_eq!(map[16], 20);
    }

    /// Clicking inside a `==highlight==` span should land on the correct
    /// raw character rather than being off-by-two because of the markers.
    #[test]
    fn click_in_highlight_places_cursor_correctly() {
        let text = "alpha ==beta== gamma\n";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = Mode::Rendered;
        let mut target: Option<DragTarget> = None;
        // Rendered line: "alpha beta gamma"
        // Click on the 't' in "beta" — rendered col 8.
        apply(&mut state, click_plain(8, 0), &mut target, &[], 10, 80);
        // Raw char 10 is 't' (0 a,1 l,2 p,3 h,4 a,5 space,6 =,7 =,8 b,9 e,10 t).
        assert_eq!(state.cursor.offset, 10);
    }

    /// Round-trip for highlights: every rendered col should map back to
    /// itself through `paragraph_raw_col_to_rendered_col`.
    #[test]
    fn paragraph_raw_col_round_trips_for_highlight() {
        use crate::markdown::{parse, Renderer};
        let raw = "alpha ==beta== gamma";
        let blocks = parse(&format!("{raw}\n"));
        let renderer = Renderer::new(theme()).with_viewport_width(80);
        let lines = renderer.render(&blocks);
        let rendered = &lines[0];

        let forward = coord::rendered_to_raw_char_map(raw);
        let limit = forward.len().saturating_sub(1);
        for (rendered_col, &raw_col) in forward.iter().enumerate().take(limit) {
            let round_tripped = paragraph_raw_col_to_rendered_col(raw, rendered, raw_col);
            assert_eq!(
                round_tripped,
                Some(rendered_col),
                "round-trip failed at rendered col {rendered_col}: raw {raw_col} \
                 → {round_tripped:?} (expected Some({rendered_col}))",
            );
        }
    }
}
