//! Integration tests for mouse support.
//!
//! Verifies that mouse click/drag/scroll/checkbox handling works end-to-end
//! against a real `EditorState` and the `mouse_ops::apply` entry point used
//! by the main app loop.

#![allow(clippy::single_range_in_vec_init)]

use std::time::{Duration, Instant};

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
        Some(mouse_ops::DragTarget::TextSelection {
            anchor: 7,
            cell: None
        })
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
    let mut anchor: Option<mouse_ops::DragTarget> = Some(mouse_ops::DragTarget::TextSelection {
        anchor: 0,
        cell: None,
    });
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
    let mut anchor: Option<mouse_ops::DragTarget> = Some(mouse_ops::DragTarget::TextSelection {
        anchor: 0,
        cell: None,
    });
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

/// An in-document heading anchor (`[text](#section)`) renders with
/// `Theme::link_heading` — link colored but deliberately NOT underlined
/// — so the hit-test must recognise it by style, not by the underline
/// alone.  Without that it stayed an I-beam while web and file links
/// showed the hand.
#[test]
fn hit_test_returns_true_over_heading_anchor_link() {
    let st = state("See [Setup](#setup) below.\n");
    // Renders as `See Setup below.` — the link text sits at cols 4..=8.
    assert!(mouse_ops::hit_test_clickable(&st, 4, 0, VW, &[]));
    assert!(mouse_ops::hit_test_clickable(&st, 8, 0, VW, &[]));
    // Surrounding prose is not clickable.
    assert!(!mouse_ops::hit_test_clickable(&st, 1, 0, VW, &[]));
    assert!(!mouse_ops::hit_test_clickable(&st, 12, 0, VW, &[]));
}

/// The hint line's hover tooltip reads the same predicate, so a heading
/// anchor surfaces its raw `#section` target too.
#[test]
fn hovered_link_url_resolves_heading_anchor() {
    let st = state("See [Setup](#setup) below.\n");
    assert_eq!(
        mouse_ops::hovered_link_url(&st, 5, 0, VW).as_deref(),
        Some("#setup")
    );
    assert_eq!(mouse_ops::hovered_link_url(&st, 1, 0, VW), None);
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

// ── Table drag flows ────────────────────────────────────────────────────────

use edamame::ui::table_view::TableLayoutSnapshot;

/// Build a snapshot for a table whose first row begins at `table_byte_start`,
/// sized to simulate what the `build_snapshots` function would have
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

/// The pointer spends much of a row drag on cells that classify as
/// something other than a row / cell hit — the `├─┼─┤` separator between
/// two rows, or any `│` border (which wins the hit-test as a
/// `ColumnBorder`).  The hover target must still follow the pointer's
/// height, or the drop indicator freezes at whatever row it last saw.
#[test]
fn row_drag_hover_follows_the_pointer_over_separators_and_borders() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    // Data rows at y=3 and y=5; y=4 is the separator between them.  The
    // interior border sits at x=5.
    let snap = fake_snapshot(FakeSnapshotSpec {
        table_byte_start: 0,
        table_byte_end: src.len(),
        col_count: 2,
        row_count: 4,
        col_ranges: vec![2..5, 6..9],
        row_ranges: vec![3..4, 5..6],
        row_handle_col: Some(0),
        top_border_row: Some(0),
    });
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    mouse_ops::apply(&mut st, click(0, 3), &mut target, &snapshots, VP, VW);

    // Over the separator, in the gutter — snaps to the nearer data row.
    mouse_ops::apply(
        &mut st,
        MouseAction::Drag { col: 0, row: 4 },
        &mut target,
        &snapshots,
        VP,
        VW,
    );
    assert!(
        matches!(
            target,
            Some(mouse_ops::DragTarget::TableRow {
                hover_row_idx: 2,
                ..
            })
        ),
        "separator hover should resolve to a data row, got: {target:?}"
    );

    // Over the interior `│` border on the second data row — classifies as
    // `ColumnBorder`, which used to leave the hover stale.
    mouse_ops::apply(
        &mut st,
        MouseAction::Drag { col: 5, row: 5 },
        &mut target,
        &snapshots,
        VP,
        VW,
    );
    assert!(
        matches!(
            target,
            Some(mouse_ops::DragTarget::TableRow {
                hover_row_idx: 3,
                ..
            })
        ),
        "border hover should still track the row under the pointer, got: {target:?}"
    );
}

/// Column-drag counterpart: dragging along the top border crosses the `┬`
/// vertices between columns, which classify as `ColumnBorder`.
#[test]
fn column_drag_hover_follows_the_pointer_over_border_vertices() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

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

    mouse_ops::apply(&mut st, click(3, 0), &mut target, &snapshots, VP, VW);
    // x=5 is the `┬` vertex between the two columns.
    mouse_ops::apply(
        &mut st,
        MouseAction::Drag { col: 5, row: 0 },
        &mut target,
        &snapshots,
        VP,
        VW,
    );
    assert!(
        matches!(
            target,
            Some(mouse_ops::DragTarget::TableColumnHeader {
                hover_col_idx: 0,
                ..
            })
        ),
        "vertex hover should resolve to a column, got: {target:?}"
    );
    // Fully inside column 1.
    mouse_ops::apply(
        &mut st,
        MouseAction::Drag { col: 7, row: 0 },
        &mut target,
        &snapshots,
        VP,
        VW,
    );
    assert!(matches!(
        target,
        Some(mouse_ops::DragTarget::TableColumnHeader {
            hover_col_idx: 1,
            ..
        })
    ));
}

/// A retry-grab of the same handle within the multi-click window arrives as
/// a `DoubleClick`.  It must arm the drag exactly like the first press —
/// the arm used to skip table hit-testing entirely, so the second and third
/// attempts at the same spot did nothing at all.
#[test]
fn multi_click_on_a_row_handle_still_arms_the_drag() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
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
        top_border_row: Some(0),
    });
    let snapshots = [snap];

    for action in [
        MouseAction::DoubleClick {
            col: 0,
            row: 3,
            modifiers: KeyModifiers::NONE,
        },
        MouseAction::TripleClick {
            col: 0,
            row: 3,
            modifiers: KeyModifiers::NONE,
        },
    ] {
        let mut target: Option<mouse_ops::DragTarget> = None;
        mouse_ops::apply(&mut st, action, &mut target, &snapshots, VP, VW);
        assert!(
            matches!(
                target,
                Some(mouse_ops::DragTarget::TableRow { row_idx: 2, .. })
            ),
            "expected a re-grab to arm the row drag, got: {target:?}"
        );
    }
}

/// The destructive delete handles are the exception: a rapid repeat press
/// on `✕` is swallowed rather than deleting a second row.  The guard is a
/// cooldown keyed off the last delete, so it catches a fast plain `Click`
/// too, not just a press the dispatcher happened to classify as a chord.
#[test]
fn rapid_repeat_on_a_delete_handle_does_not_delete_twice() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    let mut snap = fake_snapshot(FakeSnapshotSpec {
        table_byte_start: 0,
        table_byte_end: src.len(),
        col_count: 2,
        row_count: 4,
        col_ranges: vec![2..5, 6..9],
        row_ranges: vec![3..4, 5..6],
        row_handle_col: Some(0),
        top_border_row: Some(0),
    });
    snap.delete_row_handle_col = Some(9);
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    mouse_ops::apply(&mut st, click(9, 3), &mut target, &snapshots, VP, VW);
    let after_first = st.contents();
    assert!(!after_first.contains("| 1 | 2 |"), "first click deletes");

    mouse_ops::apply(
        &mut st,
        MouseAction::DoubleClick {
            col: 9,
            row: 3,
            modifiers: KeyModifiers::NONE,
        },
        &mut target,
        &snapshots,
        VP,
        VW,
    );
    assert_eq!(
        st.contents(),
        after_first,
        "a repeat press on the delete handle must not delete another row"
    );
}

/// …but the guard always expires, which is the whole reason it is anchored
/// to the delete rather than to the click chord.  A multi-click window
/// restarts on every press, so a user clicking `✕` steadily faster than the
/// window never leaves the chord and deletes exactly one row before the
/// button goes silently dead.  Backdating the stamp stands in for the
/// cooldown elapsing.
#[test]
fn a_deliberate_repeat_on_a_delete_handle_deletes_again() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    let mut snap = fake_snapshot(FakeSnapshotSpec {
        table_byte_start: 0,
        table_byte_end: src.len(),
        col_count: 2,
        row_count: 4,
        col_ranges: vec![2..5, 6..9],
        row_ranges: vec![3..4, 5..6],
        row_handle_col: Some(0),
        top_border_row: Some(0),
    });
    snap.delete_row_handle_col = Some(9);
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    mouse_ops::apply(&mut st, click(9, 3), &mut target, &snapshots, VP, VW);
    assert!(!st.contents().contains("| 1 | 2 |"), "first click deletes");

    st.last_table_delete_at = Some(Instant::now() - Duration::from_millis(400));
    mouse_ops::apply(
        &mut st,
        MouseAction::TripleClick {
            col: 9,
            row: 3,
            modifiers: KeyModifiers::NONE,
        },
        &mut target,
        &snapshots,
        VP,
        VW,
    );
    assert!(
        !st.contents().contains("| 3 | 4 |"),
        "once the cooldown lapses the handle deletes again, however the \
         dispatcher classified the press: {:?}",
        st.contents()
    );
}

/// The reorder handles are painted under the same cursor-in-table rule as
/// the `✕` pair, so they owe the same focus guard: a press on an unfocused
/// table's top border must not arm a column reorder on a control that was
/// never drawn there.
#[test]
fn column_handle_on_an_unfocused_table_focuses_it_instead_of_arming() {
    let src = "intro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
    let table_start = src.find("| a |").unwrap();
    let mut st = state(src);
    st.mode = Mode::Rendered;
    st.cursor.offset = 0; // on "intro", outside the table

    let snap = fake_snapshot(FakeSnapshotSpec {
        table_byte_start: table_start,
        table_byte_end: src.len(),
        col_count: 2,
        row_count: 3,
        col_ranges: vec![2..5, 6..9],
        row_ranges: vec![5..6],
        row_handle_col: Some(0),
        top_border_row: Some(2),
    });
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    mouse_ops::apply(&mut st, click(3, 2), &mut target, &snapshots, VP, VW);
    assert!(
        target.is_none(),
        "a press on an unfocused table's top border must not arm a reorder, got: {target:?}"
    );
    assert_eq!(
        st.buffer.rope().char_to_byte(st.cursor.offset),
        table_start,
        "it focuses the table instead"
    );

    // Focused now, so the same press grabs the handle.
    mouse_ops::apply(&mut st, click(3, 2), &mut target, &snapshots, VP, VW);
    assert!(
        matches!(
            target,
            Some(mouse_ops::DragTarget::TableColumnHeader { col_idx: 0, .. })
        ),
        "expected the second press to arm the column drag, got: {target:?}"
    );
}

/// The `✕` buttons are painted only on the table the cursor is inside, but
/// hit-testing runs against every visible table.  A click on an unpainted
/// delete button must focus the table (making the button appear) rather
/// than silently deleting a row nobody saw a button for.
#[test]
fn delete_handle_on_an_unfocused_table_focuses_it_instead_of_deleting() {
    let src = "intro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
    let table_start = src.find("| a |").unwrap();
    let mut st = state(src);
    st.mode = Mode::Rendered;
    st.cursor.offset = 0; // on "intro", outside the table

    let mut snap = fake_snapshot(FakeSnapshotSpec {
        table_byte_start: table_start,
        table_byte_end: src.len(),
        col_count: 2,
        row_count: 3,
        col_ranges: vec![2..5, 6..9],
        row_ranges: vec![5..6],
        row_handle_col: Some(0),
        top_border_row: Some(2),
    });
    snap.delete_row_handle_col = Some(9);
    let snapshots = [snap];
    let mut target: Option<mouse_ops::DragTarget> = None;

    mouse_ops::apply(&mut st, click(9, 5), &mut target, &snapshots, VP, VW);
    assert_eq!(st.contents(), src, "first click must not delete");
    assert_eq!(
        st.buffer.rope().char_to_byte(st.cursor.offset),
        table_start,
        "first click focuses the table"
    );

    // Now that the cursor is in the table (and the button is painted), the
    // same click deletes.
    mouse_ops::apply(&mut st, click(9, 5), &mut target, &snapshots, VP, VW);
    assert!(!st.contents().contains("| 1 | 2 |"), "second click deletes");
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

    // Release no longer auto-commits — it stages a pending
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

// ── Clickable links and navigation ──────────────────────────────────────────

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
    // The link line must NOT be the cursor's current line so it stays
    // rendered — otherwise reveal de-renders it and the click maps
    // against raw chars (a separate code path covered by the
    // `click_on_revealed_link_line_*` tests).
    let src = "first line\nSee [docs](https://example.com) for more.\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    let mut mouse = MouseDispatcher::new();
    // Rendered line 1 is "See docs for more." — col 5 is 'o' (S-e-e-
    // space at 0..3 + 'd' is col 4 + 'o' is col 5).
    if let Some(a) = mouse.dispatch(click_event(5, 1), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    // 'o' inside `[docs]` sits at raw byte 6 of line 2:
    // `See [docs](https://...)` — preceded by the 11-byte first line.
    assert_eq!(
        st.contents().chars().nth(st.cursor.offset),
        Some('o'),
        "expected cursor on 'o' of 'docs', landed at offset {} ({:?})",
        st.cursor.offset,
        st.contents().chars().nth(st.cursor.offset),
    );
}

/// When the cursor is on a line containing a link, the reveal in
/// `RenderedView` paints the raw `[text](url)` source over that
/// rendered row.  A click on the visible `(`, the URL chars, or `)`
/// must place the cursor at the corresponding raw column — not get
/// clamped past the rendered link text as it did before clicks on
/// revealed lines were routed through raw-char mapping.
#[test]
fn click_on_revealed_link_line_lands_on_raw_char() {
    let src = "See [docs](https://example.com) for more.\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    let mut mouse = MouseDispatcher::new();
    // Cursor is at offset 0; reveal is active on a fresh state so the
    // line is shown as the raw `See [docs](https://example.com)...`.
    // Col 11 → 'h' of `https`.
    if let Some(a) = mouse.dispatch(click_event(11, 0), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    assert_eq!(
        st.contents().chars().nth(st.cursor.offset),
        Some('h'),
        "expected cursor on 'h' of 'https', landed at offset {} ({:?})",
        st.cursor.offset,
        st.contents().chars().nth(st.cursor.offset),
    );
}

/// Regression: in a *loose* list (blank lines between items), the
/// separator blanks each render a row of their own, so a later item's
/// rendered row index is higher than its non-blank line count.
/// `cursor_rendered_line_idx` used to count only non-blank raw lines
/// while `RenderedView` counted separator blanks too, so the mouse
/// hit-test believed the reveal was on a different row than the one it
/// was painted on.  The click then mapped against the *rendered* spans
/// (`code`, backticks dropped) instead of the raw text on screen.
#[test]
fn click_on_revealed_loose_list_item_with_inline_code_lands_on_raw_char() {
    let src = "- Alpha item\n\n- Beta item\n\n- Gamma `code` tail\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    // Park the cursor on the third item so its rendered row is revealed
    // as raw source.
    st.cursor.offset = src.find("Gamma").unwrap();
    st.update_cursor_block();
    st.cursor_block_entered_at = None; // reveal returns true immediately

    // Rendered rows: 0 `• Alpha item`, 1 blank, 2 `• Beta item`,
    // 3 blank, 4 the revealed raw `- Gamma \`code\` tail`.
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    let mut mouse = MouseDispatcher::new();
    // Raw col 15 is the `t` of `tail`.
    if let Some(a) = mouse.dispatch(click_event(15, 4), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }
    let line_start = src.find("- Gamma").unwrap();
    assert_eq!(
        st.cursor.offset,
        line_start + 15,
        "expected cursor on the 't' of 'tail', landed on {:?}",
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
fn hovered_link_url_returns_raw_url_on_link_hover() {
    let st = state("See [docs](https://example.com) for more.\n");
    let url = mouse_ops::hovered_link_url(&st, 5, 0, VW);
    assert_eq!(url.as_deref(), Some("https://example.com"));
}

#[test]
fn hovered_link_url_keeps_relative_path_as_written() {
    // The hint line shows what the author wrote — no base-dir
    // resolution to an absolute path.
    let st = state("See [notes](./notes.md) for more.\n");
    let url = mouse_ops::hovered_link_url(&st, 5, 0, VW);
    assert_eq!(url.as_deref(), Some("./notes.md"));
}

#[test]
fn hovered_link_url_none_outside_link_span() {
    let st = state("See [docs](https://example.com) for more.\n");
    // Col 0 is the leading 'S', not a link.
    assert!(mouse_ops::hovered_link_url(&st, 0, 0, VW).is_none());
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

// ── Column-width injection guard ────────────────────────────────────────────

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
fn click_on_visible_mermaid_block_parks_cursor_at_end_of_last_code_line() {
    // Cursor lives in the trailer (not the mermaid block) so the image
    // is actually showing.  A click anywhere on the rendered placeholder
    // must de-render the image (clear `cursor_block_entered_at`) and
    // park the cursor right after the last char of the last code line —
    // here, after the `C` in `B-->C`.
    let src = "Intro.\n\n```mermaid\nflowchart TD\nA-->B\nB-->C\n```\n\nTrailer.\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    st.cursor.offset = src.find("Trailer").unwrap();
    st.update_cursor_block();

    // Mermaid block starts at rendered row 2 (after "Intro.\n\n").  Click
    // on row 3 (somewhere in the reserved placeholder area).
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    mouse_ops::apply(&mut st, click(5, 3), &mut anchor, &[], VP, VW);

    let chars: Vec<char> = st.contents().chars().collect();
    let last_c = src.find("B-->C").unwrap() + "B-->C".len();
    assert_eq!(
        st.cursor.offset,
        last_c,
        "click on rendered mermaid placeholder must park cursor after last code char (got {:?})",
        chars.get(st.cursor.offset),
    );
    assert!(
        st.cursor_block_revealed(),
        "image click must force-reveal the block immediately",
    );
    assert!(anchor.is_none(), "image click should not start a drag");
}

#[test]
fn click_on_visible_image_block_parks_cursor_at_end_of_source_line() {
    // Regular image — paragraph promoted to ImageBlock.  Cursor parked
    // outside the block so the image is rendered.  Clicking the rendered
    // placeholder must park the cursor at the end of `![alt](url)`.
    let src = "Before.\n\n![alt](pic.png)\n\nAfter.\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    st.cursor.offset = src.find("Before").unwrap();
    st.update_cursor_block();

    let mut anchor: Option<mouse_ops::DragTarget> = None;
    mouse_ops::apply(&mut st, click(3, 2), &mut anchor, &[], VP, VW);

    let line_end = src.find("![alt](pic.png)").unwrap() + "![alt](pic.png)".len();
    assert_eq!(
        st.cursor.offset, line_end,
        "click on rendered image must park cursor at end of source line",
    );
    assert!(
        st.cursor_block_revealed(),
        "image click must force-reveal the block immediately",
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
fn click_on_wrapped_mermaid_line_lands_on_continuation() {
    // Long mermaid lines wrap visually inside the reveal-painted
    // region.  The click loop must use the raw line's wrap count, not
    // the cached placeholder count, otherwise clicks on the wrap
    // continuation row are attributed to the next raw line.
    let long = "A".repeat(100); // 100 chars; wraps once at VW=80.
    let src = format!("```mermaid\n{long}\nB-->C\n```\n");
    let mut st = state(&src);
    st.mode = Mode::Rendered;
    st.cursor.offset = src.find(&long[..]).unwrap();
    st.update_cursor_block();
    st.cursor_block_entered_at = None;

    // Visual row 0 = "```mermaid"
    // Visual row 1 = first 80 chars of `long`
    // Visual row 2 = wrap continuation (next 20 chars of `long`)
    // Visual row 3 = "B-->C"
    // Click col 5, row 2 → should land on char (80 + 5) = 85 of `long`,
    // which is still 'A'.  More importantly, it must NOT land on
    // "B-->C" or its surroundings.
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    mouse_ops::apply(&mut st, click(5, 2), &mut anchor, &[], VP, VW);

    let long_start = src.find(&long[..]).unwrap();
    let target = long_start + 85;
    assert_eq!(
        st.cursor.offset, target,
        "click on wrap continuation at col 5 must land at char 85 of the long mermaid line",
    );

    // Click on row 3 col 0 should land at the start of "B-->C".
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    mouse_ops::apply(&mut st, click(0, 3), &mut anchor, &[], VP, VW);
    let b_start = src.find("B-->C").unwrap();
    assert_eq!(
        st.cursor.offset, b_start,
        "click on row 3 col 0 must land at start of 'B-->C', not somewhere on the long line",
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
    assert!(
        entered_at.is_some(),
        "entering mermaid arms the reveal timer"
    );

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

// ── Regression: image-block sub-row must not poison the inline-map cache ─────

#[test]
fn click_on_reserved_image_row_does_not_poison_inline_map_cache() {
    // A `Block::ImageBlock` reserves many rendered rows (`image_max_height`)
    // for its single source line, and its byte range can absorb a trailing
    // blank line.  Translating a click on one of the reserved rows used to
    // read the rendered sub-row as a raw source-line index, landing on a
    // phantom empty raw line and caching an empty `InlineColMap` for the
    // buffer line of *whatever content follows the image*
    // (`image_line + sub_idx`).  Painting that content line under a selection
    // then re-queried the cache with the real line text and tripped the
    // char-count assertion in `ParsedDoc::inline_map`.  See the
    // `is_image_block` clamp in `mouse_ops::coord` (the click path that
    // poisoned the cache here); `rendered_view::paint` is independently safe
    // via its `raw_line_idx >= raw_lines.len()` bounds check.
    use edamame::document::Selection;
    use edamame::ui::{RenderedView, RenderedViewState};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let theme = theme();
    let long = "Paragraph with a very long line that keeps going on and on past wrap width here.";
    let src = format!("![Mona](https://example.com/a.png)\n\n{long}\n");
    let mut st = EditorState::new(Buffer::from_str(&src), theme);
    st.mode = Mode::Rendered;

    // Click on a reserved row of the image (row 2, below the placeholder).
    let mut mouse = MouseDispatcher::new();
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    if let Some(a) = mouse.dispatch(click_event(2, 2), area()) {
        mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, VW);
    }

    // Select the whole document so the paragraph below the image paints under
    // the selection overlay, re-querying its inline map.
    let total = st.buffer.len_chars();
    st.selection = Some(Selection {
        anchor: 0,
        active: total,
    });
    st.cursor.offset = 0;

    let backend = TestBackend::new(80, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                cursor_style: theme.status_mode_rendered,
                visual_kind: None,
                drop_indicator: None,
                show_table_buttons: false,
                state: &st,
                theme,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();
}

// ── Cell-constrained table selection ─────────────────────────────────────────
//
// A click-and-drag selection that begins inside a table cell stays confined
// to that cell in both Rendered and Preview modes, spanning the cell's
// wrapped sub-rows as needed.  Preview triple-click selects the whole cell
// (mirroring Rendered mode's `select_line_at_cursor`).

/// Simple 3-column table.  Rendered data row (screen row 3):
/// `│ alpha │ bravo │ charlie │` with pipes at cols 0, 8, 16, 26.
/// Raw data row starts at byte 28; cell 1 (" bravo ") is bytes 37..44.
const CELL_TABLE: &str = "| a | b | c |\n|---|---|---|\n| alpha | bravo | charlie |\n";

/// Long sentence that forces the description column of `wrapped_table()` to
/// wrap onto two rendered sub-rows at the default 80-col viewport.
const LONG_CELL: &str = "the quick brown fox jumps over the lazy dog and keeps running through the quiet forest until dusk";

/// Two-column table whose second cell wraps.  Rendered rows: 0=top border,
/// 1=header, 2=thick separator, 3=data sub 0, 4=data sub 1, 5=bottom border;
/// pipes at cols 0, 6, 79 so the description cell's content band is [8, 78).
fn wrapped_table() -> String {
    format!("| k | description |\n|---|---|\n| x | {LONG_CELL} |\n")
}

fn drag(col: u16, row: u16) -> MouseAction {
    MouseAction::Drag { col, row }
}

fn triple_click_at(st: &mut EditorState, col: u16, row: u16) {
    let mut mouse = MouseDispatcher::new();
    let mut anchor: Option<mouse_ops::DragTarget> = None;
    for _ in 0..3 {
        if let Some(a) = mouse.dispatch(click_event(col, row), area()) {
            mouse_ops::apply(st, a, &mut anchor, &[], VP, VW);
        }
        if let Some(a) = mouse.dispatch(up_event(col, row), area()) {
            mouse_ops::apply(st, a, &mut anchor, &[], VP, VW);
        }
    }
}

#[test]
fn rendered_drag_from_cell_clamps_to_cell() {
    let mut st = state(CELL_TABLE);
    st.mode = Mode::Rendered;
    let mut anchor: Option<mouse_ops::DragTarget> = None;

    // Click the 'r' of "bravo" (rendered col 11 on the data row).
    mouse_ops::apply(&mut st, click(11, 3), &mut anchor, &[], VP, VW);
    assert_eq!(st.cursor.offset, 39, "anchor on the 'r' of bravo");

    // Drag into "charlie"'s columns — active clamps to the end of bravo's
    // cell content (byte 44, just before the closing pipe).
    mouse_ops::apply(&mut st, drag(22, 3), &mut anchor, &[], VP, VW);
    let sel = st.selection.expect("drag sets selection");
    let (s, e) = sel.range();
    assert_eq!(
        st.buffer.slice_to_string(s, e),
        "ravo ",
        "selection must stop at bravo's cell boundary"
    );

    // Drag up onto the header row — active clamps to the start of the cell.
    mouse_ops::apply(&mut st, drag(2, 1), &mut anchor, &[], VP, VW);
    let sel = st.selection.expect("selection persists");
    let (s, e) = sel.range();
    assert_eq!(
        st.buffer.slice_to_string(s, e),
        " b",
        "upward drag must clamp to the cell's content start"
    );

    // Drag below the table — active clamps to the cell's content end.
    mouse_ops::apply(&mut st, drag(5, 10), &mut anchor, &[], VP, VW);
    let sel = st.selection.expect("selection persists");
    let (s, e) = sel.range();
    assert_eq!(
        st.buffer.slice_to_string(s, e),
        "ravo ",
        "drag off the table must clamp to the cell's content end"
    );
}

#[test]
fn rendered_drag_across_wrapped_cell_sub_rows() {
    let src = wrapped_table();
    let mut st = state(&src);
    st.mode = Mode::Rendered;
    let mut anchor: Option<mouse_ops::DragTarget> = None;

    let cell_start = src.find(LONG_CELL).expect("cell text in source");
    // First char of the second wrap chunk ("the quiet forest until dusk").
    let chunk1 = LONG_CELL.find("the quiet").expect("wrap point");

    // Click content col 2 of the cell on sub-row 0 (rendered col 10 = 'e'
    // of "the"), then drag to the same content col on sub-row 1.
    mouse_ops::apply(&mut st, click(10, 3), &mut anchor, &[], VP, VW);
    assert_eq!(
        st.cursor.offset,
        cell_start + 2,
        "click lands on the first chunk's char 2"
    );
    mouse_ops::apply(&mut st, drag(10, 4), &mut anchor, &[], VP, VW);
    let sel = st.selection.expect("drag sets selection");
    let (s, e) = sel.range();
    assert_eq!(
        st.buffer.slice_to_string(s, e),
        &LONG_CELL[2..chunk1 + 2],
        "drag onto the wrapped sub-row must map into the second chunk"
    );

    // Drag past the cell's right edge on sub-row 1 — clamps inside the cell.
    mouse_ops::apply(&mut st, drag(79, 4), &mut anchor, &[], VP, VW);
    let sel = st.selection.expect("selection persists");
    let (s, e) = sel.range();
    let text = st.buffer.slice_to_string(s, e);
    assert!(
        !text.contains('|') && !text.contains('\n'),
        "selection escaped the cell: {text:?}"
    );
}

#[test]
fn preview_triple_click_selects_whole_wrapped_cell() {
    let src = wrapped_table();
    let mut st = state(&src);
    assert_eq!(st.mode, Mode::Preview);
    triple_click_at(&mut st, 20, 3);

    let vs = st.visual_selection.expect("triple-click sets selection");
    let band = vs.band.expect("cell band recorded");
    assert_eq!(band.lines, (3, 4), "band covers both wrapped sub-rows");
    assert_eq!(band.cols, (8, 78), "band covers the cell's content area");
    assert_eq!(vs.anchor, (3, 8));
    assert_eq!(
        vs.active,
        (4, 8 + "the quiet forest until dusk".chars().count())
    );

    let copied = mouse_ops::visual_selection_to_rendered_text(vs, &st.parsed.lines);
    assert_eq!(
        copied, LONG_CELL,
        "copy must reconstruct the full cell text without borders or padding"
    );
}

#[test]
fn preview_triple_click_outside_table_selects_line() {
    let mut st = state("first line\nsecond line\n");
    assert_eq!(st.mode, Mode::Preview);
    triple_click_at(&mut st, 3, 1);

    let vs = st.visual_selection.expect("triple-click sets selection");
    assert_eq!(vs.band, None, "non-table lines select without a band");
    assert_eq!(vs.anchor, (1, 0));
    assert_eq!(vs.active, (1, "second line".len()));
}

#[test]
fn preview_triple_click_on_table_border_selects_line() {
    let mut st = state(CELL_TABLE);
    triple_click_at(&mut st, 3, 0); // top border row
    let vs = st.visual_selection.expect("triple-click sets selection");
    assert_eq!(vs.band, None, "border rows keep full-line selection");
    assert_eq!(vs.anchor.0, 0);
}

#[test]
fn preview_drag_from_cell_constrained_to_band() {
    let src = wrapped_table();
    let mut st = state(&src);
    let mut anchor: Option<mouse_ops::DragTarget> = None;

    // Click inside the description cell on sub-row 0.
    mouse_ops::apply(&mut st, click(10, 3), &mut anchor, &[], VP, VW);
    let vs = st.visual_selection.expect("click seeds selection");
    let band = vs.band.expect("click in cell records band");
    assert_eq!(band.lines, (3, 4));
    assert_eq!(band.cols, (8, 78));

    // Drag onto the bottom border, left of the cell — both axes clamp.
    mouse_ops::apply(&mut st, drag(2, 5), &mut anchor, &[], VP, VW);
    let vs = st.visual_selection.expect("selection persists");
    assert_eq!(vs.active, (4, 8), "drag clamps to the band's corner");

    // Drag above the table and into the key column — clamps to band start.
    mouse_ops::apply(&mut st, drag(1, 1), &mut anchor, &[], VP, VW);
    let vs = st.visual_selection.expect("selection persists");
    assert_eq!(vs.active, (3, 8));
}

#[test]
fn preview_drag_in_first_cell_does_not_leak_into_neighbor() {
    let mut st = state(CELL_TABLE);
    let mut anchor: Option<mouse_ops::DragTarget> = None;

    // Click on "alpha" (cell 0) and drag right through "bravo".
    mouse_ops::apply(&mut st, click(3, 3), &mut anchor, &[], VP, VW);
    mouse_ops::apply(&mut st, drag(13, 3), &mut anchor, &[], VP, VW);
    let vs = st.visual_selection.expect("selection persists");
    let copied = mouse_ops::visual_selection_to_rendered_text(vs, &st.parsed.lines);
    assert!(
        copied.starts_with("lpha") && !copied.contains("bravo") && !copied.contains('│'),
        "selection leaked out of cell 0: {copied:?}"
    );
}

#[test]
fn preview_triple_click_on_cell_with_escaped_pipe() {
    let mut st = state("| a | b |\n|---|---|\n| x\\|y | z |\n");
    triple_click_at(&mut st, 3, 3);
    let vs = st.visual_selection.expect("triple-click sets selection");
    assert!(vs.band.is_some(), "escaped-pipe cell still maps to a band");
    let copied = mouse_ops::visual_selection_to_rendered_text(vs, &st.parsed.lines);
    assert_eq!(
        copied, "x|y",
        "escaped pipe renders literally inside the cell"
    );
}

#[test]
fn preview_triple_click_in_empty_cell_copies_nothing() {
    let mut st = state("| head | b |\n|---|---|\n|  | z |\n");
    triple_click_at(&mut st, 3, 3);
    if let Some(vs) = st.visual_selection {
        let copied = mouse_ops::visual_selection_to_rendered_text(vs, &st.parsed.lines);
        assert_eq!(copied.trim(), "", "empty cell must copy as empty");
    }
}

#[test]
fn preview_triple_click_in_header_cell_selects_header_content() {
    let mut st = state(CELL_TABLE);
    triple_click_at(&mut st, 2, 1); // header row, cell 0 ("a")
    let vs = st.visual_selection.expect("triple-click sets selection");
    assert!(vs.band.is_some(), "header cells band like data cells");
    let copied = mouse_ops::visual_selection_to_rendered_text(vs, &st.parsed.lines);
    assert_eq!(copied, "a");
}

// ── List marker-aware click mapping ──────────────────────────────────────

#[test]
fn click_on_ordered_list_content_lands_on_clicked_char() {
    // A 10-item ordered list right-aligns its numbers in a 2-digit slot, so
    // row 0 renders as ` 1. aaa` — one cell wider than the raw `1. aaa`.
    // The click map must absorb that pad instead of falling back to a 1:1
    // column mapping (which landed one char to the right).
    let doc: String = (1..=10).map(|i| format!("{i}. item\n")).collect();
    let mut st = state(&doc);
    st.mode = Mode::Rendered;
    let mut anchor: Option<mouse_ops::DragTarget> = None;

    // Cursor rests on row 0, so row 2 renders normally (a click on the
    // cursor's own revealed line maps 1:1 against the raw text instead).
    // Click the first content char of row 2 — rendered ` 3. item`, raw
    // `3. item` starting at buffer offset 16, so 'i' is offset 19.
    mouse_ops::apply(&mut st, click(4, 2), &mut anchor, &[], VP, VW);
    assert_eq!(
        st.cursor.offset, 19,
        "click on 'i' of ' 3. item' must land on raw col 3 of its line"
    );
}

#[test]
fn click_on_nested_list_content_accounts_for_indent_difference() {
    // The source nests with 2 spaces but the renderer indents children by
    // INDENT_WIDTH (4), so the rendered marker is 2 cells wider than the
    // raw one.  A click on the nested item's text used to land 2 chars off.
    let mut st = state("- top\n  - inner\n");
    st.mode = Mode::Rendered;
    let mut anchor: Option<mouse_ops::DragTarget> = None;

    // Row 1 renders as `    • inner`; click the 'i' at rendered col 6.
    mouse_ops::apply(&mut st, click(6, 1), &mut anchor, &[], VP, VW);
    // Raw line is `  - inner` starting at buffer offset 6; 'i' sits at
    // raw col 4 → offset 10.
    assert_eq!(
        st.cursor.offset, 10,
        "click on 'i' of the nested item must land on its raw position"
    );
}

#[test]
fn click_on_nested_task_checkbox_toggles_it() {
    // Nested task item: rendered `    • [ ] sub` vs raw `  - [ ] sub`.
    // The checkbox hitbox is computed in raw bytes, so the rendered→raw
    // click mapping must right-align the marker cells for the `]` of the
    // checkbox to stay inside the hitbox.
    let mut st = state("- top\n  - [ ] sub\n");
    st.mode = Mode::Rendered;
    st.cursor.offset = 0;
    let mut anchor: Option<mouse_ops::DragTarget> = None;

    // Click the `]` at rendered col 8 of row 1.
    mouse_ops::apply(&mut st, click(8, 1), &mut anchor, &[], VP, VW);
    assert!(
        st.contents().contains("  - [x] sub"),
        "click on the nested checkbox must toggle it, got: {:?}",
        st.contents()
    );
    assert_eq!(
        st.cursor.offset, 0,
        "checkbox toggle must not move the cursor"
    );
}

#[test]
fn click_on_bold_text_inside_ordered_item_uses_inline_map() {
    // Inline markup inside a list item collapses (`**bold**` → `bold`), so
    // content clicks must compose the marker shift with the inline map.
    let mut st = state("para\n\n1. **bold** tail\n");
    st.mode = Mode::Rendered;
    let mut anchor: Option<mouse_ops::DragTarget> = None;

    // Cursor rests on "para" (row 0), so the list row 2 renders normally.
    // Rendered `1. bold tail`; click the 't' of "tail" at rendered col 8.
    mouse_ops::apply(&mut st, click(8, 2), &mut anchor, &[], VP, VW);
    // Raw line `1. **bold** tail` starts at offset 6 — 't' of "tail" sits
    // at raw col 12 → offset 18.
    assert_eq!(
        st.cursor.offset, 18,
        "click on 't' of 'tail' must skip the raw `**` markers"
    );
}

/// Regression: a *wrapped* raw-revealed list item.  `RenderedView` paints
/// the raw source through `render_line`, which derives a hanging indent
/// from the leading `- ` marker — so continuation rows sit two cells in and
/// wrap against a narrower budget.  The mouse hit-test used to lay the same
/// raw line out with indent 0, so every row past the first mapped clicks a
/// couple of chars off, and the drift compounded with each wrap.
///
/// Asserted the only way that can't drift: render the view for real, then
/// check that the glyph on screen at `(col, row)` is the glyph at the offset
/// the click maps to.
#[test]
fn clicks_on_a_wrapped_revealed_list_item_land_under_the_pointer() {
    use edamame::ui::{RenderedView, RenderedViewState};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    const W: u16 = 40;
    const H: u16 = 12;
    let src = "- Alpha item\n\n- Beta item\n\n- Gamma `code` tail that keeps \
               going on and on until it wraps onto another visual row with \
               more `code` here\n";

    let build = || {
        let mut st = state(src);
        st.mode = Mode::Rendered;
        st.viewport_width = W as usize;
        st.cursor.offset = src.find("Gamma").unwrap();
        st.update_cursor_block();
        st.cursor_block_entered_at = None; // reveal returns true immediately
        st
    };

    // Paint the view so we know exactly what the user is looking at.
    let st = build();
    let theme = theme();
    let mut terminal = Terminal::new(TestBackend::new(W, H)).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                cursor_style: theme.status_mode_rendered,
                visual_kind: None,
                drop_indicator: None,
                show_table_buttons: false,
                state: &st,
                theme,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();
    let buf = terminal.backend().buffer().clone();

    // Rows 4.. are the revealed raw item; row 4 is flush, later rows are
    // hanging-indented.  Only non-blank cells are checked — trailing padding
    // legitimately clamps to the line end.
    let mut checked = 0;
    for row in 4..H {
        for col in 0..W {
            let painted = buf
                .cell((col, row))
                .and_then(|c| c.symbol().chars().next())
                .unwrap_or(' ');
            if painted == ' ' {
                continue;
            }
            let mut st = build();
            let mut anchor: Option<mouse_ops::DragTarget> = None;
            let mut mouse = MouseDispatcher::new();
            if let Some(a) = mouse.dispatch(click_event(col, row), area()) {
                mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, W as usize);
            }
            let landed = st.contents().chars().nth(st.cursor.offset);
            assert_eq!(
                landed,
                Some(painted),
                "click at (col {col}, row {row}) shows {painted:?} but landed on \
                 {landed:?} (offset {})",
                st.cursor.offset,
            );
            checked += 1;
        }
    }
    assert!(
        checked > 60,
        "expected a wrapped item to check, got {checked}"
    );
}

/// Regression: the same wrapped-reveal mapping in a viewport so narrow that
/// the marker is as wide as the terminal.  `render_line` and
/// `visual_rows_of_chars` both drop the hanging indent when
/// `indent + 1 >= width` and lay the line out flat; the hit-test used to
/// report the *unclamped* marker width anyway, which pushed every column of
/// every continuation row into `char_idx_at_cell_col`'s forbidden-indent zone
/// and collapsed the whole row onto its first character.
///
/// `- [ ] ` is a 6-cell indent, so a 7-cell viewport trips the fallback.
#[test]
fn clicks_on_a_revealed_item_wider_than_the_viewport_land_under_the_pointer() {
    use edamame::ui::{RenderedView, RenderedViewState};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    const W: u16 = 7;
    const H: u16 = 12;
    let src = "- [ ] abcdefghijklmnopqrstuvwxyz\n";

    let build = || {
        let mut st = state(src);
        st.mode = Mode::Rendered;
        st.viewport_width = W as usize;
        st.cursor.offset = src.find("abc").unwrap();
        st.update_cursor_block();
        st.cursor_block_entered_at = None; // reveal returns true immediately
        st
    };

    let st = build();
    let theme = theme();
    let mut terminal = Terminal::new(TestBackend::new(W, H)).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                cursor_style: theme.status_mode_rendered,
                visual_kind: None,
                drop_indicator: None,
                show_table_buttons: false,
                state: &st,
                theme,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();
    let buf = terminal.backend().buffer().clone();

    // Row 0 is skipped: it holds the `- [ ] ` marker, and a click on the
    // checkbox glyph deliberately toggles instead of moving the cursor
    // (`mouse_ops::checkbox` short-circuits ahead of cursor placement).
    let mut checked = 0;
    for row in 1..H {
        for col in 0..W {
            let painted = buf
                .cell((col, row))
                .and_then(|c| c.symbol().chars().next())
                .unwrap_or(' ');
            if painted == ' ' {
                continue;
            }
            let mut st = build();
            let mut anchor: Option<mouse_ops::DragTarget> = None;
            let mut mouse = MouseDispatcher::new();
            if let Some(a) = mouse.dispatch(click_event(col, row), area()) {
                mouse_ops::apply(&mut st, a, &mut anchor, &[], VP, W as usize);
            }
            let landed = st.contents().chars().nth(st.cursor.offset);
            assert_eq!(
                landed,
                Some(painted),
                "click at (col {col}, row {row}) shows {painted:?} but landed on \
                 {landed:?} (offset {})",
                st.cursor.offset,
            );
            checked += 1;
        }
    }
    assert!(
        checked > 20,
        "expected several wrapped rows to check, got {checked}"
    );
}
