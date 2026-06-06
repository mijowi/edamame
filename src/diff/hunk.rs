//! Hunk types for the diff subsystem.
//!
//! A `Hunk` carries the old-side and new-side line ranges that
//! `similar::TextDiff::from_lines` identified as differing, plus the
//! decision the user has made (or not yet made) about that change.
//!
//! Line ranges follow the half-open convention `[start_line,
//! end_line)` documented in `docs/diff-mode-plan.md` §3a — `end_line`
//! is the line index immediately past the hunk's last line.  For
//! `HunkKind::Insert`, `old_lines.start == old_lines.end` (no
//! old-side content); for `HunkKind::Delete`, similarly
//! `new_lines.start == new_lines.end`.

use std::ops::Range;

/// Stable per-hunk identifier.  Monotonically allocated from
/// `DiffState::next_hunk_id` at construction and never reused — even
/// across hunk-list recomputations triggered by in-diff edits
/// (§6 "HunkId stability").  IDs survive index shifts, so
/// `DiffState::focused_id` (and the per-hunk decision matching across
/// a recompute) can reference a specific hunk without races against
/// recomputation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HunkId(pub u64);

/// Per-hunk decision recorded by the user during diff review.
/// Resolution proceeds only when every hunk's decision is
/// non-`Pending`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Decision {
    #[default]
    Pending,
    Accepted,
    Rejected,
}

/// Whether the hunk inserts, deletes, or replaces lines.  Derived
/// from the emptiness of `old_lines` / `new_lines` at construction
/// — kept as an enum so `DiffView` can branch without re-checking
/// emptiness on every render frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkKind {
    /// Both `old_lines` and `new_lines` are non-empty.
    Replace,
    /// `old_lines` is empty; only `new_lines` carries content.
    Insert,
    /// `new_lines` is empty; only `old_lines` carries content.
    Delete,
}

/// A word-level inline highlight within a `Replace` hunk's old- or
/// new-side text.  The span is expressed as a char range within the
/// concatenated old-side (resp. new-side) text of the hunk.
///
/// The engine wires the inline spans through to the renderer
/// but does not yet split table-row sub-hunks below the row level;
/// for `Insert` and `Delete` hunks the vec is empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineSpan {
    /// Which side of the hunk this span highlights.
    pub side: InlineSide,
    /// Line index (0-based) within the hunk's old- or new-side line
    /// range that contains the span.
    pub line_in_hunk: usize,
    /// Char range within that line's text (excluding trailing
    /// newline).  Half-open `[start, end)`.
    pub chars: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineSide {
    Old,
    New,
}

/// A single contiguous diff hunk with stable id, line ranges, kind,
/// inline highlights, and current decision.
#[derive(Debug, Clone)]
pub struct Hunk {
    pub id: HunkId,
    pub old_lines: Range<usize>,
    pub new_lines: Range<usize>,
    pub inline: Vec<InlineSpan>,
    pub kind: HunkKind,
}

impl Hunk {
    /// Classify a (old_lines, new_lines) pair as `Insert` / `Delete`
    /// / `Replace`.  Used by the engine when building a fresh hunk.
    pub(crate) fn classify(old_lines: &Range<usize>, new_lines: &Range<usize>) -> HunkKind {
        let old_empty = old_lines.start == old_lines.end;
        let new_empty = new_lines.start == new_lines.end;
        match (old_empty, new_empty) {
            (true, false) => HunkKind::Insert,
            (false, true) => HunkKind::Delete,
            // (true, true) shouldn't happen — an empty hunk is not a
            // hunk.  Default to Replace so the renderer paints
            // whatever was passed; the engine never emits this case.
            (false, false) | (true, true) => HunkKind::Replace,
        }
    }
}
