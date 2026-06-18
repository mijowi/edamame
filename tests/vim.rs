//! Integration tests for the vim modal-editing reducer.
//!
//! Following the project's `mouse_ops` testing convention, these treat
//! `vim_feed` as a pure function of `(VimState, EditorState, key)` and
//! assert on the resulting state — no terminal, no `App`.  CP1 covers
//! the walking skeleton: `h j k l` motion, `i a I A` Insert entries,
//! `Esc` transitions, count accumulation, and the Normal/Insert
//! passthrough contract.  CP2 adds the core motions (`w e b W E B 0 ^ $
//! gg G`), the `o`/`O` open-line entries, and `v`/`V` Visual entry.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use edamame::config::Theme;
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
