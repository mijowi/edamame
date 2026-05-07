use std::ops::Range;

/// Maps rendered visual lines to their source byte ranges, enabling the hybrid
/// rendered/raw editing view.
///
/// # How it is built
///
/// The renderer produces one rendered line per block element (headings,
/// paragraphs, etc.). The [`crate::markdown::parse_offsets`] module provides
/// the byte range of each top-level block. Pairing these gives: for rendered
/// line `i`, the source byte range `entries[i]`.
///
/// Gaps between blocks (blank lines in the source) are absorbed into the
/// adjacent block's extended range via
/// [`crate::markdown::parse_offsets::covering_ranges`], so every source byte
/// is covered by exactly one rendered line.
///
/// # Key operations
///
/// - Given a cursor char offset → byte offset → find which rendered line(s)
///   correspond to the same source block.
/// - Given a source block → find all rendered lines it produced.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    /// For each rendered line: the block index that produced it.
    /// `rendered_to_block[i]` = which block (0-indexed) generated rendered line i.
    rendered_to_block: Vec<usize>,

    /// For each block: the *extended* byte range (covering gaps) that this
    /// block "owns". Used for cursor-to-block lookup.
    extended_ranges: Vec<Range<usize>>,

    /// For each block: the *original* byte range from pulldown-cmark (used to
    /// extract raw source text for editing).
    original_ranges: Vec<Range<usize>>,

    /// Precomputed `block_idx → rendered-line Range` lookup.  Mirrors the
    /// answer `rendered_lines_for_block` used to compute via two
    /// O(n) scans of `rendered_to_block`; storing the ranges once at
    /// construction turns the query into O(1).  Blocks that produced
    /// no rendered lines (empty list items, collapsed blanks) inherit
    /// the nearest neighbour's range — same fallback semantics the
    /// uncached version used.
    block_to_rendered_range: Vec<Range<usize>>,

    /// Total source bytes (for proptest assertions in `tests/source_map.rs`).
    #[allow(dead_code)]
    pub total_bytes: usize,
}

impl SourceMap {
    /// Construct a SourceMap.
    ///
    /// - `rendered_to_block`: per rendered line, which block produced it.
    /// - `extended_ranges`: per block, the extended range (covering gaps).
    /// - `original_ranges`: per block, the exact pulldown-cmark byte range.
    /// - `total_bytes`: length of the source string in bytes.
    pub fn new(
        rendered_to_block: Vec<usize>,
        extended_ranges: Vec<Range<usize>>,
        original_ranges: Vec<Range<usize>>,
        total_bytes: usize,
    ) -> Self {
        let block_to_rendered_range =
            build_block_to_rendered_range(&rendered_to_block, extended_ranges.len());
        Self {
            rendered_to_block,
            extended_ranges,
            original_ranges,
            block_to_rendered_range,
            total_bytes,
        }
    }

    /// Find the block index that "owns" `byte_offset` (using extended ranges).
    ///
    /// Returns `None` only for empty documents (no blocks).
    pub fn block_for_byte(&self, byte_offset: usize) -> Option<usize> {
        self.extended_ranges
            .iter()
            .position(|r| r.start <= byte_offset && byte_offset < r.end)
            // If not found (e.g. byte == total_bytes at exact end), return the last block.
            .or_else(|| {
                if !self.extended_ranges.is_empty() {
                    Some(self.extended_ranges.len() - 1)
                } else {
                    None
                }
            })
    }

    /// Return the range of rendered line indices produced by `block_idx`.
    ///
    /// The range is `start..end` (exclusive end). If the block produced no
    /// rendered lines (e.g. an empty list item before the renderer fix), the
    /// precomputed table returns the nearest adjacent block's lines to
    /// guarantee a non-empty range. O(1) — the work happens once in `new`.
    pub fn rendered_lines_for_block(&self, block_idx: usize) -> Range<usize> {
        self.block_to_rendered_range
            .get(block_idx)
            .cloned()
            .unwrap_or(0..0)
    }

    /// Return all rendered line indices produced by the block that contains
    /// `byte_offset`, as a contiguous `Range`. Returns `0..0` for empty maps.
    pub fn rendered_lines_for_byte(&self, byte_offset: usize) -> Range<usize> {
        match self.block_for_byte(byte_offset) {
            Some(block_idx) => self.rendered_lines_for_block(block_idx),
            None => 0..0,
        }
    }

    /// Return the **original** (not extended) byte range of the block that
    /// contains `byte_offset`. Used to extract raw source text for editing.
    pub fn original_range_for_byte(&self, byte_offset: usize) -> Option<Range<usize>> {
        let block = self.block_for_byte(byte_offset)?;
        self.original_ranges.get(block).cloned()
    }

    /// Total number of rendered lines tracked by this map.
    /// Used by integration tests in `tests/source_map.rs`.
    #[allow(dead_code)]
    pub fn rendered_line_count(&self) -> usize {
        self.rendered_to_block.len()
    }

    /// Total number of blocks. Used by integration tests in
    /// `tests/source_map.rs`.
    #[allow(dead_code)]
    pub fn block_count(&self) -> usize {
        self.extended_ranges.len()
    }

    /// Return the original byte range start for the block that contains
    /// `rendered_line`. Used to sync the cursor to the scroll position when
    /// entering edit mode from preview mode.
    pub fn original_byte_for_rendered_line(&self, rendered_line: usize) -> Option<usize> {
        let block_idx = *self.rendered_to_block.get(rendered_line)?;
        self.original_ranges.get(block_idx).map(|r| r.start)
    }

    /// Return the original byte range for `block_idx`, or `None` if the
    /// index is out of range.  Symmetric with `rendered_lines_for_block`.
    pub fn original_range_for_block(&self, block_idx: usize) -> Option<Range<usize>> {
        self.original_ranges.get(block_idx).cloned()
    }
}

/// Precompute the per-block rendered-line range table.  Single pass
/// over `rendered_to_block` records start / end per block; a second
/// pass fills empty-range slots from the nearest neighbour so the
/// fallback semantics of the old O(n) scan (always return a
/// non-empty range when at least one line exists) are preserved.
fn build_block_to_rendered_range(
    rendered_to_block: &[usize],
    block_count: usize,
) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = vec![0..0; block_count];
    let mut seen = vec![false; block_count];
    for (line_idx, &block_idx) in rendered_to_block.iter().enumerate() {
        if block_idx >= block_count {
            continue;
        }
        if !seen[block_idx] {
            ranges[block_idx] = line_idx..line_idx + 1;
            seen[block_idx] = true;
        } else {
            ranges[block_idx].end = line_idx + 1;
        }
    }
    let n = rendered_to_block.len();
    if n == 0 {
        return ranges;
    }
    // Fallback: blocks that produced no rendered lines inherit the
    // nearest subsequent-then-preceding neighbour's range.  Matches
    // the old uncached fallback so callers observe no behavioural
    // change.
    for i in 0..block_count {
        if !seen[i] {
            let mut fallback: Option<Range<usize>> = None;
            for next in (i + 1)..block_count {
                if seen[next] {
                    let start = ranges[next].start;
                    fallback = Some(start..(start + 1).min(n));
                    break;
                }
            }
            if fallback.is_none() {
                for prev in (0..i).rev() {
                    if seen[prev] {
                        let end = ranges[prev].end;
                        let start = end.saturating_sub(1);
                        fallback = Some(start..end);
                        break;
                    }
                }
            }
            ranges[i] = fallback.unwrap_or(0..1.min(n));
        }
    }
    ranges
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a simple SourceMap with one block owning rendered lines 0..n_lines.
    fn single_block_map(n_lines: usize, byte_len: usize) -> SourceMap {
        SourceMap::new(
            vec![0usize; n_lines],
            vec![0..byte_len],
            vec![0..byte_len],
            byte_len,
        )
    }

    #[test]
    fn block_for_byte_single_block() {
        let map = single_block_map(3, 20);
        assert_eq!(map.block_for_byte(0), Some(0));
        assert_eq!(map.block_for_byte(10), Some(0));
        assert_eq!(map.block_for_byte(19), Some(0));
    }

    #[test]
    fn block_for_byte_two_blocks() {
        let map = SourceMap::new(
            vec![0, 0, 1, 1],
            vec![0..10, 10..20],
            vec![0..10, 10..20],
            20,
        );
        assert_eq!(map.block_for_byte(5), Some(0));
        assert_eq!(map.block_for_byte(10), Some(1));
        assert_eq!(map.block_for_byte(15), Some(1));
    }

    #[test]
    fn block_for_byte_empty_map() {
        let map = SourceMap::default();
        assert_eq!(map.block_for_byte(0), None);
    }

    #[test]
    fn rendered_lines_for_block_basic() {
        let map = SourceMap::new(
            vec![0, 0, 1, 1, 1, 2],
            vec![0..5, 5..10, 10..15],
            vec![0..5, 5..10, 10..15],
            15,
        );
        assert_eq!(map.rendered_lines_for_block(0), 0..2);
        assert_eq!(map.rendered_lines_for_block(1), 2..5);
        assert_eq!(map.rendered_lines_for_block(2), 5..6);
    }

    #[test]
    fn rendered_lines_for_byte() {
        let map = SourceMap::new(
            vec![0, 0, 1, 1, 1],
            vec![0..10, 10..20],
            vec![0..10, 10..20],
            20,
        );
        assert_eq!(map.rendered_lines_for_byte(5), 0..2); // in block 0
        assert_eq!(map.rendered_lines_for_byte(12), 2..5); // in block 1
    }

    #[test]
    fn original_range_for_byte() {
        let map = SourceMap::new(
            vec![0, 1],
            vec![0..10, 10..20], // extended (no gaps here)
            vec![2..9, 11..19],  // original ranges (smaller)
            20,
        );
        assert_eq!(map.original_range_for_byte(5), Some(2..9));
        assert_eq!(map.original_range_for_byte(15), Some(11..19));
    }

    // ── Proptest-style invariant checks (deterministic) ───────────────────────

    #[test]
    fn every_byte_maps_to_some_line_single_block() {
        let map = single_block_map(2, 10);
        for b in 0..10 {
            let range = map.rendered_lines_for_byte(b);
            assert!(
                !range.is_empty(),
                "byte {} did not map to any rendered line",
                b
            );
        }
    }

    #[test]
    fn every_byte_maps_to_some_line_two_blocks() {
        let map = SourceMap::new(vec![0, 0, 1, 1], vec![0..8, 8..16], vec![0..8, 8..16], 16);
        for b in 0..16 {
            let range = map.rendered_lines_for_byte(b);
            assert!(
                !range.is_empty(),
                "byte {} did not map to any rendered line",
                b
            );
        }
    }
}
