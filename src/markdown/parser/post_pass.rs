//! AST post-passes that run after `parse_raw`, reshaping pulldown-cmark's block list for
//! the renderer: image paragraphs and mermaid code blocks become `Block::ImageBlock`,
//! pure-comment HTML becomes `Block::HtmlComment`, trailing `<!-- tui-columns -->`
//! comments fold into preceding tables.  Every promotion carries the replaced block's
//! [`SrcLines`] across.

use std::collections::HashMap;
use std::ops::Range;

use crate::diagram::DiagramSource;
use crate::markdown::ast::{Block, Inline, SrcLines};

/// Collapse a `Block::Paragraph` whose only substantive inline is an `Inline::Image` into
/// a `Block::ImageBlock`.  `real_ranges` is untouched — the promotion removes no blocks;
/// the parameter exists for symmetry with [`attach_trailing_tui_columns_comments`].
pub fn promote_image_paragraphs(
    blocks: &mut [Block],
    _real_ranges: Option<&mut Vec<Range<usize>>>,
) {
    for block in blocks.iter_mut() {
        if let Block::Paragraph { inlines, src } = block {
            if let Some((alt, url)) = extract_lone_image(inlines) {
                let src = std::mem::take(src);
                *block = Block::ImageBlock { alt, url, src };
            }
        }
    }
}

/// Replace every `mermaid`-tagged fenced code block with a synthetic `Block::ImageBlock`,
/// returning the `url → DiagramSource` map so `ParsedDoc` can hand the source to the
/// decode worker.
///
/// Only the bare `mermaid` tag is matched (not `mermaidjs`, `diagram`, …): GitHub and
/// mermaid.js accept only that, so accepting more would render here what falls back to a
/// code block everywhere else.  Called only from
/// [`crate::document::ParsedDoc::build_with_overrides`], so `parse`'s other consumers
/// still see the raw code block.
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
        let placeholder = Block::HorizontalRule {
            src: SrcLines::default(),
        };
        let Block::CodeBlock { content, src, .. } = std::mem::replace(block, placeholder) else {
            // Unreachable per the matcher above; a safe fallback rather than a panic.
            continue;
        };
        let source = DiagramSource::Mermaid(content);
        let url = crate::diagram::synthetic_url(&source);
        sources.insert(url.clone(), source);
        *block = Block::ImageBlock {
            alt: "mermaid diagram".to_string(),
            url,
            src,
        };
    }
    sources
}

/// Rescue a `$$…$$`-delimited paragraph that pulldown-cmark left as plain text.
///
/// pulldown-cmark's math extension tracks brace nesting: a `$$` closing delimiter that falls
/// inside an unbalanced `{ … }` group is not treated as a delimiter, so a formula like
/// `$$x^{123$$` (a half-typed `x^{123}`) never becomes `Event::DisplayMath` — it is emitted as
/// bare `Text`/`SoftBreak` inlines instead.  Mid-typing, that means a `$$…$$` block silently stops
/// being math the instant a brace is left open: it is no longer promoted, its reserved image rows
/// vanish, and the formula reflows to prose — snapping back only once the brace is closed.
///
/// This runs before [`promote_display_math_paragraphs`] / [`split_display_math_paragraphs`] and
/// rewrites such a paragraph back into a single `Inline::Math { display: true }`, so it flows
/// through the normal promotion and keeps its reserved rows.  The render then fails cleanly on the
/// invalid LaTeX (placeholder / persisted preview band), exactly like any other broken formula —
/// the block stays a stable, reserved figure the whole time the braces are unbalanced.
///
/// Deliberately conservative: only a paragraph whose *source* is exactly one `$$ … $$` pair (no
/// interior `$$`, non-empty body), and never one pulldown already parsed as display math.  The
/// carved `inner` matches pulldown's own source convention — the delimiters stripped, interior
/// whitespace (including the delimiter-line newlines) kept — so `split_math_ranges` and
/// `display_math_block_body` treat it identically to a formula pulldown parsed itself.
pub fn reconstruct_broken_display_math(
    blocks: &mut [Block],
    real_ranges: &[Range<usize>],
    source: &str,
) {
    for (block, range) in blocks.iter_mut().zip(real_ranges) {
        let Block::Paragraph { inlines, src } = block else {
            continue;
        };
        // Already recognized as display math — pulldown got it right, leave it.
        if collect_display_math_only(inlines).is_some() {
            continue;
        }
        let Some(raw) = source.get(range.clone()) else {
            continue;
        };
        let trimmed = raw.trim_matches(|c: char| c.is_ascii_whitespace());
        let Some(inner) = trimmed
            .strip_prefix("$$")
            .and_then(|body| body.strip_suffix("$$"))
        else {
            continue;
        };
        // One `$$…$$` pair with a body: an interior `$$` means multiple formulas or prose, which
        // we leave to pulldown's own (correct) handling.
        if inner.is_empty() || inner.contains("$$") {
            continue;
        }
        *inlines = vec![Inline::Math {
            source: inner.to_string(),
            display: true,
        }];
        // One formula is one atomic inline, recorded the way pulldown-cmark's own parse of it
        // is: its later lines carry no column.
        let cols: Vec<Option<u32>> = src
            .cols()
            .enumerate()
            .map(|(k, col)| col.filter(|_| k == 0))
            .collect();
        *src = SrcLines::new(src.first, &cols);
    }
}

/// Replace every display-math-only paragraph (one or more `Inline::Math { display: true }`,
/// separated by breaks and whitespace-only text) with one synthetic `Block::ImageBlock` **per
/// formula**, URL `diagram-math-<sha256(source)>`.  Returns the `url → DiagramSource` map, merged
/// into [`promote_diagram_code_blocks`]'s so `ParsedDoc` attaches the source to `ImageBlockInfo`
/// for the decode worker.
///
/// pulldown-cmark folds stacked `$$...$$` blocks (no blank line between) into one paragraph of
/// `[Math, SoftBreak, Math]`; this re-splits it into one image block per formula, `real_ranges`
/// rewritten to stay 1:1 with `blocks`.  Paragraphs mixing math with other inlines are left alone.
/// Called from [`crate::document::ParsedDoc::build_with_overrides`] only — not [`super::parse`] —
/// like [`promote_diagram_code_blocks`], so other `parse` consumers keep seeing the paragraph.
pub fn promote_display_math_paragraphs(
    blocks: &mut Vec<Block>,
    real_ranges: &mut Vec<Range<usize>>,
    source: &str,
) -> HashMap<String, DiagramSource> {
    let mut sources = HashMap::new();
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len());
    let mut out_ranges: Vec<Range<usize>> = Vec::with_capacity(real_ranges.len());
    for (block, range) in blocks.drain(..).zip(real_ranges.drain(..)) {
        let Block::Paragraph { inlines, src } = &block else {
            out.push(block);
            out_ranges.push(range);
            continue;
        };
        let Some(math_sources) = collect_display_math_only(inlines) else {
            out.push(block);
            out_ranges.push(range);
            continue;
        };
        // Carve each formula's byte range from the paragraph's source
        // text.  `split_math_ranges` walks the paragraph body locating
        // `$$` delimiter pairs; a failure to locate them (shouldn't
        // happen — pulldown already parsed them) falls back to keeping
        // the paragraph as-is.
        let Some(piece_ranges) = split_math_ranges(source, &range, math_sources.len()) else {
            out.push(block);
            out_ranges.push(range);
            continue;
        };
        for (i, formula) in math_sources.iter().enumerate() {
            let diagram_source = DiagramSource::Latex(formula.clone());
            let url = crate::diagram::synthetic_url(&diagram_source);
            sources.insert(url.clone(), diagram_source);
            out.push(Block::ImageBlock {
                alt: "math".to_string(),
                url,
                src: piece_src_lines(src, source, &range, &piece_ranges[i]),
            });
            out_ranges.push(piece_ranges[i].clone());
        }
    }
    *blocks = out;
    *real_ranges = out_ranges;
    sources
}

/// Figures-off counterpart of [`promote_display_math_paragraphs`]: when a display-math-only
/// paragraph holds more than one `$$...$$` formula (pulldown folds stacked formulas into one
/// paragraph), split it into one single-formula `Block::Paragraph` per formula so each renders as
/// its own fenced-style `math` code block — the figures-on block boundaries minus the image.
/// `real_ranges` is rewritten 1:1, each range carved by [`split_math_ranges`].  A single-formula
/// paragraph is left untouched (already one block, painted via [`display_math_block_body`]).
/// Called from [`crate::document::ParsedDoc::build_with_overrides`] only, figures-off branch.
pub fn split_display_math_paragraphs(
    blocks: &mut Vec<Block>,
    real_ranges: &mut Vec<Range<usize>>,
    source: &str,
) {
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len());
    let mut out_ranges: Vec<Range<usize>> = Vec::with_capacity(real_ranges.len());
    for (block, range) in blocks.drain(..).zip(real_ranges.drain(..)) {
        let split = match &block {
            Block::Paragraph { inlines, .. } => collect_display_math_only(inlines)
                .filter(|formulas| formulas.len() >= 2)
                .and_then(|formulas| {
                    split_math_ranges(source, &range, formulas.len())
                        .map(|ranges| (formulas, ranges))
                }),
            _ => None,
        };
        match split {
            Some((formulas, piece_ranges)) => {
                let para_src = block.src().cloned().unwrap_or_default();
                for (i, formula) in formulas.into_iter().enumerate() {
                    out.push(Block::Paragraph {
                        inlines: vec![Inline::Math {
                            source: formula,
                            display: true,
                        }],
                        src: piece_src_lines(&para_src, source, &range, &piece_ranges[i]),
                    });
                    out_ranges.push(piece_ranges[i].clone());
                }
            }
            None => {
                out.push(block);
                out_ranges.push(range);
            }
        }
    }
    *blocks = out;
    *real_ranges = out_ranges;
}

/// The [`SrcLines`] of one formula carved out of a display-math paragraph: the paragraph's own,
/// cut to the lines `piece` covers and re-anchored at the piece's first line, which is where
/// its new top-level range starts.
fn piece_src_lines(
    para: &SrcLines,
    source: &str,
    para_range: &Range<usize>,
    piece: &Range<usize>,
) -> SrcLines {
    let lines_between = |from: usize, to: usize| {
        source
            .get(from..to)
            .map_or(0, |s| s.bytes().filter(|&b| b == b'\n').count())
    };
    let offset = lines_between(para_range.start, piece.start);
    let body = source.get(piece.clone()).unwrap_or("");
    let len = body
        .trim_end_matches('\n')
        .bytes()
        .filter(|&b| b == b'\n')
        .count()
        + 1;
    let skip = offset.saturating_sub(para.first as usize);
    let cols: Vec<Option<u32>> = para
        .cols()
        .chain(std::iter::repeat(None))
        .skip(skip)
        .take(len)
        .collect();
    SrcLines::new(0, &cols)
}

/// If `inlines` contains only display-math inlines (plus soft/hard breaks
/// and whitespace-only text between them), return each formula's LaTeX
/// source in order.  Returns `None` for mixed paragraphs, lone inline
/// (`$...$`) math, or a paragraph with no display math at all.
fn collect_display_math_only(inlines: &[Inline]) -> Option<Vec<String>> {
    let mut formulas: Vec<String> = Vec::new();
    for inline in inlines {
        match inline {
            Inline::Math {
                source,
                display: true,
            } => formulas.push(source.clone()),
            Inline::Math { .. } => return None, // lone inline $...$
            Inline::Text(t) if t.trim().is_empty() => {}
            Inline::SoftBreak | Inline::HardBreak => {}
            _ => return None,
        }
    }
    (!formulas.is_empty()).then_some(formulas)
}

/// A figures-off `$$...$$` paragraph stays a `Block::Paragraph` and renders as a fenced-style
/// `math` code block — the source counterpart of the display-math reveal, like a `` ```mermaid ``
/// fence when figures are off.
///
/// Returns the formula body (LaTeX with the delimiter newlines stripped, for
/// `Renderer::render_code_block(Some("math"), body, true, …)`) iff `block` is a paragraph whose
/// only inline is a single **multi-line** `$$\n…\n$$` formula (delimiters on their own lines).
/// `None` for everything else — a mixed paragraph, stacked formulas, inline `$…$`, or a one-line
/// `$$x$$` (no delimiter rows to align the fence against).
///
/// The renderer records that shape's rows 1:1 with the source lines in their `RowOrigin`s, which
/// is what the cursor and the click hit-test read.
pub(crate) fn display_math_block_body(block: &Block) -> Option<String> {
    let Block::Paragraph { inlines, .. } = block else {
        return None;
    };
    let mut formula: Option<&str> = None;
    for inline in inlines {
        match inline {
            Inline::Math {
                source,
                display: true,
            } if formula.is_none() => formula = Some(source),
            Inline::Math { .. } => return None, // a second formula, or inline $…$
            Inline::Text(t) if t.trim().is_empty() => {}
            Inline::SoftBreak | Inline::HardBreak => {}
            _ => return None,
        }
    }
    // Require `$$` on their own lines: the source then reads `\n…\n`, and
    // stripping one newline each side leaves the body whose rendered fence
    // rows line up 1:1 with the source's two `$$` lines.
    let inner = formula?.strip_prefix('\n')?.strip_suffix('\n')?;
    Some(inner.to_string())
}

/// Split a display-math paragraph's source into `count` byte ranges, one per `$$...$$` formula.
/// Each range runs from a formula's opening `$$` to just past its closing `$$` (the last extends
/// to the paragraph end); the whitespace *between* formulas is left out of both — `ParsedDoc::build`
/// covers it via `extended_ranges`, and absorbing it here would give the preceding formula an extra
/// reveal row.  `None` when the delimiters cannot be matched (paragraph left as-is).
///
/// Assumes each formula is one `$$...$$` pair with no literal `$$` in its body — true for anything
/// pulldown already parsed as a single `DisplayMath` event; a stray interior `$$` would miscount,
/// which the `None` fallback does not catch, so this pins the invariant.
fn split_math_ranges(source: &str, para: &Range<usize>, count: usize) -> Option<Vec<Range<usize>>> {
    let body = source.get(para.clone())?;
    let mut ranges = Vec::with_capacity(count);
    let mut search_from = 0usize;
    for i in 0..count {
        let open = body[search_from..].find("$$")? + search_from;
        // Closing `$$`: find the next occurrence after the opening.
        let close_rel = body[open + 2..].find("$$")?;
        let close = open + 2 + close_rel;
        // If this is the last formula, the range extends to the end of
        // the paragraph (absorbing trailing soft break / whitespace).
        let end = if i + 1 == count {
            body.len()
        } else {
            close + 2
        };
        ranges.push(para.start + open..para.start + end);
        search_from = close + 2;
    }
    Some(ranges)
}

/// `Some((alt, url))` iff `inlines` is exactly one `Inline::Image` plus optional
/// whitespace-only text and breaks.  Mixed content keeps its placeholder treatment.
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

/// Whether `body` is entirely well-formed `<!-- ... -->` comments plus whitespace.
/// Anything else is `false`, so the renderer still shows the raw source for it.
pub(crate) fn is_html_comment_only(body: &str) -> bool {
    let mut rest = body.trim();
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        if !rest.starts_with("<!--") {
            return false;
        }
        // The closing `-->` must start at index 4 or later, so the delimiters can't
        // overlap on strings like `<!-->`.
        let Some(close) = rest[4..].find("-->") else {
            return false;
        };
        rest = rest[4 + close + 3..].trim_start();
    }
    true
}

/// Promote comment-only `Block::Html` into `Block::HtmlComment`, at any depth: a comment in a
/// list item, quote or footnote renders nothing, as a top-level one does.  The stored string
/// keeps its delimiters so downstream helpers need no variant-specific path.
pub fn promote_html_comments(blocks: &mut [Block]) {
    for block in blocks.iter_mut() {
        match block {
            Block::Html(body, src) if is_html_comment_only(body) => {
                let body = std::mem::take(body);
                let src = std::mem::take(src);
                *block = Block::HtmlComment(body, src);
            }
            _ => for_each_child_list(block, |children| promote_html_comments(children)),
        }
    }
}

/// Fold a `<!-- tui-columns: [..] -->` comment that directly follows a `Block::Table` into
/// that table's `user_widths`, removing the comment block.  Non-adjacent ones are left
/// intact and render as zero lines.
///
/// Must run AFTER [`promote_html_comments`], which is what creates the `HtmlComment`
/// blocks this consumes.
pub fn attach_trailing_tui_columns_comments(blocks: &mut Vec<Block>) {
    let mut i = 0;
    while i + 1 < blocks.len() {
        let is_pair = matches!(
            (&blocks[i], &blocks[i + 1]),
            (Block::Table { user_widths: None, .. }, Block::HtmlComment(body, _))
                if crate::markdown::table_layout::parse_column_widths_comment(body).is_some()
        );
        if is_pair {
            let body = match &blocks[i + 1] {
                Block::HtmlComment(s, _) => s.clone(),
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

/// [`attach_trailing_tui_columns_comments`] inside containers: a table in a list item, quote or
/// footnote takes its widths from a comment directly after it.  The comment stays, an
/// `HtmlComment` rendering nothing: a container gives every line between its children a blank
/// row (`hidden` excepts only link reference definitions), so removing it would show its line as
/// one.  Runs after the top-level merge, at every depth below it.
pub fn attach_nested_tui_columns_comments(blocks: &mut [Block]) {
    for block in blocks.iter_mut() {
        for_each_child_list(block, |children| {
            for i in 1..children.len() {
                let Block::HtmlComment(body, _) = &children[i] else {
                    continue;
                };
                let Some(widths) = crate::markdown::table_layout::parse_column_widths_comment(body)
                else {
                    continue;
                };
                if let Block::Table { user_widths, .. } = &mut children[i - 1] {
                    user_widths.get_or_insert(widths);
                }
            }
            attach_nested_tui_columns_comments(children);
        });
    }
}

/// Call `f` on each child block list of a container: a quote's or footnote's blocks, each list
/// item's.  A leaf has none.
fn for_each_child_list(block: &mut Block, mut f: impl FnMut(&mut Vec<Block>)) {
    match block {
        Block::BlockQuote { blocks, .. } | Block::FootnoteDefinition { blocks, .. } => f(blocks),
        Block::List { items, .. } => items.iter_mut().for_each(|item| f(&mut item.blocks)),
        _ => {}
    }
}
