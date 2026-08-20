pub mod post_pass;

pub use post_pass::{
    annotate_list_blanks, attach_trailing_tui_columns_comments, is_closing_fence,
    parse_opening_fence, promote_diagram_code_blocks, promote_html_comments,
    promote_image_paragraphs,
};

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, MetadataBlockKind, Parser, Tag, TagEnd};

use super::ast::{inlines_to_plain, Block, Inline, ListItem, MetadataKind};
use super::parse_offsets;

/// Parse a Markdown string into a list of `Block` AST nodes.
pub fn parse(text: &str) -> Vec<Block> {
    // Annotate loose-list items with their preceding blank-line count so the
    // renderer reproduces the legibility spacing.  Operating on a transient
    // ranges vector — `parse` doesn't expose ranges to callers — keeps
    // callers like the help overlay rendering with the same semantics
    // as the editor pipeline.
    let (mut blocks, ranges) = parse_raw_with_ranges(text);
    annotate_list_blanks(&mut blocks, &ranges, text);
    // Promote pure-comment `Block::Html` entries to `Block::HtmlComment` BEFORE
    // the tui-columns merge runs so the merge can find the comment by its new
    // variant.  Keeping these two passes separate — generic comment hiding
    // first, tui-columns absorption second — is what lets isolated
    // `<!-- tui-columns: ... -->` blocks outside any table remain as hidden
    // `Block::HtmlComment`s instead of being silently dropped.
    promote_html_comments(&mut blocks);
    attach_trailing_tui_columns_comments(&mut blocks);
    promote_image_paragraphs(&mut blocks, None);
    blocks
}

/// Lower-level parse that returns blocks without the post-pass that merges
/// trailing `<!-- tui-columns: [..] -->` comments into preceding tables.
/// Callers that need to walk blocks alongside `parse_offsets::
/// top_level_block_ranges` in a 1:1 correspondence must use this and apply
/// [`attach_trailing_tui_columns_comments`] themselves after any range-aware
/// mutations.
///
/// The editor pipeline uses [`parse_raw_with_ranges`] instead (one parse
/// for blocks AND ranges); this stays as the ranges-free entry point for
/// module tests and the pipeline benchmarks.
#[allow(dead_code)]
pub fn parse_raw(text: &str) -> Vec<Block> {
    let parser = Parser::new_ext(text, parse_offsets::options_for(text));
    let mut events = parser.peekable();
    parse_blocks(&mut events)
}

/// Like [`parse_raw`], but also returns the top-level byte range of each
/// block — equivalent to pairing `parse_raw` with
/// [`parse_offsets::top_level_block_ranges`], in a **single**
/// pulldown-cmark pass instead of two.  The ranges are collected by a
/// [`parse_offsets::RangeTracker`] observing the same offset-iterator
/// events the AST builder consumes, so blocks and ranges stay 1:1 by
/// construction.
///
/// This is the editor pipeline's parse entry point (`ParsedDoc::build`);
/// see docs/perf-benchmark-plan.md — the second full parse cost ~35% of
/// the prose-document pipeline before the merge.
pub fn parse_raw_with_ranges(text: &str) -> (Vec<Block>, Vec<Range<usize>>) {
    let mut tracker = parse_offsets::RangeTracker::new(|_| true);
    let mut events = Parser::new_ext(text, parse_offsets::options_for(text))
        .into_offset_iter()
        .map(|(event, byte_range)| {
            tracker.observe(text, &event, &byte_range);
            event
        })
        .peekable();
    let blocks = parse_blocks(&mut events);
    // `parse_blocks` stops at the top-level end-of-stream, which normally
    // exhausts the iterator — but drain defensively so the tracker has
    // seen every event before we take its ranges.
    while events.next().is_some() {}
    drop(events);
    (blocks, tracker.into_ranges())
}

// ─── Block parsing ────────────────────────────────────────────────────────────
//
// All functions use a peek-first pattern: inspect `events.peek()` to decide
// what to do, then call `events.next()` to consume the event.  This lets us
// handle tight list items, where pulldown-cmark emits `Text` directly inside
// an `Item` without a surrounding `Paragraph`.

fn parse_blocks<'a, I>(events: &mut std::iter::Peekable<I>) -> Vec<Block>
where
    I: Iterator<Item = Event<'a>>,
{
    let mut blocks = Vec::new();

    loop {
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
                events.next();
                blocks.push(Block::HorizontalRule);
            }
            Some(Event::Start(Tag::HtmlBlock)) => {
                blocks.push(parse_html_block(events));
            }
            Some(Event::Html(_)) => {
                if let Some(Event::Html(html)) = events.next() {
                    blocks.push(Block::Html(html.into_string()));
                }
            }
            Some(Event::Start(Tag::FootnoteDefinition(_))) => {
                blocks.push(parse_footnote_definition_block(events));
            }
            Some(Event::Start(Tag::MetadataBlock(_))) => {
                blocks.push(parse_metadata_block(events));
            }

            // Inline content at block level: tight lists emit Text/Code
            // directly inside Item without a Paragraph wrapper.
            Some(Event::Text(_))
            | Some(Event::Code(_))
            | Some(Event::SoftBreak)
            | Some(Event::HardBreak)
            | Some(Event::Start(Tag::Emphasis))
            | Some(Event::Start(Tag::Strong))
            | Some(Event::Start(Tag::Strikethrough))
            | Some(Event::Start(Tag::Link { .. }))
            | Some(Event::Start(Tag::Image { .. })) => {
                let inlines = parse_inlines(events);
                if !inlines.is_empty() {
                    blocks.push(Block::Paragraph { inlines });
                }
            }

            // Skip anything else (e.g. TaskListMarker at block level)
            _ => {
                events.next();
            }
        }
    }

    blocks
}

/// Consume `Start(Paragraph) … End(Paragraph)` and return the resulting
/// `Block::Paragraph`.  Empty paragraphs (only soft breaks etc.) collapse
/// to `None` so `parse_blocks` doesn't push a noise entry.
fn parse_paragraph_block<'a, I>(events: &mut std::iter::Peekable<I>) -> Option<Block>
where
    I: Iterator<Item = Event<'a>>,
{
    events.next();
    let inlines = parse_inlines(events);
    consume_end(events);
    if inlines.is_empty() {
        None
    } else {
        Some(Block::Paragraph { inlines })
    }
}

/// Consume `Start(Heading { level, .. }) … End(Heading)`.  Returns `None`
/// only when the peeked event was lying about being a heading (defensive
/// — should never happen against a well-formed pulldown-cmark stream).
fn parse_heading_block<'a, I>(events: &mut std::iter::Peekable<I>) -> Option<Block>
where
    I: Iterator<Item = Event<'a>>,
{
    let level = match events.next()? {
        Event::Start(Tag::Heading { level, .. }) => level,
        _ => return None,
    };
    let inlines = parse_inlines(events);
    consume_end(events);
    Some(Block::Heading { level, inlines })
}

fn parse_blockquote_block<'a, I>(events: &mut std::iter::Peekable<I>) -> Block
where
    I: Iterator<Item = Event<'a>>,
{
    events.next();
    let inner = parse_blocks(events);
    consume_end(events);
    Block::BlockQuote { blocks: inner }
}

/// Consume `Start(FootnoteDefinition(label)) … End(FootnoteDefinition)`,
/// parsing the definition body as a nested block sequence (mirrors
/// `parse_blockquote_block`).
fn parse_footnote_definition_block<'a, I>(events: &mut std::iter::Peekable<I>) -> Block
where
    I: Iterator<Item = Event<'a>>,
{
    let label = match events.next() {
        Some(Event::Start(Tag::FootnoteDefinition(l))) => l.into_string(),
        // Defensive: the caller only invokes us after peeking the Start.
        _ => String::new(),
    };
    let inner = parse_blocks(events);
    consume_end(events);
    Block::FootnoteDefinition {
        label,
        blocks: inner,
    }
}

/// Consume `Start(MetadataBlock(kind)) … End(MetadataBlock(_))`.  The body
/// arrives as plain `Event::Text` (pulldown-cmark does no inline parsing
/// inside a metadata block), so the stored `content` is the frontmatter
/// verbatim, minus the two delimiter lines the events' *ranges* — but not
/// their payloads — cover.
fn parse_metadata_block<'a, I>(events: &mut std::iter::Peekable<I>) -> Block
where
    I: Iterator<Item = Event<'a>>,
{
    let kind = match events.next() {
        Some(Event::Start(Tag::MetadataBlock(MetadataBlockKind::PlusesStyle))) => {
            MetadataKind::Toml
        }
        // Defensive: the caller only invokes us after peeking the Start,
        // and YAML is the flavor `---` opens.
        _ => MetadataKind::Yaml,
    };
    let content = collect_text_until_end(events);
    Block::MetadataBlock { kind, content }
}

fn parse_code_block<'a, I>(events: &mut std::iter::Peekable<I>) -> Block
where
    I: Iterator<Item = Event<'a>>,
{
    let (language, fenced) = match events.next() {
        Some(Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang)))) => {
            let s = lang.as_ref().trim().to_owned();
            (if s.is_empty() { None } else { Some(s) }, true)
        }
        Some(Event::Start(Tag::CodeBlock(CodeBlockKind::Indented))) => (None, false),
        _ => (None, false),
    };
    let content = collect_text_until_end(events);
    Block::CodeBlock {
        language,
        content,
        fenced,
    }
}

fn parse_list_block<'a, I>(events: &mut std::iter::Peekable<I>) -> Block
where
    I: Iterator<Item = Event<'a>>,
{
    let start = match events.next() {
        Some(Event::Start(Tag::List(s))) => s,
        _ => None,
    };
    let items = parse_list_items(events);
    consume_end(events);
    Block::List {
        ordered: start.is_some(),
        start,
        items,
    }
}

fn parse_table_block<'a, I>(events: &mut std::iter::Peekable<I>) -> Block
where
    I: Iterator<Item = Event<'a>>,
{
    events.next();
    let (headers, rows, col_count) = parse_table(events);
    consume_end(events);
    Block::Table {
        col_count,
        headers,
        rows,
        user_widths: None,
    }
}

/// pulldown-cmark 0.11+ wraps HTML blocks in `Start(HtmlBlock)` /
/// `End(HtmlBlock)` around one-or-more `Html(...)` events.  Consume the
/// wrapper so the outer loop's `End(_) => break` doesn't swallow the rest
/// of the document when content follows an HTML block (e.g. a persisted
/// `<!-- tui-columns: ... -->` comment between a table and subsequent
/// paragraphs).
fn parse_html_block<'a, I>(events: &mut std::iter::Peekable<I>) -> Block
where
    I: Iterator<Item = Event<'a>>,
{
    events.next();
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
    Block::Html(body)
}

// ─── List parsing ─────────────────────────────────────────────────────────────

fn parse_list_items<'a, I>(events: &mut std::iter::Peekable<I>) -> Vec<ListItem>
where
    I: Iterator<Item = Event<'a>>,
{
    let mut items = Vec::new();

    loop {
        match events.peek() {
            None | Some(Event::End(_)) => break,
            Some(Event::Start(Tag::Item)) => {
                events.next(); // consume Start(Item)

                // Task-list marker location depends on whether the
                // surrounding list is *tight* or *loose*:
                //
                //   tight:  Start(Item) → TaskListMarker → Text(…) → End(Item)
                //   loose:  Start(Item) → Start(Paragraph) → TaskListMarker
                //             → Text(…) → End(Paragraph) → End(Item)
                //
                // Handle both.  In the loose case we speculatively consume
                // the `Start(Paragraph)` so the remaining events inside it
                // can be parsed with `parse_inlines` (which doesn't handle
                // a dangling `End(Paragraph)` on its own).
                let mut task: Option<bool> = None;
                if let Some(Event::TaskListMarker(_)) = events.peek() {
                    if let Some(Event::TaskListMarker(checked)) = events.next() {
                        task = Some(checked);
                    }
                }
                let mut paragraph_consumed = false;
                if task.is_none() && matches!(events.peek(), Some(Event::Start(Tag::Paragraph))) {
                    events.next(); // consume Start(Paragraph)
                    paragraph_consumed = true;
                    if let Some(Event::TaskListMarker(_)) = events.peek() {
                        if let Some(Event::TaskListMarker(checked)) = events.next() {
                            task = Some(checked);
                        }
                    }
                }

                let mut blocks: Vec<Block> = Vec::new();
                if paragraph_consumed {
                    // Finish the opened paragraph ourselves, then delegate
                    // to `parse_blocks` for anything else inside the item
                    // (nested lists, additional paragraphs, etc.).
                    let inlines = parse_inlines(events);
                    consume_end(events); // End(Paragraph)
                    if !inlines.is_empty() {
                        blocks.push(Block::Paragraph { inlines });
                    }
                    blocks.extend(parse_blocks(events));
                } else {
                    blocks = parse_blocks(events);
                }
                consume_end(events); // End(Item)
                items.push(ListItem {
                    blocks,
                    task,
                    blank_lines_before: 0,
                });
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

fn parse_table<'a, I>(events: &mut std::iter::Peekable<I>) -> ParsedTable
where
    I: Iterator<Item = Event<'a>>,
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

fn parse_table_row<'a, I>(events: &mut std::iter::Peekable<I>) -> Vec<Vec<Inline>>
where
    I: Iterator<Item = Event<'a>>,
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

/// Split a plain text string into `Inline`s, detecting `==highlight==` spans.
///
/// pulldown-cmark does not natively support `==text==` highlight syntax, so we
/// post-process each `Text` event to find and convert those spans.
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

fn parse_inlines<'a, I>(events: &mut std::iter::Peekable<I>) -> Vec<Inline>
where
    I: Iterator<Item = Event<'a>>,
{
    let mut inlines = Vec::new();

    loop {
        // Stop at end markers or block-level events.
        match events.peek() {
            None | Some(Event::End(_)) => break,
            Some(Event::Start(Tag::Paragraph))
            | Some(Event::Start(Tag::Heading { .. }))
            | Some(Event::Start(Tag::BlockQuote(_)))
            | Some(Event::Start(Tag::CodeBlock(_)))
            | Some(Event::Start(Tag::List(_)))
            | Some(Event::Rule) => break,
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
            // pulldown-cmark 0.11+ splits HTML events by context: block-level
            // HTML comes in as `Event::Html` (handled by the block loop) and
            // inline HTML as `Event::InlineHtml`.  We still match both here
            // so a pre-0.11 event stream (should one leak through) or a
            // raw-HTML event reported inside a paragraph is handled
            // identically.
            Event::Html(html) | Event::InlineHtml(html) => {
                let s = html.into_string();
                // Inline HTML that is a single balanced `<!-- ... -->` comment
                // becomes `Inline::HtmlComment` — rendered as zero spans.  All
                // other inline HTML (e.g. a stray `<br>`) falls back to
                // `Inline::Text` so the source is still visible.
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

            // Footnote reference (`[^label]`).  pulldown-cmark only emits
            // this when a matching definition exists.  The raw label is
            // rendered verbatim inside the bracketed marker — no display
            // renumbering.
            Event::FootnoteReference(label) => {
                inlines.push(Inline::FootnoteReference {
                    label: label.into_string(),
                });
            }

            // Task list marker inside a list item paragraph — skip here.
            Event::TaskListMarker(_) => {}

            _ => {}
        }
    }

    inlines
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Consume one `Event::End(_)` if it is next in the stream.
fn consume_end<'a, I>(events: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = Event<'a>>,
{
    if matches!(events.peek(), Some(Event::End(_))) {
        events.next();
    }
}

/// Collect all `Event::Text` content until the next `Event::End`, then consume
/// that `End`.
fn collect_text_until_end<'a, I>(events: &mut std::iter::Peekable<I>) -> String
where
    I: Iterator<Item = Event<'a>>,
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
    consume_end(events);
    text
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::ast::{Block, Inline};
    use pulldown_cmark::HeadingLevel;

    /// `parse_raw_with_ranges` must produce exactly what the old two-pass
    /// pairing produced: `parse_raw`'s blocks and
    /// `parse_offsets::top_level_block_ranges`'s ranges, 1:1.
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
        assert!(matches!(&blocks[0], Block::Paragraph { inlines } if !inlines.is_empty()));
    }

    #[test]
    fn parse_bold_and_italic() {
        let blocks = parse("**bold** and *italic*\n");
        if let Block::Paragraph { inlines } = &blocks[0] {
            assert!(inlines.iter().any(|i| matches!(i, Inline::Bold(_))));
            assert!(inlines.iter().any(|i| matches!(i, Inline::Italic(_))));
        } else {
            panic!("Expected paragraph");
        }
    }

    #[test]
    fn parse_code_span() {
        let blocks = parse("`code`\n");
        if let Block::Paragraph { inlines } = &blocks[0] {
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
        assert!(blocks.contains(&Block::HorizontalRule));
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
            if let Block::Paragraph { inlines } = &items[0].blocks[0] {
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
            Block::ImageBlock { alt, url } => {
                assert_eq!(alt, "cat");
                assert_eq!(url, "cat.png");
            }
            other => panic!("expected ImageBlock, got {other:?}"),
        }
    }

    #[test]
    fn image_with_surrounding_whitespace_still_promotes() {
        // pulldown-cmark can attach zero-width text inlines around an image
        // (e.g. from trailing spaces).  The promotion pass must tolerate them.
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
            Block::Paragraph { inlines } => {
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
        // Two images in one paragraph = mixed content; keep as-is so both
        // render as inline `[Image: alt]` placeholders.
        let blocks = parse("![a](a.png) ![b](b.png)\n");
        assert!(matches!(&blocks[0], Block::Paragraph { .. }));
    }

    // ── HTML comment promotion ────────────────────────────────────────────

    #[test]
    fn block_level_html_comment_promotes_to_html_comment() {
        let blocks = parse("<!-- hello -->\n");
        assert_eq!(blocks.len(), 1, "got {blocks:?}");
        assert!(
            matches!(&blocks[0], Block::HtmlComment(body) if body.trim() == "<!-- hello -->"),
            "got {:?}",
            blocks[0]
        );
    }

    #[test]
    fn block_level_html_tag_is_not_promoted() {
        // A `<div>` block is NOT a comment and must remain `Block::Html` so
        // the renderer still prints the raw source.
        let blocks = parse("<div>stuff</div>\n");
        assert!(matches!(&blocks[0], Block::Html(_)), "got {:?}", blocks[0]);
    }

    #[test]
    fn isolated_tui_columns_comment_not_adjacent_to_table_stays_hidden_comment() {
        // An isolated `<!-- tui-columns -->` outside any table is still a
        // comment — no table to absorb it into — so it should survive as a
        // `Block::HtmlComment` (which the renderer hides).  Regression guard
        // for the trailing-comment specialisation case.
        let blocks = parse("<!-- tui-columns: [10, 20, 30] -->\n\nSome text.\n");
        assert!(
            matches!(&blocks[0], Block::HtmlComment(_)),
            "got {:?}",
            blocks[0]
        );
        // Downstream paragraph remains.
        assert!(matches!(&blocks[1], Block::Paragraph { .. }));
    }

    #[test]
    fn inline_html_comment_produces_inline_html_comment_variant() {
        let blocks = parse("hello <!-- aside --> world\n");
        match &blocks[0] {
            Block::Paragraph { inlines } => {
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
        // Unknown inline HTML (e.g. `<br>`) is NOT a comment and must stay
        // as `Inline::Text` so the raw source is visible.
        let blocks = parse("line <br> end\n");
        match &blocks[0] {
            Block::Paragraph { inlines } => {
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
        // Regression: after the generic `promote_html_comments` runs, the
        // trailing-comment attach pass must still consume the comment when
        // it directly follows a table.  This test was passing before the
        // refactor (see `table_picks_up_trailing_tui_columns_comment`) and
        // must continue to pass.
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

    // ── Loose-list blank annotation ───────────────────────────────────────

    /// Return the single `Block::List` in `blocks`, panicking if there is
    /// not exactly one.  Loose lists stay a single block now, so every test
    /// below asserts against one list plus its per-item blank counts.
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
            } => (items, *ordered, *start),
            _ => unreachable!(),
        }
    }

    fn blanks_before(items: &[ListItem]) -> Vec<usize> {
        items.iter().map(|it| it.blank_lines_before).collect()
    }

    #[test]
    fn ordered_list_blank_between_items_stays_one_list_and_is_annotated() {
        // A blank line between items makes the list "loose" but keeps it a
        // single ordered list; numbering comes straight from pulldown-cmark
        // (1, 2, 3, 4) and the item after the blank carries a count of 1.
        let blocks = parse("1. a\n2. b\n\n3. c\n4. d\n");
        let (items, ordered, start) = only_list(&blocks);
        assert!(ordered);
        assert_eq!(start, Some(1));
        assert_eq!(items.len(), 4);
        assert_eq!(blanks_before(items), vec![0, 0, 1, 0]);
    }

    #[test]
    fn ordered_list_restart_numbering_no_longer_splits() {
        // Source numbers restart at 1 after the blank, but CommonMark treats
        // this as one loose list; edamame follows suit now (renders 1,2,3,4)
        // instead of the old split-into-two behavior.
        let blocks = parse("1. a\n2. b\n\n1. c\n2. d\n");
        let (items, ordered, start) = only_list(&blocks);
        assert!(ordered);
        assert_eq!(start, Some(1));
        assert_eq!(items.len(), 4);
        assert_eq!(blanks_before(items), vec![0, 0, 1, 0]);
    }

    #[test]
    fn bullet_list_blank_between_items_stays_one_list_and_is_annotated() {
        let blocks = parse("- a\n- b\n\n- c\n- d\n");
        let (items, ordered, _) = only_list(&blocks);
        assert!(!ordered);
        assert_eq!(items.len(), 4);
        assert_eq!(blanks_before(items), vec![0, 0, 1, 0]);
    }

    #[test]
    fn list_item_with_fenced_code_block_containing_blank_line_is_not_annotated() {
        // A blank line inside a fenced code block embedded in a list item
        // must not be counted as an inter-item separator — the next bullet
        // belongs to the same list with no preceding blank.
        let src = "- intro\n  ```toml\n  [a]\n\n  [b]\n  ```\n  trailing\n- next item\n";
        let blocks = parse(src);
        let (items, _, _) = only_list(&blocks);
        assert_eq!(items.len(), 2);
        assert_eq!(blanks_before(items), vec![0, 0]);
    }

    #[test]
    fn ordered_list_no_blank_line_annotates_all_zero() {
        let blocks = parse("1. a\n2. b\n3. c\n");
        let (items, ordered, _) = only_list(&blocks);
        assert!(ordered);
        assert_eq!(items.len(), 3);
        assert_eq!(blanks_before(items), vec![0, 0, 0]);
    }

    #[test]
    fn nested_list_with_blank_line_inside_top_level_item_stays_one_list() {
        // A blank line *inside* a nested item's content shouldn't count as a
        // top-level separator — the gap is only relevant between items at the
        // same indent level.
        let blocks = parse("- outer\n  - nested\n- next\n");
        let (items, _, _) = only_list(&blocks);
        assert_eq!(items.len(), 2);
        assert_eq!(blanks_before(items), vec![0, 0]);
    }

    #[test]
    fn every_gap_blank_separated_ordered_list_is_annotated_per_item() {
        let blocks = parse("1. a\n\n1. b\n\n1. c\n");
        let (items, ordered, start) = only_list(&blocks);
        assert!(ordered);
        assert_eq!(start, Some(1));
        assert_eq!(items.len(), 3);
        assert_eq!(blanks_before(items), vec![0, 1, 1]);
    }

    #[test]
    fn interior_blank_without_separator_blank_is_not_annotated() {
        // A blank line interior to an item's content (between the item's
        // first paragraph and its continuation) is not an inter-item
        // separator — the next marker sits directly after the continuation,
        // so item `b` carries a zero count.
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
        // An item whose content spans several lines (continuation + nested
        // fenced code block) followed by a blank-separated second item stays
        // a single list; only the blank directly above item 2 is counted.
        let src = "1. **first** item\n   continuation\n\n   ```rust\n   let x = 1;\n   ```\n\n2. second\n";
        let (mut blocks, ranges) = parse_raw_with_ranges(src);
        annotate_list_blanks(&mut blocks, &ranges, src);
        let (items, _, _) = only_list(&blocks);
        assert_eq!(items.len(), 2);
        assert_eq!(blanks_before(items), vec![0, 1]);
    }

    // ── Footnotes ─────────────────────────────────────────────────────────

    #[test]
    fn footnote_reference_and_definition_parse() {
        let blocks = parse("Text.[^1]\n\n[^1]: The note.\n");
        // First block: paragraph carrying the reference.
        match &blocks[0] {
            Block::Paragraph { inlines } => {
                assert!(
                    inlines.iter().any(
                        |i| matches!(i, Inline::FootnoteReference { label, .. } if label == "1")
                    ),
                    "inlines: {inlines:?}"
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
        // Last block: the definition.
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, Block::FootnoteDefinition { label, .. } if label == "1")),
            "blocks: {blocks:?}"
        );
    }

    #[test]
    fn footnote_labels_are_preserved_verbatim() {
        // The parser stores raw labels and does NOT remap them to display
        // numbers — rendered markers never diverge from the source.  `3` is
        // referenced before `1`, but each keeps its own label.
        let src = "First[^3] then[^1].\n\n[^1]: one.\n\n[^3]: three.\n";
        let blocks = parse(src);
        let mut ref_labels: Vec<String> = Vec::new();
        for b in &blocks {
            if let Block::Paragraph { inlines } = b {
                for i in inlines {
                    if let Inline::FootnoteReference { label } = i {
                        ref_labels.push(label.clone());
                    }
                }
            }
        }
        assert_eq!(ref_labels, vec!["3".to_string(), "1".to_string()]);
        // Definitions keep their raw labels too.
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
        // pulldown-cmark only emits a FootnoteReference when a matching
        // definition exists; `[^x]` with no definition is literal text.
        let blocks = parse("A dangling[^x] marker.\n");
        match &blocks[0] {
            Block::Paragraph { inlines } => {
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
        // Multiple whitespace-separated comments also count — they're all
        // annotation, none of it should reach the rendered output.
        assert!(post_pass::is_html_comment_only("<!-- a --> <!-- b -->"));
        assert!(post_pass::is_html_comment_only("<!-- a --><!-- b -->"));
        // Tag, not a comment.
        assert!(!post_pass::is_html_comment_only("<div>foo</div>"));
        // Comment with trailing text.
        assert!(!post_pass::is_html_comment_only("<!-- a --> tail"));
        // Unclosed.
        assert!(!post_pass::is_html_comment_only("<!-- a"));
        // Too short to be balanced (delimiters would overlap).
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
            }
        );
    }

    /// Frontmatter is data, not prose: the content must arrive verbatim,
    /// with no smart-punctuation substitution and no inline parsing, so a
    /// quoted value or an `*` in a glob round-trips unchanged.
    #[test]
    fn frontmatter_content_is_verbatim() {
        let blocks = parse("---\nglob: \"src/*.rs\" -- x\n---\n\nBody.\n");
        let Block::MetadataBlock { content, .. } = &blocks[0] else {
            panic!("expected a metadata block, got: {:?}", blocks[0]);
        };
        assert_eq!(content, "glob: \"src/*.rs\" -- x\n");
    }

    /// Frontmatter is the *first* thing in a file.  pulldown-cmark's
    /// extensions are not anchored that way, so a `---` separator sitting
    /// immediately above a heading would otherwise open a block that the
    /// next `---` closes, turning the section between them into metadata.
    #[test]
    fn a_mid_document_rule_pair_stays_prose() {
        let blocks = parse("Intro.\n\n---\n## Section 2\n\nText.\n\n---\n## Section 3\n");
        assert!(
            !blocks
                .iter()
                .any(|b| matches!(b, Block::MetadataBlock { .. })),
            "got: {blocks:?}",
        );
        assert_eq!(blocks[1], Block::HorizontalRule);
    }

    /// A file that opens `+++` enables only the TOML flavor, so a later
    /// `---` pair cannot be claimed as YAML frontmatter.
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
            }
        );
    }

    /// A leading blank line means the delimiter is not at byte 0, and
    /// Hugo / Jekyll / Obsidian all require it to be.
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
        assert_eq!(blocks[0], Block::HorizontalRule);
    }
}
