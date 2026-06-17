//! Integration tests for the vim modal-editing reducer.
//!
//! Following the project's `mouse_ops` testing convention, these treat
//! `vim_feed` as a pure function of `(VimState, EditorState, key)` and
//! assert on the resulting state — no terminal, no `App`.  CP1 covers
//! the walking skeleton: `h j k l` motion, `i a I A` Insert entries,
//! `Esc` transitions, count accumulation, and the Normal/Insert
//! passthrough contract.

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
