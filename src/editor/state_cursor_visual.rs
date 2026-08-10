//! Visual-line cursor navigation.
//!
//! Visual-line moves use the same word-aware wrap algorithm as
//! `ui::line_render::render_line` so the cursor lands at the screen column
//! the user actually sees.  The free helpers in this file translate between
//! a logical-line raw column and a screen-cell column for a wrapped line.

use crate::editor::state::line_text_trimmed;
use crate::editor::{EditorState, Mode};

impl EditorState {
    /// Move the cursor one line up/down, skipping the structural rows that
    /// are not real editing targets — but **only in a rendered view**.  In
    /// `Mode::Rendered`/`Preview` a GFM table's alignment row (`|---|`) and
    /// hidden (zero-rendered-line) HTML-comment blocks are skipped, because
    /// they're artefacts the user can't sensibly land on.  In `Mode::Raw`
    /// every line is genuine, editable source, so nothing is skipped.  This
    /// rendered-vs-raw rule is shared by the default handler (`MoveUp`/
    /// `MoveDown`) and vim `j`/`k`.
    ///
    /// `visual` selects per-visual-row stepping (wrapped lines — the
    /// `gj`/`gk` feel) over logical-line stepping: the default handler
    /// passes `visual_line_nav`; vim `j`/`k` pass `false` (logical), since
    /// `gj`/`gk` are the visual variants.
    pub fn move_cursor_line(&mut self, down: bool, visual: bool, viewport_width: usize) {
        self.step_cursor_line(down, visual, viewport_width);

        // Raw view: the alignment row and comment bytes are real lines the
        // user may want to edit, so they're valid targets — skip nothing.
        if self.mode == Mode::Raw {
            return;
        }

        // Skip a landed-on alignment row, then walk past any run of hidden
        // (zero-rendered-line) comment blocks.  The loop is bounded by
        // offset-stalls at the buffer edge so it can't spin.
        if crate::editor::table_edit_ops::cursor_on_alignment_row(self) {
            self.step_cursor_line(down, visual, viewport_width);
        }
        let mut safety = 32usize;
        while crate::editor::edit_ops::cursor_on_hidden_block(self) && safety > 0 {
            let prev_offset = self.cursor.offset;
            self.step_cursor_line(down, visual, viewport_width);
            if self.cursor.offset == prev_offset {
                break;
            }
            safety -= 1;
        }
    }

    /// One raw step for [`Self::move_cursor_line`]: per-visual-row when
    /// `visual` is set and a width is known, else per-logical-line.
    fn step_cursor_line(&mut self, down: bool, visual: bool, viewport_width: usize) {
        match (down, visual && viewport_width > 0) {
            (true, true) => self.move_down_visual(viewport_width),
            (true, false) => self.cursor.move_down(&self.buffer),
            (false, true) => self.move_up_visual(viewport_width),
            (false, false) => self.cursor.move_up(&self.buffer),
        }
    }

    /// Attempt table-cell **horizontal** navigation, skipping the
    /// auto-managed border chrome (`|`, padding, the alignment row) so a
    /// motion lands cell-to-cell rather than on characters the editor owns.
    /// Reuses the default handler's [`table_edit_ops::table_move_horizontal`](crate::editor::table_edit_ops::table_move_horizontal) logic, so vim
    /// `h`/`l` and the arrow keys behave identically inside a table.
    ///
    /// Returns `true` when the cursor was moved or deliberately clamped at a
    /// table edge; `false` when the caller should fall back to a plain
    /// grapheme step.  Always `false` in `Mode::Raw`, where the borders are
    /// real, hand-editable source and every character is a valid target.
    pub fn try_table_move_horizontal(&mut self, forward: bool) -> bool {
        if self.mode == Mode::Raw {
            return false;
        }
        let moved = crate::editor::table_edit_ops::table_move_horizontal(self, forward);
        if moved {
            // `table_move_horizontal` sets the offset directly; refresh the
            // preferred column so a following logical `j`/`k` lands sensibly.
            self.cursor.preferred_col = self.cursor.cell_col(&self.buffer);
        }
        moved
    }

    /// Attempt table-cell **vertical** navigation (the `j`/`k` companion to
    /// [`Self::try_table_move_horizontal`]): move to the cell directly
    /// above/below, preserving the column and skipping the alignment row.
    /// Reuses the default handler's [`try_move_cell_vertical`](crate::editor::table_edit_ops::try_move_cell_vertical), which also
    /// refreshes the cursor block and viewport on success.  Same `Raw`-mode
    /// and fall-back contract as the horizontal variant.
    pub fn try_table_move_vertical(
        &mut self,
        down: bool,
        viewport_height: usize,
        viewport_width: usize,
    ) -> bool {
        if self.mode == Mode::Raw {
            return false;
        }
        crate::editor::table_edit_ops::try_move_cell_vertical(
            self,
            down,
            viewport_height,
            viewport_width,
        )
    }

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
