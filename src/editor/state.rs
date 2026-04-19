use std::time::Instant;

use crate::config::Theme;
use crate::document::{Buffer, Cursor, EditDelta, History, ParsedDoc, Selection, VisualSelection};
use crate::editor::Mode;

/// All mutable state owned by the editor.
///
/// `EditorState` is the single source of truth for the document contents,
/// cursor position, selection, undo/redo history, and current mode.
/// It is mutated by `edit_ops::apply()` and read by the UI layer.
pub struct EditorState {
    pub buffer: Buffer,
    pub cursor: Cursor,
    pub selection: Option<Selection>,
    /// Preview-mode selection in rendered (visible) coordinates.  Populated
    /// only when `mode == Mode::Preview` — switching to Rendered or Raw
    /// clears it because those modes drive selection from the raw buffer.
    pub visual_selection: Option<VisualSelection>,
    pub history: History,
    pub mode: Mode,
    pub parsed: ParsedDoc,
    /// Whether the buffer has unsaved changes since last save.
    pub dirty: bool,
    /// Internal clipboard (kill-ring). Used as fallback when arboard is
    /// unavailable.
    pub kill_ring: String,
    /// Scroll offset (in rendered lines) for RenderedMode.
    pub scroll: usize,
    /// Block index the cursor is currently inside (used for jitter suppression).
    pub cursor_block_idx: Option<usize>,
    /// Buffer line index the cursor is currently on (used to reset the reveal
    /// timer per logical line rather than per block).
    pub cursor_line_idx: Option<usize>,
    /// When the cursor last moved to a new buffer line. The raw/de-rendered view
    /// for the cursor block is shown only after `RAW_REVEAL_DELAY` has elapsed
    /// without further movement on the same line, preventing jitter when scrolling
    /// quickly through multi-line elements such as tables.
    pub cursor_block_entered_at: Option<Instant>,
    /// True while a mouse click-and-drag is in progress.  While true the
    /// cursor's block is never de-rendered, so the user's drag selection
    /// stays anchored to the rendered characters they clicked on — if the
    /// block reveals to raw mid-drag, the visible columns shift and the
    /// anchor would jump.
    pub drag_in_progress: bool,
    /// Optional theme reference — used to re-render after edits.
    theme: &'static Theme,
    /// Whether to preserve multiple consecutive blank lines between blocks.
    preserve_blank_lines: bool,
    /// Whether Up/Down navigate by visual lines (true) or logical lines (false).
    pub visual_line_nav: bool,
}

/// How long the cursor must rest on a block before it is shown in raw mode.
pub const RAW_REVEAL_DELAY: std::time::Duration = std::time::Duration::from_millis(120);

impl EditorState {
    /// Create an `EditorState` from a `Buffer` and a theme.
    ///
    /// # Panics
    ///
    /// Panics if the theme reference has an insufficiently long lifetime.
    /// Callers typically pass `Box::leak(Box::new(Theme::default()))` or a
    /// static reference.
    pub fn new(buffer: Buffer, theme: &'static Theme) -> Self {
        Self::new_with_config(buffer, theme, true, true)
    }

    pub fn new_with_config(
        buffer: Buffer,
        theme: &'static Theme,
        preserve_blank_lines: bool,
        visual_line_nav: bool,
    ) -> Self {
        let content = buffer.contents();
        let parsed = ParsedDoc::build(&content, theme, preserve_blank_lines);
        Self {
            buffer,
            cursor: Cursor::new(),
            selection: None,
            visual_selection: None,
            history: History::new(),
            mode: Mode::Preview,
            parsed,
            dirty: false,
            kill_ring: String::new(),
            scroll: 0,
            cursor_block_idx: None,
            cursor_line_idx: None,
            cursor_block_entered_at: None,
            drag_in_progress: false,
            theme,
            preserve_blank_lines,
            visual_line_nav,
        }
    }

    /// Convenience constructor for tests: creates an in-memory buffer from `text`.
    #[cfg(test)]
    pub fn from_str(text: &str, theme: &'static Theme) -> Self {
        Self::new(Buffer::from_str(text), theme)
    }

    // ── Buffer access ─────────────────────────────────────────────

    pub fn contents(&self) -> String {
        self.buffer.contents()
    }

    /// Re-parse and re-render after an edit. Called automatically by `edit_ops`.
    pub(crate) fn refresh_parsed(&mut self) {
        let content = self.buffer.contents();
        self.parsed = ParsedDoc::build(&content, self.theme, self.preserve_blank_lines);
    }

    /// Set the cursor offset and clamp it to buffer bounds.
    pub(crate) fn set_cursor(&mut self, offset: usize) {
        self.cursor.offset = offset;
        self.cursor.clamp(&self.buffer);
    }

    /// Apply an edit delta to the buffer, record it in history, mark dirty,
    /// and refresh the parsed document.
    pub(crate) fn apply_delta(&mut self, delta: EditDelta) {
        let new_cursor = delta.redo_cursor();
        // Apply the edit.
        let end = delta.offset + delta.removed.chars().count();
        if !delta.removed.is_empty() {
            self.buffer
                .remove(delta.offset, end.min(self.buffer.len_chars()));
        }
        if !delta.inserted.is_empty() {
            self.buffer.insert(delta.offset, &delta.inserted);
        }
        self.history.record(delta);
        self.cursor.offset = new_cursor.min(self.buffer.len_chars());
        self.dirty = true;
        self.refresh_parsed();
    }

    // ── Jitter suppression ────────────────────────────────────────

    /// Call after any cursor movement in Rendered mode. Tracks which block the
    /// cursor is in and which buffer line it is on. `RenderedView` uses
    /// `cursor_block_entered_at` to delay revealing the raw cursor-block view.
    /// The timer resets whenever the cursor moves to a **different buffer line**
    /// (not just a different block), so that the delay is consistent regardless
    /// of whether the block is a single-line paragraph or a fifty-line table.
    pub fn update_cursor_block(&mut self) {
        let cursor_byte = self.buffer.rope().char_to_byte(self.cursor.offset);
        // Always keep cursor_block_idx up-to-date (used by rendered_view for
        // extracting the raw source of the current block).
        self.cursor_block_idx = self.parsed.source_map.block_for_byte(cursor_byte);

        // Reset the reveal timer only when the cursor moves to a different
        // logical buffer line — this makes scrolling through a large table feel
        // uniform: each row gets the same delay, not the whole table at once.
        let (current_line, _) = self.cursor.line_col(&self.buffer);
        if Some(current_line) != self.cursor_line_idx {
            self.cursor_line_idx = Some(current_line);
            self.cursor_block_entered_at = Some(Instant::now());
        }
    }

    /// Returns true when the raw view for the cursor block should be shown.
    /// False during the `RAW_REVEAL_DELAY` window after the cursor entered a
    /// new block (so rapidly-traversed blocks stay rendered), and false
    /// while a mouse drag is in progress (so the user's visible click
    /// anchor doesn't shift under the drag).
    pub fn cursor_block_revealed(&self) -> bool {
        if self.drag_in_progress {
            return false;
        }
        match self.cursor_block_entered_at {
            None => true,
            Some(t) => t.elapsed() >= RAW_REVEAL_DELAY,
        }
    }

    // ── Scroll helpers ────────────────────────────────────────────

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// Scroll down by `n` rendered lines. The maximum scroll is set so that
    /// the last document line can reach the very top of the viewport (one full
    /// page past where the last line is at the bottom), giving the user room to
    /// work near the end of the document without content jumping away.
    pub fn scroll_down(&mut self, n: usize, _viewport_height: usize) {
        let total = self.total_line_count_for_mode();
        // Allow scroll until the last line sits at the top of the viewport.
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
        match self.mode {
            crate::editor::Mode::Raw => {
                let total = self.buffer.line_count();
                self.scroll = total.saturating_sub(viewport_height);
            }
            _ => {
                let total = self.parsed.line_count();
                if total == 0 {
                    self.scroll = 0;
                    return;
                }
                self.scroll =
                    self.scroll_for_last_visible(total - 1, viewport_height, viewport_width);
            }
        }
    }

    /// Smallest scroll offset such that rendered line `target_last` fits on the
    /// last visual row of a viewport of `viewport_height` rows, accounting for
    /// word-wrap at `viewport_width`.  Walks backward from `target_last`,
    /// accumulating visual rows, and stops when adding another line would
    /// overflow the viewport.
    fn scroll_for_last_visible(
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
            let rows =
                crate::ui::line_render::visual_rows_for_line(&lines[line_idx], viewport_width);
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
        if self.mode == crate::editor::Mode::Preview {
            return;
        }

        if self.mode == crate::editor::Mode::Raw {
            let (cursor_line, _) = self.cursor.line_col(&self.buffer);
            if cursor_line < self.scroll {
                let target = self.scroll.min(self.buffer.line_count().saturating_sub(1));
                self.cursor.offset = self.buffer.line_to_char(target);
                let (_, col) = self.cursor.line_col(&self.buffer);
                self.cursor.preferred_col = col;
            }
            return;
        }

        // Rendered mode: check the cursor's rendered line.
        let cursor_byte = self.buffer.rope().char_to_byte(self.cursor.offset);
        let cursor_rendered = self.parsed.source_map.rendered_lines_for_byte(cursor_byte);
        if cursor_rendered.start < self.scroll {
            // Scan forward from self.scroll to find the first block whose rendered
            // start is >= scroll. `original_byte_for_rendered_line` returns the block
            // START byte, but a block may start before scroll; in that case we skip to
            // the next block and try again.
            let total = self.parsed.lines.len();
            let mut scan = self.scroll.min(total.saturating_sub(1));
            loop {
                if let Some(byte) = self.parsed.source_map.original_byte_for_rendered_line(scan) {
                    let block_lines = self.parsed.source_map.rendered_lines_for_byte(byte);
                    if block_lines.start >= self.scroll {
                        let char_offset = self.buffer.rope().byte_to_char(byte);
                        self.cursor.offset = char_offset.min(self.buffer.len_chars());
                        self.update_cursor_block();
                        break;
                    }
                    // Block starts before scroll — skip to end of this block.
                    scan = block_lines.end;
                    if scan >= total {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
    }

    /// Total lines to use for scroll-bound calculations, based on current mode.
    fn total_line_count_for_mode(&self) -> usize {
        match self.mode {
            crate::editor::Mode::Raw => self.buffer.line_count(),
            _ => self.parsed.line_count(),
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

        if self.mode == crate::editor::Mode::Raw {
            // Raw mode: cursor visibility is in buffer-line coordinates.
            let (cursor_line, _) = self.cursor.line_col(&self.buffer);
            if cursor_line < self.scroll {
                self.scroll = cursor_line;
            }
            let visible_end = self.scroll + viewport_height;
            if cursor_line >= visible_end {
                self.scroll = cursor_line + 1 - viewport_height;
            }
            return;
        }

        // Rendered / Preview mode: use source-map rendered line coordinates.
        let cursor_byte = self.buffer.rope().char_to_byte(self.cursor.offset);
        let cursor_lines = self.parsed.source_map.rendered_lines_for_byte(cursor_byte);

        if cursor_lines.is_empty() {
            return;
        }

        let cursor_first = cursor_lines.start;

        // RenderedView now replaces only one rendered line (not the whole block),
        // so the block height does not change. Use the block's last rendered line
        // as the cursor's effective last line in all non-Raw modes.
        let cursor_last = cursor_lines.end.saturating_sub(1);

        // Scroll up if cursor is above the visible area.
        if cursor_first < self.scroll {
            self.scroll = cursor_first;
        }

        // Scroll down if the cursor line doesn't fit in the viewport, measured
        // in visual rows so that wrapped lines between `scroll` and `cursor_last`
        // are counted correctly.
        let total_rows = self.visual_rows_between(self.scroll, cursor_last, viewport_width);
        if total_rows > viewport_height {
            self.scroll =
                self.scroll_for_last_visible(cursor_last, viewport_height, viewport_width);
        }
    }

    /// Sum of visual rows for rendered lines `first..=last`, wrapped at `width`.
    fn visual_rows_between(&self, first: usize, last: usize, width: usize) -> usize {
        let lines = &self.parsed.lines;
        if lines.is_empty() || first > last {
            return 0;
        }
        let last = last.min(lines.len() - 1);
        let mut total = 0usize;
        for idx in first..=last {
            total = total.saturating_add(crate::ui::line_render::visual_rows_for_line(
                &lines[idx],
                width,
            ));
        }
        total
    }

    /// Number of raw source lines for the block that currently contains the
    /// cursor. Used by `ensure_cursor_visible` and `RenderedView` to compute
    /// the virtual height of the cursor block.
    pub fn raw_line_count_for_cursor(&self) -> usize {
        let cursor_byte = self.buffer.rope().char_to_byte(self.cursor.offset);
        self.parsed
            .source_map
            .original_range_for_byte(cursor_byte)
            .map(|r| {
                let source = self.buffer.contents();
                let text = &source[r.start..r.end.min(source.len())];
                // Same counting logic as `raw_source_lines` in rendered_view.rs.
                let mut count = text.split('\n').count();
                if text.ends_with('\n') && count > 1 {
                    count -= 1;
                }
                count.max(1)
            })
            .unwrap_or(1)
    }

    // ── Visual-line navigation ────────────────────────────────────

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
        let target_visual_col = self.cursor.preferred_col;

        let text = line_text_trimmed(&self.buffer, line);
        let rows = crate::ui::line_render::visual_rows_of_str(&text, col_width);
        let (sub_idx, _) = crate::ui::line_render::sub_line_of_col(&rows, col);

        if sub_idx > 0 {
            let target_idx = sub_idx - 1;
            let target = rows[target_idx];
            let is_last = target_idx + 1 == rows.len();
            let raw_col = raw_col_for_visual(&target, target_visual_col, is_last);
            let line_start = self.buffer.line_to_char(line);
            self.cursor.offset = line_start + raw_col;
        } else if line > 0 {
            let prev_line = line - 1;
            let prev_text = line_text_trimmed(&self.buffer, prev_line);
            let prev_rows = crate::ui::line_render::visual_rows_of_str(&prev_text, col_width);
            let target = *prev_rows.last().expect("rows always non-empty");
            let raw_col = raw_col_for_visual(&target, target_visual_col, true);
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
        let target_visual_col = self.cursor.preferred_col;

        let text = line_text_trimmed(&self.buffer, line);
        let rows = crate::ui::line_render::visual_rows_of_str(&text, col_width);
        let (sub_idx, _) = crate::ui::line_render::sub_line_of_col(&rows, col);

        if sub_idx + 1 < rows.len() {
            let target_idx = sub_idx + 1;
            let target = rows[target_idx];
            let is_last = target_idx + 1 == rows.len();
            let raw_col = raw_col_for_visual(&target, target_visual_col, is_last);
            let line_start = self.buffer.line_to_char(line);
            self.cursor.offset = line_start + raw_col;
        } else {
            let last_line = self.buffer.line_count().saturating_sub(1);
            if line < last_line {
                let next_line = line + 1;
                let next_text = line_text_trimmed(&self.buffer, next_line);
                let next_rows = crate::ui::line_render::visual_rows_of_str(&next_text, col_width);
                let target = next_rows[0];
                let is_last = next_rows.len() == 1;
                let raw_col = raw_col_for_visual(&target, target_visual_col, is_last);
                let next_start = self.buffer.line_to_char(next_line);
                self.cursor.offset = next_start + raw_col;
            } else {
                self.cursor.move_line_end(&self.buffer);
            }
        }
    }

    /// Compute the current visual column for the cursor given wrap at
    /// `col_width`.  Returns 0 when the buffer is empty or on a width of 0.
    pub fn current_visual_col(&self, col_width: usize) -> usize {
        if col_width == 0 {
            return self.cursor.line_col(&self.buffer).1;
        }
        let (line, col) = self.cursor.line_col(&self.buffer);
        let text = line_text_trimmed(&self.buffer, line);
        let rows = crate::ui::line_render::visual_rows_of_str(&text, col_width);
        let (_, visual_col) = crate::ui::line_render::sub_line_of_col(&rows, col);
        visual_col
    }
}

/// Fetch the text of buffer line `line`, stripped of any trailing newline.
fn line_text_trimmed(buf: &crate::document::Buffer, line: usize) -> String {
    buf.line(line)
        .map(|s| s.trim_end_matches('\n').to_owned())
        .unwrap_or_default()
}

/// Given a visual row `(start, end, next_start)` and a desired visual column,
/// return the raw column (offset within the line) that lands visually on that
/// row.
///
/// For non-last rows, the clamp is tighter than `row_width`: `sub_line_of_col`
/// treats `raw_col == next_start` as the start of the NEXT row, so if
/// `visual_col` exceeds the target row's width we must land at
/// `next_start - 1` (the last position still on this row) rather than
/// `next_start` (which jumps visually onto the following row and leaves the
/// cursor stuck at the wrap boundary).
///
/// For the last visual row of a logical line, `end == next_start` and the
/// cursor is allowed to sit past the final char, so we clamp to `row_width`.
fn raw_col_for_visual(row: &(usize, usize, usize), visual_col: usize, is_last_row: bool) -> usize {
    let (start, end, next_start) = *row;
    let max_visual = if is_last_row {
        end.saturating_sub(start)
    } else {
        next_start.saturating_sub(start).saturating_sub(1)
    };
    start + visual_col.min(max_visual)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Buffer;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    /// Moving down through a word-wrapped line should land on the visually
    /// corresponding column, not on `col / col_width`.
    #[test]
    fn move_down_visual_honours_word_wrap_boundaries() {
        // Line has a natural wrap point at the last space before col 20.
        // "hello world foo bar baz quux wibble wobble"
        // Wrapping at width 20: row 0 ends at last space ≤ 20.
        let text = "hello world foo bar baz quux wibble wobble";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        // Cursor on row 0 at visual col 3 ("l" in "hello").
        state.cursor.offset = 3;
        state.cursor.preferred_col = 3;

        state.move_down_visual(20);

        // After moving down one visual row, cursor should be on row 1 at
        // visual col 3 — i.e. raw col = row1_start + 3.
        let rows = crate::ui::line_render::visual_rows_of_str(text, 20);
        assert!(rows.len() >= 2, "expected wrap into at least 2 rows");
        let (row1_start, _, _) = rows[1];
        assert_eq!(state.cursor.offset, row1_start + 3);
    }

    /// Moving up from the first visual sub-line of a line should land on the
    /// LAST visual sub-line of the previous line, preserving the visual col.
    #[test]
    fn move_up_visual_crosses_to_last_subline_of_previous_line() {
        let long = "aaaaa bbbbb ccccc ddddd eeeee fffff ggggg hhhhh";
        // Two logical lines separated by \n.
        let text = format!("{}\nshort\n", long);
        let mut state = EditorState::new(Buffer::from_str(&text), theme());
        // Cursor on line 1 (the "short" line) at col 3.
        let line1_start = state.buffer.line_to_char(1);
        state.cursor.offset = line1_start + 3;
        state.cursor.preferred_col = 3;

        state.move_up_visual(20);

        // Expected: land on the LAST visual sub-line of line 0 at visual col 3.
        let rows = crate::ui::line_render::visual_rows_of_str(long, 20);
        let last = *rows.last().unwrap();
        let expected_raw_col = last.0 + 3;
        assert_eq!(
            state.cursor.offset,
            state.buffer.line_to_char(0) + expected_raw_col
        );
    }

    /// A list item's content must navigate the same way — the 2-char `- `
    /// prefix should be part of the line's raw col and must NOT shift the
    /// cursor's visual column by 2.
    /// When the cursor sits on a wide wrapped sub-row at a visual column that
    /// exceeds the width of the sub-row above, pressing Up must land the cursor
    /// visually on the previous sub-row (clamped to its last position), not on
    /// the wrap boundary — which visually renders at column 0 of the current
    /// sub-row and leaves Up "stuck" there.
    #[test]
    fn move_up_visual_clamps_within_target_subrow_not_on_wrap_boundary() {
        // Logical line: prefix that wraps at a space, followed by a long run
        // of 'a's. Width 40 gives row 0 = "Super long line of inline code ` "
        // (33 chars) and row 1+ = 40-char runs of 'a's.
        let text = format!("Super long line of inline code ` {}`", "a".repeat(150));
        let mut state = EditorState::new(Buffer::from_str(&text), theme());
        let width = 40;

        let rows = crate::ui::line_render::visual_rows_of_str(&text, width);
        assert!(rows.len() >= 2);
        let (row0_s, row0_e, _) = rows[0];
        let row0_width = row0_e - row0_s;
        let (row1_start, _, _) = rows[1];

        // Cursor on row 1 at a visual column exceeding row 0's width.
        let visual_col_on_row1 = row0_width + 3;
        state.cursor.offset = row1_start + visual_col_on_row1;
        state.cursor.preferred_col = visual_col_on_row1;

        state.move_up_visual(width);

        // Cursor must now be visually on row 0, not on the row 0/row 1 boundary
        // (which renders at column 0 of row 1).
        let (sub_idx, _) = crate::ui::line_render::sub_line_of_col(&rows, state.cursor.offset);
        assert_eq!(
            sub_idx, 0,
            "cursor at offset {} should be visually on row 0, not row {}",
            state.cursor.offset, sub_idx
        );

        // Pressing Up again from the last position of row 0 should keep moving
        // (snap to start of line), not stall at the same offset.
        let before = state.cursor.offset;
        state.move_up_visual(width);
        assert_ne!(
            state.cursor.offset, before,
            "Up from row 0 must not stall at offset {before}",
        );
    }

    #[test]
    fn move_down_visual_on_list_item_without_offset_bug() {
        // A single-line list item whose content wraps at width 20.
        let text = "- hello world foo bar baz quux wibble";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        // Cursor on row 0 at visual col 5 (the 'o' in "hello").
        state.cursor.offset = 5;
        state.cursor.preferred_col = 5;

        state.move_down_visual(20);

        let rows = crate::ui::line_render::visual_rows_of_str(text, 20);
        assert!(rows.len() >= 2);
        let (row1_start, row1_end, _) = rows[1];
        let row1_width = row1_end - row1_start;
        let expected_visual = 5.min(row1_width);
        assert_eq!(state.cursor.offset, row1_start + expected_visual);
    }

    /// When the cursor moves to the last rendered line of a document that
    /// contains wrapped lines above it, the scroll offset must back up enough
    /// visual rows (not logical lines) to keep the last line on screen.
    /// Regression test for "bottom of document is never visible in Rendered
    /// mode when earlier lines wrap".
    #[test]
    fn scroll_to_bottom_accounts_for_wrapped_lines() {
        // Build a document whose first paragraph wraps across several visual
        // rows, followed by a short final paragraph.  At viewport_height=5 and
        // viewport_width=20, the long paragraph occupies >5 visual rows, so a
        // naive "scroll = total - height" bound (which ignores wrap) would
        // push the final paragraph past the viewport bottom.
        let long = "a".repeat(100); // one rendered line, ~5 visual rows @ width 20
        let src = format!("{long}\n\nfinal line.\n");
        let mut state = EditorState::new(Buffer::from_str(&src), theme());
        state.mode = crate::editor::Mode::Rendered;
        state.cursor.move_doc_end(&state.buffer);
        state.update_cursor_block();

        let vp_h = 5;
        let vp_w = 20;
        state.scroll_to_bottom(vp_h, vp_w);
        state.ensure_cursor_visible(vp_h, vp_w);

        // From scroll..=last_rendered_line, the total visual rows must fit.
        let total = state.parsed.lines.len();
        let last = total - 1;
        let used = state.visual_rows_between(state.scroll, last, vp_w);
        assert!(
            used <= vp_h,
            "scroll {} leaves {} visual rows between scroll and last rendered line (viewport is {})",
            state.scroll,
            used,
            vp_h
        );
    }
}
