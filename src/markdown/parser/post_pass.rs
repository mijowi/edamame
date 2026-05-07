//! AST post-passes that run after `parse_raw`.
//!
//! These transforms mutate the block list emitted by pulldown-cmark into
//! the shape edamame's renderer wants: image paragraphs become
//! `Block::ImageBlock`, mermaid code blocks become synthetic image blocks,
//! pure-comment HTML blocks become `Block::HtmlComment`, trailing
//! `<!-- tui-columns -->` comments fold into preceding tables, and
//! blank-separated list items split into their own `Block::List` so each
//! gap is rendered as its own visible blank line.

use std::collections::HashMap;
use std::ops::Range;

use crate::diagram::DiagramSource;
use crate::markdown::ast::{Block, Inline, ListItem};

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

/// Post-pass: split each top-level `Block::List` whose source contains a
/// blank line outside any fenced code block between consecutive top-level
/// items.  pulldown-cmark per CommonMark merges blank-separated items
/// into a single loose list, but for editor purposes we want every
/// inter-item blank to be visible as its own line in the rendered output
/// — so each blank-separated run of items becomes its own
/// `Block::List`, and `parsed_doc` then emits the inter-block gap as a
/// rendered blank line via the same path it uses for any other top-level
/// gap.  For ordered lists, the post-split groups keep their source
/// numbers as `start`, so a list whose source items number `1. a, 2. b`
/// then (after a blank) `3. c` keeps its continuous numbering on screen,
/// while a list whose source restarts at `1. c` after the blank
/// correctly restarts at 1 in the lower group.  Blank lines that fall
/// inside a `` ``` ``/`~~~` fence inside an item are skipped: a code
/// block embedded in a list item must not fragment its enclosing list,
/// however many blank lines its content contains.
///
/// Mutates both `blocks` and `ranges` so the 1:1 invariant relied on by
/// `parsed_doc` is preserved.  For each split, the group's `start` is
/// re-derived from the source line's marker number (ordered) or left as
/// `None` (bullets).
pub fn split_lists_on_blank_lines(
    blocks: &mut Vec<Block>,
    ranges: &mut Vec<Range<usize>>,
    source: &str,
) {
    let mut i = 0;
    while i < blocks.len() {
        if !matches!(&blocks[i], Block::List { .. }) {
            i += 1;
            continue;
        }
        let list_range = ranges[i].clone();
        let list_src = &source[list_range.clone()];
        let item_offsets = top_level_item_offsets(list_src);

        // Identify split points: indices `k > 0` where item k is preceded
        // in the source by at least one blank line that isn't sitting
        // inside a fenced code block.
        let mut split_indices: Vec<usize> = Vec::new();
        for k in 1..item_offsets.len() {
            let prev_line_end = line_end_in_str(list_src, item_offsets[k - 1]);
            let between_start = (prev_line_end + 1).min(item_offsets[k]);
            if has_blank_line_outside_code(list_src, between_start, item_offsets[k]) {
                split_indices.push(k);
            }
        }
        if split_indices.is_empty() {
            i += 1;
            continue;
        }

        // Pop the list and replace with N split lists.
        let (ordered, all_items) = match blocks.remove(i) {
            Block::List { ordered, items, .. } => (ordered, items),
            _ => unreachable!(),
        };
        ranges.remove(i);

        // Defensive: if the AST item count disagrees with what we found in
        // source (e.g. unusual list formats we don't recognise), restore the
        // block intact and skip.
        if all_items.len() != item_offsets.len() {
            blocks.insert(
                i,
                Block::List {
                    ordered,
                    start: None,
                    items: all_items,
                },
            );
            ranges.insert(i, list_range);
            i += 1;
            continue;
        }

        // Build group boundaries: [0, split_indices..., items.len()].
        let mut group_first: Vec<usize> = vec![0];
        group_first.extend(split_indices.iter().copied());
        let mut group_last_exclusive: Vec<usize> = group_first[1..].to_vec();
        group_last_exclusive.push(all_items.len());

        // Move items into per-group buckets.
        let mut all_iter = all_items.into_iter();
        let mut groups: Vec<Vec<ListItem>> = Vec::with_capacity(group_first.len());
        for (g_first, g_last_exc) in group_first.iter().zip(group_last_exclusive.iter()) {
            let count = g_last_exc - g_first;
            let mut grp: Vec<ListItem> = Vec::with_capacity(count);
            for _ in 0..count {
                grp.push(all_iter.next().expect("partition matches item count"));
            }
            groups.push(grp);
        }

        let group_count = groups.len();
        for (g_idx, group_items) in groups.into_iter().enumerate() {
            let first_item_idx = group_first[g_idx];
            let group_start_in_src = item_offsets[first_item_idx];

            // The group's source ends just after the natural newline of its
            // last item's first line.  This leaves the blank-line bytes
            // between groups uncovered — `parsed_doc` then treats them as
            // virtual blank-line blocks, the same way it does for any other
            // gap between top-level blocks.  The final group always extends
            // to the original list range's end so trailing newlines stay
            // accounted for.
            let group_end_in_src = if g_idx + 1 < group_count {
                let last_item_idx = group_last_exclusive[g_idx] - 1;
                let last_line_end = line_end_in_str(list_src, item_offsets[last_item_idx]);
                (last_line_end + 1).min(list_src.len())
            } else {
                list_src.len()
            };
            let abs_start = list_range.start + group_start_in_src;
            let abs_end = list_range.start + group_end_in_src;

            let start_num = if ordered {
                let line_end = line_end_in_str(list_src, group_start_in_src);
                let line = &list_src[group_start_in_src..line_end];
                parse_marker_line(line).and_then(|(_, _, num)| num)
            } else {
                None
            };

            blocks.insert(
                i,
                Block::List {
                    ordered,
                    start: start_num,
                    items: group_items,
                },
            );
            ranges.insert(i, abs_start..abs_end);
            i += 1;
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

/// Detect whether the byte range `[start, end)` of `s` contains at least
/// one blank line that is not inside a fenced code block.
/// [`split_lists_on_blank_lines`] uses this to decide whether the gap
/// between two list items contains user-visible whitespace — the
/// post-pass then splits the parent list at that point so the gap shows
/// up as a rendered blank line.  Blank lines that fall between an
/// opening and closing `` ``` ``/`~~~` fence are ignored, so a code
/// block embedded in a list item never fragments its enclosing list.
fn has_blank_line_outside_code(s: &str, start: usize, end: usize) -> bool {
    let bytes = s.as_bytes();
    let mut pos = start;
    let mut fence: Option<(char, usize)> = None;
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
        } else if let Some((c, count)) = parse_opening_fence(line) {
            fence = Some((c, count));
        } else if line.chars().all(char::is_whitespace) {
            return true;
        }
        pos = if le < end { le + 1 } else { le };
    }
    false
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
/// Returns `None` for lines that don't start with a recognised marker.
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
