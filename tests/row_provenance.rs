//! Agreement test for row provenance (`docs/dev/plans/row-provenance.md` Phase 2): every
//! rendered row's `RowOrigin` is checked against the rendered output itself — not against any
//! other mapping, so it can't inherit that mapping's bugs.
//!
//! Universal: one origin per row; a row's lines lie inside its block and are non-empty, and a
//! block's rows never step back to an earlier first line; its content starts within the row and
//! within its source line.  Per kind, the row's content (its
//! text past `rendered_col`) is compared with the source content (the line's text past
//! `raw_col`):
//! - `Inline` / `Flow`: an `InlineColMap` over the source content renders exactly as many chars
//!   as the row shows, and every letter or digit maps onto the same letter or digit (a flow's
//!   lines mapped joined or one by one, whichever way the document parsed them);
//! - `Verbatim`: the row's content is the source content (trailing padding aside);
//! - `TableRow`: the row exists, the chunk is below its tallest cell's chunk count, and each cell
//!   shows its chunk of the shared wrap.

#[path = "support/markdown_gen.rs"]
mod markdown_gen;

use std::time::{Duration, Instant};

use crossterm::event::KeyModifiers;
use proptest::prelude::*;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::Terminal;

use edamame::config::Theme;
use edamame::document::{row_map, Buffer, ParsedDoc};
use edamame::editor::{mouse_ops, EditorState, Mode};
use edamame::input::MouseAction;
use edamame::markdown::ast::ListItem;
use edamame::markdown::table_layout::{char_cells, rendered_pipe_cells, wrap_cell};
use edamame::markdown::{
    inlines_to_plain, strip_atx_closing, Block, ColOrigin, ContentKind, Inline, InlineColMap,
    RefLabels, RowOrigin, SrcLines,
};
use edamame::ui::{RenderedView, RenderedViewState};

fn theme() -> &'static Theme {
    Box::leak(Box::new(Theme::default()))
}

/// The settings that change which rows a document renders.  Each document is checked with reflow
/// off and on, both plain and with everything that adds or reshapes rows: big H1 glyph rows, row
/// striping, and diagrams left as code (so a `$$…$$` formula renders figures-off).
#[derive(Debug, Clone, Copy)]
struct Variant {
    reflow: bool,
    big_h1: bool,
    striping: bool,
    diagrams: bool,
}

const VARIANTS: [Variant; 4] = [
    Variant {
        reflow: false,
        big_h1: false,
        striping: false,
        diagrams: true,
    },
    Variant {
        reflow: true,
        big_h1: false,
        striping: false,
        diagrams: true,
    },
    Variant {
        reflow: false,
        big_h1: true,
        striping: true,
        diagrams: false,
    },
    Variant {
        reflow: true,
        big_h1: true,
        striping: true,
        diagrams: false,
    },
];

fn build(src: &str, reflow: bool) -> ParsedDoc {
    build_variant(src, VARIANTS[usize::from(reflow)])
}

fn build_variant(src: &str, v: Variant) -> ParsedDoc {
    ParsedDoc::build_with_overrides(
        src,
        theme(),
        true,
        24,
        None,
        None,
        v.striping,
        80,
        v.big_h1,
        false,
        v.diagrams,
        v.reflow,
        None,
    )
}

fn row_chars(line: &Line<'_>) -> Vec<char> {
    line.spans.iter().flat_map(|s| s.content.chars()).collect()
}

/// The row's chars from cell `cell` on.
fn chars_from_cell(chars: &[char], cell: usize) -> Option<&[char]> {
    let mut at = 0usize;
    for (i, &ch) in chars.iter().enumerate() {
        if at == cell {
            return Some(&chars[i..]);
        }
        if at > cell {
            return None;
        }
        at += char_cells(ch);
    }
    (at == cell).then_some(&chars[chars.len()..])
}

/// The leaf `line` (block-relative) belongs to inside `block`.
fn leaf_at(block: &Block, line: u32) -> Option<&Block> {
    let children: Vec<&Block> = match block {
        Block::BlockQuote { blocks, .. } | Block::FootnoteDefinition { blocks, .. } => {
            blocks.iter().collect()
        }
        Block::List { items, .. } => items.iter().flat_map(|i: &ListItem| &i.blocks).collect(),
        leaf => return leaf.span().contains(&line).then_some(leaf),
    };
    children.into_iter().find_map(|c| leaf_at(c, line))
}

/// The span of the footnote definition holding `line` inside `block`, if any.
fn footnote_span_at(block: &Block, line: u32) -> Option<std::ops::Range<u32>> {
    if !block.span().contains(&line) {
        return None;
    }
    match block {
        Block::FootnoteDefinition { span, .. } => Some(span.clone()),
        Block::BlockQuote { blocks, .. } => blocks.iter().find_map(|b| footnote_span_at(b, line)),
        Block::List { items, .. } => items
            .iter()
            .flat_map(|i: &ListItem| &i.blocks)
            .find_map(|b| footnote_span_at(b, line)),
        _ => None,
    }
}

fn src_lines_at(block: &Block, line: u32) -> Option<&SrcLines> {
    leaf_at(block, line)?.src()
}

/// One check over one parse; the first violation, described.
fn check_doc(doc: &ParsedDoc) -> Result<(), String> {
    let origins = doc.row_origins();
    if origins.len() != doc.lines.len() {
        return Err(format!(
            "{} origins for {} rows",
            origins.len(),
            doc.lines.len()
        ));
    }
    let source = doc.source();
    let lines: Vec<&str> = source.split('\n').collect();
    let line_of = |byte: usize| {
        source.as_bytes()[..byte.min(source.len())]
            .iter()
            .filter(|&&b| b == b'\n')
            .count()
    };

    let block_range = |row: usize| {
        let byte = doc.source_map.original_byte_for_rendered_line(row)?;
        doc.source_map.original_range_for_byte(byte)
    };
    // The block and first line of the last row that showed a line: a block's rows ascend.
    let mut prev_first: Option<(usize, u32)> = None;

    for (row, (line, origin)) in doc.lines.iter().zip(origins).enumerate() {
        let err = |msg: String| {
            Err(format!(
                "row {row} {origin:?} {:?}: {msg}",
                row_chars(line).iter().collect::<String>()
            ))
        };
        let Some(range) = block_range(row) else {
            return err("no block range".into());
        };
        let base = line_of(range.start);
        let real = doc.real_block_for_byte(range.start);
        // A block's own lines; a virtual blank block has the one.
        let n = real.map_or(1, |b| b.span().end);

        let Some(span) = &origin.lines else {
            if !matches!(origin.cols, ColOrigin::Chrome) {
                return err("content row with no lines".into());
            }
            continue;
        };
        if span.start >= span.end || span.end > n {
            return err(format!("lines outside the block's {n}"));
        }
        if prev_first.is_some_and(|(block, first)| block == range.start && span.start < first) {
            return err("a line above the previous row's".into());
        }
        prev_first = Some((range.start, span.start));
        let ColOrigin::Content {
            raw_col,
            rendered_col,
            kind,
        } = origin.cols
        else {
            continue;
        };
        let chars = row_chars(line);
        let Some(content) = chars_from_cell(&chars, rendered_col as usize) else {
            return err("rendered_col past the row".into());
        };
        let src_line = lines.get(base + span.start as usize).copied().unwrap_or("");
        let src_chars: Vec<char> = src_line.chars().collect();
        if raw_col as usize > src_chars.len() {
            return err("raw_col past its line".into());
        }
        let src_content: String = src_chars[raw_col as usize..].iter().collect();
        // A footnote definition's last row ends in its `↩` back-link: trailing chrome.
        let mut content: String = content.iter().collect();
        let footnote_end = real
            .and_then(|b| footnote_span_at(b, span.start))
            .map(|s| s.end);
        let next_first = origins.get(row + 1).and_then(RowOrigin::first_line);
        let last_row_of_footnote = footnote_end.is_some_and(|end| {
            span.end == end
                && (block_range(row + 1).is_none_or(|next| next.start != range.start)
                    || next_first.is_none_or(|l| l >= end))
        });
        if last_row_of_footnote {
            if let Some(stripped) = content.strip_suffix(" ↩") {
                content = stripped.to_owned();
            }
        }
        match kind {
            ContentKind::Inline | ContentKind::Flow => {
                // Each source line past its content column.  Mapped line by line: joined, a
                // continuation reading `===` or `2. a` would parse as block syntax it wasn't in
                // the document.  A break between two lines renders as one space.
                let mut slices: Vec<String> = Vec::new();
                let mut atomic_tail = false;
                for l in span.clone() {
                    let col = if l == span.start {
                        Some(raw_col)
                    } else {
                        real.and_then(|b| src_lines_at(b, l))
                            .and_then(|s| s.col((l - s.first) as usize))
                    };
                    let text = lines.get(base + l as usize).copied().unwrap_or("");
                    // A line with no column continues an atomic inline begun above (a code
                    // span, math, inline HTML, an image's alt).  How much of its indent the
                    // inline keeps is pulldown-cmark's call, so skip only the container prefix
                    // and check such a row by its letters and digits alone (below).
                    let col = col.unwrap_or_else(|| {
                        atomic_tail = true;
                        text.chars()
                            .take_while(|c| matches!(c, '>' | ' ' | '\t'))
                            .count() as u32
                    });
                    let slice: String = text.chars().skip(col as usize).collect();
                    // A trailing `\` is a hard break only across a line end inside its leaf; on
                    // the leaf's last line it is literal.
                    let leaf_continues = real
                        .and_then(|b| src_lines_at(b, l))
                        .is_some_and(|s| s.span().end > l + 1);
                    let slice = match slice.strip_suffix('\\') {
                        Some(stripped) if leaf_continues => stripped.to_owned(),
                        _ => slice,
                    };
                    // An ATX heading's closing sequence renders nothing.
                    let atx = real.and_then(|b| leaf_at(b, l)).is_some_and(|b| {
                        matches!(b, Block::Heading { .. }) && !b.is_setext_heading()
                    });
                    let slice = if atx {
                        strip_atx_closing(&slice).to_owned()
                    } else {
                        slice
                    };
                    slices.push(slice);
                }
                // `InlineColMap` doesn't render an image's `[Image: …]` placeholder.
                if content.contains("[Image:") {
                    continue;
                }
                let labels = doc.ref_labels();
                let shown: Vec<char> = content.chars().collect();
                if atomic_tail {
                    if let Err(e) = check_alnum_sequence(&slices.join("\n"), &shown, labels) {
                        return err(e);
                    }
                    continue;
                }
                // Joined, an inline spanning a break (`*a⏎b*`) maps as the document parses it;
                // line by line, a continuation that joining would turn into block syntax does.
                // A flow must agree with the row one way or the other.
                // A row a break cuts out of a paragraph (`a *b⏎c* d`, reflow off) maps only as
                // part of the whole paragraph's text.
                if let Err(line_by_line) = check_slices(&slices, &shown, labels) {
                    let joined = slices.len() > 1
                        && check_slices(&[slices.join("\n")], &shown, labels).is_ok();
                    let part = match real.and_then(|b| leaf_at(b, span.start)) {
                        Some(Block::Paragraph { src, .. }) => {
                            let para: Vec<(u32, String)> = src
                                .span()
                                .map(|l| {
                                    let text = lines.get(base + l as usize).copied().unwrap_or("");
                                    // No column: an atomic inline's tail, past the prefix.
                                    let col =
                                        src.col((l - src.first) as usize).unwrap_or_else(|| {
                                            text.chars()
                                                .take_while(|c| matches!(c, '>' | ' ' | '\t'))
                                                .count()
                                                as u32
                                        });
                                    let slice: String = text.chars().skip(col as usize).collect();
                                    // Only an unescaped `\` breaks: an odd run.
                                    let run = slice.chars().rev().take_while(|&c| c == '\\');
                                    let slice = match slice.strip_suffix('\\') {
                                        Some(s)
                                            if l + 1 < src.span().end && run.count() % 2 == 1 =>
                                        {
                                            s.to_owned()
                                        }
                                        _ => slice,
                                    };
                                    (l, slice)
                                })
                                .collect();
                            check_paragraph_part(&para, span.clone(), &shown, labels).is_ok()
                        }
                        _ => false,
                    };
                    if !joined && !part {
                        return err(line_by_line);
                    }
                }
            }
            ContentKind::Verbatim => {
                let pad = |c: char| c == ' ' || c == '\u{00A0}';
                if content.trim_end_matches(pad) != src_content.trim_end_matches(pad) {
                    return err(format!("verbatim row is not its source {src_content:?}"));
                }
            }
            ContentKind::TableRow { row: t_row, sub } => {
                let Some(Block::Table { headers, rows, .. }) =
                    real.and_then(|b| leaf_at(b, span.start))
                else {
                    return err("table row outside a table".into());
                };
                let cells = if t_row == 0 {
                    headers
                } else if let Some(r) = rows.get(t_row as usize - 1) {
                    r
                } else {
                    return err("no such table row".into());
                };
                let pipes = rendered_pipe_cells(line);
                // The row's chunk count: its tallest cell's.
                let mut row_chunks = 1;
                for (c, cell) in cells.iter().enumerate() {
                    let (Some(&l), Some(&r)) = (pipes.get(c), pipes.get(c + 1)) else {
                        return err("missing pipes".into());
                    };
                    let width = r - l - 3;
                    let chunks = wrap_cell(&inlines_to_plain(cell), width);
                    row_chunks = row_chunks.max(chunks.len());
                    let mut at = l + 1;
                    let shown: String = chars_from_cell(&chars, l + 1)
                        .unwrap_or(&[])
                        .iter()
                        .take_while(|&&ch| {
                            at += char_cells(ch);
                            at <= r
                        })
                        .collect();
                    let plainly = cell.iter().all(|i| {
                        matches!(
                            i,
                            Inline::Text(_) | Inline::Bold(_) | Inline::Italic(_) | Inline::Code(_)
                        )
                    });
                    if !plainly || shown.contains('…') {
                        continue;
                    }
                    let Some(chunk) = chunks.get(sub as usize) else {
                        // A shorter neighbor in a taller row pads with blank chunks.
                        if shown.trim().is_empty() {
                            continue;
                        }
                        return err(format!("chunk {sub} of a {}-chunk cell", chunks.len()));
                    };
                    if shown.trim() != chunk.trim() {
                        return err(format!("cell {c} shows {shown:?}, its chunk is {chunk:?}"));
                    }
                }
                if sub as usize >= row_chunks {
                    return err(format!("chunk {sub} of a {row_chunks}-chunk row"));
                }
            }
        }
    }

    // Every line of a block shows on some row, but for the kinds known to render none, which the
    // cursor's reveal stacks with the row they share (`row_map::cursor_stack`).  A new row-less
    // kind fails here rather than reveal over its neighbor.
    let mut covered: std::collections::HashMap<usize, Vec<std::ops::Range<u32>>> =
        std::collections::HashMap::new();
    for (row, origin) in origins.iter().enumerate() {
        if let (Some(range), Some(lines)) = (block_range(row), &origin.lines) {
            covered.entry(range.start).or_default().push(lines.clone());
        }
    }
    for (&start, rows) in &covered {
        let Some(real) = doc.real_block_for_byte(start) else {
            continue;
        };
        for line in 0..real.span().end {
            if !rows.iter().any(|r| r.contains(&line)) && !renders_no_row(real, line, true) {
                return Err(format!(
                    "line {line} of the block at line {} shows on no row",
                    line_of(start)
                ));
            }
        }
    }
    Ok(())
}

/// Whether block-relative `line` of `block` is one of the kinds known to render no row: a link
/// reference definition inside a container (its `hidden` lines), a bare list marker (`-` with the
/// item's text on the lines below), or the underline of a setext heading nested in a container,
/// which draws no rule.  `top` is whether `block` is a top-level block.
fn renders_no_row(block: &Block, line: u32, top: bool) -> bool {
    if !block.span().contains(&line) {
        return false;
    }
    match block {
        Block::BlockQuote { blocks, hidden, .. }
        | Block::FootnoteDefinition { blocks, hidden, .. } => {
            hidden.contains(&line) || blocks.iter().any(|b| renders_no_row(b, line, false))
        }
        Block::List { items, .. } => items.iter().any(|i: &ListItem| {
            let bare_marker =
                line == i.span.start && i.blocks.first().is_some_and(|b| b.span().start > line);
            bare_marker
                || i.hidden.contains(&line)
                || i.blocks.iter().any(|b| renders_no_row(b, line, false))
        }),
        // A comment inside a container keeps its block (and line) but renders nothing.
        Block::HtmlComment(..) => !top,
        leaf => !top && leaf.is_setext_heading() && line + 1 == leaf.span().end,
    }
}

/// `slices`, each mapped by its own `InlineColMap` and joined by one space, render exactly the
/// row's `shown` content, every letter or digit onto the same letter or digit.  Maps are built
/// as paragraph text (a slice reading `2. a` or `# b` would otherwise parse as a block
/// construct), resolving the document's references.
fn check_slices(slices: &[String], shown: &[char], labels: &RefLabels) -> Result<(), String> {
    let mut offset = 0usize;
    for (i, text) in slices.iter().enumerate() {
        if i > 0 {
            offset += 1;
        }
        let map = InlineColMap::build_inline(text, labels);
        let len = map.rendered_len();
        let raw: Vec<char> = text.chars().collect();
        for k in 0..len {
            let Some(&ch) = shown.get(offset + k) else {
                return Err(format!("the row is shorter than {slices:?} renders"));
            };
            let at = map.rendered_to_raw_vec()[k];
            // Letters and digits only: punctuation is the renderer's to substitute (smart
            // quotes, an image's `[Image: …]` placeholder), and the length check covers it.
            if ch.is_alphanumeric() && raw.get(at) != Some(&ch) {
                return Err(format!("rendered {ch:?} maps to raw {:?}", raw.get(at)));
            }
        }
        offset += len;
    }
    if offset != shown.len() {
        return Err(format!(
            "{slices:?} render {offset} chars, the row shows {}",
            shown.len()
        ));
    }
    Ok(())
}

/// The paragraph whose `(line, slice)` pairs are `para`, mapped as one text joined by `\n`,
/// renders on `lines` exactly the row's `shown` content: the rendered chars whose raw char lies
/// in those lines' slices, short of the whitespace a break leaves at a slice's end, every letter
/// or digit onto the same letter or digit.
fn check_paragraph_part(
    para: &[(u32, String)],
    lines: std::ops::Range<u32>,
    shown: &[char],
    labels: &RefLabels,
) -> Result<(), String> {
    let text = para
        .iter()
        .map(|(_, s)| s.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let map = InlineColMap::build_inline(&text, labels);
    let raw: Vec<char> = text.chars().collect();
    // Each of the row's lines as a raw char range, trailing whitespace off.
    let mut ranges = Vec::new();
    let mut at = 0usize;
    for (l, slice) in para {
        let len = slice.trim_end().chars().count();
        if lines.contains(l) {
            ranges.push(at..at + len);
        }
        at += slice.chars().count() + 1;
    }
    let part: Vec<usize> = map.rendered_to_raw_vec()[..map.rendered_len()]
        .iter()
        .copied()
        .filter(|c| ranges.iter().any(|r| r.contains(c)))
        .collect();
    if part.len() != shown.len() {
        return Err(format!(
            "{text:?} renders {} chars on {lines:?}, the row shows {}",
            part.len(),
            shown.len()
        ));
    }
    for (&at, &ch) in part.iter().zip(shown) {
        if ch.is_alphanumeric() && raw.get(at) != Some(&ch) {
            return Err(format!("rendered {ch:?} maps to raw {:?}", raw.get(at)));
        }
    }
    Ok(())
}

/// `text`, mapped by one `InlineColMap`, renders the same letters and digits in the same order
/// as the row's `shown` content, wherever its whitespace falls.
fn check_alnum_sequence(text: &str, shown: &[char], labels: &RefLabels) -> Result<(), String> {
    let map = InlineColMap::build_inline(text, labels);
    let raw: Vec<char> = text.chars().collect();
    let rendered: String = map
        .rendered_to_raw_vec()
        .iter()
        .take(map.rendered_len())
        .filter_map(|&at| raw.get(at))
        .filter(|c| c.is_alphanumeric())
        .collect();
    let shown: String = shown.iter().filter(|c| c.is_alphanumeric()).collect();
    if rendered != shown {
        return Err(format!(
            "{text:?} renders {rendered:?}, the row shows {shown:?}"
        ));
    }
    Ok(())
}

fn check(src: &str) -> Result<(), String> {
    for v in VARIANTS {
        check_doc(&build_variant(src, v)).map_err(|e| format!("{v:?}: {e}\nin {src:?}"))?;
    }
    Ok(())
}

/// The sources of the behavioral tests the row-provenance work was written against, plus the
/// two sample fixtures and the smoke-test one.
const CORPUS: &[&str] = &[
    "a | b\n--|--\n1 | 2\n",
    "| a | b | c |\n|---|---|---|\n1 | 2 |\n| 1 | 2 | 3 | 4 |\n",
    "> x\n>\n> a | b\n> --|--\n> 1 | 2 |\n",
    "| a | b |\n|---|---|\n| 1 |\n| 1\n| 1 | 2 | 3 | 4 | 5 | 6 |\n",
    "x[^a|b]\n\n[^a|b]: | p | q |\n    |---|---|\n    | 1 | 2 |\n",
    "8. Tag it.\n\n    ```bash\n    gh run watch\n\n      indented\n    ```\n",
    "8. Tag it.\n\n    ```bash\n    git tag\n    gh run watch\n    ```\n",
    "- ```bash\n  code\n  ```\n- next item\n\n- third\n",
    "- ```bash\n  gh run watch\n  ```\n- next item\n",
    "- - a\n  - b\n- c\n",
    "1. a\n   - ```\n     x\n     ```\n2. b\n",
    "- a\n  ```\n  x\n- b\n",
    "-\n  text\n- b\n",
    "- a\n  soft\n- b\n",
    "> - a\n>\n> - b\n>\n> tail\n",
    "- a\n  - b\n    soft word\n",
    "- a\n  - b\n    soft *word*\n    - c\n      deep word\n\nafter\n",
    "- a\n- > q\n\nafter\n",
    "- a\n- - b\n\nafter\n",
    "- a\n- - q\n\nafter\n",
    "- a\n  soft\n- b\\\n  hard\n1. c\nlazy\n",
    "> q\n> ```\n> x\n> ```\n",
    "> a\n>\n>\n> b\n",
    ">\n",
    ">\n> a\n>\n",
    "- item\n\n  | a | b |\n  |---|---|\n  | 1 | 2 |\n",
    "| a | b |\n|---|---|\n| 1 | 2 |\n",
    "> | a | b |\n> |---|---|\n> | 1 | 2 |\n",
    "Title\n=====\n\nSub\n---\n\npara\n",
    "[^1]: a note\n    more\n\nref[^1]\n",
    // A break nested in emphasis or a link: cut there, one row per line with reflow off.
    "*a\nb* c\nd\n",
    "- [x\n  y](u) *p\n  q **r\n  s***\n  d\n",
    "> **a\n> b** c\n> d\n",
    "[^注]: one\n    two\n",
    // A line opening with a link's close, a quote's link definitions, multi-line setext text.
    "[a\n](u)\nc\n",
    "> x [a\n> ](u) t\n> d\n",
    "> [d]: /url\n> b\n>\n> [e]: /v\n",
    "Title\nmore\n=====\n\nSub\nmore\n---\n",
    // A task box opening a setext heading is the heading's text.
    "- [ ] a\n  ---\n- [x] b\n",
    // References resolve against the document's definitions (case-insensitively); an undefined
    // one stays literal.
    "see [it][r] now, [R] and [R][] [no]\n\n[r]: /u\n",
    "a [^x] b[^n]\n\n[^N]: note\n",
    // A closing sequence renders nothing.
    "## Title ##\n\n# T #\n\n> ## Q ##\n\n- # H #\n",
    // Smart punctuation, entities, escapes, an autolink.
    "a -- b... c---d\n\na &amp; b &copy; \\* <http://x.y>\n",
    // Smart punctuation beside a literal `==` and a highlight pair.
    "x == y... and a ==hi== b...\n",
    // Fences whose rows are all chrome, nested where char 0 is a container prefix.
    "- a\n\n  ```rust\n  x\n  ```\n\n> - ```\n>   y\n>   ```\n",
    // Nested paragraphs that reflow (Phase 7): a quote's, an item's first and later ones, a
    // list in a quote, a task's, lazy continuations, and markup spanning a soft break.
    "> first para\n>\n> alpha bravo\n> charlie *delta\n> echo* foxtrot\n>\n> last\n",
    "- one\n- alpha bravo charlie\n  delta echo foxtrot golf\n- three\n",
    "1. a\n2. b\n   bravo\n\n   second para\n   more words\n",
    "> - a\n>   b soft\n> - c\n",
    "- [ ] task text\n  more text\n- [x] done\n",
    "> alpha\nlazy line\n\nafter\n",
    "- alpha\nlazy line\n",
    "[^n]: note one\n    two three\n\nref[^n]\n",
    // Frontmatter is the byte-0 block only: a `---` opening a quote below it is a rule.
    "---\nt: x\n---\n\n> ---\n> b\n\n---\n",
    // Lines with no row of their own reveal stacked with the row they share, and a multi-line
    // setext heading stacks its text with the cursor on any of its lines.
    "Multi\nline\n---\n",
    "> a\n>\n> [d]: /u\n> b\n",
    "> [d]: /u\n> - - a\n",
    "- a\n  b\n  ---\n",
    "- a\n\n  ```\n  x\n  ```\n",
    // Rows hang where the renderer says their text starts (issue #71): a footnote flow under its
    // leader, a named one too, and text that only reads like a marker not at all.
    "[^1]: alpha bravo charlie delta echo foxtrot golf hotel\n\nref[^1]\n",
    "[^note]: alpha bravo charlie delta echo foxtrot golf\n\nref[^note]\n",
    "1\\. alpha bravo charlie delta echo\n\n\\- alpha bravo charlie\n\n> 2\\. alpha bravo charlie\n",
    // A revealed line keeps its rendered marker alignment (issue #65): ` 6.` padded beside `10.`,
    // wrapping, and a 2-space nested item at its 4-cell nesting.
    "1. a\n2. b\n3. c\n4. d\n5. e\n6. alpha bravo charlie delta echo\n7. g\n8. h\n9. i\n10. j\n",
    "- a\n  - alpha bravo charlie\n    delta echo foxtrot\n- b\n",
];

#[test]
fn origins_agree_with_the_rendered_rows_on_the_corpus() {
    for src in CORPUS {
        check(src).unwrap();
    }
    for fixture in ["general.md", "syntax.md", "row_provenance.md"] {
        let src = std::fs::read_to_string(format!(
            "{}/tests/fixtures/{fixture}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        check(&src).unwrap();
    }
}

/// The rows `build` adds itself carry origins too: a blank line's row, the phantom last line.
#[test]
fn blank_rows_are_their_virtual_blocks_only_line() {
    let doc = build("a\n\n\nb\n", false);
    let blanks: Vec<&RowOrigin> = doc
        .row_origins()
        .iter()
        .filter(|o| matches!(o.cols, ColOrigin::Chrome))
        .collect();
    assert_eq!(
        blanks.len(),
        3,
        "two blank lines and the phantom: {:?}",
        doc.row_origins()
    );
    assert!(blanks.iter().all(|o| o.lines == Some(0..1)));
}

/// A reflowed top-level paragraph is one row over all its lines.
#[test]
fn a_reflowed_paragraph_is_one_flow_row() {
    let doc = build("one\ntwo\nthree\n", true);
    assert_eq!(
        doc.row_origins()[0],
        RowOrigin::content(0..3, 0, 0, ContentKind::Flow)
    );
}

/// An image's alt text spanning lines shows on the image's row, and the rows below keep their own
/// lines.  (The agreement check can't compare an image's `[Image: …]` placeholder.)
#[test]
fn a_multi_line_image_keeps_the_rows_below_on_their_lines() {
    for (src, raw_col) in [("x ![a\nb](x)\nc\n", 0), ("- ![a\n  b](x)\n  c\n", 2)] {
        let doc = build(src, false);
        let lines: Vec<_> = doc.row_origins()[..2]
            .iter()
            .map(|o| o.lines.clone())
            .collect();
        assert_eq!(lines, [Some(0..2), Some(2..3)], "{src:?}");
        assert_eq!(
            doc.row_origins()[1],
            RowOrigin::content(2..3, raw_col, raw_col, ContentKind::Inline),
            "{src:?}"
        );
    }
}

/// The setext H2 rule is the renderer's now: its row shows the underline.
#[test]
fn a_setext_h2_rule_shows_its_underline() {
    let doc = build("Title\n-----\n", false);
    assert_eq!(doc.row_origins()[1], RowOrigin::chrome(Some(1)));
}

/// The column round trip (Phase 4) over every rendered char of every row of one parse: a click
/// on char `r` puts the cursor at `pos`, and the cursor indicator for `pos` lands on a char a
/// click on which puts the cursor at `pos` again, so clicking where the cursor shows never moves
/// it.  Where the row places `pos` exactly and `r` is content, that char is `r` itself, unless
/// `r` shows the same position as the char before it (the fill past a code line, a back-link):
/// every content char round-trips to the cell it was clicked on.  And every inline row *has* an
/// exact map, or its overlays would paint nothing: a row without one is a construct the per-line
/// maps misread.  Known exceptions: an image's `[Image: …]` placeholder; display math nested in
/// a container, which isn't promoted and shows its formula on one row, newlines and all; and a
/// row continuing a multi-line atomic inline (a code span), which keeps as much of the later
/// line's indent as pulldown-cmark decides (the agreement test checks such a row by its letters
/// and digits).  Table rows map their columns through `table_layout`'s cell geometry, not
/// `row_map`'s, and are skipped here; [`check_click_and_paint`] covers them.
fn check_round_trip(doc: &ParsedDoc) -> Result<(), String> {
    for (abs, (line, origin)) in doc.lines.iter().zip(doc.row_origins()).enumerate() {
        if matches!(
            origin.cols,
            ColOrigin::Content {
                kind: ContentKind::TableRow { .. },
                ..
            }
        ) {
            continue;
        }
        let Some(block) = doc
            .source_map
            .original_byte_for_rendered_line(abs)
            .and_then(|b| doc.source_map.block_for_byte(b))
        else {
            return Err(format!("row {abs} has no block"));
        };
        let row = abs - doc.source_map.rendered_lines_for_block(block).start;
        let chars = row_chars(line);
        let n = chars.len();
        // Where the content starts; a prefix char (a marker, a bar, a pad cell) may stand for a
        // raw char it can't be the only one showing.
        let start = match origin.cols {
            ColOrigin::Content { rendered_col, .. } => {
                n - chars_from_cell(&chars, rendered_col as usize).map_or(0, <[char]>::len)
            }
            ColOrigin::Chrome => n,
        };
        let click = |r: usize| row_map::rendered_to_raw_col(doc, block, row, r);
        if let ColOrigin::Content {
            kind: ContentKind::Inline | ContentKind::Flow,
            ..
        } = origin.cols
        {
            let shown: String = chars[start..].iter().collect();
            let real = doc
                .source_map
                .original_range_for_block(block)
                .and_then(|r| doc.real_block_for_byte(r.start));
            let atomic_tail = origin.lines.clone().is_some_and(|ls| {
                (ls.start + 1..ls.end).any(|l| {
                    real.and_then(|b| src_lines_at(b, l))
                        .is_some_and(|s| s.col((l - s.first) as usize).is_none())
                })
            });
            if !atomic_tail
                && !shown.contains("[Image:")
                && !shown.contains("$$")
                && row_map::raw_to_rendered_col(doc, block, row, click(start)).is_none()
            {
                return Err(format!("row {abs} {origin:?} {shown:?} has no exact map"));
            }
        }
        for r in 0..=n {
            let pos = click(r);
            let near = row_map::raw_to_rendered_col_near(doc, block, row, pos);
            let err = |msg: String| {
                Err(format!(
                    "row {abs} {origin:?} {:?}, char {r} → {pos:?}: {msg}",
                    chars.iter().collect::<String>()
                ))
            };
            if click(near) != pos {
                return err(format!(
                    "the indicator sits on char {near}, which clicks to {:?}",
                    click(near)
                ));
            }
            let Some(exact) = row_map::raw_to_rendered_col(doc, block, row, pos) else {
                continue;
            };
            if exact != near {
                return err(format!("exactly {exact}, near {near}"));
            }
            if exact != r && (start..n).contains(&r) && (r == start || click(r - 1) != pos) {
                return err(format!("maps back to char {exact}"));
            }
        }
    }
    Ok(())
}

fn round_trip(src: &str) -> Result<(), String> {
    for v in VARIANTS {
        check_round_trip(&build_variant(src, v)).map_err(|e| format!("{v:?}: {e}\nin {src:?}"))?;
    }
    Ok(())
}

#[test]
fn columns_round_trip_on_the_corpus() {
    for src in CORPUS {
        round_trip(src).unwrap();
    }
    for fixture in ["general.md", "syntax.md", "row_provenance.md"] {
        let src = std::fs::read_to_string(format!(
            "{}/tests/fixtures/{fixture}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        round_trip(&src).unwrap();
    }
}

/// The round trip through the real paths: `mouse_ops`' click (wrap, then `row_map`) and
/// `RenderedView`'s cursor indicator (`row_map`, then wrap), on a terminal `width` cells wide.
/// For every cell of every painted row and one past its text, a click puts the cursor somewhere,
/// and a click on the cell where the indicator then shows leaves it there.  The cursor's row
/// stays rendered (the reveal delay is kept running), which is the case `row_map` maps; a click
/// that edits the document (a task box) or follows a footnote is skipped.
fn check_click_and_paint(src: &str, width: u16) -> Result<(), String> {
    check_click_and_paint_from(src, width, None)
}

/// [`check_click_and_paint`], with the cursor starting at byte `revealed_at` (when given) and
/// its row revealed throughout: every paint and click sees the reveal on, so a reflowed
/// paragraph under the cursor shows its source lines stacked.  A click on a stacked raw cell
/// showing a char must also put the cursor on that very char.
fn check_click_and_paint_from(
    src: &str,
    width: u16,
    revealed_at: Option<usize>,
) -> Result<(), String> {
    const HEIGHT: u16 = 40;
    let cursor_style = Style::default()
        .fg(Color::Rgb(1, 2, 3))
        .bg(Color::Rgb(4, 5, 6));
    let reveal = |st: &mut EditorState| {
        // A click starts a drag that its release would end; a drag holds the reveal off.
        st.drag_in_progress = false;
        // Held off by a delay that starts in an hour, not now: the real 120 ms can run out
        // mid-check on a loaded machine (the cases run on threads), revealing the row.
        st.cursor_block_entered_at = if revealed_at.is_some() {
            None
        } else {
            Some(Instant::now() + Duration::from_secs(3600))
        };
    };
    let fresh = || {
        let mut st = EditorState::new(Buffer::from_str(src), theme());
        st.mode = Mode::Rendered;
        st.set_viewport_width(usize::from(width));
        if let Some(byte) = revealed_at {
            st.cursor.offset = st.buffer.rope().byte_to_char(byte);
            st.update_cursor_block();
        }
        st
    };
    let paint = |st: &mut EditorState| {
        reveal(st);
        let mut terminal = Terminal::new(TestBackend::new(width, HEIGHT)).unwrap();
        let mut view_state = RenderedViewState::default();
        terminal
            .draw(|frame| {
                let view = RenderedView {
                    cursor_style,
                    visual_kind: None,
                    drop_indicator: None,
                    show_table_buttons: false,
                    state: st,
                    theme: theme(),
                };
                frame.render_stateful_widget(view, frame.area(), &mut view_state);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    };
    let click = |st: &mut EditorState, col: u16, row: u16| {
        reveal(st);
        let action = MouseAction::Click {
            col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        mouse_ops::apply(
            st,
            action,
            &mut None,
            &[],
            usize::from(HEIGHT),
            usize::from(width),
        );
        st.cursor.offset
    };

    let mut start = fresh();
    let base = paint(&mut start);
    // The visual rows the start's reveal paints as stacked raw source.
    let effective = start.effective_rows(usize::from(width));
    let stacked = |y: u16| {
        effective.has_reveal()
            && matches!(
                effective.line_at_visual_row(usize::from(y)),
                edamame::editor::effective_rows::RowHit::Raw { .. }
            )
    };
    for y in 0..HEIGHT {
        let symbols: Vec<String> = (0..width)
            .map(|x| {
                base.cell((x, y))
                    .map_or(String::new(), |c| c.symbol().to_owned())
            })
            .collect();
        let Some(last) = symbols.iter().rposition(|s| !s.trim().is_empty()) else {
            continue;
        };
        for x in 0..=(last as u16 + 1).min(width - 1) {
            let mut st = fresh();
            let at = click(&mut st, x, y);
            if st.buffer.contents() != src || st.pending_link_follow.is_some() {
                continue;
            }
            let symbol = symbols[usize::from(x)].as_str();
            if stacked(y) && !symbol.trim().is_empty() {
                let shown = st.buffer.contents().chars().nth(at).map(String::from);
                if shown.as_deref() != Some(symbol) {
                    return Err(format!(
                        "a click on stacked ({x}, {y}), showing {symbol:?}, put the cursor at \
                         {at}, on {shown:?}"
                    ));
                }
            }
            let painted = paint(&mut st);
            let indicator = (0..HEIGHT)
                .flat_map(|cy| (0..width).map(move |cx| (cx, cy)))
                .find(|&cell| {
                    painted.cell(cell).is_some_and(|c| {
                        c.fg == cursor_style.fg.unwrap() && c.bg == cursor_style.bg.unwrap()
                    })
                });
            let Some((cx, cy)) = indicator else {
                return Err(format!(
                    "a click on ({x}, {y}) put the cursor at {at}, shown nowhere"
                ));
            };
            let again = click(&mut st, cx, cy);
            if again != at {
                return Err(format!(
                    "a click on ({x}, {y}) put the cursor at {at}; it shows on ({cx}, {cy}), \
                     which clicks to {again}"
                ));
            }
        }
    }
    Ok(())
}

/// [`check_click_and_paint`] over the corpus, wide and wrapping.
#[test]
fn clicking_where_the_cursor_shows_keeps_it_there() {
    // A paint and two clicks per cell: the cases are independent, so they run on threads.
    std::thread::scope(|scope| {
        for src in CORPUS {
            for width in [40, 12] {
                scope.spawn(move || {
                    check_click_and_paint(src, width)
                        .unwrap_or_else(|e| panic!("width {width}: {e}\nin {src:?}"));
                });
            }
        }
    });
}

/// Corpus entries [`clicking_around_a_revealed_row_keeps_the_cursor_where_it_shows`] skips at
/// 12 cells, each a known bug outside row provenance.  A table too wide to shrink into 12 cells
/// (quoted, in a list item, behind a footnote leader, or three columns) wraps its rows in
/// `line_render`; with a cell revealed, the cursor past a cell's text paints on the wrapped row,
/// and a click there lands one char left, or nowhere (issue #69).  Their fully piped
/// equivalents, and the footnote table under a label without a `|`, miss identically, so pairing
/// cells plays no part.  A revealed line exactly as wide as the viewport (`[^1]: a note`) has no blank cell for a cursor at its end,
/// so the painter draws that cursor over the last char, and a click there lands on that char
/// (`paint_row`; accepted, since an extra row for it would change every row count).
const KNOWN_REVEALED_MISSES: &[&str] = &[
    "> | a | b |\n> |---|---|\n> | 1 | 2 |\n",
    "- item\n\n  | a | b |\n  |---|---|\n  | 1 | 2 |\n",
    "| a | b | c |\n|---|---|---|\n1 | 2 |\n| 1 | 2 | 3 | 4 |\n",
    "> x\n>\n> a | b\n> --|--\n> 1 | 2 |\n",
    "x[^a|b]\n\n[^a|b]: | p | q |\n    |---|---|\n    | 1 | 2 |\n",
    "[^1]: a note\n    more\n\nref[^1]\n",
];

/// [`check_click_and_paint_from`] over the corpus with the cursor's row revealed: the cursor on
/// each non-blank line in turn, mid-line, so every reflowed paragraph stacks with the cursor on
/// each of its lines and every other row reveals in place.
#[test]
fn clicking_around_a_revealed_row_keeps_the_cursor_where_it_shows() {
    // Each document is independent and costs a paint and two clicks per cell per cursor line,
    // so the corpus is spread over threads.
    std::thread::scope(|scope| {
        for src in CORPUS {
            scope.spawn(|| check_revealed_on_every_line(src));
        }
    });
}

/// [`check_click_and_paint_from`] on `src`, the cursor mid-line on each non-blank line in turn.
fn check_revealed_on_every_line(src: &str) {
    let mut line_start = 0;
    for line in src.split_inclusive('\n') {
        let content = line.trim_end();
        if !content.trim().is_empty() {
            let mid = content
                .char_indices()
                .nth(content.chars().count() / 2)
                .unwrap()
                .0;
            let at = line_start + mid;
            for width in [40, 12] {
                if width == 12 && KNOWN_REVEALED_MISSES.contains(&src) {
                    continue;
                }
                check_click_and_paint_from(src, width, Some(at))
                    .unwrap_or_else(|e| panic!("width {width}, cursor at {at}: {e}\nin {src:?}"));
            }
        }
        line_start += line.len();
    }
}

/// Nested reflowed paragraphs, revealed (Phase 7): the cursor rests in one, which shows its
/// source lines stacked, whole, container prefixes and all.  Every click, wide and wrapping, on
/// the stack and around it, lands where the indicator then shows, and on the stack exactly on
/// the char clicked: across a soft break, past a wrap, through markup spanning a break, and at
/// each depth.
#[test]
fn clicking_a_revealed_nested_flow_keeps_the_cursor_where_it_shows() {
    let cases: &[(&str, &[&str])] = &[
        (
            "> first para\n>\n> alpha bravo\n> charlie *delta\n> echo* foxtrot\n>\n> last\n",
            &["charlie", "alpha"][..],
        ),
        (
            "- one\n- alpha bravo charlie\n  delta echo foxtrot golf\n- three\n",
            &["delta", "bravo"],
        ),
        (
            "1. a\n2. b\n   bravo\n\n   second para\n   more words\n",
            &["more"],
        ),
        // At 12 cells the hanging indent wraps `abcdefghijk` onto a row of its own, which a
        // flat wrap of the raw line wouldn't: the stack's rows are the painter's.
        (
            "- one\n- ab abcdefghijk\n  more words\n- three\n",
            &["more", "ab "],
        ),
        ("> - a\n>   b soft\n> - c\n", &["soft"]),
        ("- [ ] task text\n  more text\n- [x] done\n", &["more"]),
        (
            "- a\n  - b\n    soft *word*\n    - c\n      deep word\n\nafter\n",
            &["soft", "deep"],
        ),
        ("> alpha\nlazy line\n\nafter\n", &["lazy"]),
        ("- alpha\nlazy line\n", &["lazy"]),
    ];
    // The cases are independent and each clicks every painted cell, so they run on threads.
    std::thread::scope(|scope| {
        for &(src, needles) in cases {
            for &needle in needles {
                for width in [40, 12] {
                    scope.spawn(move || {
                        let at = src.find(needle).unwrap();
                        // The cursor's paragraph must actually stack, or this tests nothing new.
                        let mut st = EditorState::new(Buffer::from_str(src), theme());
                        st.mode = Mode::Rendered;
                        st.set_viewport_width(width);
                        st.cursor.offset = st.buffer.rope().byte_to_char(at);
                        st.update_cursor_block();
                        st.cursor_block_entered_at = None;
                        assert!(
                            st.effective_rows(width).has_reveal(),
                            "{needle:?} must reveal stacked in {src:?}"
                        );
                        check_click_and_paint_from(src, width as u16, Some(at)).unwrap_or_else(
                            |e| panic!("width {width}, at {needle:?}: {e}\nin {src:?}"),
                        );
                    });
                }
            }
        }
    });
}

/// A revealed footnote paragraph, first or later, stacks like any nested one: every click, wide
/// and wrapping, lands where the indicator then shows, and on the stack on the char clicked.  A
/// click on the reference, the leader or the raw `[^n]:` follows it and is skipped.
#[test]
fn clicking_a_revealed_footnote_flow_keeps_the_cursor_where_it_shows() {
    let src = "ref[^n]\n\n[^n]: note one\n    two three\n\n    second para\n    more words\n";
    // The cases are independent and each clicks every painted cell, so they run on threads.
    std::thread::scope(|scope| {
        for needle in ["note", "three", "second", "words"] {
            for width in [40, 12] {
                scope.spawn(move || {
                    let at = src.find(needle).unwrap();
                    let mut st = EditorState::new(Buffer::from_str(src), theme());
                    st.mode = Mode::Rendered;
                    st.set_viewport_width(width);
                    st.cursor.offset = st.buffer.rope().byte_to_char(at);
                    st.update_cursor_block();
                    st.cursor_block_entered_at = None;
                    assert!(
                        st.effective_rows(width).has_reveal(),
                        "{needle:?} must reveal stacked"
                    );
                    check_click_and_paint_from(src, width as u16, Some(at))
                        .unwrap_or_else(|e| panic!("width {width}, at {needle:?}: {e}"));
                });
            }
        }
    });
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn origins_agree_with_the_rendered_rows(src in markdown_gen::document()) {
        if let Err(e) = check(&src) {
            prop_assert!(false, "{}", e);
        }
    }
}

proptest! {
    // Every rendered char of every row, four variants each: a case costs far more than the
    // agreement test's, and the corpus above pins the shapes that matter.
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn columns_round_trip(src in markdown_gen::document()) {
        if let Err(e) = round_trip(&src) {
            prop_assert!(false, "{}", e);
        }
    }
}
