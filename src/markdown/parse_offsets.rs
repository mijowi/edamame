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

/// The pulldown-cmark option set shared by every parse in the crate,
/// minus the two metadata-block extensions — take the full set from
/// [`options_for`], which is what every parse site must call.
///
/// The AST parse ([`crate::markdown::parser::parse_raw`]) and the offset
/// scans here MUST use the same options — block boundaries shift between
/// option sets, and `ParsedDoc` relies on a 1:1 blocks↔ranges pairing.
/// Because the metadata half is now source-dependent, "the same options"
/// means "the same *source* through `options_for`".
const BASE_OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_FOOTNOTES)
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_SMART_PUNCTUATION);

/// The option set to parse `source` with: [`BASE_OPTIONS`], plus the
/// metadata-block extension matching `source`'s *own first line* — and
/// only then.
///
/// pulldown-cmark's metadata-block extensions are **not** anchored to the
/// start of the document: with them on, any later `---` line followed by
/// non-blank text and eventually closed by another `---` becomes a
/// metadata block.  That is a separator style ordinary Markdown uses (a
/// rule immediately above a heading, a reveal.js / Marp slide break), and
/// the consequences are not cosmetic — the swallowed section renders as
/// dim key/value data instead of prose, inline-Markdown insertion is
/// refused inside it, and pulldown-cmark's HTML writer emits *nothing*
/// for a metadata block, so an export silently drops content the user
/// wrote.  Frontmatter is defined to be the first thing in the file, so
/// gating on the first line costs nothing real and confines the
/// extension to the one place it belongs.
///
/// Only the flavor that matches is enabled: a file opening `+++` must not
/// have a later `---` pair claimed as YAML frontmatter, and vice versa.
/// A leading blank line means no frontmatter at all — Hugo, Jekyll and
/// Obsidian all require the delimiter at byte 0.
///
/// Every parse of a given document — AST, offset scan, HTML export —
/// must pass that document's own text here; two parse sites disagreeing
/// on the option set break the 1:1 blocks↔ranges pairing.
pub(crate) fn options_for(source: &str) -> Options {
    BASE_OPTIONS.union(metadata_options_for(source))
}

/// Just the metadata-block half of [`options_for`]: the extension
/// matching `source`'s own first line, or [`Options::empty`].
///
/// Split out so [`crate::export::html::render_html`] — which keeps its
/// own base option list on purpose — can adopt the same anchoring rule
/// without duplicating it.  A parse and an export that disagree on
/// whether a `---` opens frontmatter disagree on whether the block
/// survives the export at all.
pub(crate) fn metadata_options_for(source: &str) -> Options {
    let first_line = source
        .split('\n')
        .next()
        .unwrap_or("")
        .trim_end_matches('\r');
    match first_line {
        "---" => Options::ENABLE_YAML_STYLE_METADATA_BLOCKS,
        "+++" => Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS,
        _ => Options::empty(),
    }
}

/// Incremental depth-zero block-range scanner.  Feed it every
/// `(event, byte_range)` pair from an `into_offset_iter()` parse in
/// order via [`observe`](Self::observe); it records the byte range of
/// each depth-zero block whose [`BlockKind`] satisfies `keep`.
///
/// Exists as a struct (rather than only the [`block_ranges_by`] loop)
/// so [`crate::markdown::parser::parse_raw_with_ranges`] can collect
/// ranges as a side effect of the single AST-building parse instead of
/// running a second full pulldown-cmark pass.
pub struct RangeTracker<F> {
    keep: F,
    depth: usize,
    block_start: usize,
    // The kind we opened at depth==0 — recorded only when `keep`
    // accepted it, so depth tracking still increments through
    // nested-block descents but we don't emit a range on close.
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

/// Walk `source`'s pulldown-cmark events at depth zero, recording the
/// byte range of every block whose [`BlockKind`] satisfies `keep`.
///
/// Used by the diff subsystem's table-extent scan (filters to `Table`
/// only) and by [`top_level_block_ranges`] (covers all block kinds).
/// Centralizing the depth-tracking + trailing-newline logic in
/// [`RangeTracker`] keeps every block scanner honest about the same
/// edge cases.
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

/// Extract the byte range of each top-level block in `source`.
///
/// Returns one `Range<usize>` per top-level block, in document order. The
/// ranges cover the complete raw bytes of each block, including delimiters
/// (e.g. the `# ` prefix for headings, triple-backtick fences for code
/// blocks).
///
/// Nested blocks (e.g. paragraphs inside blockquotes) are NOT listed
/// separately — only the outermost container's range is recorded.
///
/// The editor pipeline gets its ranges from
/// [`crate::markdown::parser::parse_raw_with_ranges`] (same tracker, one
/// parse); this stays as the standalone entry point for module tests and
/// the pipeline benchmarks.
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

/// Byte range of every `[^label]: …` footnote definition in `source`,
/// paired with its raw label, in document order.
///
/// Unlike a single-line scan, the range covers the definition's *full*
/// extent — the leader line plus any indented continuation lines and
/// nested blocks pulldown-cmark folds into the definition. Callers
/// deleting a footnote use this so the whole definition is removed, not
/// just its first physical line (which would orphan the continuation as
/// an indented code block).
///
/// A malformed source with two `[^label]:` leaders for the same label
/// yields one entry per definition; pulldown-cmark renders only the
/// first, but callers that delete should remove every leader, so all are
/// returned.
pub fn footnote_definition_ranges(source: &str) -> Vec<(String, Range<usize>)> {
    let options = options_for(source);

    let mut ranges: Vec<(String, Range<usize>)> = Vec::new();
    let mut depth: usize = 0;
    // (label, start byte) for the depth-0 definition currently open, if
    // the depth-0 block we entered was a footnote definition. Depth-0
    // blocks never overlap, so a single slot suffices.
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
    fn footnote_definition_is_its_own_block() {
        // A footnote definition between two paragraphs must get its own
        // byte range so the block↔range pairing in `ParsedDoc` stays 1:1.
        // pulldown-cmark emits the definition at its source position.
        let src = "Intro.[^1]\n\n[^1]: The note.\n\nAfter.\n";
        let ranges = top_level_block_ranges(src);
        assert_eq!(ranges.len(), 3, "expected 3 blocks, got: {ranges:?}");
        assert!(src[ranges[0].clone()].contains("Intro."));
        assert!(src[ranges[1].clone()].contains("[^1]: The note."));
        assert!(src[ranges[2].clone()].contains("After."));
    }

    #[test]
    fn footnote_definition_range_covers_multiline_body() {
        // The range must span the leader line plus the indented
        // continuation, so a delete removes the whole definition.
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

    /// A metadata block is a depth-zero block like any other: its range
    /// must cover the whole frontmatter — both delimiter lines included —
    /// and its trailing newline, or the blocks↔ranges pairing `ParsedDoc`
    /// relies on drifts by a line.
    #[test]
    fn metadata_block_range_covers_both_delimiter_lines() {
        let src = "---\ntitle: Foo\n---\n\n# H\n";
        let ranges = top_level_block_ranges(src);
        assert_eq!(&src[ranges[0].clone()], "---\ntitle: Foo\n---\n");
        assert_eq!(&src[ranges[1].clone()], "# H\n");
    }

    /// The metadata-block extensions are not anchored to the start of the
    /// document on their own: with them on unconditionally, the `---`
    /// above `## Section 2` opens a block that the next `---` closes, and
    /// the whole section between them stops being prose.  `options_for`
    /// is what confines them to a file that actually opens with a
    /// delimiter line.
    #[test]
    fn a_mid_document_rule_pair_is_not_frontmatter() {
        let src = "Intro.\n\n---\n## Section 2\n\nText.\n\n---\n## Section 3\n";
        let ranges = top_level_block_ranges(src);
        assert_eq!(&src[ranges[1].clone()], "---\n", "got: {ranges:?}");
        assert_eq!(&src[ranges[2].clone()], "## Section 2\n\n");
    }

    /// Only the flavor the first line names is enabled — a TOML-opening
    /// file must not have a later `---` pair claimed as YAML frontmatter.
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
        // CRLF still counts; a leading blank line, indentation, a longer
        // delimiter run and a trailing info string do not.
        assert_eq!(
            metadata_options_for("---\r\ntitle: Foo\r\n---\r\n"),
            Options::ENABLE_YAML_STYLE_METADATA_BLOCKS,
        );
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
        // No closing delimiter, so the `---` stays a thematic break and
        // the line below it stays a paragraph.
        let src = "---\ntitle: Foo\n\n# H\n";
        let ranges = top_level_block_ranges(src);
        assert_eq!(ranges.len(), 3, "got: {ranges:?}");
        assert_eq!(&src[ranges[0].clone()], "---\n");
    }
}
