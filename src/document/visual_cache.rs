//! Reusable per-(width) visual-row prefix-sum cache.
//!
//! Both rendered-mode (`ParsedDoc`) and raw-mode (`EditorState`) need to
//! answer the same set of questions per scroll event:
//!
//! - How many visual rows does line `i` occupy at width `w`?
//! - How many visual rows do lines `[0..i)` occupy?
//! - Which line contains visual row `r`, and which sub-row within it?
//! - What's the document's total visual row count?
//!
//! Without a cache each answer is O(N) in the line count and re-runs the
//! wrap algorithm per line.  A long document plus a fast trackpad swipe
//! produces ~2·N wrap calls per queued scroll event, which can saturate a
//! CPU core and produce visible lag (the events back up faster than they
//! drain).
//!
//! The cache stores per-line counts plus a prefix sum, making all four
//! queries O(1) (or O(log N) for `find_visual_row` via binary search).
//! It is invalidated by a width change; callers needing additional
//! invalidation keys (e.g. raw mode tracking buffer mutations) wrap the
//! cache in their own staleness check.

/// Per-(line_count, width) prefix-sum table over a sequence of lines so
/// scroll arithmetic and visual-row → line lookups are O(1) / O(log N).
#[derive(Debug, Clone)]
pub(crate) struct VisualRowCache {
    /// Viewport width this cache was built for.  A mismatch with the
    /// caller's width forces a refill.
    pub(crate) width: usize,
    /// `visual_rows_per_line[i]` = wrapped row count of line `i` at `width`,
    /// always at least 1.
    pub(crate) visual_rows_per_line: Vec<usize>,
    /// `visual_row_prefix_sum[i]` = sum of `visual_rows_per_line[0..i]`.
    /// Length is `visual_rows_per_line.len() + 1`; `[0] == 0`,
    /// `[len()]` is the total visual row count.
    pub(crate) visual_row_prefix_sum: Vec<usize>,
}

impl VisualRowCache {
    /// Build a cache by invoking `rows_for(idx)` for each `idx` in
    /// `0..line_count`.  The returned row count is clamped to at least 1
    /// to match the renderer's behaviour for empty / blank lines.
    pub(crate) fn build<F>(line_count: usize, width: usize, mut rows_for: F) -> Self
    where
        F: FnMut(usize) -> usize,
    {
        let mut per_line = Vec::with_capacity(line_count);
        let mut prefix = Vec::with_capacity(line_count + 1);
        prefix.push(0usize);
        let mut acc = 0usize;
        for idx in 0..line_count {
            let rows = rows_for(idx).max(1);
            per_line.push(rows);
            acc = acc.saturating_add(rows);
            prefix.push(acc);
        }
        Self {
            width,
            visual_rows_per_line: per_line,
            visual_row_prefix_sum: prefix,
        }
    }

    /// Width this cache was built for.
    pub(crate) fn width(&self) -> usize {
        self.width
    }

    /// Visual rows occupied by line `idx`.  Returns 1 for out-of-range
    /// indices (matches the `.max(1)` clamp used by callers historically).
    pub(crate) fn for_line(&self, idx: usize) -> usize {
        self.visual_rows_per_line.get(idx).copied().unwrap_or(1)
    }

    /// Sum of visual rows occupied by lines `[0..idx)`.  Saturates at the
    /// total when `idx > line_count`.
    pub(crate) fn before(&self, idx: usize) -> usize {
        let clamped = idx.min(self.visual_rows_per_line.len());
        self.visual_row_prefix_sum
            .get(clamped)
            .copied()
            .unwrap_or(0)
    }

    /// Sum of visual rows occupied by lines `[first..=last]`.  Returns 0
    /// for an empty cache or a reversed range.
    #[allow(dead_code)]
    pub(crate) fn between(&self, first: usize, last: usize) -> usize {
        if first > last || self.visual_rows_per_line.is_empty() {
            return 0;
        }
        let last = last.min(self.visual_rows_per_line.len() - 1);
        self.before(last + 1).saturating_sub(self.before(first))
    }

    /// Total visual rows across all lines.
    pub(crate) fn total(&self) -> usize {
        self.before(self.visual_rows_per_line.len())
    }

    /// Return `(line_idx, sub_row)` for a document-level visual row.
    ///
    /// `sub_row` is the wrapped row within `line_idx`.  If `visual_row` is
    /// past the end of the document, returns `(line_count, 0)` so callers
    /// can stop rendering without special-case arithmetic.  O(log N) via
    /// binary search on the prefix sum.
    pub(crate) fn find_visual_row(&self, visual_row: usize) -> (usize, usize) {
        let total = self.total();
        if visual_row >= total {
            return (self.visual_rows_per_line.len(), 0);
        }
        // `prefix[i] <= visual_row < prefix[i+1]` — find the smallest `i`
        // where `prefix[i+1] > visual_row` via `partition_point`.
        let target = visual_row + 1;
        let upper = self.visual_row_prefix_sum.partition_point(|&p| p < target);
        // `upper` is the index in the prefix array where `prefix >= target`.
        // The line index is `upper - 1` because prefix has one more entry
        // than there are lines.
        let line = upper.saturating_sub(1);
        let start = self.visual_row_prefix_sum[line];
        (line, visual_row - start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cache_reports_zero_total() {
        let cache = VisualRowCache::build(0, 80, |_| 1);
        assert_eq!(cache.total(), 0);
        assert_eq!(cache.before(0), 0);
        assert_eq!(cache.find_visual_row(0), (0, 0));
    }

    #[test]
    fn prefix_sum_matches_per_line_counts() {
        // 5 lines with row counts [1, 2, 1, 3, 1] → prefix [0,1,3,4,7,8].
        let counts = [1usize, 2, 1, 3, 1];
        let cache = VisualRowCache::build(counts.len(), 80, |i| counts[i]);
        assert_eq!(cache.total(), 8);
        assert_eq!(cache.before(0), 0);
        assert_eq!(cache.before(1), 1);
        assert_eq!(cache.before(3), 4);
        assert_eq!(cache.before(5), 8);
        assert_eq!(cache.between(1, 3), 2 + 1 + 3);
        assert_eq!(cache.for_line(2), 1);
    }

    #[test]
    fn find_visual_row_lands_on_correct_line_and_subrow() {
        // Row counts [2, 1, 3] → prefix [0, 2, 3, 6].
        let counts = [2usize, 1, 3];
        let cache = VisualRowCache::build(counts.len(), 80, |i| counts[i]);
        assert_eq!(cache.find_visual_row(0), (0, 0));
        assert_eq!(cache.find_visual_row(1), (0, 1));
        assert_eq!(cache.find_visual_row(2), (1, 0));
        assert_eq!(cache.find_visual_row(3), (2, 0));
        assert_eq!(cache.find_visual_row(4), (2, 1));
        assert_eq!(cache.find_visual_row(5), (2, 2));
        // Past EOF.
        assert_eq!(cache.find_visual_row(6), (3, 0));
        assert_eq!(cache.find_visual_row(100), (3, 0));
    }

    #[test]
    fn rows_for_zero_is_clamped_to_one() {
        let cache = VisualRowCache::build(2, 80, |_| 0);
        assert_eq!(cache.for_line(0), 1);
        assert_eq!(cache.total(), 2);
    }
}
