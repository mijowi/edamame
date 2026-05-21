use crate::document::Buffer;

/// A text selection: two char offsets forming an anchor and an active end.
///
/// The "anchor" is where the selection started; the "active" is the cursor
/// (moving) end. The selected range is always `min(anchor, active)..max(anchor, active)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Char offset where the selection was started (stays fixed during Shift+move).
    pub anchor: usize,
    /// Char offset of the moveable end of the selection (typically the cursor).
    pub active: usize,
}

/// A selection in the rendered (visible) view — stored as `(rendered_line,
/// char_col)` tuples rather than raw buffer char offsets.  Used in Preview
/// mode, where the user is selecting over the rendered output (no raw
/// Markdown markers) and copy should produce the exact rendered text the
/// user sees, not the underlying Markdown source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualSelection {
    /// `(rendered_line_idx, char_col)` at which the selection was started.
    pub anchor: (usize, usize),
    /// `(rendered_line_idx, char_col)` of the moveable end (mouse pointer).
    pub active: (usize, usize),
}

impl VisualSelection {
    /// Normalized range `(start, end)` where `start <= end` in row-major
    /// ordering.  Convenience helper for highlight + copy code that needs a
    /// deterministic forward span.
    pub fn range(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.active {
            (self.anchor, self.active)
        } else {
            (self.active, self.anchor)
        }
    }

    /// True when anchor and active coincide (zero-width selection).
    pub fn is_empty(&self) -> bool {
        self.anchor == self.active
    }
}

impl Selection {
    /// Create a selection starting at `anchor` with zero width. Used by
    /// integration tests in `tests/` and unit tests in this module.
    #[allow(dead_code)]
    pub fn new(anchor: usize) -> Self {
        Self {
            anchor,
            active: anchor,
        }
    }

    /// The half-open char range `[start, end)` of the selected text.
    pub fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.active), self.anchor.max(self.active))
    }

    /// Return the selected text as a `String`.
    pub fn selected_text(&self, buf: &Buffer) -> String {
        let (start, end) = self.range();
        let end = end.min(buf.len_chars());
        if start >= end {
            return String::new();
        }
        buf.slice_to_string(start, end)
    }

    /// Whether the selection is empty (zero width).
    pub fn is_empty(&self) -> bool {
        self.anchor == self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Buffer;

    fn buf(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

    #[test]
    fn new_selection_is_empty() {
        let s = Selection::new(5);
        assert!(s.is_empty());
        assert_eq!(s.range(), (5, 5));
    }

    #[test]
    fn range_forward() {
        let s = Selection {
            anchor: 2,
            active: 7,
        };
        assert_eq!(s.range(), (2, 7));
    }

    #[test]
    fn range_backward() {
        let s = Selection {
            anchor: 7,
            active: 2,
        };
        assert_eq!(s.range(), (2, 7));
    }

    #[test]
    fn selected_text_forward() {
        let b = buf("hello world");
        let s = Selection {
            anchor: 6,
            active: 11,
        };
        assert_eq!(s.selected_text(&b), "world");
    }

    #[test]
    fn selected_text_backward() {
        let b = buf("hello world");
        let s = Selection {
            anchor: 11,
            active: 6,
        };
        assert_eq!(s.selected_text(&b), "world");
    }

    #[test]
    fn selected_text_empty() {
        let b = buf("hello");
        let s = Selection::new(3);
        assert_eq!(s.selected_text(&b), "");
    }
}
