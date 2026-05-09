//! Integration tests for Phase 5 mouse support.
//!
//! Verifies that mouse click/drag/scroll/checkbox handling works end-to-end
//! against a real `EditorState` and the `mouse_ops::apply` entry point used
//! by the main app loop.

#![allow(clippy::single_range_in_vec_init)]

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use edamame::config::Theme;
use edamame::document::Buffer;
use edamame::editor::{mouse_ops, EditorState, Mode};
use edamame::input::{MouseAction, MouseDispatcher};
use ratatui::layout::Rect;

const VP: usize = 40;
const VW: usize = 80;

fn theme() -> &'static Theme {
    Box::leak(Box::new(Theme::default()))
}

fn state(text: &str) -> EditorState {
    EditorState::new(Buffer::from_str(text), theme())
}

fn click_event(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn ctrl_click_event(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::CONTROL,
    }
}

fn click(col: u16, row: u16) -> MouseAction {
    MouseAction::Click {
        col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn drag_event(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn up_event(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn area() -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    }
}

// ── Click placement ─────────────────────────────────────────────────────────

#[test]
fn click_on_paragraph_in_rendered_mode_places_cursor() {
    let mut st = state("Hello, world!\n");
    st.mode = Mode::Rendered;
    let mut mouse = MouseDispatcher::new();
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    let action = mouse
        .dispatch(click_event(7, 0), area())
        .expect("click dispatched");
    assert!(matches!(action, MouseAction::Click { .. }));
    mouse_ops::apply(&mut st, action, &mut anchor, &[], VP, VW);

    assert_eq!(st.mode, Mode::Rendered);
    assert_eq!(st.cursor.offset, 7); // start of "world!"
    assert_eq!(
        anchor,
        Some(mouse_ops::DragTarget::TextSelection { anchor: 7 })
    );
}

#[test]
fn click_in_preview_seeds_visual_selection_without_mode_change() {
    let mut st = state("Hello, world!\n");
    assert_eq!(st.mode, Mode::Preview);
    let mut mouse = MouseDispatcher::new();
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    if let Some(a) = mouse.dispatch(click_event(7, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    // Preview mode stays — clicks no longer force entry into edit mode.
    assert_eq!(st.mode, Mode::Preview);
    let vs = st.visual_selection.expect("click seeds visual selection");
    assert_eq!(vs.anchor, (0, 7));
    assert_eq!(vs.active, (0, 7));
}

// ── Scroll wheel ────────────────────────────────────────────────────────────

#[test]
fn wheel_scroll_does_not_move_cursor() {
    let long = (0..40).map(|i| format!("line {i}\n")).collect::<String>();
    let mut st = state(&long);
    st.mode = Mode::Rendered;
    let original_cursor = st.cursor.offset;

    // 5 wheel-down ticks, each dispatching an explicit 3-line scroll delta.
    for _ in 0..5 {
        mouse_ops::apply(&mut st, MouseAction::Scroll(3), &mut None, &[], VP, VW);
    }

    assert!(st.scroll > 0, "scroll did advance");
    assert_eq!(st.cursor.offset, original_cursor, "cursor must stay put");
}

#[test]
fn wheel_scroll_can_go_past_last_line() {
    // 10-line doc, viewport height 5: normal keyboard scroll clamps to 5 (so
    // the last line sits at the bottom).  Mouse scroll instead allows the
    // last line to sit at the TOP of the viewport (scroll = total - 1 = 9).
    let text = (0..10).map(|i| format!("l{i}\n")).collect::<String>();
    let mut st = state(&text);
    st.mode = Mode::Rendered;
    let total = st.parsed.line_count();

    for _ in 0..20 {
        mouse_ops::apply(&mut st, MouseAction::Scroll(3), &mut None, &[], 5, VW);
    }

    assert_eq!(st.scroll, total.saturating_sub(1));
}

#[test]
fn same_line_click_does_not_set_drag_in_progress() {
    // Two paragraphs: clicking around within the FIRST paragraph (where
    // the cursor starts) must not set `drag_in_progress`, so the line
    // doesn't flip from raw → rendered → raw across the click.
    let mut st = state("first paragraph here\n\nsecond paragraph here\n");
    st.mode = Mode::Rendered;
    // Seed cursor on line 0 with reveal already active.
    st.cursor.offset = 0;
    st.update_cursor_block();
    st.cursor_block_entered_at = None; // reveal returns true immediately
    assert!(st.cursor_block_revealed());

    let mut anchor: Option<mouse_ops::DragTarget> = None;
    mouse_ops::apply(&mut st, click(10, 0), &mut anchor, &[], VP, VW);

    // Cursor moved within line 0 — drag flag must stay clear so the
    // raw view persists across the click.
    assert_eq!(st.cursor_line_idx, Some(0));
    assert!(
        !st.drag_in_progress,
        "same-line click must not suppress raw reveal",
    );
    assert!(st.cursor_block_revealed());
    // A drag target is still set so a subsequent drag would extend a
    // selection from this anchor.
    assert!(matches!(
        anchor,
        Some(mouse_ops::DragTarget::TextSelection { .. })
    ));
}

#[test]
fn cross_line_click_still_sets_drag_in_progress() {
    let mut st = state("first paragraph here\n\nsecond paragraph here\n");
    st.mode = Mode::Rendered;
    st.cursor.offset = 0;
    st.update_cursor_block();
    st.cursor_block_entered_at = None;

    let mut anchor: Option<mouse_ops::DragTarget> = None;
    // Click on the second paragraph (rendered row 2 — paragraphs are
    // separated by a blank line).
    mouse_ops::apply(&mut st, click(3, 2), &mut anchor, &[], VP, VW);
    assert_ne!(st.cursor_line_idx, Some(0));
    assert!(
        st.drag_in_progress,
        "cross-line click keeps drag-suppression so the new line shows rendered briefly",
    );
}

#[test]
fn same_line_click_inside_table_still_sets_drag_in_progress() {
    // Tables have cell-based reveal — a click on a different cell of
    // the same row legitimately needs the suppression to swap which
    // cell renders raw.  Make sure the same-line guard exempts tables.
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    // Place cursor in the header row's first cell (offset right after
    // "| ").
    st.cursor.offset = 2;
    st.update_cursor_block();
    st.cursor_block_entered_at = None;
    let cursor_line = st.cursor_line_idx;

    let mut anchor: Option<mouse_ops::DragTarget> = None;
    // Click further along the same header row — col 7 sits inside the
    // second cell of the rendered table.
    mouse_ops::apply(&mut st, click(7, 1), &mut anchor, &[], VP, VW);
    assert_eq!(st.cursor_line_idx, cursor_line);
    assert!(
        st.drag_in_progress,
        "table click must still suppress reveal so the active cell can swap",
    );
}

// ── Click-drag selection ────────────────────────────────────────────────────

#[test]
fn click_drag_extends_selection_anchor_to_release_position() {
    let mut st = state("the quick brown fox\n");
    st.mode = Mode::Rendered;
    let mut mouse = MouseDispatcher::new();
    let mut anchor: Option<mouse_ops::DragTarget> = None;

    // Click at col 0, drag to col 10.
    if let Some(a) = mouse.dispatch(click_event(0, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    if let Some(a) = mouse.dispatch(drag_event(10, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    if let Some(a) = mouse.dispatch(up_event(10, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }

    let sel = st.selection.expect("drag creates selection");
    assert_eq!(sel.anchor, 0);
    assert_eq!(sel.active, 10);
    let (start, end) = sel.range();
    let selected = st.buffer.slice_to_string(start, end);
    assert_eq!(selected, "the quick ");
}

// ── Double/triple click ─────────────────────────────────────────────────────

#[test]
fn double_click_selects_word_under_pointer() {
    let mut st = state("alpha bravo charlie\n");
    st.mode = Mode::Rendered;
    let mut mouse = MouseDispatcher::new();
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    // Click on 'r' in "bravo" (col 7).
    if let Some(a) = mouse.dispatch(click_event(7, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    if let Some(a) = mouse.dispatch(up_event(7, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    if let Some(a) = mouse.dispatch(click_event(7, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }

    let sel = st.selection.expect("double-click selects");
    let (s, e) = sel.range();
    assert_eq!(st.buffer.slice_to_string(s, e), "bravo");
}

#[test]
fn triple_click_selects_line() {
    let mut st = state("first line\nsecond line\nthird line\n");
    st.mode = Mode::Rendered;
    let mut mouse = MouseDispatcher::new();
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    // Click col 3, row 1 (inside "second line").
    for _ in 0..3 {
        if let Some(a) = mouse.dispatch(click_event(3, 1), area()) {
            mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
        }
        if let Some(a) = mouse.dispatch(up_event(3, 1), area()) {
            mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
        }
    }
    let sel = st.selection.expect("triple-click selects line");
    let (s, e) = sel.range();
    let text = st.buffer.slice_to_string(s, e);
    assert!(
        text.contains("second"),
        "selected text should contain the clicked line, got: {text:?}"
    );
}

// ── Checkbox toggle ─────────────────────────────────────────────────────────

#[test]
fn click_on_checkbox_in_task_list_toggles_it() {
    let mut st = state("- [ ] todo one\n- [x] todo two\n");
    st.mode = Mode::Rendered;
    let mut anchor: Option<mouse_ops::DragTarget> = None;

    // Click on the middle of the rendered `[ ]` glyph.  Task-list items
    // render without their raw bullet prefix, so `[` sits at rendered col 0.
    mouse_ops::apply(&mut st, click(1, 0), &mut anchor, &[], VP, VW);
    assert!(st.contents().starts_with("- [x] todo one"));

    // Click on the `[x]` of row 1 to uncheck.
    mouse_ops::apply(&mut st, click(1, 1), &mut anchor, &[], VP, VW);
    assert!(st.contents().contains("- [ ] todo two"));
}

#[test]
fn click_on_checkbox_on_another_line_toggles_without_moving_cursor() {
    let mut st = state("- [ ] todo one\n- [ ] todo two\n");
    st.mode = Mode::Rendered;
    st.cursor.offset = 0; // on the first line

    let mut anchor: Option<mouse_ops::DragTarget> = None;
    // Click the `[` of the second row at rendered col 0, row 1.  Cursor is on
    // row 0, so before the fix the click would land on the second line (moving
    // the cursor and de-rendering the block), without toggling.
    mouse_ops::apply(&mut st, click(0, 1), &mut anchor, &[], VP, VW);
    assert!(
        st.contents().contains("- [x] todo two"),
        "expected second checkbox to flip, got: {:?}",
        st.contents()
    );
    // Cursor must not have jumped to the second line's click position.
    assert!(
        st.cursor.offset <= 15, // byte 15 is the first char of line 1
        "cursor moved into the clicked line: offset {}",
        st.cursor.offset
    );
}

#[test]
fn triple_click_in_table_cell_selects_only_cell_content() {
    // Table data row: `| alpha | bravo | charlie |`.  Triple-clicking inside
    // "bravo" should select "bravo", NOT the whole row (which would pull in
    // the pipe borders and neighbouring cells).  Column widths in this table
    // are chosen so the raw and rendered column positions line up, keeping
    // click-to-offset mapping straightforward to reason about.
    let mut st = state("| a | b | c |\n|---|---|---|\n| alpha | bravo | charlie |\n");
    st.mode = Mode::Rendered;

    // Rendered col 11 on rendered row 3 is the 'r' of "bravo" (row 3 is the
    // data row: border=0, header=1, separator=2, data=3).
    let mut mouse = MouseDispatcher::new();
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    for _ in 0..3 {
        if let Some(a) = mouse.dispatch(click_event(11, 3), area()) {
            mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
        }
        if let Some(a) = mouse.dispatch(up_event(11, 3), area()) {
            mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
        }
    }
    let sel = st.selection.expect("triple-click sets selection");
    let (s, e) = sel.range();
    assert_eq!(
        st.buffer.slice_to_string(s, e),
        "bravo",
        "triple-click in table cell should select only the trimmed cell content"
    );
}

#[test]
fn click_on_table_cell_stays_in_that_cell_despite_padding() {
    // Column widths are driven by the longest cell; cell 1 in the header
    // ("short") is wider than its data-row counterpart ("b"), forcing the
    // renderer to pad the data-row cell with several spaces.  A click on that
    // padded area used to fall into the next cell because the raw line is
    // narrower than the rendered line.
    let mut st = state("| longheader | short | c |\n|---|---|---|\n| a | b | c |\n");
    st.mode = Mode::Rendered;

    let mut anchor: Option<mouse_ops::DragTarget> = None;
    // Row 3 of the rendered block is the data row: [0]=top border, [1]=header,
    // [2]=separator, [3]=data.  Click deep into cell 1's trailing padding.
    mouse_ops::apply(&mut st, click(18, 3), &mut anchor, &[], VP, VW);
    // The cursor should land inside cell 1 ("b"), not past the next pipe.
    // In the raw source `| a | b | c |`, cell 1 is bytes 5..9 (` b `). The
    // post-fix fix clamps clicks past `b` to the position immediately after it.
    let raw_start_of_data_row = "| longheader | short | c |\n|---|---|---|\n".len();
    let cursor_byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    let offset_in_row = cursor_byte - raw_start_of_data_row;
    assert!(
        (5..=9).contains(&offset_in_row),
        "cursor offset in row = {} (expected inside cell 1 byte range 5..=9)",
        offset_in_row
    );
}

#[test]
fn click_on_thick_header_separator_redirects_to_first_data_row() {
    // Rendered layout: [0]=top, [1]=header, [2]=thick sep, [3]=data1,
    // [4]=thin sep, [5]=data2, [6]=bottom.  Clicking the thick separator
    // (the alignment-row line) must NOT leave the cursor parked on the
    // structural `|---|` raw line — redirect to the first data row above.
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    mouse_ops::apply(&mut st, click(2, 2), &mut anchor, &[], VP, VW);
    let cursor_byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    let first_data_start = "| a | b |\n|---|---|\n".len();
    let first_data_end = first_data_start + "| 1 | 2 |".len();
    assert!(
        (first_data_start..=first_data_end).contains(&cursor_byte),
        "cursor landed at byte {cursor_byte}; expected inside first data row \
         [{first_data_start}..={first_data_end}]"
    );
}

#[test]
fn click_on_second_data_row_lands_on_second_data_row() {
    // Regression for the off-by-one: before the fix, `raw = sub - 1` mapped
    // the second data row (sub 5) to raw line 4, which — in a 2-data-row
    // table — lies past the block and the cursor overshot into the next
    // block or clamped to end.
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    mouse_ops::apply(&mut st, click(2, 5), &mut anchor, &[], VP, VW);
    let cursor_byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    let second_data_start = "| a | b |\n|---|---|\n| 1 | 2 |\n".len();
    let second_data_end = second_data_start + "| 3 | 4 |".len();
    assert!(
        (second_data_start..=second_data_end).contains(&cursor_byte),
        "cursor landed at byte {cursor_byte}; expected inside second data row \
         [{second_data_start}..={second_data_end}]"
    );
}

#[test]
fn click_on_thin_inter_row_separator_snaps_to_preceding_data_row() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    // sub 4 is the thin separator between the two data rows.
    mouse_ops::apply(&mut st, click(2, 4), &mut anchor, &[], VP, VW);
    let cursor_byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    let first_data_start = "| a | b |\n|---|---|\n".len();
    let first_data_end = first_data_start + "| 1 | 2 |".len();
    assert!(
        (first_data_start..=first_data_end).contains(&cursor_byte),
        "cursor landed at byte {cursor_byte}; expected inside first data row"
    );
}

// ── Selection expansion on release ─────────────────────────────────────────

#[test]
fn drag_release_expands_selection_to_enclosing_markers() {
    use edamame::document::Selection;
    let mut st = state("alpha *cat* beta\n");
    st.mode = Mode::Rendered;
    // Seed a selection equal to the rendered "cat" content — raw bytes 7..10
    // ("cat" in `*cat*`).  This is what a hybrid-mode drag would produce if
    // click-to-offset ignored the surrounding `*` markers.
    st.selection = Some(Selection {
        anchor: 7,
        active: 10,
    });
    // Simulate the mouse release that follows the drag.  `drag_anchor` is
    // Some(...) during drag; Release doesn't touch it otherwise.
    let mut anchor: Option<mouse_ops::DragTarget> =
        Some(mouse_ops::DragTarget::TextSelection { anchor: 0 });
    mouse_ops::apply(&mut st, MouseAction::Release, &mut anchor, &[], VP, VW);

    // Expected: selection expands to include the `*…*` markers → raw `*cat*`.
    let sel = st.selection.expect("selection still present");
    let (s, e) = sel.range();
    assert_eq!(
        st.buffer.slice_to_string(s, e),
        "*cat*",
        "selection should include the `*` markers after release"
    );
}

#[test]
fn drag_release_expands_double_markers_to_strong() {
    use edamame::document::Selection;
    let mut st = state("plain **bold** text\n");
    st.mode = Mode::Rendered;
    // Select just "bold" — raw bytes 8..12.
    st.selection = Some(Selection {
        anchor: 8,
        active: 12,
    });
    let mut anchor: Option<mouse_ops::DragTarget> =
        Some(mouse_ops::DragTarget::TextSelection { anchor: 0 });
    mouse_ops::apply(&mut st, MouseAction::Release, &mut anchor, &[], VP, VW);
    let sel = st.selection.unwrap();
    let (s, e) = sel.range();
    assert_eq!(st.buffer.slice_to_string(s, e), "**bold**");
}

// ── Preview visual-selection copy ───────────────────────────────────────────

#[test]
fn preview_click_drag_produces_rendered_text_on_copy() {
    use edamame::editor::mouse_ops::visual_selection_to_rendered_text;
    // Paragraph with inline emphasis — rendered text strips the `*` markers.
    let mut st = state("alpha *emph* beta\n");
    assert_eq!(st.mode, Mode::Preview);
    let mut mouse = MouseDispatcher::new();
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    if let Some(a) = mouse.dispatch(click_event(6, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    if let Some(a) = mouse.dispatch(drag_event(10, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    if let Some(a) = mouse.dispatch(up_event(10, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    assert_eq!(st.mode, Mode::Preview);
    let vs = st.visual_selection.expect("drag creates visual selection");
    let text = visual_selection_to_rendered_text(vs, &st.parsed.lines);
    // Rendered columns 6..10 on the first paragraph = "emph" (the four
    // characters of `*emph*` that the renderer displays without asterisks).
    assert_eq!(text, "emph");
}

// ── Hit-test for pointer-shape feedback ─────────────────────────────────────

#[test]
fn hit_test_returns_true_over_task_bullet_and_checkbox() {
    let st = state("- [ ] first\n- [x] second\n");
    // Task items render as `• [ ] first` — bullet at col 0, checkbox at
    // cols 2-4.  The whole bullet+checkbox prefix is a toggle hitbox so
    // clicks anywhere in cols 0..=4 hit; col 5 (the trailing space after
    // `]`) and beyond fall through to normal cursor placement.
    for (row, col) in [
        (0u16, 0u16),
        (0, 1),
        (0, 2),
        (0, 3),
        (0, 4),
        (1, 0),
        (1, 1),
        (1, 2),
        (1, 3),
        (1, 4),
    ] {
        assert!(
            mouse_ops::hit_test_clickable(&st, col, row, VW, &[]),
            "expected hit-test TRUE at ({col}, {row})"
        );
    }
    // Col 5 is the trailing space AFTER `]`.
    assert!(!mouse_ops::hit_test_clickable(&st, 5, 0, VW, &[]));
    // Col 7 is in the body text.
    assert!(!mouse_ops::hit_test_clickable(&st, 7, 0, VW, &[]));
}

#[test]
fn hit_test_returns_true_over_markdown_link() {
    let st = state("See [docs](https://x.com) for info.\n");
    // `[docs](url)` renders as just `docs` — 4 chars at rendered cols 4..=7.
    // The URL is invisible in rendered mode, so only the link text is a
    // visible "clickable region".
    assert!(mouse_ops::hit_test_clickable(&st, 5, 0, VW, &[])); // inside "docs"
    assert!(mouse_ops::hit_test_clickable(&st, 7, 0, VW, &[])); // last char of "docs"
                                                                // Click outside the link text.
    assert!(!mouse_ops::hit_test_clickable(&st, 2, 0, VW, &[]));
    assert!(!mouse_ops::hit_test_clickable(&st, 10, 0, VW, &[]));
}

/// Pointer-shape feedback: hovering over any of the four table buttons
/// (row reorder `⠿`, column reorder `⠿`, row delete `✕`, column delete
/// `✕`) returns true so the App switches to the hand pointer.  Resize
/// borders intentionally don't return true — they're a drag, not a
/// discrete click target.
#[test]
fn hit_test_returns_true_over_each_table_button() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let st = state(src);

    // Layout: col 0 at x=2..5, col 1 at x=6..9.  Top border y=0,
    // header y=1, thick separator y=2, data row y=3, bottom border y=4.
    let mut snap =
        snapshot_with_delete_handles(src, vec![2..5, 6..9], vec![3..4], Some(9), Some(4));
    snap.row_handle_col = Some(0);
    snap.top_border_row = Some(0);
    let snapshots = [snap];

    // Row-reorder `⠿` at (x=0, y=3).
    assert!(
        mouse_ops::hit_test_clickable(&st, 0, 3, VW, &snapshots),
        "row-reorder handle should classify as clickable",
    );
    // Column-reorder `⠿` on top border (y=0) inside col 0's x-range.
    assert!(
        mouse_ops::hit_test_clickable(&st, 3, 0, VW, &snapshots),
        "column-reorder handle should classify as clickable",
    );
    // Row-delete `✕` at right border (x=9, y=3) on data row.
    assert!(
        mouse_ops::hit_test_clickable(&st, 9, 3, VW, &snapshots),
        "row-delete glyph should classify as clickable",
    );
    // Column-delete `✕` on bottom border (y=4) inside col 1's x-range.
    assert!(
        mouse_ops::hit_test_clickable(&st, 7, 4, VW, &snapshots),
        "column-delete glyph should classify as clickable",
    );

    // Interior resize border (between columns at x=5, on a data row)
    // — `⇔` glyph plus the `±1` tolerance window — must flip the
    // cursor to hand so the resize affordance is discoverable.
    assert!(
        mouse_ops::hit_test_clickable(&st, 5, 3, VW, &snapshots),
        "interior resize border should classify as clickable",
    );
    // Leftmost outer border at x=1 is inert (no column to its left
    // to resize), so it must NOT flip the cursor.
    assert!(
        !mouse_ops::hit_test_clickable(&st, 1, 3, VW, &snapshots),
        "leftmost outer border should NOT classify as clickable",
    );
}

// ── Drag outside doc area is benign ─────────────────────────────────────────

#[test]
fn rapid_clicks_and_drags_do_not_crash() {
    let mut st = state("short doc\n");
    let mut mouse = MouseDispatcher::new();
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    for row in 0..5u16 {
        for col in 0..20u16 {
            if let Some(a) = mouse.dispatch(click_event(col, row), area()) {
                mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
            }
            if let Some(a) = mouse.dispatch(drag_event(col + 1, row), area()) {
                mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
            }
            if let Some(a) = mouse.dispatch(up_event(col + 1, row), area()) {
                mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
            }
        }
    }
}

// ── Link hit-test ───────────────────────────────────────────────────────────

#[test]
fn link_at_offset_detection_works_via_public_api() {
    let src = "See [the docs](https://example.com) for more.\n";
    assert_eq!(
        mouse_ops::link_at_offset(src, 8),
        Some("https://example.com".to_owned())
    );
    assert_eq!(mouse_ops::link_at_offset(src, 1), None);
}

// ── Phase 6: table drag flows ───────────────────────────────────────────────

use edamame::ui::table_view::TableLayoutSnapshot;

/// Build a snapshot for a table whose first row begins at `table_byte_start`,
/// sized to simulate what the Phase 6 `build_snapshots` function would have
/// produced.  We fabricate the snapshot directly rather than driving it
/// through a full render, because the headless `TestBackend` renderer isn't
/// wired up in these mouse tests and the drag-flow logic is exercised
/// entirely through the snapshot.
struct FakeSnapshotSpec {
    table_byte_start: usize,
    table_byte_end: usize,
    col_count: usize,
    row_count: usize,
    col_ranges: Vec<std::ops::Range<u16>>,
    row_ranges: Vec<std::ops::Range<u16>>,
    row_handle_col: Option<u16>,
    top_border_row: Option<u16>,
}

fn fake_snapshot(spec: FakeSnapshotSpec) -> TableLayoutSnapshot {
    TableLayoutSnapshot {
        table_byte_start: spec.table_byte_start,
        table_byte_end: spec.table_byte_end,
        col_count: spec.col_count,
        row_count: spec.row_count,
        col_ranges: spec.col_ranges,
        row_ranges: spec.row_ranges,
        row_handle_col: spec.row_handle_col,
        top_border_row: spec.top_border_row,
        header_row: None,
        delete_row_handle_col: None,
        bottom_border_row: None,
    }
}

#[test]
fn row_handle_drag_swaps_rows_in_buffer() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    // Fabricate a snapshot whose row handle sits at x=0 and whose two data
    // rows occupy y=3 and y=5.  The column content spans x=2..5 and x=6..9.
    let snap = fake_snapshot(FakeSnapshotSpec {
        table_byte_start: 0,
        table_byte_end: src.len(),
        col_count: 2,
        row_count: 4,
        col_ranges: vec![2..5, 6..9],
        row_ranges: vec![3..4, 5..6], // data rows only
        row_handle_col: Some(0),
        top_border_row: Some(0),
    });
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    // Click on the first row's handle (x=0, y=3 — row_idx 2).
    mouse_ops::apply(&mut st, click(0, 3), &mut target, &snapshots, VP, VW);
    assert!(matches!(
        target,
        Some(mouse_ops::DragTarget::TableRow { row_idx: 2, .. })
    ));

    // Drag down to the second row (y=5 — row_idx 3).
    mouse_ops::apply(
        &mut st,
        MouseAction::Drag { col: 0, row: 5 },
        &mut target,
        &snapshots,
        VP,
        VW,
    );

    // Release — should commit a row swap.
    mouse_ops::apply(
        &mut st,
        MouseAction::Release,
        &mut target,
        &snapshots,
        VP,
        VW,
    );

    let after = st.contents();
    let row_order: Vec<&str> = after.lines().skip(2).collect();
    // After dragging row 2 (| 1 | 2 |) onto row 3 (| 3 | 4 |), row order is
    // `| 3 | 4 |` then `| 1 | 2 |`.
    assert_eq!(row_order[0], "| 3 | 4 |");
    assert_eq!(row_order[1], "| 1 | 2 |");
}

#[test]
fn column_border_drag_writes_tui_columns_comment() {
    // Use wider data so the initial natural widths are well above MIN_COL_WIDTH;
    // otherwise the clamp in `resize_widths` yields no movement and nothing
    // gets persisted.
    let src = "| headerA | headerB |\n|-----|-----|\n| foo   | bar   |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    // Column content: col 0 at x=2..12, col 1 at x=13..23.  The interior
    // border sits at x=12 (col_ranges[0].end).
    let snap = fake_snapshot(FakeSnapshotSpec {
        table_byte_start: 0,
        table_byte_end: src.len(),
        col_count: 2,
        row_count: 3,
        col_ranges: vec![2..12, 13..23],
        row_ranges: vec![3..4],
        row_handle_col: None,
        top_border_row: None,
    });
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    // Click on the interior border at x=12.
    mouse_ops::apply(&mut st, click(12, 3), &mut target, &snapshots, VP, VW);
    assert!(matches!(
        target,
        Some(mouse_ops::DragTarget::TableColumnBorder { col_idx: 1, .. })
    ));

    // Drag right by 2 cells.
    mouse_ops::apply(
        &mut st,
        MouseAction::Drag { col: 14, row: 3 },
        &mut target,
        &snapshots,
        VP,
        VW,
    );

    // Release.
    mouse_ops::apply(
        &mut st,
        MouseAction::Release,
        &mut target,
        &snapshots,
        VP,
        VW,
    );

    // Phase 13: Release no longer auto-commits — it stages a pending
    // commit that the App resolves (via the warning modal or directly).
    // Tests bypass the App by calling the commit method themselves.
    assert!(st.has_pending_column_widths());
    st.commit_pending_column_widths();
    let after = st.contents();
    assert!(
        after.contains("<!-- tui-columns:"),
        "expected tui-columns comment after release, got: {after:?}"
    );
}

/// Dragging a column border wider must grow the table (pinning ONLY the
/// resized column) rather than zero-sum shrinking the neighbour.  The
/// persisted comment must therefore use `_` for the still-auto column.
#[test]
fn column_border_drag_widens_table_and_leaves_neighbour_auto() {
    // Natural widths: col 0 = 3 ("abc"), col 1 = 6 ("defghi").  Table total
    // (pre-resize) is 3 + 6 = 9 content cells.  Dragging the interior border
    // right by 2 should pin col 0 to 5 while col 1 stays auto — not shrink
    // col 1 to 4.
    let src = "| abc | defghi |\n| --- | --- |\n| bar | baz |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    // Rendered layout: col 0 content area spans x = 1..6 (pipe + ' abc ' +
    // pipe), col 1 spans x = 7..15.  The interior border sits at x = 6.
    let snap = fake_snapshot(FakeSnapshotSpec {
        table_byte_start: 0,
        table_byte_end: src.len(),
        col_count: 2,
        row_count: 3,
        col_ranges: vec![1..6, 7..15],
        row_ranges: vec![3..4],
        row_handle_col: None,
        top_border_row: None,
    });
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    mouse_ops::apply(&mut st, click(6, 3), &mut target, &snapshots, VP, VW);
    mouse_ops::apply(
        &mut st,
        MouseAction::Drag { col: 8, row: 3 },
        &mut target,
        &snapshots,
        VP,
        VW,
    );
    mouse_ops::apply(
        &mut st,
        MouseAction::Release,
        &mut target,
        &snapshots,
        VP,
        VW,
    );

    st.commit_pending_column_widths();
    let after = st.contents();
    assert!(
        after.contains("<!-- tui-columns: [5, _] -->"),
        "expected col 0 pinned to 5 and col 1 auto, got: {after:?}"
    );
}

/// The right outer border is a resize target too — dragging it widens the
/// last column (not the first), pinning it in the persisted comment.
#[test]
fn right_outer_border_drag_resizes_last_column() {
    let src = "| abc | defghi |\n| --- | --- |\n| bar | baz |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    // Rendered layout (natural widths): col 0 at x = 1..6, col 1 at x = 7..15.
    // The right outer border sits at x = 15.
    let snap = fake_snapshot(FakeSnapshotSpec {
        table_byte_start: 0,
        table_byte_end: src.len(),
        col_count: 2,
        row_count: 3,
        col_ranges: vec![1..6, 7..15],
        row_ranges: vec![3..4],
        row_handle_col: None,
        top_border_row: None,
    });
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    mouse_ops::apply(&mut st, click(15, 3), &mut target, &snapshots, VP, VW);
    assert!(
        matches!(
            target,
            Some(mouse_ops::DragTarget::TableColumnBorder { col_idx: 2, .. })
        ),
        "expected ColumnBorder {{ col_idx: 2 }} (right outer), got: {target:?}"
    );

    mouse_ops::apply(
        &mut st,
        MouseAction::Drag { col: 18, row: 3 },
        &mut target,
        &snapshots,
        VP,
        VW,
    );
    mouse_ops::apply(
        &mut st,
        MouseAction::Release,
        &mut target,
        &snapshots,
        VP,
        VW,
    );

    st.commit_pending_column_widths();
    let after = st.contents();
    assert!(
        after.contains("<!-- tui-columns: [_, 9] -->"),
        "expected col 0 auto and col 1 pinned to 9, got: {after:?}"
    );
}

#[test]
fn column_handle_drag_swaps_columns_in_buffer() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    // top_border_row at y=0, cols at x=2..5 and x=6..9.
    let snap = fake_snapshot(FakeSnapshotSpec {
        table_byte_start: 0,
        table_byte_end: src.len(),
        col_count: 2,
        row_count: 3,
        col_ranges: vec![2..5, 6..9],
        row_ranges: vec![3..4],
        row_handle_col: None,
        top_border_row: Some(0),
    });
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    // Click on column 0's handle (y=0, any x within col 0's range).
    mouse_ops::apply(&mut st, click(3, 0), &mut target, &snapshots, VP, VW);
    assert!(matches!(
        target,
        Some(mouse_ops::DragTarget::TableColumnHeader { col_idx: 0, .. })
    ));

    // Drag to column 1's handle.
    mouse_ops::apply(
        &mut st,
        MouseAction::Drag { col: 7, row: 0 },
        &mut target,
        &snapshots,
        VP,
        VW,
    );

    // Release — should commit a column swap.
    mouse_ops::apply(
        &mut st,
        MouseAction::Release,
        &mut target,
        &snapshots,
        VP,
        VW,
    );

    let after = st.contents();
    // Header row should now read `| b | a |` after the swap.
    assert!(
        after.starts_with("| b | a |"),
        "expected swapped header, got: {after:?}"
    );
}

/// Regression: after a column reorder, the cursor must NOT land on the
/// trailing `<!-- tui-columns: ... -->` comment line.  If it did, the
/// raw-reveal in `RenderedView` would overlay the comment's text onto
/// the last data row until the next cursor movement.
#[test]
fn column_reorder_leaves_cursor_off_persisted_comment_line() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n<!-- tui-columns: [4, 5] -->\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    let snap = fake_snapshot(FakeSnapshotSpec {
        table_byte_start: 0,
        table_byte_end: src.len(),
        col_count: 2,
        row_count: 3,
        col_ranges: vec![1..4, 5..8],
        row_ranges: vec![3..4],
        row_handle_col: None,
        top_border_row: Some(0),
    });
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    mouse_ops::apply(&mut st, click(2, 0), &mut target, &snapshots, VP, VW);
    mouse_ops::apply(
        &mut st,
        MouseAction::Drag { col: 6, row: 0 },
        &mut target,
        &snapshots,
        VP,
        VW,
    );
    mouse_ops::apply(
        &mut st,
        MouseAction::Release,
        &mut target,
        &snapshots,
        VP,
        VW,
    );

    let source = st.contents();
    let comment_start = source.find("<!--").expect("comment preserved");
    let cursor_byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    assert!(
        cursor_byte < comment_start,
        "cursor leaked onto comment line at byte {cursor_byte} (comment starts at {comment_start})",
    );
}

/// Same invariant as `column_reorder_leaves_cursor_off_persisted_comment_line`
/// but for row reorder — the bug is symmetrical.
#[test]
fn row_reorder_leaves_cursor_off_persisted_comment_line() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n<!-- tui-columns: [4, 5] -->\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    let snap = fake_snapshot(FakeSnapshotSpec {
        table_byte_start: 0,
        table_byte_end: src.len(),
        col_count: 2,
        row_count: 4,
        col_ranges: vec![2..5, 6..9],
        row_ranges: vec![3..4, 5..6],
        row_handle_col: Some(0),
        top_border_row: None,
    });
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    mouse_ops::apply(&mut st, click(0, 3), &mut target, &snapshots, VP, VW);
    mouse_ops::apply(
        &mut st,
        MouseAction::Drag { col: 0, row: 5 },
        &mut target,
        &snapshots,
        VP,
        VW,
    );
    mouse_ops::apply(
        &mut st,
        MouseAction::Release,
        &mut target,
        &snapshots,
        VP,
        VW,
    );

    let source = st.contents();
    let comment_start = source.find("<!--").expect("comment preserved");
    let cursor_byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    assert!(
        cursor_byte < comment_start,
        "cursor leaked onto comment line at byte {cursor_byte} (comment starts at {comment_start})",
    );
}

// ── Phase 8: clickable links and navigation ────────────────────────────────

use edamame::editor::link::LinkTarget;

#[test]
fn preview_click_on_link_sets_pending_follow_target() {
    let mut st = state("See [docs](https://example.com) for more.\n");
    // Preview is the default mode; click on the rendered link text.
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    let mut mouse = MouseDispatcher::new();
    // Rendered text for `[docs](https://example.com)` is just `docs`
    // (4 chars starting at rendered col 4).
    if let Some(a) = mouse.dispatch(click_event(5, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    let pending = st.pending_link_follow.take().expect("follow target set");
    assert!(
        matches!(&pending, LinkTarget::Url(u) if u == "https://example.com"),
        "expected URL target, got {pending:?}"
    );
}

#[test]
fn plain_click_in_rendered_on_link_places_cursor_does_not_follow() {
    // Per plan: in Rendered mode, plain click places the cursor — only
    // Ctrl-click follows the link.
    let mut st = state("See [docs](https://example.com) for more.\n");
    st.mode = Mode::Rendered;
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    let mut mouse = MouseDispatcher::new();
    if let Some(a) = mouse.dispatch(click_event(5, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    // No pending follow — the plain click in rendered mode fell through
    // to cursor placement.
    assert!(st.pending_link_follow.is_none());
    assert!(st.selection.is_none());
}

/// Regression: clicking inside a link's rendered text used to land
/// `bracket-prefix` chars short of the visible character, because the
/// renderer drops the `[` from `[text](url)` but the click→offset map
/// used the rendered column directly as the raw column.  After the
/// rendered→raw map fix, a click on the *o* of "docs" should land on
/// the *o* in `[docs]` — raw byte 6 of `See [docs](...)`.
#[test]
fn click_inside_rendered_link_text_lands_on_clicked_char() {
    let src = "See [docs](https://example.com) for more.\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    let mut mouse = MouseDispatcher::new();
    // Rendered: "See docs for more." — col 5 is 'o' (the second char of
    // "docs" since "See " is 4 chars + 'd' is col 4, 'o' is col 5).
    if let Some(a) = mouse.dispatch(click_event(5, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    // 'o' inside `[docs]` sits at raw byte 6 in `See [docs](https://...)`.
    assert_eq!(
        st.contents().chars().nth(st.cursor.offset),
        Some('o'),
        "expected cursor on 'o' of 'docs', landed at offset {} ({:?})",
        st.cursor.offset,
        st.contents().chars().nth(st.cursor.offset),
    );
}

/// Regression: clicking past the rendered end of a line containing a
/// link used to land mid-URL because the click→offset map clamped the
/// click to the rendered column count and then re-used that as a raw
/// column index.  The user expects clicks in the trailing whitespace
/// to land at the line's actual raw end (after `)`).
#[test]
fn click_past_rendered_end_of_link_line_lands_at_raw_end_of_line() {
    let src = "[File link](./plan.md)\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    let mut mouse = MouseDispatcher::new();
    // Rendered: "File link" — 9 chars.  Click at col 50 is well past
    // the rendered text in the trailing whitespace.
    if let Some(a) = mouse.dispatch(click_event(50, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    // Raw line `[File link](./plan.md)` is 22 chars; cursor should land
    // at char 22 (the position just after `)`, before the newline).
    assert_eq!(
        st.cursor.offset, 22,
        "expected cursor at end of raw line (22), got {}",
        st.cursor.offset,
    );
}

#[test]
fn ctrl_click_in_rendered_on_link_follows_without_moving_cursor() {
    let mut st = state("See [docs](https://example.com) for more.\n");
    st.mode = Mode::Rendered;
    let cursor_before = st.cursor.offset;
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    let mut mouse = MouseDispatcher::new();
    if let Some(a) = mouse.dispatch(ctrl_click_event(5, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    let pending = st
        .pending_link_follow
        .take()
        .expect("ctrl-click sets target");
    assert!(matches!(&pending, LinkTarget::Url(u) if u == "https://example.com"));
    // Ctrl-click should NOT move the cursor away from its prior position.
    assert_eq!(st.cursor.offset, cursor_before);
}

#[test]
fn raw_reveal_fallback_detects_link_in_bracket_syntax() {
    // Click on the raw `[text](url)` bytes (which will happen when the
    // cursor block is revealed as raw) — the fallback path uses
    // `link_at_offset` and produces the same LinkTarget classification.
    let src = "See [docs](https://example.com) for more.\n";
    let mut st = state(src);
    st.mode = Mode::Raw;
    // Raw mode: every col maps directly to a buffer byte.  Click col 5
    // (inside `docs` bracket text).
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    let mut mouse = MouseDispatcher::new();
    if let Some(a) = mouse.dispatch(ctrl_click_event(5, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    let pending = st
        .pending_link_follow
        .take()
        .expect("raw-mode click follows link");
    assert!(matches!(&pending, LinkTarget::Url(u) if u == "https://example.com"));
}

#[test]
fn click_on_non_link_text_does_not_set_pending_follow() {
    let mut st = state("A paragraph with no links.\n");
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    let mut mouse = MouseDispatcher::new();
    if let Some(a) = mouse.dispatch(click_event(4, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    assert!(st.pending_link_follow.is_none());
}

#[test]
fn hovered_link_target_returns_url_on_link_hover() {
    let st = state("See [docs](https://example.com) for more.\n");
    let target = mouse_ops::hovered_link_target(&st, 5, 0, VW);
    assert!(matches!(target, Some(LinkTarget::Url(ref u)) if u == "https://example.com"));
}

#[test]
fn hovered_link_target_none_outside_link_span() {
    let st = state("See [docs](https://example.com) for more.\n");
    // Col 0 is the leading 'S', not a link.
    assert!(mouse_ops::hovered_link_target(&st, 0, 0, VW).is_none());
}

#[test]
fn link_target_parse_classifies_inputs_correctly() {
    use std::path::PathBuf;
    assert_eq!(
        LinkTarget::parse("#section", None),
        LinkTarget::Anchor("section".to_owned())
    );
    assert_eq!(
        LinkTarget::parse("https://example.com", None),
        LinkTarget::Url("https://example.com".to_owned())
    );
    assert_eq!(
        LinkTarget::parse("mailto:a@b.c", None),
        LinkTarget::Url("mailto:a@b.c".to_owned())
    );
    let base = PathBuf::from("/docs");
    assert_eq!(
        LinkTarget::parse("sibling.md", Some(&base)),
        LinkTarget::LocalFile(base.join("sibling.md"))
    );
}

// ── Phase 13: column-width injection guard ──────────────────────────────────

/// On a table without a `tui-columns` comment, the Release of a column-
/// border drag stages a pending commit but does NOT yet write the
/// comment.  The App is responsible for either committing immediately
/// (when warnings are off) or showing the warning modal.  Until that
/// resolves, the buffer must be unchanged.
#[test]
fn column_border_drag_release_defers_commit_when_no_existing_comment() {
    let src = "| abc | defghi |\n| --- | --- |\n| bar | baz |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    let snap = fake_snapshot(FakeSnapshotSpec {
        table_byte_start: 0,
        table_byte_end: src.len(),
        col_count: 2,
        row_count: 3,
        col_ranges: vec![1..6, 7..15],
        row_ranges: vec![3..4],
        row_handle_col: None,
        top_border_row: None,
    });
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    mouse_ops::apply(&mut st, click(6, 3), &mut target, &snapshots, VP, VW);
    mouse_ops::apply(
        &mut st,
        MouseAction::Drag { col: 8, row: 3 },
        &mut target,
        &snapshots,
        VP,
        VW,
    );
    mouse_ops::apply(
        &mut st,
        MouseAction::Release,
        &mut target,
        &snapshots,
        VP,
        VW,
    );

    // The Release set the pending flag — buffer is still untouched.
    assert!(st.has_pending_column_widths());
    assert_eq!(st.contents(), src);

    // Cancelling the pending commit must drop the live preview without
    // ever writing the `tui-columns` comment.
    st.cancel_pending_column_widths();
    assert!(!st.has_pending_column_widths());
    assert!(
        st.live_table_widths.is_none(),
        "Cancel must clear the live preview"
    );
    assert!(
        !st.contents().contains("tui-columns"),
        "Cancel must NOT inject a comment, got: {:?}",
        st.contents()
    );
}

/// Tables that already have a `tui-columns` comment skip the warning —
/// `table_has_tui_columns_comment` returns true so the App's
/// `handle_pending_column_widths` path commits immediately.  This test
/// is the EditorState half of that contract: it just verifies the
/// detection.  The full warning-skip flow is exercised by the
/// pre-existing `column_border_drag_writes_tui_columns_comment` test.
#[test]
fn table_has_tui_columns_comment_detects_existing_persistence() {
    let with = state("| a | b |\n|---|---|\n| 1 | 2 |\n<!-- tui-columns: [5, 7] -->\n");
    assert!(with.table_has_tui_columns_comment(0));

    let without = state("| a | b |\n|---|---|\n| 1 | 2 |\n");
    assert!(!without.table_has_tui_columns_comment(0));
}

// ── Delete-handle clicks ────────────────────────────────────────────────────
//
// Snapshots constructed here are slightly richer than `fake_snapshot`
// produces, so we build them inline.  All four variants live behind a
// tiny helper to keep each test focused on the assertion.

fn snapshot_with_delete_handles(
    src: &str,
    col_ranges: Vec<std::ops::Range<u16>>,
    row_ranges: Vec<std::ops::Range<u16>>,
    delete_row_handle_col: Option<u16>,
    bottom_border_row: Option<u16>,
) -> TableLayoutSnapshot {
    TableLayoutSnapshot {
        table_byte_start: 0,
        table_byte_end: src.len(),
        col_count: col_ranges.len(),
        row_count: 2 + row_ranges.len(),
        col_ranges,
        row_ranges,
        row_handle_col: None,
        top_border_row: None,
        header_row: None,
        delete_row_handle_col,
        bottom_border_row,
    }
}

#[test]
fn delete_row_handle_click_removes_data_row() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    // Layout (matches `row_handle_drag_swaps_rows_in_buffer`): col content at
    // x=2..5 and x=6..9; data rows at y=3 and y=5.  Right outer `│` sits at
    // x=9 (last col_range.end), and the `✕` glyph overlays that border.
    let snap = snapshot_with_delete_handles(src, vec![2..5, 6..9], vec![3..4, 5..6], Some(9), None);
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    // Click the `✕` (which sits on the right border at x=9) for the FIRST
    // data row (y=3).  Deletes the first data row, leaving "| 3 | 4 |".
    mouse_ops::apply(&mut st, click(9, 3), &mut target, &snapshots, VP, VW);

    let after = st.contents();
    assert!(
        !after.contains("| 1 | 2 |"),
        "expected first data row deleted, got: {after:?}"
    );
    assert!(
        after.contains("| 3 | 4 |"),
        "expected second data row preserved, got: {after:?}"
    );
    // Click should not enter a drag.
    assert!(target.is_none());
    assert!(!st.drag_in_progress);
}

#[test]
fn delete_column_handle_click_removes_column() {
    let src = "| a | b | c |\n|---|---|---|\n| 1 | 2 | 3 |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    // Three columns at x=2..5, x=6..9, x=10..13.  Bottom border at y=4
    // (top border y=0, header y=1, thick separator y=2, data row y=3,
    // bottom border y=4).
    let snap =
        snapshot_with_delete_handles(src, vec![2..5, 6..9, 10..13], vec![3..4], None, Some(4));
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    // Click the column-delete `✕` centred on column 1 (x=7, halfway between
    // x_range start 6 and end 9, on the bottom-border row).
    mouse_ops::apply(&mut st, click(7, 4), &mut target, &snapshots, VP, VW);

    let after = st.contents();
    // Column "b"/"2" gone, "a"/"1" and "c"/"3" remain.
    assert!(
        !after.contains(" b "),
        "expected column b deleted, got: {after:?}"
    );
    assert!(
        !after.contains(" 2 "),
        "expected data 2 deleted, got: {after:?}"
    );
    assert!(after.contains(" a "), "got: {after:?}");
    assert!(after.contains(" c "), "got: {after:?}");
    assert!(target.is_none());
    assert!(!st.drag_in_progress);
}

/// The right outer `│` cell on a DATA ROW now carries the `✕` glyph
/// and deletes that row, so right-column resize on data rows happens
/// at the cell just inside the border (`last.end - 1`, within the
/// `ColumnBorder ±1` tolerance).  Verifies that path is still wired
/// and behaves as a resize drag.
#[test]
fn right_column_resize_still_works_via_cell_just_inside_border() {
    let src = "| headerA | headerB |\n|-----|-----|\n| foo   | bar   |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    // Mirror `column_border_drag_writes_tui_columns_comment` layout:
    // col 0 at x=2..12, col 1 at x=13..23.  Right outer `│` at x=23,
    // delete glyph painted there too — but the cell at x=22 is one
    // inside the border and still resolves to ColumnBorder.
    let snap =
        snapshot_with_delete_handles(src, vec![2..12, 13..23], vec![3..4], Some(23), Some(5));
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    mouse_ops::apply(&mut st, click(22, 3), &mut target, &snapshots, VP, VW);
    assert!(
        matches!(
            target,
            Some(mouse_ops::DragTarget::TableColumnBorder { .. })
        ),
        "expected ColumnBorder drag at border-1, got: {target:?}",
    );
    assert_eq!(st.contents(), src);
}

/// Click directly on the right border at the same x as the `✕` glyph
/// but on a non-data row (e.g. header / alignment / top / bottom
/// border) must still resolve to `ColumnBorder`.  This preserves
/// resize access from those rows.
#[test]
fn right_border_on_non_data_row_still_resizes() {
    let src = "| headerA | headerB |\n|-----|-----|\n| foo   | bar   |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    let snap =
        snapshot_with_delete_handles(src, vec![2..12, 13..23], vec![3..4], Some(23), Some(5));
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    // y=1 is the header row — outside row_ranges (which only carries
    // data rows), so the delete-handle hit-test bails and the click
    // falls through to ColumnBorder at the border ±1 window.
    mouse_ops::apply(&mut st, click(23, 1), &mut target, &snapshots, VP, VW);
    assert!(
        matches!(
            target,
            Some(mouse_ops::DragTarget::TableColumnBorder { .. })
        ),
        "expected ColumnBorder drag on non-data row, got: {target:?}",
    );
    assert_eq!(st.contents(), src);
}

/// The `✕` click on a data row deletes that row — the inverse of the
/// resize-still-works case above.  Documents the new contract that
/// the right-border cell on a data row is reclaimed for delete.
#[test]
fn right_border_on_data_row_deletes() {
    let src = "| headerA | headerB |\n|-----|-----|\n| foo   | bar   |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    let snap =
        snapshot_with_delete_handles(src, vec![2..12, 13..23], vec![3..4], Some(23), Some(5));
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    mouse_ops::apply(&mut st, click(23, 3), &mut target, &snapshots, VP, VW);
    let after = st.contents();
    assert!(
        !after.contains("foo"),
        "expected data row deleted, got: {after:?}"
    );
    assert!(target.is_none());
    assert!(!st.drag_in_progress);
}

/// When `show_buttons` is off, `build_snapshots` leaves the new
/// delete-handle fields at `None`, so the right border at x=9 stays
/// a pure resize target on data rows (existing behaviour) and clicks
/// on the bottom-border row don't delete a column.
#[test]
fn delete_handles_inert_when_handle_fields_are_none() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    let snap = snapshot_with_delete_handles(src, vec![2..5, 6..9], vec![3..4], None, None);
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    // Click on the right border at (9, 3): with delete handle disabled,
    // this is a column-resize drag, not a delete.
    mouse_ops::apply(&mut st, click(9, 3), &mut target, &snapshots, VP, VW);
    assert!(
        matches!(
            target,
            Some(mouse_ops::DragTarget::TableColumnBorder { .. })
        ),
        "expected ColumnBorder drag with handles disabled, got: {target:?}",
    );
    // No buffer change.
    assert_eq!(st.contents(), src);

    // Reset and try a click on what would be the column-delete bottom
    // border centre — without the snapshot field set, no delete fires.
    target = None;
    mouse_ops::apply(&mut st, click(7, 4), &mut target, &snapshots, VP, VW);
    assert_eq!(st.contents(), src);
}

// ── Cell-aware click hit-testing on wrapped lines ─────────────────────────────

/// Regression: clicking on the *second* visual row of a wrapped paragraph
/// must land in the wrapped continuation, not back on the first row.
/// Pre-fix the click ignored `sub_row_within_line` and walked from the line
/// start, so cell column 3 of row 1 lit up char 3 of row 0.
#[test]
fn click_on_wrapped_continuation_row_lands_in_continuation_text() {
    // 32-cell paragraph, viewport width 16 → wraps to 2 visual rows.
    // Row 0: "the quick brown " (16 chars / 16 cells)
    // Row 1: "fox jumps over"   (14 chars)
    let mut st = state("the quick brown fox jumps over\n");
    st.mode = Mode::Rendered;
    let viewport_w: usize = 16;
    let mut mouse = MouseDispatcher::new();
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    let click_area = Rect {
        x: 0,
        y: 0,
        width: viewport_w as u16,
        height: 24,
    };
    let action = mouse
        .dispatch(click_event(0, 1), click_area)
        .expect("click dispatched");
    mouse_ops::apply(&mut st, action, &mut anchor, &[], VP, viewport_w);
    // Pre-fix: cursor would land at offset 0 (start of row 0).  Post-fix:
    // it lands at offset 16 (start of row 1's content).
    assert_eq!(st.cursor.offset, 16);
}

/// Click on the *fifth* cell of the second wrapped row of a paragraph
/// containing a leading wide char.  Verifies wide-char snap-past survives
/// the wrap math.
#[test]
fn click_on_wrapped_row_handles_wide_char_in_row() {
    // Line: "🥇 leader plus more text after"  (29 chars, but 30 cells —
    // the emoji is 2 cells).  Viewport width 12, so:
    //   row 0: "🥇 leader "   ends at chars 9 / cells 10
    //   row 1: "plus more "   chars 9..19 / cells 10
    //   row 2: "text after"   chars 19..29
    let mut st = state("🥇 leader plus more text after\n");
    st.mode = Mode::Rendered;
    let viewport_w: usize = 12;
    let mut mouse = MouseDispatcher::new();
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    let click_area = Rect {
        x: 0,
        y: 0,
        width: viewport_w as u16,
        height: 24,
    };
    // Click on cell 5 of row 2 → 't' of "after".  ASCII chars on row 2
    // start at char 19.  Char at row-cell 5 = chars[19+5] = 'a' of "after".
    let action = mouse
        .dispatch(click_event(5, 2), click_area)
        .expect("click dispatched");
    mouse_ops::apply(&mut st, action, &mut anchor, &[], VP, viewport_w);
    assert_eq!(st.contents().chars().nth(st.cursor.offset), Some('a'));
}

// ── Mermaid block reveal interaction ────────────────────────────────────────

#[test]
fn same_mermaid_block_click_does_not_set_drag_in_progress() {
    // Mermaid blocks reveal as a single unit: every reserved row paints
    // raw source while the cursor is inside.  A click on a different
    // line within the same mermaid block must NOT set
    // `drag_in_progress`, otherwise the entire image flashes back in
    // for the click-to-mouseup window.
    let src = "```mermaid\nflowchart TD\nA-->B\nB-->C\n```\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    // Seed cursor on the first content line; reveal already active.
    st.cursor.offset = src.find("flowchart").unwrap();
    st.update_cursor_block();
    st.cursor_block_entered_at = None;
    assert!(st.cursor_block_revealed());
    let original_block = st.cursor_block_idx;

    // Click on row 2 of the rendered output (raw line "A-->B" within
    // the same mermaid block).
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    mouse_ops::apply(&mut st, click(0, 2), &mut anchor, &[], VP, VW);

    assert_eq!(
        st.cursor_block_idx, original_block,
        "click stayed inside the same mermaid block",
    );
    assert!(
        !st.drag_in_progress,
        "intra-mermaid click must not flip drag-in-progress on",
    );
    assert!(
        st.cursor_block_revealed(),
        "intra-mermaid click must keep raw reveal active",
    );
}

#[test]
fn click_on_mermaid_row_lands_on_clicked_column() {
    // The `[Image: …]` placeholder + blank reserved rows have no
    // useful per-character content for the standard rendered→raw
    // column map.  `rendered_sub_line_to_offset` must short-circuit
    // to a direct cell-aware lookup against the raw mermaid source so
    // mouse clicks land on the character under the pointer.
    let src = "```mermaid\nflowchart TD\nA-->B\n```\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    // Seed cursor inside the mermaid block so reveal is active.
    st.cursor.offset = src.find("flowchart").unwrap();
    st.update_cursor_block();
    st.cursor_block_entered_at = None;

    // Click col 5, row 2 → 6th char of "A-->B" line.  But "A-->B" has
    // only 5 chars, so the click should clamp to its last column.  Use
    // col 2 on row 2 instead — should land on the third char ('-') of
    // "A-->B".
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    mouse_ops::apply(&mut st, click(2, 2), &mut anchor, &[], VP, VW);

    let chars: Vec<char> = st.contents().chars().collect();
    assert_eq!(
        chars.get(st.cursor.offset),
        Some(&'-'),
        "click on col 2 of 'A-->B' must land on third char, landed at {} = {:?}",
        st.cursor.offset,
        chars.get(st.cursor.offset),
    );
}

#[test]
fn intra_mermaid_line_move_does_not_rearm_reveal_timer() {
    // `update_cursor_block` re-arms `cursor_block_entered_at` on every
    // buffer-line change so tables (and other multi-line blocks) get a
    // uniform per-cell reveal delay.  Mermaid is exempt: re-arming on
    // each line move would flash the image placeholder back in for
    // ~120ms after every keystroke.  Verify the timer holds steady
    // while the cursor stays inside the same mermaid block.
    let src = "Intro paragraph.\n\n```mermaid\nflowchart TD\nA-->B\nB-->C\n```\n\nTrailer.\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    // Seed cursor on the intro paragraph so the next move actually
    // crosses a block boundary (and arms the timer).
    st.cursor.offset = 0;
    st.update_cursor_block();
    st.cursor_block_entered_at = None;

    // Enter the mermaid block — block boundary crossed, timer arms.
    st.cursor.offset = src.find("flowchart").unwrap();
    st.update_cursor_block();
    let entered_at = st.cursor_block_entered_at;
    assert!(entered_at.is_some(), "entering mermaid arms the reveal timer");

    // Move to the next content line within the same mermaid block.
    st.cursor.offset = src.find("A-->B").unwrap();
    st.update_cursor_block();
    assert_eq!(
        st.cursor_block_entered_at, entered_at,
        "intra-mermaid line move must not re-arm the reveal timer",
    );

    // Moving back to the intro paragraph (a different block) must
    // re-arm the timer.
    st.cursor.offset = 0;
    st.update_cursor_block();
    assert_ne!(
        st.cursor_block_entered_at, entered_at,
        "leaving the mermaid block re-arms the reveal timer",
    );
}
