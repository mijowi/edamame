//! Integration tests for the vim modal-editing reducer.
//!
//! Following the project's `mouse_ops` testing convention, these treat
//! `vim_feed` as a pure function of `(VimState, EditorState, key)` and
//! assert on the resulting state — no terminal, no `App`.  CP1 covers
//! the walking skeleton: `h j k l` motion, `i a I A` Insert entries,
//! `Esc` transitions, count accumulation, and the Normal/Insert
//! passthrough contract.  CP2 adds the core motions (`w e b W E B 0 ^ $
//! gg G`), the `o`/`O` open-line entries, and `v`/`V` Visual entry.  CP3
//! adds the operator+motion reducer; CP4 adds the remaining Normal
//! primitives (`r{c} ~ >> << J u`, and `Ctrl-R` redo via the keymap).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use edamame::config::{Action, KeyBindingOverrides, KeyMap, Theme};
use edamame::document::{Buffer, Selection};
use edamame::editor::{EditorState, Mode};
use edamame::input::{vim_feed, VimOutcome, VimState, VimSubMode};

const VH: usize = 40;
const VW: usize = 80;

fn theme() -> &'static Theme {
    Box::leak(Box::new(Theme::default()))
}

/// A fresh editor in Rendered mode (vim never rests in Preview).
fn state(text: &str) -> EditorState {
    let mut st = EditorState::new(Buffer::from_str(text), theme());
    st.mode = Mode::Rendered;
    st.update_cursor_block();
    st
}

fn ch(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn esc() -> KeyEvent {
    KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn feed(vim: &mut VimState, st: &mut EditorState, key: KeyEvent) -> VimOutcome {
    vim_feed(vim, st, key, VH, VW)
}

// ── Motion ────────────────────────────────────────────────────────────────────

#[test]
fn hjkl_move_the_cursor() {
    let mut st = state("hello\nworld");
    let mut vim = VimState::default();
    assert_eq!(st.cursor.offset, 0);

    assert_eq!(feed(&mut vim, &mut st, ch('l')), VimOutcome::Consumed);
    assert_eq!(st.cursor.offset, 1);

    feed(&mut vim, &mut st, ch('j'));
    assert_eq!(st.cursor.offset, 7); // line 1, col 1

    feed(&mut vim, &mut st, ch('h'));
    assert_eq!(st.cursor.offset, 6);

    feed(&mut vim, &mut st, ch('k'));
    assert_eq!(st.cursor.offset, 0); // back to line 0, col 0
}

#[test]
fn bare_key_in_normal_is_consumed_without_inserting() {
    let mut st = state("ab");
    let mut vim = VimState::default();
    let out = feed(&mut vim, &mut st, ch('z'));
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(st.buffer.contents(), "ab", "normal-mode key must not type");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
}

#[test]
fn ctrl_chord_passes_through_in_normal() {
    let mut st = state("hi");
    let mut vim = VimState::default();
    let k = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(feed(&mut vim, &mut st, k), VimOutcome::Passthrough);
}

#[test]
fn motion_clears_a_lingering_selection() {
    // A mouse drag can leave a selection active; a Normal-mode motion
    // must drop it (otherwise it keeps painting under the cursor).
    let mut st = state("hello\nworld");
    let mut vim = VimState::default();
    st.selection = Some(Selection {
        anchor: 0,
        active: 3,
    });
    feed(&mut vim, &mut st, ch('l'));
    assert!(st.selection.is_none(), "motion must clear the selection");
}

#[test]
fn horizontal_motion_skips_table_border_chrome_in_rendered_mode() {
    // `l` through a table steps cell-to-cell over the auto-managed border
    // chrome — the cursor must never land on a `|`.
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n";
    let mut st = state(src);
    let mut vim = VimState::default();
    st.cursor.offset = 2; // on 'a' in the header row
    st.update_cursor_block();

    for _ in 0..4 {
        feed(&mut vim, &mut st, ch('l'));
        let off = st
            .cursor
            .offset
            .min(st.buffer.len_chars().saturating_sub(1));
        assert_ne!(
            st.buffer.rope().char(off),
            '|',
            "rendered-mode horizontal motion must skip the table border chrome"
        );
    }
}

#[test]
fn horizontal_motion_traverses_borders_in_raw_mode() {
    // In Raw mode the borders are real, editable source, so `l` walks onto
    // the `|` character rather than skipping it.
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n";
    let mut st = state(src);
    st.mode = Mode::Raw;
    st.cursor.offset = 2; // on 'a'
    st.update_cursor_block();
    let mut vim = VimState::default();

    feed(&mut vim, &mut st, ch('l')); // -> ' '
    feed(&mut vim, &mut st, ch('l')); // -> '|'
    assert_eq!(
        st.buffer.rope().char(st.cursor.offset),
        '|',
        "raw-mode horizontal motion steps onto the border character"
    );
}

#[test]
fn j_skips_the_table_alignment_row_in_rendered_mode() {
    // `j` from the header row must skip the structural `|---|---|` row and
    // land on the first data row — the same rule the default handler's
    // MoveDown follows in a rendered view.
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n";
    let mut st = state(src); // cursor at offset 0, on the header row
    let mut vim = VimState::default();

    feed(&mut vim, &mut st, ch('j'));

    let (line, _) = st.cursor.line_col(&st.buffer);
    let line_text = st.buffer.line(line).unwrap_or_default();
    assert!(
        line_text.contains("11"),
        "expected cursor on the data row, got line {line}: {line_text:?}"
    );
}

#[test]
fn j_does_not_skip_the_alignment_row_in_raw_mode() {
    // In Raw mode every line is genuine, editable source, so `j` lands on
    // the alignment row rather than skipping it.
    let src = "| a | b |\n|---|---|\n| 11 | 22 |\n";
    let mut st = state(src);
    st.mode = Mode::Raw;
    st.update_cursor_block();
    let mut vim = VimState::default();

    feed(&mut vim, &mut st, ch('j'));

    let (line, _) = st.cursor.line_col(&st.buffer);
    let line_text = st.buffer.line(line).unwrap_or_default();
    assert!(
        line_text.contains("---"),
        "expected cursor on the alignment row, got line {line}: {line_text:?}"
    );
}

// ── Insert entries ──────────────────────────────────────────────────────────

#[test]
fn i_enters_insert_and_chars_pass_through() {
    let mut st = state("ab");
    let mut vim = VimState::default();

    assert_eq!(feed(&mut vim, &mut st, ch('i')), VimOutcome::Consumed);
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(st.cursor.offset, 0, "`i` inserts before the cursor");

    // In Insert, printable chars fall through to the editing pipeline —
    // vim_feed does not type them itself.
    assert_eq!(feed(&mut vim, &mut st, ch('x')), VimOutcome::Passthrough);
    assert_eq!(st.buffer.contents(), "ab");
}

#[test]
fn esc_from_insert_returns_to_normal_and_moves_left() {
    let mut st = state("ab");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('i'));
    st.cursor.offset = 1; // simulate having typed one char

    assert_eq!(feed(&mut vim, &mut st, esc()), VimOutcome::Consumed);
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert_eq!(st.cursor.offset, 0, "Esc moves one char left");
}

#[test]
fn esc_from_insert_at_line_start_does_not_cross_lines() {
    let mut st = state("ab\ncd");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('i'));
    st.cursor.offset = 3; // start of line 1 ('c')

    feed(&mut vim, &mut st, esc());
    assert_eq!(
        st.cursor.offset, 3,
        "Esc never crosses to the previous line"
    );
}

#[test]
fn a_appends_after_the_cursor() {
    let mut st = state("ab");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('a'));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(st.cursor.offset, 1);
}

#[test]
fn capital_i_moves_to_first_non_blank() {
    let mut st = state("   foo");
    let mut vim = VimState::default();
    st.cursor.offset = 5; // inside "foo"
    feed(&mut vim, &mut st, ch('I'));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(st.cursor.offset, 3, "first non-blank is 'f'");
}

#[test]
fn shifted_capital_letter_is_handled_like_a_bare_uppercase_char() {
    // Real terminals deliver `I` as `Char('I')` with the SHIFT modifier.
    // The reducer's Ctrl/Alt/Super guard must not swallow SHIFT, so `I`
    // still routes to the first-non-blank Insert entry.
    let mut st = state("   foo");
    let mut vim = VimState::default();
    st.cursor.offset = 5; // inside "foo"
    let shifted_i = KeyEvent::new(KeyCode::Char('I'), KeyModifiers::SHIFT);
    assert_eq!(feed(&mut vim, &mut st, shifted_i), VimOutcome::Consumed);
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(st.cursor.offset, 3, "first non-blank is 'f'");
}

#[test]
fn capital_a_moves_to_line_end() {
    let mut st = state("abc\ndef");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('A'));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(st.cursor.offset, 3, "end of \"abc\", before the newline");
}

// ── Counts ────────────────────────────────────────────────────────────────────

#[test]
fn digits_accumulate_count_and_esc_clears_it() {
    let mut st = state("hello");
    let mut vim = VimState::default();

    assert_eq!(feed(&mut vim, &mut st, ch('3')), VimOutcome::Pending);
    assert_eq!(vim.count, Some(3));
    assert_eq!(feed(&mut vim, &mut st, ch('2')), VimOutcome::Pending);
    assert_eq!(vim.count, Some(32));

    feed(&mut vim, &mut st, esc());
    assert_eq!(vim.count, None, "Esc clears the in-progress count");
}

#[test]
fn leading_zero_is_not_a_count_digit() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    // A leading `0` is the line-start motion (consumed, no count), not a
    // digit — so it must not start a count.
    let out = feed(&mut vim, &mut st, ch('0'));
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(vim.count, None);
}

#[test]
fn zero_after_a_digit_extends_the_count() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('1'));
    assert_eq!(feed(&mut vim, &mut st, ch('0')), VimOutcome::Pending);
    assert_eq!(vim.count, Some(10));
}

// ── Core motions (CP2) ──────────────────────────────────────────────────────

#[test]
fn word_motions_land_on_boundaries() {
    let mut st = state("foo.bar baz");
    let mut vim = VimState::default();

    // w: 'f' → '.' → 'bar' → 'baz'.
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.cursor.offset, 3); // '.'
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.cursor.offset, 4); // 'b' of bar
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.cursor.offset, 8); // 'b' of baz

    // b walks back to word starts.
    feed(&mut vim, &mut st, ch('b'));
    assert_eq!(st.cursor.offset, 4);

    // e lands on the last char of the next word.
    st.cursor.offset = 0;
    feed(&mut vim, &mut st, ch('e'));
    assert_eq!(st.cursor.offset, 2); // 'o' of foo
}

#[test]
fn big_word_motions_ignore_punctuation() {
    let mut st = state("foo.bar baz");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('W'));
    assert_eq!(st.cursor.offset, 8, "W jumps the whole foo.bar blob");

    st.cursor.offset = 10; // 'z'
    feed(&mut vim, &mut st, ch('B'));
    assert_eq!(st.cursor.offset, 8, "B back to start of baz");

    st.cursor.offset = 0;
    feed(&mut vim, &mut st, ch('E'));
    assert_eq!(st.cursor.offset, 6, "E to the end of foo.bar");
}

#[test]
fn line_motions_zero_caret_dollar() {
    let mut st = state("  hello world\nnext");
    let mut vim = VimState::default();
    st.cursor.offset = 6; // inside "hello"

    feed(&mut vim, &mut st, ch('0'));
    assert_eq!(st.cursor.offset, 0, "0 → line start");

    feed(&mut vim, &mut st, ch('^'));
    assert_eq!(st.cursor.offset, 2, "^ → first non-blank");

    feed(&mut vim, &mut st, ch('$'));
    assert_eq!(st.cursor.offset, 13, "$ → end of line before the newline");
}

#[test]
fn gg_and_capital_g_jump_document_ends() {
    let mut st = state("  first\nmiddle\nlast");
    let mut vim = VimState::default();
    st.cursor.offset = 10; // inside "middle"

    feed(&mut vim, &mut st, ch('G'));
    assert_eq!(st.cursor.offset, 15, "G → first non-blank of last line");

    // gg is a two-key sequence: the first g is Pending.
    assert_eq!(feed(&mut vim, &mut st, ch('g')), VimOutcome::Pending);
    assert!(vim.pending_g);
    assert_eq!(feed(&mut vim, &mut st, ch('g')), VimOutcome::Consumed);
    assert_eq!(st.cursor.offset, 2, "gg → first non-blank of first line");
    assert!(!vim.pending_g);
}

#[test]
fn lone_g_followed_by_other_key_is_a_noop() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    st.cursor.offset = 3;
    feed(&mut vim, &mut st, ch('g'));
    let out = feed(&mut vim, &mut st, ch('x'));
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(st.cursor.offset, 3, "unknown g-command does not move");
    assert!(!vim.pending_g);
}

#[test]
fn pending_g_is_resolved_before_a_count_digit() {
    // A pending `g` must be consumed by the very next key, even a digit, so
    // it can never linger while the digit grows the count (which would let a
    // later lone `g` fire `gg`).
    let mut st = state("hello");
    let mut vim = VimState::default();
    st.cursor.offset = 3;

    feed(&mut vim, &mut st, ch('g'));
    assert!(vim.pending_g);
    let out = feed(&mut vim, &mut st, ch('5'));
    assert_eq!(
        out,
        VimOutcome::Consumed,
        "g then digit is a swallowed no-op"
    );
    assert!(!vim.pending_g, "pending_g must not dangle past the digit");
    assert_eq!(vim.count, None, "the digit does not accumulate a count");
    assert_eq!(st.cursor.offset, 3, "no motion fires");
}

// ── o / O (CP2) ───────────────────────────────────────────────────────────────

#[test]
fn o_opens_a_line_below_and_enters_insert() {
    let mut st = state("abc\ndef");
    let mut vim = VimState::default();
    st.cursor.offset = 1; // inside "abc"

    feed(&mut vim, &mut st, ch('o'));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(st.buffer.contents(), "abc\n\ndef");
    assert_eq!(st.cursor.offset, 4, "cursor on the new blank line below");
}

#[test]
fn capital_o_opens_a_line_above_and_enters_insert() {
    let mut st = state("abc\ndef");
    let mut vim = VimState::default();
    let line1 = st.buffer.line_to_char(1);
    st.cursor.offset = line1 + 1; // inside "def"

    feed(&mut vim, &mut st, ch('O'));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(st.buffer.contents(), "abc\n\ndef");
    assert_eq!(
        st.cursor.offset, line1,
        "cursor on the new blank line above"
    );
}

#[test]
fn o_on_the_last_line_appends_a_new_line() {
    let mut st = state("only");
    let mut vim = VimState::default();
    st.cursor.offset = 2;

    feed(&mut vim, &mut st, ch('o'));
    assert_eq!(st.buffer.contents(), "only\n");
    assert_eq!(
        st.cursor.offset, 5,
        "cursor on the freshly-opened last line"
    );
}

#[test]
fn open_line_is_a_single_undo_unit() {
    // `o`/`O` must record exactly one EditDelta so a later `u` reverses
    // the whole open-line in one step (Risk #2 in the plan).
    let mut st = state("abc\ndef");
    let mut vim = VimState::default();
    st.cursor.offset = 1; // inside "abc"

    feed(&mut vim, &mut st, ch('o'));
    assert_eq!(st.buffer.contents(), "abc\n\ndef");
    assert_eq!(st.history.undo_depth(), 1, "o records exactly one delta");

    // A single undo restores the original buffer in one step.
    assert!(st.history.undo(&mut st.buffer).is_some());
    assert_eq!(st.buffer.contents(), "abc\ndef");

    // `O` is likewise one unit.
    let mut st = state("abc\ndef");
    let mut vim = VimState::default();
    let line1 = st.buffer.line_to_char(1);
    st.cursor.offset = line1 + 1; // inside "def"

    feed(&mut vim, &mut st, ch('O'));
    assert_eq!(st.buffer.contents(), "abc\n\ndef");
    assert_eq!(st.history.undo_depth(), 1, "O records exactly one delta");

    assert!(st.history.undo(&mut st.buffer).is_some());
    assert_eq!(st.buffer.contents(), "abc\ndef");
}

// ── Visual entry (CP2) ─────────────────────────────────────────────────────────

#[test]
fn v_enters_visual_and_anchors_the_selection() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    st.cursor.offset = 1;

    assert_eq!(feed(&mut vim, &mut st, ch('v')), VimOutcome::Consumed);
    assert_eq!(vim.sub_mode, VimSubMode::Visual);
    assert_eq!(vim.visual_anchor, Some(1));
    let sel = st.selection.expect("v installs a selection");
    assert_eq!((sel.anchor, sel.active), (1, 1));
}

#[test]
fn capital_v_enters_visual_line() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    assert_eq!(vim.sub_mode, VimSubMode::VisualLine);
    assert!(st.selection.is_some());
}

#[test]
fn motions_extend_the_selection_in_visual() {
    let mut st = state("hello world");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v')); // anchor at 0
    feed(&mut vim, &mut st, ch('w')); // → start of "world" (6)

    let sel = st.selection.expect("selection persists through motion");
    assert_eq!(sel.anchor, 0, "anchor stays put");
    assert_eq!(sel.active, 6, "active follows the cursor");
    assert_eq!(st.cursor.offset, 6);
}

#[test]
fn esc_leaves_visual_and_clears_the_selection() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('l'));
    assert!(st.selection.is_some());

    feed(&mut vim, &mut st, esc());
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert!(st.selection.is_none(), "Esc drops the visual selection");
    assert_eq!(vim.visual_anchor, None);
}

#[test]
fn hjkl_extend_the_selection_in_visual() {
    let mut st = state("hello\nworld");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v')); // anchor at 0
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l'));
    let sel = st.selection.expect("selection persists");
    assert_eq!((sel.anchor, sel.active), (0, 2));
}

#[test]
fn arrow_keys_extend_the_selection_in_visual_like_hjkl() {
    // Arrow keys must behave exactly like `h j k l` in Visual — extend the
    // selection rather than passing through (which would clear it).
    let mut st = state("hello\nworld");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v')); // anchor at 0

    assert_eq!(
        feed(&mut vim, &mut st, key(KeyCode::Right)),
        VimOutcome::Consumed,
        "Right must be consumed in Visual, not passed through"
    );
    feed(&mut vim, &mut st, key(KeyCode::Right));
    let sel = st
        .selection
        .expect("selection persists through arrow motion");
    assert_eq!((sel.anchor, sel.active), (0, 2));

    feed(&mut vim, &mut st, key(KeyCode::Down));
    let sel = st.selection.expect("selection persists across a line");
    assert_eq!(sel.anchor, 0);
    assert_eq!(sel.active, st.cursor.offset);
}

#[test]
fn arrow_keys_in_normal_pass_through() {
    // Outside Visual, arrows keep their default-handler bindings.
    let mut st = state("hello");
    let mut vim = VimState::default();
    assert_eq!(
        feed(&mut vim, &mut st, key(KeyCode::Right)),
        VimOutcome::Passthrough,
        "Normal-mode arrows fall through to the default keymap"
    );
}

// ── Counts drive motions (CP3) ──────────────────────────────────────────────

#[test]
fn count_repeats_a_vertical_motion() {
    let mut st = state("l0\nl1\nl2\nl3\nl4");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('j'));
    let (line, _) = st.cursor.line_col(&st.buffer);
    assert_eq!(line, 3, "3j moves down three lines");
    assert_eq!(vim.count, None, "count clears after the motion");
}

#[test]
fn count_repeats_a_word_motion() {
    let mut st = state("aa bb cc dd ee");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.cursor.offset, 9, "3w lands on the start of the 4th word");
}

// ── Operator + motion (CP3) ─────────────────────────────────────────────────

#[test]
fn dw_deletes_a_word() {
    let mut st = state("foo bar baz");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.buffer.contents(), "bar baz");
    assert_eq!(st.cursor.offset, 0);
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
}

#[test]
fn three_dw_is_a_single_undo_unit() {
    // Risk #2: an operator must issue exactly one delta so `3dw` reverses
    // in one `u`.
    let mut st = state("foo bar baz qux");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.buffer.contents(), "qux");
    assert_eq!(st.history.undo_depth(), 1, "3dw records exactly one delta");
    assert!(st.history.undo(&mut st.buffer).is_some());
    assert_eq!(st.buffer.contents(), "foo bar baz qux");
}

#[test]
fn d2w_and_2dw_both_delete_two_words() {
    // `[op][count][motion]` and `[count][op][motion]` are equivalent.
    let mut a = state("a b c d");
    let mut va = VimState::default();
    feed(&mut va, &mut a, ch('d'));
    feed(&mut va, &mut a, ch('2'));
    feed(&mut va, &mut a, ch('w'));
    assert_eq!(a.buffer.contents(), "c d");

    let mut b = state("a b c d");
    let mut vb = VimState::default();
    feed(&mut vb, &mut b, ch('2'));
    feed(&mut vb, &mut b, ch('d'));
    feed(&mut vb, &mut b, ch('w'));
    assert_eq!(b.buffer.contents(), "c d");
}

#[test]
fn counts_multiply_across_operator_and_motion() {
    // `2d3w` deletes 2*3 = 6 words.
    let mut st = state("a b c d e f g h");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('2'));
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.buffer.contents(), "g h");
}

#[test]
fn de_deletes_to_word_end_inclusive() {
    let mut st = state("foo bar");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('e'));
    assert_eq!(st.buffer.contents(), " bar", "de deletes 'foo' inclusively");
}

#[test]
fn dollar_delete_clears_to_end_of_line() {
    let mut st = state("hello world\nnext");
    let mut vim = VimState::default();
    st.cursor.offset = 6; // on 'w'
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('$'));
    assert_eq!(st.buffer.contents(), "hello \nnext");
}

#[test]
fn dw_on_last_word_does_not_join_lines() {
    // vim's rule: `dw` on the final word of a line stops at the line end.
    let mut st = state("foo\nbar");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.buffer.contents(), "\nbar");
}

// ── dd / yy / p (CP3) ───────────────────────────────────────────────────────

#[test]
fn dd_deletes_a_line_as_one_undo_unit() {
    let mut st = state("one\ntwo\nthree");
    let mut vim = VimState::default();
    let l1 = st.buffer.line_to_char(1);
    st.cursor.offset = l1; // on "two"
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('d'));
    assert_eq!(st.buffer.contents(), "one\nthree");
    assert_eq!(st.history.undo_depth(), 1);
    assert!(vim.register.linewise);
    assert_eq!(vim.register.text, "two\n");
}

#[test]
fn count_dd_deletes_multiple_lines() {
    let mut st = state("a\nb\nc\nd");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('2'));
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('d'));
    assert_eq!(st.buffer.contents(), "c\nd");
}

#[test]
fn dd_on_last_line_removes_trailing_line_cleanly() {
    let mut st = state("one\ntwo");
    let mut vim = VimState::default();
    let l1 = st.buffer.line_to_char(1);
    st.cursor.offset = l1;
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('d'));
    assert_eq!(st.buffer.contents(), "one", "no stray blank line remains");
}

#[test]
fn yy_then_p_duplicates_the_line_below() {
    let mut st = state("one\ntwo");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('y'));
    feed(&mut vim, &mut st, ch('y'));
    assert_eq!(st.buffer.contents(), "one\ntwo", "yank does not mutate");
    assert!(vim.register.linewise);
    feed(&mut vim, &mut st, ch('p'));
    assert_eq!(st.buffer.contents(), "one\none\ntwo");
    let (line, _) = st.cursor.line_col(&st.buffer);
    assert_eq!(line, 1, "cursor lands on the pasted line below");
}

#[test]
fn capital_p_pastes_a_line_above() {
    let mut st = state("one\ntwo");
    let mut vim = VimState::default();
    let l1 = st.buffer.line_to_char(1);
    st.cursor.offset = l1; // on "two"
    feed(&mut vim, &mut st, ch('y'));
    feed(&mut vim, &mut st, ch('y'));
    feed(&mut vim, &mut st, ch('P'));
    assert_eq!(st.buffer.contents(), "one\ntwo\ntwo");
}

#[test]
fn yy_then_p_on_last_line_inserts_below() {
    let mut st = state("only");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('y'));
    feed(&mut vim, &mut st, ch('y'));
    feed(&mut vim, &mut st, ch('p'));
    assert_eq!(st.buffer.contents(), "only\nonly");
}

// ── Charwise yank / paste (CP3) ─────────────────────────────────────────────

#[test]
fn charwise_yank_and_paste_after_cursor() {
    let mut st = state("abc");
    let mut vim = VimState::default();
    // yw yanks "abc" (whole word at start of line).
    feed(&mut vim, &mut st, ch('y'));
    feed(&mut vim, &mut st, ch('e')); // ye → "abc" inclusive
    assert_eq!(vim.register.text, "abc");
    assert!(!vim.register.linewise);
    assert_eq!(st.cursor.offset, 0, "yank parks the cursor at span start");
    feed(&mut vim, &mut st, ch('p'));
    assert_eq!(st.buffer.contents(), "aabcbc", "p inserts after the cursor");
}

// ── cw / cc (CP3) ───────────────────────────────────────────────────────────

#[test]
fn cw_changes_word_and_enters_insert_keeping_trailing_space() {
    // vim special case: `cw` acts like `ce`, leaving the space after.
    let mut st = state("foo bar");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('c'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(st.buffer.contents(), " bar", "cw leaves the trailing space");
    assert_eq!(st.cursor.offset, 0);
}

// `cw`/`cW` change to the end of the word the cursor is *in*, never running
// into the next word — the regression matrix for the `CurrentWordEnd` fix.

#[test]
fn cw_on_single_char_word_changes_only_that_word() {
    // Cursor on a single-char word is simultaneously at its start and end;
    // `cw` must change just it, not spill into the next word.
    let mut st = state("a b c");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('c'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.buffer.contents(), " b c", "cw changes only 'a'");
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(st.cursor.offset, 0);
}

#[test]
fn cw_on_last_char_of_word_changes_only_that_char() {
    let mut st = state("foo bar");
    let mut vim = VimState::default();
    st.cursor.offset = 2; // last 'o' of "foo"
    feed(&mut vim, &mut st, ch('c'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.buffer.contents(), "fo bar", "cw changes only the 'o'");
}

#[test]
fn cw_mid_word_changes_to_end_of_that_word() {
    let mut st = state("hello world");
    let mut vim = VimState::default();
    st.cursor.offset = 2; // first 'l' of "hello"
    feed(&mut vim, &mut st, ch('c'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.buffer.contents(), "he world", "cw changes 'llo'");
}

#[test]
fn c2w_changes_two_words_without_overshooting() {
    // `c2w` from a word start changes exactly two words (counting the
    // current word as the first), not three.
    let mut st = state("a b c d");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('c'));
    feed(&mut vim, &mut st, ch('2'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.buffer.contents(), " c d", "c2w changes 'a b' only");
}

#[test]
fn cw_on_punctuation_changes_the_punct_run() {
    // Word-class separation: `cw` on a punctuation run changes just that
    // run, not the following identifier.
    let mut st = state("a->b");
    let mut vim = VimState::default();
    st.cursor.offset = 1; // on '-'
    feed(&mut vim, &mut st, ch('c'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.buffer.contents(), "ab", "cw changes the '->' run");
}

#[test]
fn capital_cw_changes_whole_bigword() {
    // `cW` ignores punctuation: on "foo.bar baz" it changes "foo.bar".
    let mut st = state("foo.bar baz");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('c'));
    feed(&mut vim, &mut st, ch('W'));
    assert_eq!(st.buffer.contents(), " baz", "cW changes the whole blob");
}

#[test]
fn cc_clears_the_line_keeps_it_and_enters_insert() {
    let mut st = state("one\ntwo\nthree");
    let mut vim = VimState::default();
    let l1 = st.buffer.line_to_char(1);
    st.cursor.offset = l1 + 1; // inside "two"
    feed(&mut vim, &mut st, ch('c'));
    feed(&mut vim, &mut st, ch('c'));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(
        st.buffer.contents(),
        "one\n\nthree",
        "line kept, content cleared"
    );
    assert_eq!(st.cursor.offset, l1);
}

// ── D / C / Y (CP3) ─────────────────────────────────────────────────────────

#[test]
fn capital_d_deletes_to_end_of_line() {
    let mut st = state("hello world");
    let mut vim = VimState::default();
    st.cursor.offset = 5; // on the space
    feed(&mut vim, &mut st, ch('D'));
    assert_eq!(st.buffer.contents(), "hello");
}

#[test]
fn capital_c_changes_to_end_of_line_and_enters_insert() {
    let mut st = state("hello world");
    let mut vim = VimState::default();
    st.cursor.offset = 6; // on 'w'
    feed(&mut vim, &mut st, ch('C'));
    assert_eq!(st.buffer.contents(), "hello ");
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
}

#[test]
fn capital_y_yanks_the_whole_line() {
    let mut st = state("one\ntwo");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('Y'));
    assert!(vim.register.linewise);
    assert_eq!(vim.register.text, "one\n");
    assert_eq!(st.buffer.contents(), "one\ntwo", "Y does not mutate");
}

// ── x / X (CP3) ─────────────────────────────────────────────────────────────

#[test]
fn x_deletes_char_under_cursor_with_count() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('x'));
    assert_eq!(st.buffer.contents(), "ello");
    feed(&mut vim, &mut st, ch('2'));
    feed(&mut vim, &mut st, ch('x'));
    assert_eq!(st.buffer.contents(), "lo", "2x removes two chars");
    assert_eq!(vim.register.text, "el");
}

#[test]
fn x_at_line_end_does_not_cross_the_newline() {
    let mut st = state("ab\ncd");
    let mut vim = VimState::default();
    st.cursor.offset = 2; // at the newline boundary (end of "ab")
    feed(&mut vim, &mut st, ch('x'));
    assert_eq!(st.buffer.contents(), "ab\ncd", "x at line end is a no-op");
}

#[test]
fn capital_x_deletes_char_before_cursor() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    st.cursor.offset = 3; // on second 'l'
    feed(&mut vim, &mut st, ch('X'));
    assert_eq!(st.buffer.contents(), "helo");
    assert_eq!(st.cursor.offset, 2);
}

#[test]
fn capital_x_at_line_start_is_a_noop() {
    let mut st = state("ab\ncd");
    let mut vim = VimState::default();
    let l1 = st.buffer.line_to_char(1);
    st.cursor.offset = l1; // start of "cd"
    feed(&mut vim, &mut st, ch('X'));
    assert_eq!(st.buffer.contents(), "ab\ncd", "X at line start is a no-op");
}

// ── dj / dk linewise (CP3) ──────────────────────────────────────────────────

#[test]
fn dj_deletes_two_lines() {
    let mut st = state("a\nb\nc\nd");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('j'));
    assert_eq!(
        st.buffer.contents(),
        "c\nd",
        "dj deletes the cursor line and the next"
    );
}

#[test]
fn dgg_deletes_up_to_the_first_line() {
    let mut st = state("a\nb\nc\nd");
    let mut vim = VimState::default();
    let l2 = st.buffer.line_to_char(2);
    st.cursor.offset = l2; // on "c"
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('g'));
    feed(&mut vim, &mut st, ch('g'));
    assert_eq!(st.buffer.contents(), "d", "dgg removes lines 0..=2");
}

// ── Operator cancellation (CP3) ─────────────────────────────────────────────

#[test]
fn esc_cancels_a_pending_operator() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    assert_eq!(feed(&mut vim, &mut st, ch('d')), VimOutcome::Pending);
    assert_eq!(vim.sub_mode, VimSubMode::OperatorPending);
    feed(&mut vim, &mut st, esc());
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert_eq!(vim.pending_op, None);
    assert_eq!(st.buffer.contents(), "hello");
}

#[test]
fn passthrough_chord_cancels_a_pending_operator() {
    // `d` then a `Ctrl-*` chord: the chord falls through to the default
    // handler, and the half-typed operator must not linger (otherwise the
    // next key would be read as a delete target).
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    assert_eq!(vim.sub_mode, VimSubMode::OperatorPending);
    let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(feed(&mut vim, &mut st, ctrl_s), VimOutcome::Passthrough);
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert_eq!(vim.pending_op, None);
    // The next motion behaves as a fresh Normal-mode key, not a target.
    feed(&mut vim, &mut st, ch('l'));
    assert_eq!(st.cursor.offset, 1);
    assert_eq!(st.buffer.contents(), "hello", "no edit occurred");
}

#[test]
fn passthrough_chord_clears_a_partial_count() {
    // A count interrupted by a chord must not survive to the next key.
    let mut st = state("hello world");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('3'));
    let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
    feed(&mut vim, &mut st, ctrl_s);
    assert_eq!(vim.count, None);
    feed(&mut vim, &mut st, ch('l'));
    assert_eq!(
        st.cursor.offset, 1,
        "the stale count did not repeat the move"
    );
}

#[test]
fn invalid_operator_target_cancels_without_editing() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    let out = feed(&mut vim, &mut st, ch('z'));
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert_eq!(st.buffer.contents(), "hello", "an invalid target is inert");
}

// ── CP4: replace (`r{c}`) ───────────────────────────────────────────────────────

#[test]
fn r_replaces_the_char_under_the_cursor() {
    let mut st = state("abc");
    let mut vim = VimState::default();
    assert_eq!(feed(&mut vim, &mut st, ch('r')), VimOutcome::Pending);
    assert!(vim.pending_replace);
    feed(&mut vim, &mut st, ch('x'));
    assert_eq!(st.buffer.contents(), "xbc");
    assert_eq!(st.cursor.offset, 0, "cursor stays on the replaced char");
    assert!(!vim.pending_replace, "replace consumed the pending state");
}

#[test]
fn r_with_count_replaces_multiple_chars() {
    let mut st = state("abcde");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('r'));
    feed(&mut vim, &mut st, ch('z'));
    assert_eq!(st.buffer.contents(), "zzzde");
    assert_eq!(
        st.cursor.offset, 2,
        "cursor lands on the last replaced char"
    );
}

#[test]
fn r_is_a_noop_when_fewer_chars_remain_than_count() {
    let mut st = state("ab");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('r'));
    feed(&mut vim, &mut st, ch('x'));
    assert_eq!(st.buffer.contents(), "ab", "not enough room → no edit");
}

#[test]
fn r_never_replaces_across_the_newline() {
    // `2r` on the last char of a line would need a char from the next line;
    // vim refuses (a `r` never touches the newline).
    let mut st = state("ab\ncd");
    let mut vim = VimState::default();
    st.cursor.offset = 1; // on 'b'
    feed(&mut vim, &mut st, ch('2'));
    feed(&mut vim, &mut st, ch('r'));
    feed(&mut vim, &mut st, ch('x'));
    assert_eq!(
        st.buffer.contents(),
        "ab\ncd",
        "no spill onto the next line"
    );
}

#[test]
fn r_then_esc_cancels_without_editing() {
    let mut st = state("abc");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('r'));
    let out = feed(&mut vim, &mut st, esc());
    assert_eq!(out, VimOutcome::Consumed);
    assert!(!vim.pending_replace);
    assert_eq!(st.buffer.contents(), "abc", "Esc aborts the replace");
}

// ── CP4: toggle case (`~`) ───────────────────────────────────────────────────────

#[test]
fn tilde_toggles_case_and_advances() {
    let mut st = state("aBc");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('~'));
    assert_eq!(st.buffer.contents(), "ABc");
    assert_eq!(st.cursor.offset, 1, "~ advances past the toggled char");
}

#[test]
fn tilde_with_count_toggles_a_run() {
    let mut st = state("aBcD");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('~'));
    assert_eq!(st.buffer.contents(), "AbCD");
    assert_eq!(st.cursor.offset, 3);
}

#[test]
fn tilde_clamps_to_the_line_content() {
    // A count larger than the remaining chars toggles only to the line end.
    let mut st = state("aB\ncd");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('9'));
    feed(&mut vim, &mut st, ch('~'));
    assert_eq!(st.buffer.contents(), "Ab\ncd", "stops before the newline");
}

#[test]
fn tilde_passes_non_cased_chars_through_unchanged() {
    // The `.` in the middle has no case; it is left as-is while the cased
    // chars around it still toggle, and the cursor still advances past it.
    let mut st = state("a.B");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('~'));
    assert_eq!(st.buffer.contents(), "A.b", "non-cased char untouched");
    assert_eq!(st.cursor.offset, 3, "cursor advances over the whole run");
}

// ── CP4: join (`J`) ──────────────────────────────────────────────────────────────

#[test]
fn j_joins_the_next_line_with_a_space() {
    let mut st = state("foo\nbar");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('J'));
    assert_eq!(st.buffer.contents(), "foo bar");
    assert_eq!(st.cursor.offset, 3, "cursor lands on the join column");
}

#[test]
fn j_strips_leading_whitespace_of_the_joined_line() {
    let mut st = state("foo\n    bar");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('J'));
    assert_eq!(
        st.buffer.contents(),
        "foo bar",
        "one space, indent stripped"
    );
}

#[test]
fn j_with_count_joins_multiple_lines_in_one_undo() {
    let mut st = state("a\nb\nc");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('J'));
    assert_eq!(st.buffer.contents(), "a b c");
    assert_eq!(st.history.undo_depth(), 1, "3J is a single undo unit");
    st.history.undo(&mut st.buffer);
    assert_eq!(st.buffer.contents(), "a\nb\nc");
}

#[test]
fn j_on_the_last_line_is_a_noop() {
    let mut st = state("only");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('J'));
    assert_eq!(st.buffer.contents(), "only", "nothing below to join");
}

// ── CP4: indent / outdent (`>>` / `<<`) ──────────────────────────────────────────

#[test]
fn double_indent_adds_a_tab_width_of_spaces() {
    let mut st = state("foo");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('>'));
    assert_eq!(vim.sub_mode, VimSubMode::OperatorPending);
    feed(&mut vim, &mut st, ch('>'));
    assert_eq!(st.buffer.contents(), "    foo");
    assert_eq!(st.cursor.offset, 4, "cursor on the first non-blank");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
}

#[test]
fn double_outdent_strips_a_tab_width_of_spaces() {
    let mut st = state("    foo");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('<'));
    feed(&mut vim, &mut st, ch('<'));
    assert_eq!(st.buffer.contents(), "foo");
}

#[test]
fn indent_then_outdent_round_trips() {
    let mut st = state("foo\nbar");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('>'));
    feed(&mut vim, &mut st, ch('>'));
    assert_eq!(st.buffer.contents(), "    foo\nbar");
    feed(&mut vim, &mut st, ch('<'));
    feed(&mut vim, &mut st, ch('<'));
    assert_eq!(st.buffer.contents(), "foo\nbar");
}

#[test]
fn indent_with_count_spans_lines_in_one_undo() {
    let mut st = state("a\nb\nc");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('>'));
    feed(&mut vim, &mut st, ch('>'));
    assert_eq!(st.buffer.contents(), "    a\n    b\n    c");
    assert_eq!(st.history.undo_depth(), 1, "3>> is a single undo unit");
}

#[test]
fn outdent_of_a_flush_line_is_a_noop() {
    let mut st = state("foo");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('<'));
    feed(&mut vim, &mut st, ch('<'));
    assert_eq!(st.buffer.contents(), "foo");
    assert_eq!(st.history.undo_depth(), 0, "no delta recorded");
}

#[test]
fn outdent_strips_a_single_leading_tab() {
    // A leading tab counts as one indent step regardless of tab_width.
    let mut st = state("\tfoo");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('<'));
    feed(&mut vim, &mut st, ch('<'));
    assert_eq!(st.buffer.contents(), "foo", "the leading tab is removed");
}

#[test]
fn indent_leaves_blank_lines_within_the_range_empty() {
    // `3>>` spans a blank middle line; vim never indents an empty line, so
    // it stays empty while the surrounding content lines are indented.
    let mut st = state("a\n\nc");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('>'));
    feed(&mut vim, &mut st, ch('>'));
    assert_eq!(
        st.buffer.contents(),
        "    a\n\n    c",
        "blank line untouched"
    );
}

// ── CP4: undo / redo (`u` / `Ctrl-R`) ────────────────────────────────────────────

#[test]
fn u_undoes_the_last_change() {
    let mut st = state("foo");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('>'));
    feed(&mut vim, &mut st, ch('>'));
    assert_eq!(st.buffer.contents(), "    foo");
    feed(&mut vim, &mut st, ch('u'));
    assert_eq!(st.buffer.contents(), "foo", "u reverts the indent");
}

#[test]
fn ctrl_r_passes_through_for_the_keymap_to_redo() {
    // Vim claims no `Ctrl-R`; it falls through to the default keymap, which
    // CP4 binds to Redo.  The reducer must report `Passthrough`.
    let mut st = state("foo");
    let mut vim = VimState::default();
    assert_eq!(feed(&mut vim, &mut st, ctrl('r')), VimOutcome::Passthrough);
}

#[test]
fn ctrl_r_is_bound_to_redo_in_the_default_keymap() {
    // The binding fires in both default and vim mode (vim passes through to
    // this same keymap).  Ctrl-Shift-Z stays bound to Redo as well.
    let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
    assert_eq!(km.action_for(&ctrl('r')), Some(&Action::Redo));
    let ctrl_shift_z = KeyEvent::new(
        KeyCode::Char('z'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert_eq!(km.action_for(&ctrl_shift_z), Some(&Action::Redo));
}

// ── CP5: find / paragraph / matching-pair motions ──────────────────────────────

#[test]
fn f_waits_for_a_char_then_lands_on_it() {
    let mut st = state("hello world");
    let mut vim = VimState::default();
    assert_eq!(feed(&mut vim, &mut st, ch('f')), VimOutcome::Pending);
    assert_eq!(feed(&mut vim, &mut st, ch('o')), VimOutcome::Consumed);
    assert_eq!(st.cursor.offset, 4); // first 'o'
}

#[test]
fn t_stops_one_char_before_the_target() {
    let mut st = state("hello world");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('t'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.cursor.offset, 5); // the space before 'w'
}

#[test]
fn count_finds_the_nth_occurrence() {
    let mut st = state("a.b.c.d");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('2'));
    feed(&mut vim, &mut st, ch('f'));
    feed(&mut vim, &mut st, ch('.'));
    assert_eq!(st.cursor.offset, 3); // second '.'
}

#[test]
fn find_miss_leaves_the_cursor_put() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('f'));
    feed(&mut vim, &mut st, ch('z'));
    assert_eq!(st.cursor.offset, 0);
}

#[test]
fn esc_cancels_a_pending_find() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('f'));
    assert!(vim.pending_find.is_some());
    feed(&mut vim, &mut st, esc());
    assert_eq!(vim.pending_find, None);
    assert_eq!(st.cursor.offset, 0);
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
}

#[test]
fn semicolon_replays_and_comma_reverses_the_last_find() {
    let mut st = state("a.b.c.d");
    let mut vim = VimState::default();
    // f. → first '.' (1); ; → second '.' (3); ; → third '.' (5).
    feed(&mut vim, &mut st, ch('f'));
    feed(&mut vim, &mut st, ch('.'));
    assert_eq!(st.cursor.offset, 1);
    feed(&mut vim, &mut st, ch(';'));
    assert_eq!(st.cursor.offset, 3);
    feed(&mut vim, &mut st, ch(';'));
    assert_eq!(st.cursor.offset, 5);
    // , reverses direction → back to the '.' at 3.
    feed(&mut vim, &mut st, ch(','));
    assert_eq!(st.cursor.offset, 3);
}

#[test]
fn semicolon_after_t_skips_the_adjacent_match() {
    // Repeating a `t` must not stay stuck one char before the same target:
    // "abc-d-e-f" — t- lands on 'c'(2); ; advances to 'd'(4) then 'e'(6).
    let mut st = state("abc-d-e-f");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('t'));
    feed(&mut vim, &mut st, ch('-'));
    assert_eq!(st.cursor.offset, 2); // one before the first '-'
    feed(&mut vim, &mut st, ch(';'));
    assert_eq!(st.cursor.offset, 4); // skipped the adjacent '-', one before the second
    feed(&mut vim, &mut st, ch(';'));
    assert_eq!(st.cursor.offset, 6); // one before the third '-'
                                     // No further '-' ahead → ; is a no-op, not a backward jump.
    feed(&mut vim, &mut st, ch(';'));
    assert_eq!(st.cursor.offset, 6);
}

#[test]
fn semicolon_after_capital_t_skips_the_adjacent_match() {
    // The backward form: "a-b-c-d", cursor on 'd'(6).  T- is stuck on 'd'
    // (the '-' at 5 is adjacent); ; must skip back to one after the '-' at 3.
    let mut st = state("a-b-c-d");
    st.cursor.offset = 6;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('T'));
    feed(&mut vim, &mut st, ch('-'));
    assert_eq!(st.cursor.offset, 6); // adjacent '-' → T- can't move
    feed(&mut vim, &mut st, ch(';'));
    assert_eq!(st.cursor.offset, 4); // skipped to one after the '-' at 3
}

#[test]
fn percent_jumps_between_matching_brackets() {
    let mut st = state("(a[b]c)");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('%'));
    assert_eq!(st.cursor.offset, 6); // ( → )
    feed(&mut vim, &mut st, ch('%'));
    assert_eq!(st.cursor.offset, 0); // ) → (
                                     // From the inner '[' → its ']'.
    st.cursor.offset = 2;
    feed(&mut vim, &mut st, ch('%'));
    assert_eq!(st.cursor.offset, 4);
}

#[test]
fn paragraph_motions_move_between_blank_lines() {
    let mut st = state("alpha\n\nbeta");
    let mut vim = VimState::default();
    // } → the blank line (offset 6).
    feed(&mut vim, &mut st, ch('}'));
    assert_eq!(st.cursor.offset, 6);
    // Move into the second paragraph, then { back to the blank line.
    st.cursor.offset = 7; // 'b' of beta
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('{'));
    assert_eq!(st.cursor.offset, 6);
}

#[test]
fn df_deletes_through_the_found_char_inclusive() {
    let mut st = state("hello world");
    let mut vim = VimState::default();
    // dfo → delete "hello" (through the first 'o'), one undo unit.
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('f'));
    feed(&mut vim, &mut st, ch('o'));
    assert_eq!(st.buffer.contents(), " world");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    feed(&mut vim, &mut st, ch('u'));
    assert_eq!(st.buffer.contents(), "hello world", "df is a single undo");
}

#[test]
fn dt_deletes_up_to_but_not_including_the_target() {
    let mut st = state("hello world");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('t'));
    feed(&mut vim, &mut st, ch('o'));
    assert_eq!(st.buffer.contents(), "o world"); // "hell" removed
}

#[test]
fn d_percent_deletes_the_whole_pair() {
    let mut st = state("(abc)d");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('%'));
    assert_eq!(st.buffer.contents(), "d");
}

#[test]
fn find_extends_a_visual_selection() {
    let mut st = state("foobar");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('f'));
    feed(&mut vim, &mut st, ch('b'));
    assert_eq!(vim.sub_mode, VimSubMode::Visual);
    let sel = st.selection.expect("visual selection present");
    assert_eq!(sel.anchor, 0);
    assert_eq!(sel.active, 3); // 'b' of "bar"
}
