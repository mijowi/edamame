//! Integration tests for the diff engine.  Mirrors the snapshot
//! corpus called out in `docs/diff-mode-plan.md` §13: a handful of
//! old/new pairs covering pure insert, pure delete, replace,
//! multi-hunk, and inline-word-only changes.

use edamame::diff::engine::{compute_hunks, HunkIdAllocator};
use edamame::diff::hunk::{HunkKind, InlineSide};
use edamame::diff::{Decision, DiffState};

fn run(old: &str, new: &str) -> Vec<edamame::diff::Hunk> {
    let mut ids = HunkIdAllocator::new();
    compute_hunks(old, new, &mut ids)
}

/// Does any hunk reference `old_line` on its old side or `new_line`
/// on its new side?  Used to assert a changed line was surfaced as a
/// reviewable hunk rather than silently dropped.
fn any_hunk_covers(hunks: &[edamame::diff::Hunk], old_line: usize, new_line: usize) -> bool {
    hunks
        .iter()
        .any(|h| h.old_lines.contains(&old_line) || h.new_lines.contains(&new_line))
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

// ─── Table row-level sub-diff (§3a) ──────────────────────────────────────────

const TABLE_OLD: &str = "\
intro paragraph\n\
\n\
| Name  | Score |\n\
|-------|-------|\n\
| alpha | 1     |\n\
| bravo | 2     |\n\
| gamma | 3     |\n";

/// A change confined to a single table data row is split into a
/// per-row hunk (§3a), not surfaced as one monolithic table replace.
#[test]
fn changed_table_row_splits_into_per_row_hunk() {
    // Only the `bravo` row's score changes (2 → 9).
    let new = TABLE_OLD.replace("| bravo | 2     |", "| bravo | 9     |");
    let hunks = run(TABLE_OLD, &new);
    assert_eq!(hunks.len(), 1, "one changed row → one hunk: {hunks:?}");
    assert_eq!(hunks[0].kind, HunkKind::Replace);
    // The `bravo` row is line index 5 (0: intro, 1: blank, 2: header,
    // 3: separator, 4: alpha, 5: bravo).
    assert_eq!(hunks[0].old_lines, 5..6);
    assert_eq!(hunks[0].new_lines, 5..6);
}

/// Two non-adjacent changed rows yield two independent per-row hunks
/// so the user can accept one and reject the other.
#[test]
fn two_changed_table_rows_yield_two_hunks() {
    let new = TABLE_OLD
        .replace("| alpha | 1     |", "| alpha | 7     |")
        .replace("| gamma | 3     |", "| gamma | 8     |");
    let hunks = run(TABLE_OLD, &new);
    assert_eq!(hunks.len(), 2, "two non-adjacent rows → two hunks: {hunks:?}");
    assert!(hunks.iter().all(|h| h.kind == HunkKind::Replace));
    assert_eq!(hunks[0].old_lines, 4..5); // alpha row
    assert_eq!(hunks[1].old_lines, 6..7); // gamma row
}

/// When the table's cell counts aren't uniform across rows/sides, the
/// row-uniformity guard trips and the engine falls back to a single
/// monolithic hunk rather than mis-splitting (§3a).
#[test]
fn non_uniform_table_falls_back_to_monolithic_hunk() {
    // New side adds a third column to the header only, so column
    // counts differ across rows — the uniformity guard must bail.
    let old = "| A | B |\n|---|---|\n| 1 | 2 |\n";
    let new = "| A | B | C |\n|---|---|\n| 1 | 2 |\n";
    let hunks = run(old, new);
    assert_eq!(hunks.len(), 1, "non-uniform table must not row-split: {hunks:?}");
    assert_eq!(hunks[0].kind, HunkKind::Replace);
    // The whole header line is the single reviewable hunk.
    assert!(any_hunk_covers(&hunks, 0, 0));
}

/// Regression: a single diff hunk that straddles the table boundary
/// (a non-table line *and* a table row both change, contiguously)
/// must NOT be row-split — doing so would drop the non-table line
/// from every hunk and silently apply it regardless of decision.
/// See `find_extent`'s containment requirement.
#[test]
fn hunk_straddling_table_boundary_keeps_non_table_line() {
    // `intro` and the table header both change, with no blank line
    // between them, so `similar` coalesces them into one hunk.
    let old = "intro text\n| A | B |\n|---|---|\n| 1 | 2 |\n";
    let new = "intro CHANGED\n| A | C |\n|---|---|\n| 1 | 2 |\n";
    let hunks = run(old, new);
    assert!(
        any_hunk_covers(&hunks, 0, 0),
        "the intro-paragraph change must remain reviewable, got {hunks:?}",
    );
}

/// End-to-end guard on the same straddling case: rejecting every hunk
/// must reproduce the original text exactly.  Before the containment
/// fix this returned the new intro line even on full rejection.
#[test]
fn straddling_boundary_reject_all_round_trips_to_old() {
    let old = "intro text\n| A | B |\n|---|---|\n| 1 | 2 |\n";
    let new = "intro CHANGED\n| A | C |\n|---|---|\n| 1 | 2 |\n";
    let mut state = DiffState::new(old, new).expect("differing inputs");
    state.bulk_decide_pending(Decision::Rejected);
    assert_eq!(
        state.resolved_rope().expect("all resolved").to_string(),
        old,
        "reject-all must reproduce the original text",
    );

    // And accept-all must reproduce the new text.
    let mut state = DiffState::new(old, new).expect("differing inputs");
    state.bulk_decide_pending(Decision::Accepted);
    assert_eq!(
        state.resolved_rope().expect("all resolved").to_string(),
        new,
        "accept-all must reproduce the new text",
    );
}
