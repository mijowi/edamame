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

use proptest::prelude::*;
use ratatui::text::Line;

use edamame::config::Theme;
use edamame::document::ParsedDoc;
use edamame::markdown::ast::ListItem;
use edamame::markdown::table_layout::{char_cells, rendered_pipe_cells, wrap_cell};
use edamame::markdown::{
    inlines_to_plain, Block, ColOrigin, ContentKind, Inline, InlineColMap, RowOrigin, SrcLines,
};

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
                    slices.push(slice);
                }
                // `InlineColMap` doesn't model smart punctuation collapsing a run (`---` → `—`,
                // `...` → `…`), nor tell an undefined `[^x]`, which the row shows literally, from
                // a reference it narrows to the `[x]` marker, so it can't vouch for such a row's
                // length.  Nor does it render an image's `[Image: …]` placeholder.
                let smart = |t: &String| t.contains("--") || t.contains("...");
                if slices.iter().any(smart) || content.contains("[^") || content.contains("[Image:")
                {
                    continue;
                }
                let shown: Vec<char> = content.chars().collect();
                if atomic_tail {
                    if let Err(e) = check_alnum_sequence(&slices.join("\n"), &shown) {
                        return err(e);
                    }
                    continue;
                }
                // Joined, an inline spanning a break (`*a⏎b*`) maps as the document parses it;
                // line by line, a continuation that joining would turn into block syntax does.
                // A flow must agree with the row one way or the other.
                if let Err(line_by_line) = check_slices(&slices, &shown) {
                    if slices.len() == 1 || check_slices(&[slices.join("\n")], &shown).is_err() {
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
    Ok(())
}

/// `slices`, each mapped by its own `InlineColMap` and joined by one space, render exactly the
/// row's `shown` content, every letter or digit onto the same letter or digit.
fn check_slices(slices: &[String], shown: &[char]) -> Result<(), String> {
    let mut offset = 0usize;
    for (i, text) in slices.iter().enumerate() {
        if i > 0 {
            offset += 1;
        }
        // Built on its own, a slice reading `2. a` or `# b` parses as a block construct; a
        // leading word pins it to paragraph text, as it was.
        let map = InlineColMap::build(&format!("a {text}"));
        let len = map.rendered_len().saturating_sub(2);
        let raw: Vec<char> = text.chars().collect();
        for k in 0..len {
            let Some(&ch) = shown.get(offset + k) else {
                return Err(format!("the row is shorter than {slices:?} renders"));
            };
            let at = map.rendered_to_raw_vec()[k + 2].saturating_sub(2);
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

/// `text`, mapped by one `InlineColMap`, renders the same letters and digits in the same order
/// as the row's `shown` content, wherever its whitespace falls.
fn check_alnum_sequence(text: &str, shown: &[char]) -> Result<(), String> {
    let map = InlineColMap::build(&format!("a {text}"));
    let raw: Vec<char> = text.chars().collect();
    let rendered: String = map
        .rendered_to_raw_vec()
        .iter()
        .take(map.rendered_len())
        .skip(2)
        .filter_map(|&at| raw.get(at.saturating_sub(2)))
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
/// two sample fixtures.
const CORPUS: &[&str] = &[
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
    "Title\n=====\n\nSub\n---\n\npara\n",
    "[^1]: a note\n    more\n\nref[^1]\n",
    // A break nested in emphasis or a link: one row over both lines, not one row per line.
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
];

#[test]
fn origins_agree_with_the_rendered_rows_on_the_corpus() {
    for src in CORPUS {
        check(src).unwrap();
    }
    for fixture in ["general.md", "syntax.md"] {
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn origins_agree_with_the_rendered_rows(src in markdown_gen::document()) {
        if let Err(e) = check(&src) {
            prop_assert!(false, "{}", e);
        }
    }
}
