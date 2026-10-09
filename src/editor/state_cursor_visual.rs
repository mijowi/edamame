//! Visual-line cursor navigation, using the same word-aware wrap as
//! `ui::line_render::render_line` so the cursor lands at the screen column the user sees.

use crate::document::row_map::LineLayout;
use crate::document::wrap::Indent;
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

        let shown = ShownLine::of(self, line, col_width);
        let (sub_idx, _) = crate::document::wrap::sub_line_of_col(&shown.rows, shown.col(col));

        if sub_idx > 0 {
            let target_idx = sub_idx - 1;
            let is_last = target_idx + 1 == shown.rows.len();
            self.cursor.offset = shown.offset_at(self, target_idx, target_cell, is_last);
        } else if line > 0 {
            let prev = ShownLine::of(self, line - 1, col_width);
            let target_idx = prev.rows.len() - 1;
            self.cursor.offset = prev.offset_at(self, target_idx, target_cell, true);
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

        let shown = ShownLine::of(self, line, col_width);
        let (sub_idx, _) = crate::document::wrap::sub_line_of_col(&shown.rows, shown.col(col));

        if sub_idx + 1 < shown.rows.len() {
            let target_idx = sub_idx + 1;
            let is_last = target_idx + 1 == shown.rows.len();
            self.cursor.offset = shown.offset_at(self, target_idx, target_cell, is_last);
        } else if line < self.buffer.line_count().saturating_sub(1) {
            let next = ShownLine::of(self, line + 1, col_width);
            let is_last = next.rows.len() == 1;
            self.cursor.offset = next.offset_at(self, 0, target_cell, is_last);
        } else {
            self.cursor.move_line_end(&self.buffer);
        }
    }

    /// Cell column of the cursor from the screen-row's left edge (indent included), used to
    /// seed `preferred_col`.
    pub fn current_visual_col(&self, col_width: usize) -> usize {
        if col_width == 0 {
            return self.cursor.cell_col(&self.buffer);
        }
        let (line, col) = self.cursor.line_col(&self.buffer);
        let shown = ShownLine::of(self, line, col_width);
        let col = shown.col(col);
        let (sub_idx, _) = crate::document::wrap::sub_line_of_col(&shown.rows, col);
        let row = shown.rows[sub_idx];
        cell_col_within_row(&shown.text, row, col, shown.indent.row(sub_idx))
    }
}

/// A buffer line as the view shows it, wrapped at one width: the part of it painted and the
/// rows that part wraps into.  Rendered/Preview lay it out as `row_map::source_line_layout`
/// says, so the cursor lands where it appears; Raw paints the whole line flat
/// (`line_render::render_raw_line_with_cursor`) and so wraps it flat.
struct ShownLine {
    line: usize,
    /// Chars at the line's start that aren't painted (a paragraph's or a code line's
    /// indentation, past the block's or the list's range).
    skip: usize,
    /// The painted rest of the line.
    text: String,
    /// The indent the rows start behind, as applied at the width.
    indent: Indent,
    rows: Vec<(usize, usize, usize)>,
}

impl ShownLine {
    fn of(state: &EditorState, line: usize, col_width: usize) -> Self {
        let full = line_text_trimmed(&state.buffer, line);
        let layout = if state.mode == Mode::Raw {
            LineLayout::default()
        } else {
            crate::document::row_map::source_line_layout(&state.parsed, line)
        };
        let skip = layout.skip.min(full.chars().count());
        let text: String = full.chars().skip(skip).collect();
        let rows = wrap_rows_for_text(&text, col_width, layout.indent);
        Self {
            line,
            skip,
            text,
            indent: layout.indent.at(col_width),
            rows,
        }
    }

    /// Buffer column `col` in the painted text's columns; one in the unpainted head shows on
    /// the first painted char.
    fn col(&self, col: usize) -> usize {
        col.saturating_sub(self.skip)
    }

    /// The buffer offset a cursor aiming at screen cell `target_cell` on row `row_idx` lands on.
    fn offset_at(
        &self,
        state: &EditorState,
        row_idx: usize,
        target_cell: usize,
        is_last: bool,
    ) -> usize {
        let raw_col = raw_col_for_visual_cells(
            &self.text,
            self.rows[row_idx],
            target_cell,
            is_last,
            self.indent.row(row_idx),
        );
        state.buffer.line_to_char(self.line) + self.skip + raw_col
    }
}

/// `visual_rows_of_chars` bridged from `&str`: `(start, end, next_start)` char-index rows.
fn wrap_rows_for_text(text: &str, col_width: usize, indent: Indent) -> Vec<(usize, usize, usize)> {
    let chars: Vec<(char, ratatui::style::Style)> = text
        .chars()
        .map(|c| (c, ratatui::style::Style::default()))
        .collect();
    crate::document::wrap::visual_rows_of_chars(&chars, col_width, indent)
}

/// Inverse of the wrap layout: the absolute char column on the logical line where a cursor
/// aiming at screen cell `target_cell` lands on visual row `row`.  A cell inside a wide
/// glyph snaps past it; a target in the row's indent snaps to the row's first content
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
/// the cells before the row's text ([`Indent::row`]).
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
