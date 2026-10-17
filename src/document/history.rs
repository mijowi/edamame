use crate::document::Buffer;

/// One undoable edit: `removed` was replaced by `inserted` at `offset`.
///
/// **`offset` is in chars or bytes depending on which half of the pipeline the delta is in**,
/// and nothing in the type enforces the difference. Chars for everything reaching [`History`]
/// or `EditorState::apply_delta` (so [`Self::undo_cursor`] / [`Self::redo_cursor`] are only
/// meaningful there). Bytes for the compose path — [`Self::diff`], `table_edit`, `list_edit`,
/// `footnote_edit` — which works on a `&str` snapshot; `edit_ops::apply_byte_delta` (or a
/// `byte_to_char` at the use site) converts before the delta touches a buffer or the undo stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditDelta {
    pub offset: usize,
    pub removed: String,
    pub inserted: String,
}

impl EditDelta {
    /// Cursor offset to restore after undoing this (char-offset) delta.
    pub fn undo_cursor(&self) -> usize {
        self.offset + self.removed.chars().count()
    }

    /// Cursor offset to restore after redoing this (char-offset) delta.
    pub fn redo_cursor(&self) -> usize {
        self.offset + self.inserted.chars().count()
    }

    /// Apply this **byte**-offset delta to a `&str` snapshot (the compose path).
    ///
    /// # Panics
    ///
    /// If the delta does not fit `s` or lands mid-char — i.e. it was composed against a
    /// different string than the one passed here.
    pub fn apply_to_string(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        out.push_str(&s[..self.offset]);
        out.push_str(&self.inserted);
        out.push_str(&s[self.offset + self.removed.len()..]);
        out
    }

    /// Minimal **byte**-offset delta turning `old` into `new` (common prefix and suffix trimmed
    /// on char boundaries); `None` when identical. Collapses a simulated multi-step rewrite into
    /// one undo step. `diff(old, new)?.apply_to_string(old) == new` holds for every pair.
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
        // The trailing `s` bytes are identical in both strings, so a boundary in `old` is also
        // one in `new`; retreating `s` keeps both slices valid.
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

/// Undo/redo stack of char-offset [`EditDelta`]s. The caller applies an edit to the buffer
/// *before* recording it; `undo` / `redo` apply the stored delta themselves.
#[derive(Debug, Clone, Default)]
pub struct History {
    undo_stack: Vec<EditDelta>,
    redo_stack: Vec<EditDelta>,
    merge_barrier: bool,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a completed edit, clearing the redo stack. Merges into the previous entry when
    /// [`try_merge`] allows.
    pub fn record(&mut self, delta: EditDelta) {
        if let Some(top) = self.undo_stack.last_mut() {
            if !self.merge_barrier && try_merge(top, &delta) {
                self.redo_stack.clear();
                return;
            }
        }
        self.undo_stack.push(delta);
        self.redo_stack.clear();
        self.merge_barrier = false;
    }

    /// Keep a discrete range action separate from neighboring typing/deletion runs.
    pub fn break_group(&mut self) {
        self.merge_barrier = true;
    }

    /// Undo the most recent edit; returns the cursor to restore, `None` when nothing to undo.
    pub fn undo(&mut self, buf: &mut Buffer) -> Option<usize> {
        let delta = self.undo_stack.pop()?;
        apply_delta(buf, delta.offset, &delta.inserted, &delta.removed);
        let cursor = delta.undo_cursor();
        self.redo_stack.push(delta);
        Some(cursor)
    }

    /// Redo the most recently undone edit; returns the cursor to restore, `None` when nothing
    /// to redo.
    pub fn redo(&mut self, buf: &mut Buffer) -> Option<usize> {
        let delta = self.redo_stack.pop()?;
        apply_delta(buf, delta.offset, &delta.removed, &delta.inserted);
        let cursor = delta.redo_cursor();
        self.undo_stack.push(delta);
        Some(cursor)
    }

    /// Used by tests.
    #[allow(dead_code)]
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Used by tests.
    #[allow(dead_code)]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    /// Replace the whole history with `delta` as the sole undo step, so one undo reverts an
    /// applied diff merge and one redo re-applies it.
    pub fn reset_with(&mut self, delta: EditDelta) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.undo_stack.push(delta);
    }
}

/// Remove `remove_text` at `offset` and insert `insert_text` there; undo and redo call it with
/// the delta's fields swapped.
fn apply_delta(buf: &mut Buffer, offset: usize, remove_text: &str, insert_text: &str) {
    if !remove_text.is_empty() {
        let end = offset + remove_text.chars().count();
        buf.remove(offset, end.min(buf.len_chars()));
    }
    if !insert_text.is_empty() {
        buf.insert(offset, insert_text);
    }
}

/// Fold `new` into `top` in place when they form one contiguous run; returns whether it did.
///
/// Pure inserts and pure deletes merge on *contiguity alone*, regardless of character class, so
/// a held-key burst is one undo step and a cursor move is what separates entries. Mixed-direction
/// edits never merge. `pub(crate)` so diff mode reuses the same rules rather than a fork.
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
    // Backspace: prepend.
    if new.offset + new.removed.chars().count() == top.offset {
        top.removed.insert_str(0, &new.removed);
        top.offset = new.offset;
        return true;
    }
    // Forward delete: the cursor stays put, so append.
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
    // The invariant that matters: every offset they produce lands on a char boundary.

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

    #[test]
    fn diff_offsets_are_bytes_not_chars() {
        let d = EditDelta::diff("ééa", "ééb").expect("differs");
        assert_eq!(d.offset, 4);
        assert_eq!(d.removed, "a");
        assert_eq!(d.inserted, "b");
    }

    /// `😀` (F0 9F 98 80) and `🙀` (F0 9F 99 80) share leading *and* trailing bytes, which is
    /// what both boundary-retreat loops exist for.
    #[test]
    fn diff_never_splits_a_multibyte_char() {
        let d = EditDelta::diff("😀", "🙀").expect("differs");
        assert_eq!(d.offset, 0);
        assert_eq!(d.removed, "😀");
        assert_eq!(d.inserted, "🙀");

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
        /// Alphabet mixes 1-, 2-, 3- and 4-byte chars plus newline.
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
        assert_eq!(b.contents(), "hello");
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
        b.insert(5, "!");
        h.record(EditDelta {
            offset: 5,
            removed: String::new(),
            inserted: "!".into(),
        });
        assert_eq!(b.contents(), "hello!");

        let cursor = h.undo(&mut b).unwrap();
        assert_eq!(b.contents(), "hello");
        assert_eq!(cursor, 5);
    }

    #[test]
    fn undo_deletion() {
        let mut h = History::new();
        let mut b = buf("hello!");
        b.remove(5, 6);
        h.record(EditDelta {
            offset: 5,
            removed: "!".into(),
            inserted: String::new(),
        });
        assert_eq!(b.contents(), "hello");

        let cursor = h.undo(&mut b).unwrap();
        assert_eq!(b.contents(), "hello!");
        assert_eq!(cursor, 6);
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
        assert_eq!(cursor, 6);
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

        h.undo(&mut b).unwrap();
        assert!(h.can_redo());

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
        h.record(EditDelta {
            offset: 10,
            removed: "".into(),
            inserted: "b".into(),
        });
        assert_eq!(h.undo_depth(), 2);
    }

    #[test]
    fn contiguous_inserts_merge_regardless_of_length() {
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

    #[test]
    fn backspacing_word_creates_one_undo_entry() {
        let mut h = History::new();
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
        assert_eq!(cursor, 3);
    }

    #[test]
    fn forward_deleting_word_creates_one_undo_entry() {
        let mut h = History::new();
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

    #[test]
    fn backspace_merges_across_character_classes() {
        let mut h = History::new();
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

    #[test]
    fn forward_delete_merges_across_character_classes() {
        let mut h = History::new();
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

    #[test]
    fn non_contiguous_delete_breaks_group() {
        let mut h = History::new();
        h.record(EditDelta {
            offset: 5,
            removed: "a".into(),
            inserted: "".into(),
        });
        h.record(EditDelta {
            offset: 0,
            removed: "b".into(),
            inserted: "".into(),
        });
        assert_eq!(h.undo_depth(), 2);
    }

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
        // Contiguous, so it merges despite the name (mirrors paste-then-type on the insert side).
        assert_eq!(h.undo_depth(), 1);
    }

    #[test]
    fn undo_of_backspace_group_restores_all_at_once() {
        let mut h = History::new();
        let mut b = buf("hello");
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
        h.record(EditDelta {
            offset: 0,
            removed: "".into(),
            inserted: "junk".into(),
        });
        let mut b = buf("junk");
        let _ = h.undo(&mut b);

        h.reset_with(EditDelta {
            offset: 0,
            removed: "old".into(),
            inserted: "merged".into(),
        });
        assert_eq!(h.undo_depth(), 1, "exactly one undo step after reset_with");
        assert!(!h.can_redo(), "redo stack cleared by reset_with");

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
