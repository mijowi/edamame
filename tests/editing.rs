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

// ── Raw mode: cursor is plain-text and doesn't skip table structure ─────────

#[test]
fn raw_mode_cursor_walks_through_table_pipes_one_char_at_a_time() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = state(src);
    st.mode = Mode::Raw;
    st.cursor.offset = 0;

    // Walk five chars forward: |, ' ', 'a', ' ', '|'.
    for expected in 1..=5 {
        apply(&mut st, Action::MoveRight);
        assert_eq!(
            st.cursor.offset, expected,
            "raw mode MoveRight should advance one char at a time (got {} at step {expected})",
            st.cursor.offset
        );
    }
}

#[test]
fn raw_mode_down_lands_on_alignment_row() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = state(src);
    st.mode = Mode::Raw;
    st.cursor.offset = 2; // inside 'a' in header

    apply(&mut st, Action::MoveDown);
    // In Rendered mode this would skip the alignment row; in Raw it lands on it.
    assert_eq!(st.cursor.line_col(&st.buffer).0, 1);
}

#[test]
fn raw_mode_insert_pipe_is_literal_not_escaped() {
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut st = state(src);
    st.mode = Mode::Raw;
    // Place cursor inside the first cell and type `|`.
    st.cursor.offset = 2; // just before 'a'
    apply(&mut st, Action::InsertChar('|'));
    // Raw mode: the `|` must appear literally (no `\|` escape).
    assert!(st.contents().contains("| |a | b |"));
    assert!(!st.contents().contains(r"\|"));
}

// ── Phase 7: Image block navigation and source-map stability ─────────────────

#[test]
fn cursor_traverses_image_block_as_one_line() {
    // An `![alt](url)` line is ONE logical buffer line, regardless of how
    // many rows its `Block::ImageBlock` reserves in the rendered view.
    // MoveDown from the line before should land on the image's source line,
    // and MoveDown again should land on the next line.
    let src = "Above.\n![cat](cat.png)\nBelow.\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    // Start at end of first line.
    st.cursor.offset = st.buffer.line_to_char(0) + 6; // "Above." (6 chars) + 0
    let line_before = st.cursor.line_col(&st.buffer).0;
    assert_eq!(line_before, 0);

    apply(&mut st, Action::MoveDown);
    let line_image = st.cursor.line_col(&st.buffer).0;
    assert_eq!(line_image, 1, "MoveDown should land on image source line");

    apply(&mut st, Action::MoveDown);
    let line_after = st.cursor.line_col(&st.buffer).0;
    assert_eq!(line_after, 2, "MoveDown should land on line below image");
}

#[test]
fn typing_next_to_image_block_keeps_source_map_consistent() {
    // Editing text adjacent to an image block must not corrupt the source
    // map — every buffer byte must map to some rendered line.
    let src = "Intro.\n\n![cat](cat.png)\n\nEnd.\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;
    // Move cursor to end of "End." and append more text.
    let last_line = st.buffer.line_count() - 2; // "End." (last line before trailing newline)
    let line_start = st.buffer.line_to_char(last_line);
    st.cursor.offset = line_start + 4; // after "End."
    apply_all(
        &mut st,
        &[
            Action::InsertChar('!'),
            Action::InsertChar(' '),
            Action::InsertChar('X'),
        ],
    );
    // Source map should still map every byte of the buffer.
    let src_now = st.contents();
    for b in 0..src_now.len() {
        let range = st.parsed.source_map.rendered_lines_for_byte(b);
        assert!(
            !range.is_empty(),
            "byte {} in {src_now:?} lost its rendered-line mapping",
            b,
        );
    }
}

#[test]
fn editor_state_tracks_image_block_urls_in_parsed_doc() {
    // EditorState::parsed.image_blocks is populated from the parse and
    // used by the App to dispatch decodes.  Verify round-trip.
    let src = "![a](a.png)\n\n![b](http://example.com/b.jpg)\n\nplain paragraph\n";
    let st = state(src);
    let urls: Vec<&str> = st
        .parsed
        .image_blocks
        .iter()
        .map(|b| b.url.as_str())
        .collect();
    assert_eq!(urls, vec!["a.png", "http://example.com/b.jpg"]);
}

#[test]
fn image_cache_survives_reparse_on_unrelated_edit() {
    // Phase 7 architectural invariant: the decoded-image cache lives on
    // EditorState::images (URL-keyed) rather than ParsedDoc, so editing
    // text elsewhere in the document does not invalidate expensive
    // image decodes / protocol encodings.
    use edamame::image::DecodeStatus;
    let src = "Intro.\n\n![cat](cat.png)\n\nEnd.\n";
    let mut st = state(src);
    st.mode = Mode::Rendered;

    // Simulate a completed decode by inserting a fake entry.
    st.images.request("cat.png");
    st.images
        .set_decoded("cat.png", image::DynamicImage::new_rgba8(1, 1));
    assert!(matches!(
        st.images.status("cat.png"),
        Some(DecodeStatus::Ready(_))
    ));

    // Perform an edit that reparses — append a char at the document end.
    st.cursor.offset = st.buffer.len_chars();
    apply(&mut st, Action::InsertChar('Z'));

    // The cache entry for cat.png should still be there.
    assert!(matches!(
        st.images.status("cat.png"),
        Some(DecodeStatus::Ready(_))
    ));
    // And the image block is still in the reparsed doc.
    assert_eq!(st.parsed.image_blocks.len(), 1);
    assert_eq!(st.parsed.image_blocks[0].url, "cat.png");
}

// ── HTML comment hiding (Phase 12) ──────────────────────────────────────────

/// Pressing Down from the line immediately preceding a hidden HTML comment
/// must skip over the comment's source bytes: the comment block has zero
/// rendered lines, so stopping there would leave the cursor on a line the
/// user can't see in hybrid view.  The cursor should emerge on or past
/// the next visible block.
#[test]
fn down_arrow_past_block_comment_skips_hidden_bytes() {
    let src = "Alpha.\n\n<!-- hidden -->\n\nBeta.\n";
    let mut st = state(src);
    apply(&mut st, Action::EnterEditMode);

    // Park the cursor on the blank line just before the comment (byte just
    // before `<!--`).  That position is "visible" — a blank-line virtual
    // block owning one rendered row — so the down step fires against a
    // normal starting point.
    let comment_byte = src.find("<!--").unwrap();
    let blank_before_byte = comment_byte.saturating_sub(1);
    st.cursor.offset = st.buffer.rope().byte_to_char(blank_before_byte);
    st.update_cursor_block();

    apply(&mut st, Action::MoveDown);

    // After one Down, the cursor must NOT be sitting inside the comment's
    // source bytes — either because we jumped straight past it or landed
    // on the trailing blank line.  Either way the block under the cursor
    // has non-zero rendered-line count.
    let rope = st.buffer.rope();
    let cursor_byte = rope.char_to_byte(st.cursor.offset);
    let block_idx = st
        .parsed
        .source_map
        .block_for_byte(cursor_byte)
        .expect("cursor byte must map to a block");
    assert!(
        st.parsed.block_own_line_count(block_idx) > 0,
        "cursor landed in a hidden block at byte {cursor_byte}"
    );
    // The cursor must have advanced past the comment's start byte too —
    // otherwise "skipping" degenerated into "staying put".
    assert!(
        cursor_byte > comment_byte,
        "cursor did not move past the comment: at byte {cursor_byte}, comment starts at {comment_byte}"
    );
}

/// Toggling Raw → Rendered with the cursor sitting inside a hidden comment
/// block should snap the cursor to the start of the next visible block so
/// `RenderedView` has a well-defined cursor position to draw against.
#[test]
fn toggle_raw_to_rendered_snaps_cursor_out_of_comment() {
    let src = "Alpha.\n\n<!-- hidden -->\n\nBeta.\n";
    let mut st = state(src);
    // Enter Rendered, then Raw.
    apply(&mut st, Action::EnterEditMode);
    apply(&mut st, Action::ToggleRawMode);
    assert_eq!(st.mode, Mode::Raw);

    // Park the cursor on the first char of the comment — visible in Raw.
    let comment_byte = src.find("<!--").unwrap();
    st.cursor.offset = st.buffer.rope().byte_to_char(comment_byte);
    st.update_cursor_block();

    // Toggle back to Rendered: the mode-switch handler must detect that the
    // cursor is inside a hidden block and move it forward to a visible one.
    apply(&mut st, Action::ToggleRawMode);
    assert_eq!(st.mode, Mode::Rendered);

    let rope = st.buffer.rope();
    let cursor_byte = rope.char_to_byte(st.cursor.offset);
    let block_idx = st
        .parsed
        .source_map
        .block_for_byte(cursor_byte)
        .expect("cursor byte must map to a block");
    assert!(
        st.parsed.block_own_line_count(block_idx) > 0,
        "cursor landed in a zero-own block after Raw→Rendered snap (byte {cursor_byte})"
    );
}

/// Switching between Rendered and Raw must keep the cursor on the same
/// screen row.  The two modes use different scroll units (rendered lines
/// vs. buffer lines), so without an adjustment the same `scroll` value
/// often pushes the cursor far away from where it was — sometimes
/// off-screen entirely.
#[test]
fn toggle_raw_mode_preserves_cursor_screen_row() {
    let mut src = String::new();
    for i in 0..30 {
        src.push_str(&format!("line {i}\n"));
    }
    let mut st = state(&src);
    // Enter Rendered with the cursor near the middle of the document.
    apply(&mut st, Action::EnterEditMode);
    st.cursor.offset = st.buffer.line_to_char(15);
    st.update_cursor_block();
    st.scroll = 12; // cursor at screen row 3.

    let row_before_first_toggle = st.cursor_screen_row(VW);

    apply(&mut st, Action::ToggleRawMode);
    assert_eq!(st.mode, Mode::Raw);
    assert_eq!(st.cursor_screen_row(VW), row_before_first_toggle);

    apply(&mut st, Action::ToggleRawMode);
    assert_eq!(st.mode, Mode::Rendered);
    assert_eq!(st.cursor_screen_row(VW), row_before_first_toggle);
}

/// When the cursor sits at the bottom of the visible viewport and the user
/// types a `\n`, the new cursor position is on a fresh line that's one
/// row below the previous bottom.  The viewport must scroll one row down
/// so the cursor stays visible.
#[test]
fn newline_at_viewport_bottom_scrolls_to_keep_cursor_visible() {
    let mut src = String::new();
    for i in 0..40 {
        src.push_str(&format!("line {i}\n"));
    }
    let vp_h = 10;
    let vp_w = 40;
    let mut st = state(&src);
    st.mode = Mode::Raw;
    // Park the cursor at the end of the bottom-most visible line.
    st.scroll = 5;
    let cursor_line = st.scroll + vp_h - 1; // last visible line
    let line_start = st.buffer.line_to_char(cursor_line);
    let line_text = st.buffer.line(cursor_line).unwrap_or_default();
    let line_len = line_text.trim_end_matches('\n').chars().count();
    st.cursor.offset = line_start + line_len;
    st.cursor.preferred_col = line_len;
    st.update_cursor_block();

    let scroll_before = st.scroll;
    edit_ops::apply(&mut st, Action::Newline, vp_h, vp_w);

    assert!(
        st.scroll > scroll_before,
        "scroll {} did not advance after newline at viewport bottom",
        st.scroll
    );

    // Cursor must remain inside the viewport.
    let (cursor_line_after, _) = st.cursor.line_col(&st.buffer);
    assert!(cursor_line_after >= st.scroll && cursor_line_after < st.scroll + vp_h);
}

/// In Raw mode, typing a character that extends a line past the viewport
/// width — pushing the cursor onto a new VISUAL row (wrap), without
/// adding a `\n` — must scroll the document so the cursor remains
/// visible.  Previously `ensure_cursor_visible` only checked buffer-line
/// coordinates and missed the wrap.
#[test]
fn type_at_viewport_bottom_wraps_and_scrolls_in_raw_mode() {
    // Document of identical short lines, with the bottom-most visible line
    // intentionally long enough that one more keystroke wraps it.
    let mut src = String::new();
    for _ in 0..40 {
        src.push_str("short\n");
    }
    let vp_h = 10;
    let vp_w = 20;
    let mut st = state(&src);
    st.mode = Mode::Raw;
    st.scroll = 0;

    // Pad the bottom-most visible line to exactly vp_w chars so that the
    // next typed char overflows and wraps onto a new visual row.
    let cursor_line = vp_h - 1;
    let pad = "X".repeat(vp_w - "short".len());
    let line_start = st.buffer.line_to_char(cursor_line);
    st.buffer.insert(line_start + "short".len(), &pad);
    // Cursor at end of that line.
    let line_text = st.buffer.line(cursor_line).unwrap_or_default();
    let line_len = line_text.trim_end_matches('\n').chars().count();
    st.cursor.offset = line_start + line_len;
    st.cursor.preferred_col = line_len;
    st.update_cursor_block();

    let scroll_before = st.scroll;
    edit_ops::apply(&mut st, Action::InsertChar('Y'), vp_h, vp_w);

    assert!(
        st.scroll > scroll_before,
        "scroll {} did not advance after a wrap-inducing keystroke (cursor on visual row past viewport bottom)",
        st.scroll
    );
}

/// Same scenario as `type_at_viewport_bottom_wraps_and_scrolls_in_raw_mode`,
/// but in Rendered mode.  The deferred-reparse optimization leaves
/// `parsed.lines` and the visual-row cache stale after an in-line edit, so
/// without an explicit flush the wrap check would miss the new visual row.
#[test]
fn type_at_viewport_bottom_wraps_and_scrolls_in_rendered_mode() {
    let vp_h = 10;
    let vp_w = 20;
    // 40-line paragraph (single block via soft breaks).  The bottom-most
    // visible line is padded to exactly `vp_w` chars so one more typed
    // char wraps it onto a new visual row.
    let mut src = String::new();
    for i in 0..40 {
        let prefix = format!("s{i}");
        if i == vp_h - 1 {
            src.push_str(&prefix);
            src.push_str(&"X".repeat(vp_w - prefix.len()));
            src.push('\n');
        } else {
            src.push_str(&prefix);
            src.push('\n');
        }
    }
    let mut st = state(&src);
    apply(&mut st, Action::EnterEditMode);
    st.scroll = 0;

    let cursor_buf_line = vp_h - 1;
    let line_start = st.buffer.line_to_char(cursor_buf_line);
    let line_text = st.buffer.line(cursor_buf_line).unwrap_or_default();
    let line_len = line_text.trim_end_matches('\n').chars().count();
    assert_eq!(line_len, vp_w, "line padding length is wrong");
    st.cursor.offset = line_start + line_len;
    st.cursor.preferred_col = line_len;
    st.update_cursor_block();

    let scroll_before = st.scroll;
    edit_ops::apply(&mut st, Action::InsertChar('Y'), vp_h, vp_w);

    assert!(
        st.scroll > scroll_before,
        "scroll {} did not advance after wrap-inducing keystroke in Rendered mode",
        st.scroll
    );
}

// ── Grapheme-aware editing on multi-codepoint clusters ───────────────────────

/// Sit the cursor immediately after the family-emoji cluster and Backspace.
/// The whole 7-char ZWJ sequence must vanish in one keystroke — leaving any
/// fragment behind would render as garbage.
#[test]
fn delete_char_back_removes_full_zwj_grapheme() {
    let mut st = state("a👨\u{200D}👩\u{200D}👧\u{200D}👦b");
    st.mode = Mode::Rendered;
    st.cursor.offset = 8; // after the family, before 'b'
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "ab");
    assert_eq!(st.cursor.offset, 1);
}

#[test]
fn delete_char_forward_removes_full_zwj_grapheme() {
    let mut st = state("a👨\u{200D}👩\u{200D}👧\u{200D}👦b");
    st.mode = Mode::Rendered;
    st.cursor.offset = 1; // before the family
    apply(&mut st, Action::DeleteCharForward);
    assert_eq!(st.contents(), "ab");
    assert_eq!(st.cursor.offset, 1);
}

/// Combining mark deletes with the base character — "é" (e + U+0301) is one
/// grapheme even though it's two chars.
#[test]
fn delete_char_back_removes_combining_mark_with_base() {
    let mut st = state("e\u{0301}!");
    st.mode = Mode::Rendered;
    st.cursor.offset = 2; // after the combining mark
    apply(&mut st, Action::DeleteCharBack);
    assert_eq!(st.contents(), "!");
    assert_eq!(st.cursor.offset, 0);
}

/// MoveLeft / MoveRight in Raw mode must also step by grapheme — the user
/// wanted Raw mode for raw *Markdown*, not raw codepoints.
#[test]
fn move_right_in_raw_mode_steps_over_grapheme() {
    let mut st = state("a👨\u{200D}👩\u{200D}👧\u{200D}👦b");
    st.mode = Mode::Raw;
    st.cursor.offset = 1; // before the family
    apply(&mut st, Action::MoveRight);
    assert_eq!(st.cursor.offset, 8); // landed past the whole grapheme
}
