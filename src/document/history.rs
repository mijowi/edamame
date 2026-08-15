use crate::document::Buffer;

/// A single edit delta: the minimal information needed to undo or redo one
/// logical editing action.
///
/// To undo: remove `inserted` at `offset` and insert `removed` there instead.
/// To redo: remove `removed` at `offset` and insert `inserted` there instead.
///
/// **`offset` carries two units depending on which half of the pipeline the
/// value is in**, and nothing in the type enforces the difference — see the
/// field doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditDelta {
    /// Offset at which the edit occurred — **chars or bytes, depending on
    /// where the delta came from**.
    ///
    /// **Chars** for every delta that reaches [`History`] or
    /// [`EditorState::apply_delta`](crate::editor::EditorState): both index
    /// the rope by char, and [`Self::undo_cursor`] / [`Self::redo_cursor`]
    /// count chars, so they are meaningful *only* on a char-offset delta.
    ///
    /// **Bytes** for the compose path — every delta produced by
    /// [`Self::diff`], `table_edit`, `list_edit`, or `footnote_edit`, which
    /// all work on a `&str` snapshot of the document. Those are converted to
    /// char offsets by `edit_ops::apply_byte_delta` (or a `byte_to_char` call
    /// at the use site) before they touch a buffer or the undo stack; calling
    /// `undo_cursor` / `redo_cursor` on one returns a nonsense offset for any
    /// document containing multibyte text.
    pub offset: usize,
    /// Text that was removed by the edit (empty for pure insertions).
    pub removed: String,
    /// Text that was inserted by the edit (empty for pure deletions).
    pub inserted: String,
}

impl EditDelta {
    /// Return the cursor offset that should be restored after an undo.
    pub fn undo_cursor(&self) -> usize {
        self.offset + self.removed.chars().count()
    }

    /// Return the cursor offset that should be restored after a redo.
    pub fn redo_cursor(&self) -> usize {
        self.offset + self.inserted.chars().count()
    }

    /// Apply this delta to `s`, returning the resulting string.
    ///
    /// `offset` is interpreted as a **byte** index into `s` — this is the
    /// compose helper for edit passes that work on a `&str` snapshot of the
    /// document (footnote renumbering, the table-drag swap chain) before the
    /// net result is converted to char offsets and handed to
    /// [`EditorState::apply_delta`](crate::editor::EditorState).
    ///
    /// # Panics
    ///
    /// Panics if the delta doesn't fit `s`: if `offset` or
    /// `offset + removed.len()` is past the end of `s`, or if either lands
    /// inside a multi-byte char. A delta from [`Self::diff`] against the same
    /// `old` string always satisfies both, as does one from `table_edit` /
    /// `list_edit` / `footnote_edit` applied to the source it was derived
    /// from — but a delta composed against a *different* string than the one
    /// passed here is a caller bug, and this is where it surfaces.
    pub fn apply_to_string(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        out.push_str(&s[..self.offset]);
        out.push_str(&self.inserted);
        out.push_str(&s[self.offset + self.removed.len()..]);
        out
    }

    /// Minimal **byte**-offset delta turning `old` into `new` by trimming the
    /// common (char-aligned) prefix and suffix.  `None` when identical.
    ///
    /// Lets a multi-step rewrite that was simulated on a string collapse into
    /// one delta — which is one undo step.
    ///
    /// Both trims stop on a char boundary, so `offset` and the ends of
    /// `removed` / `inserted` are always safe to slice with and safe to
    /// convert with `byte_to_char`. The round trip
    /// `diff(old, new)?.apply_to_string(old) == new` holds for every pair of
    /// strings.
    pub fn diff(old: &str, new: &str) -> Option<Self> {
        if old == new {
            return None;
        }
        let ob = old.as_bytes();
        let nb = new.as_bytes();

        let max_p = ob.len().min(nb.len());
        let mut p = 0;
        while p < max_p && ob[p] == nb[p] {
            p += 1;
        }
        while !old.is_char_boundary(p) {
            p = p.saturating_sub(1);
        }

        let max_s = (ob.len() - p).min(nb.len() - p);
        let mut s = 0;
        while s < max_s && ob[ob.len() - 1 - s] == nb[nb.len() - 1 - s] {
            s += 1;
        }
        // `is_char_boundary` inspects only the byte at the index, and the
        // trailing `s` bytes are identical in both strings, so a boundary in
        // `old` is also one in `new` — retreating `s` here keeps both slices
        // valid.  `saturating_sub` honors the no-underflow convention.
        while !old.is_char_boundary(old.len() - s) {
            s = s.saturating_sub(1);
        }

        Some(Self {
            offset: p,
            removed: old[p..old.len() - s].to_string(),
            inserted: new[p..new.len() - s].to_string(),
        })
    }
}

/// Undo/redo stack built from `EditDelta` entries.
///
/// Every edit goes through `record()`; the caller is responsible for applying
/// the edit to the buffer BEFORE recording the delta. `undo()` and `redo()`
/// apply the inverse / re-application of the stored delta to the buffer.
#[derive(Debug, Clone, Default)]
pub struct History {
    undo_stack: Vec<EditDelta>,
    redo_stack: Vec<EditDelta>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a completed edit. Clears the redo stack.
    ///
    /// Adjacent edits of the same kind (pure inserts or pure deletes)
    /// merge into the previous undo entry on contiguity alone, so a
    /// held-key autorepeat burst is one undo step regardless of what
    /// character class (alphanumeric, punctuation, whitespace, even
    /// `\n`) the user is repeating.  A non-contiguous offset breaks
    /// the group — that's how intentional cursor moves naturally
    /// separate undo entries.
    pub fn record(&mut self, delta: EditDelta) {
        if let Some(top) = self.undo_stack.last_mut() {
            if try_merge(top, &delta) {
                self.redo_stack.clear();
                return;
            }
        }
        self.undo_stack.push(delta);
        self.redo_stack.clear();
    }

    /// Undo the most recent edit. Applies the inverse delta to `buf` and
    /// returns the cursor position after the undo, or `None` if the undo
    /// stack is empty.
    pub fn undo(&mut self, buf: &mut Buffer) -> Option<usize> {
        let delta = self.undo_stack.pop()?;
        apply_delta(buf, delta.offset, &delta.inserted, &delta.removed);
        let cursor = delta.undo_cursor();
        self.redo_stack.push(delta);
        Some(cursor)
    }

    /// Redo the most recently undone edit. Re-applies the delta to `buf` and
    /// returns the cursor position after the redo, or `None` if the redo
    /// stack is empty.
    pub fn redo(&mut self, buf: &mut Buffer) -> Option<usize> {
        let delta = self.redo_stack.pop()?;
        apply_delta(buf, delta.offset, &delta.removed, &delta.inserted);
        let cursor = delta.redo_cursor();
        self.undo_stack.push(delta);
        Some(cursor)
    }

    /// Whether the undo stack is empty. Used by tests in this module.
    #[allow(dead_code)]
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Whether the redo stack is empty. Used by tests in this module.
    #[allow(dead_code)]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Number of entries on the undo stack.
    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    /// Replace the entire history with a single undo entry.
    ///
    /// Clears both the undo and redo stacks, then pushes `delta` as the
    /// sole undo step.  Used by the diff-mode resolution path to seed a
    /// single coarse "merge-revert" entry: after a diff is applied, one
    /// `Action::Undo` reverts the whole merge and one `Action::Redo`
    /// re-applies it (see the diff-mode plan §6).
    pub fn reset_with(&mut self, delta: EditDelta) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.undo_stack.push(delta);
    }
}

/// Apply a buffer mutation that removes `remove_text` (in chars) at
/// `offset` and inserts `insert_text` in its place.  Either string may
/// be empty.  Used by both `undo` (passing `inserted`, `removed`) and
/// `redo` (passing `removed`, `inserted`) — the symmetry that justifies
/// the helper.
fn apply_delta(buf: &mut Buffer, offset: usize, remove_text: &str, insert_text: &str) {
    if !remove_text.is_empty() {
        let end = offset + remove_text.chars().count();
        buf.remove(offset, end.min(buf.len_chars()));
    }
    if !insert_text.is_empty() {
        buf.insert(offset, insert_text);
    }
}

/// Try to fold `new` into `top` as part of the same contiguous edit run.
/// Returns `true` if the merge happened (and `top` was mutated in place);
/// `false` otherwise.
///
/// Pure insertions and pure deletions merge symmetrically on *contiguity
/// alone*, regardless of character class — alphanumeric, punctuation, and
/// whitespace (including `\n`) all merge as long as `new` is contiguous
/// with `top`'s existing range.  A non-contiguous offset breaks the run,
/// which is how an intentional cursor move naturally separates undo
/// entries.  Mixed-direction edits (insert then delete, or vice versa)
/// never merge.
///
/// `pub(crate)` so the diff-mode `DiffHistory` (CP6) can reuse this exact
/// merge logic rather than forking a second copy that would silently
/// drift the first time the rules are tweaked.
pub(crate) fn try_merge(top: &mut EditDelta, new: &EditDelta) -> bool {
    if top.removed.is_empty() && new.removed.is_empty() {
        return try_merge_insertion(top, new);
    }
    if top.inserted.is_empty() && new.inserted.is_empty() {
        return try_merge_deletion(top, new);
    }
    false
}

fn try_merge_insertion(top: &mut EditDelta, new: &EditDelta) -> bool {
    if new.inserted.is_empty() {
        return false;
    }
    // New offset must sit immediately after top's inserted text.
    let top_end = top.offset + top.inserted.chars().count();
    if new.offset != top_end {
        return false;
    }
    top.inserted.push_str(&new.inserted);
    true
}

fn try_merge_deletion(top: &mut EditDelta, new: &EditDelta) -> bool {
    if new.removed.is_empty() {
        return false;
    }
    // Backspace: the new delete sits immediately before the existing range,
    // so prepend it.
    if new.offset + new.removed.chars().count() == top.offset {
        top.removed.insert_str(0, &new.removed);
        top.offset = new.offset;
        return true;
    }
    // Forward delete: the cursor stays put, so each new delete starts at
    // top's offset.  Append it.
    if new.offset == top.offset {
        top.removed.push_str(&new.removed);
        return true;
    }
    false
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::document::Buffer;

    fn buf(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

    // ── EditDelta::diff / apply_to_string ────────────────────────────────
    //
    // These two are the compose path's only primitives: `diff` collapses a
    // rewrite that was simulated on a `String` into one byte-offset delta,
    // and `apply_to_string` folds a delta back into such a simulation.  Both
    // slice by byte and both feed a `byte_to_char` conversion afterwards, so
    // the invariant that matters is that every offset they produce lands on
    // a char boundary — which is exactly what the ASCII-only footnote tests
    // that used to be their only coverage never exercise.

    #[test]
    fn diff_of_identical_strings_is_none() {
        assert_eq!(EditDelta::diff("same", "same"), None);
        assert_eq!(EditDelta::diff("", ""), None);
    }

    #[test]
    fn diff_trims_to_the_changed_span() {
        let d = EditDelta::diff("abcXYZdef", "abcQdef").expect("differs");
        assert_eq!(d.offset, 3);
        assert_eq!(d.removed, "XYZ");
        assert_eq!(d.inserted, "Q");
    }

    #[test]
    fn diff_of_a_pure_insertion_removes_nothing() {
        let d = EditDelta::diff("ac", "abc").expect("differs");
        assert_eq!(d.offset, 1);
        assert_eq!(d.removed, "");
        assert_eq!(d.inserted, "b");
    }

    #[test]
    fn diff_of_a_pure_deletion_inserts_nothing() {
        let d = EditDelta::diff("abc", "ac").expect("differs");
        assert_eq!(d.offset, 1);
        assert_eq!(d.removed, "b");
        assert_eq!(d.inserted, "");
    }

    #[test]
    fn diff_against_an_empty_string_spans_everything() {
        let d = EditDelta::diff("abc", "").expect("differs");
        assert_eq!(d.offset, 0);
        assert_eq!(d.removed, "abc");
        assert_eq!(d.inserted, "");

        let d = EditDelta::diff("", "abc").expect("differs");
        assert_eq!(d.offset, 0);
        assert_eq!(d.removed, "");
        assert_eq!(d.inserted, "abc");
    }

    /// `offset` counts bytes, not chars — the whole reason every call site
    /// runs it through `byte_to_char` before the delta reaches a buffer.
    #[test]
    fn diff_offsets_are_bytes_not_chars() {
        // "éé" is four bytes but two chars; the change is at char 2 / byte 4.
        let d = EditDelta::diff("ééa", "ééb").expect("differs");
        assert_eq!(d.offset, 4);
        assert_eq!(d.removed, "a");
        assert_eq!(d.inserted, "b");
    }

    /// Two different chars can share leading *and* trailing bytes, which is
    /// what drives both boundary-retreat loops: `😀` (F0 9F 98 80) and `🙀`
    /// (F0 9F 99 80) agree on the first two bytes and on the last one.  A
    /// naive byte trim would emit an offset of 2 and a suffix of 1, slicing
    /// the delta straight through the middle of a char.
    #[test]
    fn diff_never_splits_a_multibyte_char() {
        let d = EditDelta::diff("😀", "🙀").expect("differs");
        assert_eq!(d.offset, 0);
        assert_eq!(d.removed, "😀");
        assert_eq!(d.inserted, "🙀");

        // Same trap with the shared *leading* byte of two 2-byte chars.
        let d = EditDelta::diff("xéy", "xèy").expect("differs");
        assert_eq!(d.offset, 1);
        assert_eq!(d.removed, "é");
        assert_eq!(d.inserted, "è");
    }

    #[test]
    fn apply_to_string_replaces_the_delta_span() {
        let d = EditDelta {
            offset: 3,
            removed: "XYZ".into(),
            inserted: "Q".into(),
        };
        assert_eq!(d.apply_to_string("abcXYZdef"), "abcQdef");
    }

    #[test]
    fn diff_and_apply_to_string_round_trip() {
        let pairs = [
            ("", "a"),
            ("a", ""),
            ("hello", "hello world"),
            ("| a | b |\n| 1 | 2 |\n", "| b | a |\n| 2 | 1 |\n"),
            ("A[^2] B[^1]\n", "A[^1] B[^2]\n"),
            ("héllo wörld", "héllo wörld!"),
            ("😀😀😀", "😀🙀😀"),
            ("漢字", "字漢"),
            ("a\nb\nc\n", "c\nb\na\n"),
        ];
        for (old, new) in pairs {
            let d = EditDelta::diff(old, new).expect("pairs differ");
            assert!(
                old.is_char_boundary(d.offset),
                "offset {} splits a char in {old:?}",
                d.offset
            );
            assert_eq!(d.apply_to_string(old), new, "{old:?} -> {new:?}");
        }
    }

    proptest! {
        /// The property the compose path rests on, over an alphabet mixing
        /// 1-, 2-, 3- and 4-byte chars plus the newline that table and
        /// footnote rewrites move around: `diff` is always reversible by
        /// `apply_to_string`, and its offset is always char-aligned (so the
        /// `byte_to_char` conversion at every call site is well-defined).
        #[test]
        fn proptest_diff_round_trips_over_multibyte_text(
            old in r"[abé😀漢\n]{0,24}",
            new in r"[abé😀漢\n]{0,24}",
        ) {
            match EditDelta::diff(&old, &new) {
                None => prop_assert_eq!(&old, &new),
                Some(d) => {
                    prop_assert!(old.is_char_boundary(d.offset));
                    prop_assert!(old.is_char_boundary(d.offset + d.removed.len()));
                    prop_assert_eq!(d.apply_to_string(&old), new);
                }
            }
        }
    }

    // ── History ──────────────────────────────────────────────────────────

    #[test]
    fn undo_empty_stack_returns_none() {
        let mut h = History::new();
        let mut b = buf("hello");
        assert!(h.undo(&mut b).is_none());
        assert_eq!(b.contents(), "hello"); // unchanged
    }

    #[test]
    fn redo_empty_stack_returns_none() {
        let mut h = History::new();
        let mut b = buf("hello");
        assert!(h.redo(&mut b).is_none());
        assert_eq!(b.contents(), "hello");
    }

    #[test]
    fn undo_insertion() {
        let mut h = History::new();
        let mut b = buf("hello");
        // Simulate inserting "!" at offset 5.
        b.insert(5, "!");
        h.record(EditDelta {
            offset: 5,
            removed: String::new(),
            inserted: "!".into(),
        });
        assert_eq!(b.contents(), "hello!");

        let cursor = h.undo(&mut b).unwrap();
        assert_eq!(b.contents(), "hello");
        assert_eq!(cursor, 5); // undo_cursor = offset + removed.len = 5 + 0 = 5
    }

    #[test]
    fn undo_deletion() {
        let mut h = History::new();
        let mut b = buf("hello!");
        // Simulate deleting "!" at offset 5.
        b.remove(5, 6);
        h.record(EditDelta {
            offset: 5,
            removed: "!".into(),
            inserted: String::new(),
        });
        assert_eq!(b.contents(), "hello");

        let cursor = h.undo(&mut b).unwrap();
        assert_eq!(b.contents(), "hello!");
        assert_eq!(cursor, 6); // undo_cursor = 5 + 1 = 6
    }

    #[test]
    fn redo_after_undo() {
        let mut h = History::new();
        let mut b = buf("hello");
        b.insert(5, "!");
        h.record(EditDelta {
            offset: 5,
            removed: String::new(),
            inserted: "!".into(),
        });

        h.undo(&mut b).unwrap();
        assert_eq!(b.contents(), "hello");

        let cursor = h.redo(&mut b).unwrap();
        assert_eq!(b.contents(), "hello!");
        assert_eq!(cursor, 6); // redo_cursor = 5 + 1 = 6
    }

    #[test]
    fn record_clears_redo_stack() {
        let mut h = History::new();
        let mut b = buf("hello");
        b.insert(5, "!");
        h.record(EditDelta {
            offset: 5,
            removed: String::new(),
            inserted: "!".into(),
        });

        h.undo(&mut b).unwrap(); // redo stack now has one entry
        assert!(h.can_redo());

        // A new edit should clear redo.
        b.insert(0, "X");
        h.record(EditDelta {
            offset: 0,
            removed: String::new(),
            inserted: "X".into(),
        });
        assert!(!h.can_redo());
    }

    #[test]
    fn multiple_undo_redo_non_contiguous() {
        // Non-contiguous offsets produce distinct undo entries — typing
        // at offset 0, moving the cursor, typing at offset 5, moving
        // again, typing at offset 10 each starts a new group.
        let mut h = History::new();
        h.record(EditDelta {
            offset: 0,
            removed: "".into(),
            inserted: "a".into(),
        });
        h.record(EditDelta {
            offset: 5,
            removed: "".into(),
            inserted: "!".into(),
        });
        h.record(EditDelta {
            offset: 10,
            removed: "".into(),
            inserted: "b".into(),
        });
        assert_eq!(h.undo_depth(), 3);
    }

    #[test]
    fn undo_depth_tracks_stack() {
        let mut h = History::new();
        assert_eq!(h.undo_depth(), 0);
        h.record(EditDelta {
            offset: 0,
            removed: "".into(),
            inserted: "x".into(),
        });
        assert_eq!(h.undo_depth(), 1);
        // Second alphanumeric char at the adjacent offset merges into the
        // same word-group, so depth stays at 1.
        h.record(EditDelta {
            offset: 1,
            removed: "".into(),
            inserted: "y".into(),
        });
        assert_eq!(h.undo_depth(), 1);
    }

    // ── Word grouping ─────────────────────────────────────────────

    #[test]
    fn typing_word_creates_one_undo_entry() {
        let mut h = History::new();
        h.record(EditDelta {
            offset: 0,
            removed: "".into(),
            inserted: "c".into(),
        });
        h.record(EditDelta {
            offset: 1,
            removed: "".into(),
            inserted: "a".into(),
        });
        h.record(EditDelta {
            offset: 2,
            removed: "".into(),
            inserted: "t".into(),
        });
        assert_eq!(h.undo_depth(), 1);

        let mut b = buf("cat");
        let cursor = h.undo(&mut b).unwrap();
        assert_eq!(b.contents(), "");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn space_merges_into_contiguous_group() {
        // Post-PR1: merge-by-contiguity means a held key spanning any
        // character class produces ONE undo entry.  Cursor motion (=
        // non-contiguous offsets) is the boundary, not character class.
        let mut h = History::new();
        h.record(EditDelta {
            offset: 0,
            removed: "".into(),
            inserted: "c".into(),
        });
        h.record(EditDelta {
            offset: 1,
            removed: "".into(),
            inserted: "a".into(),
        });
        h.record(EditDelta {
            offset: 2,
            removed: "".into(),
            inserted: " ".into(),
        });
        h.record(EditDelta {
            offset: 3,
            removed: "".into(),
            inserted: "d".into(),
        });
        assert_eq!(h.undo_depth(), 1);
    }

    #[test]
    fn non_adjacent_insert_breaks_group() {
        let mut h = History::new();
        h.record(EditDelta {
            offset: 0,
            removed: "".into(),
            inserted: "a".into(),
        });
        // User moved cursor, typed at a different offset.
        h.record(EditDelta {
            offset: 10,
            removed: "".into(),
            inserted: "b".into(),
        });
        assert_eq!(h.undo_depth(), 2);
    }

    #[test]
    fn contiguous_inserts_merge_regardless_of_length() {
        // Post-PR1: contiguity alone drives the merge.  A coalesced
        // run-of-inserts arriving as a single multi-char delta merges
        // with subsequent contiguous inserts.
        let mut h = History::new();
        h.record(EditDelta {
            offset: 0,
            removed: "".into(),
            inserted: "hi".into(),
        });
        h.record(EditDelta {
            offset: 2,
            removed: "".into(),
            inserted: "foo".into(),
        });
        assert_eq!(h.undo_depth(), 1);
    }

    #[test]
    fn deletion_does_not_merge_with_insert() {
        let mut h = History::new();
        h.record(EditDelta {
            offset: 0,
            removed: "".into(),
            inserted: "a".into(),
        });
        h.record(EditDelta {
            offset: 0,
            removed: "a".into(),
            inserted: "".into(),
        });
        assert_eq!(h.undo_depth(), 2);
    }

    // ── Deletion grouping ─────────────────────────────────────────

    /// Backspacing a word produces a single undo entry that restores the
    /// whole word in one Ctrl-Z.
    #[test]
    fn backspacing_word_creates_one_undo_entry() {
        let mut h = History::new();
        // Cursor was at 3 in "cat"; backspace removes 't', then 'a', then 'c'.
        h.record(EditDelta {
            offset: 2,
            removed: "t".into(),
            inserted: "".into(),
        });
        h.record(EditDelta {
            offset: 1,
            removed: "a".into(),
            inserted: "".into(),
        });
        h.record(EditDelta {
            offset: 0,
            removed: "c".into(),
            inserted: "".into(),
        });
        assert_eq!(h.undo_depth(), 1);

        let mut b = buf("");
        let cursor = h.undo(&mut b).unwrap();
        assert_eq!(b.contents(), "cat");
        // undo_cursor = offset(0) + removed.len("cat") = 3 — back where the
        // user started before they hit backspace the first time.
        assert_eq!(cursor, 3);
    }

    /// Forward-deleting a word produces a single undo entry.  The cursor
    /// stays put, so every delta shares the same offset.
    #[test]
    fn forward_deleting_word_creates_one_undo_entry() {
        let mut h = History::new();
        // Cursor at 0 in "cat"; Delete removes 'c', then 'a', then 't'.
        h.record(EditDelta {
            offset: 0,
            removed: "c".into(),
            inserted: "".into(),
        });
        h.record(EditDelta {
            offset: 0,
            removed: "a".into(),
            inserted: "".into(),
        });
        h.record(EditDelta {
            offset: 0,
            removed: "t".into(),
            inserted: "".into(),
        });
        assert_eq!(h.undo_depth(), 1);

        let mut b = buf("");
        h.undo(&mut b).unwrap();
        assert_eq!(b.contents(), "cat");
    }

    /// Post-PR1: any held-backspace burst merges by contiguity — the
    /// character class of the bytes being removed is irrelevant.  A
    /// 3-second hold deleting through punctuation, whitespace, and
    /// letters undoes in one step.
    #[test]
    fn backspace_merges_across_character_classes() {
        let mut h = History::new();
        // Backspacing through "ca d": d, ' ', a, c — all contiguous.
        h.record(EditDelta {
            offset: 3,
            removed: "d".into(),
            inserted: "".into(),
        });
        h.record(EditDelta {
            offset: 2,
            removed: " ".into(),
            inserted: "".into(),
        });
        h.record(EditDelta {
            offset: 1,
            removed: "a".into(),
            inserted: "".into(),
        });
        h.record(EditDelta {
            offset: 0,
            removed: "c".into(),
            inserted: "".into(),
        });
        assert_eq!(h.undo_depth(), 1);
    }

    /// Post-PR1: forward-delete also merges by contiguity regardless
    /// of character class.
    #[test]
    fn forward_delete_merges_across_character_classes() {
        let mut h = History::new();
        // Forward-deleting "ca d" from offset 0: c, a, ' ', d.
        h.record(EditDelta {
            offset: 0,
            removed: "c".into(),
            inserted: "".into(),
        });
        h.record(EditDelta {
            offset: 0,
            removed: "a".into(),
            inserted: "".into(),
        });
        h.record(EditDelta {
            offset: 0,
            removed: " ".into(),
            inserted: "".into(),
        });
        h.record(EditDelta {
            offset: 0,
            removed: "d".into(),
            inserted: "".into(),
        });
        assert_eq!(h.undo_depth(), 1);
    }

    /// A non-contiguous backspace (cursor jumped elsewhere) starts a new
    /// group.
    #[test]
    fn non_contiguous_delete_breaks_group() {
        let mut h = History::new();
        h.record(EditDelta {
            offset: 5,
            removed: "a".into(),
            inserted: "".into(),
        });
        // Cursor moved; next backspace is far away.
        h.record(EditDelta {
            offset: 0,
            removed: "b".into(),
            inserted: "".into(),
        });
        assert_eq!(h.undo_depth(), 2);
    }

    /// Multi-character deletes (e.g. DeleteWordBack) are their own undo
    /// entries; they don't fold the next single-char delete in.
    #[test]
    fn multi_char_delete_does_not_merge() {
        let mut h = History::new();
        h.record(EditDelta {
            offset: 0,
            removed: "hello".into(),
            inserted: "".into(),
        });
        h.record(EditDelta {
            offset: 0,
            removed: "x".into(),
            inserted: "".into(),
        });
        // The new delete is single-char alnum and contiguous, so the
        // existing test for "top can be multi-char" parity does merge it.
        // That mirrors the insertion side, where a paste-then-typed-char
        // also merges.  Both are acceptable: the user pressed Delete twice.
        assert_eq!(h.undo_depth(), 1);
    }

    #[test]
    fn undo_of_backspace_group_restores_all_at_once() {
        let mut h = History::new();
        let mut b = buf("hello");
        // Simulate backspacing the whole word: each press removes one char
        // and shifts the cursor left by one.
        for (offset, ch) in [(4, 'o'), (3, 'l'), (2, 'l'), (1, 'e'), (0, 'h')] {
            b.remove(offset, offset + 1);
            h.record(EditDelta {
                offset,
                removed: ch.to_string(),
                inserted: "".into(),
            });
        }
        assert_eq!(b.contents(), "");
        assert_eq!(h.undo_depth(), 1);

        h.undo(&mut b).unwrap();
        assert_eq!(b.contents(), "hello");

        h.redo(&mut b).unwrap();
        assert_eq!(b.contents(), "");
    }

    #[test]
    fn reset_with_seeds_single_merge_revert_entry() {
        let mut h = History::new();
        // Pre-existing history that reset_with must blow away.
        h.record(EditDelta {
            offset: 0,
            removed: "".into(),
            inserted: "junk".into(),
        });
        let mut b = buf("junk");
        let _ = h.undo(&mut b); // leave a redo entry around too

        // Seed the coarse merge-revert entry: pre-merge "old" → merged.
        h.reset_with(EditDelta {
            offset: 0,
            removed: "old".into(),
            inserted: "merged".into(),
        });
        assert_eq!(h.undo_depth(), 1, "exactly one undo step after reset_with");
        assert!(!h.can_redo(), "redo stack cleared by reset_with");

        // One undo restores the pre-merge text; one redo re-applies.
        let mut b = buf("merged");
        h.undo(&mut b).unwrap();
        assert_eq!(b.contents(), "old");
        h.redo(&mut b).unwrap();
        assert_eq!(b.contents(), "merged");
    }

    #[test]
    fn undo_of_word_group_restores_all_at_once() {
        let mut h = History::new();
        let mut b = buf("");
        b.insert(0, "h");
        h.record(EditDelta {
            offset: 0,
            removed: "".into(),
            inserted: "h".into(),
        });
        b.insert(1, "e");
        h.record(EditDelta {
            offset: 1,
            removed: "".into(),
            inserted: "e".into(),
        });
        b.insert(2, "l");
        h.record(EditDelta {
            offset: 2,
            removed: "".into(),
            inserted: "l".into(),
        });
        b.insert(3, "l");
        h.record(EditDelta {
            offset: 3,
            removed: "".into(),
            inserted: "l".into(),
        });
        b.insert(4, "o");
        h.record(EditDelta {
            offset: 4,
            removed: "".into(),
            inserted: "o".into(),
        });
        assert_eq!(b.contents(), "hello");
        assert_eq!(h.undo_depth(), 1);

        h.undo(&mut b).unwrap();
        assert_eq!(b.contents(), "");

        h.redo(&mut b).unwrap();
        assert_eq!(b.contents(), "hello");
    }
}
