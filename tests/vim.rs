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
//! primitives (`r{c} ~ >> << J u`, and `Ctrl-R` redo via the keymap).  CP6
//! adds the Visual / VisualLine operators (`d/x y c/s > < ~ J`, `o`
//! swap-ends, `v`/`V` toggle) and the line-expansion of a VisualLine
//! selection for operators and the system clipboard.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use edamame::config::{Action, KeyBindingOverrides, KeyMap, Theme};
use edamame::document::{Buffer, Selection};
use edamame::editor::vim_ops::visual_charwise_range;
use edamame::editor::{EditorState, Mode};
use edamame::input::{vim_feed, VimOutcome, VimState, VimSubMode};
use edamame::search::SearchState;

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

/// Like [`state`] but with an associated file path, so `:w` / `:wq`
/// resolve to a direct save rather than the Save As prompt a never-saved
/// (path-less) buffer triggers.
fn state_with_path(text: &str) -> EditorState {
    let mut buf = Buffer::for_new_file(std::path::Path::new("/tmp/edamame-vim-test.md"));
    buf.insert(0, text);
    let mut st = EditorState::new(buf, theme());
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
fn ctrl_backspace_and_delete_do_not_edit_in_normal() {
    // The default keymap binds Ctrl-Backspace / Ctrl-Delete to word-delete.
    // In Normal they must never mutate the buffer — they move the cursor
    // (left / right) like the plain Backspace / Delete keys instead.
    let ctrl_bs = KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL);
    let ctrl_del = KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL);

    let mut st = state("foo bar");
    let mut vim = VimState::default();
    st.cursor.offset = 4; // start of "bar"

    assert_eq!(feed(&mut vim, &mut st, ctrl_bs), VimOutcome::Consumed);
    assert_eq!(
        st.buffer.contents(),
        "foo bar",
        "Ctrl-Backspace must not edit"
    );
    assert_eq!(st.cursor.offset, 3, "Ctrl-Backspace moves left");

    assert_eq!(feed(&mut vim, &mut st, ctrl_del), VimOutcome::Consumed);
    assert_eq!(st.buffer.contents(), "foo bar", "Ctrl-Delete must not edit");
    assert_eq!(st.cursor.offset, 4, "Ctrl-Delete moves right");
}

#[test]
fn ctrl_backspace_and_delete_do_not_edit_in_visual() {
    // Same guard in Visual: the chords extend the selection rather than
    // editing through it.
    let ctrl_bs = KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL);
    let ctrl_del = KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL);

    let mut st = state("foo bar");
    let mut vim = VimState::default();
    st.cursor.offset = 4;
    feed(&mut vim, &mut st, ch('v')); // enter Visual, anchor at 4

    assert_eq!(feed(&mut vim, &mut st, ctrl_bs), VimOutcome::Consumed);
    assert_eq!(
        st.buffer.contents(),
        "foo bar",
        "Ctrl-Backspace must not edit"
    );
    assert!(st.selection.is_some(), "selection is extended, not dropped");

    assert_eq!(feed(&mut vim, &mut st, ctrl_del), VimOutcome::Consumed);
    assert_eq!(st.buffer.contents(), "foo bar", "Ctrl-Delete must not edit");
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
fn esc_clears_a_lingering_selection_in_normal() {
    // A mouse drag can leave a selection active while in Normal; `Esc`
    // must drop both the buffer and its paint (mirroring `Esc` in Visual).
    let mut st = state("hello\nworld");
    let mut vim = VimState::default();
    st.selection = Some(Selection {
        anchor: 0,
        active: 3,
    });
    assert_eq!(feed(&mut vim, &mut st, esc()), VimOutcome::Consumed);
    assert!(st.selection.is_none(), "Esc must clear the selection");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
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
fn a_at_end_of_line_does_not_cross_the_newline() {
    let mut st = state("ab\ncd");
    let mut vim = VimState::default();
    st.cursor.offset = 2; // end of "ab", on the newline
    feed(&mut vim, &mut st, ch('a'));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(
        st.cursor.offset, 2,
        "stays before the newline, not on line 2"
    );
}

#[test]
fn a_on_the_last_char_appends_before_the_newline() {
    let mut st = state("ab\ncd");
    let mut vim = VimState::default();
    st.cursor.offset = 1; // on 'b', the last char of line 1
    feed(&mut vim, &mut st, ch('a'));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(
        st.cursor.offset, 2,
        "insertion point after 'b', before the newline"
    );
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

/// The four-line fixture the line-jump tests share.  Line 3 carries leading
/// blanks so the landing spot proves "first non-blank", not "line start".
const LINES: &str = "alpha\nbravo\n  charlie\ndelta";

#[test]
fn count_g_jumps_to_that_line() {
    let mut st = state(LINES);
    let mut vim = VimState::default();

    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('G'));
    assert_eq!(
        st.cursor.offset,
        LINES.find("charlie").unwrap(),
        "3G → first non-blank of line 3"
    );
    assert!(vim.count.is_none(), "the count is consumed");

    // `1G` is the first line — the case a repeat-count reading gets wrong.
    feed(&mut vim, &mut st, ch('1'));
    feed(&mut vim, &mut st, ch('G'));
    assert_eq!(st.cursor.offset, 0);
}

#[test]
fn bare_g_still_jumps_to_the_last_line() {
    // Guards the `count_of`-defaults-to-1 trap: an absent count must not
    // read as `1G`.
    let mut st = state(LINES);
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('G'));
    assert_eq!(st.cursor.offset, LINES.find("delta").unwrap());
}

#[test]
fn count_gg_jumps_to_that_line() {
    let mut st = state(LINES);
    let mut vim = VimState::default();
    st.cursor.offset = LINES.len();

    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('g'));
    feed(&mut vim, &mut st, ch('g'));
    assert_eq!(st.cursor.offset, LINES.find("charlie").unwrap());
}

#[test]
fn count_g_clamps_past_the_end_of_the_document() {
    let mut st = state(LINES);
    let mut vim = VimState::default();
    for c in "999".chars() {
        feed(&mut vim, &mut st, ch(c));
    }
    feed(&mut vim, &mut st, ch('G'));
    assert_eq!(st.cursor.offset, LINES.find("delta").unwrap());
}

#[test]
fn d_count_g_deletes_the_linewise_span() {
    // Downward: from line 1, `d2G` takes lines 1–2.
    let mut st = state(LINES);
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('2'));
    feed(&mut vim, &mut st, ch('G'));
    assert_eq!(st.buffer.contents(), "  charlie\ndelta");

    // Upward: from line 3, `d1G` takes lines 1–3.
    let mut st = state(LINES);
    let mut vim = VimState::default();
    st.cursor.offset = LINES.find("charlie").unwrap();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('1'));
    feed(&mut vim, &mut st, ch('G'));
    assert_eq!(st.buffer.contents(), "delta");
}

#[test]
fn count_g_extends_a_visual_selection() {
    let mut st = state(LINES);
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('G'));
    let sel = st.selection.as_ref().expect("visual selection live");
    assert_eq!(sel.anchor, 0);
    assert_eq!(sel.active, LINES.find("charlie").unwrap());
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
fn yy_arms_a_yank_flash_over_the_line() {
    let mut st = state("one\ntwo");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('y'));
    feed(&mut vim, &mut st, ch('y'));
    let flash = st.active_yank_flash().expect("yy arms a flash");
    // Covers the first line's bytes (including its trailing newline).
    assert_eq!((flash.start, flash.end), (0, 4));
}

#[test]
fn charwise_yank_arms_a_flash_over_the_span() {
    let mut st = state("abc");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('y'));
    feed(&mut vim, &mut st, ch('e')); // ye → "abc" inclusive
    let flash = st.active_yank_flash().expect("charwise yank arms a flash");
    assert_eq!((flash.start, flash.end), (0, 3));
}

#[test]
fn visual_yank_arms_a_flash() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('l')); // extend selection to cover "he"
    feed(&mut vim, &mut st, ch('y'));
    assert!(st.active_yank_flash().is_some(), "visual y arms a flash");
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

// ── CP6: Visual & VisualLine operators ─────────────────────────────────────────

#[test]
fn visual_charwise_delete_removes_the_highlighted_span() {
    // `v l l d` deletes the *inclusive* span — anchor through the char under
    // the cursor, as in stock vim — yanks it charwise, and returns to Normal.
    let mut st = state("hello world");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v')); // anchor 0
    feed(&mut vim, &mut st, ch('l')); // active 1
    feed(&mut vim, &mut st, ch('l')); // active 2
    feed(&mut vim, &mut st, ch('d'));
    assert_eq!(st.buffer.contents(), "lo world");
    assert_eq!(vim.register.text, "hel");
    assert!(!vim.register.linewise);
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert!(st.selection.is_none());
    assert_eq!(st.cursor.offset, 0);
}

#[test]
fn visual_x_is_an_alias_for_delete() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('x'));
    assert_eq!(
        st.buffer.contents(),
        "llo",
        "`v l` covers two chars, inclusively"
    );
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
}

#[test]
fn visual_charwise_yank_leaves_the_buffer_and_parks_at_start() {
    let mut st = state("hello world");
    let mut vim = VimState::default();
    st.cursor.offset = 6; // on "world"
    feed(&mut vim, &mut st, ch('v')); // anchor 6
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('y'));
    assert_eq!(st.buffer.contents(), "hello world", "yank never mutates");
    assert_eq!(vim.register.text, "wor");
    assert!(!vim.register.linewise);
    assert_eq!(st.cursor.offset, 6, "cursor parks at the span start");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
}

#[test]
fn v_alone_covers_the_char_under_the_cursor() {
    // The headline inclusivity case: `v y` in vim yanks one character, not
    // zero — the span is never empty, so `v d` / `v y` can't be no-ops.
    let mut st = state("hello");
    let mut vim = VimState::default();
    st.cursor.offset = 1;
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('y'));
    assert_eq!(vim.register.text, "e");
    assert!(!vim.register.linewise);
}

#[test]
fn visual_to_end_of_line_stops_before_the_newline() {
    // `$` parks edamame's cursor on the newline slot (vim's cursor can't go
    // there), so the inclusive extension is suppressed: `v $ y` covers the
    // line's content exactly, and matches walking `l` to the last char.
    let mut st = state("abc\ndef");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('$'));
    feed(&mut vim, &mut st, ch('y'));
    assert_eq!(vim.register.text, "abc", "the newline is never swallowed");

    let mut st = state("abc\ndef");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('y'));
    assert_eq!(vim.register.text, "abc", "`v l l` agrees with `v $`");
}

#[test]
fn a_backward_visual_selection_includes_the_anchor_char() {
    // With the cursor carried behind the anchor, the anchor's own character
    // is the high end — so it's the one the inclusive rule covers.
    let mut st = state("abcd");
    let mut vim = VimState::default();
    st.cursor.offset = 2; // on 'c'
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('h'));
    feed(&mut vim, &mut st, ch('h'));
    feed(&mut vim, &mut st, ch('d'));
    assert_eq!(st.buffer.contents(), "d");
    assert_eq!(vim.register.text, "abc");
}

#[test]
fn visual_change_deletes_and_enters_insert() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('c'));
    assert_eq!(st.buffer.contents(), "llo");
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert!(st.selection.is_none());
}

#[test]
fn visual_s_is_an_alias_for_change() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('s'));
    assert_eq!(st.buffer.contents(), "llo");
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
}

#[test]
fn visual_line_delete_removes_whole_lines_linewise() {
    // `V j d` deletes both touched lines, yanks them linewise.
    let mut st = state("alpha\nbeta\ngamma");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V')); // line 0
    feed(&mut vim, &mut st, ch('j')); // extend onto line 1
    feed(&mut vim, &mut st, ch('d'));
    assert_eq!(st.buffer.contents(), "gamma");
    assert_eq!(vim.register.text, "alpha\nbeta\n");
    assert!(vim.register.linewise);
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
}

#[test]
fn visual_line_yank_is_linewise() {
    let mut st = state("alpha\nbeta\ngamma");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('j'));
    feed(&mut vim, &mut st, ch('y'));
    assert_eq!(
        st.buffer.contents(),
        "alpha\nbeta\ngamma",
        "yank never mutates"
    );
    assert_eq!(vim.register.text, "alpha\nbeta\n");
    assert!(vim.register.linewise);
}

#[test]
fn visual_line_delete_then_paste_duplicates() {
    // A VisualLine yank fills the linewise register, so `p` opens a new line.
    let mut st = state("one\ntwo\nthree");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('y')); // yank line 0 linewise
    feed(&mut vim, &mut st, ch('p')); // paste below
    assert_eq!(st.buffer.contents(), "one\none\ntwo\nthree");
}

#[test]
fn visual_indent_right_and_left_round_trip() {
    let mut st = state("ab\ncd");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('j'));
    feed(&mut vim, &mut st, ch('>')); // indent both lines by tab_width (4)
    assert_eq!(st.buffer.contents(), "    ab\n    cd");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    // Re-select and outdent back.
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('j'));
    feed(&mut vim, &mut st, ch('<'));
    assert_eq!(st.buffer.contents(), "ab\ncd");
}

#[test]
fn visual_indent_does_not_touch_the_register() {
    let mut st = state("ab\ncd");
    let mut vim = VimState::default();
    vim.register.text = "kept".into();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('>'));
    assert_eq!(
        vim.register.text, "kept",
        "indent must not fill the register"
    );
}

#[test]
fn visual_charwise_indent_acts_on_whole_lines() {
    // `>` from charwise Visual still indents every line the span touches.
    let mut st = state("ab\ncd");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('j')); // span from line 0 into line 1
    feed(&mut vim, &mut st, ch('>'));
    assert_eq!(st.buffer.contents(), "    ab\n    cd");
}

#[test]
fn visual_tilde_toggles_case_of_the_span() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l')); // inclusive span [0,3) → "hel"
    feed(&mut vim, &mut st, ch('~'));
    assert_eq!(st.buffer.contents(), "HELlo");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
}

#[test]
fn visual_line_tilde_toggles_whole_lines() {
    let mut st = state("ab\ncd");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('j'));
    feed(&mut vim, &mut st, ch('~'));
    assert_eq!(st.buffer.contents(), "AB\nCD");
}

#[test]
fn visual_join_collapses_the_selected_lines() {
    let mut st = state("one\ntwo\nthree");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('j'));
    feed(&mut vim, &mut st, ch('j')); // span lines 0..=2
    feed(&mut vim, &mut st, ch('J'));
    assert_eq!(st.buffer.contents(), "one two three");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
}

#[test]
fn visual_single_line_join_pulls_up_the_line_below() {
    let mut st = state("one\ntwo");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v')); // single-line span on line 0
    feed(&mut vim, &mut st, ch('J'));
    assert_eq!(st.buffer.contents(), "one two");
}

#[test]
fn visual_o_swaps_the_selection_ends() {
    let mut st = state("hello world");
    let mut vim = VimState::default();
    st.cursor.offset = 2;
    feed(&mut vim, &mut st, ch('v')); // anchor 2
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l')); // active 4
    feed(&mut vim, &mut st, ch('o')); // swap: cursor jumps to the anchor end
    assert_eq!(vim.sub_mode, VimSubMode::Visual);
    assert_eq!(st.cursor.offset, 2);
    let sel = st.selection.expect("selection persists through swap");
    assert_eq!((sel.anchor, sel.active), (4, 2));
    assert_eq!(vim.visual_anchor, Some(4));
    // A motion now grows the (newly active) low end.
    feed(&mut vim, &mut st, ch('h')); // active 1
    let sel = st.selection.expect("selection persists");
    assert_eq!((sel.anchor, sel.active), (4, 1));
}

#[test]
fn capital_v_then_v_switches_to_charwise_keeping_anchor() {
    let mut st = state("alpha\nbeta");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    let anchor = vim.visual_anchor;
    feed(&mut vim, &mut st, ch('v')); // VisualLine → Visual
    assert_eq!(vim.sub_mode, VimSubMode::Visual);
    assert_eq!(vim.visual_anchor, anchor, "anchor survives the toggle");
    assert!(st.selection.is_some());
}

#[test]
fn v_then_v_exits_to_normal() {
    let mut st = state("alpha");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('v')); // same key → exit
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert!(st.selection.is_none());
}

#[test]
fn capital_v_then_capital_v_exits_to_normal() {
    let mut st = state("alpha");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('V'));
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert!(st.selection.is_none());
}

#[test]
fn visual_operator_is_a_single_undo_unit() {
    // A multi-line VisualLine delete collapses to one `u`.
    let mut st = state("a\nb\nc\nd");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('j'));
    feed(&mut vim, &mut st, ch('j')); // lines 0..=2
    feed(&mut vim, &mut st, ch('d'));
    assert_eq!(st.buffer.contents(), "d");
    feed(&mut vim, &mut st, ch('u'));
    assert_eq!(
        st.buffer.contents(),
        "a\nb\nc\nd",
        "one undo restores all three lines"
    );
}

#[test]
fn visual_line_change_clears_the_lines_and_enters_insert() {
    // `V j c` deletes both touched lines but (like `cc`) keeps one empty
    // line to type on, yanks them linewise, and enters Insert.
    let mut st = state("alpha\nbeta\ngamma");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V')); // line 0
    feed(&mut vim, &mut st, ch('j')); // extend onto line 1
    feed(&mut vim, &mut st, ch('c'));
    assert_eq!(st.buffer.contents(), "\ngamma");
    assert_eq!(vim.register.text, "alpha\nbeta\n");
    assert!(vim.register.linewise);
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert!(st.selection.is_none());
}

// ── CP6: Visual `p` / `r{c}` / `u` / `U` ────────────────────────────────────────

#[test]
fn visual_charwise_paste_replaces_the_span_with_the_register() {
    // Yank "world", select "hello", then `p` over it.
    let mut st = state("hello world");
    let mut vim = VimState::default();
    vim.register.text = "world".into();
    vim.register.linewise = false;
    feed(&mut vim, &mut st, ch('v')); // anchor 0
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l')); // inclusive span [0,5) → "hello"
    feed(&mut vim, &mut st, ch('p'));
    assert_eq!(st.buffer.contents(), "world world");
    // The register is left untouched so it can be pasted over again.
    assert_eq!(vim.register.text, "world");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert!(st.selection.is_none());
}

#[test]
fn visual_line_paste_replaces_whole_lines_with_a_linewise_register() {
    let mut st = state("one\ntwo\nthree");
    let mut vim = VimState::default();
    vim.register.text = "X\n".into();
    vim.register.linewise = true;
    feed(&mut vim, &mut st, ch('V')); // line 0
    feed(&mut vim, &mut st, ch('j')); // extend onto line 1
    feed(&mut vim, &mut st, ch('p'));
    assert_eq!(st.buffer.contents(), "X\nthree");
    assert_eq!(vim.register.text, "X\n", "register kept");
}

#[test]
fn visual_line_paste_of_a_charwise_register_keeps_its_own_line() {
    // A charwise register dropped over whole lines gets a trailing newline.
    let mut st = state("one\ntwo");
    let mut vim = VimState::default();
    vim.register.text = "X".into();
    vim.register.linewise = false;
    feed(&mut vim, &mut st, ch('V')); // line 0 only
    feed(&mut vim, &mut st, ch('p'));
    assert_eq!(st.buffer.contents(), "X\ntwo");
}

#[test]
fn visual_paste_with_empty_register_is_a_noop_that_leaves_visual() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('p'));
    assert_eq!(
        st.buffer.contents(),
        "hello",
        "empty register changes nothing"
    );
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert!(st.selection.is_none());
}

#[test]
fn visual_charwise_replace_fills_the_span_with_one_char() {
    // `v l l r x` replaces the inclusive [0,3) span ("hel") with "xxx".
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l')); // inclusive span [0,3)
    assert_eq!(feed(&mut vim, &mut st, ch('r')), VimOutcome::Pending);
    feed(&mut vim, &mut st, ch('x'));
    assert_eq!(st.buffer.contents(), "xxxlo");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert!(st.selection.is_none());
}

#[test]
fn visual_line_replace_preserves_newlines() {
    // `V j r *` replaces every char of both lines with '*', keeping the
    // line break between them.
    let mut st = state("ab\ncd");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('j'));
    feed(&mut vim, &mut st, ch('r'));
    feed(&mut vim, &mut st, ch('*'));
    assert_eq!(st.buffer.contents(), "**\n**");
}

#[test]
fn visual_replace_cancelled_by_esc_keeps_the_selection() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('r')); // arm pending replace
    feed(&mut vim, &mut st, esc()); // cancel
    assert_eq!(st.buffer.contents(), "hello", "no edit on cancel");
    assert_eq!(vim.sub_mode, VimSubMode::Visual, "still in Visual");
    assert!(!vim.pending_replace);
    assert!(st.selection.is_some());
}

#[test]
fn visual_u_forces_lowercase_and_capital_u_forces_uppercase() {
    let mut st = state("Hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l')); // inclusive span [0,5) → "Hello"
    feed(&mut vim, &mut st, ch('u'));
    assert_eq!(st.buffer.contents(), "hello");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);

    // Now uppercase the same span.
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('U'));
    assert_eq!(st.buffer.contents(), "HELLO");
}

#[test]
fn visual_line_set_case_covers_whole_lines() {
    let mut st = state("ab\ncd");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('j'));
    feed(&mut vim, &mut st, ch('U'));
    assert_eq!(st.buffer.contents(), "AB\nCD");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
}

#[test]
fn visual_paste_and_replace_are_single_undo_units() {
    // VisualLine replace over two lines collapses to one `u`.
    let mut st = state("ab\ncd\nef");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('j'));
    feed(&mut vim, &mut st, ch('r'));
    feed(&mut vim, &mut st, ch('z'));
    assert_eq!(st.buffer.contents(), "zz\nzz\nef");
    feed(&mut vim, &mut st, ch('u'));
    assert_eq!(
        st.buffer.contents(),
        "ab\ncd\nef",
        "one undo restores both lines"
    );
}

// ── CP7: text objects ──────────────────────────────────────────────────────────

#[test]
fn diw_deletes_the_inner_word() {
    // Cursor on "bar"; diw removes just the word, leaving the spaces.
    let mut st = state("foo bar baz");
    st.cursor.offset = 5;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('i'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.buffer.contents(), "foo  baz");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
}

#[test]
fn daw_includes_the_trailing_space() {
    let mut st = state("foo bar baz");
    st.cursor.offset = 4; // 'b' of bar
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('a'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.buffer.contents(), "foo baz");
}

#[test]
fn ciw_changes_word_and_enters_insert() {
    let mut st = state("foo bar");
    st.cursor.offset = 1; // inside "foo"
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('c'));
    feed(&mut vim, &mut st, ch('i'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.buffer.contents(), " bar");
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
}

#[test]
fn diw_is_a_single_undo_unit() {
    let mut st = state("foo bar");
    st.cursor.offset = 0;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('i'));
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(st.buffer.contents(), " bar");
    feed(&mut vim, &mut st, ch('u'));
    assert_eq!(
        st.buffer.contents(),
        "foo bar",
        "one undo restores the word"
    );
}

#[test]
fn di_quote_deletes_inside_quotes() {
    let mut st = state("say \"hi\" now");
    st.cursor.offset = 5; // inside "hi"
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('i'));
    feed(&mut vim, &mut st, ch('"'));
    assert_eq!(st.buffer.contents(), "say \"\" now");
}

#[test]
fn da_quote_deletes_quotes_and_trailing_space() {
    let mut st = state("say \"hi\" now");
    st.cursor.offset = 5;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('a'));
    feed(&mut vim, &mut st, ch('"'));
    assert_eq!(st.buffer.contents(), "say now");
}

#[test]
fn ci_paren_changes_inside_parens() {
    let mut st = state("f(arg)");
    st.cursor.offset = 3; // inside "arg"
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('c'));
    feed(&mut vim, &mut st, ch('i'));
    feed(&mut vim, &mut st, ch('('));
    assert_eq!(st.buffer.contents(), "f()");
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(st.cursor.offset, 2, "Insert begins between the parens");
}

#[test]
fn da_bracket_deletes_brackets_too() {
    let mut st = state("x[ab]y");
    st.cursor.offset = 2;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('a'));
    feed(&mut vim, &mut st, ch('['));
    assert_eq!(st.buffer.contents(), "xy");
}

#[test]
fn di_brace_works_from_a_closing_bracket() {
    // Cursor on the closing brace resolves to its own pair.
    let mut st = state("{ab}");
    st.cursor.offset = 3; // on '}'
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('i'));
    feed(&mut vim, &mut st, ch('}'));
    assert_eq!(st.buffer.contents(), "{}");
}

#[test]
fn ci_paren_picks_the_innermost_nested_pair() {
    let mut st = state("(a(b)c)");
    st.cursor.offset = 3; // 'b'
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('i'));
    feed(&mut vim, &mut st, ch('('));
    assert_eq!(st.buffer.contents(), "(a()c)");
}

#[test]
fn text_object_on_a_missing_pair_is_a_noop() {
    let mut st = state("abc");
    st.cursor.offset = 1;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('i'));
    feed(&mut vim, &mut st, ch('('));
    assert_eq!(
        st.buffer.contents(),
        "abc",
        "no enclosing pair → nothing deleted"
    );
    assert_eq!(vim.sub_mode, VimSubMode::Normal, "operator is cancelled");
}

#[test]
fn visual_inner_word_selects_the_word() {
    let mut st = state("foo bar");
    st.cursor.offset = 5; // inside "bar"
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('i'));
    feed(&mut vim, &mut st, ch('w'));
    let sel = st.selection.expect("selection set");
    assert_eq!(
        visual_charwise_range(&sel, &st.buffer),
        4..7,
        "the whole word is selected"
    );
    assert_eq!(
        st.cursor.offset, 6,
        "the cursor parks on the word's last char, as in vim"
    );
    // A following `d` deletes exactly the selection.
    feed(&mut vim, &mut st, ch('d'));
    assert_eq!(st.buffer.contents(), "foo ");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
}

#[test]
fn visual_inner_quote_selects_inside() {
    let mut st = state("a \"hi\" b");
    st.cursor.offset = 3; // inside "hi"
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('i'));
    feed(&mut vim, &mut st, ch('"'));
    let sel = st.selection.expect("selection set");
    assert_eq!(visual_charwise_range(&sel, &st.buffer), 3..5);
}

#[test]
fn cancelling_a_text_object_drops_the_operator() {
    let mut st = state("foo bar");
    st.cursor.offset = 0;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('i'));
    // Esc after the `i` prefix cancels with no edit.
    feed(&mut vim, &mut st, esc());
    assert_eq!(st.buffer.contents(), "foo bar");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert_eq!(vim.pending_op, None);
    assert_eq!(vim.pending_text_object, None);
}

// ── Ctrl-* chord passthrough while a sub-state is pending ───────────────────────

#[test]
fn ctrl_chord_during_a_pending_text_object_passes_through() {
    // `di` then Ctrl-S: the operator is abandoned with no edit, and the
    // chord passes through so its app action (Save) still fires — matching
    // the bare-operator path where `d` then Ctrl-S also passes through.
    let mut st = state("foo bar");
    st.cursor.offset = 0;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('i'));
    assert_eq!(feed(&mut vim, &mut st, ctrl('s')), VimOutcome::Passthrough);
    assert_eq!(st.buffer.contents(), "foo bar", "no edit");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert_eq!(vim.pending_op, None);
    assert_eq!(vim.pending_text_object, None);
}

#[test]
fn ctrl_chord_during_a_pending_find_passes_through() {
    // `df` then Ctrl-S: same contract for the find pending state.
    let mut st = state("foo bar");
    st.cursor.offset = 0;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('f'));
    assert_eq!(feed(&mut vim, &mut st, ctrl('s')), VimOutcome::Passthrough);
    assert_eq!(st.buffer.contents(), "foo bar", "no edit");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert_eq!(vim.pending_op, None);
    assert_eq!(vim.pending_find, None);
}

#[test]
fn ctrl_chord_during_a_pending_replace_passes_through() {
    // `r` then Ctrl-S: the replace is abandoned and the chord passes through.
    let mut st = state("foo bar");
    st.cursor.offset = 0;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('r'));
    assert_eq!(feed(&mut vim, &mut st, ctrl('s')), VimOutcome::Passthrough);
    assert_eq!(st.buffer.contents(), "foo bar", "no replacement");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert!(!vim.pending_replace);
}

#[test]
fn non_chord_cancel_of_a_pending_text_object_is_still_consumed() {
    // `di` then `j`: `j` names no object, so it cancels — but a plain key is
    // swallowed (Consumed), not passed through, and applies no motion.
    let mut st = state("foo bar");
    st.cursor.offset = 0;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('i'));
    assert_eq!(feed(&mut vim, &mut st, ch('j')), VimOutcome::Consumed);
    assert_eq!(st.buffer.contents(), "foo bar");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
    assert_eq!(vim.pending_op, None);
    assert_eq!(vim.pending_text_object, None);
}

#[test]
fn ctrl_chord_during_a_visual_text_object_passes_through_keeping_selection() {
    // In Visual, `vi` then Ctrl-C cancels the pending object but keeps the
    // selection and passes the chord through (so Ctrl-C copies the span).
    let mut st = state("foo bar");
    st.cursor.offset = 5;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    let before = st.selection.expect("visual selection set on entry");
    feed(&mut vim, &mut st, ch('i'));
    assert_eq!(feed(&mut vim, &mut st, ctrl('c')), VimOutcome::Passthrough);
    assert_eq!(vim.sub_mode, VimSubMode::Visual, "still in Visual");
    assert_eq!(
        st.selection.expect("selection retained"),
        before,
        "the selection is untouched by the cancel"
    );
    assert_eq!(vim.pending_text_object, None);
}

// ── CP8: search (`/ ? n N * #`) ─────────────────────────────────────────────────

/// Install an active navigate-only search for `query` on `st`.
fn with_search(st: &mut EditorState, query: &str) {
    let s = SearchState::new(query.to_owned(), None).expect("valid query");
    st.enter_search(s);
}

#[test]
fn slash_opens_a_command_line_and_enter_submits_a_forward_search() {
    let mut st = state("foo bar foo");
    let mut vim = VimState::default();

    assert_eq!(feed(&mut vim, &mut st, ch('/')), VimOutcome::Pending);
    assert!(vim.cmdline.is_some(), "`/` arms the command line");

    feed(&mut vim, &mut st, ch('f'));
    feed(&mut vim, &mut st, ch('o'));
    feed(&mut vim, &mut st, ch('o'));
    let out = feed(&mut vim, &mut st, key(KeyCode::Enter));
    assert_eq!(
        out,
        VimOutcome::EnterSearch {
            forward: true,
            query: "foo".to_string()
        }
    );
    assert!(vim.cmdline.is_none(), "submit closes the command line");
}

#[test]
fn question_mark_submits_a_backward_search() {
    let mut st = state("foo bar foo");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('?'));
    feed(&mut vim, &mut st, ch('b'));
    feed(&mut vim, &mut st, ch('a'));
    feed(&mut vim, &mut st, ch('r'));
    assert_eq!(
        feed(&mut vim, &mut st, key(KeyCode::Enter)),
        VimOutcome::EnterSearch {
            forward: false,
            query: "bar".to_string()
        }
    );
}

#[test]
fn esc_in_the_command_line_cancels_without_searching() {
    let mut st = state("foo");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('/'));
    feed(&mut vim, &mut st, ch('x'));
    assert_eq!(feed(&mut vim, &mut st, esc()), VimOutcome::Consumed);
    assert!(vim.cmdline.is_none(), "Esc closes the command line");
}

#[test]
fn empty_search_submit_is_a_noop() {
    let mut st = state("foo");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('/'));
    assert_eq!(
        feed(&mut vim, &mut st, key(KeyCode::Enter)),
        VimOutcome::Consumed,
        "an empty query closes the prompt with no search"
    );
    assert!(vim.cmdline.is_none());
}

#[test]
fn command_line_typing_never_edits_the_buffer() {
    // Incsearch parks the cursor on the live focus while typing, but the
    // buffer itself must stay untouched and Esc must return the cursor
    // to the origin.
    let mut st = state("hello ell");
    let before = st.buffer.contents();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('/'));
    for c in "ell".chars() {
        feed(&mut vim, &mut st, ch(c));
    }
    assert_eq!(st.buffer.contents(), before, "no buffer edit");
    feed(&mut vim, &mut st, esc());
    assert_eq!(st.cursor.offset, 0, "Esc restores the origin");
    assert_eq!(st.buffer.contents(), before);
}

#[test]
fn star_searches_the_word_under_the_cursor_forward() {
    let mut st = state("foo bar foo");
    st.cursor.offset = 4; // on "bar"
    st.update_cursor_block();
    let mut vim = VimState::default();
    assert_eq!(
        feed(&mut vim, &mut st, ch('*')),
        VimOutcome::EnterSearch {
            forward: true,
            query: "bar".to_string()
        }
    );
}

#[test]
fn hash_searches_the_word_under_the_cursor_backward() {
    let mut st = state("alpha beta");
    st.cursor.offset = 7; // inside "beta"
    st.update_cursor_block();
    let mut vim = VimState::default();
    assert_eq!(
        feed(&mut vim, &mut st, ch('#')),
        VimOutcome::EnterSearch {
            forward: false,
            query: "beta".to_string()
        }
    );
}

#[test]
fn star_on_whitespace_with_no_following_word_is_a_noop() {
    let mut st = state("hi    ");
    st.cursor.offset = 3; // trailing spaces, no word after
    st.update_cursor_block();
    let mut vim = VimState::default();
    assert_eq!(feed(&mut vim, &mut st, ch('*')), VimOutcome::Consumed);
}

#[test]
fn n_and_capital_n_advance_and_retreat_the_focused_match() {
    let mut st = state("foo bar foo baz foo");
    with_search(&mut st, "foo"); // matches at 0, 8, 16
    let mut vim = VimState::default();
    assert_eq!(st.search.as_ref().unwrap().focused_idx, 0);

    feed(&mut vim, &mut st, ch('n'));
    assert_eq!(st.search.as_ref().unwrap().focused_idx, 1);
    assert_eq!(st.cursor.offset, 8, "cursor follows the match");

    feed(&mut vim, &mut st, ch('N'));
    assert_eq!(st.search.as_ref().unwrap().focused_idx, 0);
    assert_eq!(st.cursor.offset, 0);

    // Retreat past the first wraps to the last.
    feed(&mut vim, &mut st, ch('N'));
    assert_eq!(st.search.as_ref().unwrap().focused_idx, 2);
}

#[test]
fn count_drives_n() {
    let mut st = state("x y x y x y x"); // "x" at 0,4,8,12
    with_search(&mut st, "x");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('2'));
    feed(&mut vim, &mut st, ch('n'));
    assert_eq!(
        st.search.as_ref().unwrap().focused_idx,
        2,
        "2n advances twice"
    );
}

#[test]
fn n_with_no_active_search_is_an_inert_noop() {
    let mut st = state("foo bar");
    let mut vim = VimState::default();
    assert_eq!(feed(&mut vim, &mut st, ch('n')), VimOutcome::Consumed);
    assert_eq!(st.cursor.offset, 0);
    assert!(st.search.is_none());
}

#[test]
fn esc_in_normal_dismisses_an_active_search() {
    let mut st = state("foo bar foo");
    with_search(&mut st, "foo");
    let mut vim = VimState::default();
    assert!(st.search.is_some());
    feed(&mut vim, &mut st, esc());
    assert!(
        st.search.is_none(),
        "Esc clears the search highlight (`:noh`)"
    );
}

// ── Normal mode never edits (Backspace / Delete / Enter / Tab) ───────────────────

#[test]
fn backspace_in_normal_moves_left_without_editing() {
    let mut st = state("hello");
    st.cursor.offset = 3;
    st.update_cursor_block();
    let mut vim = VimState::default();
    let out = feed(&mut vim, &mut st, key(KeyCode::Backspace));
    assert_eq!(out, VimOutcome::Consumed, "Backspace is swallowed");
    assert_eq!(st.buffer.contents(), "hello", "no buffer edit");
    assert_eq!(st.cursor.offset, 2, "cursor moved left");
}

#[test]
fn delete_in_normal_moves_right_without_editing() {
    let mut st = state("hello");
    st.cursor.offset = 1;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, key(KeyCode::Delete));
    assert_eq!(st.buffer.contents(), "hello", "no buffer edit");
    assert_eq!(st.cursor.offset, 2, "cursor moved right");
}

#[test]
fn count_drives_backspace_in_normal() {
    let mut st = state("hello");
    st.cursor.offset = 4;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, key(KeyCode::Backspace));
    assert_eq!(st.buffer.contents(), "hello");
    assert_eq!(st.cursor.offset, 1, "3<BS> moves left three times");
}

#[test]
fn enter_in_normal_moves_to_next_line_first_non_blank_without_editing() {
    let mut st = state("abc\n   xyz");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, key(KeyCode::Enter));
    assert_eq!(st.buffer.contents(), "abc\n   xyz", "no newline inserted");
    assert_eq!(
        st.cursor.offset, 7,
        "cursor on the first non-blank of line 2"
    );
}

#[test]
fn tab_in_normal_without_a_search_is_inert() {
    let mut st = state("hi");
    let mut vim = VimState::default();
    let out = feed(&mut vim, &mut st, key(KeyCode::Tab));
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(st.buffer.contents(), "hi", "Tab must not insert a tab");
    assert_eq!(st.cursor.offset, 0);
}

#[test]
fn tab_and_backtab_walk_search_matches_like_n_and_capital_n() {
    let mut st = state("foo bar foo baz foo");
    with_search(&mut st, "foo"); // matches at 0, 8, 16
    let mut vim = VimState::default();
    assert_eq!(st.search.as_ref().unwrap().focused_idx, 0);

    feed(&mut vim, &mut st, key(KeyCode::Tab));
    assert_eq!(st.search.as_ref().unwrap().focused_idx, 1, "Tab advances");
    assert_eq!(st.cursor.offset, 8, "cursor follows the match");

    feed(&mut vim, &mut st, key(KeyCode::BackTab));
    assert_eq!(
        st.search.as_ref().unwrap().focused_idx,
        0,
        "Shift-Tab retreats"
    );
}

#[test]
fn backspace_and_delete_extend_a_visual_selection_without_editing() {
    let mut st = state("hello world");
    st.cursor.offset = 4;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v')); // enter Visual, anchor at 4
    feed(&mut vim, &mut st, key(KeyCode::Delete)); // extend right
    feed(&mut vim, &mut st, key(KeyCode::Delete));
    assert_eq!(
        st.buffer.contents(),
        "hello world",
        "Visual edit-keys don't edit"
    );
    let sel = st.selection.as_ref().expect("selection still set");
    assert_eq!((sel.anchor, sel.active), (4, 6), "selection extended right");
    feed(&mut vim, &mut st, key(KeyCode::Backspace)); // extend back left
    let sel = st.selection.as_ref().unwrap();
    assert_eq!(sel.active, 5, "Backspace pulls the active end left");
}

// ── CP9: Ex commands (`:w :q :wq :s :%s`) ──────────────────────────────────────

/// Type a full `:`-command (the leading `:`, the body, then Enter) and
/// return the outcome of the submitting Enter press.
fn ex_cmd(vim: &mut VimState, st: &mut EditorState, body: &str) -> VimOutcome {
    feed(vim, st, ch(':'));
    for c in body.chars() {
        feed(vim, st, ch(c));
    }
    feed(vim, st, key(KeyCode::Enter))
}

#[test]
fn colon_opens_the_ex_command_line() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    assert_eq!(feed(&mut vim, &mut st, ch(':')), VimOutcome::Pending);
    let cl = vim.cmdline.as_ref().expect("ex command line armed");
    assert_eq!(cl.kind.prefix(), ':');
    // Bare letters now edit the command line, not the buffer.
    assert_eq!(feed(&mut vim, &mut st, ch('w')), VimOutcome::Consumed);
    assert_eq!(vim.cmdline.as_ref().unwrap().input, "w");
    // Esc cancels the prompt without acting.
    assert_eq!(feed(&mut vim, &mut st, esc()), VimOutcome::Consumed);
    assert!(vim.cmdline.is_none());
    assert_eq!(st.buffer.contents(), "hello", "cancelled `:` never edits");
}

#[test]
fn ex_line_address_jumps_to_that_line() {
    let mut st = state(LINES);
    let mut vim = VimState::default();

    assert_eq!(ex_cmd(&mut vim, &mut st, "3"), VimOutcome::Consumed);
    assert_eq!(st.cursor.offset, LINES.find("charlie").unwrap());

    // `:$` is the last line, clamped like `G`.
    ex_cmd(&mut vim, &mut st, "$");
    assert_eq!(st.cursor.offset, LINES.find("delta").unwrap());

    // Still an unknown command when it isn't a line address.
    assert!(matches!(
        ex_cmd(&mut vim, &mut st, "3x"),
        VimOutcome::Flash(_)
    ));
}

#[test]
fn ex_write_returns_save_outcome() {
    let mut st = state_with_path("hello");
    let mut vim = VimState::default();
    assert_eq!(ex_cmd(&mut vim, &mut st, "w"), VimOutcome::Save);
    assert!(vim.cmdline.is_none(), "prompt closes on submit");
}

#[test]
fn ex_quit_and_writequit_outcomes() {
    let mut st = state_with_path("hello");
    let mut vim = VimState::default();
    assert_eq!(
        ex_cmd(&mut vim, &mut st, "q"),
        VimOutcome::Quit { save_first: false }
    );
    assert_eq!(
        ex_cmd(&mut vim, &mut st, "wq"),
        VimOutcome::Quit { save_first: true }
    );
}

#[test]
fn ex_write_path_copies_saveas_repoints() {
    let mut st = state_with_path("hello");
    let mut vim = VimState::default();
    // `:w <path>` writes a copy (keeps the current file) — real vim.
    assert_eq!(
        ex_cmd(&mut vim, &mut st, "w other.md"),
        VimOutcome::SaveCopy {
            path: std::path::PathBuf::from("other.md"),
            then_quit: false,
            force: false,
        }
    );
    // `:w! <path>` forces past the overwrite-confirmation prompt.
    assert_eq!(
        ex_cmd(&mut vim, &mut st, "w! other.md"),
        VimOutcome::SaveCopy {
            path: std::path::PathBuf::from("other.md"),
            then_quit: false,
            force: true,
        }
    );
    // `:saveas <path>` re-points the buffer at the new path.
    assert_eq!(
        ex_cmd(&mut vim, &mut st, "saveas other.md"),
        VimOutcome::SaveAs {
            path: Some(std::path::PathBuf::from("other.md")),
            then_quit: false,
            force: false,
        }
    );
    // `:saveas` with no argument prompts (path None).
    assert_eq!(
        ex_cmd(&mut vim, &mut st, "saveas"),
        VimOutcome::SaveAs {
            path: None,
            then_quit: false,
            force: false,
        }
    );
    // `:wq <path>` writes a copy, then quits.
    assert_eq!(
        ex_cmd(&mut vim, &mut st, "wq out.md"),
        VimOutcome::SaveCopy {
            path: std::path::PathBuf::from("out.md"),
            then_quit: true,
            force: false,
        }
    );
}

#[test]
fn ex_write_on_pathless_buffer_prompts() {
    // A never-saved buffer has no destination, so a bare `:w` opens the
    // Save As prompt; `:wq` does the same but also carries the quit intent.
    let mut st = state("hello");
    let mut vim = VimState::default();
    assert_eq!(
        ex_cmd(&mut vim, &mut st, "w"),
        VimOutcome::SaveAs {
            path: None,
            then_quit: false,
            force: false,
        }
    );
    assert_eq!(
        ex_cmd(&mut vim, &mut st, "wq"),
        VimOutcome::SaveAs {
            path: None,
            then_quit: true,
            force: false,
        }
    );
}

#[test]
fn ex_empty_command_is_a_silent_noop() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch(':'));
    assert_eq!(
        feed(&mut vim, &mut st, key(KeyCode::Enter)),
        VimOutcome::Consumed
    );
    assert!(vim.cmdline.is_none());
    assert_eq!(st.buffer.contents(), "hello");
}

#[test]
fn ex_substitute_current_line_first_match_only() {
    let mut st = state("foo foo\nfoo foo");
    let mut vim = VimState::default();
    let out = ex_cmd(&mut vim, &mut st, "s/foo/bar/");
    assert_eq!(out, VimOutcome::Flash("1 substitution".to_owned()));
    assert_eq!(
        st.buffer.contents(),
        "bar foo\nfoo foo",
        ":s replaces only the first match on the current line"
    );
}

#[test]
fn ex_substitute_current_line_global_flag() {
    let mut st = state("foo foo\nfoo foo");
    let mut vim = VimState::default();
    let out = ex_cmd(&mut vim, &mut st, "s/foo/bar/g");
    assert_eq!(out, VimOutcome::Flash("2 substitutions".to_owned()));
    assert_eq!(
        st.buffer.contents(),
        "bar bar\nfoo foo",
        "g replaces every match on the current line only"
    );
}

#[test]
fn ex_substitute_whole_file_global() {
    let mut st = state("foo foo\nfoo foo");
    let mut vim = VimState::default();
    let out = ex_cmd(&mut vim, &mut st, "%s/foo/bar/g");
    assert_eq!(out, VimOutcome::Flash("4 substitutions".to_owned()));
    assert_eq!(st.buffer.contents(), "bar bar\nbar bar");
}

#[test]
fn ex_substitute_ignore_case_flag() {
    let mut st = state("Foo foo FOO");
    let mut vim = VimState::default();
    let out = ex_cmd(&mut vim, &mut st, "%s/foo/x/gi");
    assert_eq!(out, VimOutcome::Flash("3 substitutions".to_owned()));
    assert_eq!(st.buffer.contents(), "x x x");
}

#[test]
fn ex_substitute_is_a_single_undo_unit() {
    let mut st = state("foo\nfoo\nfoo");
    let mut vim = VimState::default();
    ex_cmd(&mut vim, &mut st, "%s/foo/bar/g");
    assert_eq!(st.buffer.contents(), "bar\nbar\nbar");
    // One `u` reverts the whole substitution.
    feed(&mut vim, &mut st, ch('u'));
    assert_eq!(
        st.buffer.contents(),
        "foo\nfoo\nfoo",
        ":%s undoes in a single step"
    );
}

#[test]
fn ex_substitute_no_match_flashes_and_leaves_buffer() {
    let mut st = state("abc");
    let mut vim = VimState::default();
    let out = ex_cmd(&mut vim, &mut st, "s/zzz/x/");
    assert_eq!(out, VimOutcome::Flash("Pattern not found: zzz".to_owned()));
    assert_eq!(st.buffer.contents(), "abc", "a no-match records no edit");
    assert!(
        !st.dirty,
        "a no-match substitution does not dirty the buffer"
    );
}

#[test]
fn ex_substitute_supports_regex() {
    let mut st = state("a1b2c3");
    let mut vim = VimState::default();
    let out = ex_cmd(&mut vim, &mut st, r"%s/[0-9]/-/g");
    assert_eq!(out, VimOutcome::Flash("3 substitutions".to_owned()));
    assert_eq!(st.buffer.contents(), "a-b-c-");
}

/// Type the body of a `:`-command that is already open (its `'<,'>` prefix
/// pre-filled by a Visual-mode `:`), then submit with Enter.
fn submit_open_ex(vim: &mut VimState, st: &mut EditorState, body: &str) -> VimOutcome {
    for c in body.chars() {
        feed(vim, st, ch(c));
    }
    feed(vim, st, key(KeyCode::Enter))
}

#[test]
fn colon_in_visual_prefills_the_visual_range() {
    let mut st = state("foo\nfoo\nfoo");
    let mut vim = VimState::default();
    // V-LINE over lines 0..=1, then `:` opens the ex prompt pre-filled with
    // the `'<,'>` range and drops the selection (as vim does).
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('j'));
    assert_eq!(feed(&mut vim, &mut st, ch(':')), VimOutcome::Pending);
    let cl = vim.cmdline.as_ref().expect("ex prompt armed from Visual");
    assert_eq!(cl.input, "'<,'>", "the `'<,'>` range is pre-filled");
    assert_eq!(cl.cursor, 5, "cursor parks at the end so typing appends");
    assert_eq!(vim.sub_mode, VimSubMode::Normal, "Visual exits on `:`");
    assert!(st.selection.is_none(), "the highlight is dropped");
    assert_eq!(
        vim.last_visual_range,
        Some((0, 1)),
        "marks span the selection"
    );
}

#[test]
fn ex_substitute_over_visual_line_range() {
    let mut st = state("foo\nfoo\nfoo\nfoo");
    let mut vim = VimState::default();
    // Select lines 0..=1 linewise, then `:'<,'>s/foo/bar/g`.
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('j'));
    feed(&mut vim, &mut st, ch(':'));
    let out = submit_open_ex(&mut vim, &mut st, "s/foo/bar/g");
    assert_eq!(out, VimOutcome::Flash("2 substitutions".to_owned()));
    assert_eq!(
        st.buffer.contents(),
        "bar\nbar\nfoo\nfoo",
        ":'<,'>s only touches the selected lines"
    );
}

#[test]
fn ex_substitute_over_charwise_visual_uses_whole_lines() {
    let mut st = state("foo foo\nfoo foo\nfoo foo");
    let mut vim = VimState::default();
    // Charwise Visual from mid-line 0 into line 1; the ex range is still
    // line-oriented (whole lines 0..=1), matching vim.
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('l'));
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('j'));
    feed(&mut vim, &mut st, ch(':'));
    let out = submit_open_ex(&mut vim, &mut st, "s/foo/bar/g");
    assert_eq!(out, VimOutcome::Flash("4 substitutions".to_owned()));
    assert_eq!(
        st.buffer.contents(),
        "bar bar\nbar bar\nfoo foo",
        "charwise selection substitutes over the whole touched lines"
    );
}

#[test]
fn ex_write_from_visual_ignores_the_range_prefix() {
    let mut st = state_with_path("hello");
    let mut vim = VimState::default();
    // `:` in Visual auto-inserts `'<,'>`; the write/quit family ignores it and
    // acts on the whole buffer, so a Visual `:w` still just saves.
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch(':'));
    assert_eq!(submit_open_ex(&mut vim, &mut st, "w"), VimOutcome::Save);
    assert_eq!(st.buffer.contents(), "hello");
}

#[test]
fn ex_unknown_command_from_visual_keeps_the_range_in_the_error() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch(':'));
    let out = submit_open_ex(&mut vim, &mut st, "nope");
    assert_eq!(
        out,
        VimOutcome::Flash("Not an editor command: '<,'>nope".to_owned())
    );
    assert_eq!(st.buffer.contents(), "hello");
}

#[test]
fn ex_parse_error_flashes_without_editing() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    let out = ex_cmd(&mut vim, &mut st, "nope");
    assert_eq!(
        out,
        VimOutcome::Flash("Not an editor command: nope".to_owned())
    );
    assert_eq!(st.buffer.contents(), "hello");
    assert_eq!(vim.sub_mode, VimSubMode::Normal);
}

#[test]
fn ex_invalid_regex_flashes() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    // `\(` translates to an unbalanced `(` — it passes the vim→regex
    // translator but the engine rejects it at compile time.
    let out = ex_cmd(&mut vim, &mut st, r"s/\(/x/");
    match out {
        VimOutcome::Flash(text) => assert!(text.starts_with("Invalid pattern:"), "got: {text}"),
        other => panic!("expected an invalid-pattern flash, got {other:?}"),
    }
    assert_eq!(st.buffer.contents(), "hello");
}

// ── CP9 follow-up: vim-syntax substitution (translator + fancy-regex) ──────────

#[test]
fn ex_substitute_uses_vim_pattern_syntax() {
    // Vim grouping/quantifiers (`\(…\)`, `\+`) and a backreference in the
    // replacement (`\1`), not the regex-crate `$1` form.
    let mut st = state("hello world");
    let mut vim = VimState::default();
    let out = ex_cmd(&mut vim, &mut st, r"s/\(\w\+\) \(\w\+\)/\2 \1/");
    assert_eq!(out, VimOutcome::Flash("1 substitution".to_owned()));
    assert_eq!(st.buffer.contents(), "world hello");
}

#[test]
fn ex_substitute_replacement_case_modifier() {
    // `\U\1` upcases the captured group — a vim replacement feature the plain
    // regex crate cannot express.
    let mut st = state("shout please");
    let mut vim = VimState::default();
    let out = ex_cmd(&mut vim, &mut st, r"s/\(\w\+\)/\U\1/");
    assert_eq!(out, VimOutcome::Flash("1 substitution".to_owned()));
    assert_eq!(st.buffer.contents(), "SHOUT please");
}

#[test]
fn ex_substitute_pattern_backreference() {
    // `\(.\)\1` (a doubled character) is impossible with the linear-time
    // regex crate; fancy-regex makes it work.
    let mut st = state("aa bb cd");
    let mut vim = VimState::default();
    let out = ex_cmd(&mut vim, &mut st, r"%s/\(.\)\1/X/g");
    assert_eq!(out, VimOutcome::Flash("2 substitutions".to_owned()));
    assert_eq!(st.buffer.contents(), "X X cd");
}

#[test]
fn ex_substitute_very_magic_mode() {
    let mut st = state("foofoo bar");
    let mut vim = VimState::default();
    let out = ex_cmd(&mut vim, &mut st, r"s/\v(foo)+/baz/");
    assert_eq!(out, VimOutcome::Flash("1 substitution".to_owned()));
    assert_eq!(st.buffer.contents(), "baz bar");
}

#[test]
fn ex_substitute_word_boundary() {
    // `\<cat\>` matches the whole word only, not the `cat` in `category`.
    let mut st = state("cat category cat");
    let mut vim = VimState::default();
    let out = ex_cmd(&mut vim, &mut st, r"%s/\<cat\>/dog/g");
    assert_eq!(out, VimOutcome::Flash("2 substitutions".to_owned()));
    assert_eq!(st.buffer.contents(), "dog category dog");
}

#[test]
fn ex_substitute_unsupported_atom_flashes() {
    let mut st = state("foobar");
    let mut vim = VimState::default();
    let out = ex_cmd(&mut vim, &mut st, r"s/foo\zsbar/X/");
    match out {
        VimOutcome::Flash(text) => {
            assert!(text.starts_with("Unsupported vim pattern:"), "got: {text}")
        }
        other => panic!("expected unsupported-pattern flash, got {other:?}"),
    }
    assert_eq!(
        st.buffer.contents(),
        "foobar",
        "rejected pattern records no edit"
    );
}

// ── Command-line history (Up/Down recall) ───────────────────────────────────────

#[test]
fn ex_history_recalls_previous_commands_with_arrows() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    ex_cmd(&mut vim, &mut st, "w");
    ex_cmd(&mut vim, &mut st, "q");
    // Reopen the prompt and type a partial draft.
    feed(&mut vim, &mut st, ch(':'));
    feed(&mut vim, &mut st, ch('x'));
    // Up walks newest → oldest.
    feed(&mut vim, &mut st, key(KeyCode::Up));
    assert_eq!(vim.cmdline.as_ref().unwrap().input, "q");
    feed(&mut vim, &mut st, key(KeyCode::Up));
    assert_eq!(vim.cmdline.as_ref().unwrap().input, "w");
    feed(&mut vim, &mut st, key(KeyCode::Up)); // already oldest — no further change
    assert_eq!(vim.cmdline.as_ref().unwrap().input, "w");
    // Down walks back, and past the newest restores the typed draft.
    feed(&mut vim, &mut st, key(KeyCode::Down));
    assert_eq!(vim.cmdline.as_ref().unwrap().input, "q");
    feed(&mut vim, &mut st, key(KeyCode::Down));
    assert_eq!(vim.cmdline.as_ref().unwrap().input, "x");
    assert_eq!(vim.cmdline.as_ref().unwrap().cursor, 1);
}

#[test]
fn recalled_ex_command_runs_when_submitted() {
    let mut st = state_with_path("hello");
    let mut vim = VimState::default();
    ex_cmd(&mut vim, &mut st, "w");
    feed(&mut vim, &mut st, ch(':'));
    feed(&mut vim, &mut st, key(KeyCode::Up));
    assert_eq!(
        feed(&mut vim, &mut st, key(KeyCode::Enter)),
        VimOutcome::Save
    );
}

#[test]
fn ex_and_search_keep_independent_histories() {
    let mut st = state("foo bar");
    let mut vim = VimState::default();
    ex_cmd(&mut vim, &mut st, "w");
    // A search submission records into the (separate) search history.
    feed(&mut vim, &mut st, ch('/'));
    feed(&mut vim, &mut st, ch('f'));
    feed(&mut vim, &mut st, ch('o'));
    feed(&mut vim, &mut st, ch('o'));
    feed(&mut vim, &mut st, key(KeyCode::Enter));
    assert_eq!(vim.ex_history, vec!["w".to_owned()]);
    assert_eq!(vim.search_history, vec!["foo".to_owned()]);
    // The `:` prompt only recalls ex history, never the search query.
    feed(&mut vim, &mut st, ch(':'));
    feed(&mut vim, &mut st, key(KeyCode::Up));
    assert_eq!(vim.cmdline.as_ref().unwrap().input, "w");
}

#[test]
fn empty_ex_submit_is_not_recorded() {
    let mut st = state("hello");
    let mut vim = VimState::default();
    ex_cmd(&mut vim, &mut st, "");
    assert!(
        vim.ex_history.is_empty(),
        "an empty `:` line is not history"
    );
}

// ── CP10: markdown-aware list wiring ────────────────────────────────────────────

#[test]
fn o_continues_an_ordered_list_with_the_next_number() {
    // `o` after `1. Item` opens a fresh `2. ` item below and enters Insert.
    let mut st = state("1. Item\n");
    let mut vim = VimState::default();
    st.cursor.offset = 3; // inside "Item"
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('o'));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(st.buffer.contents(), "1. Item\n2. \n");
}

#[test]
fn o_continues_a_bullet_list_with_the_same_marker() {
    let mut st = state("- one\n");
    let mut vim = VimState::default();
    st.cursor.offset = 2; // inside "one"
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('o'));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(st.buffer.contents(), "- one\n- \n");
}

#[test]
fn o_continue_is_a_single_undo_unit() {
    let mut st = state("1. a\n2. b\n");
    let mut vim = VimState::default();
    st.cursor.offset = 0;
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('o'));
    // Inserted `2. ` and renumbered `b` to `3.` inline — all one delta.
    assert_eq!(st.buffer.contents(), "1. a\n2. \n3. b\n");
    // One undo restores the original document.
    feed(&mut vim, &mut st, esc());
    feed(&mut vim, &mut st, ch('u'));
    assert_eq!(st.buffer.contents(), "1. a\n2. b\n");
}

#[test]
fn capital_o_opens_an_item_above_a_later_list_item() {
    // `O` on the second item continues from the first, landing a new item
    // between them and renumbering the tail.
    let mut st = state("1. a\n2. b\n");
    let mut vim = VimState::default();
    st.cursor.offset = 5; // on "2. b"
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('O'));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(st.buffer.contents(), "1. a\n2. \n3. b\n");
}

#[test]
fn o_outside_a_list_opens_a_plain_line() {
    let mut st = state("hello\n");
    let mut vim = VimState::default();
    st.cursor.offset = 1;
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('o'));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
    assert_eq!(st.buffer.contents(), "hello\n\n");
}

#[test]
fn dd_renumbers_an_ordered_list() {
    // Deleting the middle item renumbers the survivors monotonically.
    let mut st = state("1. a\n2. b\n3. c\n");
    let mut vim = VimState::default();
    st.cursor.offset = 5; // on "2. b"
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('d'));
    assert_eq!(st.buffer.contents(), "1. a\n2. c\n");
}

#[test]
fn dd_does_not_renumber_a_bullet_list() {
    let mut st = state("- a\n- b\n- c\n");
    let mut vim = VimState::default();
    st.cursor.offset = 4; // on "- b"
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('d'));
    assert_eq!(st.buffer.contents(), "- a\n- c\n");
}

#[test]
fn indent_indents_a_bullet_list_item() {
    let mut st = state("- item\n");
    let mut vim = VimState::default();
    st.cursor.offset = 2; // on "item"
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('>'));
    feed(&mut vim, &mut st, ch('>'));
    assert_eq!(st.buffer.contents(), "    - item\n");
}

#[test]
fn outdent_round_trips_a_list_indent() {
    let mut st = state("    - item\n");
    let mut vim = VimState::default();
    st.cursor.offset = 6; // on "item"
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('<'));
    feed(&mut vim, &mut st, ch('<'));
    assert_eq!(st.buffer.contents(), "- item\n");
}

#[test]
fn indent_indents_a_nested_ordered_list_item() {
    // `>>` on the second ordered item nests it (fresh `1.`) and renumbers
    // the outer run — the structure-aware path, not plain space-indent.
    let mut st = state("1. a\n2. b\n3. c\n");
    let mut vim = VimState::default();
    st.cursor.offset = 5; // on "2. b"
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('>'));
    feed(&mut vim, &mut st, ch('>'));
    assert_eq!(st.buffer.contents(), "1. a\n    1. b\n2. c\n");
}

#[test]
fn indent_outside_a_list_falls_back_to_plain_spaces() {
    let mut st = state("hello\n");
    let mut vim = VimState::default();
    st.cursor.offset = 0;
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('>'));
    feed(&mut vim, &mut st, ch('>'));
    assert_eq!(st.buffer.contents(), "    hello\n");
}

#[test]
fn outdent_renumbers_the_item_into_the_outer_ordered_run() {
    // De-nesting an ordered item must renumber the source so the raw markers
    // match the (already sequential) rendered numbers — the outdented item
    // carries a stale nested `1.` into the outer list otherwise.
    let mut st = state("1. a\n2. b\n    1. c\n3. d\n");
    let mut vim = VimState::default();
    st.cursor.offset = 17; // on "c" in "    1. c"
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('<'));
    feed(&mut vim, &mut st, ch('<'));
    assert_eq!(st.buffer.contents(), "1. a\n2. b\n3. c\n4. d\n");
}

#[test]
fn indent_onto_an_existing_nested_run_renumbers_the_join() {
    // Nesting an item below a sibling nested run joins it as a duplicate `1.`;
    // the renumber pass must fix the nested sequence so the source matches the
    // rendered numbers.
    let mut st = state("1. a\n    1. x\n    2. y\n2. b\n");
    let mut vim = VimState::default();
    st.cursor.offset = 26; // on "b" in "2. b"
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('>'));
    feed(&mut vim, &mut st, ch('>'));
    assert_eq!(st.buffer.contents(), "1. a\n    1. x\n    2. y\n    3. b\n");
}

#[test]
fn dd_renumbers_an_outer_list_across_a_nested_child() {
    // Deleting an outer item whose sibling carries a nested child must still
    // renumber the outer run — the post-`dd` cursor lands on the nested child,
    // so a flat renumber would touch only the inner list and leave the outer
    // sequence stale (the rendered view shows it sequential regardless).
    let mut st = state("1. Ordered\n2. Second\n    1. nested\n3. Third\n");
    let mut vim = VimState::default();
    st.cursor.offset = 11; // on "2. Second"
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('d'));
    assert_eq!(
        st.buffer.contents(),
        "1. Ordered\n    1. nested\n2. Third\n",
    );
}

#[test]
fn dd_renumbers_each_nested_sublist_under_its_own_parent() {
    // Two parents each own a nested ordered sub-list; deleting a middle
    // top-level item renumbers the outer run (3→2) while both sub-lists keep
    // restarting at 1 under their own parent.
    let mut st = state("1. a\n    1. x\n    2. y\n2. b\n3. c\n    1. p\n    2. q\n");
    let mut vim = VimState::default();
    st.cursor.offset = 23; // on "2. b"
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('d'));
    assert_eq!(
        st.buffer.contents(),
        "1. a\n    1. x\n    2. y\n2. c\n    1. p\n    2. q\n",
    );
}

// ── Live `:s` substitution preview (inccommand) ──────────────────────────────

/// Open the `:` prompt and type `body` without submitting.
fn type_ex(vim: &mut VimState, st: &mut EditorState, body: &str) {
    feed(vim, st, ch(':'));
    for c in body.chars() {
        feed(vim, st, ch(c));
    }
}

#[test]
fn typing_a_substitution_pattern_previews_matches_live() {
    let mut st = state("foo bar\nfoo");
    let mut vim = VimState::default();
    type_ex(&mut vim, &mut st, "%s/foo");
    let preview = st.substitute_preview.as_ref().expect("preview active");
    assert_eq!(
        preview.highlights,
        vec![0..3, 8..11],
        "every line's first match highlights while the pattern is typed"
    );
    assert_eq!(st.buffer.contents(), "foo bar\nfoo", "text untouched");
    assert!(!st.dirty, "a highlight-only preview must not dirty");
}

#[test]
fn typing_a_replacement_previews_the_substituted_text() {
    let mut st = state("foo bar\nfoo");
    let mut vim = VimState::default();
    type_ex(&mut vim, &mut st, "%s/foo/quux/g");
    assert_eq!(
        st.buffer.contents(),
        "quux bar\nquux",
        "the buffer shows the substituted text while typing"
    );
    assert!(!st.dirty, "the preview edit must not dirty the buffer");
    assert_eq!(
        st.history.undo_depth(),
        0,
        "no undo delta may be recorded for the preview"
    );
    let preview = st.substitute_preview.as_ref().expect("preview active");
    assert_eq!(
        preview.highlights,
        vec![0..4, 9..13],
        "the inserted segments highlight in the previewed text"
    );
}

#[test]
fn esc_reverts_the_preview_and_restores_the_view() {
    let mut st = state("foo bar\nfoo");
    let mut vim = VimState::default();
    st.cursor.offset = 5;
    st.update_cursor_block();
    type_ex(&mut vim, &mut st, "%s/foo/quux/g");
    assert_eq!(st.buffer.contents(), "quux bar\nquux");
    feed(&mut vim, &mut st, esc());
    assert_eq!(st.buffer.contents(), "foo bar\nfoo", "Esc reverts");
    assert_eq!(st.cursor.offset, 5, "cursor restored");
    assert!(st.substitute_preview.is_none());
    assert!(vim.cmdline.is_none());
    // No phantom undo step was recorded by the preview round-trip.
    feed(&mut vim, &mut st, ch('u'));
    assert_eq!(st.buffer.contents(), "foo bar\nfoo");
}

#[test]
fn submitting_a_previewed_substitution_is_one_undo_unit() {
    let mut st = state("foo bar\nfoo");
    let mut vim = VimState::default();
    type_ex(&mut vim, &mut st, "%s/foo/bar/g");
    assert!(
        st.substitute_preview.is_some(),
        "preview active before Enter"
    );
    let out = feed(&mut vim, &mut st, key(KeyCode::Enter));
    assert_eq!(out, VimOutcome::Flash("2 substitutions".to_owned()));
    assert_eq!(st.buffer.contents(), "bar bar\nbar");
    assert!(st.dirty, "the committed substitution dirties the buffer");
    assert!(st.substitute_preview.is_none(), "preview ended on submit");
    feed(&mut vim, &mut st, ch('u'));
    assert_eq!(
        st.buffer.contents(),
        "foo bar\nfoo",
        "one `u` reverts the whole substitution — identical to a preview-less submit"
    );
}

#[test]
fn backspace_across_the_second_delimiter_walks_the_preview_back() {
    let mut st = state("a foo b");
    let mut vim = VimState::default();
    type_ex(&mut vim, &mut st, "%s/foo/XY");
    assert_eq!(st.buffer.contents(), "a XY b", "replacement previewed");
    feed(&mut vim, &mut st, key(KeyCode::Backspace));
    feed(&mut vim, &mut st, key(KeyCode::Backspace));
    // `:%s/foo/` — replacement field present but empty: deletion preview.
    assert_eq!(st.buffer.contents(), "a  b", "deletion previewed");
    feed(&mut vim, &mut st, key(KeyCode::Backspace));
    // `:%s/foo` — back to highlight-only on the original text.
    assert_eq!(st.buffer.contents(), "a foo b");
    let preview = st.substitute_preview.as_ref().expect("highlight-only");
    assert_eq!(preview.highlights, vec![2..5]);
}

#[test]
fn visual_range_preview_touches_only_the_selected_lines() {
    let mut st = state("foo\nfoo\nfoo");
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('j'));
    feed(&mut vim, &mut st, ch(':'));
    for c in "s/foo/bar/g".chars() {
        feed(&mut vim, &mut st, ch(c));
    }
    assert_eq!(
        st.buffer.contents(),
        "bar\nbar\nfoo",
        "the preview honours the '<,'> line span"
    );
    feed(&mut vim, &mut st, esc());
    assert_eq!(st.buffer.contents(), "foo\nfoo\nfoo");
}

#[test]
fn cmdline_cursor_moves_do_not_recompute_the_preview() {
    let mut st = state("foo bar");
    let mut vim = VimState::default();
    type_ex(&mut vim, &mut st, "%s/foo/XY/");
    assert_eq!(st.buffer.contents(), "XY bar");
    let version = st.buffer.version();
    // Keys that leave the command line unchanged must not revert +
    // reapply the preview (each recompute costs two full reparses).
    feed(&mut vim, &mut st, key(KeyCode::Left));
    feed(&mut vim, &mut st, key(KeyCode::Right));
    feed(&mut vim, &mut st, key(KeyCode::Home));
    assert_eq!(
        st.buffer.version(),
        version,
        "a no-op cmdline key must not touch the buffer"
    );
    assert_eq!(st.buffer.contents(), "XY bar");
}

#[test]
fn substitute_preview_suppresses_the_cursor_block_raw_reveal() {
    let mut st = state("foo bar\n");
    st.mode = Mode::Rendered;
    let mut vim = VimState::default();
    // No reveal timer armed → normally revealed; the preview parks the
    // cursor on the first affected line while the user types, so the
    // reveal must be suppressed or the block flips to raw source under
    // the preview highlights.
    st.cursor_block_entered_at = None;
    assert!(st.cursor_block_revealed());
    type_ex(&mut vim, &mut st, "%s/foo/bar/");
    assert!(st.substitute_preview.is_some(), "preview active");
    st.cursor_block_entered_at = None;
    assert!(
        !st.cursor_block_revealed(),
        "an active preview must suppress the raw reveal"
    );
    feed(&mut vim, &mut st, esc());
    st.cursor_block_entered_at = None;
    assert!(st.cursor_block_revealed(), "reveal returns after Esc");
}

#[test]
fn an_invalid_or_matchless_pattern_shows_no_preview() {
    let mut st = state("abc");
    let mut vim = VimState::default();
    type_ex(&mut vim, &mut st, "%s/zzz/x/");
    assert!(st.substitute_preview.is_none(), "no matches → no preview");
    assert_eq!(st.buffer.contents(), "abc");
    feed(&mut vim, &mut st, esc());
    // Half-typed group: invalid regex must not preview (and not panic).
    type_ex(&mut vim, &mut st, r"%s/a\v(");
    assert!(st.substitute_preview.is_none());
    assert_eq!(st.buffer.contents(), "abc");
}

// ── Live `/` `?` incremental search (incsearch) ───────────────────────────────

/// Open the search prompt (`/` or `?`) and type `query` without submitting.
fn type_search(vim: &mut VimState, st: &mut EditorState, prompt: char, query: &str) {
    feed(vim, st, ch(prompt));
    for c in query.chars() {
        feed(vim, st, ch(c));
    }
}

#[test]
fn typing_a_search_highlights_live_and_parks_on_the_next_match() {
    let mut st = state("foo bar\nfoo");
    let mut vim = VimState::default();
    type_search(&mut vim, &mut st, '/', "foo");
    let s = st.search.as_ref().expect("live session while typing");
    assert_eq!(s.matches, vec![0..3, 8..11]);
    // Forward search focuses the first match strictly after the origin.
    assert_eq!(s.focused_range(), Some(8..11));
    assert_eq!(st.cursor.offset, 8, "cursor parked on the focus");
    assert!(vim.cmdline.is_some(), "prompt still open");
}

#[test]
fn backward_search_parks_on_the_previous_match() {
    let mut st = state("foo bar\nfoo");
    let mut vim = VimState::default();
    st.place_cursor(8); // at the start of the second "foo"
    type_search(&mut vim, &mut st, '?', "foo");
    let s = st.search.as_ref().expect("live session while typing");
    assert_eq!(s.focused_range(), Some(0..3));
    assert_eq!(st.cursor.offset, 0);
}

#[test]
fn esc_restores_view_and_prior_hlsearch_session() {
    let mut st = state("foo bar\nfoo");
    let mut vim = VimState::default();
    // A prior hlsearch session is live when `/` opens.
    st.enter_search(SearchState::new("bar".to_owned(), None).expect("valid"));
    st.place_cursor(5);
    st.scroll = 1;
    type_search(&mut vim, &mut st, '/', "foo");
    assert_eq!(st.search.as_ref().map(|s| s.query.as_str()), Some("foo"));
    feed(&mut vim, &mut st, esc());
    assert_eq!(
        st.search.as_ref().map(|s| s.query.as_str()),
        Some("bar"),
        "Esc restores the prior hlsearch session"
    );
    assert_eq!(st.cursor.offset, 5, "cursor restored");
    assert_eq!(st.scroll, 1, "scroll restored");
    assert!(vim.cmdline.is_none());
    assert!(vim.incsearch.is_none());
}

#[test]
fn a_matchless_query_shows_no_highlights_and_stays_at_the_origin() {
    let mut st = state("foo bar");
    let mut vim = VimState::default();
    type_search(&mut vim, &mut st, '/', "barz");
    assert!(st.search.is_none(), "no match → no highlights");
    assert_eq!(st.cursor.offset, 0);
    // Backspacing to a matching prefix resumes the live session.
    feed(&mut vim, &mut st, key(KeyCode::Backspace));
    let s = st.search.as_ref().expect("prefix matches again");
    assert_eq!(s.focused_range(), Some(4..7));
}

#[test]
fn submit_restores_the_origin_before_the_app_level_search_runs() {
    let mut st = state("foo bar\nfoo");
    let mut vim = VimState::default();
    type_search(&mut vim, &mut st, '/', "foo");
    assert_eq!(st.cursor.offset, 8, "parked on the focus while typing");
    let out = feed(&mut vim, &mut st, key(KeyCode::Enter));
    // The reducer hands the query to the App (which runs
    // `enter_vim_search`); the incsearch session ends first, restoring
    // the origin so the App resolves the same cursor-relative focus a
    // preview-less submit would.
    assert_eq!(
        out,
        VimOutcome::EnterSearch {
            forward: true,
            query: "foo".to_owned(),
        }
    );
    assert_eq!(st.cursor.offset, 0, "origin restored for the App");
    assert!(st.search.is_none(), "transient session ended");
    assert!(vim.incsearch.is_none());
}

#[test]
fn search_history_recall_updates_the_live_session() {
    let mut st = state("foo bar");
    let mut vim = VimState {
        search_history: vec!["bar".to_owned()],
        ..Default::default()
    };
    feed(&mut vim, &mut st, ch('/'));
    assert!(st.search.is_none(), "empty prompt: nothing live yet");
    feed(&mut vim, &mut st, key(KeyCode::Up));
    let s = st.search.as_ref().expect("recalled query goes live");
    assert_eq!(s.query, "bar");
    assert_eq!(s.focused_range(), Some(4..7));
}

#[test]
fn incsearch_suppresses_the_cursor_block_raw_reveal() {
    let mut st = state("foo bar\n");
    let mut vim = VimState::default();
    st.cursor_block_entered_at = None;
    assert!(st.cursor_block_revealed());
    type_search(&mut vim, &mut st, '/', "bar");
    st.cursor_block_entered_at = None;
    assert!(
        !st.cursor_block_revealed(),
        "a live search session suppresses the raw reveal like any hlsearch"
    );
    feed(&mut vim, &mut st, esc());
    st.cursor_block_entered_at = None;
    assert!(st.cursor_block_revealed());
}

// ── Table-aware motions and commands ──────────────────────────────────────────
//
// A rendered table's `|` delimiters and alignment row are auto-managed
// chrome, so the motions that mean "move within this text" stay inside the
// cursor's cell and the line-oriented commands act on the row.  Every case
// below has a Raw-mode counterpart asserting stock vim behavior there —
// Raw is hand-editable source and gets no special treatment.

/// `| alpha | bravo |` / alignment / `| one | two |`.
const TBL: &str = "| alpha | bravo |\n|---|---|\n| one | two |\n";

/// Char offset of the first occurrence of `needle` in [`TBL`].
fn tbl_at(needle: &str) -> usize {
    TBL.find(needle).expect("needle present in the fixture")
}

/// A Rendered-mode editor on [`TBL`] with the cursor at `offset`.
fn table_state(offset: usize) -> EditorState {
    let mut st = state(TBL);
    st.cursor.offset = offset;
    st.update_cursor_block();
    st
}

#[test]
fn word_motion_stops_at_the_cell_edge() {
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('w'));
    assert_eq!(
        st.cursor.offset,
        tbl_at("alpha") + "alpha".len(),
        "`w` must stop at the cell's content end, not cross into `bravo`"
    );
}

#[test]
fn dollar_lands_on_the_cell_end_not_the_row_end() {
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('$'));
    assert_eq!(st.cursor.offset, tbl_at("alpha") + "alpha".len());
}

#[test]
fn caret_lands_on_the_cell_start_not_the_row_start() {
    let mut st = table_state(tbl_at("alpha") + 3);
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('^'));
    assert_eq!(st.cursor.offset, tbl_at("alpha"));
}

#[test]
fn find_for_a_char_in_another_cell_does_not_move() {
    // `b` only occurs in the next cell, so this is a failed find — vim
    // leaves the cursor where it was rather than moving it partway.
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('f'));
    feed(&mut vim, &mut st, ch('b'));
    assert_eq!(st.cursor.offset, tbl_at("alpha"));
}

#[test]
fn find_within_the_cell_still_works() {
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('f'));
    feed(&mut vim, &mut st, ch('h'));
    assert_eq!(st.cursor.offset, tbl_at("alpha") + 3);
}

#[test]
fn dw_never_eats_the_cell_delimiter() {
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    feed(&mut vim, &mut st, ch('w'));
    assert!(
        st.buffer.contents().starts_with("|  | bravo |\n"),
        "got {:?}",
        st.buffer.contents().lines().next()
    );
}

#[test]
fn capital_d_stops_at_the_cell_end() {
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('D'));
    assert!(
        st.buffer.contents().starts_with("|  | bravo |\n"),
        "`D` must not wipe the rest of the row's delimiters"
    );
}

#[test]
fn x_at_the_cell_end_does_not_delete_the_delimiter() {
    let mut st = table_state(tbl_at("alpha") + "alpha".len());
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('x'));
    assert_eq!(st.buffer.contents(), TBL);
}

#[test]
fn insert_entries_land_inside_the_cell() {
    let mut st = table_state(tbl_at("alpha") + 2);
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('I'));
    assert_eq!(st.cursor.offset, tbl_at("alpha"));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);

    let mut st = table_state(tbl_at("alpha") + 2);
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('A'));
    assert_eq!(st.cursor.offset, tbl_at("alpha") + "alpha".len());
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
}

#[test]
fn o_opens_a_structural_table_row() {
    let mut st = table_state(tbl_at("one"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('o'));
    let contents = st.buffer.contents();
    assert_eq!(contents.lines().count(), 4, "got {contents:?}");
    assert!(
        contents.lines().nth(3).is_some_and(|l| l.contains('|')),
        "the opened row must carry the table's delimiters, not be a blank line"
    );
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
}

#[test]
fn dd_deletes_the_table_row_and_fills_the_register() {
    let mut st = table_state(tbl_at("one"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    let out = feed(&mut vim, &mut st, ch('d'));
    assert_eq!(out, VimOutcome::Consumed);
    assert!(!st.buffer.contents().contains("one"));
    assert!(st.buffer.contents().contains("| alpha | bravo |"));
    assert_eq!(vim.register.text, "| one | two |\n");
    assert!(vim.register.linewise);
}

#[test]
fn dd_refuses_on_the_header_and_alignment_rows() {
    for offset in [tbl_at("alpha"), tbl_at("|---|") + 2] {
        let mut st = table_state(offset);
        let mut vim = VimState::default();
        feed(&mut vim, &mut st, ch('d'));
        let out = feed(&mut vim, &mut st, ch('d'));
        assert!(
            matches!(out, VimOutcome::Flash(_)),
            "expected a refusal flash, got {out:?}"
        );
        assert_eq!(st.buffer.contents(), TBL, "the table must be untouched");
    }
}

#[test]
fn cc_clears_the_cell_and_enters_insert() {
    let mut st = table_state(tbl_at("alpha") + 2);
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('c'));
    feed(&mut vim, &mut st, ch('c'));
    assert!(
        st.buffer.contents().starts_with("|  | bravo |\n"),
        "got {:?}",
        st.buffer.contents().lines().next()
    );
    assert!(st.buffer.contents().contains("| one | two |"));
    assert_eq!(vim.sub_mode, VimSubMode::Insert);
}

#[test]
fn join_and_indent_refuse_inside_a_table() {
    for keys in [vec!['J'], vec!['>', '>'], vec!['<', '<']] {
        let mut st = table_state(tbl_at("one"));
        let mut vim = VimState::default();
        let mut out = VimOutcome::Consumed;
        for k in keys.iter() {
            out = feed(&mut vim, &mut st, ch(*k));
        }
        assert!(
            matches!(out, VimOutcome::Flash(_)),
            "expected {keys:?} to refuse inside a table, got {out:?}"
        );
        assert_eq!(st.buffer.contents(), TBL);
    }
}

#[test]
fn visual_line_delete_refuses_on_a_protected_row() {
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    let out = feed(&mut vim, &mut st, ch('d'));
    assert!(
        matches!(out, VimOutcome::Flash(_)),
        "expected a refusal flash, got {out:?}"
    );
    assert_eq!(st.buffer.contents(), TBL);
}

#[test]
fn visual_line_delete_removes_a_data_row() {
    let mut st = table_state(tbl_at("one"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('d'));
    assert!(!st.buffer.contents().contains("one"));
    assert!(st.buffer.contents().contains("| alpha | bravo |"));
}

#[test]
fn document_motions_still_cross_the_table() {
    // `gg` / `G` / `}` exist to leave the current context — they are never
    // confined to a cell.
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('G'));
    assert_eq!(
        st.buffer.char_to_line(st.cursor.offset),
        2,
        "`G` must reach the last row rather than clamping inside the header cell"
    );
}

#[test]
fn a_counted_line_jump_still_crosses_the_table() {
    // `{count}G` inherits `G`'s unscoped classification.
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('3'));
    feed(&mut vim, &mut st, ch('G'));
    assert_eq!(st.buffer.char_to_line(st.cursor.offset), 2);
}

// ── Raw mode keeps stock vim behavior ─────────────────────────────────────────

/// A Raw-mode editor on [`TBL`] with the cursor at `offset`.
fn raw_table_state(offset: usize) -> EditorState {
    let mut st = table_state(offset);
    st.mode = Mode::Raw;
    st.update_cursor_block();
    st
}

#[test]
fn raw_mode_dd_deletes_the_raw_line() {
    // No structural protection in Raw — `dd` on the header deletes that
    // source line, exactly as it would on any other text.
    let mut st = raw_table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('d'));
    let out = feed(&mut vim, &mut st, ch('d'));
    assert_eq!(out, VimOutcome::Consumed);
    assert!(!st.buffer.contents().contains("alpha"));
}

#[test]
fn raw_mode_word_motion_crosses_the_delimiter() {
    let mut st = raw_table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('w'));
    assert!(
        st.cursor.offset > tbl_at("alpha") + "alpha".len(),
        "Raw mode gets no cell scoping — `w` walks onto the `|`"
    );
}

#[test]
fn raw_mode_dollar_reaches_the_row_end() {
    let mut st = raw_table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('$'));
    // The whole row is ordinary text in Raw, so `$` reaches its end rather
    // than stopping at the first cell's content (offset 7 in Rendered).
    assert_eq!(st.cursor.offset, "| alpha | bravo |".len());
}

#[test]
fn raw_mode_o_opens_a_plain_line() {
    let mut st = raw_table_state(tbl_at("one"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('o'));
    assert_eq!(
        st.buffer.contents(),
        "| alpha | bravo |\n|---|---|\n| one | two |\n\n",
        "Raw `o` inserts a bare newline, not a table row"
    );
}

#[test]
fn raw_mode_join_and_indent_are_not_refused() {
    let mut st = raw_table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    let out = feed(&mut vim, &mut st, ch('J'));
    assert_eq!(out, VimOutcome::Consumed);
    assert!(st
        .buffer
        .contents()
        .starts_with("| alpha | bravo | |---|---|"));
}

// ── Structural refusals: the routes that reach a protected row ────────────────
//
// The cell clamp shapes the ordinary keystroke; these are the ways a range
// gets to a header or alignment row *without* a cell to clamp against.  Each
// one corrupted the table silently before `range_breaks_a_table` guarded the
// two operator funnels.

/// Feed each char of `keys` and return the last outcome.
fn feed_keys(vim: &mut VimState, st: &mut EditorState, keys: &str) -> VimOutcome {
    let mut out = VimOutcome::Consumed;
    for k in keys.chars() {
        out = feed(vim, st, ch(k));
    }
    out
}

#[test]
fn a_counted_dd_cannot_delete_the_header() {
    // `2dd` on the header used to bypass the refusal entirely and leave
    // `| one | two |` standing alone as paragraph text.
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    let out = feed_keys(&mut vim, &mut st, "2dd");
    assert!(
        matches!(out, VimOutcome::Flash(_)),
        "expected a refusal flash, got {out:?}"
    );
    assert_eq!(st.buffer.contents(), TBL);
}

#[test]
fn dj_cannot_slice_the_header_off_the_table() {
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    let out = feed_keys(&mut vim, &mut st, "dj");
    assert!(matches!(out, VimOutcome::Flash(_)), "got {out:?}");
    assert_eq!(st.buffer.contents(), TBL);
}

#[test]
fn cc_cannot_blank_the_alignment_row() {
    // `cc` there fell through to a plain linewise change, which split the
    // table into two paragraphs.
    let mut st = table_state(tbl_at("|---|") + 2);
    let mut vim = VimState::default();
    let out = feed_keys(&mut vim, &mut st, "cc");
    assert!(matches!(out, VimOutcome::Flash(_)), "got {out:?}");
    assert_eq!(st.buffer.contents(), TBL);
    assert_eq!(vim.sub_mode, VimSubMode::Normal, "no Insert on a refusal");
}

#[test]
fn a_visual_line_selection_leaving_the_table_still_protects_the_header() {
    // Anchor on the header, then move the cursor *out* of the table: the
    // guard keys on the selection, not on where the cursor ended up.
    let src = "para\n| alpha | bravo |\n|---|---|\n| one | two |\n";
    let mut st = state(src);
    st.cursor.offset = src.find("alpha").expect("fixture");
    st.update_cursor_block();
    let mut vim = VimState::default();
    let out = feed_keys(&mut vim, &mut st, "Vkd");
    assert!(matches!(out, VimOutcome::Flash(_)), "got {out:?}");
    assert_eq!(st.buffer.contents(), src);
}

#[test]
fn a_visual_line_selection_covering_the_whole_table_may_delete_it() {
    // Deleting a table outright is a legitimate edit — there is no
    // half-table left to be broken, so this must *not* be refused.
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed_keys(&mut vim, &mut st, "Vjjd");
    assert_eq!(st.buffer.contents().trim(), "");
}

#[test]
fn a_charwise_visual_selection_stops_at_the_cell_edge() {
    // `l` steps cell-to-cell in Normal, but a charwise Visual highlight
    // that crossed the `|` would promise an edit the structural guard then
    // refuses — so here it stops on the cell's last character, however many
    // times `l` is pressed, and the delete it promised goes through.
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    for _ in 0..9 {
        feed(&mut vim, &mut st, ch('l'));
    }
    assert_eq!(
        st.cursor.offset,
        tbl_at("alpha") + "alpha".len() - 1,
        "the cursor must rest on the cell's last char, not the append slot"
    );
    let out = feed(&mut vim, &mut st, ch('d'));
    assert_eq!(out, VimOutcome::Consumed, "the highlight was all in-cell");
    assert_eq!(
        st.buffer.contents(),
        "|  | bravo |\n|---|---|\n| one | two |\n"
    );
}

/// The mirror of the above: `h` can't reverse out of the cell either.
#[test]
fn a_charwise_visual_selection_stops_at_the_cell_start() {
    let mut st = table_state(tbl_at("bravo") + 2);
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    for _ in 0..9 {
        feed(&mut vim, &mut st, ch('h'));
    }
    assert_eq!(st.cursor.offset, tbl_at("bravo"));
    let out = feed(&mut vim, &mut st, ch('d'));
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(
        st.buffer.contents(),
        "| alpha | vo |\n|---|---|\n| one | two |\n"
    );
}

/// `$` parks on the cell's append slot — the padding space before the `|`.
/// A charwise selection opened there covers that space, and the edit takes
/// it: the guard permits the range (it measures the cell untrimmed), so `d`
/// would leave `alpha` abutting its delimiter.  `v` anchors one grapheme
/// back instead.
#[test]
fn opening_visual_on_the_cell_append_slot_anchors_on_the_last_char() {
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('$'));
    assert_eq!(st.cursor.offset, tbl_at("alpha") + "alpha".len());
    feed(&mut vim, &mut st, ch('v'));
    assert_eq!(st.cursor.offset, tbl_at("alpha") + "alpha".len() - 1);
    let out = feed(&mut vim, &mut st, ch('d'));
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(
        st.buffer.contents(),
        "| alph | bravo |\n|---|---|\n| one | two |\n"
    );
}

/// `V`→`v` is the other door into charwise Visual, and it inherits a cursor
/// `$` may have parked on the append slot.  The span here runs from the
/// cell's first char (where `V` anchored) to its last, so `d` clears the
/// content and leaves both padding spaces — without the pull-back it would
/// take the trailing one too and leave `| | bravo |`.
#[test]
fn toggling_visual_line_to_charwise_pulls_the_cursor_off_the_append_slot() {
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('$'));
    assert_eq!(st.cursor.offset, tbl_at("alpha") + "alpha".len());
    feed(&mut vim, &mut st, ch('v'));
    assert_eq!(st.cursor.offset, tbl_at("alpha") + "alpha".len() - 1);
    let out = feed(&mut vim, &mut st, ch('d'));
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(
        st.buffer.contents(),
        "|  | bravo |\n|---|---|\n| one | two |\n"
    );
}

/// The same toggle, with `r` — which overwrites rather than deletes, so an
/// unpulled cursor would replace the padding space itself and leave the
/// cell's text touching the `|`.
#[test]
fn toggling_visual_line_to_charwise_protects_the_padding_from_replace() {
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('$'));
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('r'));
    let out = feed(&mut vim, &mut st, ch('z'));
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(
        st.buffer.contents(),
        "| zzzzz | bravo |\n|---|---|\n| one | two |\n",
        "the space before the delimiter survives"
    );
}

/// `o` puts the append slot on the *anchor* end instead, so the pull-back
/// has to reach both ends of the span — each against its own cell.
#[test]
fn toggling_visual_line_to_charwise_pulls_the_anchor_too() {
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('V'));
    feed(&mut vim, &mut st, ch('$'));
    feed(&mut vim, &mut st, ch('o')); // anchor ← append slot, cursor ← cell start
    feed(&mut vim, &mut st, ch('v'));
    let out = feed(&mut vim, &mut st, ch('d'));
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(
        st.buffer.contents(),
        "|  | bravo |\n|---|---|\n| one | two |\n"
    );
}

/// The clamp is horizontal only, so the range guard is still the thing that
/// catches a selection which left the cell by another route — here `j`,
/// which steps to the cell below and drags the row's `|`s and newlines into
/// the span.
#[test]
fn a_charwise_visual_delete_reaching_another_row_is_refused() {
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('j'));
    let out = feed(&mut vim, &mut st, ch('d'));
    assert!(matches!(out, VimOutcome::Flash(_)), "got {out:?}");
    assert_eq!(st.buffer.contents(), TBL);
}

#[test]
fn a_charwise_visual_delete_inside_one_cell_still_works() {
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed_keys(&mut vim, &mut st, "vlld");
    assert!(
        st.buffer.contents().starts_with("| ha | bravo |\n"),
        "got {:?}",
        st.buffer.contents().lines().next()
    );
}

#[test]
fn a_visual_replace_cannot_overwrite_a_delimiter() {
    // `l` can no longer carry the selection out of the cell, so the span
    // that reaches a delimiter comes from `j` — the vertical step the cell
    // clamp deliberately leaves alone.
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('v'));
    feed(&mut vim, &mut st, ch('j'));
    feed(&mut vim, &mut st, ch('r'));
    let out = feed(&mut vim, &mut st, ch('z'));
    assert!(matches!(out, VimOutcome::Flash(_)), "got {out:?}");
    assert_eq!(st.buffer.contents(), TBL);
}

#[test]
fn yy_on_a_protected_row_is_never_refused() {
    // Yank mutates nothing, so the guard must not fire on it.
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    let out = feed_keys(&mut vim, &mut st, "yy");
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(vim.register.text, "| alpha | bravo |\n");
    assert_eq!(st.buffer.contents(), TBL);
}

#[test]
fn join_refuses_on_the_line_above_a_table() {
    // `J` there merges the paragraph onto the header row.
    let src = "para\n| alpha | bravo |\n|---|---|\n| one | two |\n";
    let mut st = state(src);
    st.cursor.offset = 0;
    st.update_cursor_block();
    let mut vim = VimState::default();
    let out = feed(&mut vim, &mut st, ch('J'));
    assert!(matches!(out, VimOutcome::Flash(_)), "got {out:?}");
    assert_eq!(st.buffer.contents(), src);
}

// ── Paste ─────────────────────────────────────────────────────────────────────

#[test]
fn a_yanked_row_pasted_on_the_header_lands_below_the_alignment_row() {
    // `dd` fills the register with a data row; the ordinary linewise paste
    // would drop it between the header and the alignment row.
    let mut st = table_state(tbl_at("one"));
    let mut vim = VimState::default();
    feed_keys(&mut vim, &mut st, "dd");
    st.cursor.offset = tbl_at("alpha");
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('p'));
    assert_eq!(
        st.buffer.contents(),
        "| alpha | bravo |\n|---|---|\n| one | two |\n",
        "the row must come back below the alignment row"
    );
}

#[test]
fn pasting_prose_into_a_table_is_refused() {
    // Yank a paragraph line, then try to drop it between two table rows.
    let src = "para\n| alpha | bravo |\n|---|---|\n| one | two |\n";
    let mut st = state(src);
    st.cursor.offset = 0;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed_keys(&mut vim, &mut st, "yy");
    st.cursor.offset = src.find("one").expect("fixture");
    st.update_cursor_block();
    let out = feed(&mut vim, &mut st, ch('p'));
    assert!(matches!(out, VimOutcome::Flash(_)), "got {out:?}");
    assert_eq!(st.buffer.contents(), src);
}

#[test]
fn raw_mode_paste_keeps_the_plain_linewise_landing_spot() {
    // No table awareness in Raw: yank the data row, then `p` on the header
    // drops it right below, above the alignment row, as stock vim would.
    let mut st = raw_table_state(tbl_at("one"));
    let mut vim = VimState::default();
    feed_keys(&mut vim, &mut st, "yy");
    st.cursor.offset = tbl_at("alpha");
    st.update_cursor_block();
    feed(&mut vim, &mut st, ch('p'));
    assert_eq!(
        st.buffer.contents(),
        "| alpha | bravo |\n| one | two |\n|---|---|\n| one | two |\n"
    );
}

#[test]
fn raw_mode_counted_dd_deletes_the_raw_lines() {
    let mut st = raw_table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    let out = feed_keys(&mut vim, &mut st, "2dd");
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(st.buffer.contents(), "| one | two |\n");
}

#[test]
fn a_multibyte_cell_clamps_on_char_boundaries() {
    // `cell_scope` converts `table_edit`'s byte offsets to char offsets;
    // a cell of multibyte text is where an off-by-a-byte would panic or
    // land the cursor mid-character.
    let src = "| héllo wörld | b |\n|---|---|\n| one | two |\n";
    let mut st = state(src);
    st.cursor.offset = 2; // on the `h`
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('$'));
    assert_eq!(st.cursor.offset, 13, "`$` stops past the cell's last char");

    // `D` from the cell's first char clears the cell and nothing else.
    let mut st = state(src);
    st.cursor.offset = 2;
    st.update_cursor_block();
    let mut vim = VimState::default();
    feed(&mut vim, &mut st, ch('D'));
    assert_eq!(
        st.buffer.contents(),
        "|  | b |\n|---|---|\n| one | two |\n",
        "`D` clears the cell without touching the delimiters"
    );
}

// ── The guard must not over-refuse ────────────────────────────────────────────

#[test]
fn a_counted_dd_over_data_rows_only_is_allowed() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut st = state(src);
    st.cursor.offset = src.find('1').expect("fixture");
    st.update_cursor_block();
    let mut vim = VimState::default();
    let out = feed_keys(&mut vim, &mut st, "2dd");
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(
        st.buffer.contents(),
        "| a | b |\n|---|---|",
        "both data rows go; the last line takes the trailing newline with it"
    );
}

#[test]
fn ordinary_cell_edits_are_never_refused() {
    // `diw`, `ciw`, `x` and `dw` all stay inside the cell, so none of them
    // may trip the structural guard.
    for (keys, expected) in [
        ("diw", "|  | bravo |"),
        ("ciw", "|  | bravo |"),
        ("dw", "|  | bravo |"),
        ("x", "| lpha | bravo |"),
    ] {
        let mut st = table_state(tbl_at("alpha"));
        let mut vim = VimState::default();
        let out = feed_keys(&mut vim, &mut st, keys);
        assert_eq!(out, VimOutcome::Consumed, "{keys} must not be refused");
        assert_eq!(
            st.buffer.contents().lines().next(),
            Some(expected),
            "{keys} left the wrong row"
        );
    }
}

#[test]
fn the_alignment_row_stays_hand_editable() {
    // No cell scope there, and `x` must keep working so a user can retype
    // the dashes — only whole-row loss is refused.
    let mut st = table_state(tbl_at("|---|") + 2);
    let mut vim = VimState::default();
    let out = feed(&mut vim, &mut st, ch('x'));
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(
        st.buffer.contents(),
        "| alpha | bravo |\n|--|---|\n| one | two |\n"
    );
}

#[test]
fn edits_outside_a_table_are_untouched_by_the_guard() {
    let src = "para one\npara two\n\n| a | b |\n|---|---|\n";
    let mut st = state(src);
    st.cursor.offset = 0;
    st.update_cursor_block();
    let mut vim = VimState::default();
    let out = feed_keys(&mut vim, &mut st, "dd");
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(st.buffer.contents(), "para two\n\n| a | b |\n|---|---|\n");
}

// ── Register shape vs. selection shape (Visual `p`) ───────────────────────────
//
// A Visual paste reconciles the register's shape with the selection's before
// it writes, so the guard has to be asked about the payload as it will land.
// Asking about the register's own flags let each mismatch through.

#[test]
fn a_row_register_cannot_be_pasted_into_a_cell() {
    // `dd` fills the register linewise with a whole row; dropping that over a
    // charwise selection spliced the `|` and the newline into mid-cell.
    let mut st = table_state(tbl_at("one"));
    let mut vim = VimState::default();
    feed_keys(&mut vim, &mut st, "dd");
    assert!(vim.register.linewise);
    let remaining = st.buffer.contents();

    st.cursor.offset = tbl_at("alpha");
    st.update_cursor_block();
    let out = feed_keys(&mut vim, &mut st, "vlp");
    assert!(matches!(out, VimOutcome::Flash(_)), "got {out:?}");
    assert_eq!(st.buffer.contents(), remaining);
}

#[test]
fn a_prose_register_cannot_replace_a_whole_row() {
    // The mirror case: a charwise register grows a trailing newline when it
    // lands over a VisualLine selection, so the row it replaces stops being a
    // table row at all.
    let src = "para\n| alpha | bravo |\n|---|---|\n| one | two |\n";
    let mut st = state(src);
    st.cursor.offset = 0;
    st.update_cursor_block();
    let mut vim = VimState::default();
    // Yank `para` charwise, then select the data row linewise and paste.
    feed_keys(&mut vim, &mut st, "v$y");
    assert!(!vim.register.linewise);
    st.cursor.offset = src.find("one").expect("fixture");
    st.update_cursor_block();
    let out = feed_keys(&mut vim, &mut st, "Vp");
    assert!(matches!(out, VimOutcome::Flash(_)), "got {out:?}");
    assert_eq!(st.buffer.contents(), src);
}

#[test]
fn a_row_register_still_replaces_a_selected_row() {
    // The shapes agree here, so the paste must go through.
    let mut st = table_state(tbl_at("one"));
    let mut vim = VimState::default();
    feed_keys(&mut vim, &mut st, "yy");
    let out = feed_keys(&mut vim, &mut st, "Vp");
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(st.buffer.contents(), TBL);
}

#[test]
fn ordinary_text_still_pastes_inside_a_cell() {
    // A charwise register over a charwise selection only widens the cell.
    let mut st = table_state(tbl_at("one"));
    let mut vim = VimState::default();
    feed_keys(&mut vim, &mut st, "vly"); // yank "on"
    assert!(!vim.register.linewise);
    st.cursor.offset = tbl_at("alpha");
    st.update_cursor_block();
    let out = feed_keys(&mut vim, &mut st, "vp");
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(
        st.buffer.contents(),
        "| onlpha | bravo |\n|---|---|\n| one | two |\n"
    );
}

// ── Normal-mode `r{c}` ────────────────────────────────────────────────────────

#[test]
fn a_counted_replace_cannot_overwrite_a_delimiter() {
    // `7rx` on `alpha` reached past the cell and turned `alpha |` into
    // `xxxxxxx`, leaving a one-cell header over a two-cell alignment row.
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    let out = feed_keys(&mut vim, &mut st, "7rx");
    assert!(matches!(out, VimOutcome::Flash(_)), "got {out:?}");
    assert_eq!(st.buffer.contents(), TBL);
}

#[test]
fn a_replace_inside_the_cell_still_works() {
    let mut st = table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    let out = feed_keys(&mut vim, &mut st, "3rx");
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(
        st.buffer.contents(),
        "| xxxha | bravo |\n|---|---|\n| one | two |\n"
    );
}

#[test]
fn raw_mode_counted_replace_is_unscoped() {
    // Raw is hand-editable source: no table awareness, so the delimiter is
    // just another character.
    let mut st = raw_table_state(tbl_at("alpha"));
    let mut vim = VimState::default();
    let out = feed_keys(&mut vim, &mut st, "7rx");
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(
        st.buffer.contents(),
        "| xxxxxxx bravo |\n|---|---|\n| one | two |\n"
    );
}

// ── The `J` guard's span ──────────────────────────────────────────────────────

#[test]
fn a_counted_join_that_never_reaches_the_table_is_allowed() {
    // `2J` makes one join, exactly as bare `J` does, so it stops a line short
    // of the header — the guard used to over-reach by one line and refuse it.
    let src = "para one\npara two\n| alpha | bravo |\n|---|---|\n| one | two |\n";
    let mut st = state(src);
    st.cursor.offset = 0;
    st.update_cursor_block();
    let mut vim = VimState::default();
    let out = feed_keys(&mut vim, &mut st, "2J");
    assert_eq!(out, VimOutcome::Consumed);
    assert_eq!(
        st.buffer.contents(),
        "para one para two\n| alpha | bravo |\n|---|---|\n| one | two |\n"
    );
}

#[test]
fn a_counted_join_that_does_reach_the_table_is_refused() {
    // `3J` makes two joins, so it pulls the header row up onto the prose.
    let src = "para one\npara two\n| alpha | bravo |\n|---|---|\n| one | two |\n";
    let mut st = state(src);
    st.cursor.offset = 0;
    st.update_cursor_block();
    let mut vim = VimState::default();
    let out = feed_keys(&mut vim, &mut st, "3J");
    assert!(matches!(out, VimOutcome::Flash(_)), "got {out:?}");
    assert_eq!(st.buffer.contents(), src);
}
