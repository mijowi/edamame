//! Integration tests for Phase 5 mouse support.
//!
//! Verifies that mouse click/drag/scroll/checkbox handling works end-to-end
//! against a real `EditorState` and the `mouse_ops::apply` entry point used
//! by the main app loop.

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
fn hit_test_returns_true_over_task_checkbox() {
    let st = state("- [ ] first\n- [x] second\n");
    // Task items render without the `- ` prefix, so `[` is at rendered col 0.
    // Rows 0 and 1 both expose a `[ ]`/`[x]` glyph at cols 0-2.
    for (row, col) in [(0u16, 0u16), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)] {
        assert!(
            mouse_ops::hit_test_clickable(&st, col, row, VW),
            "expected hit-test TRUE at ({col}, {row})"
        );
    }
    // Col 3 is the space AFTER `]`, outside the glyph.
    assert!(!mouse_ops::hit_test_clickable(&st, 3, 0, VW));
    // Col 5 is in the body text.
    assert!(!mouse_ops::hit_test_clickable(&st, 5, 0, VW));
}

#[test]
fn hit_test_returns_true_over_markdown_link() {
    let st = state("See [docs](https://x.com) for info.\n");
    // `[docs](url)` renders as just `docs` — 4 chars at rendered cols 4..=7.
    // The URL is invisible in rendered mode, so only the link text is a
    // visible "clickable region".
    assert!(mouse_ops::hit_test_clickable(&st, 5, 0, VW)); // inside "docs"
    assert!(mouse_ops::hit_test_clickable(&st, 7, 0, VW)); // last char of "docs"
                                                           // Click outside the link text.
    assert!(!mouse_ops::hit_test_clickable(&st, 2, 0, VW));
    assert!(!mouse_ops::hit_test_clickable(&st, 10, 0, VW));
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
fn fake_snapshot(
    table_byte_start: usize,
    table_byte_end: usize,
    col_count: usize,
    row_count: usize,
    col_ranges: Vec<std::ops::Range<u16>>,
    row_ranges: Vec<std::ops::Range<u16>>,
    row_handle_col: Option<u16>,
    top_border_row: Option<u16>,
) -> TableLayoutSnapshot {
    TableLayoutSnapshot {
        table_byte_start,
        table_byte_end,
        col_count,
        row_count,
        col_ranges,
        row_ranges,
        row_handle_col,
        top_border_row,
        header_row: None,
    }
}

#[test]
fn row_handle_drag_swaps_rows_in_buffer() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    // Fabricate a snapshot whose row handle sits at x=0 and whose two data
    // rows occupy y=3 and y=5.  The column content spans x=2..5 and x=6..9.
    let snap = fake_snapshot(
        0,                // table_byte_start
        src.len(),        // table_byte_end
        2,                // col_count
        4,                // row_count
        vec![2..5, 6..9], // col_ranges
        vec![3..4, 5..6], // row_ranges (data rows only)
        Some(0),          // row_handle_col
        Some(0),          // top_border_row
    );
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
    let snap = fake_snapshot(
        0,
        src.len(),
        2,
        3,
        vec![2..12, 13..23],
        vec![3..4],
        None,
        None,
    );
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
    let snap = fake_snapshot(
        0,
        src.len(),
        2,
        3,
        vec![1..6, 7..15],
        vec![3..4],
        None,
        None,
    );
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
    let snap = fake_snapshot(
        0,
        src.len(),
        2,
        3,
        vec![1..6, 7..15],
        vec![3..4],
        None,
        None,
    );
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
    let snap = fake_snapshot(
        0,
        src.len(),
        2,
        3,
        vec![2..5, 6..9],
        vec![3..4],
        None,
        Some(0),
    );
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

    let snap = fake_snapshot(
        0,
        src.len(),
        2,
        3,
        vec![1..4, 5..8],
        vec![3..4],
        None,
        Some(0),
    );
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

    let snap = fake_snapshot(
        0,
        src.len(),
        2,
        4,
        vec![2..5, 6..9],
        vec![3..4, 5..6],
        Some(0),
        None,
    );
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

    let snap = fake_snapshot(
        0,
        src.len(),
        2,
        3,
        vec![1..6, 7..15],
        vec![3..4],
        None,
        None,
    );
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
