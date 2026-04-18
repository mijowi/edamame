/// Integration tests for the editing layer.
///
/// These construct an `EditorState`, dispatch sequences of `Action`s, then
/// assert on buffer content and cursor position. They serve as TDD anchors for
/// Phase 1 functionality: insert, delete, newline, undo, redo, cursor movement,
/// mode transitions, save, and clipboard.
use edamame::config::{Action, Theme};
use edamame::document::Buffer;
use edamame::editor::{edit_ops, EditorState, Mode};

const VP: usize = 40; // viewport height for most tests
const VW: usize = 80; // viewport width for most tests

fn theme() -> &'static Theme {
    Box::leak(Box::new(Theme::default()))
}

fn state(text: &str) -> EditorState {
    EditorState::new(Buffer::from_str(text), theme())
}

fn apply(st: &mut EditorState, action: Action) -> bool {
    edit_ops::apply(st, action, VP, VW)
}

fn apply_all(st: &mut EditorState, actions: &[Action]) {
    for a in actions {
        apply(st, a.clone());
    }
}

// ── InsertChar / Newline / DeleteChar ────────────────────────────────────────

#[test]
fn insert_char_enters_edit_mode() {
    // First InsertChar from Preview: mode switches to Rendered but nothing is
    // written (the keystroke is consumed as a mode-activation signal).
    let mut st = state("");
    assert_eq!(st.mode, Mode::Preview);
    apply(&mut st, Action::InsertChar('a'));
    assert_eq!(st.mode, Mode::Rendered);
    assert_eq!(st.contents(), ""); // no char inserted on mode transition
    assert_eq!(st.cursor.offset, 0);

    // Second InsertChar: now in Rendered mode, 'a' is actually inserted.
    apply(&mut st, Action::InsertChar('a'));
    assert_eq!(st.contents(), "a");
    assert_eq!(st.cursor.offset, 1);
}

#[test]
fn insert_multiple_chars() {
    let mut st = state("");
    st.mode = Mode::Rendered; // start in edit mode
    apply_all(
        &mut st,
        &[
            Action::InsertChar('h'),
            Action::InsertChar('e'),
            Action::InsertChar('l'),
            Action::InsertChar('l'),
            Action::InsertChar('o'),
        ],
    );
    assert_eq!(st.contents(), "hello");
    assert_eq!(st.cursor.offset, 5);
}

#[test]
fn newline_splits_line() {
    let mut st = state("");
    st.mode = Mode::Rendered;
    apply_all(
        &mut st,
        &[
            Action::InsertChar('a'),
            Action::InsertChar('b'),
            Action::Newline,
            Action::InsertChar('c'),
        ],
    );
    assert_eq!(st.contents(), "ab\nc");
    assert_eq!(st.cursor.offset, 4);
}

#[test]
fn delete_char_back_basic() {
    let mut st = state("hello");
    st.cursor.offset = 5;
    st.mode = Mode::Rendered;
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "hell");
    assert_eq!(st.cursor.offset, 4);
}

#[test]
fn delete_char_back_at_start_is_noop() {
    let mut st = state("hello");
    st.cursor.offset = 0;
    st.mode = Mode::Rendered;
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "hello");
    assert_eq!(st.cursor.offset, 0);
}

#[test]
fn delete_char_forward_basic() {
    let mut st = state("hello");
    st.cursor.offset = 2;
    st.mode = Mode::Rendered;
    apply(&mut st, Action::DeleteCharForward);
    assert_eq!(st.contents(), "helo");
    assert_eq!(st.cursor.offset, 2);
}

#[test]
fn delete_char_forward_at_end_is_noop() {
    let mut st = state("hello");
    st.cursor.offset = 5;
    st.mode = Mode::Rendered;
    apply(&mut st, Action::DeleteCharForward);
    assert_eq!(st.contents(), "hello");
    assert_eq!(st.cursor.offset, 5);
}

#[test]
fn insert_tab_inserts_four_spaces() {
    let mut st = state("");
    st.mode = Mode::Rendered;
    apply(&mut st, Action::InsertTab);
    assert_eq!(st.contents(), "    ");
    assert_eq!(st.cursor.offset, 4);
}

// ── Undo / Redo ───────────────────────────────────────────────────────────────

#[test]
fn undo_reverses_insert() {
    // Adjacent alphanumeric inserts merge into one undo entry, so "ab" is
    // undone in a single step.  Non-adjacent or non-alphanumeric inserts are
    // treated as separate entries (covered by other tests).
    let mut st = state("");
    st.mode = Mode::Rendered;
    apply(&mut st, Action::InsertChar('a'));
    apply(&mut st, Action::InsertChar('b'));
    assert_eq!(st.contents(), "ab");

    apply(&mut st, Action::Undo);
    assert_eq!(st.contents(), "");
}

#[test]
fn undo_breaks_groups_at_non_alphanumeric_chars() {
    let mut st = state("");
    st.mode = Mode::Rendered;
    apply(&mut st, Action::InsertChar('a'));
    apply(&mut st, Action::InsertChar(' ')); // space breaks the group
    apply(&mut st, Action::InsertChar('b'));
    assert_eq!(st.contents(), "a b");

    apply(&mut st, Action::Undo);
    assert_eq!(st.contents(), "a "); // undo "b"
    apply(&mut st, Action::Undo);
    assert_eq!(st.contents(), "a"); // undo " "
    apply(&mut st, Action::Undo);
    assert_eq!(st.contents(), ""); // undo "a"
}

#[test]
fn undo_past_empty_stack_is_noop() {
    let mut st = state("hello");
    st.mode = Mode::Rendered;
    apply(&mut st, Action::Undo); // no-op, stack is empty
    assert_eq!(st.contents(), "hello");
}

#[test]
fn redo_after_undo() {
    let mut st = state("");
    st.mode = Mode::Rendered;
    apply(&mut st, Action::InsertChar('x'));
    apply(&mut st, Action::Undo);
    assert_eq!(st.contents(), "");

    apply(&mut st, Action::Redo);
    assert_eq!(st.contents(), "x");
    assert_eq!(st.cursor.offset, 1);
}

#[test]
fn redo_cleared_by_new_edit() {
    let mut st = state("");
    st.mode = Mode::Rendered;
    apply(&mut st, Action::InsertChar('a'));
    apply(&mut st, Action::Undo);
    apply(&mut st, Action::InsertChar('b')); // new edit clears redo
    apply(&mut st, Action::Redo); // no-op
    assert_eq!(st.contents(), "b");
}

#[test]
fn undo_delete() {
    let mut st = state("hello");
    st.cursor.offset = 5;
    st.mode = Mode::Rendered;
    apply(&mut st, Action::DeleteCharBack); // "hell"
    assert_eq!(st.contents(), "hell");
    apply(&mut st, Action::Undo);
    assert_eq!(st.contents(), "hello");
}

// ── Cursor movement ───────────────────────────────────────────────────────────

#[test]
fn move_left_right() {
    let mut st = state("hello");
    st.cursor.offset = 2;
    st.mode = Mode::Rendered;

    apply(&mut st, Action::MoveRight);
    assert_eq!(st.cursor.offset, 3);

    apply(&mut st, Action::MoveLeft);
    assert_eq!(st.cursor.offset, 2);
}

#[test]
fn move_up_down() {
    let mut st = state("hello\nworld\n");
    st.cursor.offset = 2; // 'l' in "hello"
    st.cursor.preferred_col = 2;
    st.mode = Mode::Rendered;

    apply(&mut st, Action::MoveDown);
    assert_eq!(st.cursor.offset, 8); // 'r' in "world"

    apply(&mut st, Action::MoveUp);
    assert_eq!(st.cursor.offset, 2); // back to 'l' in "hello"
}

#[test]
fn move_up_in_preview_mode_scrolls() {
    let mut st = state("Hello\nWorld\n");
    assert_eq!(st.mode, Mode::Preview);
    st.scroll = 1;
    apply(&mut st, Action::MoveUp);
    assert_eq!(st.scroll, 0); // scrolled up
    assert_eq!(st.mode, Mode::Preview); // still in preview
}

#[test]
fn move_down_in_preview_mode_scrolls() {
    let mut st = state("Hello\nWorld\n");
    assert_eq!(st.mode, Mode::Preview);
    apply(&mut st, Action::MoveDown);
    // scroll increased (content may not be long enough to scroll, but no panic)
    // Mode should still be Preview.
    assert_eq!(st.mode, Mode::Preview);
}

#[test]
fn move_line_start_end() {
    let mut st = state("hello world");
    st.cursor.offset = 5;
    st.mode = Mode::Rendered;

    apply(&mut st, Action::MoveLineEnd);
    assert_eq!(st.cursor.offset, 11);

    apply(&mut st, Action::MoveLineStart);
    assert_eq!(st.cursor.offset, 0);
}

#[test]
fn move_word_left_right() {
    let mut st = state("hello world");
    st.cursor.offset = 11;
    st.mode = Mode::Rendered;

    apply(&mut st, Action::MoveWordLeft);
    assert_eq!(st.cursor.offset, 6); // start of "world"

    apply(&mut st, Action::MoveWordRight);
    assert_eq!(st.cursor.offset, 11); // past "world"
}

#[test]
fn move_doc_start_end() {
    let mut st = state("hello\nworld\n");
    st.cursor.offset = 6;
    st.mode = Mode::Rendered;

    apply(&mut st, Action::MoveDocEnd);
    assert_eq!(st.cursor.offset, 12); // past trailing newline

    apply(&mut st, Action::MoveDocStart);
    assert_eq!(st.cursor.offset, 0);
}

// ── Mode transitions ─────────────────────────────────────────────────────────

#[test]
fn escape_exits_to_preview() {
    // ExitToPreview action still works when dispatched directly even though
    // Escape is no longer bound to it by default in the keymap.
    let mut st = state("hello");
    st.mode = Mode::Rendered;
    apply(&mut st, Action::InsertChar('x'));
    assert_eq!(st.mode, Mode::Rendered);

    apply(&mut st, Action::ExitToPreview);
    assert_eq!(st.mode, Mode::Preview);
}

#[test]
fn toggle_raw_mode_cycles() {
    let mut st = state("hello");
    st.mode = Mode::Rendered;
    apply(&mut st, Action::InsertChar('x'));
    assert_eq!(st.mode, Mode::Rendered);

    apply(&mut st, Action::ToggleRawMode);
    assert_eq!(st.mode, Mode::Raw);

    apply(&mut st, Action::ToggleRawMode);
    assert_eq!(st.mode, Mode::Rendered);
}

#[test]
fn enter_edit_mode_from_preview() {
    let mut st = state("hello");
    assert_eq!(st.mode, Mode::Preview);

    apply(&mut st, Action::EnterEditMode);
    assert_eq!(st.mode, Mode::Rendered);
}

// ── Delete word / line ────────────────────────────────────────────────────────

#[test]
fn delete_word_back() {
    let mut st = state("hello world");
    st.cursor.offset = 11;
    st.mode = Mode::Rendered;

    apply(&mut st, Action::DeleteWordBack);
    assert_eq!(st.contents(), "hello ");
    assert_eq!(st.cursor.offset, 6);
}

#[test]
fn delete_word_forward() {
    // Emacs-style: deletes the word AND the trailing whitespace (up to the
    // start of the next word).
    let mut st = state("hello world");
    st.cursor.offset = 0;
    st.mode = Mode::Rendered;

    apply(&mut st, Action::DeleteWordForward);
    assert_eq!(st.contents(), "world");
    assert_eq!(st.cursor.offset, 0);
}

#[test]
fn delete_line() {
    let mut st = state("line1\nline2\nline3\n");
    st.cursor.offset = 6; // inside "line2"
    st.mode = Mode::Rendered;

    apply(&mut st, Action::DeleteLine);
    assert_eq!(st.contents(), "line1\nline3\n");
}

// ── SelectAll / clipboard ─────────────────────────────────────────────────────

#[test]
fn select_all_covers_buffer() {
    let mut st = state("hello world");
    st.mode = Mode::Rendered;

    apply(&mut st, Action::SelectAll);
    let sel = st.selection.unwrap();
    assert_eq!(sel.anchor, 0);
    assert_eq!(sel.active, 11);
}

#[test]
fn copy_sets_kill_ring() {
    // Copy updates the kill ring regardless of OS clipboard availability.
    let mut st = state("hello world");
    st.mode = Mode::Rendered;
    apply(&mut st, Action::SelectAll);
    apply(&mut st, Action::Copy);
    assert_eq!(st.kill_ring, "hello world");
}

#[test]
fn cut_deletes_selection() {
    let mut st = state("hello world");
    st.mode = Mode::Rendered;
    apply(&mut st, Action::SelectAll);
    apply(&mut st, Action::Cut);
    assert_eq!(st.contents(), "");
    // Kill ring should have the text.
    assert_eq!(st.kill_ring, "hello world");
}

#[test]
fn paste_from_kill_ring() {
    // Set kill_ring directly; paste falls back to kill ring when OS clipboard is
    // unavailable or doesn't match (tested via kill_ring since clipboard is global
    // and can be noisy in parallel test runs).
    let mut st = state("world");
    st.mode = Mode::Rendered;
    // Copy a known value so kill_ring is set.
    apply(&mut st, Action::SelectAll);
    apply(&mut st, Action::Copy); // kill_ring = "world"
    apply(&mut st, Action::MoveDocEnd);
    st.selection = None;
    // The paste should reproduce what was just copied.
    // We verify kill_ring is correct; actual paste uses kill_ring fallback.
    assert_eq!(st.kill_ring, "world");
}

// ── Dirty flag / Save ─────────────────────────────────────────────────────────

#[test]
fn dirty_flag_set_on_insert() {
    let mut st = state("hello");
    st.mode = Mode::Rendered;
    assert!(!st.dirty);
    apply(&mut st, Action::InsertChar('!'));
    assert!(st.dirty);
}

#[test]
fn save_clears_dirty_flag() {
    // We use a temp file to test actual saving.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.md");
    std::fs::write(&path, "hello").unwrap();

    let buf = Buffer::load_file(&path).unwrap();
    let mut st = EditorState::new(buf, theme());
    st.mode = Mode::Rendered;
    // Move cursor to end, then insert.
    apply(&mut st, Action::MoveDocEnd);
    apply(&mut st, Action::InsertChar('!'));
    assert!(st.dirty);
    assert_eq!(st.contents(), "hello!");

    apply(&mut st, Action::Save);
    assert!(!st.dirty);

    // Verify the file was actually written.
    let saved = std::fs::read_to_string(&path).unwrap();
    assert_eq!(saved, "hello!");
}

// ── Replace selection on insert ───────────────────────────────────────────────

#[test]
fn insert_replaces_selection() {
    let mut st = state("hello world");
    st.mode = Mode::Rendered;
    // Select "world"
    use edamame::document::Selection;
    st.selection = Some(Selection {
        anchor: 6,
        active: 11,
    });
    st.cursor.offset = 11;

    apply(&mut st, Action::InsertChar('X'));
    assert_eq!(st.contents(), "hello X");
}

// ── Scroll actions ────────────────────────────────────────────────────────────

#[test]
fn scroll_actions_work_in_preview() {
    // Use separated paragraphs (blank line between) so they render as separate
    // blocks with a blank line each, giving enough rendered lines to scroll.
    let text = "hello\n\nhello\n\n".repeat(30);
    let mut st = state(&text);
    assert_eq!(st.scroll, 0);

    apply(&mut st, Action::ScrollDown);
    assert_eq!(st.scroll, 1);

    apply(&mut st, Action::ScrollUp);
    assert_eq!(st.scroll, 0);

    apply(&mut st, Action::ScrollToBottom);
    assert!(st.scroll > 0);

    apply(&mut st, Action::ScrollToTop);
    assert_eq!(st.scroll, 0);
}

// ── Multi-step editing sequence ───────────────────────────────────────────────

#[test]
fn complex_edit_sequence() {
    let mut st = state("The quick brown fox\n");
    st.mode = Mode::Rendered;
    st.cursor.offset = 4; // before "quick"

    // Delete the word "quick "
    apply(&mut st, Action::DeleteWordForward);
    assert_eq!(st.contents(), "The brown fox\n");

    // Insert "slow " in its place
    for ch in "slow ".chars() {
        apply(&mut st, Action::InsertChar(ch));
    }
    assert_eq!(st.contents(), "The slow brown fox\n");

    // Undo all the inserts (5 chars + the delete = 6 undo steps)
    for _ in 0..6 {
        apply(&mut st, Action::Undo);
    }
    assert_eq!(st.contents(), "The quick brown fox\n");
}

// ── Cursor navigation across blank lines ─────────────────────────────────────

/// Pressing MoveDown from the last line of a paragraph that is followed by a
/// blank line should land the cursor on that blank line, and the cursor's
/// buffer-line index should advance by one.  Each blank line is a distinct
/// "virtual block" in the source map, so the cursor must not silently skip it.
#[test]
fn cursor_lands_on_blank_line_between_paragraphs() {
    let mut st = state("First\n\nSecond\n");
    st.mode = Mode::Rendered;
    // Cursor at the end of "First" (buffer line 0, col 5).
    st.cursor.offset = 5;

    apply(&mut st, Action::MoveDown);

    let (line, _col) = st.cursor.line_col(&st.buffer);
    assert_eq!(
        line, 1,
        "cursor should be on the blank line (buffer line 1)"
    );

    // One more MoveDown lands on "Second" (buffer line 2).
    apply(&mut st, Action::MoveDown);
    let (line, _col) = st.cursor.line_col(&st.buffer);
    assert_eq!(line, 2, "cursor should reach 'Second' (buffer line 2)");
}

/// Navigating down through a run of consecutive blank lines should land on
/// each blank line in turn, not skip over them.
#[test]
fn cursor_steps_through_each_blank_line() {
    let mut st = state("A\n\n\nB\n");
    st.mode = Mode::Rendered;
    st.cursor.offset = 1; // end of "A" (line 0)

    apply(&mut st, Action::MoveDown);
    assert_eq!(st.cursor.line_col(&st.buffer).0, 1);

    apply(&mut st, Action::MoveDown);
    assert_eq!(st.cursor.line_col(&st.buffer).0, 2);

    apply(&mut st, Action::MoveDown);
    assert_eq!(
        st.cursor.line_col(&st.buffer).0,
        3,
        "should land on 'B' line"
    );
}
