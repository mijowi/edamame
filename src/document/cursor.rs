use crate::document::graphemes::{next_grapheme_offset, prev_grapheme_offset};
use crate::document::wrap::{cell_col_at_char_idx, char_cells};
use crate::document::Buffer;

/// Cursor position: a char offset into the rope plus the column vertical movement aims for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub offset: usize,
    /// Column preserved across up/down moves, in **terminal cells** (wide chars count 2,
    /// combining marks 0) from the screen-row left edge. Updated on horizontal moves; for
    /// visual-line nav `EditorState::current_visual_col` rewrites it relative to the wrapped
    /// sub-row.
    pub preferred_col: usize,
}

impl Cursor {
    pub fn new() -> Self {
        Self::default()
    }

    /// `(line_idx, col)` for the current offset; `col` counts chars, not graphemes or cells.
    pub fn line_col(&self, buf: &Buffer) -> (usize, usize) {
        let line = buf.char_to_line(self.offset);
        let line_start = buf.line_to_char(line);
        (line, self.offset - line_start)
    }

    /// Cell column on the current logical line; the canonical seed for `preferred_col`.
    pub fn cell_col(&self, buf: &Buffer) -> usize {
        let (line, _) = self.line_col(buf);
        let line_start = buf.line_to_char(line);
        let chars = buf.rope().slice(line_start..self.offset).chars();
        cell_col_at_char_idx(chars, usize::MAX, 0)
    }

    // ── Horizontal movement ───────────────────────────────────────

    /// Move one grapheme cluster left.
    pub fn move_left(&mut self, buf: &Buffer) {
        if self.offset > 0 {
            self.offset = prev_grapheme_offset(buf, self.offset);
            self.preferred_col = self.cell_col(buf);
        }
    }

    /// Move one grapheme cluster right.
    pub fn move_right(&mut self, buf: &Buffer) {
        if self.offset < buf.len_chars() {
            self.offset = next_grapheme_offset(buf, self.offset);
            self.preferred_col = self.cell_col(buf);
        }
    }

    pub fn move_line_start(&mut self, buf: &Buffer) {
        let (line, _) = self.line_col(buf);
        self.offset = buf.line_to_char(line);
        self.preferred_col = 0;
    }

    /// Move to the end of the current line, before any trailing newline.
    pub fn move_line_end(&mut self, buf: &Buffer) {
        let (line, _) = self.line_col(buf);
        let line_start = buf.line_to_char(line);
        let len = line_len_no_newline(buf, line);
        self.offset = line_start + len;
        self.preferred_col = self.cell_col(buf);
    }

    /// Move one word left (skip whitespace, then non-whitespace), stepping by grapheme.
    pub fn move_word_left(&mut self, buf: &Buffer) {
        // Whitespace codepoints are always single-char graphemes, so peeking one char back
        // while stepping by grapheme is safe inside multi-codepoint clusters.
        while self.offset > 0 && char_at(buf, self.offset - 1).is_whitespace() {
            self.offset = prev_grapheme_offset(buf, self.offset);
        }
        while self.offset > 0 && !char_at(buf, self.offset - 1).is_whitespace() {
            self.offset = prev_grapheme_offset(buf, self.offset);
        }
        self.preferred_col = self.cell_col(buf);
    }

    /// Move one word right (skip non-whitespace, then whitespace), stepping by grapheme.
    pub fn move_word_right(&mut self, buf: &Buffer) {
        let len = buf.len_chars();
        while self.offset < len && !char_at(buf, self.offset).is_whitespace() {
            self.offset = next_grapheme_offset(buf, self.offset);
        }
        while self.offset < len && char_at(buf, self.offset).is_whitespace() {
            self.offset = next_grapheme_offset(buf, self.offset);
        }
        self.preferred_col = self.cell_col(buf);
    }

    // ── Vertical movement ─────────────────────────────────────────

    /// Move one line up, landing at the char whose cell range covers `preferred_col`; a mid-glyph
    /// target snaps *past* the wide char so the cursor never sits in its right half.
    pub fn move_up(&mut self, buf: &Buffer) {
        let (line, _) = self.line_col(buf);
        if line == 0 {
            self.offset = buf.line_to_char(0);
            return;
        }
        self.offset = char_offset_at_cell_col(buf, line - 1, self.preferred_col);
    }

    /// Move one line down; same landing rule as `move_up`.
    pub fn move_down(&mut self, buf: &Buffer) {
        let (line, _) = self.line_col(buf);
        let last = buf.line_count().saturating_sub(1);
        if line >= last {
            self.move_line_end(buf);
            return;
        }
        self.offset = char_offset_at_cell_col(buf, line + 1, self.preferred_col);
    }

    // ── Document-level movement ───────────────────────────────────

    pub fn move_doc_start(&mut self) {
        self.offset = 0;
        self.preferred_col = 0;
    }

    pub fn move_doc_end(&mut self, buf: &Buffer) {
        self.offset = buf.len_chars();
        self.preferred_col = self.cell_col(buf);
    }

    /// Clamp the offset to buffer bounds; used by tests.
    #[allow(dead_code)]
    pub fn clamp(&mut self, buf: &Buffer) {
        let len = buf.len_chars();
        if self.offset > len {
            self.offset = len;
        }
    }
}

/// Length of `line_idx` in chars, excluding a trailing `\n`.
pub fn line_len_no_newline(buf: &Buffer, line_idx: usize) -> usize {
    match buf.line(line_idx) {
        None => 0,
        Some(s) => s.trim_end_matches('\n').chars().count(),
    }
}

fn char_at(buf: &Buffer, offset: usize) -> char {
    buf.rope().char(offset)
}

/// Absolute char offset on `line_idx` at screen cell column `target_cell`, with the same landing
/// rules as `wrap::char_idx_at_cell_col` (wide-char snap-past, past-content clamp).
fn char_offset_at_cell_col(buf: &Buffer, line_idx: usize, target_cell: usize) -> usize {
    let line_start = buf.line_to_char(line_idx);
    let line_end = if line_idx + 1 < buf.line_count() {
        buf.line_to_char(line_idx + 1).saturating_sub(1)
    } else {
        buf.len_chars()
    };
    if target_cell == 0 {
        return line_start;
    }
    let mut acc = 0usize;
    let mut offset = line_start;
    while offset < line_end {
        let ch = char_at(buf, offset);
        let w = char_cells(ch);
        if acc + w > target_cell {
            return if acc == target_cell {
                offset
            } else {
                offset + 1
            };
        }
        acc += w;
        offset += 1;
    }
    line_end
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
        let c = cur(8);
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
        let mut c = cur(3);
        c.move_line_start(&b);
        assert_eq!(c.offset, 0);
        assert_eq!(c.preferred_col, 0);
    }

    #[test]
    fn move_line_end_trims_newline() {
        let b = buf("hello\nworld");
        let mut c = cur(0);
        c.move_line_end(&b);
        assert_eq!(c.offset, 5);
        assert_eq!(c.preferred_col, 5);
    }

    #[test]
    fn move_line_end_second_line() {
        let b = buf("hello\nworld");
        let mut c = cur(7);
        c.move_line_end(&b);
        assert_eq!(c.offset, 11);
    }

    // ── move_up / move_down ───────────────────────────────────────

    #[test]
    fn move_up_from_second_line() {
        let b = buf("hello\nworld");
        let mut c = cur_pc(8, 2);
        c.move_up(&b);
        assert_eq!(c.offset, 2);
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
        let mut c = cur_pc(2, 2);
        c.move_down(&b);
        assert_eq!(c.offset, 8);
    }

    #[test]
    fn move_down_clamps_to_short_line() {
        let b = buf("hello\nhi");
        let mut c = cur_pc(4, 4);
        c.move_down(&b);
        assert_eq!(c.offset, 8);
    }

    #[test]
    fn move_down_from_last_line_snaps_to_end() {
        let b = buf("hello\nworld");
        let mut c = cur_pc(8, 2);
        c.move_down(&b);
        assert_eq!(c.offset, 11);
    }

    // ── move_word_left / move_word_right ──────────────────────────

    #[test]
    fn move_word_left_from_word() {
        let b = buf("hello world");
        let mut c = cur(10);
        c.move_word_left(&b);
        assert_eq!(c.offset, 6);
    }

    #[test]
    fn move_word_right_from_word() {
        let b = buf("hello world");
        let mut c = cur(0);
        c.move_word_right(&b);
        assert_eq!(c.offset, 6);
    }

    // ── move_doc_start / move_doc_end ─────────────────────────────

    #[test]
    fn move_doc_start() {
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
        let b = buf("hello world\nhi\nhello again");
        let mut c = cur_pc(6, 6);
        c.move_down(&b);
        assert_eq!(c.offset, 14);
        assert_eq!(c.preferred_col, 6);
        c.move_down(&b);
        assert_eq!(c.offset, 15 + 6);
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

    // ── cell-aware vertical landing ───────────────────────────────

    #[test]
    fn move_down_from_after_wide_char_aligns_by_cells() {
        // Offset 2 on line 0 sits at cell 3 (2-cell emoji + 'x'); must land at cell 3, not char 3.
        let b = buf("🥇x\nABCDE");
        let mut c = cur_pc(2, 3);
        c.move_down(&b);
        assert_eq!(c.offset, 6);
        assert_eq!(b.rope().char(c.offset), 'D');
    }

    #[test]
    fn move_down_onto_wide_char_snaps_past_glyph() {
        // preferred_col=1 is mid-emoji on line 1: snap after it, never into its right half.
        let b = buf("AB\n🥇C");
        let mut c = cur_pc(1, 1);
        c.move_down(&b);
        assert_eq!(b.rope().char(c.offset), 'C');
    }

    #[test]
    fn move_down_preserves_cell_column_with_snap_past() {
        // Cell 1 is past the "!" line's content, then mid-emoji on the last line (snap-past).
        let b = buf("XYZ\n!\n🥇A");
        let mut c = cur_pc(1, 1);
        c.move_down(&b);
        assert_eq!(c.offset, 5);
        c.move_down(&b);
        assert_eq!(b.rope().char(c.offset), 'A');
    }

    // ── grapheme-aware horizontal stepping ────────────────────────

    #[test]
    fn move_right_steps_over_zwj_family_as_one_grapheme() {
        let b = buf("👨\u{200D}👩\u{200D}👧\u{200D}👦x");
        let mut c = cur(0);
        c.move_right(&b);
        assert_eq!(c.offset, 7);
        c.move_right(&b);
        assert_eq!(c.offset, 8);
    }

    #[test]
    fn move_left_steps_over_zwj_family_as_one_grapheme() {
        let b = buf("a👨\u{200D}👩\u{200D}👧\u{200D}👦");
        let mut c = cur(8);
        c.move_left(&b);
        assert_eq!(c.offset, 1);
        c.move_left(&b);
        assert_eq!(c.offset, 0);
    }

    #[test]
    fn move_right_steps_over_combining_mark_as_one_grapheme() {
        let b = buf("e\u{0301}!");
        let mut c = cur(0);
        c.move_right(&b);
        assert_eq!(c.offset, 2);
    }

    #[test]
    fn move_word_right_treats_emoji_as_part_of_word() {
        let b = buf("hi 👨\u{200D}👩\u{200D}👧\u{200D}👦x");
        let mut c = cur(0);
        c.move_word_right(&b);
        assert_eq!(c.offset, 3);
        c.move_word_right(&b);
        assert_eq!(c.offset, 11);
    }
}
