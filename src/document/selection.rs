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

impl Selection {
    /// Create a selection starting at `anchor` with zero width.
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
