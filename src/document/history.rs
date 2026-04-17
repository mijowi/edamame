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
    /// Adjacent single-character alphanumeric insertions are merged into the
    /// previous undo entry so that typing "cat" is one undo step instead of
    /// three.  The same boundary detection as word-wrap is used — a space,
    /// punctuation, or any non-alphanumeric character starts a new group.
    pub fn record(&mut self, delta: EditDelta) {
        if let Some(top) = self.undo_stack.last_mut() {
            if can_merge_word_group(top, &delta) {
                top.inserted.push_str(&delta.inserted);
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

/// Can the incoming `new` delta be merged into `top` as part of the same
/// word-typing group?
///
/// Both must be pure insertions, and the new delta must be a single
/// alphanumeric character typed immediately after the top's last inserted
/// character (by byte offset).  Multi-character pastes, newlines, spaces, and
/// any punctuation all break the group.
fn can_merge_word_group(top: &EditDelta, new: &EditDelta) -> bool {
    if !top.removed.is_empty() || !new.removed.is_empty() {
        return false;
    }
    // New insert must be exactly one alphanumeric char.
    let mut new_chars = new.inserted.chars();
    let new_ch = match new_chars.next() {
        Some(c) if new_chars.next().is_none() => c,
        _ => return false,
    };
    if !new_ch.is_alphanumeric() {
        return false;
    }
    // The top's LAST char must also be alphanumeric.
    let top_last = top.inserted.chars().last();
    match top_last {
        Some(c) if c.is_alphanumeric() => {}
        _ => return false,
    }
    // New offset must be immediately after top's inserted text.
    let top_end = top.offset + top.inserted.chars().count();
    new.offset == top_end
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
