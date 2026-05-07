//! Visual-line cursor navigation.
//!
//! Visual-line moves use the same word-aware wrap algorithm as
//! `ui::line_render::render_line` so the cursor lands at the screen column
//! the user actually sees.  The free helpers in this file translate between
//! a logical-line raw column and a screen-cell column for a wrapped line.

use crate::editor::state::line_text_trimmed;
use crate::editor::EditorState;

impl EditorState {
    /// Move the cursor up by one **visual** line, accounting for word-wrap at
    /// `col_width`.
    ///
    /// Uses the same word-aware wrap algorithm as `line_render::render_line`
    /// so navigation lands on the same visual column the user sees on screen.
    /// If the cursor is on the first visual sub-line of its logical line, it
    /// moves to the LAST visual sub-line of the previous logical line,
    /// preserving `cursor.preferred_col` as the target visual column.
    pub fn move_up_visual(&mut self, col_width: usize) {
        if col_width == 0 {
            self.cursor.move_up(&self.buffer);
            return;
        }
        let (line, col) = self.cursor.line_col(&self.buffer);
        let target_cell = self.cursor.preferred_col;

        let text = line_text_trimmed(&self.buffer, line);
        let indent = crate::ui::line_render::compute_hanging_indent_str(&text);
        let rows = wrap_rows_for_text(&text, col_width, indent);
        let (sub_idx, _) = crate::ui::line_render::sub_line_of_col(&rows, col);

        if sub_idx > 0 {
            let target_idx = sub_idx - 1;
            let target = rows[target_idx];
            let is_last = target_idx + 1 == rows.len();
            let row_indent = if target_idx == 0 { 0 } else { indent };
            let raw_col = raw_col_for_visual_cells(&text, target, target_cell, is_last, row_indent);
            let line_start = self.buffer.line_to_char(line);
            self.cursor.offset = line_start + raw_col;
        } else if line > 0 {
            let prev_line = line - 1;
            let prev_text = line_text_trimmed(&self.buffer, prev_line);
            let prev_indent = crate::ui::line_render::compute_hanging_indent_str(&prev_text);
            let prev_rows = wrap_rows_for_text(&prev_text, col_width, prev_indent);
            let target_idx = prev_rows.len() - 1;
            let target = *prev_rows.last().expect("rows always non-empty");
            let row_indent = if target_idx == 0 { 0 } else { prev_indent };
            let raw_col =
                raw_col_for_visual_cells(&prev_text, target, target_cell, true, row_indent);
            let prev_start = self.buffer.line_to_char(prev_line);
            self.cursor.offset = prev_start + raw_col;
        } else {
            self.cursor.offset = self.buffer.line_to_char(0);
        }
    }

    /// Move the cursor down by one **visual** line, accounting for word-wrap at
    /// `col_width`. See `move_up_visual` for details.
    pub fn move_down_visual(&mut self, col_width: usize) {
        if col_width == 0 {
            self.cursor.move_down(&self.buffer);
            return;
        }
        let (line, col) = self.cursor.line_col(&self.buffer);
        let target_cell = self.cursor.preferred_col;

        let text = line_text_trimmed(&self.buffer, line);
        let indent = crate::ui::line_render::compute_hanging_indent_str(&text);
        let rows = wrap_rows_for_text(&text, col_width, indent);
        let (sub_idx, _) = crate::ui::line_render::sub_line_of_col(&rows, col);

        if sub_idx + 1 < rows.len() {
            let target_idx = sub_idx + 1;
            let target = rows[target_idx];
            let is_last = target_idx + 1 == rows.len();
            let row_indent = if target_idx == 0 { 0 } else { indent };
            let raw_col = raw_col_for_visual_cells(&text, target, target_cell, is_last, row_indent);
            let line_start = self.buffer.line_to_char(line);
            self.cursor.offset = line_start + raw_col;
        } else {
            let last_line = self.buffer.line_count().saturating_sub(1);
            if line < last_line {
                let next_line = line + 1;
                let next_text = line_text_trimmed(&self.buffer, next_line);
                let next_indent = crate::ui::line_render::compute_hanging_indent_str(&next_text);
                let next_rows = wrap_rows_for_text(&next_text, col_width, next_indent);
                let target = next_rows[0];
                let is_last = next_rows.len() == 1;
                // First row of the next logical line uses no hanging indent;
                // continuation rows of the same line do.
                let raw_col = raw_col_for_visual_cells(&next_text, target, target_cell, is_last, 0);
                let next_start = self.buffer.line_to_char(next_line);
                self.cursor.offset = next_start + raw_col;
            } else {
                self.cursor.move_line_end(&self.buffer);
            }
        }
    }

    /// Cell column of the cursor measured from the **screen-row** left
    /// edge, including any hanging-indent padding when the cursor sits on
    /// a wrapped continuation row.  Used to seed `preferred_col` so
    /// vertical navigation lands at the same screen X on the target row.
    pub fn current_visual_col(&self, col_width: usize) -> usize {
        if col_width == 0 {
            return self.cursor.cell_col(&self.buffer);
        }
        let (line, col) = self.cursor.line_col(&self.buffer);
        let text = line_text_trimmed(&self.buffer, line);
        let indent = crate::ui::line_render::compute_hanging_indent_str(&text);
        let rows = wrap_rows_for_text(&text, col_width, indent);
        let (sub_idx, _) = crate::ui::line_render::sub_line_of_col(&rows, col);
        let row = rows[sub_idx];
        let row_indent = if sub_idx == 0 { 0 } else { indent };
        cell_col_within_row(&text, row, col, row_indent)
    }
}

/// Wrap `text` at `col_width` cells with a hanging `indent` on continuation
/// rows.  Returns `(start, end, next_start)` tuples (char indices) — the
/// same shape `visual_rows_of_chars` returns, just bridged from `&str`
/// since `EditorState` works in raw text.
fn wrap_rows_for_text(text: &str, col_width: usize, indent: usize) -> Vec<(usize, usize, usize)> {
    let chars: Vec<(char, ratatui::style::Style)> = text
        .chars()
        .map(|c| (c, ratatui::style::Style::default()))
        .collect();
    crate::ui::line_render::visual_rows_of_chars(&chars, col_width, indent)
}

/// Cell-aware inverse of the wrap layout: given the text of a logical line,
/// one of its visual rows `(start, end, next_start)` (char indices), the
/// desired screen cell column `target_cell`, whether this row is the last
/// in its line, and the row's hanging-indent width in cells, return the
/// absolute char column on the logical line where the cursor should land.
///
/// Wide chars (CJK, emoji) are handled via the snap-past rule: a target
/// cell that lands inside a wide glyph places the cursor *after* the glyph
/// rather than splitting it.  For non-last rows, the cursor is kept off the
/// wrap boundary at `next_start` so it stays visually on this row.  When
/// `indent > 0` (a continuation row of a wrapped list item), the indent
/// area is a forbidden zone — clicks inside it snap forward to the row's
/// first content char.
fn raw_col_for_visual_cells(
    text: &str,
    row: (usize, usize, usize),
    target_cell: usize,
    is_last_row: bool,
    indent: usize,
) -> usize {
    let (start, end, next_start) = row;
    let max_char_in_row = if is_last_row {
        end
    } else {
        next_start.saturating_sub(1).max(start)
    };
    let row_chars = text.chars().skip(start).take(end - start);
    let in_row_idx = crate::ui::line_render::char_idx_at_cell_col(row_chars, target_cell, indent);
    let absolute = start + in_row_idx;
    absolute.min(max_char_in_row)
}

/// Screen cell column of char position `char_col` within its visual row
/// `row`, for a logical line whose raw text is `text`.  `indent` is the
/// hanging-indent in cells for this row (0 for first rows; the line's
/// detected indent for continuation rows).  Used by `current_visual_col`
/// to seed `preferred_col` after a horizontal move so subsequent vertical
/// nav preserves the cursor's screen X.
fn cell_col_within_row(
    text: &str,
    row: (usize, usize, usize),
    char_col: usize,
    indent: usize,
) -> usize {
    let (start, _, _) = row;
    let take = char_col.saturating_sub(start);
    let row_chars = text.chars().skip(start).take(take);
    crate::ui::line_render::cell_col_at_char_idx(row_chars, take, indent)
}
