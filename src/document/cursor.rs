use crate::document::graphemes::{next_grapheme_offset, prev_grapheme_offset};
use crate::document::Buffer;
use crate::ui::line_render::{cell_col_at_char_idx, char_cells};

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
    /// Preferred visual column for vertical movement, measured in **terminal
    /// cells** from the cursor's screen-row left edge.  Wide chars (CJK,
    /// emoji) consume two cells; combining marks zero.  For logical-line
    /// nav this is the cell column from the line's start; for visual-line
    /// nav `EditorState::current_visual_col` rewrites it to the cell column
    /// on the wrapped sub-row (so wrap-aware up/down preserves screen X).
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

    /// Cell column of the cursor on its current logical line, counted in
    /// terminal display cells from the line's start.  Wide chars (CJK,
    /// emoji) contribute 2; combining marks 0.  This is the canonical way
    /// to seed `preferred_col` after a horizontal cursor move.
    pub fn cell_col(&self, buf: &Buffer) -> usize {
        let (line, _) = self.line_col(buf);
        let line_start = buf.line_to_char(line);
        let chars = buf.rope().slice(line_start..self.offset).chars();
        cell_col_at_char_idx(chars, usize::MAX, 0)
    }

    // ── Horizontal movement ───────────────────────────────────────

    /// Move one grapheme cluster to the left.  Steps over multi-codepoint
    /// graphemes (flag emoji, ZWJ sequences, combining marks) as a single
    /// keystroke so navigation matches the user's perception of a "character".
    pub fn move_left(&mut self, buf: &Buffer) {
        if self.offset > 0 {
            self.offset = prev_grapheme_offset(buf, self.offset);
            self.preferred_col = self.cell_col(buf);
        }
    }

    /// Move one grapheme cluster to the right.  Symmetric with `move_left`.
    pub fn move_right(&mut self, buf: &Buffer) {
        if self.offset < buf.len_chars() {
            self.offset = next_grapheme_offset(buf, self.offset);
            self.preferred_col = self.cell_col(buf);
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
        self.preferred_col = self.cell_col(buf);
    }

    /// Move one word to the left (skips whitespace then non-whitespace).
    /// Steps by grapheme cluster so multi-codepoint glyphs stay intact.
    pub fn move_word_left(&mut self, buf: &Buffer) {
        // Skip whitespace backward.  Whitespace codepoints are always
        // single-char graphemes, so checking the preceding char and
        // stepping by grapheme behaves identically on ASCII while
        // remaining safe inside multi-codepoint clusters.
        while self.offset > 0 && char_at(buf, self.offset - 1).is_whitespace() {
            self.offset = prev_grapheme_offset(buf, self.offset);
        }
        // Skip word characters backward.
        while self.offset > 0 && !char_at(buf, self.offset - 1).is_whitespace() {
            self.offset = prev_grapheme_offset(buf, self.offset);
        }
        self.preferred_col = self.cell_col(buf);
    }

    /// Move one word to the right (skips non-whitespace then whitespace).
    /// Steps by grapheme cluster so multi-codepoint glyphs stay intact.
    pub fn move_word_right(&mut self, buf: &Buffer) {
        let len = buf.len_chars();
        // Skip non-whitespace forward.
        while self.offset < len && !char_at(buf, self.offset).is_whitespace() {
            self.offset = next_grapheme_offset(buf, self.offset);
        }
        // Skip whitespace forward.
        while self.offset < len && char_at(buf, self.offset).is_whitespace() {
            self.offset = next_grapheme_offset(buf, self.offset);
        }
        self.preferred_col = self.cell_col(buf);
    }

    // ── Vertical movement ─────────────────────────────────────────

    /// Move one line up.  Lands at the char on the previous line whose
    /// cell range covers `preferred_col`, applying the wide-char snap-past
    /// rule (mid-glyph → cursor *after* the glyph) so the cursor never
    /// sits in the right half of a wide character.
    pub fn move_up(&mut self, buf: &Buffer) {
        let (line, _) = self.line_col(buf);
        if line == 0 {
            self.offset = buf.line_to_char(0);
            return;
        }
        self.offset = char_offset_at_cell_col(buf, line - 1, self.preferred_col);
    }

    /// Move one line down.  See `move_up` for the cell-aware landing rule.
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

    /// Clamp the cursor so it never exceeds buffer bounds. Used by
    /// tests in this crate.
    #[allow(dead_code)]
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

/// Char offset on `line_idx` corresponding to screen cell column
/// `target_cell`.  Walks the line and applies the same landing rules as
/// `line_render::char_idx_at_cell_col` (forbidden indent zone, wide-char
/// snap-past, past-content clamp); returns an absolute char offset into
/// the rope rather than a row-relative index.
fn char_offset_at_cell_col(buf: &Buffer, line_idx: usize, target_cell: usize) -> usize {
    let line_start = buf.line_to_char(line_idx);
    let line_end = if line_idx + 1 < buf.line_count() {
        buf.line_to_char(line_idx + 1).saturating_sub(1) // exclude trailing '\n'
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

    // ── cell-aware vertical landing ───────────────────────────────

    #[test]
    fn move_down_from_after_wide_char_aligns_by_cells() {
        // Line 0 ends `🥇x` — offset 2 sits visually at cell 3 (after the
        // 2-cell emoji and the 1-cell 'x').  Line 1 is plain ASCII; the
        // cursor must land at *cell* 3, not char 3.
        let b = buf("🥇x\nABCDE");
        let mut c = cur_pc(2, 3);
        c.move_down(&b);
        // Char offset 6 = 'D' on line 1 (chars 3..8, with 'A' at 3).
        assert_eq!(c.offset, 6);
        assert_eq!(b.rope().char(c.offset), 'D');
    }

    #[test]
    fn move_down_onto_wide_char_snaps_past_glyph() {
        // Line 0 column 1 (after 'A').  Line 1 starts with a wide char
        // at cells 0–1.  preferred_col=1 → mid-glyph → snap *after* the
        // emoji, never into its right half.
        let b = buf("AB\n🥇C");
        let mut c = cur_pc(1, 1);
        c.move_down(&b);
        // Line 1 starts at char 3.  Cursor lands on 'C' at char 4.
        assert_eq!(b.rope().char(c.offset), 'C');
    }

    #[test]
    fn move_down_preserves_cell_column_with_snap_past() {
        // Sequence: cursor on "XYZ" at cell 1 (after 'X') → down lands at
        // end of "!" line (only 1 cell of content, cell 1 is past it) →
        // down lands on 'A' of "🥇A" via snap-past (cell 1 is mid-emoji,
        // so the cursor leaves the emoji's right half and sits past it).
        let b = buf("XYZ\n!\n🥇A");
        let mut c = cur_pc(1, 1);
        c.move_down(&b);
        // End of "!" line (after '!', before '\n'): char offset 5.
        assert_eq!(c.offset, 5);
        c.move_down(&b);
        // Mid-emoji target → snap past → land on 'A'.
        assert_eq!(b.rope().char(c.offset), 'A');
    }

    // ── grapheme-aware horizontal stepping ────────────────────────

    #[test]
    fn move_right_steps_over_zwj_family_as_one_grapheme() {
        // Family emoji = 7 chars / 1 grapheme.  One MoveRight should clear it.
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
        let mut c = cur(8); // EOF
        c.move_left(&b);
        assert_eq!(c.offset, 1); // landed before the family
        c.move_left(&b);
        assert_eq!(c.offset, 0); // before 'a'
    }

    #[test]
    fn move_right_steps_over_combining_mark_as_one_grapheme() {
        // 'e' + U+0301 = 2 chars / 1 grapheme ("é").
        let b = buf("e\u{0301}!");
        let mut c = cur(0);
        c.move_right(&b);
        assert_eq!(c.offset, 2);
    }

    #[test]
    fn move_word_right_treats_emoji_as_part_of_word() {
        // "hi 👨‍👩‍👧‍👦x" — first word is "hi", whitespace, then the
        // family emoji + 'x' as one word.  Two MoveWordRights cross the doc.
        let b = buf("hi 👨\u{200D}👩\u{200D}👧\u{200D}👦x");
        let mut c = cur(0);
        c.move_word_right(&b);
        // Skipped "hi" (2 chars) then space (1 char) → at 3.
        assert_eq!(c.offset, 3);
        c.move_word_right(&b);
        // Skipped the family (7 chars) + 'x' (1 char) = 11.
        assert_eq!(c.offset, 11);
    }
}
