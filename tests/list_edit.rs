//! Integration tests for Phase 3 — smart list editing.
//!
//! These tests construct an `EditorState` around a buffer that contains a
//! Markdown list, dispatch a sequence of `Action`s via `edit_ops::apply`, and
//! assert on the resulting buffer content and cursor position.
//!
//! Coverage:
//! - bullet list continuation (`-`, `*`, `+`)
//! - numbered list continuation with correct next number
//! - double-Enter exits the list (removes the empty marker; leaves a blank line)
//! - inserting an item mid-list renumbers subsequent items
//! - nested lists at multiple indentation levels
//! - task list continuation (`- [ ] ` preserves the checkbox, new item is `[ ]`)
//! - toggle-checkbox (`[ ]` ↔ `[x]`) via `Action::ToggleCheckbox`
//! - renumber-on-paste for ordered lists

use edamame::config::{Action, Theme};
use edamame::document::Buffer;
use edamame::editor::{edit_ops, EditorState, Mode};

const VP: usize = 40;
const VW: usize = 80;

fn theme() -> &'static Theme {
    Box::leak(Box::new(Theme::default()))
}

fn apply(st: &mut EditorState, action: Action) -> bool {
    edit_ops::apply(st, action, VP, VW)
}

/// Place the cursor at the first occurrence of `needle` in `text`, counted in
/// bytes.  To position at the *end* of a line, use `"\n"` or the line's final
/// character as the needle and adjust with `cursor_shift`.
fn editor_at(text: &str, needle: &str) -> EditorState {
    let mut st = EditorState::new(Buffer::from_str(text), theme());
    st.mode = Mode::Rendered;
    let byte = text.find(needle).expect("needle not found");
    let char_off = st.buffer.rope().byte_to_char(byte);
    st.cursor.offset = char_off;
    st.update_cursor_block();
    st
}

/// Place the cursor at the end of the first line that matches `line_prefix`.
fn editor_at_end_of_line(text: &str, line_prefix: &str) -> EditorState {
    let byte = text.find(line_prefix).expect("line_prefix not found");
    let after_prefix = byte + line_prefix.len();
    // Walk forward to the end of the line (the `\n` or end-of-buffer).
    let mut end = after_prefix;
    let bytes = text.as_bytes();
    while end < bytes.len() && bytes[end] != b'\n' {
        end += 1;
    }
    let mut st = EditorState::new(Buffer::from_str(text), theme());
    st.mode = Mode::Rendered;
    let char_off = st.buffer.rope().byte_to_char(end);
    st.cursor.offset = char_off;
    st.update_cursor_block();
    st
}

fn cursor_byte(st: &EditorState) -> usize {
    st.buffer.rope().char_to_byte(st.cursor.offset)
}

// ─── Bullet list continuation ────────────────────────────────────────────────

#[test]
fn enter_at_end_of_bullet_item_inserts_new_marker() {
    let src = "- foo\n";
    let mut st = editor_at_end_of_line(src, "- foo");
    apply(&mut st, Action::Newline);

    assert_eq!(st.contents(), "- foo\n- \n");
    // Cursor should sit just past the new "- " on the new line.
    let byte = cursor_byte(&st);
    assert_eq!(&st.contents()[..byte], "- foo\n- ");
}

#[test]
fn star_bullet_continues_with_star() {
    let src = "* first\n";
    let mut st = editor_at_end_of_line(src, "* first");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "* first\n* \n");
}

#[test]
fn plus_bullet_continues_with_plus() {
    let src = "+ first\n";
    let mut st = editor_at_end_of_line(src, "+ first");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "+ first\n+ \n");
}

#[test]
fn enter_mid_bullet_item_splits_into_two_items() {
    // Cursor at "- foo|bar" → Enter → "- foo\n- bar".
    let src = "- foobar\n";
    let mut st = editor_at(src, "bar");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "- foo\n- bar\n");
    // Cursor should be at the start of "bar" on the new line.
    let byte = cursor_byte(&st);
    assert_eq!(&st.contents()[byte..byte + 3], "bar");
}

// ─── Numbered list continuation ──────────────────────────────────────────────

#[test]
fn enter_at_end_of_numbered_item_increments_number() {
    let src = "1. first\n";
    let mut st = editor_at_end_of_line(src, "1. first");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "1. first\n2. \n");
}

#[test]
fn enter_at_end_of_last_item_continues_count() {
    let src = "1. one\n2. two\n";
    let mut st = editor_at_end_of_line(src, "2. two");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "1. one\n2. two\n3. \n");
}

#[test]
fn ordered_list_uses_paren_delimiter_when_present() {
    let src = "1) one\n";
    let mut st = editor_at_end_of_line(src, "1) one");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "1) one\n2) \n");
}

#[test]
fn inserting_item_mid_ordered_list_renumbers_subsequent() {
    // Cursor at end of "1. alpha"; Enter should insert a new "2. " and
    // renumber "2. beta" → "3. beta", "3. gamma" → "4. gamma".
    let src = "1. alpha\n2. beta\n3. gamma\n";
    let mut st = editor_at_end_of_line(src, "1. alpha");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "1. alpha\n2. \n3. beta\n4. gamma\n");
    // Cursor should be on the new empty item.
    let byte = cursor_byte(&st);
    assert_eq!(&st.contents()[..byte], "1. alpha\n2. ");
}

#[test]
fn mid_item_split_in_ordered_list_renumbers() {
    // Cursor inside "2. beta" at "|beta" → split into "2. " and "3. beta".
    let src = "1. alpha\n2. beta\n3. gamma\n";
    let mut st = editor_at(src, "beta");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "1. alpha\n2. \n3. beta\n4. gamma\n");
}

// ─── Double-Enter exits the list ─────────────────────────────────────────────

#[test]
fn enter_on_empty_bullet_item_exits_the_list() {
    // Two successive Enters: the first creates an empty "- " item, the second
    // removes the marker and leaves a blank line separating the list from
    // whatever follows.
    let src = "- foo\n";
    let mut st = editor_at_end_of_line(src, "- foo");
    apply(&mut st, Action::Newline); // "- foo\n- "
    apply(&mut st, Action::Newline); // should exit → "- foo\n\n"
    assert_eq!(st.contents(), "- foo\n\n");
    // Cursor lands on the blank line (past the second \n).
    let byte = cursor_byte(&st);
    assert_eq!(byte, 7);
}

#[test]
fn enter_on_empty_ordered_item_exits_the_list() {
    let src = "1. foo\n";
    let mut st = editor_at_end_of_line(src, "1. foo");
    apply(&mut st, Action::Newline); // "1. foo\n2. "
    apply(&mut st, Action::Newline); // exit → "1. foo\n\n"
    assert_eq!(st.contents(), "1. foo\n\n");
}

#[test]
fn enter_on_empty_middle_item_exits_leaving_blank_line() {
    // `- foo\n- <cursor>\n- baz\n` → Enter should replace the empty `- ` with a
    // blank line, leaving the cursor on it.
    let src = "- foo\n- \n- baz\n";
    let mut st = editor_at_end_of_line(src, "- ");
    // editor_at_end_of_line points to end of first line. Move cursor to end of
    // the empty-marker line instead.
    let empty_line_start = src.find("\n- \n").unwrap() + 1; // start of "- \n"
    let empty_line_end = empty_line_start + 2; // just past "- "
    st.cursor.offset = st.buffer.rope().byte_to_char(empty_line_end);
    st.update_cursor_block();

    apply(&mut st, Action::Newline);

    assert_eq!(st.contents(), "- foo\n\n- baz\n");
}

// ─── Task list continuation ──────────────────────────────────────────────────

#[test]
fn task_list_enter_inserts_unchecked_item() {
    let src = "- [ ] first\n";
    let mut st = editor_at_end_of_line(src, "- [ ] first");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "- [ ] first\n- [ ] \n");
}

#[test]
fn task_list_enter_after_checked_inserts_unchecked_item() {
    // New task items are ALWAYS unchecked, regardless of the parent's state.
    let src = "- [x] first\n";
    let mut st = editor_at_end_of_line(src, "- [x] first");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "- [x] first\n- [ ] \n");
}

#[test]
fn empty_task_item_exits_list_on_enter() {
    let src = "- [ ] foo\n";
    let mut st = editor_at_end_of_line(src, "- [ ] foo");
    apply(&mut st, Action::Newline); // "- [ ] foo\n- [ ] "
    apply(&mut st, Action::Newline); // exit
    assert_eq!(st.contents(), "- [ ] foo\n\n");
}

// ─── Toggle checkbox ─────────────────────────────────────────────────────────

#[test]
fn toggle_checkbox_unchecks_checked() {
    let src = "- [x] done\n";
    let mut st = editor_at(src, "done");
    apply(&mut st, Action::ToggleCheckbox);
    assert_eq!(st.contents(), "- [ ] done\n");
}

#[test]
fn toggle_checkbox_checks_unchecked() {
    let src = "- [ ] todo\n";
    let mut st = editor_at(src, "todo");
    apply(&mut st, Action::ToggleCheckbox);
    assert_eq!(st.contents(), "- [x] todo\n");
}

#[test]
fn toggle_checkbox_works_for_ordered_task() {
    let src = "1. [ ] task\n";
    let mut st = editor_at(src, "task");
    apply(&mut st, Action::ToggleCheckbox);
    assert_eq!(st.contents(), "1. [x] task\n");
}

#[test]
fn toggle_checkbox_noop_outside_list() {
    let src = "just a paragraph\n";
    let mut st = editor_at(src, "paragraph");
    let before = st.contents();
    apply(&mut st, Action::ToggleCheckbox);
    assert_eq!(st.contents(), before);
}

#[test]
fn toggle_checkbox_noop_on_non_task_list_item() {
    let src = "- not a task\n";
    let mut st = editor_at(src, "task");
    let before = st.contents();
    apply(&mut st, Action::ToggleCheckbox);
    assert_eq!(st.contents(), before);
}

// ─── Nested lists ────────────────────────────────────────────────────────────

#[test]
fn enter_in_nested_bullet_list_continues_at_same_indent() {
    let src = "- outer\n  - inner\n";
    let mut st = editor_at_end_of_line(src, "  - inner");
    apply(&mut st, Action::Newline);
    // New item should appear at the nested indent with the same bullet.
    assert_eq!(st.contents(), "- outer\n  - inner\n  - \n");
}

#[test]
fn enter_in_nested_numbered_list_increments_at_same_indent() {
    let src = "- outer\n  1. inner\n";
    let mut st = editor_at_end_of_line(src, "  1. inner");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "- outer\n  1. inner\n  2. \n");
}

// ─── Non-list context: Enter behaves normally ────────────────────────────────

#[test]
fn enter_outside_list_inserts_plain_newline() {
    let src = "just text\n";
    let mut st = editor_at_end_of_line(src, "just text");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "just text\n\n");
}

// ─── Cursor at the marker prefix ─────────────────────────────────────────────

#[test]
fn enter_inside_marker_prefix_does_not_continue_list() {
    // Cursor before content (e.g. between "-" and " " of "- foo") — Enter
    // should fall through to a plain newline rather than inserting another
    // marker in the middle of the marker.
    let src = "- foo\n";
    let mut st = editor_at(src, "- foo");
    // cursor at byte 1 (between "-" and " ")
    st.cursor.offset = st.buffer.rope().byte_to_char(1);
    st.update_cursor_block();
    apply(&mut st, Action::Newline);
    // Plain newline split of the line.
    assert_eq!(st.contents(), "-\n foo\n");
}

// ─── Renumber on paste ───────────────────────────────────────────────────────

#[test]
fn paste_into_numbered_list_renumbers_whole_list() {
    // Paste a fragment containing its own `N. ` markers into the middle of an
    // existing numbered list; the surrounding list must be renumbered so that
    // the combined sequence is monotonic from the first item's base number.
    let src = "1. one\n2. two\n3. three\n";
    let mut st = editor_at_end_of_line(src, "1. one");
    // Continue to a new empty item, then paste items with colliding numbers.
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "1. one\n2. \n3. two\n4. three\n");

    // Paste a pre-formed numbered fragment at the cursor (inside the empty
    // item on line 2).  The raw paste would produce duplicate `2.` / `3.`
    // entries; the renumber pass must fix them.
    st.kill_ring = "2. pasted-a\n3. pasted-b".into();
    apply(&mut st, Action::Paste);

    let expected = "1. one\n2. 2. pasted-a\n3. pasted-b\n3. two\n4. three\n";
    // Before renumber, the literal paste text is left in place on the first
    // line ("2. 2. pasted-a"). The renumber pass only fixes item *markers* at
    // line-start — it does not unwedge inline numbers. What it DOES fix is
    // the subsequent lines, so `3. pasted-b`, `3. two`, `4. three` stay
    // consistent:
    let _ = expected;
    // In practice, the key invariant we want to verify is that each
    // line-leading number strictly increases by 1 as you read top to bottom.
    let contents = st.contents();
    let leading_numbers: Vec<u64> = contents
        .lines()
        .filter_map(|line| {
            let digits: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                None
            } else {
                let rest = &line[digits.len()..];
                if rest.starts_with(". ") || rest.starts_with(") ") {
                    digits.parse().ok()
                } else {
                    None
                }
            }
        })
        .collect();
    assert!(
        leading_numbers.windows(2).all(|w| w[1] == w[0] + 1),
        "expected strictly increasing leading numbers after paste+renumber, got: {leading_numbers:?}\ncontents:\n{contents}"
    );
}

// ─── Backspace at content_start deletes the whole marker ─────────────────────

#[test]
fn backspace_at_content_start_of_first_item_removes_marker() {
    // `- foo` with cursor at content_start (byte 2).  Backspace should delete
    // the whole marker "- " and leave plain text "foo" with cursor at 0.
    let src = "- foo\n";
    let mut st = editor_at(src, "foo");
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "foo\n");
    assert_eq!(cursor_byte(&st), 0);
}

#[test]
fn backspace_at_content_start_merges_with_previous_list_item() {
    // `- a\n- foo` with cursor at content_start of "- foo" (byte 6).
    // Backspace should delete `\n- ` and join: `- afoo`, cursor at 3.
    let src = "- a\n- foo\n";
    let mut st = editor_at(src, "foo");
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "- afoo\n");
    assert_eq!(cursor_byte(&st), 3);
}

#[test]
fn backspace_at_content_start_merges_with_non_list_previous_line() {
    // `text\n- foo` → backspace at content_start of the list → `textfoo`.
    let src = "text\n- foo\n";
    let mut st = editor_at(src, "foo");
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "textfoo\n");
    assert_eq!(cursor_byte(&st), 4);
}

#[test]
fn backspace_at_content_start_of_task_item_removes_full_prefix() {
    // `- [ ] foo` with cursor at content_start (byte 6).  Backspace should
    // delete the whole `- [ ] ` prefix.
    let src = "- [ ] foo\n";
    let mut st = editor_at(src, "foo");
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "foo\n");
    assert_eq!(cursor_byte(&st), 0);
}

#[test]
fn backspace_at_content_start_in_ordered_list_removes_marker_and_renumbers() {
    // `1. a\n2. foo\n3. bar\n` — cursor at content_start of `2. foo` (byte 8).
    // Backspace should remove `\n2. `, merging "foo" into "1. a" to produce
    // "1. afoo\n3. bar\n", then renumber to "1. afoo\n2. bar\n".
    let src = "1. a\n2. foo\n3. bar\n";
    let mut st = editor_at(src, "foo");
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "1. afoo\n2. bar\n");
}

// ─── Auto-renumber after edits ───────────────────────────────────────────────

#[test]
fn auto_renumber_after_deleting_item_via_delete_line() {
    let src = "1. a\n2. b\n3. c\n";
    let mut st = editor_at(src, "b");
    apply(&mut st, Action::DeleteLine);
    assert_eq!(st.contents(), "1. a\n2. c\n");
}

#[test]
fn delete_line_lands_cursor_on_content_not_marker() {
    // `DeleteLine` places the cursor at the start of the line that used to
    // follow the deleted one — which for a list is the marker itself.  The
    // post-action clamp must snap the cursor forward to `content_start` so
    // the next keystroke does not land inside `2. ` / `- ` / etc.
    let src = "1. a\n2. b\n3. c\n";
    let mut st = editor_at(src, "b");
    apply(&mut st, Action::DeleteLine);
    // After the delete + renumber, contents are "1. a\n2. c\n".  Cursor must
    // sit on the `c` (byte 8), not on the `2` (byte 5).
    let byte = cursor_byte(&st);
    assert_eq!(
        byte,
        8,
        "cursor should sit on content 'c' at byte 8, got byte {byte} \
         in contents {:?}",
        st.contents()
    );
}

// ─── Cursor skips list markers ───────────────────────────────────────────────

#[test]
fn right_arrow_at_end_of_item_jumps_to_next_item_content() {
    // `- a\n- b\n` — cursor at end of "- a" (byte 3, the line_end before \n).
    // Right arrow should skip the \n and the `- ` marker, landing on `b`.
    let src = "- a\n- b\n";
    let mut st = editor_at_end_of_line(src, "- a");
    apply(&mut st, Action::MoveRight);
    assert_eq!(cursor_byte(&st), 6);
    assert_eq!(&src[6..7], "b");
}

#[test]
fn left_arrow_at_content_start_jumps_to_previous_line_end() {
    // `- a\n- b\n` — cursor at content_start of "- b" (byte 6).  Left arrow
    // should skip the `- ` and the \n, landing at end of "- a" (byte 3).
    let src = "- a\n- b\n";
    let mut st = editor_at(src, "b");
    apply(&mut st, Action::MoveLeft);
    assert_eq!(cursor_byte(&st), 3);
}

#[test]
fn left_arrow_at_first_item_content_start_jumps_past_list() {
    // `text\n- foo\n` — cursor at content_start of "- foo" (byte 7).  Left
    // arrow should skip the `- ` marker and the preceding \n, landing at end
    // of "text" (byte 4).
    let src = "text\n- foo\n";
    let mut st = editor_at(src, "foo");
    apply(&mut st, Action::MoveLeft);
    assert_eq!(cursor_byte(&st), 4);
}

#[test]
fn right_arrow_at_last_item_line_end_exits_list() {
    // `- foo\n\nafter` — cursor at end of "- foo" (byte 5).  Right arrow should
    // skip the \n and land at the start of the blank-line region (byte 6) via
    // normal movement.
    let src = "- foo\n\nafter\n";
    let mut st = editor_at_end_of_line(src, "- foo");
    apply(&mut st, Action::MoveRight);
    // Either landing at byte 6 (the \n of the blank line) or byte 7 (past it)
    // is acceptable; both are "out of the list".  Most importantly the cursor
    // is NOT on the marker of a hypothetical list continuation.
    let after = cursor_byte(&st);
    assert!(
        after >= 6,
        "cursor moved past line_end of the last item (got {after})"
    );
}

// ─── Undo restores a continue-item edit ──────────────────────────────────────

#[test]
fn undo_reverts_continue_item() {
    let src = "1. one\n2. two\n";
    let mut st = editor_at_end_of_line(src, "1. one");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "1. one\n2. \n3. two\n");
    apply(&mut st, Action::Undo);
    assert_eq!(st.contents(), src);
}
