//! Per-(ParsedDoc, viewport-width) cache of visual-row counts.
//!
//! The cache is populated lazily on first query at a given width and reused
//! across scroll-only frames so the snapshot builders (`link_view`,
//! `image_view`, `table_view`) and scroll arithmetic
//! (`EditorState::scroll_for_last_visible`, `visual_rows_between`) don't
//! re-walk and re-allocate per call.  A terminal resize triggers a single
//! rebuild at the new width — `App` debounces resizes so this is rare.

use super::parsed_doc::ParsedDoc;

/// Per-(ParsedDoc, width) prefix-sum table over `lines` so the snapshot
/// builders can answer "how many visual rows do lines [0..i) consume?"
/// in O(1).  Replaces the per-block walks that dominated scroll-path
/// CPU on large documents prior to Phase 15.
#[derive(Debug, Clone)]
pub(super) struct VisualRowCache {
    /// Viewport width this cache was built for.  A mismatch with the
    /// caller's width forces a refill.
    pub(super) width: usize,
    /// `visual_rows_per_line[i]` = `visual_rows_for_line(&lines[i], width).max(1)`.
    pub(super) visual_rows_per_line: Vec<usize>,
    /// `visual_row_prefix_sum[i]` = sum of `visual_rows_per_line[0..i]`.
    /// Length is `lines.len() + 1`; `[0] == 0`, `[lines.len()]` is the
    /// total visual row count.
    pub(super) visual_row_prefix_sum: Vec<usize>,
}

impl ParsedDoc {
    /// Number of visual rows rendered line `idx` occupies at `width`.
    /// Returns 1 for out-of-range indices (matches the `.max(1)` clamp
    /// the snapshot builders applied historically).  O(1) after the
    /// cache is populated; first call at a given width is O(lines).
    pub fn visual_rows_for_line_at(&self, idx: usize, width: usize) -> usize {
        self.ensure_visual_rows(width);
        self.visual_rows
            .borrow()
            .as_ref()
            .and_then(|c| c.visual_rows_per_line.get(idx).copied())
            .unwrap_or(1)
    }

    /// Sum of visual rows occupied by rendered lines `[0..idx)` at
    /// `width`.  O(1) after the cache is populated.  Replaces the
    /// per-block `for idx in 0..start` loop in
    /// `link_view::extract_block_links` and the
    /// `for idx in scroll..end` loop in `image_view::build_snapshots`.
    pub fn visual_rows_before(&self, idx: usize, width: usize) -> usize {
        self.ensure_visual_rows(width);
        let clamped = idx.min(self.lines.len());
        self.visual_rows
            .borrow()
            .as_ref()
            .and_then(|c| c.visual_row_prefix_sum.get(clamped).copied())
            .unwrap_or(0)
    }

    /// Sum of visual rows occupied by rendered lines `[first..=last]`
    /// at `width`.  O(1) after the cache is populated.  Used by tests
    /// in this crate.
    #[allow(dead_code)]
    pub fn visual_rows_between(&self, first: usize, last: usize, width: usize) -> usize {
        if first > last || self.lines.is_empty() {
            return 0;
        }
        let last = last.min(self.lines.len() - 1);
        self.visual_rows_before(last + 1, width)
            .saturating_sub(self.visual_rows_before(first, width))
    }

    /// Total visual rows occupied by the rendered document at `width`.
    pub fn total_visual_rows(&self, width: usize) -> usize {
        self.visual_rows_before(self.lines.len(), width)
    }

    /// Return `(rendered_line_idx, sub_row)` for a document-level visual row.
    ///
    /// `sub_row` is the wrapped visual row within `rendered_line_idx`.  If the
    /// requested row is past EOF, returns `(lines.len(), 0)` so callers can stop
    /// rendering without special-case arithmetic.
    pub fn line_at_visual_row(&self, visual_row: usize, width: usize) -> (usize, usize) {
        self.ensure_visual_rows(width);
        let borrow = self.visual_rows.borrow();
        let Some(cache) = borrow.as_ref() else {
            return (0, 0);
        };
        for (idx, window) in cache.visual_row_prefix_sum.windows(2).enumerate() {
            let start = window[0];
            let end = window[1];
            if visual_row < end {
                return (idx, visual_row.saturating_sub(start));
            }
        }
        (self.lines.len(), 0)
    }

    /// Populate or refresh the visual-row cache for `width`.  Cheap
    /// when the cached width already matches; otherwise walks every
    /// line once and stores per-line counts plus a prefix sum.
    /// Two-phase borrow: the immutable check releases before the
    /// `borrow_mut` so we don't alias the `RefCell`.
    fn ensure_visual_rows(&self, width: usize) {
        {
            let borrow = self.visual_rows.borrow();
            if let Some(c) = borrow.as_ref() {
                if c.width == width {
                    return;
                }
            }
        }
        let len = self.lines.len();
        let mut per_line = Vec::with_capacity(len);
        let mut prefix = Vec::with_capacity(len + 1);
        prefix.push(0usize);
        let mut acc = 0usize;
        for line in &self.lines {
            // Reuse the canonical wrap algorithm — never duplicate it.
            let rows = crate::ui::line_render::visual_rows_for_line(line, width).max(1);
            per_line.push(rows);
            acc = acc.saturating_add(rows);
            prefix.push(acc);
        }
        *self.visual_rows.borrow_mut() = Some(VisualRowCache {
            width,
            visual_rows_per_line: per_line,
            visual_row_prefix_sum: prefix,
        });
    }
}
