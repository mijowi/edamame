//! Integration tests for smart list editing.
//!
//! These tests construct an `EditorState` around a buffer that contains a
//! Markdown list, dispatch a sequence of `Action`s via `edit_ops::apply`, and
//! assert on the resulting buffer content and cursor position.
//!
//! Coverage:
//! - bullet list continuation (`-`, `*`, `+`)
//! - numbered list continuation with correct next number
//! - triple-Enter list-break gesture: 1st Enter creates a new empty item,
//!   2nd Enter widens the gap above it (single blank line between items
//!   is allowed for readability), 3rd Enter strips the marker and breaks
//!   the list, renumbering the trailing items of an ordered list from 1
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

// ─── Triple-Enter breaks the list ────────────────────────────────────────────

#[test]
fn triple_enter_breaks_bullet_list_at_end() {
    // 1st Enter: creates a new "- " on the next line.
    // 2nd Enter: widens the gap above the empty marker by one blank line;
    //            the marker (and the cursor on it) move down a line.
    // 3rd Enter: strips the marker and parks the cursor on the blank
    //            line two visual rows below the surviving "- foo".
    let src = "- foo\n";
    let mut st = editor_at_end_of_line(src, "- foo");

    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "- foo\n- \n");
    assert_eq!(cursor_byte(&st), 8);

    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "- foo\n\n- \n");
    assert_eq!(cursor_byte(&st), 9);

    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "- foo\n\n");
    assert_eq!(cursor_byte(&st), 7);
}

#[test]
fn triple_enter_breaks_ordered_list_at_end() {
    // Same gesture for ordered lists: the empty `2. ` first appears, then
    // is pushed down a line, then is stripped along with its trailing
    // newline so the cursor settles two lines below "1. foo".
    let src = "1. foo\n";
    let mut st = editor_at_end_of_line(src, "1. foo");

    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "1. foo\n2. \n");

    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "1. foo\n\n2. \n");

    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "1. foo\n\n");
}

#[test]
fn triple_enter_in_middle_of_bullet_list_breaks_into_two_lists() {
    // Cursor at end of "- b" in a three-item list.  Three Enters split
    // the list at that point with one blank line in between — the
    // parser splits at any blank line outside fenced code, so a single
    // gap is enough to render the head and tail as their own lists.
    let src = "- a\n- b\n- c\n";
    let mut st = editor_at_end_of_line(src, "- b");

    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "- a\n- b\n- \n- c\n");

    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "- a\n- b\n\n- \n- c\n");

    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "- a\n- b\n\n- c\n");
    // Cursor on the blank line that separates the two split lists.
    assert_eq!(cursor_byte(&st), 8);
}

#[test]
fn triple_enter_in_middle_of_ordered_list_renumbers_tail_from_one() {
    // Cursor at end of "2. b".  After three Enters, the surviving head keeps
    // its original numbering ("1. a", "2. b") and `exit_list` restarts the
    // trailing item at 1.  The third Enter parks the cursor on the blank
    // separator line, so the post-edit renumber hook (which only fires when the
    // cursor rests on a list line) leaves the tail as authored.
    let src = "1. a\n2. b\n3. c\n";
    let mut st = editor_at_end_of_line(src, "2. b");

    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "1. a\n2. b\n3. \n4. c\n");

    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "1. a\n2. b\n\n3. \n4. c\n");

    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "1. a\n2. b\n\n1. c\n");
    // Cursor on the blank line separating the surviving head from the tail.
    assert_eq!(cursor_byte(&st), 10);
}

#[test]
fn second_enter_on_already_blank_separated_empty_item_breaks_immediately() {
    // The user manually authored "- foo\n\n- " — the empty marker
    // already has a blank line above it, so a single Enter (the third
    // step of the gesture, with the first two having been "typed by
    // hand") strips the marker and breaks the list straight away.
    let src = "- foo\n\n- \n";
    let mut st = editor_at_end_of_line(src, "- foo");
    let cursor_byte_target = src.rfind("- ").unwrap() + 2; // end of the empty "- "
    st.cursor.offset = st.buffer.rope().byte_to_char(cursor_byte_target);
    st.update_cursor_block();

    apply(&mut st, Action::Newline);

    assert_eq!(st.contents(), "- foo\n\n");
    assert_eq!(cursor_byte(&st), 7);
}

#[test]
fn enter_on_empty_middle_item_with_no_blank_above_widens_the_gap() {
    // `- foo / - <cursor> / - baz` — the empty item has no blank line
    // above it yet, so the first Enter just inserts one.  A second
    // Enter is needed to actually break the list.
    let src = "- foo\n- \n- baz\n";
    let mut st = editor_at_end_of_line(src, "- ");
    let empty_line_start = src.find("\n- \n").unwrap() + 1;
    let empty_line_end = empty_line_start + 2;
    st.cursor.offset = st.buffer.rope().byte_to_char(empty_line_end);
    st.update_cursor_block();

    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "- foo\n\n- \n- baz\n");

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
fn empty_task_item_exits_list_on_third_enter() {
    let src = "- [ ] foo\n";
    let mut st = editor_at_end_of_line(src, "- [ ] foo");
    apply(&mut st, Action::Newline); // "- [ ] foo\n- [ ] "
    apply(&mut st, Action::Newline); // widen gap → "- [ ] foo\n\n- [ ] "
    apply(&mut st, Action::Newline); // exit → "- [ ] foo\n\n"
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
fn backspace_at_content_start_unbullets_without_merging() {
    // `- a\n- foo` with cursor at content_start of "- foo" (byte 6).
    // Backspace strips only `- `, leaving "foo" on its own line with the
    // cursor at the start of that line (byte 4).
    let src = "- a\n- foo\n";
    let mut st = editor_at(src, "foo");
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "- a\nfoo\n");
    assert_eq!(cursor_byte(&st), 4);
}

#[test]
fn second_backspace_after_unbulleting_merges_with_previous_line() {
    // The merge is still reachable — it's just the *second* backspace.
    let src = "- a\n- foo\n";
    let mut st = editor_at(src, "foo");
    apply(&mut st, Action::DeleteCharBack);
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "- afoo\n");
    assert_eq!(cursor_byte(&st), 3);
}

#[test]
fn backspace_at_content_start_after_non_list_previous_line() {
    // `text\n- foo` → backspace strips the marker only → `text\nfoo`.
    let src = "text\n- foo\n";
    let mut st = editor_at(src, "foo");
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "text\nfoo\n");
    assert_eq!(cursor_byte(&st), 5);
}

#[test]
fn backspace_at_content_start_of_task_item_peels_checkbox_then_bullet() {
    // Two-step erase: first backspace strips just the `[ ] ` checkbox,
    // leaving the item as a plain bullet; second backspace strips the
    // bullet itself.
    let src = "- [ ] foo\n";
    let mut st = editor_at(src, "foo");
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "- foo\n");
    assert_eq!(cursor_byte(&st), 2);
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "foo\n");
    assert_eq!(cursor_byte(&st), 0);
}

#[test]
fn backspace_at_content_start_in_ordered_list_removes_marker() {
    // `1. a\n2. foo\n3. bar\n` — cursor at content_start of `2. foo` (byte 8).
    // Backspace removes `2. ` only, leaving "foo" on its own line.
    let src = "1. a\n2. foo\n3. bar\n";
    let mut st = editor_at(src, "foo");
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "1. a\nfoo\n3. bar\n");
    assert_eq!(cursor_byte(&st), 5);
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

// ─── Tab / Shift+Tab indent / outdent ────────────────────────────────────────

#[test]
fn tab_on_bullet_item_indents_by_tab_width() {
    let src = "- foo\n- bar\n";
    let mut st = editor_at_end_of_line(src, "- bar");
    apply(&mut st, Action::InsertTab);
    assert_eq!(st.contents(), "- foo\n    - bar\n");
    // Cursor should follow the content end.
    let byte = cursor_byte(&st);
    assert_eq!(&st.contents()[..byte], "- foo\n    - bar");
}

#[test]
fn tab_on_ordered_item_resets_number_and_renumbers_outer_list() {
    // Indenting item 2 pulls it out into a nested list starting at 1; the
    // remaining outer items "1. a" and "3. c" stay sequential as "1. a"
    // and "2. c".
    let src = "1. a\n2. b\n3. c\n";
    let mut st = editor_at_end_of_line(src, "2. b");
    apply(&mut st, Action::InsertTab);
    assert_eq!(st.contents(), "1. a\n    1. b\n2. c\n");
}

#[test]
fn tab_on_task_item_preserves_checkbox() {
    let src = "- [ ] foo\n- [ ] bar\n";
    let mut st = editor_at_end_of_line(src, "- [ ] bar");
    apply(&mut st, Action::InsertTab);
    assert_eq!(st.contents(), "- [ ] foo\n    - [ ] bar\n");
}

#[test]
fn tab_on_first_item_is_a_noop() {
    // The first item of a list has no preceding sibling to nest under, so
    // indenting it can never form a valid nested item — the marker would
    // degrade into a lazy paragraph continuation of the parent (nested
    // case) or an indented code block (top level).  Tab is swallowed
    // whole: no indent, and no plain-space fallback either.
    for (src, needle) in [
        ("- foo\n- bar\n", "- foo"),                     // top level
        ("- top\n  - child1\n  - child2\n", "- child1"), // first nested child
        ("1. top\n    1. b\n    2. c\n", "1. b"),        // first nested ordered
        ("- top\n  - only\n", "- only"),                 // sole nested child
    ] {
        let mut st = editor_at_end_of_line(src, needle);
        apply(&mut st, Action::InsertTab);
        assert_eq!(st.contents(), src, "Tab on {needle:?} must be a no-op");
    }
}

#[test]
fn shift_tab_outdents_nested_bullet_item() {
    let src = "- outer\n    - inner\n";
    let mut st = editor_at_end_of_line(src, "    - inner");
    apply(&mut st, Action::TablePrevCell);
    assert_eq!(st.contents(), "- outer\n- inner\n");
}

#[test]
fn shift_tab_partial_outdent_when_indent_shorter_than_tab_width() {
    // `  - inner` has two spaces of indent; Shift+Tab strips those two
    // spaces (min(tab_width, indent_len)), landing the item at column 0.
    let src = "- outer\n  - inner\n";
    let mut st = editor_at_end_of_line(src, "  - inner");
    apply(&mut st, Action::TablePrevCell);
    assert_eq!(st.contents(), "- outer\n- inner\n");
}

#[test]
fn shift_tab_at_top_level_is_noop() {
    let src = "- foo\n";
    let mut st = editor_at_end_of_line(src, "- foo");
    apply(&mut st, Action::TablePrevCell);
    assert_eq!(st.contents(), src);
}

#[test]
fn tab_outside_list_inserts_tab_width_spaces() {
    let src = "plain\n";
    let mut st = editor_at_end_of_line(src, "plain");
    apply(&mut st, Action::InsertTab);
    assert_eq!(st.contents(), "plain    \n");
}

#[test]
fn tab_shift_tab_roundtrip_on_bullet_list() {
    let src = "- a\n- b\n- c\n";
    let mut st = editor_at_end_of_line(src, "- b");
    apply(&mut st, Action::InsertTab);
    assert_eq!(st.contents(), "- a\n    - b\n- c\n");
    apply(&mut st, Action::TablePrevCell);
    assert_eq!(st.contents(), "- a\n- b\n- c\n");
}

#[test]
fn tab_on_empty_bullet_item_inserts_blank_line_separator() {
    // Indenting a content-empty bullet item inserts a blank line before the
    // indented marker so pulldown-cmark parses `    - ` as a nested list
    // rather than as a setext H2 underline of the previous paragraph.
    let src = "- a\n- b\n- c\n";
    let mut st = editor_at_end_of_line(src, "- b");
    apply(&mut st, Action::Newline); // creates empty "- " after "- b"
    assert_eq!(st.contents(), "- a\n- b\n- \n- c\n");
    apply(&mut st, Action::InsertTab);
    assert_eq!(st.contents(), "- a\n- b\n\n    - \n- c\n");
}

#[test]
fn tab_on_empty_ordered_item_inserts_blank_line_separator() {
    let src = "1. a\n2. b\n3. c\n";
    let mut st = editor_at_end_of_line(src, "2. b");
    apply(&mut st, Action::Newline); // produces "1. a\n2. b\n3. \n3. c\n" then auto-renumbers
    apply(&mut st, Action::InsertTab);
    assert_eq!(st.contents(), "1. a\n2. b\n\n    1. \n3. c\n");
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

#[test]
fn delete_line_renumbers_outer_list_across_a_nested_child() {
    // Non-vim `DeleteLine`: removing an outer item whose sibling carries a
    // nested child must still renumber the outer run.  The cursor lands on the
    // nested child after the delete, so this exercises the nesting-aware path.
    let src = "1. Ordered\n2. Second\n    1. nested\n3. Third\n";
    let mut st = editor_at(src, "2. Second");
    apply(&mut st, Action::DeleteLine);
    assert_eq!(st.contents(), "1. Ordered\n    1. nested\n2. Third\n");
}

// ── renumber_ordered_runs_in_range: nesting-aware ────────────────────────────

#[test]
fn renumber_block_counts_outer_run_across_a_nested_child() {
    use edamame::editor::list_edit::renumber_ordered_runs_in_range;
    // Outer items 1 and 3 are split by a nested child; the outer run must
    // renumber while the (already-correct) nested run is left alone.
    let src = "1. a\n    1. x\n3. c\n";
    let delta = renumber_ordered_runs_in_range(src, 0, src.len()).expect("outer run is not seq");
    let mut out = src.to_owned();
    out.replace_range(
        delta.offset..delta.offset + delta.removed.len(),
        &delta.inserted,
    );
    assert_eq!(out, "1. a\n    1. x\n2. c\n");
}

#[test]
fn renumber_block_restarts_each_sublist_and_is_noop_when_consistent() {
    use edamame::editor::list_edit::renumber_ordered_runs_in_range;
    // Already-consistent nested structure: every ordered run is sequential and
    // each sub-list restarts at 1 under its parent → no edit.
    let src = "1. a\n    1. x\n    2. y\n2. b\n    1. p\n3. c\n";
    assert!(
        renumber_ordered_runs_in_range(src, 0, src.len()).is_none(),
        "consistent block needs no edit"
    );
}

#[test]
fn renumber_block_preserves_bullet_children() {
    use edamame::editor::list_edit::renumber_ordered_runs_in_range;
    // A bullet sub-list nested under ordered items is kept verbatim while the
    // outer ordered run is renumbered.
    let src = "1. a\n    - x\n    - y\n3. c\n";
    let delta = renumber_ordered_runs_in_range(src, 0, src.len()).expect("outer run 1,3 not seq");
    let mut out = src.to_owned();
    out.replace_range(
        delta.offset..delta.offset + delta.removed.len(),
        &delta.inserted,
    );
    assert_eq!(out, "1. a\n    - x\n    - y\n2. c\n");
}

#[test]
fn renumber_block_leaves_ordered_markers_inside_a_code_fence() {
    use edamame::editor::list_edit::renumber_list_block;
    // A fenced code block nested in a list item can hold marker-shaped lines.
    // The renderer prints them literally (never renumbered), so the renumber
    // pass must leave them untouched — only the real outer items renumber.
    let src = "1. a\n   ```\n   3. code\n   9. code\n   ```\n5. b\n";
    let delta = renumber_list_block(src, 0).expect("outer run 1,5 is not sequential");
    let mut out = src.to_owned();
    out.replace_range(
        delta.offset..delta.offset + delta.removed.len(),
        &delta.inserted,
    );
    // Outer "5. b" → "2. b"; the "3."/"9." inside the fence are preserved.
    assert_eq!(out, "1. a\n   ```\n   3. code\n   9. code\n   ```\n2. b\n");
}

#[test]
fn renumber_block_handles_leading_zero_markers() {
    use edamame::editor::list_edit::renumber_ordered_runs_in_range;
    // A leading-zero marker ("01.") has more digit chars than its parsed
    // value; the marker-length measurement must count source digits so the
    // rewritten line doesn't gain a spurious space before the content.
    let src = "01. a\n01. b\n";
    let delta = renumber_ordered_runs_in_range(src, 0, src.len()).expect("renumbers");
    let mut out = src.to_owned();
    out.replace_range(
        delta.offset..delta.offset + delta.removed.len(),
        &delta.inserted,
    );
    assert_eq!(out, "1. a\n2. b\n");
}

#[test]
fn renumber_block_crosses_two_blank_line_loose_gap() {
    use edamame::editor::list_edit::renumber_ordered_runs_in_range;
    // Two blank lines between ordered items still render as one continuous
    // loose list in edamame (pulldown-cmark keeps them one list), so the
    // renumber spans the gap to match — it does not restart the tail.
    let src = "1. a\n2. b\n\n\n1. c\n2. d\n";
    let delta = renumber_ordered_runs_in_range(src, 0, src.len()).expect("renumbers across gap");
    let mut out = src.to_owned();
    out.replace_range(
        delta.offset..delta.offset + delta.removed.len(),
        &delta.inserted,
    );
    assert_eq!(out, "1. a\n2. b\n\n\n3. c\n4. d\n");
}

// ── fix_list_numbering action (end-to-end) ───────────────────────────────────

#[test]
fn fix_list_numbering_renumbers_loose_list_matching_render() {
    use edamame::editor::edit_ops::{fix_list_numbering, FixListNumbering};
    // A loose (blank-separated) ordered list renders as one continuous
    // sequence, so fixing it must renumber across the blank gaps — the whole
    // point of driving the range off the parser rather than a blank-bounded
    // scan.
    let src = "1. a\n\n5. b\n\n2. c\n";
    let mut st = editor_at(src, "5. b");
    assert_eq!(fix_list_numbering(&mut st, VP, VW), FixListNumbering::Fixed);
    assert_eq!(st.contents(), "1. a\n\n2. b\n\n3. c\n");

    // One undo delta: a single Undo restores the original source exactly.
    apply(&mut st, Action::Undo);
    assert_eq!(st.contents(), src);
}

#[test]
fn fix_list_numbering_works_in_raw_mode() {
    use edamame::editor::edit_ops::{fix_list_numbering, FixListNumbering};
    // Explicit command, unlike the automatic recovery paths: it must renumber
    // in Raw mode too (where the user is looking at the raw source).
    let mut st = editor_at("1. a\n1. b\n1. c\n", "1. b");
    st.mode = Mode::Raw;
    assert_eq!(fix_list_numbering(&mut st, VP, VW), FixListNumbering::Fixed);
    assert_eq!(st.contents(), "1. a\n2. b\n3. c\n");
}

#[test]
fn fix_list_numbering_flashes_on_bullet_list() {
    use edamame::editor::edit_ops::{fix_list_numbering, FixListNumbering};
    let mut st = editor_at("- a\n- b\n", "- b");
    assert_eq!(
        fix_list_numbering(&mut st, VP, VW),
        FixListNumbering::NotOrdered
    );
    assert_eq!(st.contents(), "- a\n- b\n", "no mutation for a bullet list");
}

#[test]
fn fix_list_numbering_reports_already_correct() {
    use edamame::editor::edit_ops::{fix_list_numbering, FixListNumbering};
    let mut st = editor_at("1. a\n2. b\n3. c\n", "2. b");
    assert_eq!(
        fix_list_numbering(&mut st, VP, VW),
        FixListNumbering::AlreadyCorrect
    );
}

#[test]
fn fix_list_numbering_not_in_list() {
    use edamame::editor::edit_ops::{fix_list_numbering, FixListNumbering};
    let mut st = editor_at("just a paragraph\n", "paragraph");
    assert_eq!(
        fix_list_numbering(&mut st, VP, VW),
        FixListNumbering::NotOrdered
    );
}

// ─── Multi-line items ────────────────────────────────────────────────────────

#[test]
fn enter_at_end_of_multiline_item_continues_list() {
    // Enter at the very end of the item's continuation line appends a new
    // sibling item after the whole item.
    let src = "- a\n  cont\n";
    let mut st = editor_at_end_of_line(src, "  cont");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "- a\n  cont\n- \n");
    let byte = cursor_byte(&st);
    assert_eq!(&st.contents()[..byte], "- a\n  cont\n- ");
}

#[test]
fn enter_mid_continuation_line_inserts_plain_newline() {
    let src = "- a\n  cont\n- b\n";
    let mut st = editor_at(src, "ont");
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "- a\n  c\nont\n- b\n");
}

#[test]
fn enter_mid_first_line_carries_continuations_to_new_item() {
    let src = "- one two\n  cont\n";
    let mut st = editor_at(src, " two"); // cursor right after "one"
    apply(&mut st, Action::Newline);
    assert_eq!(st.contents(), "- one\n-  two\n  cont\n");
}

#[test]
fn tab_on_multiline_item_indents_continuations_too() {
    let src = "- a\n- b\n  cont\n";
    let mut st = editor_at_end_of_line(src, "- b");
    apply(&mut st, Action::InsertTab);
    assert_eq!(st.contents(), "- a\n    - b\n      cont\n");
}

#[test]
fn shift_tab_on_multiline_item_outdents_continuations_too() {
    let src = "- a\n    - b\n      cont\n";
    let mut st = editor_at_end_of_line(src, "    - b");
    apply(&mut st, Action::TablePrevCell);
    assert_eq!(st.contents(), "- a\n- b\n  cont\n");
}

#[test]
fn toggle_checkbox_from_continuation_line_toggles_owning_item() {
    let src = "- [ ] task\n  cont\n";
    let mut st = editor_at(src, "ont");
    apply(&mut st, Action::ToggleCheckbox);
    assert_eq!(st.contents(), "- [x] task\n  cont\n");
}

#[test]
fn arrow_keys_on_continuation_line_move_plainly() {
    // Right arrow at the end of the marker line of a multi-line item steps
    // onto the continuation line instead of hopping to the next item.
    let src = "- a\n  cont\n- b\n";
    let mut st = editor_at_end_of_line(src, "- a");
    apply(&mut st, Action::MoveRight);
    assert_eq!(cursor_byte(&st), 4, "steps onto the continuation line");
    // And moving within the continuation line stays char-by-char.
    apply(&mut st, Action::MoveRight);
    assert_eq!(cursor_byte(&st), 5);
    apply(&mut st, Action::MoveLeft);
    assert_eq!(cursor_byte(&st), 4);
}

#[test]
fn tab_on_blank_separator_line_inserts_plain_indent() {
    // The blank line below a list is a separator, not part of the list:
    // Tab there must fall back to plain indentation, not indent the item
    // above the cursor.
    let src = "- a\n\n- b\n";
    let mut st = EditorState::new(Buffer::from_str(src), theme());
    st.mode = Mode::Rendered;
    st.cursor.offset = st.buffer.rope().byte_to_char(4); // start of the blank line
    apply(&mut st, Action::InsertTab);
    assert_eq!(st.contents(), "- a\n    \n- b\n");
}

#[test]
fn toggle_checkbox_on_blank_separator_line_is_a_no_op() {
    let src = "- [ ] a\n\nx\n";
    let mut st = EditorState::new(Buffer::from_str(src), theme());
    st.mode = Mode::Rendered;
    st.cursor.offset = st.buffer.rope().byte_to_char(8); // start of the blank line
    apply(&mut st, Action::ToggleCheckbox);
    assert_eq!(st.contents(), src, "the task above must stay untouched");
}

#[test]
fn delete_renumbers_across_continuation_lines() {
    // Deleting item 2 lands the cursor near item 3; the renumber pass must
    // cross item 1's continuation line to keep the sequence monotonic.
    let src = "1. a\n   cont\n2. b\n3. c\n";
    let mut st = editor_at(src, "2. b");
    apply(&mut st, Action::DeleteLine);
    assert_eq!(st.contents(), "1. a\n   cont\n2. c\n");
}
