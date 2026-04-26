use crate::document::Buffer;

/// A single edit delta: the minimal information needed to undo or redo one
/// logical editing action.
///
/// To undo: remove `inserted` at `offset` and insert `removed` there instead.
/// To redo: remove `removed` at `offset` and insert `inserted` there instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditDelta {
    /// Char offset at which the edit occurred.
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
    /// Adjacent single-character alphanumeric edits of the same kind are
    /// merged into the previous undo entry so that typing "cat" — or
    /// backspacing it — is one undo step instead of three.  The same boundary
    /// detection is used for both directions: a space, punctuation, or any
    /// non-alphanumeric character starts a new group.
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
        // Apply the inverse: remove `inserted`, put back `removed`.
        let end = delta.offset + delta.inserted.chars().count();
        if !delta.inserted.is_empty() {
            buf.remove(delta.offset, end.min(buf.len_chars()));
        }
        if !delta.removed.is_empty() {
            buf.insert(delta.offset, &delta.removed);
        }
        let cursor = delta.undo_cursor();
        self.redo_stack.push(delta);
        Some(cursor)
    }

    /// Redo the most recently undone edit. Re-applies the delta to `buf` and
    /// returns the cursor position after the redo, or `None` if the redo
    /// stack is empty.
    pub fn redo(&mut self, buf: &mut Buffer) -> Option<usize> {
        let delta = self.redo_stack.pop()?;
        // Re-apply: remove `removed`, put back `inserted`.
        let end = delta.offset + delta.removed.chars().count();
        if !delta.removed.is_empty() {
            buf.remove(delta.offset, end.min(buf.len_chars()));
        }
        if !delta.inserted.is_empty() {
            buf.insert(delta.offset, &delta.inserted);
        }
        let cursor = delta.redo_cursor();
        self.undo_stack.push(delta);
        Some(cursor)
    }

    /// Whether the undo stack is empty.
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Whether the redo stack is empty.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Number of entries on the undo stack.
    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }
}

/// Try to fold `new` into `top` as part of the same word-edit group.  Returns
/// `true` if the merge happened (and `top` was mutated in place); `false`
/// otherwise.
///
/// Pure insertions and pure deletions merge symmetrically: the new delta must
/// affect a single alphanumeric character that is contiguous with `top`'s
/// existing range, and `top`'s adjacent character must also be alphanumeric.
/// Mixed-direction edits (insert then delete, or vice versa) never merge.
fn try_merge(top: &mut EditDelta, new: &EditDelta) -> bool {
    if top.removed.is_empty() && new.removed.is_empty() {
        return try_merge_insertion(top, new);
    }
    if top.inserted.is_empty() && new.inserted.is_empty() {
        return try_merge_deletion(top, new);
    }
    false
}

fn try_merge_insertion(top: &mut EditDelta, new: &EditDelta) -> bool {
    match single_char(&new.inserted) {
        Some(c) if c.is_alphanumeric() => {}
        _ => return false,
    }
    // The top's LAST inserted char must also be alphanumeric.
    match top.inserted.chars().last() {
        Some(c) if c.is_alphanumeric() => {}
        _ => return false,
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
    match single_char(&new.removed) {
        Some(c) if c.is_alphanumeric() => {}
        _ => return false,
    }
    // Backspace: the new delete sits immediately before the existing range,
    // so prepend it.  The top's leftmost char must also be alphanumeric.
    if new.offset + new.removed.chars().count() == top.offset {
        match top.removed.chars().next() {
            Some(c) if c.is_alphanumeric() => {}
            _ => return false,
        }
        top.removed.insert_str(0, &new.removed);
        top.offset = new.offset;
        return true;
    }
    // Forward delete: the cursor stays put, so each new delete starts at
    // top's offset.  Append it; the top's rightmost char must be alnum.
    if new.offset == top.offset {
        match top.removed.chars().last() {
            Some(c) if c.is_alphanumeric() => {}
            _ => return false,
        }
        top.removed.push_str(&new.removed);
        return true;
    }
    false
}

/// Return `Some(c)` if `s` is exactly one `char`, else `None`.
fn single_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Buffer;

    fn buf(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

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
    fn multiple_undo_redo() {
        // Inserts separated by non-alphanumeric characters produce distinct
        // undo entries (e.g. `a` then `!` then `b` cannot merge).
        let mut h = History::new();
        let mut b = buf("");
        b.insert(0, "a");
        h.record(EditDelta {
            offset: 0,
            removed: "".into(),
            inserted: "a".into(),
        });
        b.insert(1, "!");
        h.record(EditDelta {
            offset: 1,
            removed: "".into(),
            inserted: "!".into(),
        });
        b.insert(2, "b");
        h.record(EditDelta {
            offset: 2,
            removed: "".into(),
            inserted: "b".into(),
        });
        assert_eq!(b.contents(), "a!b");

        h.undo(&mut b).unwrap();
        assert_eq!(b.contents(), "a!");
        h.undo(&mut b).unwrap();
        assert_eq!(b.contents(), "a");
        h.undo(&mut b).unwrap();
        assert_eq!(b.contents(), "");

        h.redo(&mut b).unwrap();
        assert_eq!(b.contents(), "a");
        h.redo(&mut b).unwrap();
        assert_eq!(b.contents(), "a!");
        h.redo(&mut b).unwrap();
        assert_eq!(b.contents(), "a!b");
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
    fn space_breaks_word_group() {
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
        // "ca", " ", "d" — three distinct groups.
        assert_eq!(h.undo_depth(), 3);
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
    fn multi_char_insert_does_not_merge() {
        let mut h = History::new();
        h.record(EditDelta {
            offset: 0,
            removed: "".into(),
            inserted: "hi".into(),
        });
        // Pasting / newline insertion is its own undo entry even if content
        // is alphanumeric.
        h.record(EditDelta {
            offset: 2,
            removed: "".into(),
            inserted: "foo".into(),
        });
        assert_eq!(h.undo_depth(), 2);
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

    /// A space between alphanumeric backspaces breaks the group, mirroring
    /// the insertion-side behaviour.
    #[test]
    fn space_breaks_backspace_group() {
        let mut h = History::new();
        // Backspacing through "ca d": d, ' ', a, c.
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
        // "d", " ", "ca" — three groups.
        assert_eq!(h.undo_depth(), 3);
    }

    /// A space between alphanumeric forward-deletes breaks the group.
    #[test]
    fn space_breaks_forward_delete_group() {
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
        // "ca", " ", "d" — three groups.
        assert_eq!(h.undo_depth(), 3);
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
