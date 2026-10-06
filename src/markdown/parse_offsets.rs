use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::ops::Range;

/// The block-level constructs [`block_ranges_by`] understands.  Maps both
/// `pulldown_cmark::Tag` and `TagEnd` into one enum so the scanner can pair starts and ends
/// without exposing that asymmetry.  `HtmlLeaf` covers block HTML emitted as a bare
/// `Event::Html(_)`, with no surrounding `Tag::HtmlBlock`.
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
    FootnoteDefinition,
    MetadataBlock,
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
        Tag::FootnoteDefinition(_) => BlockKind::FootnoteDefinition,
        Tag::MetadataBlock(_) => BlockKind::MetadataBlock,
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
        TagEnd::FootnoteDefinition => BlockKind::FootnoteDefinition,
        TagEnd::MetadataBlock(_) => BlockKind::MetadataBlock,
        _ => return None,
    })
}

/// The shared pulldown-cmark option set, minus the two metadata-block extensions — every parse
/// site must go through [`options_for`] instead.
///
/// The AST parse and the offset scans here MUST use the same options: block boundaries shift
/// between option sets, and `ParsedDoc` relies on a 1:1 blocks↔ranges pairing.  Since the
/// metadata half is source-dependent, "the same options" means "the same *source*".
const BASE_OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_FOOTNOTES)
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_SMART_PUNCTUATION)
    .union(Options::ENABLE_MATH);

/// [`BASE_OPTIONS`] plus the metadata-block extension matching `source`'s *own first line* —
/// and only then.
///
/// pulldown-cmark's metadata-block extensions are **not** anchored to the document start: with
/// them on, any later `---`…`---` pair becomes a metadata block.  That is ordinary Markdown
/// separator style (a rule above a heading, a slide break), and the damage is not cosmetic —
/// the section renders as dim key/value data, inline insertion is refused inside it, and the
/// HTML writer emits *nothing* for a metadata block, so an export silently drops content.
/// Frontmatter is by definition the first thing in the file, so gating on the first line costs
/// nothing and confines the extension to where it belongs.  Only the matching flavor is
/// enabled, and a leading blank line means no frontmatter at all.
///
/// Every parse of a document — AST, offset scan, HTML export — must pass that document's own
/// text here, or the 1:1 blocks↔ranges pairing breaks.
pub fn options_for(source: &str) -> Options {
    BASE_OPTIONS.union(metadata_options_for(source))
}

/// Just the metadata-block half of [`options_for`].  Split out so
/// [`crate::export::html::render_html`], which keeps its own base option list, shares the
/// anchoring rule: a parse and an export disagreeing about frontmatter disagree about whether
/// the block survives the export at all.
pub(crate) fn metadata_options_for(source: &str) -> Options {
    // Text is `\n`-normalized before any parse, so the first line carries no trailing `\r`.
    let first_line = source.split('\n').next().unwrap_or("");
    match first_line {
        "---" => Options::ENABLE_YAML_STYLE_METADATA_BLOCKS,
        "+++" => Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS,
        _ => Options::empty(),
    }
}

/// Incremental depth-zero block-range scanner: feed it every `(event, byte_range)` pair from an
/// `into_offset_iter()` parse via [`observe`](Self::observe).
///
/// A struct rather than only the [`block_ranges_by`] loop so
/// [`crate::markdown::parser::parse_raw_with_ranges`] can collect ranges as a side effect of
/// the AST-building parse instead of running a second full pass.
pub struct RangeTracker<F> {
    keep: F,
    depth: usize,
    block_start: usize,
    // Set only when `keep` accepted the depth-0 open, so depth tracking still descends through
    // nested blocks without emitting a range on close.
    open_kept: bool,
    ranges: Vec<Range<usize>>,
}

impl<F: FnMut(BlockKind) -> bool> RangeTracker<F> {
    pub fn new(keep: F) -> Self {
        Self {
            keep,
            depth: 0,
            block_start: 0,
            open_kept: false,
            ranges: Vec::new(),
        }
    }

    #[inline]
    pub fn observe(&mut self, source: &str, event: &Event<'_>, byte_range: &Range<usize>) {
        match event {
            Event::Start(tag) => {
                if let Some(kind) = tag_kind(tag) {
                    if self.depth == 0 {
                        self.block_start = byte_range.start;
                        self.open_kept = (self.keep)(kind);
                    }
                    self.depth += 1;
                }
            }
            Event::End(tag_end) => {
                if tag_end_kind(tag_end).is_some() && self.depth > 0 {
                    self.depth -= 1;
                    if self.depth == 0 && self.open_kept {
                        let end = advance_past_newline(source, byte_range.end);
                        self.ranges.push(self.block_start..end);
                        self.open_kept = false;
                    }
                }
            }
            Event::Rule => {
                if self.depth == 0 && (self.keep)(BlockKind::Rule) {
                    let end = advance_past_newline(source, byte_range.end);
                    self.ranges.push(byte_range.start..end);
                }
            }
            Event::Html(_) if self.depth == 0 && (self.keep)(BlockKind::HtmlLeaf) => {
                let end = advance_past_newline(source, byte_range.end);
                self.ranges.push(byte_range.start..end);
            }
            _ => {}
        }
    }

    pub fn into_ranges(self) -> Vec<Range<usize>> {
        self.ranges
    }
}

/// Walk `source`'s events at depth zero, recording the byte range of every block whose
/// [`BlockKind`] satisfies `keep`.  Used by the diff table-extent scan and by
/// [`top_level_block_ranges`].
pub fn block_ranges_by<F>(source: &str, keep: F) -> Vec<Range<usize>>
where
    F: FnMut(BlockKind) -> bool,
{
    let mut tracker = RangeTracker::new(keep);
    for (event, byte_range) in Parser::new_ext(source, options_for(source)).into_offset_iter() {
        tracker.observe(source, &event, &byte_range);
    }
    tracker.into_ranges()
}

/// One `Range<usize>` per top-level block of `source`, in document order, covering the complete
/// raw bytes including delimiters.  Nested blocks are not listed separately — only the
/// outermost container.
///
/// The editor pipeline gets its ranges from
/// [`crate::markdown::parser::parse_raw_with_ranges`]; this is the standalone entry point for
/// tests.
#[allow(dead_code)]
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
                | BlockKind::FootnoteDefinition
                | BlockKind::MetadataBlock
        )
    })
}

/// Byte range of every `[^label]: …` footnote definition, paired with its raw label, in
/// document order.
///
/// The range covers the definition's *full* extent — the leader line plus indented
/// continuations — so a delete cannot orphan a continuation as an indented code block.  Two
/// leaders for the same label yield two entries: pulldown-cmark renders only the first, but a
/// delete should remove both.
pub fn footnote_definition_ranges(source: &str) -> Vec<(String, Range<usize>)> {
    let options = options_for(source);

    let mut ranges: Vec<(String, Range<usize>)> = Vec::new();
    let mut depth: usize = 0;
    // The open depth-0 definition, if any.  Depth-0 blocks never overlap, so one slot suffices.
    let mut open: Option<(String, usize)> = None;

    for (event, byte_range) in Parser::new_ext(source, options).into_offset_iter() {
        match &event {
            Event::Start(tag) => {
                if let Tag::FootnoteDefinition(label) = tag {
                    if depth == 0 {
                        open = Some((label.to_string(), byte_range.start));
                    }
                }
                if tag_kind(tag).is_some() {
                    depth += 1;
                }
            }
            Event::End(tag_end) if tag_end_kind(tag_end).is_some() && depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some((label, start)) = open.take() {
                        let end = advance_past_newline(source, byte_range.end);
                        ranges.push((label, start..end));
                    }
                }
            }
            _ => {}
        }
    }

    ranges
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Advance `pos` past a single `\n`, capturing the trailing newlines pulldown-cmark sometimes
/// excludes from block event ranges.
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
        // Heading.
        assert!(src[ranges[0].clone()].contains("Heading"));
        // Paragraph.
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
    fn footnote_definition_is_its_own_block() {
        // A footnote definition needs its own range so `ParsedDoc`'s pairing stays 1:1.
        let src = "Intro.[^1]\n\n[^1]: The note.\n\nAfter.\n";
        let ranges = top_level_block_ranges(src);
        assert_eq!(ranges.len(), 3, "expected 3 blocks, got: {ranges:?}");
        assert!(src[ranges[0].clone()].contains("Intro."));
        assert!(src[ranges[1].clone()].contains("[^1]: The note."));
        assert!(src[ranges[2].clone()].contains("After."));
    }

    #[test]
    fn footnote_definition_range_covers_multiline_body() {
        // The range must span the leader plus the indented continuation.
        let src = "A[^1]\n\n[^1]: first line\n    continuation line\n\nAfter.\n";
        let defs = footnote_definition_ranges(src);
        assert_eq!(defs.len(), 1);
        let (label, range) = &defs[0];
        assert_eq!(label, "1");
        let text = &src[range.clone()];
        assert!(text.contains("first line"), "got: {text:?}");
        assert!(text.contains("continuation line"), "got: {text:?}");
        assert!(!text.contains("After."), "should not absorb the next block");
    }

    /// A metadata block's range must cover both delimiter lines and the trailing newline, or
    /// `ParsedDoc`'s blocks↔ranges pairing drifts by a line.
    #[test]
    fn metadata_block_range_covers_both_delimiter_lines() {
        let src = "---\ntitle: Foo\n---\n\n# H\n";
        let ranges = top_level_block_ranges(src);
        assert_eq!(&src[ranges[0].clone()], "---\ntitle: Foo\n---\n");
        assert_eq!(&src[ranges[1].clone()], "# H\n");
    }

    /// Unconditionally enabled, the extensions would let the `---` above `## Section 2` open a
    /// block the next `---` closes, and the section between them stops being prose.
    #[test]
    fn a_mid_document_rule_pair_is_not_frontmatter() {
        let src = "Intro.\n\n---\n## Section 2\n\nText.\n\n---\n## Section 3\n";
        let ranges = top_level_block_ranges(src);
        assert_eq!(&src[ranges[1].clone()], "---\n", "got: {ranges:?}");
        assert_eq!(&src[ranges[2].clone()], "## Section 2\n\n");
    }

    /// Only the flavor the first line names is enabled.
    #[test]
    fn options_enable_only_the_flavor_the_first_line_opens() {
        assert_eq!(
            metadata_options_for("---\ntitle: Foo\n---\n"),
            Options::ENABLE_YAML_STYLE_METADATA_BLOCKS,
        );
        assert_eq!(
            metadata_options_for("+++\ntitle = \"Foo\"\n+++\n"),
            Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS,
        );
        // None of these open a metadata block.  CRLF is untested because text is
        // `\n`-normalized before reaching this function.
        for src in [
            "\n---\na: 1\n---\n",
            " ---\na: 1\n---\n",
            "----\na: 1\n----\n",
            "--- yaml\na: 1\n---\n",
            "",
        ] {
            assert_eq!(metadata_options_for(src), Options::empty(), "got: {src:?}");
        }
    }

    #[test]
    fn a_rule_is_not_a_metadata_block() {
        // No closing delimiter, so the `---` stays a thematic break.
        let src = "---\ntitle: Foo\n\n# H\n";
        let ranges = top_level_block_ranges(src);
        assert_eq!(ranges.len(), 3, "got: {ranges:?}");
        assert_eq!(&src[ranges[0].clone()], "---\n");
    }
}
