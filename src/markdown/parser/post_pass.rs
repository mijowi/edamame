//! AST post-passes that run after `parse_raw`.
//!
//! These transforms mutate the block list emitted by pulldown-cmark into
//! the shape edamame's renderer wants: image paragraphs become
//! `Block::ImageBlock`, mermaid code blocks become synthetic image blocks,
//! pure-comment HTML blocks become `Block::HtmlComment`, trailing
//! `<!-- tui-columns -->` comments fold into preceding tables, and
//! blank-separated ("loose") list items are annotated with the number of
//! blank source lines preceding them so the renderer can reproduce the
//! legibility spacing while keeping the list a single `Block::List`.

use std::collections::HashMap;
use std::ops::Range;

use crate::diagram::DiagramSource;
use crate::markdown::ast::{Block, Inline};

/// Post-pass: collapse a `Block::Paragraph` whose only substantive inline
/// is an `Inline::Image` into a `Block::ImageBlock`.  Whitespace-only
/// leading/trailing `Inline::Text` and soft/hard breaks are tolerated and
/// stripped.  If `real_ranges` is provided, it is kept 1:1 with `blocks`
/// (the promotion does not remove blocks, so the range vector is
/// unchanged; the parameter exists so callers that care about range
/// alignment can stay symmetric with [`attach_trailing_tui_columns_comments`]).
pub fn promote_image_paragraphs(
    blocks: &mut [Block],
    _real_ranges: Option<&mut Vec<Range<usize>>>,
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
/// only — not from [`super::parse`] — so the other consumers of `parse`
/// (help overlay preview, link-scan helpers, renderer tests) continue to
/// see the raw code block.
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
pub(super) fn is_html_comment_only(body: &str) -> bool {
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

/// Post-pass: annotate each `ListItem` with the number of blank source
/// lines directly preceding its marker line (`ListItem::blank_lines_before`).
///
/// pulldown-cmark per CommonMark merges blank-separated items into a single
/// "loose" list.  edamame wants those inter-item blanks visible as their own
/// rendered lines (a TUI's only way to space a dense list — see the
/// discussion in `docs`), but *without* fragmenting the list: the block stays
/// one `Block::List`, and the renderer emits `blank_lines_before` blank lines
/// ahead of each item.  Keeping the list whole is what lets ordered numbering
/// come straight from pulldown-cmark (no per-group `start` re-derivation) and
/// keeps the block↔range vectors trivially 1:1 — no surgery here.
///
/// The reveal in `RenderedView` maps rendered lines to source lines by
/// splitting the block's raw text on `\n`, so the count recorded here must
/// equal the number of blank source lines actually present: only a contiguous
/// run of blank lines *directly* above item k counts.  Blank lines interior
/// to the previous item (before a nested code block, between its paragraphs)
/// reset the run and don't count, and blanks inside a `` ``` ``/`~~~` fence
/// embedded in an item are skipped entirely.
///
/// `ranges` is read-only here and stays 1:1 with `blocks`.
pub fn annotate_list_blanks(blocks: &mut [Block], ranges: &[Range<usize>], source: &str) {
    for (block, range) in blocks.iter_mut().zip(ranges.iter()) {
        let Block::List { items, .. } = block else {
            continue;
        };
        let list_src = &source[range.clone()];
        let item_offsets = top_level_item_offsets(list_src);
        // Defensive: if the source scan disagrees with the AST item count
        // (unusual list formats we don't recognise), leave the list untouched
        // — every item keeps `blank_lines_before == 0`.
        if item_offsets.len() != items.len() {
            continue;
        }
        for k in 1..item_offsets.len() {
            let prev_line_end = line_end_in_str(list_src, item_offsets[k - 1]);
            let between_start = (prev_line_end + 1).min(item_offsets[k]);
            if let Some(gap_start) =
                separator_blank_run_start(list_src, between_start, item_offsets[k])
            {
                // The run [gap_start, item_offsets[k]) is all blank lines by
                // construction; each contributes one trailing `\n`.
                items[k].blank_lines_before = list_src.as_bytes()[gap_start..item_offsets[k]]
                    .iter()
                    .filter(|&&b| b == b'\n')
                    .count();
            }
        }
    }
}

/// Return the byte offsets of every top-level item-start line within
/// `list_src`.  "Top-level" means the line's leading-whitespace indent
/// matches the first item's indent — nested content is ignored.
fn top_level_item_offsets(list_src: &str) -> Vec<usize> {
    let bytes = list_src.as_bytes();
    let mut offsets = Vec::new();
    let mut first_indent: Option<String> = None;
    let mut pos = 0;
    while pos < bytes.len() {
        let line_end = line_end_in_str(list_src, pos);
        let line = &list_src[pos..line_end];
        if let Some((indent, _, _)) = parse_marker_line(line) {
            if first_indent.is_none() {
                first_indent = Some(indent.clone());
            }
            if first_indent.as_deref() == Some(indent.as_str()) {
                offsets.push(pos);
            }
        }
        pos = if line_end < bytes.len() {
            line_end + 1
        } else {
            line_end
        };
    }
    offsets
}

fn line_end_in_str(s: &str, start: usize) -> usize {
    let bytes = s.as_bytes();
    let mut p = start;
    while p < bytes.len() && bytes[p] != b'\n' {
        p += 1;
    }
    p
}

/// Byte offset within `s` where the run of blank lines *directly
/// preceding* `end` starts, scanning `[start, end)` line by line.
/// [`annotate_list_blanks`] uses this to decide whether the next item is
/// separated from the previous one by user-visible whitespace — the count
/// of blank lines from here to the item is then recorded on the item so the
/// renderer reproduces the gap.  Returns `None` when the line directly
/// above `end` isn't blank: blank lines interior to the previous item's
/// content (before a nested code block, between its paragraphs) are not
/// separators.  Blank lines that fall between an opening and closing
/// `` ``` ``/`~~~` fence are ignored, so a code block embedded in a list
/// item never fragments its enclosing list.
fn separator_blank_run_start(s: &str, start: usize, end: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut pos = start;
    let mut fence: Option<(char, usize)> = None;
    let mut run_start: Option<usize> = None;
    while pos < end {
        let mut le = pos;
        while le < end && bytes[le] != b'\n' {
            le += 1;
        }
        let line = &s[pos..le];
        if let Some((fence_char, min_count)) = fence {
            if is_closing_fence(line, fence_char, min_count) {
                fence = None;
            }
            run_start = None;
        } else if let Some((c, count)) = parse_opening_fence(line) {
            fence = Some((c, count));
            run_start = None;
        } else if line.chars().all(char::is_whitespace) {
            run_start.get_or_insert(pos);
        } else {
            run_start = None;
        }
        pos = if le < end { le + 1 } else { le };
    }
    run_start
}

/// Recognise an opening fenced-code-block marker.  Returns the fence
/// character (`` ` `` or `~`) and its run length, or `None` if `line`
/// isn't a fence opener.  Indentation up to any depth is permitted —
/// inside a list item the fence is indented to the item's content
/// column, and we only need to track the fence/no-fence state.
fn parse_opening_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let count = trimmed.chars().take_while(|&c| c == first).count();
    if count < 3 {
        return None;
    }
    // Backtick fences disallow backticks anywhere in the info string.
    if first == '`' && trimmed[count..].contains('`') {
        return None;
    }
    Some((first, count))
}

/// Recognise a closing fence for an open fence of `fence_char` × `min_count`.
/// Per CommonMark, the closing run must use the same character, be at
/// least as long, and have only whitespace after it.
fn is_closing_fence(line: &str, fence_char: char, min_count: usize) -> bool {
    let trimmed = line.trim_start();
    let count = trimmed.chars().take_while(|&c| c == fence_char).count();
    if count < min_count {
        return false;
    }
    trimmed[count..].chars().all(char::is_whitespace)
}

/// Parse the marker prefix of `line` (a raw line without trailing `\n`).
/// Returns `(indent, marker_or_delim, optional_number)` — bullet markers
/// have `None` as the number; ordered markers carry their parsed integer.
/// Returns `None` for lines that don't start with a recognized marker.
fn parse_marker_line(line: &str) -> Option<(String, char, Option<u64>)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let indent = line[..i].to_owned();
    let rest = &line[i..];
    let rb = rest.as_bytes();
    if let Some(&c) = rb.first() {
        if matches!(c, b'-' | b'*' | b'+') && rb.get(1) == Some(&b' ') {
            return Some((indent, c as char, None));
        }
    }
    let digits_len = rb.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits_len > 0 {
        let num: u64 = rest[..digits_len].parse().ok()?;
        let delim = *rb.get(digits_len)?;
        if matches!(delim, b'.' | b')') && rb.get(digits_len + 1) == Some(&b' ') {
            return Some((indent, delim as char, Some(num)));
        }
    }
    None
}
