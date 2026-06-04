//! `DiffState` — the in-flight review session attached to
//! `EditorState::diff` while `Mode::Diff` is active.  Owns the
//! pre-change rope (`old_rope`), the working new-side buffer, the
//! hunk list, per-hunk decisions, and focus.
//!
//! There is no per-diff undo stack today: hunk decisions are
//! deliberately not undoable (a mis-press is recovered by navigating
//! back and re-deciding, or via `DiffResetHunk`; the accidental
//! bulk-flip case is guarded by a confirmation modal instead), and the
//! in-diff text-editing mode that *will* record edits — `DiffHistory`
//! in `src/diff/history.rs` — does not land until the Edit sub-mode
//! (CP6).  `Action::Undo` / `Action::Redo` are no-ops in diff Review.

use std::cell::RefCell;

use ropey::Rope;

use crate::document::{Buffer, Cursor};

use super::engine::{compute, pending_decisions, HunkIdAllocator};
use super::hunk::{Decision, Hunk, HunkId};
use super::layout::DiffLayoutCache;

/// Lifecycle: see `EditorState::enter_diff_mode`.  Created with
/// `DiffState::new(old, new)`, owned by `EditorState::diff` for the
/// duration of the review.
pub struct DiffState {
    /// Pre-change rope — the in-memory buffer at the moment the
    /// watcher reported a disk change.  Immutable across the review:
    /// recomputations after in-diff edits run against `old_rope` and
    /// the mutated `new_buffer`.
    pub old_rope: Rope,
    /// Working copy of the new-side text.  Today this is read-only
    /// from a user perspective (no in-diff text editing yet) — the
    /// buffer wrapper exists so a future Edit mode can route diff-side
    /// writes through the same `Buffer::insert` / `Buffer::remove`
    /// API.  The text on disk seeded this buffer at diff entry.
    pub new_buffer: Buffer,
    /// Cursor into `new_buffer.rope()`.  Reserved for a future in-diff
    /// Edit mode; today it sits at the start of the focused hunk's
    /// new-side range but is otherwise unread.
    #[allow(dead_code)]
    pub cursor: Cursor,
    /// Per-hunk diff list, ordered by document position.
    pub hunks: Vec<Hunk>,
    /// Decision per hunk; `decisions[i]` corresponds to `hunks[i]`.
    pub decisions: Vec<Decision>,
    /// Stable id of the currently focused hunk.  Stored as an id
    /// (not an index) so it survives the index shuffle a recompute can
    /// introduce — e.g. when an external change is reconciled into the
    /// review, or a future in-diff edit reshapes the hunk list.
    pub focused_id: HunkId,
    /// Monotonic id allocator — kept on the state so a follow-up
    /// recompute can mint fresh ids without colliding with
    /// previously-issued ones.  Currently unused; preserved so the
    /// recompute path won't need to re-thread the allocator through
    /// every call site.
    #[allow(dead_code)]
    pub(crate) ids: HunkIdAllocator,
    /// True when at least one table couldn't be row-diffed because its
    /// rows had uneven cell counts, so its change is surfaced as the
    /// coarser line-level hunk(s) instead of per-row hunks (§3a).
    /// `App::enter_diff_mode` flashes a hint on entry so the user
    /// understands why that table isn't reviewable row-by-row.
    pub uneven_table_fallback: bool,
    /// Lazily-built flat visual-line list + per-width row-count cache
    /// (see [`super::layout`]).  Interior-mutable so the immutable
    /// render / scroll-query paths can populate it on first use.
    pub(crate) layout: RefCell<DiffLayoutCache>,
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
            uneven_table_fallback: computation.uneven_table_fallback,
            layout: RefCell::new(DiffLayoutCache::default()),
        })
    }

    /// Look up the focused hunk's index in `hunks` (or `None` when
    /// `focused_id` has gone stale — possible after a recompute drops
    /// the prior hunk).
    pub fn focused_idx(&self) -> Option<usize> {
        self.hunks.iter().position(|h| h.id == self.focused_id)
    }

    /// Set the decision for the focused hunk *without* moving focus.
    /// The caller advances separately (after a brief reveal delay) so
    /// the user sees the resolved checkbox land before focus jumps to
    /// the next hunk.  Returns `true` when applied.
    pub fn decide_focused(&mut self, decision: Decision) -> bool {
        let Some(idx) = self.focused_idx() else {
            return false;
        };
        self.decisions[idx] = decision;
        true
    }

    /// Reset the focused hunk's decision back to `Pending`
    /// ("undecide").  Returns `true` when a decision was actually
    /// cleared; `false` when the hunk is already `Pending` (so the
    /// caller can treat it as a no-op).
    pub fn reset_focused(&mut self) -> bool {
        let Some(idx) = self.focused_idx() else {
            return false;
        };
        if self.decisions[idx] == Decision::Pending {
            return false;
        }
        self.decisions[idx] = Decision::Pending;
        true
    }

    /// The focused hunk's current decision, if focus is still valid.
    /// Used by the hint line to show the `Reset` chord only when there
    /// is actually a decision to reset.
    pub fn focused_decision(&self) -> Option<Decision> {
        self.focused_idx().map(|idx| self.decisions[idx])
    }

    /// Move focus to the next still-`Pending` hunk after the focused
    /// one (wrapping).  No-op (returns `false`) when nothing else is
    /// pending, leaving focus on the current hunk.  Used by the
    /// deferred post-decision advance.
    pub fn advance_to_next_pending(&mut self) -> bool {
        let Some(idx) = self.focused_idx() else {
            return false;
        };
        match self.next_pending_after(idx) {
            Some(next) if next != idx => {
                self.focused_id = self.hunks[next].id;
                true
            }
            _ => false,
        }
    }

    /// Apply `decision` to *every* hunk in one go, overriding any prior
    /// choices.  Used by `Action::DiffAcceptAll` / `Action::DiffRejectAll`:
    /// "accept all" / "reject all" are decisive whole-diff actions, so
    /// they set every hunk rather than only the still-`Pending` ones.
    /// This also keeps the keys functional once the diff is already fully
    /// resolved — the user can flip a fully-decided diff to all-accepted
    /// or all-rejected in a single keystroke (a per-pending-only version
    /// would be a silent no-op in that state).
    pub fn bulk_decide(&mut self, decision: Decision) {
        for d in &mut self.decisions {
            *d = decision;
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

    /// Number of hunks still awaiting a decision.
    pub fn pending_count(&self) -> usize {
        self.decisions
            .iter()
            .filter(|d| **d == Decision::Pending)
            .count()
    }

    /// Number of hunks that have been accepted or rejected.  The status
    /// bar shows `resolved/total` via this — a progress counter that
    /// climbs from `0/n` to `n/n` as the user works through the review.
    pub fn resolved_count(&self) -> usize {
        self.decisions
            .iter()
            .filter(|d| **d != Decision::Pending)
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
        state.bulk_decide(Decision::Accepted);
        let resolved = state.resolved_rope().unwrap();
        assert_eq!(resolved.to_string(), "a\nB\nc\n");
    }

    #[test]
    fn reject_all_yields_old_rope() {
        let mut state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
        state.bulk_decide(Decision::Rejected);
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
        state.bulk_decide(Decision::Rejected);
        let resolved = state.resolved_rope().unwrap();
        assert_eq!(resolved.to_string(), old);
    }

    #[test]
    fn pending_count_tracks_decisions() {
        let mut state = DiffState::new("a\nb\n", "a\nB\n").unwrap();
        assert_eq!(state.pending_count(), 1);
        state.decide_focused(Decision::Accepted);
        assert_eq!(state.pending_count(), 0);
        assert!(state.all_resolved());
    }

    #[test]
    fn reset_focused_undecides_and_noops_when_pending() {
        let mut state = DiffState::new("a\nb\n", "a\nB\n").unwrap();
        // Fresh hunk is Pending → reset is a no-op.
        assert_eq!(state.focused_decision(), Some(Decision::Pending));
        assert!(
            !state.reset_focused(),
            "resetting a Pending hunk is a no-op"
        );

        // Decide it, then reset back to Pending.
        state.decide_focused(Decision::Accepted);
        assert_eq!(state.focused_decision(), Some(Decision::Accepted));
        assert!(state.reset_focused(), "resetting a decided hunk clears it");
        assert_eq!(state.focused_decision(), Some(Decision::Pending));
        assert_eq!(state.pending_count(), state.hunks.len());

        // And reset is once again a no-op.
        assert!(!state.reset_focused());
    }

    #[test]
    fn resolved_count_climbs_as_hunks_are_decided() {
        // The status-bar chip reads `resolved/total`, so resolved_count
        // must start at 0 and climb to the total as decisions land.
        let mut state = DiffState::new("a\nb\n", "a\nB\n").unwrap();
        assert_eq!(state.resolved_count(), 0);
        state.decide_focused(Decision::Rejected);
        assert_eq!(state.resolved_count(), state.hunks.len());
    }
}
