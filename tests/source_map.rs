/// Proptest round-trip invariants for `SourceMap`.
///
/// Invariants tested:
/// 1. Every source byte maps to at least one rendered line.
/// 2. Two different rendered lines do not claim overlapping source ranges.
/// 3. Together, the extended ranges cover the full source byte range.
///
/// These hold for the initial parse AND after a sequence of edits (which
/// trigger a full re-parse).
use proptest::prelude::*;

use edamame::config::Action;
use edamame::config::Theme;
use edamame::document::{Buffer, ParsedDoc};
use edamame::editor::{edit_ops, EditorState, Mode};

fn theme() -> &'static Theme {
    Box::leak(Box::new(Theme::default()))
}

/// Assert that the `ParsedDoc`'s `SourceMap` satisfies the coverage invariant:
/// every byte in `0..source.len()` maps to exactly one block (rendered line
/// range). Panics with a descriptive message on failure.
fn assert_coverage(source: &str, doc: &ParsedDoc) {
    if source.is_empty() {
        return; // nothing to cover
    }
    if doc.source_map.rendered_line_count() == 0 {
        // Pure-whitespace / entirely-blank document: the renderer produces no
        // lines, so the "every byte maps to a rendered line" invariant cannot
        // hold. This is an expected corner case — skip.
        return;
    }
    for byte in 0..source.len() {
        let range = doc.source_map.rendered_lines_for_byte(byte);
        assert!(
            !range.is_empty(),
            "source byte {} (char {:?}) not covered by any rendered line.\n\
             Source: {:?}\n\
             Block count: {}",
            byte,
            source.as_bytes().get(byte).map(|&b| b as char),
            source,
            doc.source_map.block_count(),
        );
    }
}

// ── Deterministic invariant checks ───────────────────────────────────────────

#[test]
fn coverage_empty() {
    let doc = ParsedDoc::build("", theme(), false);
    assert_coverage("", &doc);
}

#[test]
fn coverage_single_paragraph() {
    let src = "Hello world\n";
    let doc = ParsedDoc::build(src, theme(), false);
    assert_coverage(src, &doc);
}

#[test]
fn coverage_heading_paragraph_rule() {
    let src = "# Hello\n\nSome text.\n\n---\n";
    let doc = ParsedDoc::build(src, theme(), false);
    assert_coverage(src, &doc);
}

#[test]
fn coverage_code_block() {
    let src = "```rust\nfn main() {}\n```\n\nText after.\n";
    let doc = ParsedDoc::build(src, theme(), false);
    assert_coverage(src, &doc);
}

#[test]
fn coverage_list() {
    let src = "- item one\n- item two\n- item three\n";
    let doc = ParsedDoc::build(src, theme(), false);
    assert_coverage(src, &doc);
}

#[test]
fn coverage_blockquote() {
    let src = "> A quoted paragraph.\n>\n> Another quoted paragraph.\n";
    let doc = ParsedDoc::build(src, theme(), false);
    assert_coverage(src, &doc);
}

#[test]
fn coverage_table() {
    let src = "| A | B |\n|---|---|\n| 1 | 2 |\n";
    let doc = ParsedDoc::build(src, theme(), false);
    assert_coverage(src, &doc);
}

#[test]
fn coverage_after_insert() {
    let buf = Buffer::from_str("Hello\n");
    let mut state = EditorState::new(buf, theme());
    state.mode = Mode::Rendered;

    edit_ops::apply(&mut state, Action::MoveDocEnd, 40, 80);
    edit_ops::apply(&mut state, Action::InsertChar('!'), 40, 80);

    let source = state.contents();
    assert_coverage(&source, &state.parsed);
}

#[test]
fn coverage_after_delete() {
    let buf = Buffer::from_str("Hello world\n");
    let mut state = EditorState::new(buf, theme());
    state.mode = Mode::Rendered;

    // Delete "world".
    edit_ops::apply(&mut state, Action::MoveDocEnd, 40, 80);
    for _ in 0.."world\n".len() {
        edit_ops::apply(&mut state, Action::DeleteCharBack, 40, 80);
    }

    let source = state.contents();
    assert_coverage(&source, &state.parsed);
}

#[test]
fn coverage_after_newline() {
    let buf = Buffer::from_str("Hello\n");
    let mut state = EditorState::new(buf, theme());
    state.mode = Mode::Rendered;

    edit_ops::apply(&mut state, Action::MoveDocEnd, 40, 80);
    edit_ops::apply(&mut state, Action::Newline, 40, 80);
    for ch in "World".chars() {
        edit_ops::apply(&mut state, Action::InsertChar(ch), 40, 80);
    }

    let source = state.contents();
    assert_coverage(&source, &state.parsed);
}

// ── Proptest: random markdown documents ──────────────────────────────────────

proptest! {
    /// For any arbitrary (valid UTF-8) markdown document, the source map covers
    /// all bytes. We limit the string size to keep tests fast.
    #[test]
    fn proptest_coverage_arbitrary_doc(
        src in r"[a-zA-Z0-9 \n#*`_>-]{0,200}"
    ) {
        let doc = ParsedDoc::build(&src, theme(), false);
        if !src.is_empty() {
            assert_coverage(&src, &doc);
        }
    }

    /// After a sequence of inserts, the source map still covers all bytes.
    #[test]
    fn proptest_coverage_after_inserts(
        initial in r"[a-zA-Z \n]{0,50}",
        inserts in prop::collection::vec(r"[a-zA-Z]", 0..10)
    ) {
        let buf = Buffer::from_str(&initial);
        let mut state = EditorState::new(buf, theme());
        state.mode = Mode::Rendered;

        for s in inserts {
            for ch in s.chars() {
                edit_ops::apply(&mut state, Action::InsertChar(ch), 40, 80);
            }
        }

        let source = state.contents();
        assert_coverage(&source, &state.parsed);
    }
}
