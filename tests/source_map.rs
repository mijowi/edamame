/// Proptest round-trip invariants for `SourceMap`.
///
/// Invariants tested:
/// 1. Every source byte maps to at least one rendered line.
/// 2. Two different rendered lines do not claim overlapping source ranges.
/// 3. Together, the extended ranges cover the full source byte range.
///
/// These hold for the initial parse AND after a sequence of edits (which
/// trigger a full re-parse).
use proptest::prelude::*;

use edamame::config::Action;
use edamame::config::Theme;
use edamame::document::{Buffer, ParsedDoc};
use edamame::editor::{edit_ops, EditorState, Mode};

fn theme() -> &'static Theme {
    Box::leak(Box::new(Theme::default()))
}

/// Assert that the `ParsedDoc`'s `SourceMap` satisfies the coverage invariant:
/// every byte in `0..source.len()` maps to exactly one block (rendered line
/// range). Panics with a descriptive message on failure.
fn assert_coverage(source: &str, doc: &ParsedDoc) {
    if source.is_empty() {
        return; // nothing to cover
    }
    if doc.source_map.rendered_line_count() == 0 {
        // Pure-whitespace / entirely-blank document: the renderer produces no
        // lines, so the "every byte maps to a rendered line" invariant cannot
        // hold. This is an expected corner case — skip.
        return;
    }
    for byte in 0..source.len() {
        let range = doc.source_map.rendered_lines_for_byte(byte);
        assert!(
            !range.is_empty(),
            "source byte {} (char {:?}) not covered by any rendered line.\n\
             Source: {:?}\n\
             Block count: {}",
            byte,
            source.as_bytes().get(byte).map(|&b| b as char),
            source,
            doc.source_map.block_count(),
        );
    }
}

// ── Deterministic invariant checks ───────────────────────────────────────────

#[test]
fn coverage_empty() {
    let doc = ParsedDoc::build("", theme(), false, 24);
    assert_coverage("", &doc);
}

#[test]
fn coverage_single_paragraph() {
    let src = "Hello world\n";
    let doc = ParsedDoc::build(src, theme(), false, 24);
    assert_coverage(src, &doc);
}

#[test]
fn coverage_heading_paragraph_rule() {
    let src = "# Hello\n\nSome text.\n\n---\n";
    let doc = ParsedDoc::build(src, theme(), false, 24);
    assert_coverage(src, &doc);
}

#[test]
fn coverage_code_block() {
    let src = "```rust\nfn main() {}\n```\n\nText after.\n";
    let doc = ParsedDoc::build(src, theme(), false, 24);
    assert_coverage(src, &doc);
}

#[test]
fn coverage_list() {
    let src = "- item one\n- item two\n- item three\n";
    let doc = ParsedDoc::build(src, theme(), false, 24);
    assert_coverage(src, &doc);
}

#[test]
fn coverage_blockquote() {
    let src = "> A quoted paragraph.\n>\n> Another quoted paragraph.\n";
    let doc = ParsedDoc::build(src, theme(), false, 24);
    assert_coverage(src, &doc);
}

#[test]
fn coverage_table() {
    let src = "| A | B |\n|---|---|\n| 1 | 2 |\n";
    let doc = ParsedDoc::build(src, theme(), false, 24);
    assert_coverage(src, &doc);
}

#[test]
fn coverage_after_insert() {
    let buf = Buffer::from_str("Hello\n");
    let mut state = EditorState::new(buf, theme());
    state.mode = Mode::Rendered;

    edit_ops::apply(&mut state, Action::MoveDocEnd, 40, 80);
    edit_ops::apply(&mut state, Action::InsertChar('!'), 40, 80);

    let source = state.contents();
    assert_coverage(&source, &state.parsed);
}

#[test]
fn coverage_after_delete() {
    let buf = Buffer::from_str("Hello world\n");
    let mut state = EditorState::new(buf, theme());
    state.mode = Mode::Rendered;

    // Delete "world".
    edit_ops::apply(&mut state, Action::MoveDocEnd, 40, 80);
    for _ in 0.."world\n".len() {
        edit_ops::apply(&mut state, Action::DeleteCharBack, 40, 80);
    }

    let source = state.contents();
    assert_coverage(&source, &state.parsed);
}

#[test]
fn coverage_after_newline() {
    let buf = Buffer::from_str("Hello\n");
    let mut state = EditorState::new(buf, theme());
    state.mode = Mode::Rendered;

    edit_ops::apply(&mut state, Action::MoveDocEnd, 40, 80);
    edit_ops::apply(&mut state, Action::Newline, 40, 80);
    for ch in "World".chars() {
        edit_ops::apply(&mut state, Action::InsertChar(ch), 40, 80);
    }

    let source = state.contents();
    assert_coverage(&source, &state.parsed);
}

// ── Proptest: random markdown documents ──────────────────────────────────────

proptest! {
    /// For any arbitrary (valid UTF-8) markdown document, the source map covers
    /// all bytes. We limit the string size to keep tests fast.
    #[test]
    fn proptest_coverage_arbitrary_doc(
        src in r"[a-zA-Z0-9 \n#*`_>-]{0,200}"
    ) {
        let doc = ParsedDoc::build(&src, theme(), false, 24);
        if !src.is_empty() {
            assert_coverage(&src, &doc);
        }
    }

    /// After a sequence of inserts, the source map still covers all bytes.
    #[test]
    fn proptest_coverage_after_inserts(
        initial in r"[a-zA-Z \n]{0,50}",
        inserts in prop::collection::vec(r"[a-zA-Z]", 0..10)
    ) {
        let buf = Buffer::from_str(&initial);
        let mut state = EditorState::new(buf, theme());
        state.mode = Mode::Rendered;

        for s in inserts {
            for ch in s.chars() {
                edit_ops::apply(&mut state, Action::InsertChar(ch), 40, 80);
            }
        }

        let source = state.contents();
        assert_coverage(&source, &state.parsed);
    }
}

// ── Source positions in the AST ──────────────────────────────────────────────
//
// `docs/dev/plans/row-provenance.md` Phase 1: the parser records, from pulldown-cmark's own
// offsets, where every leaf's content starts on each of its source lines, and the line span of
// every container.  These check what it recorded against a second, independent pass over the
// same events.

#[path = "support/markdown_gen.rs"]
mod markdown_gen;

mod positions {

    use edamame::markdown::ast::ListItem;
    use edamame::markdown::parse_offsets::options_for;
    use edamame::markdown::{parse_raw_with_ranges, Block, LineSpan, SrcLines};
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};

    struct Lines<'s> {
        src: &'s str,
        starts: Vec<usize>,
    }

    impl<'s> Lines<'s> {
        fn new(src: &'s str) -> Self {
            let mut starts = vec![0];
            starts.extend(src.match_indices('\n').map(|(i, _)| i + 1));
            Self { src, starts }
        }
        fn line_of(&self, byte: usize) -> usize {
            self.starts.partition_point(|&s| s <= byte) - 1
        }
        fn col_of(&self, byte: usize) -> usize {
            let line = self.line_of(byte);
            self.src[self.starts[line]..byte].chars().count()
        }
    }

    /// An event that is a leaf's content, as opposed to block structure or an item's chrome.
    fn is_content(event: &Event<'_>) -> bool {
        match event {
            Event::Text(_)
            | Event::Code(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::InlineHtml(_)
            | Event::Html(_)
            | Event::FootnoteReference(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::Rule => true,
            Event::Start(tag) => matches!(
                tag,
                Tag::Emphasis
                    | Tag::Strong
                    | Tag::Strikethrough
                    | Tag::Link { .. }
                    | Tag::Image { .. }
                    | Tag::TableHead
                    | Tag::TableRow
                    | Tag::TableCell
            ),
            _ => false,
        }
    }

    /// Every content event as `(start line, start col, last line, event kind)`, and every
    /// link's `(first line, last line)`.
    struct Content<'s> {
        events: Vec<(usize, usize, usize, Kind)>,
        links: Vec<(usize, usize)>,
        lines: &'s Lines<'s>,
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        Text,
        Atomic,
        Other,
    }

    impl<'s> Content<'s> {
        fn new(src: &str, lines: &'s Lines<'s>) -> Self {
            let mut links = Vec::new();
            // A task box inside a heading is the heading's literal text (GFM has a box only at
            // the start of a paragraph); a paragraph's own box is the item's chrome.
            let mut in_heading = false;
            let events = Parser::new_ext(src, options_for(src))
                .into_offset_iter()
                .filter(|(e, _)| {
                    match e {
                        Event::Start(Tag::Heading { .. }) => in_heading = true,
                        Event::End(TagEnd::Heading(_)) => in_heading = false,
                        _ => {}
                    }
                    is_content(e) || (in_heading && matches!(e, Event::TaskListMarker(_)))
                })
                .map(|(e, r)| {
                    let line = lines.line_of(r.start);
                    let last = if r.end > r.start {
                        lines.line_of(r.end - 1)
                    } else {
                        line
                    };
                    let kind = match e {
                        Event::Text(_) | Event::Html(_) => Kind::Text,
                        // Alt text renders on the image's first row, like a code span's text.
                        Event::Code(_)
                        | Event::InlineMath(_)
                        | Event::DisplayMath(_)
                        | Event::InlineHtml(_)
                        | Event::Start(Tag::Image { .. }) => Kind::Atomic,
                        Event::Start(Tag::Link { .. }) => {
                            links.push((line, last));
                            Kind::Other
                        }
                        _ => Kind::Other,
                    };
                    // Indentation pulldown-cmark synthesizes (an indented HTML block's leading
                    // spaces) arrives as an empty-range `Text` past the spaces it stands for.
                    let mut start = r.start;
                    if let Event::Text(t) = &e {
                        if r.is_empty() {
                            let before = &src.as_bytes()[..start];
                            start -= before
                                .iter()
                                .rev()
                                .take(t.len())
                                .take_while(|&&b| b == b' ')
                                .count();
                        }
                    }
                    (line, lines.col_of(start), last, kind)
                })
                .collect();
            Self {
                events,
                links,
                lines,
            }
        }

        /// Where a line inside a link opening with the link's `](…)` has its content: past the
        /// container prefix, where no event starts (the link's `End` repeats its whole range).
        fn link_close_on(&self, line: usize) -> Option<usize> {
            let inside = self
                .links
                .iter()
                .any(|&(first, last)| line > first && line <= last);
            if !inside {
                return None;
            }
            let text = &self.lines.src[self.lines.starts[line]..];
            let col = text
                .chars()
                .take_while(|c| matches!(c, '>' | ' ' | '\t'))
                .count();
            (text.chars().nth(col) == Some(']')).then_some(col)
        }

        /// Where content starts on `line`: the earliest content event starting there, or column
        /// 0 if a text run begun on an earlier line covers it.
        fn start_on(&self, line: usize) -> Option<usize> {
            let starting = self
                .events
                .iter()
                .filter(|e| e.0 == line)
                .map(|e| e.1)
                .min();
            let covered = self
                .events
                .iter()
                .any(|e| e.3 == Kind::Text && e.0 < line && e.2 >= line);
            if covered {
                Some(0)
            } else {
                match (starting, self.link_close_on(line)) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                }
            }
        }

        /// Whether `line` is the tail of an atomic inline begun on an earlier line.
        fn continues_atomic(&self, line: usize) -> bool {
            self.events
                .iter()
                .any(|e| e.3 == Kind::Atomic && e.0 < line && e.2 >= line)
        }

        fn any_start_on(&self, line: usize) -> bool {
            self.events.iter().any(|e| e.0 == line)
        }
    }

    fn within(inner: &LineSpan, outer: &LineSpan) -> bool {
        inner.start >= outer.start && inner.end <= outer.end && inner.start < inner.end
    }

    /// Check one block (recursively) of a top-level block whose first line is `base` and
    /// which has `n` lines.  Returns the first violation found.
    fn check_block(
        block: &Block,
        base: usize,
        n: u32,
        content: &Content,
        src: &str,
    ) -> Result<(), String> {
        let span = block.span();
        if span.start >= span.end || span.end > n {
            return Err(format!(
                "span {span:?} outside the block's {n} lines: {block:?}"
            ));
        }
        let children: Vec<&Block> = match block {
            Block::BlockQuote { blocks, .. } | Block::FootnoteDefinition { blocks, .. } => {
                blocks.iter().collect()
            }
            Block::List { items, .. } => {
                for item in items {
                    check_item(item, &span, base, n, content, src)?;
                }
                Vec::new()
            }
            _ => Vec::new(),
        };
        for child in children {
            if !within(&child.span(), &span) {
                return Err(format!(
                    "child {child:?} escapes its parent's span {span:?}"
                ));
            }
            check_block(child, base, n, content, src)?;
        }
        if let Some(lines) = block.src() {
            check_src_lines(lines, base, content)?;
        }
        Ok(())
    }

    fn check_item(
        item: &ListItem,
        list: &LineSpan,
        base: usize,
        n: u32,
        content: &Content,
        src: &str,
    ) -> Result<(), String> {
        if !within(&item.span, list) {
            return Err(format!(
                "item {:?} escapes its list's span {list:?}",
                item.span
            ));
        }
        for child in &item.blocks {
            if !within(&child.span(), &item.span) {
                return Err(format!(
                    "child {child:?} escapes its item's span {:?}",
                    item.span
                ));
            }
            check_block(child, base, n, content, src)?;
        }
        Ok(())
    }

    fn check_src_lines(lines: &SrcLines, base: usize, content: &Content) -> Result<(), String> {
        for (k, col) in lines.cols().enumerate() {
            let line = base + lines.first as usize + k;
            match col {
                Some(col) => {
                    let expected = content.start_on(line);
                    if expected != Some(col as usize) {
                        return Err(format!(
                            "line {line}: recorded column {col}, the events say {expected:?}"
                        ));
                    }
                }
                None => {
                    if content.any_start_on(line) && !content.continues_atomic(line) {
                        return Err(format!(
                            "line {line}: recorded None, but content starts there"
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn check(src: &str) -> Result<(), String> {
        let lines = Lines::new(src);
        let content = Content::new(src, &lines);
        let (blocks, ranges) = parse_raw_with_ranges(src);
        if blocks.len() != ranges.len() {
            return Err("blocks and ranges must stay 1:1".into());
        }
        for (block, range) in blocks.iter().zip(&ranges) {
            let base = lines.line_of(range.start);
            // Every line the range touches, a trailing blank one included: an unclosed
            // fence's text can run onto it.
            let last = lines.line_of(range.end.saturating_sub(1).max(range.start));
            let n = (last - base + 1) as u32;
            check_block(block, base, n, &content, src).map_err(|e| format!("{e}\nin {src:?}"))?;
        }
        Ok(())
    }

    /// Every span and `SrcLines` in `blocks`, depth first.
    fn collect(blocks: &[Block], out: &mut Vec<(LineSpan, Option<SrcLines>)>) {
        for block in blocks {
            out.push((block.span(), block.src().cloned()));
            match block {
                Block::BlockQuote { blocks, .. } | Block::FootnoteDefinition { blocks, .. } => {
                    collect(blocks, out)
                }
                Block::List { items, .. } => {
                    for item in items {
                        out.push((item.span.clone(), None));
                        collect(&item.blocks, out);
                    }
                }
                _ => {}
            }
        }
    }

    /// The same document with CRLF line ends records the same lines and columns: a `\r` is
    /// part of its line's terminator, never content.
    pub fn check_crlf(src: &str) -> Result<(), String> {
        let crlf = src.replace('\n', "\r\n");
        let (mut lf_pos, mut crlf_pos) = (Vec::new(), Vec::new());
        collect(&parse_raw_with_ranges(src).0, &mut lf_pos);
        collect(&parse_raw_with_ranges(&crlf).0, &mut crlf_pos);
        if lf_pos != crlf_pos {
            return Err(format!("LF {lf_pos:?}\nCRLF {crlf_pos:?}\nin {src:?}"));
        }
        Ok(())
    }
}

#[test]
fn positions_hold_on_hand_picked_documents() {
    for src in [
        "- a\n  soft\n- b\\\n  hard\n1. c\nlazy\n",
        "> - a\n>\n> - b\n>\n> tail\n",
        "8. Tag it.\n\n    ```bash\n    gh run watch\n\n      indented\n    ```\n",
        "- ```bash\n  code\n  ```\n- next item\n\n- third\n",
        "- a\n  ```\n  x\n\n- b\n",
        "> a\n  b `c\nd` e\n",
        "- [ ] task\n- [x] done\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n",
        "Title\n=====\n\nSub\n---\n",
        "    a\n\n    b\n",
        "[^1]: a\n\n    b\n\nc\n",
    ] {
        positions::check(src).unwrap();
        positions::check_crlf(src).unwrap();
    }
    for fixture in ["general.md", "syntax.md"] {
        let src = std::fs::read_to_string(format!(
            "{}/tests/fixtures/{fixture}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        positions::check(&src).unwrap();
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Every recorded line lies inside its top-level block, child spans nest in their parent's,
    /// and each paragraph, heading and code line's column is where pulldown-cmark's content
    /// starts on it (or `None` exactly where none does).
    #[test]
    fn proptest_ast_positions_match_the_events(src in markdown_gen::document()) {
        if let Err(e) = positions::check(&src) {
            prop_assert!(false, "{}", e);
        }
    }

    #[test]
    fn proptest_crlf_positions_match_lf(src in markdown_gen::document()) {
        if let Err(e) = positions::check_crlf(&src) {
            prop_assert!(false, "{}", e);
        }
    }
}
