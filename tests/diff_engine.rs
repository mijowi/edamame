//! Integration tests for the diff engine.  Mirrors the snapshot
//! corpus called out in `docs/diff-mode-plan.md` §13: a handful of
//! old/new pairs covering pure insert, pure delete, replace,
//! multi-hunk, and inline-word-only changes.

use edamame::diff::engine::{compute_hunks, HunkIdAllocator};
use edamame::diff::hunk::{HunkKind, InlineSide};

fn run(old: &str, new: &str) -> Vec<edamame::diff::Hunk> {
    let mut ids = HunkIdAllocator::new();
    compute_hunks(old, new, &mut ids)
}

#[test]
fn identical_text_yields_no_hunks() {
    assert!(run("alpha\nbeta\ngamma\n", "alpha\nbeta\ngamma\n").is_empty());
}

#[test]
fn pure_insert_at_end_of_file() {
    let hunks = run("a\nb\n", "a\nb\nc\n");
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].kind, HunkKind::Insert);
    assert_eq!(hunks[0].old_lines, 2..2);
    assert_eq!(hunks[0].new_lines, 2..3);
}

#[test]
fn pure_delete_in_middle_of_file() {
    let hunks = run("a\nb\nc\n", "a\nc\n");
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].kind, HunkKind::Delete);
    assert_eq!(hunks[0].old_lines, 1..2);
    assert_eq!(hunks[0].new_lines.start, 1);
    assert_eq!(hunks[0].new_lines.end, 1);
}

#[test]
fn replace_emits_inline_word_spans_on_paired_lines() {
    let hunks = run("alpha bravo charlie\n", "alpha bravo DELTA\n");
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].kind, HunkKind::Replace);
    let inline = &hunks[0].inline;
    assert!(inline.iter().any(|s| s.side == InlineSide::Old));
    assert!(inline.iter().any(|s| s.side == InlineSide::New));
}

#[test]
fn multi_hunk_keeps_order_and_unique_ids() {
    let old = "a\nb\nc\nd\ne\n";
    let new = "A\nb\nC\nd\nE\n";
    let hunks = run(old, new);
    assert!(hunks.len() >= 3, "expected several hunks, got {hunks:?}");
    let mut ids: Vec<_> = hunks.iter().map(|h| h.id).collect();
    ids.sort_by_key(|id| id.0);
    ids.dedup();
    assert_eq!(ids.len(), hunks.len(), "ids must be unique");
}

#[test]
fn empty_old_is_one_big_insert() {
    let hunks = run("", "first\nsecond\n");
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].kind, HunkKind::Insert);
    assert_eq!(hunks[0].new_lines.start, 0);
    assert_eq!(hunks[0].new_lines.end, 2);
}

#[test]
fn empty_new_is_one_big_delete() {
    let hunks = run("first\nsecond\n", "");
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].kind, HunkKind::Delete);
}

/// Pin the exact ropey behavior the half-open `[start, end)`
/// line-range convention in §3a rides on.
///
/// For both trailing-newline and no-trailing-newline cases,
/// `byte_to_line(len_bytes())` returns `len_lines() - 1`.  The two
/// cases differ in *what* that index points at:
///
/// - **With trailing `\n`** (`"a\nb\n"`): `len_lines() == 3` (the
///   trailing empty line counts), and `byte_to_line(4) == 2` points
///   at the empty line.  A block-extent end at the file's last byte
///   then yields `end_line = 2`, which iterating as `start..2`
///   correctly covers lines 0 and 1 (the content lines) and excludes
///   the trailing empty line.
/// - **Without trailing `\n`** (`"a\nb"`): `len_lines() == 2`, and
///   `byte_to_line(3) == 1` points at the last content line.  A
///   block-extent end at the file's last byte then yields
///   `end_line = 1`, which iterating as `start..1` covers only line
///   0 and **misses line 1**.  This is a known limitation; pin it
///   here so a future ropey upgrade can't shift it silently, and so
///   we remember to revisit if we ever support files without a
///   trailing newline as first-class inputs.
#[test]
fn ropey_line_range_invariants() {
    use ropey::Rope;

    let with_nl = Rope::from_str("a\nb\n");
    assert_eq!(with_nl.len_lines(), 3);
    assert_eq!(with_nl.byte_to_line(with_nl.len_bytes()), 2);

    let without_nl = Rope::from_str("a\nb");
    assert_eq!(without_nl.len_lines(), 2);
    assert_eq!(without_nl.byte_to_line(without_nl.len_bytes()), 1);
}
