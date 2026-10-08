//! Visual-line cursor navigation, using the same word-aware wrap as
//! `ui::line_render::render_line` so the cursor lands at the screen column the user sees.

use crate::editor::state::line_text_trimmed;
use crate::editor::{EditorState, Mode};

impl EditorState {
    /// Move the cursor one line up/down.  In rendered views a table's alignment row and
    /// hidden (zero-rendered-line) HTML-comment blocks are skipped; in `Mode::Raw` every
    /// line is editable source, so nothing is.  Shared by the default handler and vim
    /// `j`/`k`.  `visual` selects per-visual-row stepping (the `gj`/`gk` feel).
    pub fn move_cursor_line(&mut self, down: bool, visual: bool, viewport_width: usize) {
        self.step_cursor_line(down, visual, viewport_width);

        if self.mode == Mode::Raw {
            return;
        }

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

    /// One step for [`Self::move_cursor_line`].
    fn step_cursor_line(&mut self, down: bool, visual: bool, viewport_width: usize) {
        match (down, visual && viewport_width > 0) {
            (true, true) => self.move_down_visual(viewport_width),
            (true, false) => self.cursor.move_down(&self.buffer),
            (false, true) => self.move_up_visual(viewport_width),
            (false, false) => self.cursor.move_up(&self.buffer),
        }
    }

    /// Table-cell horizontal navigation that skips the border chrome, via
    /// [`table_edit_ops::table_move_horizontal`](crate::editor::table_edit_ops::table_move_horizontal)
    /// so vim `h`/`l` and the arrow keys agree.  Returns `true` when the cursor moved or was
    /// clamped at a table edge; `false` (fall back to a grapheme step) otherwise, and always
    /// in `Mode::Raw` where the borders are editable source.
    pub fn try_table_move_horizontal(&mut self, forward: bool) -> bool {
        if self.mode == Mode::Raw {
            return false;
        }
        let moved = crate::editor::table_edit_ops::table_move_horizontal(self, forward);
        if moved {
            self.cursor.preferred_col = self.cursor.cell_col(&self.buffer);
        }
        moved
    }

    /// Vertical companion to [`Self::try_table_move_horizontal`], via
    /// [`try_move_cell_vertical`](crate::editor::table_edit_ops::try_move_cell_vertical);
    /// same `Raw`-mode and fall-back contract.
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

    /// Move the cursor up one visual row (wrapped at `col_width`), crossing into the last
    /// row of the previous logical line and keeping `preferred_col` as the target column.
    pub fn move_up_visual(&mut self, col_width: usize) {
        if col_width == 0 {
            self.cursor.move_up(&self.buffer);
            return;
        }
        let (line, col) = self.cursor.line_col(&self.buffer);
        let target_cell = self.cursor.preferred_col;

        let text = line_text_trimmed(&self.buffer, line);
        let indent = hanging_indent_for_mode(&text, self.mode);
        let rows = wrap_rows_for_text(&text, col_width, indent);
        let (sub_idx, _) = crate::document::wrap::sub_line_of_col(&rows, col);

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
            let prev_indent = hanging_indent_for_mode(&prev_text, self.mode);
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

    /// Down counterpart of [`Self::move_up_visual`].
    pub fn move_down_visual(&mut self, col_width: usize) {
        if col_width == 0 {
            self.cursor.move_down(&self.buffer);
            return;
        }
        let (line, col) = self.cursor.line_col(&self.buffer);
        let target_cell = self.cursor.preferred_col;

        let text = line_text_trimmed(&self.buffer, line);
        let indent = hanging_indent_for_mode(&text, self.mode);
        let rows = wrap_rows_for_text(&text, col_width, indent);
        let (sub_idx, _) = crate::document::wrap::sub_line_of_col(&rows, col);

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
                let next_indent = hanging_indent_for_mode(&next_text, self.mode);
                let next_rows = wrap_rows_for_text(&next_text, col_width, next_indent);
                let target = next_rows[0];
                let is_last = next_rows.len() == 1;
                let raw_col = raw_col_for_visual_cells(&next_text, target, target_cell, is_last, 0);
                let next_start = self.buffer.line_to_char(next_line);
                self.cursor.offset = next_start + raw_col;
            } else {
                self.cursor.move_line_end(&self.buffer);
            }
        }
    }

    /// Cell column of the cursor from the screen-row's left edge (hanging indent included),
    /// used to seed `preferred_col`.
    pub fn current_visual_col(&self, col_width: usize) -> usize {
        if col_width == 0 {
            return self.cursor.cell_col(&self.buffer);
        }
        let (line, col) = self.cursor.line_col(&self.buffer);
        let text = line_text_trimmed(&self.buffer, line);
        let indent = hanging_indent_for_mode(&text, self.mode);
        let rows = wrap_rows_for_text(&text, col_width, indent);
        let (sub_idx, _) = crate::document::wrap::sub_line_of_col(&rows, col);
        let row = rows[sub_idx];
        let row_indent = if sub_idx == 0 { 0 } else { indent };
        cell_col_within_row(&text, row, col, row_indent)
    }
}

/// Hanging indent to wrap `text` with: the same one `line_render` paints the revealed line
/// with in Rendered/Preview, so the cursor lands where it appears; Raw paints flat
/// (`line_render::render_raw_line_with_cursor`) and so wraps flat.
fn hanging_indent_for_mode(text: &str, mode: Mode) -> usize {
    if mode == Mode::Raw {
        0
    } else {
        crate::document::wrap::compute_hanging_indent_str(text)
    }
}

/// `visual_rows_of_chars` bridged from `&str`: `(start, end, next_start)` char-index rows.
fn wrap_rows_for_text(text: &str, col_width: usize, indent: usize) -> Vec<(usize, usize, usize)> {
    let chars: Vec<(char, ratatui::style::Style)> = text
        .chars()
        .map(|c| (c, ratatui::style::Style::default()))
        .collect();
    crate::document::wrap::visual_rows_of_chars(&chars, col_width, indent)
}

/// Inverse of the wrap layout: the absolute char column on the logical line where a cursor
/// aiming at screen cell `target_cell` lands on visual row `row`.  A cell inside a wide
/// glyph snaps past it; a target in the hanging-indent area snaps to the row's first content
/// char; non-last rows clamp via `wrap::last_col_in_row` (measured against `end`,
/// never `next_start`, which would land on a break-absorbed space that paints on the next row).
fn raw_col_for_visual_cells(
    text: &str,
    row: (usize, usize, usize),
    target_cell: usize,
    is_last_row: bool,
    indent: usize,
) -> usize {
    let (start, end, _) = row;
    let max_char_in_row = crate::document::wrap::last_col_in_row(row, is_last_row);
    let row_chars = text.chars().skip(start).take(end - start);
    let in_row_idx = crate::document::wrap::char_idx_at_cell_col(row_chars, target_cell, indent);
    let absolute = start + in_row_idx;
    absolute.min(max_char_in_row)
}

/// Screen cell column of char `char_col` within visual row `row` of `text`, `indent` being
/// the row's hanging indent in cells (0 on first rows).
fn cell_col_within_row(
    text: &str,
    row: (usize, usize, usize),
    char_col: usize,
    indent: usize,
) -> usize {
    let (start, _, _) = row;
    let take = char_col.saturating_sub(start);
    let row_chars = text.chars().skip(start).take(take);
    crate::document::wrap::cell_col_at_char_idx(row_chars, take, indent)
}
