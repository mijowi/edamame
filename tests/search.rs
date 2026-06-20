//! Search-flow integration tests at the editor / UI layer: flow entry
//! and exit on `EditorState`, the hint-line row, and match-highlight
//! painting in the rendered and raw views.
//!
//! App-level behavior (action gating, replace, replace-all, the
//! deferred advance) is covered by the unit tests in
//! `src/app/search.rs` and `src/app/modal/search_replace.rs`.

use edamame::config::{KeyBindingOverrides, KeyMap, Theme};
use edamame::document::Buffer;
use edamame::editor::{EditorState, Mode};
use edamame::search::SearchState;
use edamame::ui::bottom_region::hint_line_for;
use edamame::ui::{EditorView, EditorViewState};
use ratatui::{backend::TestBackend, Terminal};

fn theme() -> &'static Theme {
    Box::leak(Box::new(Theme::default()))
}

fn state_with_search(text: &str, query: &str, replace: Option<&str>) -> EditorState {
    let mut st = EditorState::new(Buffer::from_str(text), theme());
    let search = SearchState::new(query.to_owned(), replace.map(str::to_owned), st.scroll)
        .expect("valid query");
    st.enter_search(search);
    st
}

fn keymap() -> KeyMap {
    KeyMap::build(&KeyBindingOverrides::default()).unwrap()
}

// ── Smartcase (base feature) ──────────────────────────────────────────

#[test]
fn smartcase_lowercase_query_matches_every_case() {
    // A lowercase pattern is case-insensitive — every variant matches.
    let st = state_with_search("Foo foo FOO\n", "foo", None);
    assert_eq!(st.search.as_ref().unwrap().matches.len(), 3);
}

#[test]
fn smartcase_uppercase_query_is_case_sensitive() {
    // Any uppercase letter flips the search to case-sensitive.
    let st = state_with_search("Foo foo FOO\n", "Foo", None);
    let matches = &st.search.as_ref().unwrap().matches;
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0], 0..3);
}

// ── EditorState flow lifecycle ────────────────────────────────────────

#[test]
fn enter_search_populates_matches_and_keeps_mode() {
    let mut st = EditorState::new(Buffer::from_str("foo bar foo\n"), theme());
    st.mode = Mode::Rendered;
    let search = SearchState::new("foo".to_owned(), None, st.scroll).unwrap();
    st.enter_search(search);
    assert_eq!(st.mode, Mode::Rendered, "search must not change the mode");
    assert_eq!(st.search.as_ref().unwrap().matches.len(), 2);
    assert!(st.pending_focus_scroll, "scroll-to-match is deferred");
}

#[test]
fn exit_search_restores_scroll_and_drops_session() {
    let mut st = state_with_search(&"line\n".repeat(50), "line", None);
    st.search.as_mut().unwrap().pre_search_scroll = 9;
    st.scroll = 30;
    st.exit_search();
    assert!(st.search.is_none());
    assert_eq!(st.scroll, 9);
}

#[test]
fn search_suppresses_the_cursor_block_raw_reveal() {
    let mut st = state_with_search("foo bar\n", "foo", None);
    st.mode = Mode::Rendered;
    // No reveal timer armed → normally revealed; the active flow must
    // override that.
    assert!(!st.cursor_block_revealed());
    st.exit_search();
    assert!(st.cursor_block_revealed());
}

#[test]
fn replace_buffer_drops_an_active_flow() {
    let mut st = state_with_search("foo bar\n", "foo", None);
    st.replace_buffer(Buffer::from_str("entirely new\n"));
    assert!(st.search.is_none());
}

// ── Hint line ─────────────────────────────────────────────────────────

#[test]
fn navigate_only_flow_hints_omit_replace_chords() {
    let st = state_with_search("foo bar foo\n", "foo", None);
    let set = hint_line_for(&st, &keymap());
    let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["Next", "Prev", "Exit"]);
    assert_eq!(set.chords[0].chord, "Tab");
    assert_eq!(set.chords[1].chord, "⇧Tab");
    assert_eq!(set.chords.last().unwrap().chord, "Esc");
}

#[test]
fn replace_flow_hints_add_replace_and_replace_all() {
    let st = state_with_search("foo bar foo\n", "foo", Some("baz"));
    let set = hint_line_for(&st, &keymap());
    let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["Next", "Prev", "Replace", "Replace all", "Undo", "Exit"]
    );
    let replace = &set.chords[2];
    assert_eq!(replace.chord, "r");
    let all = &set.chords[3];
    assert_eq!(all.chord, "a");
}

// ── Highlight painting ────────────────────────────────────────────────

/// Render `state` through the full `EditorView` and return the terminal
/// buffer for cell-style assertions.
fn render_editor(state: &mut EditorState, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = EditorViewState::new();
    let caps = edamame::terminal::Capabilities::default();
    // Build the hint from the live state, exactly as the app does, so
    // the search match counter that now leads the hint line is present.
    let hint = edamame::ui::bottom_region::HintContent::Chords(hint_line_for(state, &keymap()));
    terminal
        .draw(|frame| {
            let view = EditorView {
                state,
                theme: theme(),
                filename: "test.md",
                show_table_buttons: false,
                table_drop_indicator: None,
                capabilities: &caps,
                show_line_numbers: false,
                is_scrolling: false,
                hint,
                vim_mode_label: None,
                visual_line_mode: false,
                max_width_enabled: false,
                max_width_cols: 0,
                scrollbar_active: false,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn rendered_view_paints_all_matches_with_focused_emphasis() {
    let t = theme();
    let mut st = state_with_search("foo bar foo\n", "foo", None);
    st.mode = Mode::Rendered;
    let buf = render_editor(&mut st, 40, 8);
    // First match (focused) at cols 0..3, second at cols 8..11 of row 0.
    for x in 0..3u16 {
        let cell = buf.cell((x, 0)).unwrap();
        assert_eq!(
            cell.style().bg,
            t.selection.bg,
            "focused match col {x} must carry the emphasized bg"
        );
    }
    for x in 8..11u16 {
        let cell = buf.cell((x, 0)).unwrap();
        assert_eq!(
            cell.style().bg,
            t.selection_muted.bg,
            "non-focused match col {x} must carry the standard bg"
        );
    }
    // The gap between matches keeps the document background.
    let gap = buf.cell((5, 0)).unwrap();
    assert_ne!(gap.style().bg, t.selection_muted.bg);
}

#[test]
fn preview_mode_paints_matches_too() {
    let t = theme();
    let mut st = state_with_search("foo bar foo\n", "foo", None);
    assert_eq!(st.mode, Mode::Preview);
    let buf = render_editor(&mut st, 40, 8);
    let cell = buf.cell((0, 0)).unwrap();
    assert_eq!(cell.style().bg, t.selection.bg);
}

#[test]
fn raw_view_paints_matches_per_line() {
    let t = theme();
    let mut st = state_with_search("foo bar\nbaz foo\n", "foo", None);
    st.mode = Mode::Raw;
    let buf = render_editor(&mut st, 40, 8);
    // Focused match on line 0 cols 0..3.  Col 0 carries the block
    // cursor (cursor wins over the highlight per cell), so assert the
    // remaining match columns.
    for x in 1..3u16 {
        let cell = buf.cell((x, 0)).unwrap();
        assert_eq!(cell.style().bg, t.selection.bg);
    }
    // Second match on line 1 cols 4..7.
    let cell = buf.cell((4, 1)).unwrap();
    assert_eq!(cell.style().bg, t.selection_muted.bg);
}

#[test]
fn multibyte_text_before_a_match_keeps_columns_aligned() {
    let t = theme();
    // "naïve " is 6 chars (7 bytes); the match starts at char col 6.
    let mut st = state_with_search("naïve foo\n", "foo", None);
    st.mode = Mode::Raw;
    let buf = render_editor(&mut st, 40, 8);
    for x in 6..9u16 {
        let cell = buf.cell((x, 0)).unwrap();
        assert_eq!(
            cell.style().bg,
            t.selection.bg,
            "match col {x} must be highlighted despite multi-byte prefix"
        );
    }
    let before = buf.cell((5, 0)).unwrap();
    assert_ne!(before.style().bg, t.selection.bg);
}

#[test]
fn hint_counter_walks_with_focus() {
    let mut st = state_with_search("x y x y x\n", "x", None);
    st.mode = Mode::Rendered;
    st.search.as_mut().unwrap().advance_focus();
    let buf = render_editor(&mut st, 40, 8);
    // The match counter now leads the hint line — the row directly
    // above the status bar (rows 6 = hint, 7 = status).
    let mut hint = String::new();
    for x in 0..40u16 {
        hint.push(
            buf.cell((x, 6))
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .unwrap_or(' '),
        );
    }
    assert!(hint.contains("2/3"), "hint line must show 2/3, got: {hint}");
}

#[test]
fn heading_match_highlight_aligns_with_prefixed_text() {
    let t = theme();
    // H1 renders as a one-space prefix + inline content, so "foo" in
    // "# foo bar" sits at rendered cols 1..4.
    let mut st = state_with_search("# foo bar\n", "foo", None);
    st.mode = Mode::Rendered;
    let buf = render_editor(&mut st, 40, 8);
    for x in 1..4u16 {
        let cell = buf.cell((x, 0)).unwrap();
        assert_eq!(
            cell.style().bg,
            t.selection.bg,
            "heading match col {x} must be highlighted"
        );
    }
    // The char after the match stays unhighlighted.  (Col 0 isn't
    // probed: the pre-reveal cursor indicator sits there and shares
    // the highlight's `primary` background.)
    assert_ne!(buf.cell((4, 0)).unwrap().style().bg, t.selection.bg);
}

#[test]
fn code_block_match_highlight_aligns_with_padded_text() {
    let t = theme();
    // Fenced code body rows render the raw text behind one leading pad
    // cell: row 0 is the fence/lang row, row 1 carries " foo bar", so
    // the match sits at rendered cols 1..4 of row 1.
    let mut st = state_with_search("```\nfoo bar\n```\n", "foo", None);
    st.mode = Mode::Rendered;
    // Match the renderer's pad width to the test viewport so the
    // padded fence/code rows occupy exactly one visual row each.
    st.set_viewport_width(40);
    let buf = render_editor(&mut st, 40, 8);
    for x in 1..4u16 {
        let cell = buf.cell((x, 1)).unwrap();
        assert_eq!(
            cell.style().bg,
            t.selection.bg,
            "code match col {x} must be highlighted"
        );
    }
    assert_ne!(buf.cell((0, 1)).unwrap().style().bg, t.selection.bg);
    assert_ne!(buf.cell((4, 1)).unwrap().style().bg, t.selection.bg);
}

#[test]
fn replace_flow_hints_include_undo_before_exit() {
    let st = state_with_search("foo bar foo\n", "foo", Some("baz"));
    let set = hint_line_for(&st, &keymap());
    let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["Next", "Prev", "Replace", "Replace all", "Undo", "Exit"],
        "Undo rides the replace-flow row; Redo only once can_redo()"
    );
}

#[test]
fn navigate_only_flow_hints_omit_undo() {
    let st = state_with_search("foo bar foo\n", "foo", None);
    let set = hint_line_for(&st, &keymap());
    assert!(!set.chords.iter().any(|c| c.label == "Undo"));
}

#[test]
fn preview_and_edit_hint_rows_offer_find_after_go_to() {
    let mut st = EditorState::new(Buffer::from_str("hello\n"), theme());
    for mode in [Mode::Preview, Mode::Rendered, Mode::Raw] {
        st.mode = mode;
        let labels: Vec<String> = hint_line_for(&st, &keymap())
            .chords
            .iter()
            .map(|c| c.label.clone())
            .collect();
        let go_to = labels.iter().position(|l| l == "Go to");
        let find = labels.iter().position(|l| l == "Find");
        assert!(find.is_some(), "{mode:?} row must offer Find: {labels:?}");
        assert_eq!(
            find,
            go_to.map(|i| i + 1),
            "{mode:?}: Find must directly follow Go to: {labels:?}"
        );
    }
}

#[test]
fn highlights_stay_aligned_in_documents_with_blank_lines() {
    // Regression: `source_map.block_for_byte` counts blank-line
    // virtual blocks, so its index diverges from `parsed.blocks` by
    // one per preceding blank line.  The block-kind lookup that drives
    // the heading / code-block prefix shift must resolve through
    // `real_block_for_byte`, or every block after the first blank line
    // paints off by its prefix.
    let t = theme();
    let doc = "intro paragraph\n\n# foo bar\n\n```\nfoo bar\n```\n";
    let mut st = state_with_search(doc, "foo", None);
    st.mode = Mode::Rendered;
    st.set_viewport_width(40);
    let buf = render_editor(&mut st, 40, 12);
    // Rows: 0 intro, 1 blank, 2 heading, 3 H1 rule, 4 blank,
    // 5 code fence pad, 6 code body, 7 closing pad.
    // Heading match (focused): prefix-shifted to cols 1..4 of row 2.
    for x in 1..4u16 {
        let cell = buf.cell((x, 2)).unwrap();
        assert_eq!(
            cell.style().bg,
            t.selection.bg,
            "heading match col {x} must align after blank lines"
        );
    }
    assert_ne!(buf.cell((4, 2)).unwrap().style().bg, t.selection.bg);
    // Code-block match: pad-shifted to cols 1..4 of row 6.
    for x in 1..4u16 {
        let cell = buf.cell((x, 6)).unwrap();
        assert_eq!(
            cell.style().bg,
            t.selection_muted.bg,
            "code match col {x} must align after blank lines"
        );
    }
    assert_ne!(buf.cell((4, 6)).unwrap().style().bg, t.selection_muted.bg);
}

#[test]
fn focused_match_uses_selection_and_others_the_muted_variant() {
    let t = theme();
    assert_ne!(
        t.selection.bg, t.selection_muted.bg,
        "muted selection must be visually distinct from selection"
    );
    let mut st = state_with_search("foo bar foo\n", "foo", None);
    st.mode = Mode::Rendered;
    let buf = render_editor(&mut st, 40, 8);
    assert_eq!(buf.cell((1, 0)).unwrap().style().bg, t.selection.bg);
    assert_eq!(buf.cell((8, 0)).unwrap().style().bg, t.selection_muted.bg);
}
