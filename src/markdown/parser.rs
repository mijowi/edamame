use std::collections::HashMap;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use super::ast::{inlines_to_plain, Block, Inline, ListItem};
use crate::diagram::DiagramSource;

/// Parse a Markdown string into a list of `Block` AST nodes.
pub fn parse(text: &str) -> Vec<Block> {
    let mut blocks = parse_raw(text);
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
pub fn parse_raw(text: &str) -> Vec<Block> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_SMART_PUNCTUATION;

    let parser = Parser::new_ext(text, options);
    let mut events = parser.peekable();
    parse_blocks(&mut events)
}

/// Post-pass: collapse a `Block::Paragraph` whose only substantive inline
/// is an `Inline::Image` into a `Block::ImageBlock`.  Whitespace-only
/// leading/trailing `Inline::Text` and soft/hard breaks are tolerated and
/// stripped.  If `real_ranges` is provided, it is kept 1:1 with `blocks`
/// (the promotion does not remove blocks, so the range vector is
/// unchanged; the parameter exists so callers that care about range
/// alignment can stay symmetric with [`attach_trailing_tui_columns_comments`]).
pub fn promote_image_paragraphs(
    blocks: &mut [Block],
    _real_ranges: Option<&mut Vec<std::ops::Range<usize>>>,
) {
    for block in blocks.iter_mut() {
        if let Block::Paragraph { inlines } = block {
            if let Some((alt, url)) = extract_lone_image(inlines) {
                *block = Block::ImageBlock { alt, url };
            }
        }
    }
}

/// Post-pass: replace every fenced code block whose language tag is
/// `mermaid` (case-insensitive) with a synthetic `Block::ImageBlock`
/// whose URL is `diagram-mermaid-<sha256(source)>`.  Returns the
/// `url → DiagramSource` map so `ParsedDoc` can attach the source to
/// `ImageBlockInfo` (needed by the decode worker to render the PNG).
///
/// Only `mermaid` is matched — not `mermaidjs`, `mermaid-diagram`, or
/// `diagram`.  GitHub and mermaid.js itself accept only the bare
/// `mermaid` tag, so accepting more would render a diagram in edamame
/// that silently falls back to a code block everywhere else.
///
/// Called from [`crate::document::ParsedDoc::build_with_overrides`]
/// only — not from [`parse`] — so the other consumers of `parse` (help
/// overlay preview, link-scan helpers, renderer tests) continue to see
/// the raw code block.
pub fn promote_diagram_code_blocks(blocks: &mut [Block]) -> HashMap<String, DiagramSource> {
    let mut sources = HashMap::new();
    for block in blocks.iter_mut() {
        let is_mermaid = matches!(
            block,
            Block::CodeBlock { language: Some(lang), .. } if lang.eq_ignore_ascii_case("mermaid")
        );
        if !is_mermaid {
            continue;
        }
        let Block::CodeBlock { content, .. } = std::mem::replace(block, Block::HorizontalRule)
        else {
            // unreachable per the matcher above, but std::mem::replace
            // forces us to consume the old value — guard with a safe
            // fallback rather than an `unwrap_or_unreachable!`.
            continue;
        };
        let source = DiagramSource::Mermaid(content);
        let url = crate::diagram::synthetic_url(&source);
        sources.insert(url.clone(), source);
        *block = Block::ImageBlock {
            alt: "mermaid diagram".to_string(),
            url,
        };
    }
    sources
}

/// Return `Some((alt, url))` iff `inlines` contains exactly one
/// `Inline::Image` plus optional whitespace-only `Inline::Text` and break
/// inlines surrounding it.  Returns `None` for paragraphs with mixed
/// content — those keep their placeholder treatment.
fn extract_lone_image(inlines: &[Inline]) -> Option<(String, String)> {
    let mut image: Option<(String, String)> = None;
    for inline in inlines {
        match inline {
            Inline::Image { alt, url } => {
                if image.is_some() {
                    return None;
                }
                image = Some((alt.clone(), url.clone()));
            }
            Inline::Text(t) if t.trim().is_empty() => {}
            Inline::SoftBreak | Inline::HardBreak => {}
            _ => return None,
        }
    }
    image
}

/// Return true iff `body` consists entirely of one or more well-formed
/// `<!-- ... -->` HTML comments, possibly separated and surrounded by
/// whitespace.  Non-comment HTML (e.g. `<div>`) and comments mixed with
/// other text both return `false` so the renderer still shows the raw
/// source for those cases.
fn is_html_comment_only(body: &str) -> bool {
    let mut rest = body.trim();
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        if !rest.starts_with("<!--") {
            return false;
        }
        // Locate the end of this comment.  `<!--` is 4 bytes; the closing
        // `-->` must start at least at index 4 so the delimiters don't
        // overlap on strings like `<!-->`.
        let Some(close) = rest[4..].find("-->") else {
            return false;
        };
        rest = rest[4 + close + 3..].trim_start();
    }
    true
}

/// Post-pass: promote any `Block::Html(body)` whose `body` matches a single
/// `<!-- ... -->` comment into `Block::HtmlComment(body)`.  The stored
/// string keeps the delimiters so downstream helpers
/// (`parse_column_widths_comment`, persistence round-trips) don't need a
/// variant-specific code path.
pub fn promote_html_comments(blocks: &mut [Block]) {
    for block in blocks.iter_mut() {
        if let Block::Html(body) = block {
            if is_html_comment_only(body) {
                let body = std::mem::take(body);
                *block = Block::HtmlComment(body);
            }
        }
    }
}

/// Post-pass: any `Block::HtmlComment` whose body is a valid
/// `<!-- tui-columns: [..] -->` marker and that directly follows a
/// `Block::Table` gets consumed — its widths are moved onto the table's
/// `user_widths` field, and the comment block is removed from the list.
/// Other comment blocks (including `<!-- tui-columns: [..] -->` comments
/// that aren't adjacent to a table) are left intact and render as zero
/// lines.
///
/// Must run AFTER [`promote_html_comments`] — the generic promotion pass
/// converts `Block::Html` comments to `Block::HtmlComment`, which this
/// function then consumes when adjacent to a table.
pub fn attach_trailing_tui_columns_comments(blocks: &mut Vec<Block>) {
    let mut i = 0;
    while i + 1 < blocks.len() {
        let is_pair = matches!(
            (&blocks[i], &blocks[i + 1]),
            (Block::Table { user_widths: None, .. }, Block::HtmlComment(body))
                if crate::markdown::table_layout::parse_column_widths_comment(body).is_some()
        );
        if is_pair {
            let body = match &blocks[i + 1] {
                Block::HtmlComment(s) => s.clone(),
                _ => unreachable!(),
            };
            let widths = crate::markdown::table_layout::parse_column_widths_comment(&body).unwrap();
            if let Block::Table { user_widths, .. } = &mut blocks[i] {
                *user_widths = Some(widths);
            }
            blocks.remove(i + 1);
            continue;
        }
        i += 1;
    }
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

            // ── Paragraph ────────────────────────────────────────────
            Some(Event::Start(Tag::Paragraph)) => {
                events.next();
                let inlines = parse_inlines(events);
                consume_end(events);
                if !inlines.is_empty() {
                    blocks.push(Block::Paragraph { inlines });
                }
            }

            // ── Headings ─────────────────────────────────────────────
            Some(Event::Start(Tag::Heading { .. })) => {
                let level = match events.next() {
                    Some(Event::Start(Tag::Heading { level, .. })) => level,
                    _ => break,
                };
                let inlines = parse_inlines(events);
                consume_end(events);
                blocks.push(Block::Heading { level, inlines });
            }

            // ── Block quote ──────────────────────────────────────────
            Some(Event::Start(Tag::BlockQuote(_))) => {
                events.next();
                let inner = parse_blocks(events);
                consume_end(events);
                blocks.push(Block::BlockQuote { blocks: inner });
            }

            // ── Fenced / indented code block ─────────────────────────
            Some(Event::Start(Tag::CodeBlock(_))) => {
                let language = match events.next() {
                    Some(Event::Start(Tag::CodeBlock(kind))) => match kind {
                        CodeBlockKind::Fenced(lang) => {
                            let s = lang.as_ref().trim().to_owned();
                            if s.is_empty() {
                                None
                            } else {
                                Some(s)
                            }
                        }
                        CodeBlockKind::Indented => None,
                    },
                    _ => None,
                };
                let content = collect_text_until_end(events);
                blocks.push(Block::CodeBlock { language, content });
            }

            // ── List ─────────────────────────────────────────────────
            Some(Event::Start(Tag::List(_))) => {
                let start = match events.next() {
                    Some(Event::Start(Tag::List(s))) => s,
                    _ => None,
                };
                let items = parse_list_items(events);
                consume_end(events);
                blocks.push(Block::List {
                    ordered: start.is_some(),
                    start,
                    items,
                });
            }

            // ── GFM table ────────────────────────────────────────────
            Some(Event::Start(Tag::Table(_))) => {
                events.next();
                let (headers, rows, col_count) = parse_table(events);
                consume_end(events);
                blocks.push(Block::Table {
                    col_count,
                    headers,
                    rows,
                    user_widths: None,
                });
            }

            // ── Horizontal rule ──────────────────────────────────────
            Some(Event::Rule) => {
                events.next();
                blocks.push(Block::HorizontalRule);
            }

            // ── Raw HTML block ───────────────────────────────────────
            // pulldown-cmark 0.11+ wraps HTML blocks in Start(HtmlBlock) /
            // End(HtmlBlock) around one-or-more Html(...) events.  Consume
            // the wrapper so the outer loop's `End(_) => break` doesn't
            // swallow the rest of the document when content follows an HTML
            // block (e.g. a persisted `<!-- tui-columns: ... -->` comment
            // between a table and subsequent paragraphs).
            Some(Event::Start(Tag::HtmlBlock)) => {
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
                blocks.push(Block::Html(body));
            }

            // Bare HTML event (pulldown-cmark < 0.11 compatibility path).
            Some(Event::Html(_)) => {
                if let Some(Event::Html(html)) = events.next() {
                    blocks.push(Block::Html(html.into_string()));
                }
            }

            // ── Inline content at block level ────────────────────────
            // This happens in tight lists: pulldown-cmark emits Text/Code
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
                items.push(ListItem { blocks, task });
            }
            _ => {
                events.next(); // skip unexpected events
            }
        }
    }

    items
}

// ─── Table parsing ────────────────────────────────────────────────────────────

fn parse_table<'a, I>(
    events: &mut std::iter::Peekable<I>,
) -> (Vec<Vec<Inline>>, Vec<Vec<Vec<Inline>>>, usize)
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
                if is_html_comment_only(&s) {
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

    // ── HTML comment promotion (Phase 12) ─────────────────────────────────

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
        // for the "Tasks — Phase 6 specialisation" bullet.
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

    #[test]
    fn is_html_comment_only_detects_comment_and_rejects_other_html() {
        assert!(is_html_comment_only("<!-- hi -->"));
        assert!(is_html_comment_only("   <!-- hi -->   "));
        assert!(is_html_comment_only("<!---->"));
        // Multiple whitespace-separated comments also count — they're all
        // annotation, none of it should reach the rendered output.
        assert!(is_html_comment_only("<!-- a --> <!-- b -->"));
        assert!(is_html_comment_only("<!-- a --><!-- b -->"));
        // Tag, not a comment.
        assert!(!is_html_comment_only("<div>foo</div>"));
        // Comment with trailing text.
        assert!(!is_html_comment_only("<!-- a --> tail"));
        // Unclosed.
        assert!(!is_html_comment_only("<!-- a"));
        // Too short to be balanced (delimiters would overlap).
        assert!(!is_html_comment_only("<!-->"));
    }
}
