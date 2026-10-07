//! Viewport / scroll arithmetic for `EditorState`.  Scroll bounds are visual rows (wrapped
//! at `viewport_width`) in Rendered/Preview and buffer lines in Raw; pass the width and the
//! implementation picks the ruler.

use crate::document::visual_cache::VisualRowCache;
use crate::editor::state::{line_text_trimmed, raw_cursor_visual_row, rendered_cursor_visual_row};
use crate::editor::{EditorState, Mode};

/// Raw-mode visual-row cache entry: a [`VisualRowCache`] keyed by the `Buffer::version()`
/// it was built from (width invalidation is the cache's own job).
#[derive(Debug, Clone)]
pub(crate) struct RawVisualRowCache {
    buffer_version: u64,
    inner: VisualRowCache,
}

impl EditorState {
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// Scroll down by `n` visual rows; the last row may reach the top of the viewport.
    pub fn scroll_down(&mut self, n: usize, _viewport_height: usize) {
        let total = self.total_visual_rows_for_mode(self.viewport_width);
        let max = total.saturating_sub(1);
        self.scroll = (self.scroll + n).min(max);
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }

    /// Pull `scroll` back inside the document when a rebuild has shrunk the row space under a
    /// reader who is scrolled down.  Without it the viewport sits past the last row and paints
    /// nothing at all — a blank screen for a document that is merely shorter than it was.
    ///
    /// The bound is the one the scroll ops use ([`Self::scroll_down`]: the last row may reach the
    /// top of the viewport), not `scroll_to_bottom`'s tighter one, so this can only ever undo a
    /// scroll the reader could not have asked for.
    pub fn clamp_scroll_to_document(&mut self) {
        let max = self
            .total_visual_rows_for_mode(self.viewport_width)
            .saturating_sub(1);
        self.scroll = self.scroll.min(max);
    }

    /// Scroll so the last document line sits at the bottom of the viewport.
    pub fn scroll_to_bottom(&mut self, viewport_height: usize, viewport_width: usize) {
        let total = self.total_visual_rows_for_mode(viewport_width);
        if total == 0 {
            self.scroll = 0;
        } else {
            self.scroll = total.saturating_sub(viewport_height);
        }
    }

    /// Smallest scroll offset at which rendered line `target_last` still fits on the last
    /// visual row of a `viewport_height`-row viewport wrapped at `viewport_width`.
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
            let rows = self
                .parsed
                .visual_rows_for_line_at(line_idx, viewport_width);
            if rows_used + rows > viewport_height {
                return line_idx + 1;
            }
            rows_used += rows;
            if line_idx == 0 {
                return 0;
            }
            line_idx -= 1;
        }
    }

    /// Pull a cursor that was scrolled off the top back to the first visible line.
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
            Mode::Diff => self
                .diff
                .as_ref()
                .map(|d| d.total_visual_rows(width))
                .unwrap_or(0),
            // `effective_rows` is the identity unless a reflowed cursor block is revealed, so this
            // equals `parsed.total_visual_rows(width)` everywhere except that reveal.
            _ => self.effective_rows(width).total_visual_rows(),
        }
    }

    /// Ensure the cursor is visible within the viewport.
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

    /// Sum of visual rows for rendered lines `first..=last` wrapped at `width` (test helper).
    #[allow(dead_code)]
    pub(crate) fn visual_rows_between(&self, first: usize, last: usize, width: usize) -> usize {
        self.parsed.visual_rows_between(first, last, width)
    }

    pub fn rendered_line_at_visual_row(&self, visual_row: usize, width: usize) -> (usize, usize) {
        self.parsed.line_at_visual_row(visual_row, width)
    }

    pub fn raw_line_at_visual_row(&self, visual_row: usize, width: usize) -> (usize, usize) {
        self.with_raw_visual_cache(width, |c| c.find_visual_row(visual_row))
    }

    pub fn visual_rows_before_raw_line(&self, line_idx: usize, width: usize) -> usize {
        self.with_raw_visual_cache(width, |c| c.before(line_idx))
    }

    pub(crate) fn raw_total_visual_rows(&self, width: usize) -> usize {
        self.with_raw_visual_cache(width, |c| c.total())
    }

    /// Run `f` against the raw-mode visual-row cache, rebuilding on a buffer-version or width
    /// change.  A small LRU of widths is kept because the editor view queries two distinct
    /// widths per frame (pre- and post-scrollbar-gutter), so a single slot would thrash.
    fn with_raw_visual_cache<R>(&self, width: usize, f: impl FnOnce(&VisualRowCache) -> R) -> R {
        /// Must be ≥ 2 for the two-width-per-frame pattern.
        const LRU_CAP: usize = 2;
        let width = width.max(1);
        let buffer_version = self.buffer.version();
        let mut entries = self.raw_visual_rows.borrow_mut();
        if entries
            .first()
            .is_some_and(|e| e.buffer_version != buffer_version)
        {
            entries.clear();
        }
        if let Some(pos) = entries
            .iter()
            .position(|e| e.buffer_version == buffer_version && e.inner.width() == width)
        {
            let entry = entries.remove(pos);
            entries.insert(0, entry);
        } else {
            let inner = VisualRowCache::build(self.buffer.line_count(), width, |i| {
                let text = line_text_trimmed(&self.buffer, i);
                crate::ui::line_render::visual_rows_of_str(&text, width).len()
            });
            entries.insert(
                0,
                RawVisualRowCache {
                    buffer_version,
                    inner,
                },
            );
            entries.truncate(LRU_CAP);
        }
        drop(entries);
        let borrow = self.raw_visual_rows.borrow();
        f(&borrow
            .first()
            .expect("raw visual cache populated above")
            .inner)
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
                use crate::editor::effective_rows::RowHit;
                match self
                    .effective_rows(width.max(1))
                    .line_at_visual_row(visual_row)
                {
                    RowHit::Raw { raw_line, .. } => {
                        // Inside a revealed reflowed paragraph: snap to that raw source line's
                        // start (`raw_line` is block-relative).
                        let cursor_byte = self.buffer.rope().char_to_byte(self.cursor.offset);
                        let Some(range) =
                            self.parsed.source_map.original_range_for_byte(cursor_byte)
                        else {
                            return self.buffer.len_chars();
                        };
                        let first_line = self.buffer.rope().byte_to_line(range.start);
                        let target =
                            (first_line + raw_line).min(self.buffer.line_count().saturating_sub(1));
                        self.buffer
                            .line_to_char(target)
                            .min(self.buffer.len_chars())
                    }
                    RowHit::Rendered { line: line_idx, .. } => {
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
    }
}
