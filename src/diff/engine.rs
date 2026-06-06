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
//! Inline word-level highlights are currently restricted to text
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
/// achieved by old-side overlap matching (§6).
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

/// Outcome of [`compute`]: the hunk list plus advisory warnings the
/// UI should surface.
#[derive(Debug, Default)]
pub struct HunkComputation {
    /// The hunks, in document order.
    pub hunks: Vec<Hunk>,
    /// At least one markdown table couldn't be row-split because its
    /// rows had uneven cell counts; its change is kept as the original
    /// line-level hunk(s) rather than per-row hunks, so it can't be
    /// reviewed row-by-row (§3a).  The UI flashes a hint so the
    /// coarse-grained hunk doesn't read like a bug.
    pub uneven_table_fallback: bool,
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
pub fn compute(old_text: &str, new_text: &str, ids: &mut HunkIdAllocator) -> HunkComputation {
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

    // Build per-side line indices once; the inline-span and table
    // passes below slice lines and convert byte↔line offsets through
    // these instead of rebuilding a `Rope` per hunk.
    let old_index = LineIndex::new(old_text);
    let new_index = LineIndex::new(new_text);

    // Second pass: word-level inline highlights inside `Replace`
    // hunks.  Skipped for `Insert` / `Delete` (no other side to diff
    // against) and for table hunks (handled by row-sub-diff below).
    for h in &mut hunks {
        if h.kind == HunkKind::Replace {
            populate_inline_spans(h, &old_index, &new_index);
        }
    }

    // Third pass: row-level table sub-diff.  A hunk fully contained
    // within a table extent on *both* sides is a candidate for
    // splitting that table into per-row hunks.  A single row-diff over
    // the whole extent captures every changed row regardless of how the
    // line-level pass chunked it, so each table is split at most once
    // and the remaining contained hunks are dropped — re-splitting the
    // same extent would emit every row twice (duplicate hunks →
    // duplicated lines on resolve).
    //
    // Containment can map several contained hunks of one old table to
    // *different* new-side extents when the table splits into two
    // tables on the new side — i.e. a fresh header+separator appears
    // mid-table, so the lower fragment parses as its own table (a plain
    // paragraph wouldn't: the fragment would lose its header and not
    // parse as a table at all).  A single extent re-diff can't represent
    // two new tables, so such an old extent is left un-split and its
    // hunks pass through as line-level hunks rather than being silently
    // dropped — see `table_split_into_two_keeps_both_changes_reviewable`.
    let old_table_extents = table_line_extents(&old_index);
    let new_table_extents = table_line_extents(&new_index);

    // Pre-scan: for each old table extent, record which new-side extent
    // its contained hunks map to (`One` = all agree, `Conflict` = they
    // map to different new extents).  `meta[i]` caches the per-side
    // containment lookup for `hunks[i]` so the main loop doesn't repeat
    // it.
    let meta: Vec<(Option<usize>, Option<usize>)> = hunks
        .iter()
        .map(|h| {
            (
                find_extent_idx(&old_table_extents, &h.old_lines),
                find_extent_idx(&new_table_extents, &h.new_lines),
            )
        })
        .collect();
    let mut ni_map = vec![NiMap::Unset; old_table_extents.len()];
    for &(oi, ni) in &meta {
        if let (Some(oi), Some(ni)) = (oi, ni) {
            ni_map[oi] = match ni_map[oi] {
                NiMap::Unset => NiMap::One(ni),
                NiMap::One(prev) if prev == ni => NiMap::One(prev),
                _ => NiMap::Conflict,
            };
        }
    }

    let mut split: Vec<Hunk> = Vec::with_capacity(hunks.len());
    let mut split_done = vec![false; old_table_extents.len()];
    let mut uneven_table_fallback = false;
    for (h, &(old_idx, new_idx)) in hunks.into_iter().zip(meta.iter()) {
        let (Some(oi), Some(ni)) = (old_idx, new_idx) else {
            // Not inside a table on both sides — keep as-is (covers
            // non-table hunks and boundary-straddling hunks).
            split.push(h);
            continue;
        };
        if !matches!(ni_map[oi], NiMap::One(_)) {
            // Fragmented table (Conflict) — keep the line-level hunk so
            // its change stays reviewable rather than being dropped.
            split.push(h);
            continue;
        }
        if split_done[oi] {
            // Table already row-split; its rows are represented.  Drop
            // this hunk so they aren't emitted twice.
            continue;
        }
        match split_table_hunk(
            &old_table_extents[oi],
            &new_table_extents[ni],
            &old_index,
            &new_index,
            ids,
        ) {
            SplitOutcome::Rows(rows) => {
                split_done[oi] = true;
                split.extend(rows);
            }
            SplitOutcome::Uneven => {
                // Uneven cell counts — review the table as a unit.  Do
                // NOT mark the extent done: any other contained hunks
                // fall through here too and stay as disjoint line-level
                // hunks (no whole-table coverage means no duplication).
                uneven_table_fallback = true;
                split.push(h);
            }
            SplitOutcome::Degenerate => {
                // Row-diff produced nothing (defensive) — keep the
                // monolithic hunk, same non-marking rationale as above.
                split.push(h);
            }
        }
    }

    HunkComputation {
        hunks: split,
        uneven_table_fallback,
    }
}

/// Pre-scan result for one old table extent: which new-side table
/// extent its contained hunks map to.
#[derive(Clone, Copy)]
enum NiMap {
    /// No hunk is contained in this extent on both sides.
    Unset,
    /// Every contained hunk maps to the same new extent index.
    One(usize),
    /// Contained hunks map to *different* new extents (the table
    /// fragmented into several on the new side).
    Conflict,
}

/// Result of attempting to row-split a table extent.
enum SplitOutcome {
    /// Per-row hunks covering the whole table.
    Rows(Vec<Hunk>),
    /// Rows had uneven cell counts — the caller keeps the original
    /// line-level hunk(s) (no per-row split) and flashes the §3a hint.
    Uneven,
    /// Row-diff produced no hunks (defensive; shouldn't happen).
    Degenerate,
}

/// Populate `hunk.inline` with word-level diff spans inside a
/// `Replace` hunk.  Restricted to per-line diffs: each line of the
/// hunk's old-side and new-side is word-diffed via
/// [`TextDiff::from_words`] and the changed word ranges are emitted
/// as `InlineSpan`s.  Lines that don't pair 1:1 across sides (e.g.
/// the hunk has 3 old lines and 2 new lines) skip inline highlighting
/// — the line-level bg highlight is sufficient signal.
fn populate_inline_spans(hunk: &mut Hunk, old_index: &LineIndex, new_index: &LineIndex) {
    let old_lines = old_index.slice(hunk.old_lines.clone());
    let new_lines = new_index.slice(hunk.new_lines.clone());
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

/// Precomputed line-start byte offsets for a text, mirroring ropey's
/// line model: N newlines → N+1 lines, with a final empty line when
/// the text ends in `\n`.  Built once per side in [`compute`] so the
/// per-hunk slicing and byte↔line conversions don't each rebuild a
/// `Rope`.
struct LineIndex<'a> {
    text: &'a str,
    /// Byte offset of the first byte of each line.  `starts.len()` is
    /// the line count.  Strictly increasing, so [`Self::byte_to_line`]
    /// can binary-search it.
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    fn new(text: &'a str) -> Self {
        let mut starts = vec![0usize];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self { text, starts }
    }

    fn len_lines(&self) -> usize {
        self.starts.len()
    }

    /// Line index containing byte offset `byte`.  Mirrors
    /// `Rope::byte_to_line`, including its behavior at `text.len()`
    /// (returns `len_lines() - 1`).
    fn byte_to_line(&self, byte: usize) -> usize {
        match self.starts.binary_search(&byte) {
            Ok(i) => i,
            // `Err(0)` is impossible: `starts[0] == 0 <= byte` always.
            Err(i) => i - 1,
        }
    }

    /// Trimmed (no trailing `\n`) text of each line in `range`, clamped
    /// to the available lines.  Returns an empty vec for an empty or
    /// inverted range.
    fn slice(&self, range: Range<usize>) -> Vec<&'a str> {
        if range.start >= range.end {
            return Vec::new();
        }
        let total = self.len_lines();
        let end = range.end.min(total);
        let start = range.start.min(end);
        let mut out = Vec::with_capacity(end - start);
        for i in start..end {
            let line_start = self.starts[i];
            let line_end = if i + 1 < total {
                self.starts[i + 1]
            } else {
                self.text.len()
            };
            let raw = &self.text[line_start..line_end];
            out.push(raw.strip_suffix('\n').unwrap_or(raw));
        }
        out
    }
}

/// Extent of one table block as line indices in the source text.
#[derive(Debug, Clone)]
struct TableExtent {
    /// Half-open line range `[start_line, end_line)`.
    lines: Range<usize>,
}

/// Walk the indexed source via [`block_ranges_by`] filtered to tables;
/// return each table's line range.
fn table_line_extents(index: &LineIndex) -> Vec<TableExtent> {
    block_ranges_by(index.text, |kind| kind == BlockKind::Table)
        .into_iter()
        .map(|r| TableExtent {
            lines: index.byte_to_line(r.start)..index.byte_to_line(r.end),
        })
        .collect()
}

/// Row-split one table, given the table's old- and new-side line
/// extents.  Runs a single `similar` diff over the table's rows and
/// emits one hunk per coalesced run of changed rows.  Returns
/// [`SplitOutcome::Uneven`] when the row-uniformity guard trips
/// (non-rectangular table — the caller then keeps the original
/// line-level hunk(s) and flashes a hint).  The caller is responsible
/// for having verified, via [`find_extent_idx`], that the triggering
/// hunk is fully contained in these extents on both sides.
fn split_table_hunk(
    old_extent: &TableExtent,
    new_extent: &TableExtent,
    old_index: &LineIndex,
    new_index: &LineIndex,
    ids: &mut HunkIdAllocator,
) -> SplitOutcome {
    // Column-count guard: every row on each side must have the same
    // cell count, and the per-side maxima must match across sides.
    let old_rows = old_index.slice(old_extent.lines.clone());
    let new_rows = new_index.slice(new_extent.lines.clone());
    if !table_rows_uniform(&old_rows, &new_rows) {
        return SplitOutcome::Uneven;
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
            populate_inline_spans(h, old_index, new_index);
        }
    }

    // If the row-diff degenerated to zero hunks (theoretically
    // unreachable — the parent hunk only got here because there was
    // a real difference inside the table extent — but worth guarding
    // defensively), report it so the caller keeps the original
    // monolithic hunk rather than silently dropping it.
    if hunks.is_empty() {
        return SplitOutcome::Degenerate;
    }

    SplitOutcome::Rows(hunks)
}

/// Find the index of the table extent that *fully contains* `lines`.
///
/// Containment (not mere overlap) is required: a hunk that only
/// partially overlaps a table — i.e. it also covers non-table lines
/// above or below the table — must NOT be row-split, because
/// [`split_table_hunk`] re-diffs the whole table extent and would
/// silently drop the hunk's out-of-extent lines from its output
/// (losing a reviewable change and corrupting the merge). Such a
/// straddling hunk falls back to a single monolithic `Replace`
/// instead (§3a "render as a single monolithic Replace").
fn find_extent_idx(extents: &[TableExtent], lines: &Range<usize>) -> Option<usize> {
    extents
        .iter()
        .position(|e| lines.start >= e.lines.start && lines.end <= e.lines.end)
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

/// Number of overlapping lines between two half-open line ranges
/// (`0` when disjoint).
fn old_range_overlap(a: &Range<usize>, b: &Range<usize>) -> usize {
    let start = a.start.max(b.start);
    let end = a.end.min(b.end);
    end.saturating_sub(start)
}

/// Match `hunk` against a prior hunk list by **old-side overlap** — the
/// §6 rule-2 stability primitive, used by both the §11b reconcile path
/// (CP5) and CP6's post-edit recompute.
///
/// `old_rope` is invariant for the whole life of a review, so the
/// old-side line range is a stable anchor: an external write (or an
/// in-diff edit) only ever changes the *new* side.  The matched prior
/// is the one whose `old_lines` overlaps `hunk.old_lines` most; ties
/// break toward the smallest `old_lines.start`.
///
/// **Insert hunks** have an empty old-side range (`start == end`), so
/// they overlap nothing and can't be matched by overlap length.  They
/// are instead anchored by their *insertion point*: a candidate Insert
/// matches a prior Insert at the same old-side position.  This keeps an
/// accepted/rejected insertion's decision across an unrelated external
/// write (the common AI-collaboration case — an agent adds a block, the
/// user accepts it, then the agent edits elsewhere).  Distinct Insert
/// hunks always sit at distinct old positions (separated by context), so
/// the anchor is unambiguous.  An overlap match (score ≥ 1) always
/// outranks an insertion-point match (score 0); the two never compete
/// for the same candidate (only an empty candidate uses the latter).
///
/// Returns `None` when no prior matches at all.
pub fn match_by_old_overlap(hunk: &Hunk, priors: &[Hunk]) -> Option<usize> {
    let cand = &hunk.old_lines;
    let cand_empty = cand.start == cand.end;
    let mut best: Option<(usize, usize, usize)> = None; // (score, start, idx)
    for (i, p) in priors.iter().enumerate() {
        let po = &p.old_lines;
        let overlap = old_range_overlap(cand, po);
        let score = if overlap > 0 {
            overlap
        } else if cand_empty && po.start == po.end && po.start == cand.start {
            // Both are Inserts at the same old-side anchor — the same
            // insertion point.  Score 0 so any real overlap still wins.
            0
        } else {
            continue;
        };
        let better = match best {
            None => true,
            Some((best_score, best_start, _)) => {
                score > best_score || (score == best_score && po.start < best_start)
            }
        };
        if better {
            best = Some((score, po.start, i));
        }
    }
    best.map(|(_, _, idx)| idx)
}

/// The hunk's new-side text — the concatenation of its `new_lines`
/// (including trailing newlines) read from `rope`.  A `Delete` hunk
/// has an empty new-side range and yields `""`.  Used by the reconcile
/// gate to decide whether a matched hunk's new-side target was changed
/// by the external write (carry the decision) or not (reset to
/// `Pending`).
pub fn hunk_new_side_text(hunk: &Hunk, rope: &Rope) -> String {
    let mut out = String::new();
    let total = rope.len_lines();
    for line_idx in hunk.new_lines.clone() {
        if line_idx < total {
            out.push_str(&rope.line(line_idx).to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> HunkIdAllocator {
        HunkIdAllocator::new()
    }

    /// Test convenience: the hunks of [`compute`], discarding the
    /// advisory warnings these cases don't assert on.
    fn compute_hunks(old: &str, new: &str, ids: &mut HunkIdAllocator) -> Vec<Hunk> {
        compute(old, new, ids).hunks
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

    fn hunk_with_old(old_lines: Range<usize>) -> Hunk {
        Hunk {
            id: HunkId(0),
            old_lines: old_lines.clone(),
            new_lines: 0..0,
            inline: Vec::new(),
            kind: Hunk::classify(&old_lines, &(0..0)),
        }
    }

    #[test]
    fn match_by_old_overlap_picks_largest_overlap_and_breaks_ties() {
        // Largest overlap wins.
        let priors = vec![hunk_with_old(0..3), hunk_with_old(5..7)];
        let cand = hunk_with_old(1..2);
        assert_eq!(match_by_old_overlap(&cand, &priors), Some(0));

        // Tie on overlap → smallest old_lines.start wins.
        let priors = vec![hunk_with_old(0..4), hunk_with_old(2..6)];
        let cand = hunk_with_old(2..4); // overlaps both by 2 lines
        assert_eq!(match_by_old_overlap(&cand, &priors), Some(0));

        // No overlap → None.
        let priors = vec![hunk_with_old(0..2)];
        assert_eq!(match_by_old_overlap(&hunk_with_old(10..12), &priors), None);
        // An empty candidate (Insert) does NOT match a non-empty prior,
        // even one whose range straddles the insertion point.
        assert_eq!(match_by_old_overlap(&hunk_with_old(1..1), &priors), None);
    }

    #[test]
    fn match_by_old_overlap_anchors_inserts_by_position() {
        // Two prior Inserts at distinct old-side positions.
        let priors = vec![hunk_with_old(1..1), hunk_with_old(3..3)];
        // A candidate Insert at the same anchor matches that prior.
        assert_eq!(match_by_old_overlap(&hunk_with_old(3..3), &priors), Some(1));
        assert_eq!(match_by_old_overlap(&hunk_with_old(1..1), &priors), Some(0));
        // An Insert at a fresh position matches nothing.
        assert_eq!(match_by_old_overlap(&hunk_with_old(5..5), &priors), None);
        // A real overlap outranks any insertion-point match.
        let priors = vec![hunk_with_old(2..2), hunk_with_old(1..4)];
        assert_eq!(match_by_old_overlap(&hunk_with_old(2..3), &priors), Some(1));
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
