//! `DiffState` — the in-flight review session attached to
//! `EditorState::diff` while `Mode::Diff` is active.  Owns the
//! pre-change rope (`old_rope`), the working new-side buffer, the
//! hunk list, per-hunk decisions, focus, and an undo stack that
//! lights up in CP4.
//!
//! In CP3 the stack stays empty; `Action::Undo` / `Action::Redo`
//! in diff mode are explicit no-ops until the wiring lands.

use ropey::Rope;

use crate::document::{Buffer, Cursor};

use super::engine::{compute, pending_decisions, HunkIdAllocator};
use super::hunk::{Decision, Hunk, HunkId};

/// Lifecycle: see `EditorState::enter_diff_mode`.  Created with
/// `DiffState::new(old, new)`, owned by `EditorState::diff` for the
/// duration of the review.
pub struct DiffState {
    /// Pre-change rope — the in-memory buffer at the moment the
    /// watcher reported a disk change.  Immutable across the review:
    /// recomputations after in-diff edits run against `old_rope` and
    /// the mutated `new_buffer`.
    pub old_rope: Rope,
    /// Working copy of the new-side text.  In CP3 this is read-only
    /// from a user perspective (no Edit sub-mode yet) — the buffer
    /// wrapper still exists so CP5 can route diff-Edit writes through
    /// the same `Buffer::insert` / `Buffer::remove` API.  The text on
    /// disk seeded this buffer at diff entry.
    pub new_buffer: Buffer,
    /// Cursor into `new_buffer.rope()`.  Used by CP5's Edit sub-mode;
    /// in CP3 it sits at the start of the focused hunk's new-side
    /// range but is otherwise unread.
    #[allow(dead_code)]
    pub cursor: Cursor,
    /// Per-hunk diff list, ordered by document position.
    pub hunks: Vec<Hunk>,
    /// Decision per hunk; `decisions[i]` corresponds to `hunks[i]`.
    pub decisions: Vec<Decision>,
    /// Stable id of the currently focused hunk.  Stored as an id
    /// (not an index) so it survives the index shuffle that future
    /// in-diff edits will introduce (Phase 1 §6 "HunkId stability").
    pub focused_id: HunkId,
    /// Monotonic id allocator — kept on the state so a follow-up
    /// recompute (CP4 / CP5) can mint fresh ids without colliding
    /// with previously-issued ones.  Unused in CP3; preserved so the
    /// follow-up checkpoints don't need to re-thread the allocator
    /// through every call site.
    #[allow(dead_code)]
    pub(crate) ids: HunkIdAllocator,
    /// Undo / redo stack for in-diff operations (`Decision`,
    /// `BulkDecision`, `Edit`).  Empty in CP3; wired in CP4 / CP5.
    #[allow(dead_code)]
    pub history: DiffHistory,
    /// True when at least one table couldn't be row-diffed because its
    /// rows had uneven cell counts, so its change is surfaced as the
    /// coarser line-level hunk(s) instead of per-row hunks (§3a).
    /// `App::enter_diff_mode` flashes a hint on entry so the user
    /// understands why that table isn't reviewable row-by-row.
    pub uneven_table_fallback: bool,
}

/// CP3 placeholder for the per-diff undo stack — concrete `DiffOp`
/// variants and merge/undo logic land in CP4 (`Decision` /
/// `BulkDecision`) and CP5 (`Edit`).
#[derive(Debug, Default)]
pub struct DiffHistory {
    #[allow(dead_code)]
    pub past: Vec<DiffOp>,
    #[allow(dead_code)]
    pub future: Vec<DiffOp>,
}

/// CP3 placeholder — the real variants (Decision, BulkDecision,
/// Edit) land in CP4 / CP5.  An empty enum would make the field
/// type unconstructible, but `record` is never called in CP3 so the
/// stack stays empty.
#[derive(Debug, Clone)]
pub enum DiffOp {
    /// Intentionally unused in CP3; placeholder for the CP4 variant.
    #[allow(dead_code)]
    Placeholder,
}

impl DiffState {
    /// Construct a fresh diff state from the pre-change buffer text
    /// (`old`) and the just-read disk contents (`new`).
    ///
    /// The caller (see [`crate::editor::EditorState::enter_diff_mode`])
    /// must have already verified that `old != new` — calling this
    /// with byte-equal inputs produces an empty hunk vec, which
    /// `enter_diff_mode` does not accept (the buffer-vs-disk check
    /// in §2 short-circuits the entry path).  Empty hunks defeat
    /// the `focused_id = hunks[0].id` invariant.
    pub fn new(old: &str, new: &str) -> Option<Self> {
        let mut ids = HunkIdAllocator::new();
        let computation = compute(old, new, &mut ids);
        let hunks = computation.hunks;
        if hunks.is_empty() {
            return None;
        }
        let decisions = pending_decisions(&hunks);
        let focused_id = hunks[0].id;
        let old_rope = Rope::from_str(old);
        let new_rope = Rope::from_str(new);
        let new_buffer = Buffer::from_rope(new_rope);
        // Start the cursor at the focused hunk's new-side first
        // line.  Even for Delete hunks (empty new-side range) this
        // is the canonical anchor in `new_rope`.
        let cursor_offset = new_buffer
            .rope()
            .line_to_char(hunks[0].new_lines.start.min(new_buffer.line_count()));
        let mut cursor = Cursor::new();
        cursor.offset = cursor_offset;
        Some(Self {
            old_rope,
            new_buffer,
            cursor,
            hunks,
            decisions,
            focused_id,
            ids,
            history: DiffHistory::default(),
            uneven_table_fallback: computation.uneven_table_fallback,
        })
    }

    /// Look up the focused hunk's index in `hunks` (or `None` when
    /// `focused_id` has gone stale — possible after a CP5 recompute
    /// drops the prior hunk).
    pub fn focused_idx(&self) -> Option<usize> {
        self.hunks.iter().position(|h| h.id == self.focused_id)
    }

    /// Set the decision for the focused hunk and advance focus to
    /// the next `Pending` hunk (wrapping).  Returns `true` when the
    /// decision was applied.  Used by `Action::DiffAcceptHunk` /
    /// `Action::DiffRejectHunk`.
    pub fn set_focused_decision(&mut self, decision: Decision) -> bool {
        let Some(idx) = self.focused_idx() else {
            return false;
        };
        self.decisions[idx] = decision;
        if let Some(next) = self.next_pending_after(idx) {
            self.focused_id = self.hunks[next].id;
        }
        true
    }

    /// Apply `decision` to every currently-`Pending` hunk in one go.
    /// Used by `Action::DiffAcceptAll` / `Action::DiffRejectAll`.
    /// Decisions already non-`Pending` are left untouched so the
    /// user's prior choices are preserved.
    pub fn bulk_decide_pending(&mut self, decision: Decision) {
        for d in &mut self.decisions {
            if *d == Decision::Pending {
                *d = decision;
            }
        }
    }

    /// Advance `focused_id` to the next hunk in document order
    /// (wraps).  Used by `Action::DiffNext` ("skip / next"). Returns
    /// `true` when focus actually moved.
    pub fn advance_focus(&mut self) -> bool {
        let Some(idx) = self.focused_idx() else {
            return false;
        };
        if self.hunks.is_empty() {
            return false;
        }
        let next = (idx + 1) % self.hunks.len();
        if next == idx {
            return false;
        }
        self.focused_id = self.hunks[next].id;
        true
    }

    /// Retreat focus to the previous hunk in document order.
    pub fn retreat_focus(&mut self) -> bool {
        let Some(idx) = self.focused_idx() else {
            return false;
        };
        if self.hunks.is_empty() {
            return false;
        }
        let prev = if idx == 0 {
            self.hunks.len() - 1
        } else {
            idx - 1
        };
        if prev == idx {
            return false;
        }
        self.focused_id = self.hunks[prev].id;
        true
    }

    /// Number of hunks still awaiting a decision.  Status bar shows
    /// `pending/total` via this.
    pub fn pending_count(&self) -> usize {
        self.decisions
            .iter()
            .filter(|d| **d == Decision::Pending)
            .count()
    }

    /// True iff every hunk has a non-`Pending` decision.  Triggers
    /// the `DiffResolveConfirmModal`.
    pub fn all_resolved(&self) -> bool {
        self.pending_count() == 0
    }

    /// Walk the hunk list and produce the merged rope per
    /// `decisions[i]`: `Accepted` picks the new-side range,
    /// `Rejected` picks the old-side range, `Pending` is a
    /// programming error.  Returns `None` if any decision is still
    /// `Pending` (rather than panicking — caller can surface a
    /// status flash).
    pub fn resolved_rope(&self) -> Option<Rope> {
        if !self.all_resolved() {
            return None;
        }
        let old_text = self.old_rope.to_string();
        let new_text = self.new_buffer.contents();
        let old_rope = &self.old_rope;
        let new_rope = self.new_buffer.rope();
        let mut out = String::new();
        let mut new_cursor = 0usize; // line index into new_rope
        for (h, dec) in self.hunks.iter().zip(self.decisions.iter()) {
            // Emit unchanged context up to the hunk start.  Context
            // lines come from `new_rope` (they are byte-identical on
            // both sides — we only care about line count).  We
            // advance both cursors by the same amount.
            let new_gap = h.new_lines.start.saturating_sub(new_cursor);
            for _ in 0..new_gap {
                if new_cursor < new_rope.len_lines() {
                    append_line(&mut out, &new_text, new_rope, new_cursor);
                    new_cursor += 1;
                }
            }
            match dec {
                Decision::Accepted => {
                    for i in h.new_lines.clone() {
                        if i < new_rope.len_lines() {
                            append_line(&mut out, &new_text, new_rope, i);
                        }
                    }
                }
                Decision::Rejected => {
                    for i in h.old_lines.clone() {
                        if i < old_rope.len_lines() {
                            append_line(&mut out, &old_text, old_rope, i);
                        }
                    }
                }
                Decision::Pending => unreachable!("guarded by all_resolved()"),
            }
            new_cursor = h.new_lines.end;
        }
        // Trailing context after the last hunk.
        while new_cursor < new_rope.len_lines() {
            append_line(&mut out, &new_text, new_rope, new_cursor);
            new_cursor += 1;
        }
        Some(Rope::from_str(&out))
    }

    fn next_pending_after(&self, start: usize) -> Option<usize> {
        if self.hunks.is_empty() {
            return None;
        }
        let n = self.hunks.len();
        for step in 1..=n {
            let i = (start + step) % n;
            if self.decisions[i] == Decision::Pending {
                return Some(i);
            }
        }
        // Nothing pending — leave focus on `start` so the user can
        // still navigate manually before resolution fires.
        Some(start)
    }
}

fn append_line(out: &mut String, text: &str, rope: &Rope, line_idx: usize) {
    let line_start = rope.line_to_byte(line_idx);
    let line_end = if line_idx + 1 < rope.len_lines() {
        rope.line_to_byte(line_idx + 1)
    } else {
        rope.len_bytes()
    };
    let raw = &text[line_start..line_end];
    out.push_str(raw);
    // Ensure a trailing newline so the next line starts on its own.
    if !raw.ends_with('\n') && line_idx + 1 < rope.len_lines() {
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_diff_returns_none() {
        assert!(DiffState::new("same\n", "same\n").is_none());
    }

    #[test]
    fn accept_all_yields_new_rope() {
        let mut state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
        state.bulk_decide_pending(Decision::Accepted);
        let resolved = state.resolved_rope().unwrap();
        assert_eq!(resolved.to_string(), "a\nB\nc\n");
    }

    #[test]
    fn reject_all_yields_old_rope() {
        let mut state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
        state.bulk_decide_pending(Decision::Rejected);
        let resolved = state.resolved_rope().unwrap();
        assert_eq!(resolved.to_string(), "a\nb\nc\n");
    }

    #[test]
    fn mixed_decisions_pick_per_hunk() {
        // Two hunks: replace line 1, insert at end.
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\nD\n";
        let mut state = DiffState::new(old, new).unwrap();
        assert!(state.hunks.len() >= 2);
        // Accept first, reject second.
        state.decisions[0] = Decision::Accepted;
        state.decisions[1] = Decision::Rejected;
        let resolved = state.resolved_rope().unwrap();
        assert_eq!(resolved.to_string(), "a\nB\nc\n");
    }

    #[test]
    fn two_deletes_mixed_decisions_reconstruct_correctly() {
        // Two separate Delete hunks (b on line 1, d on line 3).
        // Regression coverage: a Rejected Delete must not advance
        // `new_cursor` past its zero-length new-side range, otherwise
        // the gap before the next hunk would skip context lines or
        // re-emit them.
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nc\ne\n";
        let mut state = DiffState::new(old, new).unwrap();
        assert_eq!(state.hunks.len(), 2);

        // Reject H1 (keep b), accept H2 (delete d).
        state.decisions[0] = Decision::Rejected;
        state.decisions[1] = Decision::Accepted;
        assert_eq!(state.resolved_rope().unwrap().to_string(), "a\nb\nc\ne\n");

        // Accept H1 (delete b), reject H2 (keep d).
        state.decisions[0] = Decision::Accepted;
        state.decisions[1] = Decision::Rejected;
        assert_eq!(state.resolved_rope().unwrap().to_string(), "a\nc\nd\ne\n");
    }

    #[test]
    fn all_rejected_yields_original_old_text() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nC\ne\n";
        let mut state = DiffState::new(old, new).unwrap();
        state.bulk_decide_pending(Decision::Rejected);
        let resolved = state.resolved_rope().unwrap();
        assert_eq!(resolved.to_string(), old);
    }

    #[test]
    fn pending_count_tracks_decisions() {
        let mut state = DiffState::new("a\nb\n", "a\nB\n").unwrap();
        assert_eq!(state.pending_count(), 1);
        state.set_focused_decision(Decision::Accepted);
        assert_eq!(state.pending_count(), 0);
        assert!(state.all_resolved());
    }
}
