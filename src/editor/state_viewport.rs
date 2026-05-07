//! Viewport / scroll arithmetic for `EditorState`.
//!
//! Scroll bounds are measured in **visual** rows for Rendered/Preview mode
//! (long lines wrap at `viewport_width` and consume multiple rows) and in
//! buffer lines for Raw mode.  Methods here keep that distinction
//! transparent to callers: pass viewport width and let the implementation
//! pick the right ruler.

use crate::editor::state::{line_text_trimmed, raw_cursor_visual_row, rendered_cursor_visual_row};
use crate::editor::{EditorState, Mode};

impl EditorState {
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// Scroll down by `n` visual rows. The maximum scroll is set so that the
    /// last visual row can reach the very top of the viewport.
    pub fn scroll_down(&mut self, n: usize, _viewport_height: usize) {
        let total = self.total_visual_rows_for_mode(self.viewport_width);
        let max = total.saturating_sub(1);
        self.scroll = (self.scroll + n).min(max);
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }

    /// Scroll so the last document line sits at the bottom of the viewport.
    ///
    /// `viewport_width` is used to compute visual-wrap-aware scroll positions
    /// in Rendered/Preview mode, where long rendered lines may wrap onto
    /// multiple visual rows.  In Raw mode visibility is measured by logical
    /// buffer lines, so `viewport_width` is ignored.
    pub fn scroll_to_bottom(&mut self, viewport_height: usize, viewport_width: usize) {
        let total = self.total_visual_rows_for_mode(viewport_width);
        if total == 0 {
            self.scroll = 0;
        } else {
            self.scroll = total.saturating_sub(viewport_height);
        }
    }

    /// Smallest scroll offset such that rendered line `target_last` fits on the
    /// last visual row of a viewport of `viewport_height` rows, accounting for
    /// word-wrap at `viewport_width`.  Walks backward from `target_last`,
    /// accumulating visual rows, and stops when adding another line would
    /// overflow the viewport.
    #[allow(dead_code)]
    pub(crate) fn scroll_for_last_visible(
        &self,
        target_last: usize,
        viewport_height: usize,
        viewport_width: usize,
    ) -> usize {
        if viewport_height == 0 {
            return target_last;
        }
        let lines = &self.parsed.lines;
        if lines.is_empty() {
            return 0;
        }
        let target_last = target_last.min(lines.len() - 1);

        let mut rows_used = 0usize;
        let mut line_idx = target_last;
        loop {
            // O(1) cache lookup — the historical inline `visual_rows_for_line`
            // call here was a per-keystroke cost on long documents.
            let rows = self
                .parsed
                .visual_rows_for_line_at(line_idx, viewport_width);
            if rows_used + rows > viewport_height {
                // Including this line would overflow — start from the next one.
                return line_idx + 1;
            }
            rows_used += rows;
            if line_idx == 0 {
                return 0;
            }
            line_idx -= 1;
        }
    }

    /// If the cursor has scrolled above the top of the viewport (because the
    /// user scrolled down past it), move the cursor to the first visible line.
    /// No-op in Preview mode (no editing cursor there).
    pub fn clamp_cursor_to_viewport_top(&mut self) {
        if self.mode == Mode::Preview {
            return;
        }

        let cursor_row = self.cursor_visual_row(self.viewport_width);
        if cursor_row < self.scroll {
            self.cursor.offset = self.char_offset_at_visual_row(self.scroll, self.viewport_width);
            self.cursor.preferred_col = self.cursor.cell_col(&self.buffer);
            self.update_cursor_block();
        }
    }

    /// Total visual rows to use for scroll-bound calculations, based on current mode.
    pub fn total_visual_rows_for_mode(&self, width: usize) -> usize {
        match self.mode {
            Mode::Raw => self.raw_total_visual_rows(width),
            _ => self.parsed.total_visual_rows(width),
        }
    }

    /// Ensure the cursor is visible within the viewport.
    ///
    /// In Raw mode, visibility is based on buffer line numbers.
    /// In Rendered/Preview mode, visibility is measured in visual rows —
    /// long rendered lines wrap at `viewport_width` and consume multiple rows,
    /// so scroll bounds must account for that or the last lines of the
    /// document get pushed off-screen.
    pub fn ensure_cursor_visible(&mut self, viewport_height: usize, viewport_width: usize) {
        if viewport_height == 0 {
            return;
        }

        let cursor_row = self.cursor_visual_row(viewport_width);
        if cursor_row < self.scroll {
            self.scroll = cursor_row;
        } else if cursor_row >= self.scroll + viewport_height {
            self.scroll = cursor_row + 1 - viewport_height;
        }
    }

    /// Sum of visual rows for rendered lines `first..=last`, wrapped at
    /// `width`.  Delegates to the per-frame visual-row cache.  Used by
    /// tests in this crate.
    #[allow(dead_code)]
    pub(crate) fn visual_rows_between(&self, first: usize, last: usize, width: usize) -> usize {
        self.parsed.visual_rows_between(first, last, width)
    }

    pub fn rendered_line_at_visual_row(&self, visual_row: usize, width: usize) -> (usize, usize) {
        self.parsed.line_at_visual_row(visual_row, width)
    }

    pub fn raw_line_at_visual_row(&self, visual_row: usize, width: usize) -> (usize, usize) {
        let width = width.max(1);
        let mut acc = 0usize;
        for line_idx in 0..self.buffer.line_count() {
            let rows = self.raw_visual_rows_for_line(line_idx, width);
            if visual_row < acc + rows {
                return (line_idx, visual_row - acc);
            }
            acc += rows;
        }
        (self.buffer.line_count(), 0)
    }

    pub fn visual_rows_before_raw_line(&self, line_idx: usize, width: usize) -> usize {
        let width = width.max(1);
        (0..line_idx.min(self.buffer.line_count()))
            .map(|i| self.raw_visual_rows_for_line(i, width))
            .sum()
    }

    pub(crate) fn raw_total_visual_rows(&self, width: usize) -> usize {
        self.visual_rows_before_raw_line(self.buffer.line_count(), width)
    }

    pub(crate) fn raw_visual_rows_for_line(&self, line_idx: usize, width: usize) -> usize {
        let text = line_text_trimmed(&self.buffer, line_idx);
        crate::ui::line_render::visual_rows_of_str(&text, width)
            .len()
            .max(1)
    }

    pub(crate) fn cursor_visual_row(&self, width: usize) -> usize {
        match self.mode {
            Mode::Raw => raw_cursor_visual_row(self, width),
            _ => rendered_cursor_visual_row(self, width),
        }
    }

    pub(crate) fn char_offset_at_visual_row(&self, visual_row: usize, width: usize) -> usize {
        match self.mode {
            Mode::Raw => {
                let (line, sub) = self.raw_line_at_visual_row(visual_row, width);
                if line >= self.buffer.line_count() {
                    return self.buffer.len_chars();
                }
                let text = line_text_trimmed(&self.buffer, line);
                let rows = crate::ui::line_render::visual_rows_of_str(&text, width.max(1));
                let raw_col = rows.get(sub).map(|r| r.0).unwrap_or(0);
                self.buffer.line_to_char(line) + raw_col
            }
            _ => {
                let (line_idx, _sub) = self.rendered_line_at_visual_row(visual_row, width.max(1));
                if line_idx >= self.parsed.lines.len() {
                    return self.buffer.len_chars();
                }
                self.parsed
                    .source_map
                    .original_byte_for_rendered_line(line_idx)
                    .map(|byte| {
                        self.buffer
                            .rope()
                            .byte_to_char(byte)
                            .min(self.buffer.len_chars())
                    })
                    .unwrap_or(self.buffer.len_chars())
            }
        }
    }
}
