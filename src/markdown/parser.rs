pub mod post_pass;
mod stream;

pub use post_pass::{
    attach_trailing_tui_columns_comments, promote_diagram_code_blocks,
    promote_display_math_paragraphs, promote_html_comments, promote_image_paragraphs,
    reconstruct_broken_display_math, split_display_math_paragraphs,
};

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, MetadataBlockKind, Parser, Tag, TagEnd};

use super::ast::{inlines_to_plain, Block, Inline, ListItem, MetadataKind, SrcLines};
use super::parse_offsets;
use stream::{EventStream, LeafMode};

#[cfg(test)]
thread_local! {
    /// Block parses run on this thread: incremented where [`parse_document`] builds its
    /// `Parser`, so a test can assert one `ParsedDoc::build` is still exactly one pulldown-cmark
    /// pass.  The per-line `InlineColMap` parses and [`parse_raw`] are not the block parse and
    /// don't count.
    pub(crate) static BLOCK_PARSE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Parse a Markdown string into a list of `Block` AST nodes.
pub fn parse(text: &str) -> Vec<Block> {
    let mut blocks = parse_raw(text);
    // The comment promotion must run first so the merge can find comments by their new
    // variant; keeping the two passes separate is what lets an isolated
    // `<!-- tui-columns -->` outside any table survive as a hidden comment.
    promote_html_comments(&mut blocks);
    attach_trailing_tui_columns_comments(&mut blocks);
    promote_image_paragraphs(&mut blocks, None);
    blocks
}

/// [`parse`] without the trailing-`<!-- tui-columns -->` merge, so callers walking blocks
/// 1:1 against `parse_offsets::top_level_block_ranges` can apply
/// [`attach_trailing_tui_columns_comments`] after their own range-aware mutations.  The
/// editor pipeline uses [`parse_raw_with_ranges`]; this is the ranges-free entry point, which
/// skips draining the events the AST builder leaves unread.
pub fn parse_raw(text: &str) -> Vec<Block> {
    let parser = Parser::new_ext(text, parse_offsets::options_for(text));
    let mut events = EventStream::new(text, parser.into_offset_iter());
    parse_blocks(&mut events, true)
}

/// [`parse_raw`] plus each block's top-level byte range, in a **single** pulldown-cmark
/// pass: the [`EventStream`]'s [`parse_offsets::RangeTracker`] observes the same events the AST
/// builder consumes, so blocks and ranges are 1:1 by construction.
///
/// The editor pipeline's parse entry point.  Folding the second pass in saved ~18% of the
/// pipeline — see docs/dev/performance.md.  The same pass records every leaf's
/// [`SrcLines`] and every container's span.
pub fn parse_raw_with_ranges(text: &str) -> (Vec<Block>, Vec<Range<usize>>) {
    let parse = parse_document(text);
    (parse.blocks, parse.ranges)
}

/// What the editor pipeline's single pulldown-cmark pass yields: [`parse_raw_with_ranges`]'s
/// blocks and ranges, plus two by-products the pass has anyway.
pub struct DocParse {
    pub blocks: Vec<Block>,
    pub ranges: Vec<Range<usize>>,
    /// Byte offset of every source line's start (`[0] == 0`): the index the stream built to
    /// record positions.
    pub line_starts: Vec<usize>,
    /// The labels of the document's link reference definitions, as pulldown-cmark keys them.
    pub link_labels: Vec<String>,
}

/// [`parse_raw_with_ranges`] with the rest of [`DocParse`].  `ParsedDoc::build`'s entry point.
pub fn parse_document(text: &str) -> DocParse {
    #[cfg(test)]
    BLOCK_PARSE_COUNT.with(|c| c.set(c.get() + 1));
    let parser = Parser::new_ext(text, parse_offsets::options_for(text));
    // Definitions are collected by the parser's first pass, before any event is pulled.
    let link_labels = parser
        .reference_definitions()
        .iter()
        .map(|(label, _)| label.to_owned())
        .collect();
    let mut events = EventStream::new(text, parser.into_offset_iter());
    let blocks = parse_blocks(&mut events, true);
    let (ranges, line_starts) = events.into_parts();
    DocParse {
        blocks,
        ranges,
        line_starts,
        link_labels,
    }
}

// ─── Block parsing ────────────────────────────────────────────────────────────
//
// Peek-first throughout: inspect `events.peek()`, then `events.next()` to consume.  That
// is what lets tight list items work, where pulldown-cmark emits `Text` directly inside an
// `Item` with no surrounding `Paragraph`.

/// `top_level` anchors each block's relative line numbers at its own first line; nested calls
/// (container children) inherit their top-level block's anchor.
fn parse_blocks<'a, I>(events: &mut EventStream<'a, I>, top_level: bool) -> Vec<Block>
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    let mut blocks = Vec::new();

    loop {
        if top_level {
            events.set_base();
        }
        match events.peek() {
            None | Some(Event::End(_)) => break,

            Some(Event::Start(Tag::Paragraph)) => {
                if let Some(b) = parse_paragraph_block(events) {
                    blocks.push(b);
                }
            }
            Some(Event::Start(Tag::Heading { .. })) => match parse_heading_block(events) {
                Some(b) => blocks.push(b),
                None => break,
            },
            Some(Event::Start(Tag::BlockQuote(_))) => {
                blocks.push(parse_blockquote_block(events));
            }
            Some(Event::Start(Tag::CodeBlock(_))) => {
                blocks.push(parse_code_block(events));
            }
            Some(Event::Start(Tag::List(_))) => {
                blocks.push(parse_list_block(events));
            }
            Some(Event::Start(Tag::Table(_))) => {
                blocks.push(parse_table_block(events));
            }
            Some(Event::Rule) => {
                events.begin_leaf(None, LeafMode::Verbatim);
                events.next();
                blocks.push(Block::HorizontalRule {
                    src: events.end_leaf(),
                });
            }
            Some(Event::Start(Tag::HtmlBlock)) => {
                blocks.push(parse_html_block(events));
            }
            Some(Event::Html(_)) => {
                events.begin_leaf(None, LeafMode::Verbatim);
                if let Some(Event::Html(html)) = events.next() {
                    blocks.push(Block::Html(html.into_string(), events.end_leaf()));
                } else {
                    events.end_leaf();
                }
            }
            Some(Event::Start(Tag::FootnoteDefinition(_))) => {
                blocks.push(parse_footnote_definition_block(events));
            }
            Some(Event::Start(Tag::MetadataBlock(_))) => {
                blocks.push(parse_metadata_block(events));
            }

            // Tight lists emit inline content directly inside Item, no Paragraph wrapper.
            Some(Event::Text(_))
            | Some(Event::Code(_))
            | Some(Event::InlineMath(_))
            | Some(Event::DisplayMath(_))
            | Some(Event::InlineHtml(_))
            | Some(Event::FootnoteReference(_))
            | Some(Event::SoftBreak)
            | Some(Event::HardBreak)
            | Some(Event::Start(Tag::Emphasis))
            | Some(Event::Start(Tag::Strong))
            | Some(Event::Start(Tag::Strikethrough))
            | Some(Event::Start(Tag::Link { .. }))
            | Some(Event::Start(Tag::Image { .. })) => {
                events.begin_leaf(None, LeafMode::Prose);
                let inlines = parse_inlines(events);
                let src = events.end_leaf();
                if !inlines.is_empty() {
                    blocks.push(Block::Paragraph { inlines, src });
                }
            }

            _ => {
                events.next();
            }
        }
    }

    blocks
}

/// Consume `Start(Paragraph) … End(Paragraph)`.  An empty paragraph collapses to `None`
/// so `parse_blocks` doesn't push a noise entry.
fn parse_paragraph_block<'a, I>(events: &mut EventStream<'a, I>) -> Option<Block>
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    events.next();
    events.begin_leaf(Some(events.last_range()), LeafMode::Prose);
    let inlines = parse_inlines(events);
    let src = events.end_leaf();
    consume_end(events);
    if inlines.is_empty() {
        None
    } else {
        Some(Block::Paragraph { inlines, src })
    }
}

/// Consume `Start(Heading { .. }) … End(Heading)`.  `None` only if the peeked event was
/// not in fact a heading — defensive against a malformed event stream.
fn parse_heading_block<'a, I>(events: &mut EventStream<'a, I>) -> Option<Block>
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    let level = match events.next()? {
        Event::Start(Tag::Heading { level, .. }) => level,
        _ => return None,
    };
    events.begin_leaf(Some(events.last_range()), LeafMode::Prose);
    let inlines = parse_inlines(events);
    let src = events.end_leaf();
    consume_end(events);
    Some(Block::Heading {
        level,
        inlines,
        src,
    })
}

fn parse_blockquote_block<'a, I>(events: &mut EventStream<'a, I>) -> Block
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    events.next();
    let start = events.open_container();
    let inner = parse_blocks(events, false);
    consume_end(events);
    // A bare `>` before or after the children is the quote's own line, so its range counts.
    let span = events.container_span(&start, children_end(&inner), true);
    // Uncovered lines that aren't bare `>`: link reference definitions.  The empty range at
    // the span's end closes the gap after the last child.
    let mut hidden = Vec::new();
    let mut next_line = span.start;
    for child in inner
        .iter()
        .map(Block::span)
        .chain(std::iter::once(span.end..span.end))
    {
        hidden.extend((next_line..child.start).filter(|&line| !events.is_bare_line(line)));
        next_line = next_line.max(child.end);
    }
    Block::BlockQuote {
        blocks: inner,
        span,
        hidden,
    }
}

/// Consume a footnote definition, parsing its body as a nested block sequence.
fn parse_footnote_definition_block<'a, I>(events: &mut EventStream<'a, I>) -> Block
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    let label = match events.next() {
        Some(Event::Start(Tag::FootnoteDefinition(l))) => l.into_string(),
        _ => String::new(),
    };
    let start = events.open_container();
    let inner = parse_blocks(events, false);
    consume_end(events);
    let span = events.container_span(&start, children_end(&inner), false);
    Block::FootnoteDefinition {
        label,
        blocks: inner,
        span,
    }
}

/// Consume a metadata block.  The body arrives as plain `Event::Text` (no inline parsing
/// inside), so `content` is the frontmatter verbatim, minus the two delimiter lines that
/// the events' *ranges* — but not their payloads — cover.
fn parse_metadata_block<'a, I>(events: &mut EventStream<'a, I>) -> Block
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    let kind = match events.next() {
        Some(Event::Start(Tag::MetadataBlock(MetadataBlockKind::PlusesStyle))) => {
            MetadataKind::Toml
        }
        // YAML is the flavor `---` opens.
        _ => MetadataKind::Yaml,
    };
    events.begin_leaf(Some(events.last_range()), LeafMode::Verbatim);
    let (content, src) = collect_text_until_end(events);
    Block::MetadataBlock { kind, content, src }
}

fn parse_code_block<'a, I>(events: &mut EventStream<'a, I>) -> Block
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    let (language, fenced) = match events.next() {
        Some(Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang)))) => {
            let s = lang.as_ref().trim().to_owned();
            (if s.is_empty() { None } else { Some(s) }, true)
        }
        Some(Event::Start(Tag::CodeBlock(CodeBlockKind::Indented))) => (None, false),
        _ => (None, false),
    };
    events.begin_leaf(Some(events.last_range()), LeafMode::Verbatim);
    let (content, src) = collect_text_until_end(events);
    Block::CodeBlock {
        language,
        content,
        fenced,
        src,
    }
}

fn parse_list_block<'a, I>(events: &mut EventStream<'a, I>) -> Block
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    let start = match events.next() {
        Some(Event::Start(Tag::List(s))) => s,
        _ => None,
    };
    let start_range = events.open_container();
    let items = parse_list_items(events);
    consume_end(events);
    let items_end = items.iter().map(|item| item.span.end).max();
    let span = events.container_span(&start_range, items_end, false);
    Block::List {
        ordered: start.is_some(),
        start,
        items,
        span,
    }
}

fn parse_table_block<'a, I>(events: &mut EventStream<'a, I>) -> Block
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    events.next();
    events.begin_leaf(Some(events.last_range()), LeafMode::Table);
    let (headers, rows, col_count) = parse_table(events);
    let src = events.end_leaf();
    consume_end(events);
    Block::Table {
        col_count,
        headers,
        rows,
        user_widths: None,
        src,
    }
}

/// Consume the `Start(HtmlBlock)` / `End(HtmlBlock)` wrapper pulldown-cmark 0.11+ puts
/// around `Html(...)` events, so the outer loop's `End(_) => break` doesn't swallow every
/// block after an HTML block.
fn parse_html_block<'a, I>(events: &mut EventStream<'a, I>) -> Block
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    events.next();
    events.begin_leaf(Some(events.last_range()), LeafMode::Verbatim);
    let mut body = String::new();
    loop {
        match events.peek() {
            None => break,
            Some(Event::End(TagEnd::HtmlBlock)) => {
                events.next();
                break;
            }
            _ => match events.next() {
                Some(Event::Html(h)) => body.push_str(&h),
                Some(Event::Text(t)) => body.push_str(&t),
                Some(_) | None => {}
            },
        }
    }
    Block::Html(body, events.end_leaf())
}

/// The last line any of `blocks` covers, end exclusive; `None` for no blocks.
fn children_end(blocks: &[Block]) -> Option<u32> {
    blocks.iter().map(|b| b.span().end).max()
}

// ─── List parsing ─────────────────────────────────────────────────────────────

fn parse_list_items<'a, I>(events: &mut EventStream<'a, I>) -> Vec<ListItem>
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    let mut items = Vec::new();

    loop {
        match events.peek() {
            None | Some(Event::End(_)) => break,
            Some(Event::Start(Tag::Item)) => {
                events.next(); // consume Start(Item)
                let item_range = events.open_container();

                // A loose list wraps the marker in a Paragraph, a tight one doesn't.  In
                // the loose case the `Start(Paragraph)` is consumed speculatively, since
                // `parse_inlines` can't handle a dangling `End(Paragraph)`.
                let mut task: Option<bool> = None;
                if let Some(Event::TaskListMarker(_)) = events.peek() {
                    if let Some(Event::TaskListMarker(checked)) = events.next() {
                        task = Some(checked);
                    }
                }
                let mut paragraph_range: Option<Range<usize>> = None;
                if task.is_none() && matches!(events.peek(), Some(Event::Start(Tag::Paragraph))) {
                    events.next(); // consume Start(Paragraph)
                    paragraph_range = Some(events.last_range());
                    if let Some(Event::TaskListMarker(_)) = events.peek() {
                        if let Some(Event::TaskListMarker(checked)) = events.next() {
                            task = Some(checked);
                        }
                    }
                }

                let mut blocks: Vec<Block> = Vec::new();
                if let Some(range) = paragraph_range {
                    // Close the paragraph we opened, then let `parse_blocks` take the
                    // rest of the item (nested lists, further paragraphs).  The leaf opens
                    // past the task marker, which is the item's chrome, not its content.
                    events.begin_leaf(Some(range), LeafMode::Prose);
                    let inlines = parse_inlines(events);
                    let src = events.end_leaf();
                    consume_end(events); // End(Paragraph)
                    if !inlines.is_empty() {
                        blocks.push(Block::Paragraph { inlines, src });
                    }
                    blocks.extend(parse_blocks(events, false));
                } else {
                    blocks = parse_blocks(events, false);
                }
                consume_end(events); // End(Item)
                let span = events.container_span(&item_range, children_end(&blocks), false);
                items.push(ListItem { blocks, task, span });
            }
            _ => {
                events.next(); // skip unexpected events
            }
        }
    }

    items
}

// ─── Table parsing ────────────────────────────────────────────────────────────

/// Output of [`parse_table`]: `(headers, rows, col_count)` where each
/// header / row cell is a `Vec<Inline>`.
type ParsedTable = (Vec<Vec<Inline>>, Vec<Vec<Vec<Inline>>>, usize);

fn parse_table<'a, I>(events: &mut EventStream<'a, I>) -> ParsedTable
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    let mut headers: Vec<Vec<Inline>> = Vec::new();
    let mut rows: Vec<Vec<Vec<Inline>>> = Vec::new();
    let mut col_count = 0;

    loop {
        match events.peek() {
            None | Some(Event::End(TagEnd::Table)) => break,
            Some(Event::Start(Tag::TableHead)) => {
                events.next();
                headers = parse_table_row(events);
                col_count = headers.len();
                consume_end(events); // End(TableHead)
            }
            Some(Event::Start(Tag::TableRow)) => {
                events.next();
                let row = parse_table_row(events);
                consume_end(events); // End(TableRow)
                rows.push(row);
            }
            _ => {
                events.next();
            }
        }
    }

    (headers, rows, col_count)
}

fn parse_table_row<'a, I>(events: &mut EventStream<'a, I>) -> Vec<Vec<Inline>>
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    let mut cells = Vec::new();

    loop {
        match events.peek() {
            None | Some(Event::End(TagEnd::TableHead)) | Some(Event::End(TagEnd::TableRow)) => {
                break
            }
            Some(Event::Start(Tag::TableCell)) => {
                events.next();
                let inlines = parse_inlines(events);
                consume_end(events); // End(TableCell)
                cells.push(inlines);
            }
            _ => {
                events.next();
            }
        }
    }

    cells
}
// ─── Highlight post-processing ────────────────────────────────────────────────

/// Split a text string into `Inline`s, detecting `==highlight==` spans — pulldown-cmark
/// has no native support for them, so each `Text` event is post-processed here.
fn parse_highlight_in_text(text: &str) -> Vec<Inline> {
    let mut result = Vec::new();
    let mut rest = text;

    loop {
        match rest.find("==") {
            None => break,
            Some(start) => {
                let after_open = &rest[start + 2..];
                match after_open.find("==") {
                    None => break, // unclosed marker — treat the rest as plain text
                    Some(rel_end) => {
                        if start > 0 {
                            result.push(Inline::Text(rest[..start].to_owned()));
                        }
                        let inner = &after_open[..rel_end];
                        result.push(Inline::Highlight(vec![Inline::Text(inner.to_owned())]));
                        rest = &after_open[rel_end + 2..];
                    }
                }
            }
        }
    }

    if !rest.is_empty() {
        result.push(Inline::Text(rest.to_owned()));
    }
    result
}

// ─── Inline parsing ───────────────────────────────────────────────────────────

fn parse_inlines<'a, I>(events: &mut EventStream<'a, I>) -> Vec<Inline>
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    let mut inlines = Vec::new();

    loop {
        match events.peek() {
            None | Some(Event::End(_)) | Some(Event::Rule) => break,
            // Stop at every block start, not an allowlist of them: a tight item's text has no
            // `Paragraph` wrapper, so a block that follows it (an HTML block, a table) arrives
            // here, and folding it in mismatches every `End` after it.
            Some(Event::Start(tag)) if !is_inline_tag(tag) => break,
            _ => {}
        }

        let event = match events.next() {
            Some(e) => e,
            None => break,
        };

        match event {
            Event::Text(text) => inlines.extend(parse_highlight_in_text(&text)),
            Event::Code(code) => inlines.push(Inline::Code(code.into_string())),
            Event::SoftBreak => inlines.push(Inline::SoftBreak),
            Event::HardBreak => inlines.push(Inline::HardBreak),
            // 0.11+ reports inline HTML as `InlineHtml`; both are matched so a raw-HTML
            // event reported inside a paragraph is handled identically.
            Event::Html(html) | Event::InlineHtml(html) => {
                let s = html.into_string();
                // A balanced comment renders as zero spans; anything else stays text so
                // the source is still visible.
                if post_pass::is_html_comment_only(&s) {
                    inlines.push(Inline::HtmlComment(s));
                } else {
                    inlines.push(Inline::Text(s));
                }
            }

            Event::Start(Tag::Emphasis) => {
                let inner = parse_inlines(events);
                consume_end(events);
                inlines.push(Inline::Italic(inner));
            }
            Event::Start(Tag::Strong) => {
                let inner = parse_inlines(events);
                consume_end(events);
                inlines.push(Inline::Bold(inner));
            }
            Event::Start(Tag::Strikethrough) => {
                let inner = parse_inlines(events);
                consume_end(events);
                inlines.push(Inline::Strikethrough(inner));
            }

            Event::Start(Tag::Link {
                dest_url, title, ..
            }) => {
                let text = parse_inlines(events);
                consume_end(events);
                let title_str = title.as_ref().to_owned();
                inlines.push(Inline::Link {
                    text,
                    url: dest_url.into_string(),
                    title: if title_str.is_empty() {
                        None
                    } else {
                        Some(title_str)
                    },
                });
            }

            Event::Start(Tag::Image { dest_url, .. }) => {
                let alt_inlines = parse_inlines(events);
                consume_end(events);
                let alt = inlines_to_plain(&alt_inlines);
                inlines.push(Inline::Image {
                    alt,
                    url: dest_url.into_string(),
                });
            }

            // Emitted only when a matching definition exists.  The raw label is kept
            // verbatim — no display renumbering.
            Event::FootnoteReference(label) => {
                inlines.push(Inline::FootnoteReference {
                    label: label.into_string(),
                });
            }

            // Math — `$...$` inline and `$$...$$` display, LaTeX source kept verbatim.  Requires
            // `ENABLE_MATH` in the shared parse options (`parse_offsets::BASE_OPTIONS`).
            Event::InlineMath(source) => {
                inlines.push(Inline::Math {
                    source: source.into_string(),
                    display: false,
                });
            }
            Event::DisplayMath(source) => {
                inlines.push(Inline::Math {
                    source: source.into_string(),
                    display: true,
                });
            }
            // A list item's paragraph takes its task box before its text is parsed, so one
            // reaching here opens something else: pulldown-cmark reports it inside a setext
            // heading that opens the item.  GFM has a box only at the start of a paragraph, so
            // it stays the heading's literal text.
            Event::TaskListMarker(_) => {
                inlines.push(Inline::Text(format!("{} ", events.last_text())))
            }

            _ => {}
        }
    }

    inlines
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Whether `tag` opens inline content — the only starts [`parse_inlines`] consumes.
fn is_inline_tag(tag: &Tag) -> bool {
    matches!(
        tag,
        Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. }
    )
}

/// Consume one `Event::End(_)` if it is next in the stream.
fn consume_end<'a, I>(events: &mut EventStream<'a, I>)
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    if matches!(events.peek(), Some(Event::End(_))) {
        events.next();
    }
}

/// Collect `Event::Text` content until the next `Event::End`, consuming that `End`, and close
/// the leaf the caller opened.
fn collect_text_until_end<'a, I>(events: &mut EventStream<'a, I>) -> (String, SrcLines)
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    let mut text = String::new();
    loop {
        match events.peek() {
            None | Some(Event::End(_)) => break,
            _ => {}
        }
        if let Some(Event::Text(t)) = events.next() {
            text.push_str(&t);
        }
    }
    let src = events.end_leaf();
    consume_end(events);
    (text, src)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::ast::{Block, Inline};
    use pulldown_cmark::HeadingLevel;

    fn src_lines(first: u32, content_col: &[Option<u32>]) -> SrcLines {
        SrcLines::new(first, content_col)
    }

    /// The merged parse must produce exactly what the two-pass pairing produced.
    #[test]
    fn merged_parse_matches_two_pass_parse() {
        let src = "# Title\n\nA paragraph with **bold**.\n\n- item one\n- item two\n\n\
                   ```rust\nfn x() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n\
                   > quoted\n\n---\n\nRef.[^1]\n\n[^1]: A note.\n\n<!-- a comment -->\n";
        let (blocks, ranges) = parse_raw_with_ranges(src);
        assert_eq!(blocks, parse_raw(src));
        assert_eq!(ranges, parse_offsets::top_level_block_ranges(src));
        assert_eq!(blocks.len(), ranges.len(), "blocks↔ranges must stay 1:1");
    }

    #[test]
    fn parse_heading() {
        let blocks = parse("# Hello\n");
        assert_eq!(
            blocks,
            vec![Block::Heading {
                level: HeadingLevel::H1,
                inlines: vec![Inline::Text("Hello".into())],
                src: src_lines(0, &[Some(2)]),
            }]
        );
    }

    #[test]
    fn setext_h2_is_heading_not_rule() {
        let blocks = parse("H2 text\n---\n");
        eprintln!("setext H2 blocks: {:?}", blocks);
        assert!(
            matches!(
                &blocks[0],
                Block::Heading {
                    level: HeadingLevel::H2,
                    ..
                }
            ),
            "expected H2 heading, got: {:?}",
            blocks
        );
    }

    #[test]
    fn parse_paragraph() {
        let blocks = parse("Hello world\n");
        assert!(matches!(&blocks[0], Block::Paragraph { inlines, .. } if !inlines.is_empty()));
    }

    /// A `$$...$$` math block standing alone in a paragraph must parse as a
    /// single `Inline::Math { display: true }` so the post-pass can promote
    /// it to a block-level rendered image (see docs/dev design, phase 1).
    /// Requires `Options::ENABLE_MATH` in the shared parse options.
    #[test]
    fn parse_display_math_paragraph() {
        let blocks = parse("$$\nx^2 + y^2 = z^2\n$$\n");
        assert_eq!(
            blocks,
            vec![Block::Paragraph {
                inlines: vec![Inline::Math {
                    source: "\nx^2 + y^2 = z^2\n".into(),
                    display: true,
                }],
                // The formula's later lines continue the one math span begun on line 0.
                src: src_lines(0, &[Some(0), None, None]),
            }],
            "a paragraph holding only $$...$$ should parse as one display-math inline"
        );
    }

    #[test]
    fn parse_bold_and_italic() {
        let blocks = parse("**bold** and *italic*\n");
        if let Block::Paragraph { inlines, .. } = &blocks[0] {
            assert!(inlines.iter().any(|i| matches!(i, Inline::Bold(_))));
            assert!(inlines.iter().any(|i| matches!(i, Inline::Italic(_))));
        } else {
            panic!("Expected paragraph");
        }
    }

    #[test]
    fn parse_code_span() {
        let blocks = parse("`code`\n");
        if let Block::Paragraph { inlines, .. } = &blocks[0] {
            assert!(inlines.iter().any(|i| matches!(i, Inline::Code(_))));
        } else {
            panic!("Expected paragraph");
        }
    }

    #[test]
    fn parse_fenced_code_block() {
        let blocks = parse("```rust\nfn main() {}\n```\n");
        assert!(matches!(
            &blocks[0],
            Block::CodeBlock { language: Some(lang), .. } if lang == "rust"
        ));
    }

    #[test]
    fn parse_horizontal_rule() {
        let blocks = parse("---\n");
        assert!(blocks
            .iter()
            .any(|b| matches!(b, Block::HorizontalRule { .. })));
    }

    #[test]
    fn parse_tight_unordered_list() {
        let blocks = parse("- one\n- two\n");
        match &blocks[0] {
            Block::List { ordered, items, .. } => {
                assert!(!ordered);
                assert_eq!(items.len(), 2);
            }
            other => panic!("Expected List, got: {:?}", other),
        }
    }

    /// A block that follows a tight item's text has no `Paragraph` between them, so
    /// `parse_inlines` must stop at it.  Folding it into the item's inlines mismatched every
    /// `End` after it and dropped the rest of the document (#66).
    #[test]
    fn block_after_tight_item_text_keeps_later_blocks() {
        for src in [
            "- a\n  <div>\n\nafter\n",
            "- a\n  <div>\n  x\n  </div>\n\nafter\n",
            "- a\n  | t |\n  |---|\n\nafter\n",
        ] {
            let (blocks, ranges) = parse_raw_with_ranges(src);
            assert_eq!(blocks.len(), 2, "{src:?}: {blocks:?}");
            assert_eq!(blocks.len(), ranges.len(), "{src:?}: blocks and ranges 1:1");
            assert!(
                matches!(&blocks[1], Block::Paragraph { inlines, .. }
                    if inlines == &[Inline::Text("after".into())]),
                "{src:?}: {blocks:?}"
            );
            let Block::List { items, .. } = &blocks[0] else {
                panic!("{src:?}: expected a list, got {blocks:?}");
            };
            assert_eq!(items.len(), 1);
            assert!(
                matches!(&items[0].blocks[0], Block::Paragraph { inlines, .. }
                    if inlines == &[Inline::Text("a".into())]),
                "{src:?}: the item's text must not absorb the block: {items:?}"
            );
            assert_eq!(items[0].blocks.len(), 2, "{src:?}: {items:?}");
        }
    }

    #[test]
    fn parse_ordered_list() {
        let blocks = parse("1. first\n2. second\n");
        assert!(matches!(&blocks[0], Block::List { ordered: true, items, .. } if items.len() == 2));
    }

    #[test]
    fn parse_blockquote() {
        let blocks = parse("> quoted\n");
        assert!(matches!(&blocks[0], Block::BlockQuote { .. }));
    }

    #[test]
    fn table_picks_up_trailing_tui_columns_comment() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n<!-- tui-columns: [10, 20] -->\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1, "expected one block, got: {blocks:?}");
        match &blocks[0] {
            Block::Table {
                user_widths: Some(w),
                ..
            } => assert_eq!(w, &vec![Some(10), Some(20)]),
            other => panic!("expected Table with user_widths, got: {other:?}"),
        }
    }

    #[test]
    fn content_after_tui_columns_comment_is_preserved() {
        // Regression: pulldown-cmark 0.11+ wraps HTML blocks in
        // Start/End(HtmlBlock).  Without explicit handling in `parse_blocks`,
        // the End(HtmlBlock) event terminates the top-level block loop and
        // every paragraph after an HTML block vanishes.
        let src =
            "| a | b |\n|---|---|\n| 1 | 2 |\n<!-- tui-columns: [10, 20] -->\n\nContent below\n";
        let blocks = parse(src);
        assert_eq!(
            blocks.len(),
            2,
            "expected Table + Paragraph, got: {blocks:?}"
        );
        assert!(matches!(
            &blocks[0],
            Block::Table {
                user_widths: Some(_),
                ..
            }
        ));
        assert!(matches!(&blocks[1], Block::Paragraph { .. }));
    }

    #[test]
    fn table_without_tui_columns_has_none_widths() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let blocks = parse(src);
        match &blocks[0] {
            Block::Table { user_widths, .. } => assert!(user_widths.is_none()),
            other => panic!("expected Table, got: {other:?}"),
        }
    }

    #[test]
    fn parse_list_item_text_present() {
        let blocks = parse("- item one\n- item two\n");
        if let Block::List { items, .. } = &blocks[0] {
            assert!(!items.is_empty(), "list has no items");
            assert!(!items[0].blocks.is_empty(), "first item has no blocks");
            if let Block::Paragraph { inlines, .. } = &items[0].blocks[0] {
                let text = super::super::ast::inlines_to_plain(inlines);
                assert!(text.contains("item one"), "text was: {text:?}");
            } else {
                panic!("First block is not a Paragraph: {:?}", items[0].blocks[0]);
            }
        } else {
            panic!("Expected List");
        }
    }

    #[test]
    fn image_only_paragraph_promotes_to_image_block() {
        let blocks = parse("![cat](cat.png)\n");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::ImageBlock { alt, url, .. } => {
                assert_eq!(alt, "cat");
                assert_eq!(url, "cat.png");
            }
            other => panic!("expected ImageBlock, got {other:?}"),
        }
    }

    #[test]
    fn image_with_surrounding_whitespace_still_promotes() {
        // pulldown-cmark can attach zero-width text inlines around an image.
        let blocks = parse("   ![dog](dog.png)   \n");
        assert!(
            matches!(&blocks[0], Block::ImageBlock { url, .. } if url == "dog.png"),
            "got {:?}",
            blocks[0]
        );
    }

    #[test]
    fn mixed_content_paragraph_keeps_inline_image() {
        let blocks = parse("Prefix ![cat](cat.png) suffix\n");
        match &blocks[0] {
            Block::Paragraph { inlines, .. } => {
                assert!(inlines.iter().any(|i| matches!(i, Inline::Image { .. })));
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn multiple_stacked_image_paragraphs_each_promote() {
        let blocks = parse("![a](a.png)\n\n![b](b.png)\n");
        let image_blocks: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b, Block::ImageBlock { .. }))
            .collect();
        assert_eq!(image_blocks.len(), 2);
    }

    #[test]
    fn paragraph_with_two_images_does_not_promote() {
        // Mixed content: both stay inline `[Image: alt]` placeholders.
        let blocks = parse("![a](a.png) ![b](b.png)\n");
        assert!(matches!(&blocks[0], Block::Paragraph { .. }));
    }

    // ── HTML comment promotion ────────────────────────────────────────────

    #[test]
    fn block_level_html_comment_promotes_to_html_comment() {
        let blocks = parse("<!-- hello -->\n");
        assert_eq!(blocks.len(), 1, "got {blocks:?}");
        assert!(
            matches!(&blocks[0], Block::HtmlComment(body, _) if body.trim() == "<!-- hello -->"),
            "got {:?}",
            blocks[0]
        );
    }

    #[test]
    fn block_level_html_tag_is_not_promoted() {
        let blocks = parse("<div>stuff</div>\n");
        assert!(matches!(&blocks[0], Block::Html(..)), "got {:?}", blocks[0]);
    }

    #[test]
    fn isolated_tui_columns_comment_not_adjacent_to_table_stays_hidden_comment() {
        // With no table to absorb it, the comment must survive as a hidden
        // `Block::HtmlComment` rather than being dropped.
        let blocks = parse("<!-- tui-columns: [10, 20, 30] -->\n\nSome text.\n");
        assert!(
            matches!(&blocks[0], Block::HtmlComment(..)),
            "got {:?}",
            blocks[0]
        );
        assert!(matches!(&blocks[1], Block::Paragraph { .. }));
    }

    #[test]
    fn inline_html_comment_produces_inline_html_comment_variant() {
        let blocks = parse("hello <!-- aside --> world\n");
        match &blocks[0] {
            Block::Paragraph { inlines, .. } => {
                assert!(
                    inlines.iter().any(|i| matches!(i, Inline::HtmlComment(_))),
                    "inlines: {inlines:?}"
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn inline_html_tag_stays_as_text() {
        let blocks = parse("line <br> end\n");
        match &blocks[0] {
            Block::Paragraph { inlines, .. } => {
                assert!(
                    inlines
                        .iter()
                        .any(|i| matches!(i, Inline::Text(t) if t.contains("<br>"))),
                    "inlines: {inlines:?}"
                );
                assert!(!inlines.iter().any(|i| matches!(i, Inline::HtmlComment(_))));
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn tui_columns_still_absorbed_after_html_comment_promotion() {
        // The attach pass must still consume the comment after the generic
        // `promote_html_comments` has converted it.
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n<!-- tui-columns: [10, 20] -->\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0],
            Block::Table {
                user_widths: Some(_),
                ..
            }
        ));
    }

    // ── Loose-list spacing ───────────────────────────────────────

    /// The single `Block::List` in `blocks` — loose lists stay one block, so every test
    /// below asserts against one list plus the blank lines between its items' spans.
    fn only_list(blocks: &[Block]) -> (&[ListItem], bool, Option<u64>) {
        let lists: Vec<&Block> = blocks
            .iter()
            .filter(|b| matches!(b, Block::List { .. }))
            .collect();
        assert_eq!(lists.len(), 1, "expected exactly one list, got {blocks:?}");
        match lists[0] {
            Block::List {
                items,
                ordered,
                start,
                ..
            } => (items, *ordered, *start),
            _ => unreachable!(),
        }
    }

    /// Blank source lines between each item's span and the previous one's — what the renderer
    /// spaces a loose list with.
    fn blanks_before(items: &[ListItem]) -> Vec<usize> {
        let mut prev_end: Option<u32> = None;
        items
            .iter()
            .map(|it| {
                let gap = prev_end.map_or(0, |end| it.span.start.saturating_sub(end));
                prev_end = Some(it.span.end);
                gap as usize
            })
            .collect()
    }

    #[test]
    fn ordered_list_blank_between_items_stays_one_list_with_a_gap() {
        // A blank makes the list loose but keeps it one ordered list; numbering comes
        // straight from pulldown-cmark and the item after the blank counts 1.
        let blocks = parse("1. a\n2. b\n\n3. c\n4. d\n");
        let (items, ordered, start) = only_list(&blocks);
        assert!(ordered);
        assert_eq!(start, Some(1));
        assert_eq!(items.len(), 4);
        assert_eq!(blanks_before(items), vec![0, 0, 1, 0]);
    }

    #[test]
    fn ordered_list_restart_numbering_no_longer_splits() {
        // Source numbers restart at 1 after the blank, but CommonMark makes this one
        // loose list, so it renders 1,2,3,4.
        let blocks = parse("1. a\n2. b\n\n1. c\n2. d\n");
        let (items, ordered, start) = only_list(&blocks);
        assert!(ordered);
        assert_eq!(start, Some(1));
        assert_eq!(items.len(), 4);
        assert_eq!(blanks_before(items), vec![0, 0, 1, 0]);
    }

    #[test]
    fn bullet_list_blank_between_items_stays_one_list_with_a_gap() {
        let blocks = parse("- a\n- b\n\n- c\n- d\n");
        let (items, ordered, _) = only_list(&blocks);
        assert!(!ordered);
        assert_eq!(items.len(), 4);
        assert_eq!(blanks_before(items), vec![0, 0, 1, 0]);
    }

    #[test]
    fn a_blank_in_an_items_fenced_code_block_is_not_a_gap() {
        // A blank inside an embedded fence is not an inter-item separator.
        let src = "- intro\n  ```toml\n  [a]\n\n  [b]\n  ```\n  trailing\n- next item\n";
        let blocks = parse(src);
        let (items, _, _) = only_list(&blocks);
        assert_eq!(items.len(), 2);
        assert_eq!(blanks_before(items), vec![0, 0]);
    }

    #[test]
    fn ordered_list_without_blank_lines_has_no_gaps() {
        let blocks = parse("1. a\n2. b\n3. c\n");
        let (items, ordered, _) = only_list(&blocks);
        assert!(ordered);
        assert_eq!(items.len(), 3);
        assert_eq!(blanks_before(items), vec![0, 0, 0]);
    }

    #[test]
    fn nested_list_with_blank_line_inside_top_level_item_stays_one_list() {
        // Only gaps between items at the same indent level count.
        let blocks = parse("- outer\n  - nested\n- next\n");
        let (items, _, _) = only_list(&blocks);
        assert_eq!(items.len(), 2);
        assert_eq!(blanks_before(items), vec![0, 0]);
    }

    #[test]
    fn every_blank_separated_item_has_its_gap() {
        let blocks = parse("1. a\n\n1. b\n\n1. c\n");
        let (items, ordered, start) = only_list(&blocks);
        assert!(ordered);
        assert_eq!(start, Some(1));
        assert_eq!(items.len(), 3);
        assert_eq!(blanks_before(items), vec![0, 1, 1]);
    }

    #[test]
    fn an_interior_blank_is_not_a_gap() {
        // A blank interior to an item's content is not an inter-item separator.
        let blocks = parse("- a\n\n  cont\n- b\n");
        let (items, _, _) = only_list(&blocks);
        assert_eq!(items.len(), 2);
        assert_eq!(blanks_before(items), vec![0, 0]);
    }

    #[test]
    fn double_blank_between_items_counts_two() {
        let blocks = parse("- a\n\n\n- b\n");
        let (items, _, _) = only_list(&blocks);
        assert_eq!(items.len(), 2);
        assert_eq!(blanks_before(items), vec![0, 2]);
    }

    #[test]
    fn multi_line_item_content_before_separator_blank_counts_one() {
        // Only the blank directly above item 2 is counted.
        let src = "1. **first** item\n   continuation\n\n   ```rust\n   let x = 1;\n   ```\n\n2. second\n";
        let blocks = parse(src);
        let (items, _, _) = only_list(&blocks);
        assert_eq!(items.len(), 2);
        assert_eq!(blanks_before(items), vec![0, 1]);
    }

    // ── Source positions ──────────────────────────────────────────────────

    fn first_src(src: &str) -> SrcLines {
        parse_raw(src)[0].src().cloned().expect("a leaf")
    }

    #[test]
    fn paragraph_lines_start_past_container_prefixes_and_markers() {
        assert_eq!(first_src("a\n  b\n"), src_lines(0, &[Some(0), Some(2)]));
        let Block::BlockQuote { blocks, span, .. } = &parse_raw("> a\n> b\nlazy\n")[0] else {
            panic!("expected a quote");
        };
        assert_eq!(*span, 0..3);
        assert_eq!(
            blocks[0].src(),
            Some(&src_lines(0, &[Some(2), Some(2), Some(0)]))
        );
    }

    #[test]
    fn chrome_lines_are_none() {
        // Fences, a setext underline, a table's delimiter row.
        assert_eq!(
            first_src("```rust\nx\n```\n"),
            src_lines(0, &[None, Some(0), None])
        );
        assert_eq!(first_src("Title\n---\n"), src_lines(0, &[Some(0), None]));
        assert_eq!(
            first_src("| a | b |\n|---|---|\n| 1 | 2 |\n"),
            src_lines(0, &[Some(0), None, Some(0)])
        );
        // An unclosed fence has no closing line.
        assert_eq!(first_src("```\nx\n"), src_lines(0, &[None, Some(0)]));
    }

    #[test]
    fn a_code_blocks_blank_line_has_a_column() {
        // pulldown-cmark folds a blank code line into the previous line's text; it is still a
        // body line, and it has no prefix to skip.
        assert_eq!(
            first_src("    a\n\n    b\n"),
            src_lines(0, &[Some(4), Some(0), Some(4)])
        );
        // CRLF the same: the blank line's text starts at its `\r`, not past it.
        assert_eq!(
            first_src("```\r\nx\r\n\r\ny\r\n```\r\n"),
            src_lines(0, &[None, Some(0), Some(0), Some(0), None])
        );
    }

    #[test]
    fn a_rule_and_a_comment_record_where_they_start() {
        assert_eq!(first_src("  ***\n"), src_lines(0, &[Some(2)]));
        let blocks = parse("<!-- a\nb -->\n");
        assert_eq!(blocks[0].src(), Some(&src_lines(0, &[Some(0), Some(0)])));
    }

    #[test]
    fn nested_leaves_are_relative_to_their_top_level_block() {
        let blocks = parse_raw("intro\n\n- a\n- ```bash\n  code\n  ```\n\n  tail\n");
        let Block::List { items, span, .. } = &blocks[1] else {
            panic!("expected a list: {blocks:?}");
        };
        assert_eq!(*span, 0..6);
        assert_eq!(items[0].span, 0..1);
        assert_eq!(items[1].span, 1..6);
        // The fence opens on the marker line, so its first line is chrome; the body is past
        // the item's indent.
        assert_eq!(
            items[1].blocks[0].src(),
            Some(&src_lines(1, &[None, Some(2), None]))
        );
        assert_eq!(items[1].blocks[1].src(), Some(&src_lines(5, &[Some(2)])));
    }

    #[test]
    fn a_task_marker_is_not_content() {
        for src in ["- [ ] task\n", "- [ ] a\n\n- [x] b\n"] {
            let Block::List { items, .. } = &parse_raw(src)[0] else {
                panic!("expected a list");
            };
            assert_eq!(
                items[0].blocks[0].src(),
                Some(&src_lines(0, &[Some(6)])),
                "{src:?}"
            );
        }
    }

    #[test]
    fn an_item_span_stops_at_its_content_not_its_trailing_blanks() {
        // pulldown-cmark's item range runs through the bare `>` below it.
        let Block::BlockQuote { blocks, span, .. } = &parse_raw("> - a\n>\n> - b\n>\n> tail\n")[0]
        else {
            panic!("expected a quote");
        };
        assert_eq!(*span, 0..5);
        let Block::List { items, span, .. } = &blocks[0] else {
            panic!("expected a list");
        };
        assert_eq!(*span, 0..3);
        assert_eq!([items[0].span.clone(), items[1].span.clone()], [0..1, 2..3]);
        assert_eq!(blocks[1].span(), 4..5);
    }

    /// A tab the container consumes part of: pulldown-cmark synthesizes its unconsumed columns
    /// as spaces placed past the tab, but a raw char column can only name the tab itself.
    #[test]
    fn content_starting_inside_a_tab_records_the_tabs_own_column() {
        // A continuation line indented by a tab: the tab is whitespace, `b` is the content.
        let Block::List { items, .. } = &parse_raw("- a\n\tb\n")[0] else {
            panic!("expected a list");
        };
        assert_eq!(
            items[0].blocks[0].src(),
            Some(&src_lines(0, &[Some(2), Some(1)]))
        );
        // The item takes two of the first tab's columns, and the indented code begins two
        // columns into the second tab.
        let Block::List { items, .. } = &parse_raw("- a\n\n\t\tcode\n")[0] else {
            panic!("expected a list");
        };
        assert_eq!(items[0].blocks[1].src(), Some(&src_lines(2, &[Some(1)])));
    }

    /// An HTML block's leading spaces arrive as an empty-range `Text` placed past them; the
    /// recorder counts back over those spaces only, never into the container's prefix.
    #[test]
    fn indented_html_starts_at_its_indent_not_inside_the_prefix() {
        let Block::BlockQuote { blocks, .. } = &parse_raw(">   <div>\n")[0] else {
            panic!("expected a quote");
        };
        assert_eq!(blocks[0].src(), Some(&src_lines(0, &[Some(2)])));
        let Block::List { items, .. } = &parse_raw("- a\n\n   <div>\n")[0] else {
            panic!("expected a list");
        };
        assert_eq!(items[0].blocks[1].src(), Some(&src_lines(2, &[Some(2)])));
    }

    /// A column is bounded by line length, not terminal width, so it must not narrow to `u16`.
    #[test]
    fn a_column_past_u16_max_is_recorded_whole() {
        let src = format!("> a\n{}b\n", " ".repeat(70_000));
        let Block::BlockQuote { blocks, .. } = &parse_raw(&src)[0] else {
            panic!("expected a quote");
        };
        assert_eq!(
            blocks[0].src(),
            Some(&src_lines(0, &[Some(2), Some(70_000)]))
        );
    }

    #[test]
    fn a_multi_line_code_span_continues_on_its_first_lines_row() {
        assert_eq!(
            first_src("a `b\nc` d\ne\n"),
            src_lines(0, &[Some(0), None, Some(0)])
        );
    }

    /// A tight item's text is a bare inline run, which can open with any inline event.
    #[test]
    fn a_tight_item_keeps_content_that_opens_with_math_html_or_a_footnote() {
        for (src, first) in [
            (
                "- $x$ y\n",
                Inline::Math {
                    source: "x".into(),
                    display: false,
                },
            ),
            ("- <b>x</b> y\n", Inline::Text("<b>".into())),
            (
                "- [^n] y\n\n[^n]: note\n",
                Inline::FootnoteReference { label: "n".into() },
            ),
        ] {
            let Block::List { items, .. } = &parse_raw(src)[0] else {
                panic!("expected a list: {src:?}");
            };
            let Block::Paragraph {
                inlines,
                src: lines,
            } = &items[0].blocks[0]
            else {
                panic!("expected a paragraph: {src:?}");
            };
            assert_eq!(inlines[0], first, "{src:?}");
            assert_eq!(*lines, src_lines(0, &[Some(2)]), "{src:?}");
        }
    }

    /// GFM has a task box only at the start of a paragraph, so `[ ]` opening a setext heading
    /// is the heading's literal text (pulldown-cmark reports it as a task marker regardless).
    #[test]
    fn a_task_box_opening_a_heading_is_its_text() {
        for (src, text) in [("- [ ] a\n  ---\n", "[ ] a"), ("- [X] a\n  ===\n", "[X] a")] {
            let Block::List { items, .. } = &parse_raw(src)[0] else {
                panic!("expected a list");
            };
            assert_eq!(items[0].task, None, "{src:?}");
            let Block::Heading {
                inlines,
                src: lines,
                ..
            } = &items[0].blocks[0]
            else {
                panic!("expected a heading: {src:?}");
            };
            assert_eq!(inlines_to_plain(inlines), text, "{src:?}");
            assert_eq!(*lines, src_lines(0, &[Some(2), None]), "{src:?}");
        }
    }

    /// The closing `](u)` is the line's only event, an `End` repeating the whole link's range.
    #[test]
    fn a_line_opening_with_a_links_close_has_a_column() {
        assert_eq!(
            first_src("[a\n](u)\nc\n"),
            src_lines(0, &[Some(0), Some(0), Some(0)])
        );
        assert_eq!(
            first_src("x [a\n](u) t\n"),
            src_lines(0, &[Some(0), Some(0)])
        );
        let Block::BlockQuote { blocks, .. } = &parse_raw("> x [a\n>  ](u)\n")[0] else {
            panic!("expected a quote");
        };
        assert_eq!(blocks[0].src(), Some(&src_lines(0, &[Some(2), Some(3)])));
    }

    /// Alt text renders on the image's first row, whatever lines it spans.
    #[test]
    fn a_multi_line_image_continues_on_its_first_lines_row() {
        assert_eq!(
            first_src("x ![a\nb](x)\nc\n"),
            src_lines(0, &[Some(0), None, Some(0)])
        );
        // A code span inside the alt doesn't cut the image's run short.
        assert_eq!(
            first_src("![`a\nb` c\nd](x)\ne\n"),
            src_lines(0, &[Some(0), None, None, Some(0)])
        );
    }

    #[test]
    fn promotions_keep_the_replaced_blocks_lines() {
        assert_eq!(
            parse("  ![cat](cat.png)\n")[0],
            Block::ImageBlock {
                alt: "cat".into(),
                url: "cat.png".into(),
                src: src_lines(0, &[Some(2)]),
            }
        );
    }

    #[test]
    fn a_split_formula_is_relative_to_its_own_range() {
        let src = "$$\nx\n$$\n$$\ny\n$$\n";
        let (mut blocks, mut ranges) = parse_raw_with_ranges(src);
        split_display_math_paragraphs(&mut blocks, &mut ranges, src);
        assert_eq!(blocks.len(), 2, "{blocks:?}");
        for block in &blocks {
            assert_eq!(block.src(), Some(&src_lines(0, &[Some(0), None, None])));
        }
    }

    // ── Footnotes ─────────────────────────────────────────────────────────

    #[test]
    fn footnote_reference_and_definition_parse() {
        let blocks = parse("Text.[^1]\n\n[^1]: The note.\n");
        match &blocks[0] {
            Block::Paragraph { inlines, .. } => {
                assert!(
                    inlines.iter().any(
                        |i| matches!(i, Inline::FootnoteReference { label, .. } if label == "1")
                    ),
                    "inlines: {inlines:?}"
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, Block::FootnoteDefinition { label, .. } if label == "1")),
            "blocks: {blocks:?}"
        );
    }

    #[test]
    fn footnote_labels_are_preserved_verbatim() {
        // Raw labels are never remapped to display numbers, so a marker can't diverge
        // from the source: `3` is referenced before `1` and each keeps its label.
        let src = "First[^3] then[^1].\n\n[^1]: one.\n\n[^3]: three.\n";
        let blocks = parse(src);
        let mut ref_labels: Vec<String> = Vec::new();
        for b in &blocks {
            if let Block::Paragraph { inlines, .. } = b {
                for i in inlines {
                    if let Inline::FootnoteReference { label } = i {
                        ref_labels.push(label.clone());
                    }
                }
            }
        }
        assert_eq!(ref_labels, vec!["3".to_string(), "1".to_string()]);
        let def_labels: Vec<String> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::FootnoteDefinition { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(def_labels, vec!["1".to_string(), "3".to_string()]);
    }

    #[test]
    fn undefined_footnote_reference_stays_literal_text() {
        // A reference with no definition is literal text.
        let blocks = parse("A dangling[^x] marker.\n");
        match &blocks[0] {
            Block::Paragraph { inlines, .. } => {
                assert!(
                    !inlines
                        .iter()
                        .any(|i| matches!(i, Inline::FootnoteReference { .. })),
                    "undefined ref should not parse as a footnote: {inlines:?}"
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn is_html_comment_only_detects_comment_and_rejects_other_html() {
        assert!(post_pass::is_html_comment_only("<!-- hi -->"));
        assert!(post_pass::is_html_comment_only("   <!-- hi -->   "));
        assert!(post_pass::is_html_comment_only("<!---->"));
        assert!(post_pass::is_html_comment_only("<!-- a --> <!-- b -->"));
        assert!(post_pass::is_html_comment_only("<!-- a --><!-- b -->"));
        assert!(!post_pass::is_html_comment_only("<div>foo</div>"));
        assert!(!post_pass::is_html_comment_only("<!-- a --> tail"));
        assert!(!post_pass::is_html_comment_only("<!-- a"));
        assert!(!post_pass::is_html_comment_only("<!-->"));
    }

    #[test]
    fn parse_yaml_frontmatter() {
        let blocks = parse("---\ntitle: Foo\ntags: [a]\n---\n\nBody.\n");
        assert_eq!(
            blocks[0],
            Block::MetadataBlock {
                kind: MetadataKind::Yaml,
                content: "title: Foo\ntags: [a]\n".into(),
                src: src_lines(0, &[None, Some(0), Some(0), None]),
            }
        );
    }

    #[test]
    fn parse_toml_frontmatter() {
        let blocks = parse("+++\ntitle = \"Foo\"\n+++\n\nBody.\n");
        assert_eq!(
            blocks[0],
            Block::MetadataBlock {
                kind: MetadataKind::Toml,
                content: "title = \"Foo\"\n".into(),
                src: src_lines(0, &[None, Some(0), None]),
            }
        );
    }

    /// Frontmatter is data, not prose: verbatim, with no smart punctuation or inline
    /// parsing, so a quoted value or a glob's `*` round-trips unchanged.
    #[test]
    fn frontmatter_content_is_verbatim() {
        let blocks = parse("---\nglob: \"src/*.rs\" -- x\n---\n\nBody.\n");
        let Block::MetadataBlock { content, .. } = &blocks[0] else {
            panic!("expected a metadata block, got: {:?}", blocks[0]);
        };
        assert_eq!(content, "glob: \"src/*.rs\" -- x\n");
    }

    /// Frontmatter is the *first* thing in a file, but pulldown-cmark's extension is not
    /// anchored that way: an unguarded `---` above a heading would open a metadata block
    /// that the next `---` closes.
    #[test]
    fn a_mid_document_rule_pair_stays_prose() {
        let blocks = parse("Intro.\n\n---\n## Section 2\n\nText.\n\n---\n## Section 3\n");
        assert!(
            !blocks
                .iter()
                .any(|b| matches!(b, Block::MetadataBlock { .. })),
            "got: {blocks:?}",
        );
        assert!(
            matches!(blocks[1], Block::HorizontalRule { .. }),
            "got: {blocks:?}"
        );
    }

    /// Opening `+++` enables only the TOML flavor, so a later `---` pair can't be claimed.
    #[test]
    fn a_toml_opening_file_does_not_claim_a_later_dash_pair() {
        let blocks = parse("+++\na = 1\n+++\n\n---\nSection\n---\n\nEnd.\n");
        let metadata: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b, Block::MetadataBlock { .. }))
            .collect();
        assert_eq!(metadata.len(), 1, "got: {blocks:?}");
        assert_eq!(
            metadata[0],
            &Block::MetadataBlock {
                kind: MetadataKind::Toml,
                content: "a = 1\n".into(),
                src: src_lines(0, &[None, Some(0), None]),
            }
        );
    }

    /// Hugo / Jekyll / Obsidian all require the delimiter at byte 0.
    #[test]
    fn a_delimiter_below_a_blank_first_line_is_not_frontmatter() {
        let blocks = parse("\n---\ntitle: Foo\n---\n\nBody.\n");
        assert!(
            !blocks
                .iter()
                .any(|b| matches!(b, Block::MetadataBlock { .. })),
            "got: {blocks:?}",
        );
    }

    #[test]
    fn an_unclosed_frontmatter_delimiter_stays_a_rule() {
        let blocks = parse("---\ntitle: Foo\n\nBody.\n");
        assert!(
            matches!(blocks[0], Block::HorizontalRule { .. }),
            "got: {blocks:?}"
        );
    }
}

#[cfg(test)]
mod math_regression_tests {
    use super::*;
    use crate::markdown::ast::{Block, Inline};

    /// Dollar amounts in prose must not be swallowed as math: an unclosed
    /// `$` (only one delimiter) stays literal text.
    #[test]
    fn dollar_amount_stays_text() {
        let blocks = parse("Cost: $5 and $10 total.\n");
        assert!(
            matches!(&blocks[0], Block::Paragraph { inlines, .. } if inlines.iter().all(|i| !matches!(i, Inline::Math { .. }))),
            "unclosed $ must not parse as math: {:?}",
            blocks
        );
    }

    /// Inline `$x$` parses as non-display math inside a mixed paragraph.
    #[test]
    fn inline_math_parses_non_display() {
        let blocks = parse("Solve $x^2$ for x.\n");
        assert!(
            matches!(&blocks[0], Block::Paragraph { inlines, .. } if inlines.iter().any(|i| matches!(i, Inline::Math { display: false, .. }))),
            "expected inline math: {:?}",
            blocks
        );
    }

    /// Escaped `\$` stays literal text (pulldown backslash-escape).
    #[test]
    fn escaped_dollar_stays_text() {
        let blocks = parse(r"Price: \$5.\n");
        assert!(
            matches!(&blocks[0], Block::Paragraph { inlines, .. } if !inlines.iter().any(|i| matches!(i, Inline::Math { .. }))),
            "escaped $ must stay text: {:?}",
            blocks
        );
    }
}

#[cfg(test)]
mod math_bracket_tests {
    use super::*;
    use crate::markdown::ast::{Block, Inline};

    /// LaTeX `\[ ... \]` display math: pulldown-cmark 0.13 does not parse
    /// it (only `$` / `$$`), so today it arrives as literal text.  Phase 1
    /// keeps this behaviour — `\[..\]` is NOT promoted — until a custom
    /// pre-scan exists.  This test pins that decision so a future change
    /// is deliberate.
    #[test]
    fn bracket_math_is_not_parsed_yet() {
        let blocks = parse(
            r"\[
x^2
\]
",
        );
        assert!(
            matches!(&blocks[0], Block::Paragraph { inlines, .. } if inlines.iter().all(|i| !matches!(i, Inline::Math { .. }))),
            r"\[..\] must stay literal until custom pre-scan lands: {:?}",
            blocks
        );
        // Sanity: the paragraph is NOT a lone display-math paragraph.
        assert!(
            !matches!(&blocks[0], Block::Paragraph { inlines, .. } if inlines.len() == 1 && matches!(&inlines[0], Inline::Math { display: true, .. }))
        );
    }
}
