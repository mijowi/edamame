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

use std::cell::{Cell, RefCell};

use ropey::Rope;

use crate::document::{Buffer, Cursor, ParsedDoc};

use super::engine::{
    compute, hunk_new_side_text, match_by_old_overlap, pending_decisions, HunkIdAllocator,
};
use super::hunk::{Decision, Hunk, HunkId};
use super::layout::DiffLayoutCache;

/// Outcome of [`DiffState::reconcile_with_disk`] (§11b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// Hunks remain; the user is still reviewing.  `reset` counts hunks
    /// whose decision was dropped back to `Pending` because their
    /// new-side target changed (drives the flash wording).
    StillReviewing { reset: usize },
    /// The recompute yielded no hunks (every change reverted, so the
    /// new disk contents match `old_rope`).  The caller exits diff mode.
    NoChangesRemain,
}

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
    /// previously-issued ones.  Used by
    /// [`Self::reconcile_with_disk`] when a mid-review external write
    /// is folded in (§11b); id stability across the recompute is then
    /// achieved by old-side overlap matching.
    pub(crate) ids: HunkIdAllocator,
    /// True when at least one table couldn't be row-diffed because its
    /// rows had uneven cell counts, so its change is surfaced as the
    /// coarser line-level hunk(s) instead of per-row hunks (§3a).
    /// `App::enter_diff_mode` flashes a hint on entry so the user
    /// understands why that table isn't reviewable row-by-row.
    pub uneven_table_fallback: bool,
    /// Rendered parse of the *new side*, when the review is showing
    /// unchanged regions as rendered Markdown.
    ///
    /// `None` means "render every line raw" — the pre-CP5b behavior,
    /// still reachable and still correct.  Every `DiffState::new` starts
    /// there, and a review is queried in that state at least once per
    /// entry: `EditorState::enter_diff_mode` defers the build by a frame
    /// (see `diff_parse_dirty`) and `App::compute_doc_dims` asks for the
    /// row total before `prepare_viewport` flushes it.
    /// [`Self::reconcile_with_disk`] returns to it deliberately, because
    /// the parse it holds was built from the `new_buffer` that call
    /// replaces — there is no stamp pairing the two, so the pairing is
    /// kept true by dropping the parse at the one site that can break
    /// it.
    /// The parse is built and installed by
    /// [`crate::editor::EditorState::refresh_diff_parse`]: `DiffState`
    /// deliberately holds no theme / width of its own, so ownership of
    /// *when* to rebuild stays with the state that already tracks both.
    pub parsed_new: Option<ParsedDoc>,
    /// Lazily-built flat visual-line list + per-width row-count cache
    /// (see [`super::layout`]).  Interior-mutable so the immutable
    /// render / scroll-query paths can populate it on first use.
    pub(crate) layout: RefCell<DiffLayoutCache>,
    /// Monotonic counter bumped by [`Self::invalidate_layout`] and
    /// [`Self::set_rendered_parse`] — the diff's analogue of
    /// `ParsedDoc::parsed_version`, and the cache key for the diff-side
    /// image-snapshot geometry (`ui::image_view::build_diff_snapshots_cached`).
    ///
    /// A `Cell` rather than a plain field because `invalidate_layout`
    /// takes `&self` (the layout cache behind it is interior-mutable for
    /// the same reason); every mutation of the line set has to bump this,
    /// so it has to be reachable from the immutable path too.
    layout_version: Cell<u64>,
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
            parsed_new: None,
            layout: RefCell::new(DiffLayoutCache::default()),
            layout_version: Cell::new(0),
        })
    }

    /// Install (or drop) the rendered new-side parse.  Always
    /// invalidates the layout: the visual-line *set* depends on the
    /// parse (which blocks are clean, and how many rendered rows each
    /// one contributes), not just on the hunk list.
    ///
    /// Takes an `Option` so the raw state stays expressible after
    /// construction, which [`Self::reconcile_with_disk`] relies on: it
    /// replaces `new_buffer` wholesale and drops the parse built from
    /// the old one, leaving the review raw until its caller reinstalls
    /// a fresh parse.  The tests that pin the raw layout use the same
    /// door.
    pub fn set_rendered_parse(&mut self, parsed: Option<ParsedDoc>) {
        self.parsed_new = parsed;
        self.invalidate_layout();
    }

    /// Current layout version — see [`Self::layout_version`].
    pub(crate) fn layout_version(&self) -> u64 {
        self.layout_version.get()
    }

    /// Advance the layout version.  Called from
    /// [`Self::invalidate_layout`], which is the single funnel every
    /// line-set change already goes through.
    pub(crate) fn bump_layout_version(&self) {
        self.layout_version
            .set(self.layout_version.get().wrapping_add(1));
    }

    /// Look up the focused hunk's index in `hunks` (or `None` when
    /// `focused_id` has gone stale — possible after a recompute drops
    /// the prior hunk).
    pub fn focused_idx(&self) -> Option<usize> {
        self.hunks.iter().position(|h| h.id == self.focused_id)
    }

    /// The id of the first still-`Pending` hunk in document order, if
    /// any.  Used by [`Self::reconcile_with_disk`] to land focus on
    /// something needing attention when the previously-focused hunk
    /// vanished in a recompute.
    pub fn first_pending_id(&self) -> Option<HunkId> {
        self.hunks
            .iter()
            .zip(self.decisions.iter())
            .find(|(_, d)| **d == Decision::Pending)
            .map(|(h, _)| h.id)
    }

    /// Fold a fresh on-disk write into the live review **in place**
    /// (§11b), preserving every decision the user already made on hunks
    /// the write did not touch.
    ///
    /// `old_rope` is invariant for the life of the review, so the diff
    /// is recomputed against the *new* disk contents and each new hunk
    /// is matched to a prior one by old-side overlap
    /// ([`match_by_old_overlap`]).  A matched hunk carries its prior
    /// decision forward **only when its new-side text is byte-identical
    /// to what the user already reviewed** — an external write that
    /// changed the new side resets that hunk to `Pending`, because the
    /// user never saw the now-different change.  Hunks that no longer
    /// exist (the write reverted that region to `old_rope`) silently
    /// drop their decisions.
    ///
    /// Returns [`ReconcileOutcome::NoChangesRemain`] when the recompute
    /// yields no hunks (every change reverted) so the caller can exit
    /// diff mode; otherwise [`ReconcileOutcome::StillReviewing`] carrying
    /// the count of hunks reset for re-review.
    ///
    /// **`NoChangesRemain` leaves `self` in a half-cleared, invalid state**
    /// (`hunks` / `decisions` already taken and not restored).  The only
    /// valid response is to drop the `DiffState` / exit diff mode — never
    /// keep reviewing after this outcome.
    pub fn reconcile_with_disk(&mut self, new_disk: &str) -> ReconcileOutcome {
        // Snapshot prior state before we overwrite anything.
        let prior_hunks = std::mem::take(&mut self.hunks);
        let prior_decisions = std::mem::take(&mut self.decisions);
        let prior_new_rope = self.new_buffer.rope().clone();
        let prior_focused = self.focused_id;

        let old = self.old_rope.to_string();
        let computation = compute(&old, new_disk, &mut self.ids);
        let mut hunks = computation.hunks;
        if hunks.is_empty() {
            return ReconcileOutcome::NoChangesRemain;
        }
        let new_rope = Rope::from_str(new_disk);

        let mut decisions = vec![Decision::Pending; hunks.len()];
        let mut reset = 0usize;
        // A prior hunk may be adopted by at most one new hunk: old-side
        // boundaries shift when the new side changes, so a prior spanning
        // several old lines can split into two new hunks that both overlap
        // it.  Without this guard both would inherit the prior's id,
        // breaking the unique-id invariant (and id-based focus nav).  The
        // earliest (document-order) new hunk wins the prior's id and
        // decision; any later claimant keeps its fresh id and stays Pending.
        let mut claimed = vec![false; prior_hunks.len()];
        for (i, h) in hunks.iter_mut().enumerate() {
            // §6 rule-2 overlap match against the prior hunk list.
            let Some(p) = match_by_old_overlap(h, &prior_hunks) else {
                continue;
            };
            if claimed[p] {
                continue; // prior already adopted → keep this hunk's fresh id, Pending
            }
            claimed[p] = true;
            h.id = prior_hunks[p].id; // inherit the stable id
            let same_new = hunk_new_side_text(h, &new_rope)
                == hunk_new_side_text(&prior_hunks[p], &prior_new_rope);
            if same_new {
                decisions[i] = prior_decisions[p]; // carry the decision
            } else if prior_decisions[p] != Decision::Pending {
                reset += 1; // changed target → re-review
            }
        }

        self.new_buffer.set_rope(new_rope);
        self.hunks = hunks;
        self.decisions = decisions;
        self.uneven_table_fallback = computation.uneven_table_fallback;

        // Keep focus if it survived; else the first pending hunk; else
        // the first hunk.
        self.focused_id = if self.hunks.iter().any(|h| h.id == prior_focused) {
            prior_focused
        } else {
            self.first_pending_id().unwrap_or(self.hunks[0].id)
        };

        // The external reshape invalidates the cached layout, *and* the
        // rendered new-side parse with it: `parsed_new` was built from
        // the `new_buffer` we just replaced, and `build_visual_lines_rendered`
        // reads source lines out of the parse while taking the document's
        // line count from the buffer.  Dropping it here is what makes
        // that mismatch unreachable rather than merely unreached —
        // nothing in `DiffState` can rebuild the parse (it holds no
        // theme and no width; see [`Self::parsed_new`]), so the honest
        // move is to fall back to the raw layout and let the caller's
        // `EditorState::refresh_diff_parse` reinstall it.  A caller that
        // forgets loses the rendered presentation for a frame, until
        // `refresh_parsed`'s tail call comes round — never a review
        // partitioned against stale line ranges.  (There is no in-diff
        // undo history to clear in CP5 — decision undo was never
        // implemented; the Edit-text `DiffHistory` arrives in CP6, and
        // `reconcile_with_disk` will then clear it here.)
        self.set_rendered_parse(None);
        ReconcileOutcome::StillReviewing { reset }
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

    // ── Reconcile (§11b) ───────────────────────────────────────────

    /// Find the index of the hunk with the given id, or panic.
    fn idx_of(state: &DiffState, id: HunkId) -> usize {
        state
            .hunks
            .iter()
            .position(|h| h.id == id)
            .expect("hunk id present")
    }

    /// `reconcile_with_disk` replaces `new_buffer`, so the parse built
    /// from the previous one must not survive: nothing pairs the two,
    /// and `build_visual_lines_rendered` reads source lines out of the
    /// parse while taking the line count from the buffer.  Dropping it
    /// makes the review fall back to the raw layout — correct, if
    /// plainer — until the caller reinstalls a fresh parse.
    #[test]
    fn reconcile_drops_the_stale_rendered_parse() {
        let theme: &'static crate::config::Theme =
            Box::leak(Box::new(crate::config::Theme::default()));
        let old = "# Title\n\nAlpha.\n";
        let new1 = "# Title\n\nALPHA.\n";
        let mut state = DiffState::new(old, new1).unwrap();
        state.set_rendered_parse(Some(ParsedDoc::build(new1, theme, true, 20)));
        assert!(state.parsed_new.is_some());

        let outcome = state.reconcile_with_disk("# Title\n\nALPHA!\n");
        assert_eq!(outcome, ReconcileOutcome::StillReviewing { reset: 0 });
        assert!(
            state.parsed_new.is_none(),
            "a parse built from the replaced buffer must not survive"
        );
        // And the layout it feeds is rebuilt raw rather than reused.
        let raw = DiffState::new(old, "# Title\n\nALPHA!\n").unwrap();
        assert_eq!(state.total_visual_rows(80), raw.total_visual_rows(80));
    }

    #[test]
    fn reconcile_preserves_decision_on_unchanged_hunk() {
        // Two Replace hunks (line 1 b→B, line 3 d→D), separated by
        // context line c.  Accept h0, reject h1.
        let old = "a\nb\nc\nd\ne\n";
        let new1 = "a\nB\nc\nD\ne\n";
        let mut state = DiffState::new(old, new1).unwrap();
        assert_eq!(state.hunks.len(), 2);
        let h0_id = state.hunks[0].id;
        let h1_id = state.hunks[1].id;
        state.decisions[0] = Decision::Accepted;
        state.decisions[1] = Decision::Rejected;

        // External write touches only h1's region (D → DD); h0's
        // new-side text is unchanged.
        let new2 = "a\nB\nc\nDD\ne\n";
        let outcome = state.reconcile_with_disk(new2);

        assert_eq!(outcome, ReconcileOutcome::StillReviewing { reset: 1 });
        // h0 keeps its id and its Accepted decision.
        let h0 = idx_of(&state, h0_id);
        assert_eq!(state.decisions[h0], Decision::Accepted);
        // h1 keeps its id but its changed target reset it to Pending.
        let h1 = idx_of(&state, h1_id);
        assert_eq!(state.decisions[h1], Decision::Pending);
    }

    #[test]
    fn reconcile_resets_decision_when_new_side_changes() {
        let mut state = DiffState::new("a\nb\n", "a\nB\n").unwrap();
        state.decisions[0] = Decision::Accepted;
        let outcome = state.reconcile_with_disk("a\nC\n");
        assert_eq!(outcome, ReconcileOutcome::StillReviewing { reset: 1 });
        assert_eq!(state.decisions[0], Decision::Pending);
    }

    #[test]
    fn reconcile_keeps_ids_unique_when_a_prior_hunk_splits() {
        // One multi-line Replace hunk over old lines 1..4 (b,c,d →
        // B,C,D).  Decide it.
        let old = "a\nb\nc\nd\ne\n";
        let new1 = "a\nB\nC\nD\ne\n";
        let mut state = DiffState::new(old, new1).unwrap();
        assert_eq!(state.hunks.len(), 1, "single coalesced replace hunk");
        let prior_id = state.hunks[0].id;
        state.decisions[0] = Decision::Accepted;

        // External write reverts the middle line (c) to its original, so
        // the prior hunk splits into two new hunks (old 1..2 and 3..4),
        // both of which overlap the prior's 1..4 old range.
        // The decided multi-line change reshaped into two smaller hunks
        // whose new-side text differs from what was reviewed, so the
        // decision resets (one reset counted).
        let outcome = state.reconcile_with_disk("a\nB\nc\nD\ne\n");
        assert_eq!(outcome, ReconcileOutcome::StillReviewing { reset: 1 });
        assert_eq!(state.hunks.len(), 2, "prior hunk split in two");

        // The invariant under test: both ids are distinct.  Only the
        // earliest claimant inherits the prior id; the other keeps a
        // freshly-allocated one — without the claimed-prior guard both
        // would inherit `prior_id`.
        assert_ne!(
            state.hunks[0].id, state.hunks[1].id,
            "split hunks must not share an id",
        );
        let inheritors = state.hunks.iter().filter(|h| h.id == prior_id).count();
        assert_eq!(inheritors, 1, "exactly one hunk inherits the prior id");
        assert_eq!(state.hunks[0].id, prior_id, "earliest claimant wins it");
        // Both hunks are Pending: the inheritor reset (changed new-side),
        // the fresh hunk started Pending.
        assert_eq!(state.decisions[0], Decision::Pending);
        assert_eq!(state.decisions[1], Decision::Pending);
    }

    #[test]
    fn reconcile_drops_vanished_hunk() {
        let old = "a\nb\nc\nd\ne\n";
        let new1 = "a\nB\nc\nD\ne\n";
        let mut state = DiffState::new(old, new1).unwrap();
        let h0_id = state.hunks[0].id;
        state.decisions[0] = Decision::Accepted;
        state.decisions[1] = Decision::Rejected;

        // External write reverts h1's region back to `old` (d), so only
        // h0 remains; h1's Rejected decision is silently dropped.
        let new2 = "a\nB\nc\nd\ne\n";
        let outcome = state.reconcile_with_disk(new2);

        assert_eq!(outcome, ReconcileOutcome::StillReviewing { reset: 0 });
        assert_eq!(state.hunks.len(), 1);
        assert_eq!(state.hunks[0].id, h0_id);
        assert_eq!(state.decisions[0], Decision::Accepted);
    }

    #[test]
    fn reconcile_preserves_accepted_insertion() {
        // Regression: a pure Insert hunk (empty old-side range) must keep
        // its decision across an unrelated external write.  An agent adds
        // a block, the user accepts it, then the agent edits elsewhere —
        // the accepted insertion must not silently revert to Pending.
        let old = "a\nb\n";
        let new1 = "a\nNEW\nb\n";
        let mut state = DiffState::new(old, new1).unwrap();
        assert_eq!(state.hunks.len(), 1);
        assert_eq!(state.hunks[0].kind, crate::diff::HunkKind::Insert);
        let ins_id = state.hunks[0].id;
        state.decisions[0] = Decision::Accepted;

        // External write appends a second, unrelated insertion.
        let new2 = "a\nNEW\nb\nEXTRA\n";
        let outcome = state.reconcile_with_disk(new2);

        assert_eq!(outcome, ReconcileOutcome::StillReviewing { reset: 0 });
        assert_eq!(state.hunks.len(), 2);
        // The original insertion kept its id and its Accepted decision.
        let ins = idx_of(&state, ins_id);
        assert_eq!(state.decisions[ins], Decision::Accepted);
        // The freshly-added insertion is Pending.
        let other = 1 - ins;
        assert_eq!(state.decisions[other], Decision::Pending);
    }

    #[test]
    fn reconcile_collapses_to_no_changes() {
        let mut state = DiffState::new("a\nb\n", "a\nB\n").unwrap();
        // Disk reverted to the original buffer — nothing differs.
        let outcome = state.reconcile_with_disk("a\nb\n");
        assert_eq!(outcome, ReconcileOutcome::NoChangesRemain);
    }

    #[test]
    fn reconcile_focus_survives_or_falls_back() {
        let old = "a\nb\nc\nd\ne\n";
        let new1 = "a\nB\nc\nD\ne\n";

        // Focused hunk survives → focus kept.
        let mut state = DiffState::new(old, new1).unwrap();
        let h0_id = state.hunks[0].id;
        assert_eq!(state.focused_id, h0_id);
        state.reconcile_with_disk("a\nB\nc\nDD\ne\n");
        assert_eq!(state.focused_id, h0_id, "surviving focus is kept");

        // Focused hunk vanishes → focus lands on the first pending hunk.
        let mut state = DiffState::new(old, new1).unwrap();
        let h0_id = state.hunks[0].id;
        let h1_id = state.hunks[1].id;
        state.focused_id = h1_id;
        // Revert h1's region; h1 disappears.
        state.reconcile_with_disk("a\nB\nc\nd\ne\n");
        assert_eq!(
            state.focused_id, h0_id,
            "vanished focus falls back to the first pending hunk",
        );
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
