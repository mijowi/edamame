//! Integration tests for Phase 2 — table editing.
//!
//! These tests construct an `EditorState` around a buffer that contains a GFM
//! table, dispatch a sequence of `Action`s via `edit_ops::apply`, and assert
//! on the resulting buffer content and cursor position.
//!
//! The tests cover:
//! - cell navigation via `Tab`, `Shift+Tab`, `Enter`, and arrow keys crossing
//!   cell boundaries,
//! - structure edits (insert/delete/swap rows and columns) as single-step
//!   undoable operations,
//! - guardrails: no deletion of header/alignment/last column, adjacent-only
//!   row/column swaps.

use edamame::config::{Action, Theme};
use edamame::document::Buffer;
use edamame::editor::{edit_ops, EditorState, Mode};
use edamame::ui::table_view;
use ratatui::layout::Rect;

const VP: usize = 40;
const VW: usize = 80;

fn theme() -> &'static Theme {
    Box::leak(Box::new(Theme::default()))
}

fn apply(st: &mut EditorState, action: Action) -> bool {
    edit_ops::apply(st, action, VP, VW)
}

fn apply_all(st: &mut EditorState, actions: &[Action]) {
    for a in actions {
        apply(st, a.clone());
    }
}

/// Construct a Rendered-mode editor around `text` with the cursor positioned
/// at the char offset of `needle`'s first occurrence in `text`.  Panics if
/// `needle` is not found.
fn editor_at(text: &str, needle: &str) -> EditorState {
    let mut st = EditorState::new(Buffer::from_str(text), theme());
    st.mode = Mode::Rendered;
    let byte = text.find(needle).expect("needle not found");
    let char_off = st.buffer.rope().byte_to_char(byte);
    st.cursor.offset = char_off;
    st.update_cursor_block();
    st
}

// ── Setup sanity ─────────────────────────────────────────────────────────────

#[test]
fn editor_at_places_cursor_on_needle() {
    let src = "| a | b |\n|---|---|\n| 1 | 22 |\n";
    let st = editor_at(src, "22");
    let byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    assert_eq!(&src[byte..byte + 2], "22");
}

// ── Tab / Shift+Tab navigation ───────────────────────────────────────────────

#[test]
fn tab_advances_to_next_cell_within_row() {
    let src = "| a | b | c |\n|---|---|---|\n| 11 | 22 | 33 |\n";
    let mut st = editor_at(src, "11");
    apply(&mut st, Action::InsertTab);

    // Cursor should have jumped to the end-of-content of cell (2,1) = just
    // past "22", on its trailing pad space — so the user can immediately
    // start typing to append.
    let byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    let contents = st.contents();
    assert_eq!(&contents[byte - 2..byte], "22");
    assert_eq!(&contents[byte..byte + 1], " ");
}

#[test]
fn tab_at_end_of_row_wraps_to_next_row() {
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n| 33 | 44 |\n";
    let mut st = editor_at(src, "22");
    apply(&mut st, Action::InsertTab);

    // Wraps to first cell of next row; cursor lands at cell-end of "33".
    let byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    let contents = st.contents();
    assert_eq!(&contents[byte - 2..byte], "33");
    assert_eq!(&contents[byte..byte + 1], " ");
}

#[test]
fn tab_at_end_of_last_row_appends_new_row() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = editor_at(src, "2");
    apply(&mut st, Action::InsertTab);

    // Buffer should now contain an additional empty row.
    let contents = st.contents();
    assert!(
        contents.lines().filter(|l| l.starts_with('|')).count() >= 4,
        "expected a 4th table line after Tab-creates-row; got:\n{contents}"
    );
    // The buffer must still be a valid table with 2 columns.
    assert!(contents.contains("|   |   |"));
}

#[test]
fn shift_tab_retreats_to_previous_cell() {
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n";
    let mut st = editor_at(src, "22");
    apply(&mut st, Action::TablePrevCell);

    // Lands at cell-end of "11".
    let byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    let contents = st.contents();
    assert_eq!(&contents[byte - 2..byte], "11");
    assert_eq!(&contents[byte..byte + 1], " ");
}

#[test]
fn shift_tab_at_start_of_row_wraps_to_previous_row() {
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n| 33 | 44 |\n";
    let mut st = editor_at(src, "33");
    apply(&mut st, Action::TablePrevCell);

    // Lands at cell-end of "22" (last cell of previous row).
    let byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    let contents = st.contents();
    assert_eq!(&contents[byte - 2..byte], "22");
    assert_eq!(&contents[byte..byte + 1], " ");
}

// ── Enter / TableNextRow ─────────────────────────────────────────────────────

#[test]
fn enter_inside_table_moves_down_a_row_preserving_column() {
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n| 33 | 44 |\n";
    let mut st = editor_at(src, "22");
    apply(&mut st, Action::Newline);

    // Cursor should now land at cell-end of col 1 on the row below ("44").
    let byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    let contents = st.contents();
    assert_eq!(&contents[byte - 2..byte], "44");
    assert_eq!(&contents[byte..byte + 1], " ");
    // Enter inside a table must NOT insert a literal newline.
    assert_eq!(contents.as_str(), src);
}

#[test]
fn enter_at_last_row_appends_new_row() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = editor_at(src, "2");
    apply(&mut st, Action::Newline);

    // Buffer gains a new row.
    let contents = st.contents();
    assert!(contents.contains("|   |   |"));
    // The new row's cursor should be on its empty cell 1.
    let new_rows: Vec<&str> = contents.lines().filter(|l| l.starts_with('|')).collect();
    assert_eq!(new_rows.len(), 4);
}

// ── Arrow-key boundary crossing ──────────────────────────────────────────────

#[test]
fn move_right_skips_pipe_separator_in_table() {
    // Cursor placed on `|` between cells should auto-advance past it.
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = editor_at(src, "1");
    // Move right from cell 0's content until we cross the `|` into cell 1.
    // Starting at "1", Right should land on ' ' (still in col 0), another
    // Right on '|', and `skip_table_pipe` should push us into cell 1's "2".
    for _ in 0..3 {
        apply(&mut st, Action::MoveRight);
    }
    // After 3 right-presses from "1", we expect to have crossed the pipe and
    // be at or past the start of cell 1's content.
    let byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    let contents = st.contents();
    let col1_content = contents.find(" 2 ").unwrap() + 1;
    assert!(
        byte >= col1_content,
        "expected cursor past the `|` into cell 1 (>= {col1_content}), got {byte}"
    );
}

#[test]
fn move_left_skips_pipe_separator_in_table() {
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n";
    let mut st = editor_at(src, "22");
    // Move left from "22"'s first char until we cross the `|` into cell 0.
    for _ in 0..3 {
        apply(&mut st, Action::MoveLeft);
    }
    // The cursor should now be within the "11" cell on the data row — i.e.
    // somewhere inside the text "11".  Locate the data row's cell-0 content
    // range and assert the cursor byte falls within it.
    let byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    let contents = st.contents();
    let cell_0_data = contents.find("| 11").unwrap() + 1; // char after the leading `|`
    let cell_1_pipe = contents.find("| 22").unwrap();
    assert!(
        byte >= cell_0_data && byte < cell_1_pipe,
        "expected cursor within cell 0 of the data row (>= {cell_0_data}, < {cell_1_pipe}), got {byte}"
    );
}

#[test]
fn move_right_outside_table_does_not_skip_pipes() {
    // Literal `|` outside a table must not be skipped by arrow-key movement.
    let src = "abc | def\n";
    let mut st = EditorState::new(Buffer::from_str(src), theme());
    st.mode = Mode::Rendered;
    // Move right across the `|`; it should take a single step.
    st.cursor.offset = 4; // at '|'
    let before = st.cursor.offset;
    apply(&mut st, Action::MoveRight);
    assert_eq!(st.cursor.offset, before + 1);
}

// ── Row structure edits ──────────────────────────────────────────────────────

#[test]
fn alt_down_swaps_current_row_with_row_below() {
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n| 33 | 44 |\n";
    let mut st = editor_at(src, "11");
    apply(&mut st, Action::TableMoveRowDown);

    let contents = st.contents();
    let expected = "| a | b |\n|---|---|\n| 33 | 44 |\n| 11 | 22 |\n";
    assert_eq!(contents, expected);
}

#[test]
fn alt_up_swaps_current_row_with_row_above() {
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n| 33 | 44 |\n";
    let mut st = editor_at(src, "44");
    apply(&mut st, Action::TableMoveRowUp);

    let contents = st.contents();
    let expected = "| a | b |\n|---|---|\n| 33 | 44 |\n| 11 | 22 |\n";
    assert_eq!(contents, expected);
}

#[test]
fn alt_up_on_first_data_row_is_noop() {
    // Row 2 is the first data row. Moving it "up" would put it above the
    // alignment row, which is not allowed — the operation must be a no-op.
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n| 33 | 44 |\n";
    let mut st = editor_at(src, "11");
    apply(&mut st, Action::TableMoveRowUp);
    assert_eq!(st.contents(), src);
}

#[test]
fn alt_shift_down_inserts_new_row_below() {
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n";
    let mut st = editor_at(src, "11");
    apply(&mut st, Action::TableInsertRowBelow);

    let contents = st.contents();
    assert!(contents.contains("| 11 | 22 |\n|   |   |\n"));
}

#[test]
fn alt_shift_up_inserts_new_row_above() {
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n| 33 | 44 |\n";
    let mut st = editor_at(src, "33");
    apply(&mut st, Action::TableInsertRowAbove);

    let contents = st.contents();
    // New empty row should be between the "11" row and the "33" row.
    assert!(contents.contains("| 11 | 22 |\n|   |   |\n| 33 | 44 |\n"));
}

#[test]
fn alt_backspace_deletes_current_row() {
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n| 33 | 44 |\n";
    let mut st = editor_at(src, "11");
    apply(&mut st, Action::TableDeleteRow);

    let contents = st.contents();
    let expected = "| a | b |\n|---|---|\n| 33 | 44 |\n";
    assert_eq!(contents, expected);
}

#[test]
fn alt_backspace_refuses_header_and_alignment() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    // Place cursor on header row "a".
    let mut st = editor_at(src, "a");
    apply(&mut st, Action::TableDeleteRow);
    assert_eq!(st.contents(), src);

    // Place cursor on alignment row.
    let mut st = editor_at(src, "---");
    apply(&mut st, Action::TableDeleteRow);
    assert_eq!(st.contents(), src);
}

// ── Column structure edits ───────────────────────────────────────────────────

#[test]
fn alt_right_swaps_with_column_to_the_right() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = editor_at(src, "a");
    apply(&mut st, Action::TableMoveColumnRight);

    // Header swapped: "| b | a |"
    let contents = st.contents();
    assert!(contents.starts_with("| b | a |"));
}

#[test]
fn alt_left_swaps_with_column_to_the_left() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = editor_at(src, "b");
    apply(&mut st, Action::TableMoveColumnLeft);

    let contents = st.contents();
    assert!(contents.starts_with("| b | a |"));
}

#[test]
fn alt_shift_right_inserts_new_column_on_the_right() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = editor_at(src, "a");
    apply(&mut st, Action::TableInsertColumnRight);

    // Table should have 3 columns.  Expect the inserted column to appear
    // between the existing two.
    let contents = st.contents();
    let first_line = contents.lines().next().unwrap();
    let pipe_count = first_line.matches('|').count();
    assert_eq!(pipe_count, 4, "expected 4 `|`s in header of 3-col table");
}

#[test]
fn alt_shift_left_inserts_new_column_on_the_left() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = editor_at(src, "b");
    apply(&mut st, Action::TableInsertColumnLeft);

    let contents = st.contents();
    let first_line = contents.lines().next().unwrap();
    assert_eq!(first_line.matches('|').count(), 4);
}

#[test]
fn alt_shift_backspace_deletes_column() {
    let src = "| a | b | c |\n|---|---|---|\n| 1 | 2 | 3 |\n";
    let mut st = editor_at(src, "b");
    apply(&mut st, Action::TableDeleteColumn);

    // Column 1 removed; table has 2 cols.
    let contents = st.contents();
    let first_line = contents.lines().next().unwrap();
    assert_eq!(first_line.matches('|').count(), 3);
}

#[test]
fn alt_shift_backspace_refuses_to_delete_last_column() {
    // One-column table must keep its column.
    let src = "| a |\n|---|\n| 1 |\n";
    let mut st = editor_at(src, "a");
    apply(&mut st, Action::TableDeleteColumn);
    assert_eq!(st.contents(), src);
}

// ── Atomic-undo verification ─────────────────────────────────────────────────

#[test]
fn ctrl_z_undoes_structure_edit_in_one_step() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut st = editor_at(src, "1");
    apply(&mut st, Action::TableMoveRowDown);
    assert_ne!(st.contents(), src);

    apply(&mut st, Action::Undo);
    assert_eq!(st.contents(), src);
}

#[test]
fn ctrl_z_undoes_row_insertion_in_one_step() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = editor_at(src, "1");
    apply(&mut st, Action::TableInsertRowBelow);
    assert_ne!(st.contents(), src);

    apply(&mut st, Action::Undo);
    assert_eq!(st.contents(), src);
}

#[test]
fn ctrl_z_undoes_column_deletion_in_one_step() {
    let src = "| a | b | c |\n|---|---|---|\n| 1 | 2 | 3 |\n";
    let mut st = editor_at(src, "b");
    apply(&mut st, Action::TableDeleteColumn);
    assert_ne!(st.contents(), src);

    apply(&mut st, Action::Undo);
    assert_eq!(st.contents(), src);
}

// ── Guardrails outside tables ────────────────────────────────────────────────

#[test]
fn table_actions_are_noop_outside_table() {
    let src = "# Title\n\nJust a paragraph.\n";
    let mut st = editor_at(src, "Just");
    apply_all(
        &mut st,
        &[
            Action::TableNextCell,
            Action::TablePrevCell,
            Action::TableMoveRowUp,
            Action::TableInsertRowBelow,
            Action::TableDeleteRow,
            Action::TableDeleteColumn,
        ],
    );
    assert_eq!(st.contents(), src);
}

#[test]
fn tab_outside_table_inserts_spaces() {
    let src = "hello\n";
    let mut st = editor_at(src, "hello");
    apply(&mut st, Action::InsertTab);
    assert!(st.contents().contains("    hello"));
}

// ── `|` escaping inside cells ───────────────────────────────────────────────

#[test]
fn pipe_inside_cell_is_escaped() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = editor_at(src, "1");
    // Simulate user typing `|`: InsertChar('|').  Cursor lands at the
    // start of "1", so the escaped pipe is inserted immediately before.
    apply(&mut st, Action::InsertChar('|'));

    let contents = st.contents();
    assert!(
        contents.contains("| \\|1 |"),
        "expected escaped pipe, got: {contents}"
    );
    // Most importantly: the table still has exactly 3 unescaped `|`s per row.
    // Count unescaped pipes in the first data row.
    let data_row = contents
        .lines()
        .find(|l| l.contains("1"))
        .expect("data row");
    let mut unescaped = 0;
    let b = data_row.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'|' && (i == 0 || b[i - 1] != b'\\') {
            unescaped += 1;
        }
    }
    assert_eq!(unescaped, 3, "row should still have 3 unescaped `|`s");
}

#[test]
fn pipe_outside_table_is_literal() {
    let src = "hello world\n";
    let mut st = editor_at(src, "hello");
    apply(&mut st, Action::InsertChar('|'));
    assert!(st.contents().contains("|hello"));
    // Must not be escaped outside a table.
    assert!(!st.contents().contains("\\|"));
}

// ── Shift+Enter inserts <br> inside a cell ──────────────────────────────────

#[test]
fn shift_enter_inserts_br_inside_cell() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = editor_at(src, "1");
    apply(&mut st, Action::TableInsertBreak);

    let contents = st.contents();
    // Cursor starts at "1"; `<br>` is inserted just before it.
    assert!(contents.contains("| <br>1 |"));
    // Must NOT have inserted a literal newline that would break the row.
    let data_rows: Vec<&str> = contents
        .lines()
        .filter(|l| l.starts_with('|') && !l.starts_with("|-"))
        .collect();
    assert_eq!(data_rows.len(), 2); // still only one header + one data row
}

#[test]
fn shift_enter_outside_table_inserts_newline() {
    let src = "hello\n";
    let mut st = editor_at(src, "hello");
    apply(&mut st, Action::TableInsertBreak);
    assert!(st.contents().contains("\nhello"));
}

// ── Arrow-key row navigation skips the alignment row ───────────────────────

/// MoveDown from the header row must skip the alignment row `|---|---|` and
/// land on the first data row.  The alignment row is a structural artefact
/// and is never a navigation target.
#[test]
fn move_down_from_header_skips_alignment_row() {
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n";
    let mut st = editor_at(src, "a");
    apply(&mut st, Action::MoveDown);

    // Cursor should now be on the first data row (containing "11"), not on
    // the alignment row.
    let (line, _) = st.cursor.line_col(&st.buffer);
    let line_text = st.buffer.line(line).unwrap_or_default();
    assert!(
        line_text.contains("11"),
        "expected cursor on data row, got line {line}: {line_text:?}"
    );
}

/// MoveUp from the first data row must skip the alignment row and land on
/// the header row.
#[test]
fn move_up_from_first_data_row_skips_alignment_row() {
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n";
    let mut st = editor_at(src, "11");
    apply(&mut st, Action::MoveUp);

    let (line, _) = st.cursor.line_col(&st.buffer);
    let line_text = st.buffer.line(line).unwrap_or_default();
    assert!(
        line_text.contains("| a "),
        "expected cursor on header row, got line {line}: {line_text:?}"
    );
}

// ── Vertical cell navigation lands at cell end (not preserved offset) ──────

/// Moving Down between two data rows must land the cursor at the end of the
/// destination cell's content, not at the same horizontal offset.
#[test]
fn move_down_between_data_rows_lands_at_cell_end() {
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n| 333 | 444 |\n";
    let mut st = editor_at(src, "11");
    apply(&mut st, Action::MoveDown);

    // Cursor should now sit just past the final '3' of cell (3, 0) = "333".
    let byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    let contents = st.contents();
    // The byte immediately before the cursor must be '3'.
    assert_eq!(&contents[byte - 1..byte], "3");
    // And immediately after the cursor must be the trailing space before `|`.
    assert_eq!(&contents[byte..byte + 1], " ");
}

/// MoveUp from a wider cell onto a narrower cell must land at the end of the
/// narrower cell's content.
#[test]
fn move_up_between_data_rows_lands_at_cell_end() {
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n| 333 | 444 |\n";
    let mut st = editor_at(src, "333");
    apply(&mut st, Action::MoveUp);

    let byte = st.buffer.rope().char_to_byte(st.cursor.offset);
    let contents = st.contents();
    // Cursor must be just past "11".
    assert_eq!(&contents[byte - 1..byte], "1");
    assert_eq!(&contents[byte..byte + 1], " ");
}

/// Regression: pasting a multi-byte char (e.g. an emoji) in-line shifts
/// buffer bytes after the insertion point, leaving `parsed.source_map`
/// byte ranges stale until the next parse refresh.  `paste_text` does not
/// flush the parse, so the next render reads the live buffer through stale
/// ranges; `table_view::build_snapshots` used to panic with "byte index N
/// is not a char boundary" when a stale boundary fell inside the new
/// char's UTF-8 sequence.
#[test]
fn build_snapshots_does_not_panic_after_inline_emoji_paste() {
    // Block 0 is a one-byte paragraph; block 2 is a one-byte paragraph
    // whose pre-edit source_map range (3..4) will land mid-emoji once the
    // 4-byte 🥇 is inserted at byte offset 1.
    let src = "a\n\nb";
    let mut st = editor_at(src, "b");
    // Move the cursor to between 'a' and the first newline so the paste
    // lands at byte offset 1 — the byte that becomes the emoji's first
    // byte and pushes block 2's stale range into the middle of it.
    let char_off = st.buffer.rope().byte_to_char(1);
    st.cursor.offset = char_off;
    st.update_cursor_block();
    // `paste_text` is the user-facing path that doesn't flush the
    // parse afterwards (single-char InsertChar does flush via
    // edit_ops::apply, masking the bug).
    edit_ops::paste_text(&mut st, "🥇", VP, VW);
    assert!(
        st.parsed_dirty,
        "in-line paste must leave `parsed_dirty = true`",
    );

    // Pre-fix this panicked at table_view.rs:381 with
    // "byte index N is not a char boundary".
    let _ = table_view::build_snapshots(&st, Rect::new(0, 0, 80, 24), false);
}
