use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, RefDefs, Tag, TagEnd};

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

/// The shared pulldown-cmark option set, without the two metadata-block extensions: those are
/// [`DocParser`]'s to apply, to the frontmatter alone.
///
/// The AST parse and the offset scans here MUST parse alike: block boundaries shift between
/// option sets, and `ParsedDoc` relies on a 1:1 blocks↔ranges pairing.  Every parse of a
/// document goes through [`DocParser`], which is what guarantees it.
pub(crate) const BASE_OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_FOOTNOTES)
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_SMART_PUNCTUATION)
    .union(Options::ENABLE_MATH);

/// A document's pulldown-cmark parse, with frontmatter recognized at byte 0 and nowhere else.
///
/// pulldown-cmark's metadata-block extensions are **not** anchored to the document start: with
/// one on, a `---` opening *any* block (top-level, or the first line inside a quote or list
/// item) starts a metadata block whenever a closing `---` follows somewhere below.  That is
/// ordinary Markdown (a rule above a heading, a slide break, a rule in a quote), and the damage
/// is not cosmetic: the section renders as dim key/value data, inline insertion is refused
/// inside it, and the HTML writer emits *nothing* for a metadata block, so an export silently
/// drops content.  Gating the extension on the document's first line kept it off in documents
/// without frontmatter, but a document *with* frontmatter had it on everywhere.
///
/// So the extension never sees anything but the frontmatter: [`frontmatter_end`] finds where
/// the byte-0 block ends, a parse with its flavor's extension on covers exactly that slice (so
/// its events are pulldown-cmark's own, CRLF and all), and the rest of the document is parsed
/// with both extensions off, its ranges shifted back to document offsets.  The body is still
/// one pass; the frontmatter's is a few lines.
///
/// Every parse of a document (AST, offset scans, HTML export) goes through here, or the 1:1
/// blocks↔ranges pairing breaks.
pub struct DocParser<'a> {
    /// The frontmatter, parsed on its own with its flavor's extension on.
    head: Option<Parser<'a>>,
    /// Everything after the frontmatter, with both extensions off.
    body: Parser<'a>,
    /// Where `body`'s text starts in the document: its ranges are relative to this.
    body_start: usize,
}

impl<'a> DocParser<'a> {
    /// The editor's parse: [`BASE_OPTIONS`].
    pub fn new(source: &'a str) -> Self {
        Self::with_options(source, BASE_OPTIONS)
    }

    /// A parse with another base option set (the HTML export keeps its own).  `options` must
    /// not enable a metadata-block extension; the frontmatter's is added here.
    pub(crate) fn with_options(source: &'a str, options: Options) -> Self {
        match frontmatter_end(source) {
            Some((end, flavor)) => Self {
                head: Some(Parser::new_ext(&source[..end], options | flavor)),
                body: Parser::new_ext(&source[end..], options),
                body_start: end,
            },
            None => Self {
                head: None,
                body: Parser::new_ext(source, options),
                body_start: 0,
            },
        }
    }

    /// The document's link reference definitions.  Frontmatter holds none, so the body's are
    /// all of them.
    pub fn reference_definitions(&self) -> &RefDefs<'_> {
        self.body.reference_definitions()
    }

    /// Every event with its byte range in the document.
    pub fn into_offset_iter(self) -> impl Iterator<Item = (Event<'a>, Range<usize>)> {
        let shift = self.body_start;
        self.head
            .into_iter()
            .flat_map(Parser::into_offset_iter)
            .chain(
                self.body
                    .into_offset_iter()
                    .map(move |(event, range)| (event, range.start + shift..range.end + shift)),
            )
    }

    /// Every event, without ranges.
    pub(crate) fn into_events(self) -> impl Iterator<Item = Event<'a>> {
        self.head.into_iter().flatten().chain(self.body)
    }
}

/// The end of `source`'s frontmatter (past its closing line's newline) and the metadata-block
/// extension its delimiter calls for, or `None` when it has none.
///
/// Frontmatter opens on the first line, which is exactly `---` (YAML) or `+++` (TOML): a
/// leading blank line or indent, a longer run, or anything after the delimiter means no
/// frontmatter, as Hugo / Jekyll / Obsidian read it.  From there the rules are pulldown-cmark's
/// (`scan_metadata_block`), so the slice handed to it parses as one metadata block: the line
/// below the opener is neither blank nor a closer, and the block ends at the first line that is
/// exactly the closer (`---` or `...` for YAML, `+++` for TOML) plus trailing spaces.  Without a
/// closer there is no frontmatter, and the opener is a thematic break.
fn frontmatter_end(source: &str) -> Option<(usize, Options)> {
    let (opener, rest) = source.split_once('\n')?;
    let (flavor, closers): (Options, &[&str]) = match opener {
        "---" => (Options::ENABLE_YAML_STYLE_METADATA_BLOCKS, &["---", "..."]),
        "+++" => (Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS, &["+++"]),
        _ => return None,
    };
    let mut end = opener.len() + 1;
    for (i, line) in rest.split_inclusive('\n').enumerate() {
        let text = line.strip_suffix('\n').unwrap_or(line);
        let text = text.strip_suffix('\r').unwrap_or(text);
        let closes = closers.iter().any(|closer| {
            text.strip_prefix(closer)
                .is_some_and(|tail| tail.bytes().all(|b| b == b' '))
        });
        if i == 0 && (closes || text.bytes().all(|b| b == b' ' || b == b'\t')) {
            return None;
        }
        end += line.len();
        if closes {
            return Some((end, flavor));
        }
    }
    None
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
    for (event, byte_range) in DocParser::new(source).into_offset_iter() {
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
    let mut ranges: Vec<(String, Range<usize>)> = Vec::new();
    let mut depth: usize = 0;
    // The open depth-0 definition, if any.  Depth-0 blocks never overlap, so one slot suffices.
    let mut open: Option<(String, usize)> = None;

    for (event, byte_range) in DocParser::new(source).into_offset_iter() {
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

    /// Only the flavor the first line names is applied, and only to the frontmatter's lines.
    #[test]
    fn frontmatter_ends_past_its_closing_line() {
        let yaml = Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
        let toml = Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS;
        for (src, end, flavor) in [
            ("---\ntitle: Foo\n---\n\nBody.\n", 19, yaml),
            ("---\ntitle: Foo\n...\n", 19, yaml),
            ("---\ntitle: Foo\n---  \n# H\n", 21, yaml),
            ("---\ntitle: Foo\n---", 18, yaml),
            ("---\na: 1\n\nb: 2\n---\n", 19, yaml),
            ("+++\ntitle = \"Foo\"\n+++\n---\n", 22, toml),
        ] {
            assert_eq!(frontmatter_end(src), Some((end, flavor)), "got: {src:?}");
        }
        // None of these open a metadata block.  CRLF is untested because text is
        // `\n`-normalized before reaching the parse.
        for src in [
            "\n---\na: 1\n---\n",
            " ---\na: 1\n---\n",
            "----\na: 1\n----\n",
            "--- yaml\na: 1\n---\n",
            "---\n\na: 1\n---\n",
            "---\n---\n",
            "---\na: 1\n----\n",
            "+++\na = 1\n---\n",
            "---\na: 1\n",
            "---",
            "",
        ] {
            assert_eq!(frontmatter_end(src), None, "got: {src:?}");
        }
    }

    /// [`frontmatter_end`] restates pulldown-cmark's rules; with the extension on over the whole
    /// source, pulldown-cmark must open a metadata block at byte 0 exactly when it finds one, and
    /// end it on the same line.
    #[test]
    fn frontmatter_end_agrees_with_pulldown_cmark() {
        for src in [
            "---\ntitle: Foo\n---\n\nBody.\n",
            "---\ntitle: Foo\n...\n",
            "---\ntitle: Foo\n---  \n# H\n",
            "---\ntitle: Foo\n---",
            "---\na: 1\n\nb: 2\n---\n",
            "---\n\na: 1\n---\n",
            "---\n---\n",
            "---\na: 1\n----\n",
            "---\na: 1\n ---\n---\n",
            "---\na: 1\n\t---\n.... \n...\n",
            "+++\na = 1\n+++\n",
            "+++\na = 1\n++++\n",
            "---\na: 1\n",
        ] {
            let options = BASE_OPTIONS
                | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
                | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS;
            let pulldown =
                Parser::new_ext(src, options)
                    .into_offset_iter()
                    .find_map(|(event, range)| match event {
                        Event::Start(Tag::MetadataBlock(_)) if range.start == 0 => {
                            Some(advance_past_newline(src, range.end))
                        }
                        _ => None,
                    });
            let ours = frontmatter_end(src).map(|(end, _)| end);
            assert_eq!(ours, pulldown, "got: {src:?}");
        }
    }

    /// The extension once stayed on for the whole of a document opening with frontmatter, so a
    /// `---` opening a block below it (here a quote's first line, or a rule above a heading)
    /// started a second metadata block that ran to the next `---`.
    #[test]
    fn only_the_byte_zero_block_is_frontmatter() {
        let src = "---\nt: x\n---\n\n> ---\n> b\n\n---\n## Section\n\n---\n";
        let mut events = DocParser::new(src).into_offset_iter();
        let metadata: Vec<Range<usize>> = events
            .by_ref()
            .filter_map(|(event, range)| {
                matches!(event, Event::Start(Tag::MetadataBlock(_))).then_some(range)
            })
            .collect();
        assert_eq!(metadata.len(), 1, "got: {metadata:?}");
        assert_eq!(metadata[0], 0..12);
        let ranges = top_level_block_ranges(src);
        let texts: Vec<&str> = ranges.iter().map(|r| &src[r.clone()]).collect();
        assert_eq!(
            texts,
            [
                "---\nt: x\n---\n",
                "> ---\n> b\n\n",
                "---\n",
                "## Section\n\n",
                "---\n"
            ],
        );
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
