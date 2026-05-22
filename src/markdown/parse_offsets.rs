use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::ops::Range;

/// Symbolic identifier for the kinds of block-level construct the
/// shared scanner [`block_ranges_by`] understands.  Maps both
/// `pulldown_cmark::Tag` (Start) and `TagEnd` (End) into a single
/// enum so the scanner can pair starts and ends cleanly without
/// exposing the `Tag` / `TagEnd` asymmetry to callers.
///
/// `HtmlLeaf` covers block-level HTML emitted by pulldown-cmark as a
/// bare `Event::Html(_)` (no surrounding `Tag::HtmlBlock`) — the
/// scanner records its byte range when seen at depth zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Heading,
    CodeBlock,
    BlockQuote,
    List,
    Table,
    HtmlBlock,
    Rule,
    HtmlLeaf,
}

fn tag_kind(tag: &Tag<'_>) -> Option<BlockKind> {
    Some(match tag {
        Tag::Paragraph => BlockKind::Paragraph,
        Tag::Heading { .. } => BlockKind::Heading,
        Tag::CodeBlock(_) => BlockKind::CodeBlock,
        Tag::BlockQuote(_) => BlockKind::BlockQuote,
        Tag::List(_) => BlockKind::List,
        Tag::Table(_) => BlockKind::Table,
        Tag::HtmlBlock => BlockKind::HtmlBlock,
        _ => return None,
    })
}

fn tag_end_kind(tag_end: &TagEnd) -> Option<BlockKind> {
    Some(match tag_end {
        TagEnd::Paragraph => BlockKind::Paragraph,
        TagEnd::Heading(_) => BlockKind::Heading,
        TagEnd::CodeBlock => BlockKind::CodeBlock,
        TagEnd::BlockQuote(_) => BlockKind::BlockQuote,
        TagEnd::List(_) => BlockKind::List,
        TagEnd::Table => BlockKind::Table,
        TagEnd::HtmlBlock => BlockKind::HtmlBlock,
        _ => return None,
    })
}

/// Walk `source`'s pulldown-cmark events at depth zero, recording the
/// byte range of every block whose [`BlockKind`] satisfies `keep`.
///
/// Used by [`top_level_block_ranges`] (covers all block kinds) and by
/// the diff subsystem's table-extent scan (filters to `Table` only).
/// Centralizing the depth-tracking + trailing-newline logic in one
/// place keeps every block scanner honest about the same edge cases.
pub fn block_ranges_by<F>(source: &str, mut keep: F) -> Vec<Range<usize>>
where
    F: FnMut(BlockKind) -> bool,
{
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_SMART_PUNCTUATION;

    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut depth: usize = 0;
    let mut block_start: usize = 0;
    // The kind we opened at depth==0 — recorded only when `keep`
    // accepted it, so depth tracking still increments through
    // nested-block descents but we don't emit a range on close.
    let mut open_kept: bool = false;

    for (event, byte_range) in Parser::new_ext(source, options).into_offset_iter() {
        match &event {
            Event::Start(tag) => {
                if let Some(kind) = tag_kind(tag) {
                    if depth == 0 {
                        block_start = byte_range.start;
                        open_kept = keep(kind);
                    }
                    depth += 1;
                }
            }
            Event::End(tag_end) => {
                if tag_end_kind(tag_end).is_some() && depth > 0 {
                    depth -= 1;
                    if depth == 0 && open_kept {
                        let end = advance_past_newline(source, byte_range.end);
                        ranges.push(block_start..end);
                        open_kept = false;
                    }
                }
            }
            Event::Rule => {
                if depth == 0 && keep(BlockKind::Rule) {
                    let end = advance_past_newline(source, byte_range.end);
                    ranges.push(byte_range.start..end);
                }
            }
            Event::Html(_) => {
                if depth == 0 && keep(BlockKind::HtmlLeaf) {
                    let end = advance_past_newline(source, byte_range.end);
                    ranges.push(byte_range.start..end);
                }
            }
            _ => {}
        }
    }

    ranges
}

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
    block_ranges_by(source, |kind| {
        matches!(
            kind,
            BlockKind::Paragraph
                | BlockKind::Heading
                | BlockKind::CodeBlock
                | BlockKind::BlockQuote
                | BlockKind::List
                | BlockKind::Table
                | BlockKind::HtmlBlock
                | BlockKind::Rule
                | BlockKind::HtmlLeaf
        )
    })
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

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
}
