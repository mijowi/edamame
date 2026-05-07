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
}
