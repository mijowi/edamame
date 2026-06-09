//! Integration tests for the footnote feature.
//!
//! Covers the library-facing `footnote_edit` API, the `EditorState`-level
//! `edit_ops` wrappers, and the two-layer mouse path (`MouseAction` →
//! `mouse_ops::apply` → `pending_link_follow`) that turns a click on a
//! rendered footnote reference into a follow — the layer CLAUDE.md asks be
//! tested as a pure function of input event + editor state.

use crossterm::event::KeyModifiers;
use edamame::config::Theme;
use edamame::document::Buffer;
use edamame::editor::link::LinkTarget;
use edamame::editor::{edit_ops, footnote_edit, mouse_ops, EditorState, Mode};
use edamame::input::MouseAction;

const VP: usize = 40;
const VW: usize = 80;

fn theme() -> &'static Theme {
    Box::leak(Box::new(Theme::default()))
}

fn state(text: &str) -> EditorState {
    EditorState::new(Buffer::from_str(text), theme())
}

// ── Library API ───────────────────────────────────────────────────────────────

#[test]
fn footnote_edit_api_is_reachable_and_correct() {
    let src = "A[^2] B[^1]\n\n[^2]: two\n[^1]: one\n";
    assert_eq!(footnote_edit::next_footnote_number(src), 3);
    // `2` is referenced before `1`, so renumber produces a (non-empty)
    // delta; already-sequential input produces none.
    assert!(footnote_edit::renumber_footnotes(src).is_some());
    assert!(footnote_edit::renumber_footnotes("A[^1] B[^2]\n\n[^1]: x\n[^2]: y\n").is_none());
}

#[test]
fn renumber_wrapper_reorders_buffer_by_first_reference() {
    let mut st = state("A[^2] B[^1]\n\n[^2]: two\n[^1]: one\n");
    st.mode = Mode::Rendered;
    assert!(edit_ops::renumber_footnotes(&mut st, VP, VW));
    assert_eq!(st.contents(), "A[^1] B[^2]\n\n[^1]: two\n[^2]: one\n");
}

// ── EditorState wrappers ────────────────────────────────────────────────────────

#[test]
fn insert_footnote_wrapper_auto_numbers() {
    let mut st = state("Claim.\n\n[^1]: prior note.\n");
    st.mode = Mode::Rendered;
    st.cursor.offset = st.buffer.rope().byte_to_char("Claim.".len());
    edit_ops::insert_footnote_at_cursor(&mut st, VP, VW);
    assert!(
        st.contents().starts_with("Claim.[^2]"),
        "got: {:?}",
        st.contents()
    );
}

#[test]
fn delete_footnote_wrapper_off_a_footnote_returns_false() {
    let mut st = state("plain text\n");
    st.mode = Mode::Rendered;
    st.cursor.offset = 3;
    assert!(!edit_ops::delete_footnote_at_cursor(&mut st, VP, VW));
}

// ── Two-layer mouse path ────────────────────────────────────────────────────────

#[test]
fn mouse_click_on_reference_stages_footnote_follow() {
    // Preview mode: a plain click on the rendered superscript marker stages
    // a footnote follow.  Rendered line 0 is "Body¹ more." — the superscript
    // sits at column 4 (after "Body"), and the 1:1 raw-column map sends it
    // onto the `[^1]` source.
    let mut st = state("Body[^1] more.\n\n[^1]: the note.\n");
    assert_eq!(st.mode, Mode::Preview);
    let action = MouseAction::Click {
        col: 4,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    let mut drag = None;
    mouse_ops::apply(&mut st, action, &mut drag, &[], VP, VW);
    assert_eq!(
        st.pending_link_follow,
        Some(LinkTarget::Footnote("1".into())),
        "click on the superscript should stage a footnote follow"
    );
}

#[test]
fn hover_over_reference_marker_is_clickable() {
    // The pointer-shape hit-test should report the superscript marker as
    // clickable (hand cursor), but not the surrounding text.
    let st = state("Body[^1] more.\n\n[^1]: the note.\n");
    // Rendered line 0: "Body¹ more." — superscript at col 4.
    assert!(
        mouse_ops::hit_test_clickable(&st, 4, 0, VW, &[]),
        "the footnote superscript should be clickable"
    );
    assert!(
        !mouse_ops::hit_test_clickable(&st, 1, 0, VW, &[]),
        "plain text should not be clickable"
    );
}

#[test]
fn hover_over_definition_back_link_is_clickable() {
    // The definition leader (`↩ N.`) maps onto the `[^1]:` source and
    // should be clickable; the definition's body text should not.
    let st = state("Body[^1] more.\n\n[^1]: the note.\n");
    // Find the rendered row of the definition line.
    let def_row = st
        .parsed
        .lines
        .iter()
        .position(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .contains("the note.")
        })
        .expect("definition line") as u16;
    // Column 0 is the leading space of the `  1.  ` leader, which is
    // column-matched to the raw `[^1]:` source, so it resolves as the
    // back-link.
    assert!(
        mouse_ops::hit_test_clickable(&st, 0, def_row, VW, &[]),
        "the definition leader should be clickable"
    );
    // The trailing `↩` glyph (last cell of the line) is also clickable.
    let def_line: String = st.parsed.lines[def_row as usize]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    let glyph_col = (def_line.chars().count() - 1) as u16;
    assert!(
        def_line.ends_with('↩'),
        "definition should end with the back-link glyph: {def_line:?}"
    );
    assert!(
        mouse_ops::hit_test_clickable(&st, glyph_col, def_row, VW, &[]),
        "the trailing back-link glyph should be clickable"
    );
}

fn row_containing(st: &EditorState, needle: &str) -> u16 {
    st.parsed
        .lines
        .iter()
        .position(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .contains(needle)
        })
        .expect("line present") as u16
}

#[test]
fn rendered_mode_plain_click_on_reference_follows() {
    // A plain (non-Ctrl) click on the superscript in Rendered mode must
    // follow the footnote, just like Preview.  The reference is on a line
    // away from the cursor so it renders normally (not revealed as raw).
    let mut st = state("First line.\n\nBody[^1] more.\n\n[^1]: the note.\n");
    st.mode = Mode::Rendered;
    st.cursor.offset = 0;
    let row = row_containing(&st, "Body"); // "Body¹ more." — superscript at col 4
    let action = MouseAction::Click {
        col: 4,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let mut drag = None;
    mouse_ops::apply(&mut st, action, &mut drag, &[], VP, VW);
    assert_eq!(
        st.pending_link_follow,
        Some(LinkTarget::Footnote("1".into())),
        "plain click on the marker should follow in Rendered mode"
    );
}

#[test]
fn rendered_mode_plain_click_on_back_link_follows() {
    let mut st = state("First line.\n\nBody[^1] more.\n\n[^1]: the note.\n");
    st.mode = Mode::Rendered;
    st.cursor.offset = 0;
    let row = row_containing(&st, "the note."); // "  1.  the note. ↩"
                                                // Column 0 is the leader, column-matched to the raw `[^1]:` source.
    let action = MouseAction::Click {
        col: 0,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let mut drag = None;
    mouse_ops::apply(&mut st, action, &mut drag, &[], VP, VW);
    assert_eq!(
        st.pending_link_follow,
        Some(LinkTarget::FootnoteBack("1".into())),
        "plain click on the leader should follow in Rendered mode"
    );
}

#[test]
fn rendered_mode_plain_click_on_trailing_glyph_follows() {
    // The visible `↩` affordance at the end of the definition follows the
    // back-link even though it has no raw source byte.
    let mut st = state("First line.\n\nBody[^1] more.\n\n[^1]: the note.\n");
    st.mode = Mode::Rendered;
    st.cursor.offset = 0;
    let row = row_containing(&st, "the note.");
    let def_line: String = st.parsed.lines[row as usize]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(def_line.ends_with('↩'), "got: {def_line:?}");
    let glyph_col = (def_line.chars().count() - 1) as u16;
    let action = MouseAction::Click {
        col: glyph_col,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let mut drag = None;
    mouse_ops::apply(&mut st, action, &mut drag, &[], VP, VW);
    assert_eq!(
        st.pending_link_follow,
        Some(LinkTarget::FootnoteBack("1".into())),
        "plain click on the trailing ↩ glyph should follow"
    );
}

#[test]
fn rendered_mode_plain_click_on_text_places_cursor_not_follow() {
    // A plain click on ordinary text in Rendered mode must NOT follow — it
    // places the cursor as usual.
    let mut st = state("First line.\n\nBody[^1] more.\n\n[^1]: the note.\n");
    st.mode = Mode::Rendered;
    st.cursor.offset = 0;
    let row = row_containing(&st, "Body");
    let action = MouseAction::Click {
        col: 1, // on "o" of "Body"
        row,
        modifiers: KeyModifiers::NONE,
    };
    let mut drag = None;
    mouse_ops::apply(&mut st, action, &mut drag, &[], VP, VW);
    assert_eq!(st.pending_link_follow, None);
}

#[test]
fn mouse_click_on_plain_text_stages_nothing() {
    let mut st = state("Body[^1] more.\n\n[^1]: the note.\n");
    let action = MouseAction::Click {
        col: 1, // on "o" of "Body" — not the marker
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    let mut drag = None;
    mouse_ops::apply(&mut st, action, &mut drag, &[], VP, VW);
    assert_eq!(st.pending_link_follow, None);
}
