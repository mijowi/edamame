use std::ops::Range;

/// Maps rendered lines to the source blocks that produced them and back, for the hybrid
/// rendered/raw editing view (see `docs/dev/editing-model.md`).
///
/// Block byte ranges come from [`crate::markdown::parse_offsets`]; `ParsedDoc` extends them so
/// blank-line gaps are absorbed and every source byte belongs to exactly one block.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    /// Per rendered line, the block index that produced it.
    rendered_to_block: Vec<usize>,

    /// Per block, the gap-covering byte range used for cursor → block lookup.
    extended_ranges: Vec<Range<usize>>,

    /// Per block, the exact pulldown-cmark byte range, used to extract raw source for editing.
    original_ranges: Vec<Range<usize>>,

    /// Per block, its rendered-line range, precomputed so the query is O(1). Blocks that produced
    /// no rendered lines inherit the nearest neighbor's range so the result is never empty.
    block_to_rendered_range: Vec<Range<usize>>,

    /// Total source bytes, for proptest assertions in `tests/source_map.rs`.
    #[allow(dead_code)]
    pub total_bytes: usize,
}

impl SourceMap {
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

    /// Block whose extended range owns `byte_offset`; the last block for an offset at or past
    /// the end. `None` only for an empty document.
    pub fn block_for_byte(&self, byte_offset: usize) -> Option<usize> {
        self.extended_ranges
            .iter()
            .position(|r| r.start <= byte_offset && byte_offset < r.end)
            .or_else(|| {
                if !self.extended_ranges.is_empty() {
                    Some(self.extended_ranges.len() - 1)
                } else {
                    None
                }
            })
    }

    /// Rendered-line range produced by `block_idx`; never empty for a known block (see
    /// `block_to_rendered_range`).
    pub fn rendered_lines_for_block(&self, block_idx: usize) -> Range<usize> {
        self.block_to_rendered_range
            .get(block_idx)
            .cloned()
            .unwrap_or(0..0)
    }

    /// Rendered-line range of the block containing `byte_offset`; `0..0` for an empty map.
    pub fn rendered_lines_for_byte(&self, byte_offset: usize) -> Range<usize> {
        match self.block_for_byte(byte_offset) {
            Some(block_idx) => self.rendered_lines_for_block(block_idx),
            None => 0..0,
        }
    }

    /// Original (not extended) byte range of the block containing `byte_offset`.
    pub fn original_range_for_byte(&self, byte_offset: usize) -> Option<Range<usize>> {
        let block = self.block_for_byte(byte_offset)?;
        self.original_ranges.get(block).cloned()
    }

    pub fn rendered_line_count(&self) -> usize {
        self.rendered_to_block.len()
    }

    /// Block count *including* the virtual block per blank line — this index space is the source
    /// map's own, never `ParsedDoc::blocks`'.
    pub fn block_count(&self) -> usize {
        self.extended_ranges.len()
    }

    /// The block that produced `rendered_line`.
    pub fn block_for_rendered_line(&self, rendered_line: usize) -> Option<usize> {
        self.rendered_to_block.get(rendered_line).copied()
    }

    /// Original byte-range start of the block that produced `rendered_line`.
    pub fn original_byte_for_rendered_line(&self, rendered_line: usize) -> Option<usize> {
        let block_idx = self.block_for_rendered_line(rendered_line)?;
        self.original_ranges.get(block_idx).map(|r| r.start)
    }

    /// Original byte range of `block_idx`, `None` when out of range.
    pub fn original_range_for_block(&self, block_idx: usize) -> Option<Range<usize>> {
        self.original_ranges.get(block_idx).cloned()
    }
}

/// Build the per-block rendered-line table; blocks with no rendered lines borrow one line from
/// the nearest following (else preceding) block so every range is non-empty when any line exists.
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
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

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
        assert_eq!(map.rendered_lines_for_byte(5), 0..2);
        assert_eq!(map.rendered_lines_for_byte(12), 2..5);
    }

    #[test]
    fn original_range_for_byte() {
        let map = SourceMap::new(vec![0, 1], vec![0..10, 10..20], vec![2..9, 11..19], 20);
        assert_eq!(map.original_range_for_byte(5), Some(2..9));
        assert_eq!(map.original_range_for_byte(15), Some(11..19));
    }

    // ── Coverage invariants ───────────────────────────────────────────────────

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
