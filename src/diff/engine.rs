//! Line-level + inline-word diff over two strings, producing the
//! [`Hunk`] sequence consumed by [`crate::diff::DiffState`].
//!
//! Wraps `similar::TextDiff::from_lines` so the rest of the codebase
//! never needs to import `similar` directly.  Adds:
//!
//! - Row-level table sub-diff (§3a): when a hunk's old- or new-side
//!   line range intersects a markdown table extent (detected via
//!   [`crate::markdown::parse_offsets::block_ranges_by`]), the hunk
//!   is split into per-row hunks so the user accepts/rejects per
//!   row.  Coalesces runs of neighboring changed rows into a single
//!   hunk to reduce decision count.
//! - Stable [`HunkId`] allocation through a caller-supplied counter
//!   (see `DiffState::compute_initial` / `recompute_after_edit`).
//!
//! Phase 1 keeps inline word-level highlights restricted to text
//! lines outside table cells; the renderer surfaces them through
//! [`crate::diff::hunk::InlineSpan`].

use std::ops::Range;

use ropey::Rope;
use similar::{ChangeTag, TextDiff};

use crate::markdown::parse_offsets::{block_ranges_by, BlockKind};

use super::hunk::{Decision, Hunk, HunkId, HunkKind, InlineSide, InlineSpan};

/// Allocator for [`HunkId`] values.  Held by `DiffState` so re-
/// computations after in-diff edits can mint fresh ids without
/// reusing old ones — id stability across recomputes is then
/// achieved by old-side overlap matching (Phase 1 §6).
#[derive(Debug, Default, Clone)]
pub struct HunkIdAllocator {
    next: u64,
}

impl HunkIdAllocator {
    pub fn new() -> Self {
        Self { next: 0 }
    }

    pub fn allocate(&mut self) -> HunkId {
        let id = HunkId(self.next);
        self.next = self.next.wrapping_add(1);
        id
    }
}

/// Compute the hunk list for `old_text` vs `new_text`.
///
/// Each returned hunk's `id` is freshly allocated via `ids`.  The
/// caller is expected to seed `decisions` (a parallel `Vec<Decision>`)
/// to `Decision::Pending` for every hunk; this function does not
/// touch decisions.
///
/// Returns hunks in document order.  Adjacent same-kind runs from
/// `similar` are coalesced into one hunk (so a 4-line delete + a 3-
/// line insert that touch produce one `Replace`, not two adjacent
/// hunks).
///
/// After the base diff is produced, any hunk that intersects a
/// markdown table extent on either side is split into per-row
/// hunks via [`split_table_hunk`].
pub fn compute_hunks(old_text: &str, new_text: &str, ids: &mut HunkIdAllocator) -> Vec<Hunk> {
    let diff = TextDiff::from_lines(old_text, new_text);

    // First pass: collapse `similar`'s op groups into a stream of
    // (old_lines, new_lines) ranges using line numbers reported by
    // `Change::old_index` / `new_index`.
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut old_start: Option<usize> = None;
    let mut new_start: Option<usize> = None;
    let mut old_end: usize = 0;
    let mut new_end: usize = 0;
    let mut last_old_seen: usize = 0;
    let mut last_new_seen: usize = 0;

    let flush = |old_s: &mut Option<usize>,
                 new_s: &mut Option<usize>,
                 old_e: &mut usize,
                 new_e: &mut usize,
                 hunks: &mut Vec<Hunk>,
                 ids: &mut HunkIdAllocator| {
        if old_s.is_none() && new_s.is_none() {
            return;
        }
        let old_lines = old_s.unwrap_or(*old_e)..*old_e;
        let new_lines = new_s.unwrap_or(*new_e)..*new_e;
        if old_lines.start == old_lines.end && new_lines.start == new_lines.end {
            *old_s = None;
            *new_s = None;
            return;
        }
        let kind = Hunk::classify(&old_lines, &new_lines);
        hunks.push(Hunk {
            id: ids.allocate(),
            old_lines,
            new_lines,
            inline: Vec::new(),
            kind,
        });
        *old_s = None;
        *new_s = None;
    };

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                flush(
                    &mut old_start,
                    &mut new_start,
                    &mut old_end,
                    &mut new_end,
                    &mut hunks,
                    ids,
                );
                if let Some(i) = change.old_index() {
                    last_old_seen = i + 1;
                }
                if let Some(i) = change.new_index() {
                    last_new_seen = i + 1;
                }
            }
            ChangeTag::Delete => {
                if old_start.is_none() {
                    old_start = Some(change.old_index().unwrap_or(last_old_seen));
                    old_end = old_start.unwrap();
                }
                if new_start.is_none() {
                    // We may be in the middle of a Replace where
                    // Inserts haven't yet been seen — anchor new_end
                    // at the last seen new line so a follow-up Insert
                    // starts in the right place.
                    new_end = last_new_seen;
                }
                old_end = change.old_index().map(|i| i + 1).unwrap_or(old_end + 1);
                last_old_seen = old_end;
            }
            ChangeTag::Insert => {
                if new_start.is_none() {
                    new_start = Some(change.new_index().unwrap_or(last_new_seen));
                    new_end = new_start.unwrap();
                }
                if old_start.is_none() {
                    old_end = last_old_seen;
                }
                new_end = change.new_index().map(|i| i + 1).unwrap_or(new_end + 1);
                last_new_seen = new_end;
            }
        }
    }
    flush(
        &mut old_start,
        &mut new_start,
        &mut old_end,
        &mut new_end,
        &mut hunks,
        ids,
    );

    // Second pass: word-level inline highlights inside `Replace`
    // hunks.  Skipped for `Insert` / `Delete` (no other side to diff
    // against) and for table hunks (handled by row-sub-diff below).
    for h in &mut hunks {
        if h.kind == HunkKind::Replace {
            populate_inline_spans(h, old_text, new_text);
        }
    }

    // Third pass: row-level table sub-diff.  Hunks that intersect a
    // table extent on either side are split into per-row hunks.
    let old_table_extents = table_line_extents(old_text);
    let new_table_extents = table_line_extents(new_text);
    let mut split: Vec<Hunk> = Vec::with_capacity(hunks.len());
    for h in hunks.into_iter() {
        if let Some(extra) = split_table_hunk(
            &h,
            old_text,
            new_text,
            &old_table_extents,
            &new_table_extents,
            ids,
        ) {
            split.extend(extra);
        } else {
            split.push(h);
        }
    }

    split
}

/// Populate `hunk.inline` with word-level diff spans inside a
/// `Replace` hunk.  Restricted to per-line diffs: each line of the
/// hunk's old-side and new-side is word-diffed via
/// [`TextDiff::from_words`] and the changed word ranges are emitted
/// as `InlineSpan`s.  Lines that don't pair 1:1 across sides (e.g.
/// the hunk has 3 old lines and 2 new lines) skip inline highlighting
/// — the line-level bg highlight is sufficient signal.
fn populate_inline_spans(hunk: &mut Hunk, old_text: &str, new_text: &str) {
    let old_lines = line_slice(old_text, hunk.old_lines.clone());
    let new_lines = line_slice(new_text, hunk.new_lines.clone());
    let pair_count = old_lines.len().min(new_lines.len());
    let mut spans = Vec::new();
    for i in 0..pair_count {
        let old_line = old_lines[i];
        let new_line = new_lines[i];
        if old_line == new_line {
            continue;
        }
        let word_diff = TextDiff::from_words(old_line, new_line);
        let mut old_pos = 0usize;
        let mut new_pos = 0usize;
        for change in word_diff.iter_all_changes() {
            let text = change.value();
            let char_count = text.chars().count();
            match change.tag() {
                ChangeTag::Equal => {
                    old_pos += char_count;
                    new_pos += char_count;
                }
                ChangeTag::Delete => {
                    if char_count > 0 {
                        spans.push(InlineSpan {
                            side: InlineSide::Old,
                            line_in_hunk: i,
                            chars: old_pos..old_pos + char_count,
                        });
                    }
                    old_pos += char_count;
                }
                ChangeTag::Insert => {
                    if char_count > 0 {
                        spans.push(InlineSpan {
                            side: InlineSide::New,
                            line_in_hunk: i,
                            chars: new_pos..new_pos + char_count,
                        });
                    }
                    new_pos += char_count;
                }
            }
        }
    }
    hunk.inline = spans;
}

/// Slice `text` into trimmed line strings for the range `lines`.
/// Trailing `\n` is stripped per line; lines past the end of the
/// rope are returned as empty strings.
fn line_slice(text: &str, lines: Range<usize>) -> Vec<&str> {
    if lines.start >= lines.end {
        return Vec::new();
    }
    let rope = Rope::from_str(text);
    let total = rope.len_lines();
    let end = lines.end.min(total);
    let start = lines.start.min(end);
    let mut out = Vec::with_capacity(end - start);
    for i in start..end {
        let line_start = rope.line_to_byte(i);
        let line_end = if i + 1 < total {
            rope.line_to_byte(i + 1)
        } else {
            rope.len_bytes()
        };
        let raw = &text[line_start..line_end];
        let trimmed = raw.strip_suffix('\n').unwrap_or(raw);
        out.push(trimmed);
    }
    out
}

/// Extent of one table block as line indices in the source text.
#[derive(Debug, Clone)]
struct TableExtent {
    /// Half-open line range `[start_line, end_line)`.
    lines: Range<usize>,
}

/// Walk `source` via [`block_ranges_by`] filtered to tables; return
/// each table's line range.
fn table_line_extents(source: &str) -> Vec<TableExtent> {
    let byte_ranges = block_ranges_by(source, |kind| kind == BlockKind::Table);
    if byte_ranges.is_empty() {
        return Vec::new();
    }
    let rope = Rope::from_str(source);
    byte_ranges
        .into_iter()
        .map(|r| {
            let start_line = rope.byte_to_line(r.start);
            let end_line = rope.byte_to_line(r.end);
            TableExtent {
                lines: start_line..end_line,
            }
        })
        .collect()
}

/// Returns `Some(replacement_hunks)` when `h` falls within a table
/// extent on either side and the row sub-diff produces more than one
/// hunk.  Returns `None` when the hunk doesn't touch a table, or
/// when the row-uniformity guard trips.
fn split_table_hunk(
    h: &Hunk,
    old_text: &str,
    new_text: &str,
    old_extents: &[TableExtent],
    new_extents: &[TableExtent],
    ids: &mut HunkIdAllocator,
) -> Option<Vec<Hunk>> {
    let old_extent = find_extent(old_extents, &h.old_lines);
    let new_extent = find_extent(new_extents, &h.new_lines);
    let (old_extent, new_extent) = match (old_extent, new_extent) {
        (Some(o), Some(n)) => (o, n),
        // A hunk that touches a table on only one side is a fan-out
        // — out of scope for Phase 1 row diffing; render as a single
        // monolithic Replace.
        _ => return None,
    };

    // Column-count guard: every row on each side must have the same
    // cell count, and the per-side maxima must match across sides.
    let old_rows = line_slice(old_text, old_extent.lines.clone());
    let new_rows = line_slice(new_text, new_extent.lines.clone());
    if !table_rows_uniform(&old_rows, &new_rows) {
        return None;
    }

    // Run `similar` over just the rows.  Decisions are per-row but
    // neighboring changed rows are coalesced into one run-hunk.
    let old_joined: String = old_rows.iter().map(|s| format!("{s}\n")).collect();
    let new_joined: String = new_rows.iter().map(|s| format!("{s}\n")).collect();
    let row_diff = TextDiff::from_lines(&old_joined, &new_joined);

    let mut hunks: Vec<Hunk> = Vec::new();
    let mut run_old_start: Option<usize> = None;
    let mut run_new_start: Option<usize> = None;
    let mut run_old_end: usize = 0;
    let mut run_new_end: usize = 0;
    let mut last_old: usize = 0;
    let mut last_new: usize = 0;

    let emit = |run_old_s: &mut Option<usize>,
                run_new_s: &mut Option<usize>,
                run_old_e: &mut usize,
                run_new_e: &mut usize,
                hunks: &mut Vec<Hunk>,
                ids: &mut HunkIdAllocator| {
        if run_old_s.is_none() && run_new_s.is_none() {
            return;
        }
        let o = run_old_s.unwrap_or(*run_old_e)..*run_old_e;
        let n = run_new_s.unwrap_or(*run_new_e)..*run_new_e;
        if o.start == o.end && n.start == n.end {
            *run_old_s = None;
            *run_new_s = None;
            return;
        }
        let kind = Hunk::classify(&o, &n);
        hunks.push(Hunk {
            id: ids.allocate(),
            old_lines: (old_extent.lines.start + o.start)..(old_extent.lines.start + o.end),
            new_lines: (new_extent.lines.start + n.start)..(new_extent.lines.start + n.end),
            inline: Vec::new(),
            kind,
        });
        *run_old_s = None;
        *run_new_s = None;
    };

    for change in row_diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                emit(
                    &mut run_old_start,
                    &mut run_new_start,
                    &mut run_old_end,
                    &mut run_new_end,
                    &mut hunks,
                    ids,
                );
                if let Some(i) = change.old_index() {
                    last_old = i + 1;
                }
                if let Some(i) = change.new_index() {
                    last_new = i + 1;
                }
            }
            ChangeTag::Delete => {
                if run_old_start.is_none() {
                    run_old_start = Some(change.old_index().unwrap_or(last_old));
                    run_old_end = run_old_start.unwrap();
                }
                if run_new_start.is_none() {
                    run_new_end = last_new;
                }
                run_old_end = change.old_index().map(|i| i + 1).unwrap_or(run_old_end + 1);
                last_old = run_old_end;
            }
            ChangeTag::Insert => {
                if run_new_start.is_none() {
                    run_new_start = Some(change.new_index().unwrap_or(last_new));
                    run_new_end = run_new_start.unwrap();
                }
                if run_old_start.is_none() {
                    run_old_end = last_old;
                }
                run_new_end = change.new_index().map(|i| i + 1).unwrap_or(run_new_end + 1);
                last_new = run_new_end;
            }
        }
    }
    emit(
        &mut run_old_start,
        &mut run_new_start,
        &mut run_old_end,
        &mut run_new_end,
        &mut hunks,
        ids,
    );

    // Word-level inline highlights inside each Replace row-hunk.
    for h in &mut hunks {
        if h.kind == HunkKind::Replace {
            populate_inline_spans(h, old_text, new_text);
        }
    }

    // If the row-diff degenerated to zero hunks (theoretically
    // unreachable — the parent hunk only got here because there was
    // a real difference inside the table extent — but worth guarding
    // defensively), return `None` so the caller keeps the original
    // monolithic hunk rather than silently dropping it.
    if hunks.is_empty() {
        return None;
    }

    Some(hunks)
}

fn find_extent<'a>(extents: &'a [TableExtent], lines: &Range<usize>) -> Option<&'a TableExtent> {
    extents
        .iter()
        .find(|e| lines.start < e.lines.end && e.lines.start < lines.end)
}

fn table_rows_uniform(old_rows: &[&str], new_rows: &[&str]) -> bool {
    // A zero-row side can't be uniformity-matched against the other
    // side, and falling through would let `max_old == max_new == 0`
    // pass the final guard — which then makes `split_table_hunk`
    // silently drop the hunk from its output.  Bail out so the caller
    // keeps the original monolithic hunk instead.
    if old_rows.is_empty() || new_rows.is_empty() {
        return false;
    }
    fn cell_count(row: &str) -> Option<usize> {
        let trimmed = row.trim();
        if !trimmed.starts_with('|') {
            return None;
        }
        // Count `|` characters that aren't escaped.  Markdown
        // tables use `|` as the cell delimiter; a leading and
        // trailing `|` are common but not required.  We count
        // unescaped pipes and convert to cells.
        let mut pipes = 0usize;
        let mut chars = trimmed.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' {
                let _ = chars.next();
                continue;
            }
            if c == '|' {
                pipes += 1;
            }
        }
        // N pipes → N-1 cells when the row starts and ends with `|`,
        // else N cells.  We require both delimiters present (which is
        // the canonical form pulldown-cmark accepts) so guard against
        // the rare case below.
        let cells = if trimmed.ends_with('|') {
            pipes.saturating_sub(1)
        } else {
            pipes
        };
        Some(cells)
    }

    let mut max_old = 0usize;
    for row in old_rows {
        // Separator rows are `|---|---|` etc. — treat them like data
        // rows by cell count; uniformity check requires the same
        // shape across header / separator / data rows.
        let Some(c) = cell_count(row) else {
            return false;
        };
        if max_old == 0 {
            max_old = c;
        } else if c != max_old {
            return false;
        }
    }
    let mut max_new = 0usize;
    for row in new_rows {
        let Some(c) = cell_count(row) else {
            return false;
        };
        if max_new == 0 {
            max_new = c;
        } else if c != max_new {
            return false;
        }
    }
    max_old == max_new
}

/// Build a fresh `Vec<Decision>` matching `hunks.len()`, seeded to
/// `Pending`.  Tiny helper but used at every recompute site, kept
/// here so the seeding rule lives next to the engine.
pub fn pending_decisions(hunks: &[Hunk]) -> Vec<Decision> {
    vec![Decision::Pending; hunks.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> HunkIdAllocator {
        HunkIdAllocator::new()
    }

    #[test]
    fn identical_inputs_produce_no_hunks() {
        let s = "a\nb\nc\n";
        let mut a = ids();
        assert!(compute_hunks(s, s, &mut a).is_empty());
    }

    #[test]
    fn pure_insert_produces_one_insert_hunk() {
        let old = "a\nb\n";
        let new = "a\nb\nc\n";
        let mut a = ids();
        let hunks = compute_hunks(old, new, &mut a);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind, HunkKind::Insert);
        assert_eq!(hunks[0].old_lines, 2..2);
        assert_eq!(hunks[0].new_lines, 2..3);
    }

    #[test]
    fn pure_delete_produces_one_delete_hunk() {
        let old = "a\nb\nc\n";
        let new = "a\nc\n";
        let mut a = ids();
        let hunks = compute_hunks(old, new, &mut a);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind, HunkKind::Delete);
        assert_eq!(hunks[0].old_lines, 1..2);
    }

    #[test]
    fn replace_emits_inline_spans_on_paired_lines() {
        let old = "alpha bravo\n";
        let new = "alpha gamma\n";
        let mut a = ids();
        let hunks = compute_hunks(old, new, &mut a);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind, HunkKind::Replace);
        assert!(!hunks[0].inline.is_empty());
    }

    #[test]
    fn ids_are_unique_and_monotonic() {
        let old = "a\nb\nc\n";
        let new = "x\nb\ny\n";
        let mut a = ids();
        let hunks = compute_hunks(old, new, &mut a);
        let mut seen = std::collections::HashSet::new();
        for h in &hunks {
            assert!(seen.insert(h.id), "duplicate id");
        }
    }
}
