use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::ops::Range;

/// Extract the byte range of each top-level block in `source`.
///
/// Returns one `Range<usize>` per top-level block, in document order. The
/// ranges cover the complete raw bytes of each block, including delimiters
/// (e.g. the `# ` prefix for headings, triple-backtick fences for code
/// blocks).
///
/// Nested blocks (e.g. paragraphs inside blockquotes) are NOT listed
/// separately — only the outermost container's range is recorded.
pub fn top_level_block_ranges(source: &str) -> Vec<Range<usize>> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_SMART_PUNCTUATION;

    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut depth: usize = 0;
    let mut block_start: usize = 0;

    for (event, byte_range) in Parser::new_ext(source, options).into_offset_iter() {
        match &event {
            Event::Start(tag) if is_block_tag(tag) => {
                if depth == 0 {
                    block_start = byte_range.start;
                }
                depth += 1;
            }
            Event::End(tag_end) if is_block_end_tag(tag_end) => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        // Extend end to include any trailing newline not covered
                        // by the event range (pulldown-cmark sometimes stops short).
                        let end = advance_past_newline(source, byte_range.end);
                        ranges.push(block_start..end);
                    }
                }
            }
            // HorizontalRule and HTML are leaf events (no Start/End pair).
            Event::Rule | Event::Html(_) => {
                if depth == 0 {
                    let end = advance_past_newline(source, byte_range.end);
                    ranges.push(byte_range.start..end);
                }
            }
            _ => {}
        }
    }

    ranges
}

/// Build a covering partition of `0..total_bytes` from `block_ranges`.
///
/// Gaps between blocks (blank lines, leading/trailing whitespace) are merged
/// into the adjacent block's range:
/// - Leading gap → first block
/// - Gap between blocks → previous block (its range is extended forward)
/// - Trailing gap → last block
///
/// Returns one extended `Range<usize>` per block, non-overlapping, covering
/// `0..total_bytes` completely. If `block_ranges` is empty and `total_bytes > 0`,
/// returns a single range `0..total_bytes`.
pub fn covering_ranges(block_ranges: &[Range<usize>], total_bytes: usize) -> Vec<Range<usize>> {
    if total_bytes == 0 {
        return Vec::new();
    }
    if block_ranges.is_empty() {
        return vec![0..total_bytes];
    }

    let n = block_ranges.len();
    let mut result = Vec::with_capacity(n);

    for i in 0..n {
        // Extended start: the first block starts at 0 (covering any leading
        // whitespace); subsequent blocks start at their own original start.
        let start = if i == 0 { 0 } else { block_ranges[i].start };

        // Extended end: stretch to the start of the next block (absorbing any
        // gap between this block and the next). The last block stretches to
        // total_bytes.
        let end = if i + 1 < n {
            block_ranges[i + 1].start
        } else {
            total_bytes.max(block_ranges[i].end)
        };

        // Sanity: never allow an inverted range (shouldn't happen for valid input).
        let start = start.min(end);
        result.push(start..end);
    }

    result
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn is_block_tag(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::CodeBlock(_)
            | Tag::BlockQuote(_)
            | Tag::List(_)
            | Tag::Table(_)
            | Tag::HtmlBlock
    )
}

fn is_block_end_tag(tag_end: &TagEnd) -> bool {
    matches!(
        tag_end,
        TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::CodeBlock
            | TagEnd::BlockQuote(_)
            | TagEnd::List(_)
            | TagEnd::Table
            | TagEnd::HtmlBlock
    )
}

/// Advance `pos` past any single `\n` at `source[pos]` (so the range includes
/// it). Used to capture trailing newlines that pulldown-cmark sometimes excludes
/// from block event ranges.
fn advance_past_newline(source: &str, pos: usize) -> usize {
    if source.as_bytes().get(pos) == Some(&b'\n') {
        pos + 1
    } else {
        pos
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_paragraph() {
        let src = "Hello world\n";
        let ranges = top_level_block_ranges(src);
        assert_eq!(ranges.len(), 1);
        assert_eq!(&src[ranges[0].clone()], "Hello world\n");
    }

    #[test]
    fn heading_and_paragraph() {
        let src = "# Heading\n\nParagraph\n";
        let ranges = top_level_block_ranges(src);
        assert_eq!(ranges.len(), 2, "expected 2 blocks, got: {:?}", ranges);
        // First block is the heading.
        assert!(src[ranges[0].clone()].contains("Heading"));
        // Second block is the paragraph.
        assert!(src[ranges[1].clone()].contains("Paragraph"));
    }

    #[test]
    fn code_block_and_paragraph() {
        let src = "```\ncode\n```\n\nText\n";
        let ranges = top_level_block_ranges(src);
        assert_eq!(ranges.len(), 2);
        assert!(src[ranges[0].clone()].contains("code"));
        assert!(src[ranges[1].clone()].contains("Text"));
    }

    #[test]
    fn horizontal_rule() {
        let src = "---\n";
        let ranges = top_level_block_ranges(src);
        assert_eq!(ranges.len(), 1);
    }

    #[test]
    fn covering_ranges_no_gaps() {
        // Block ranges already abut perfectly.
        let blocks = vec![0..5, 5..10];
        let covered = covering_ranges(&blocks, 10);
        assert_eq!(covered, vec![0..5, 5..10]);
    }

    #[test]
    fn covering_ranges_with_gaps() {
        // Gap between blocks: byte 8 (between 0..8 and 9..15).
        let blocks = vec![0..8, 9..15];
        let covered = covering_ranges(&blocks, 15);
        // Gap byte 8 is absorbed into block 0's extended range (its end stretches
        // to the start of block 1).
        assert_eq!(covered[0], 0..9);
        assert_eq!(covered[1], 9..15);
    }

    #[test]
    fn covering_ranges_leading_gap() {
        // Document starts with a blank line (byte 0 is '\n'), first block at byte 1.
        let blocks = vec![1..8];
        let covered = covering_ranges(&blocks, 8);
        assert_eq!(covered[0], 0..8); // starts at 0
    }

    #[test]
    fn covering_ranges_trailing_gap() {
        let blocks = vec![0..8];
        let covered = covering_ranges(&blocks, 10); // 2 bytes after the block
        assert_eq!(covered[0], 0..10);
    }

    #[test]
    fn covering_ranges_empty_blocks() {
        let covered = covering_ranges(&[], 5);
        assert_eq!(covered, vec![0..5]);
    }

    #[test]
    fn covering_ranges_zero_bytes() {
        let covered = covering_ranges(&[], 0);
        assert!(covered.is_empty());
    }

    #[test]
    fn covering_covers_all_bytes() {
        let src = "# Hello\n\nWorld\n\n---\n";
        let ranges = top_level_block_ranges(src);
        let covered = covering_ranges(&ranges, src.len());
        // Every byte 0..src.len() must be in exactly one range.
        let mut seen = vec![false; src.len()];
        for r in &covered {
            for b in r.clone() {
                assert!(!seen[b], "byte {} covered twice", b);
                seen[b] = true;
            }
        }
        for (i, s) in seen.iter().enumerate() {
            assert!(s, "byte {} not covered", i);
        }
    }
}
