use crate::document::Buffer;

/// The cursor position within the document.
///
/// The cursor is a char offset into the rope buffer. A `preferred_col` stores
/// the visual column the cursor "wants" to be at during vertical movement — it
/// is updated on horizontal movement and preserved on vertical movement so that
/// moving up and down through lines of varying length feels natural.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    /// Char offset in the rope buffer.
    pub offset: usize,
    /// Preferred visual column for vertical movement (char-count-based, not grapheme-based).
    pub preferred_col: usize,
}

impl Cursor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the `(line_idx, col)` for the current offset (both 0-indexed).
    ///
    /// `col` counts Unicode scalar values (chars) from the line start, not
    /// grapheme clusters or display columns. Use this for logic, not display.
    pub fn line_col(&self, buf: &Buffer) -> (usize, usize) {
        let line = buf.char_to_line(self.offset);
        let line_start = buf.line_to_char(line);
        (line, self.offset - line_start)
    }

    // ── Horizontal movement ───────────────────────────────────────

    /// Move one char to the left. Updates preferred column.
    pub fn move_left(&mut self, buf: &Buffer) {
        if self.offset > 0 {
            self.offset -= 1;
            let (_, col) = self.line_col(buf);
            self.preferred_col = col;
        }
    }

    /// Move one char to the right. Updates preferred column.
    pub fn move_right(&mut self, buf: &Buffer) {
        if self.offset < buf.len_chars() {
            self.offset += 1;
            let (_, col) = self.line_col(buf);
            self.preferred_col = col;
        }
    }

    /// Move to the start of the current line. Resets preferred column.
    pub fn move_line_start(&mut self, buf: &Buffer) {
        let (line, _) = self.line_col(buf);
        self.offset = buf.line_to_char(line);
        self.preferred_col = 0;
    }

    /// Move to the end of the current line (before any trailing newline). Updates preferred column.
    pub fn move_line_end(&mut self, buf: &Buffer) {
        let (line, _) = self.line_col(buf);
        let line_start = buf.line_to_char(line);
        let len = line_len_no_newline(buf, line);
        self.offset = line_start + len;
        self.preferred_col = len;
    }

    /// Move one word to the left (skips whitespace then non-whitespace).
    pub fn move_word_left(&mut self, buf: &Buffer) {
        // Skip whitespace backward.
        while self.offset > 0 && char_at(buf, self.offset - 1).is_whitespace() {
            self.offset -= 1;
        }
        // Skip word characters backward.
        while self.offset > 0 && !char_at(buf, self.offset - 1).is_whitespace() {
            self.offset -= 1;
        }
        let (_, col) = self.line_col(buf);
        self.preferred_col = col;
    }

    /// Move one word to the right (skips non-whitespace then whitespace).
    pub fn move_word_right(&mut self, buf: &Buffer) {
        let len = buf.len_chars();
        // Skip non-whitespace forward.
        while self.offset < len && !char_at(buf, self.offset).is_whitespace() {
            self.offset += 1;
        }
        // Skip whitespace forward.
        while self.offset < len && char_at(buf, self.offset).is_whitespace() {
            self.offset += 1;
        }
        let (_, col) = self.line_col(buf);
        self.preferred_col = col;
    }

    // ── Vertical movement ─────────────────────────────────────────

    /// Move one line up. Tries to land on `preferred_col`; clamps to line end.
    pub fn move_up(&mut self, buf: &Buffer) {
        let (line, _) = self.line_col(buf);
        if line == 0 {
            // Already on first line — snap to start.
            self.offset = buf.line_to_char(0);
            return;
        }
        let prev = line - 1;
        let prev_start = buf.line_to_char(prev);
        let prev_len = line_len_no_newline(buf, prev);
        self.offset = prev_start + self.preferred_col.min(prev_len);
    }

    /// Move one line down. Tries to land on `preferred_col`; clamps to line end.
    pub fn move_down(&mut self, buf: &Buffer) {
        let (line, _) = self.line_col(buf);
        let last = buf.line_count().saturating_sub(1);
        if line >= last {
            // Already on last line — snap to end.
            self.move_line_end(buf);
            return;
        }
        let next = line + 1;
        let next_start = buf.line_to_char(next);
        let next_len = line_len_no_newline(buf, next);
        self.offset = next_start + self.preferred_col.min(next_len);
    }

    // ── Document-level movement ───────────────────────────────────

    pub fn move_doc_start(&mut self) {
        self.offset = 0;
        self.preferred_col = 0;
    }

    pub fn move_doc_end(&mut self, buf: &Buffer) {
        self.offset = buf.len_chars();
        let (_, col) = self.line_col(buf);
        self.preferred_col = col;
    }

    // ── Utilities ─────────────────────────────────────────────────

    /// Clamp the cursor so it never exceeds buffer bounds.
    pub fn clamp(&mut self, buf: &Buffer) {
        let len = buf.len_chars();
        if self.offset > len {
            self.offset = len;
        }
    }
}

/// Length of `line_idx` in chars, NOT including a trailing `\n`.
pub fn line_len_no_newline(buf: &Buffer, line_idx: usize) -> usize {
    match buf.line(line_idx) {
        None => 0,
        Some(s) => s.trim_end_matches('\n').chars().count(),
    }
}

/// Return the char at `offset` in the buffer.
fn char_at(buf: &Buffer, offset: usize) -> char {
    buf.rope().char(offset)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Buffer;

    fn buf(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

    fn cur(offset: usize) -> Cursor {
        Cursor {
            offset,
            preferred_col: 0,
        }
    }

    fn cur_pc(offset: usize, preferred_col: usize) -> Cursor {
        Cursor {
            offset,
            preferred_col,
        }
    }

    // ── line_col ──────────────────────────────────────────────────

    #[test]
    fn line_col_first_line() {
        let b = buf("hello\nworld");
        let c = cur(3);
        assert_eq!(c.line_col(&b), (0, 3));
    }

    #[test]
    fn line_col_second_line() {
        let b = buf("hello\nworld");
        let c = cur(8); // 'r' in "world"
        assert_eq!(c.line_col(&b), (1, 2));
    }

    // ── move_left / move_right ────────────────────────────────────

    #[test]
    fn move_left_basic() {
        let b = buf("hello");
        let mut c = cur(3);
        c.move_left(&b);
        assert_eq!(c.offset, 2);
        assert_eq!(c.preferred_col, 2);
    }

    #[test]
    fn move_left_at_start_is_noop() {
        let b = buf("hello");
        let mut c = cur(0);
        c.move_left(&b);
        assert_eq!(c.offset, 0);
    }

    #[test]
    fn move_right_basic() {
        let b = buf("hello");
        let mut c = cur(2);
        c.move_right(&b);
        assert_eq!(c.offset, 3);
        assert_eq!(c.preferred_col, 3);
    }

    #[test]
    fn move_right_at_end_is_noop() {
        let b = buf("hi");
        let mut c = cur(2);
        c.move_right(&b);
        assert_eq!(c.offset, 2);
    }

    // ── move_line_start / move_line_end ───────────────────────────

    #[test]
    fn move_line_start_mid_line() {
        let b = buf("hello\nworld");
        let mut c = cur(3); // 'l' in hello
        c.move_line_start(&b);
        assert_eq!(c.offset, 0);
        assert_eq!(c.preferred_col, 0);
    }

    #[test]
    fn move_line_end_trims_newline() {
        let b = buf("hello\nworld");
        let mut c = cur(0);
        c.move_line_end(&b);
        assert_eq!(c.offset, 5); // after 'o', before '\n'
        assert_eq!(c.preferred_col, 5);
    }

    #[test]
    fn move_line_end_second_line() {
        let b = buf("hello\nworld");
        let mut c = cur(7); // inside "world"
        c.move_line_end(&b);
        assert_eq!(c.offset, 11); // past 'd'
    }

    // ── move_up / move_down ───────────────────────────────────────

    #[test]
    fn move_up_from_second_line() {
        let b = buf("hello\nworld");
        let mut c = cur_pc(8, 2); // 'r' in "world", preferred_col=2
        c.move_up(&b);
        assert_eq!(c.offset, 2); // 'l' in "hello"
    }

    #[test]
    fn move_up_from_first_line_snaps_to_start() {
        let b = buf("hello\nworld");
        let mut c = cur_pc(3, 3);
        c.move_up(&b);
        assert_eq!(c.offset, 0);
    }

    #[test]
    fn move_down_from_first_line() {
        let b = buf("hello\nworld");
        let mut c = cur_pc(2, 2); // 'l' in "hello", preferred_col=2
        c.move_down(&b);
        assert_eq!(c.offset, 8); // 'r' in "world"
    }

    #[test]
    fn move_down_clamps_to_short_line() {
        let b = buf("hello\nhi");
        // preferred_col=4, but "hi" only has 2 chars
        let mut c = cur_pc(4, 4);
        c.move_down(&b);
        assert_eq!(c.offset, 8); // end of "hi" (no newline)
    }

    #[test]
    fn move_down_from_last_line_snaps_to_end() {
        let b = buf("hello\nworld");
        let mut c = cur_pc(8, 2); // inside "world"
        c.move_down(&b);
        assert_eq!(c.offset, 11); // end of document
    }

    // ── move_word_left / move_word_right ──────────────────────────

    #[test]
    fn move_word_left_from_word() {
        let b = buf("hello world");
        let mut c = cur(10); // 'l' in "world"
        c.move_word_left(&b);
        assert_eq!(c.offset, 6); // start of "world"
    }

    #[test]
    fn move_word_right_from_word() {
        let b = buf("hello world");
        let mut c = cur(0);
        c.move_word_right(&b);
        assert_eq!(c.offset, 6); // start of "world"
    }

    // ── move_doc_start / move_doc_end ─────────────────────────────

    #[test]
    fn move_doc_start() {
        let b = buf("hello\nworld");
        let mut c = cur(8);
        c.move_doc_start();
        assert_eq!(c.offset, 0);
        assert_eq!(c.preferred_col, 0);
    }

    #[test]
    fn move_doc_end() {
        let b = buf("hello\nworld");
        let mut c = cur(0);
        c.move_doc_end(&b);
        assert_eq!(c.offset, 11);
    }

    // ── preferred_col preservation ────────────────────────────────

    #[test]
    fn preferred_col_preserved_through_short_line() {
        // Moving through a short line and back should restore preferred_col.
        let b = buf("hello world\nhi\nhello again");
        let mut c = cur_pc(6, 6); // 'w' in "hello world"
        c.move_down(&b); // → "hi" (col clamped to 2)
        assert_eq!(c.offset, 14); // end of "hi"
        assert_eq!(c.preferred_col, 6); // preferred_col unchanged
        c.move_down(&b); // → "hello again" col 6
        assert_eq!(c.offset, 15 + 6); // 'a' in "again"
    }

    // ── clamp ──────────────────────────────────────────────────────

    #[test]
    fn clamp_within_bounds_is_noop() {
        let b = buf("hi");
        let mut c = cur(1);
        c.clamp(&b);
        assert_eq!(c.offset, 1);
    }

    #[test]
    fn clamp_past_end_snaps_to_len() {
        let b = buf("hi");
        let mut c = cur(100);
        c.clamp(&b);
        assert_eq!(c.offset, 2);
    }
}
