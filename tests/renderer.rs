use edamame::config::Theme;
/// Snapshot tests for the Markdown renderer.
///
/// Each test parses a Markdown string, renders it, and checks the result with
/// `insta::assert_debug_snapshot!`.  Run `cargo insta review` after any change
/// that alters rendered output to review and accept updated snapshots.
use edamame::markdown::parser::parse;
use edamame::markdown::renderer::Renderer;

fn render(md: &str) -> Vec<ratatui::text::Line<'static>> {
    let theme = Box::leak(Box::new(Theme::default()));
    let blocks = parse(md);
    Renderer::new(theme).render(&blocks)
}

/// Collect all text content from a rendered line (spans concatenated).
fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

// ── Headings ─────────────────────────────────────────────────────────────────

#[test]
fn h1_contains_text() {
    let lines = render("# Heading One\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("Heading One"), "got: {text:?}");
}

#[test]
fn h1_has_rule_line() {
    let lines = render("# H1\n");
    // Second non-empty line after the heading should be the ─── rule
    let rule_line = lines.iter().find(|l| line_text(l).contains('─'));
    assert!(rule_line.is_some(), "H1 should produce a ─── rule line");
}

#[test]
fn h2_contains_text() {
    let lines = render("## Heading Two\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("Heading Two"));
}

#[test]
fn h3_contains_text() {
    let lines = render("### Three\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("Three"));
}

#[test]
fn h4_contains_text() {
    let lines = render("#### Four\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("Four"));
}

#[test]
fn h5_contains_text() {
    let lines = render("##### Five\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("Five"));
}

#[test]
fn h6_contains_text() {
    let lines = render("###### Six\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("Six"));
}

// ── Inline formatting ─────────────────────────────────────────────────────────

#[test]
fn bold_text_present() {
    let lines = render("**bold text**\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("bold text"), "got: {text:?}");
}

#[test]
fn italic_text_present() {
    let lines = render("*italic text*\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("italic text"), "got: {text:?}");
}

#[test]
fn inline_code_present() {
    let lines = render("`code_here`\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("code_here"), "got: {text:?}");
}

// ── Block elements ────────────────────────────────────────────────────────────

#[test]
fn fenced_code_block_content() {
    let lines = render("```rust\nfn main() {}\n```\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("fn main()"), "got: {text:?}");
}

#[test]
fn fenced_code_block_has_background() {
    let lines = render("```\nhello\n```\n");
    // Code block renders three rows: opening-fence placeholder, body, and
    // closing-fence placeholder.  All three carry the code-block background.
    let body_text = line_text(&lines[1]);
    assert!(
        body_text.contains("hello"),
        "Expected code content, got: {body_text:?}"
    );
    // There should be no box border characters anywhere in the block.
    let all_text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(
        !all_text.contains('╭'),
        "Unexpected box border: {all_text:?}"
    );
}

#[test]
fn fenced_code_block_language_tag() {
    let lines = render("```python\npass\n```\n");
    // Language tag is shown on its own line above the code content.
    let first_text = line_text(&lines[0]);
    assert!(
        first_text.contains("python"),
        "Expected language label, got: {first_text:?}"
    );
}

#[test]
fn blockquote_has_bar() {
    let lines = render("> A blockquote\n");
    let first_text = line_text(&lines[0]);
    assert!(
        first_text.contains('▎'),
        "Expected ▎ bar, got: {first_text:?}"
    );
}

#[test]
fn blockquote_contains_text() {
    let lines = render("> A blockquote\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("A blockquote"), "got: {text:?}");
}

#[test]
fn bullet_list_marker() {
    let lines = render("- item one\n- item two\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains('•'), "Expected bullet •, got: {text:?}");
    assert!(text.contains("item one"));
    assert!(text.contains("item two"));
}

#[test]
fn ordered_list_numbers() {
    let lines = render("1. first\n2. second\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("1."), "got: {text:?}");
    assert!(text.contains("2."), "got: {text:?}");
}

#[test]
fn horizontal_rule() {
    let lines = render("---\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains('─'), "Expected ─ rule, got: {text:?}");
}

// ── Snapshot tests ────────────────────────────────────────────────────────────

#[test]
fn snapshot_all_headings() {
    let md = "# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6\n";
    let lines = render(md);
    let output: Vec<String> = lines.iter().map(line_text).collect();
    insta::assert_debug_snapshot!(output);
}

#[test]
fn snapshot_inline_formatting() {
    let md = "Normal **bold** *italic* `code` ~~strike~~\n";
    let lines = render(md);
    let output: Vec<String> = lines.iter().map(line_text).collect();
    insta::assert_debug_snapshot!(output);
}

#[test]
fn snapshot_code_block() {
    let md = "```rust\nfn hello() {\n    println!(\"hi\");\n}\n```\n";
    let lines = render(md);
    let output: Vec<String> = lines.iter().map(line_text).collect();
    insta::assert_debug_snapshot!(output);
}

#[test]
fn snapshot_blockquote() {
    let md = "> This is a blockquote.\n> Second line.\n";
    let lines = render(md);
    let output: Vec<String> = lines.iter().map(line_text).collect();
    insta::assert_debug_snapshot!(output);
}

#[test]
fn snapshot_bullet_list() {
    let md = "- Alpha\n- Beta\n- Gamma\n";
    let lines = render(md);
    let output: Vec<String> = lines.iter().map(line_text).collect();
    insta::assert_debug_snapshot!(output);
}

#[test]
fn empty_task_item_renders_checkbox() {
    // An empty task item (`- [ ] ` with nothing after) must still render with
    // its checkbox visible — otherwise users can create a "phantom" item by
    // pressing Enter on a task row and find the new item invisible.
    let lines = render("- [ ] ");
    let output: Vec<String> = lines.iter().map(line_text).collect();
    assert_eq!(
        output.len(),
        1,
        "expected one rendered line, got: {output:?}"
    );
    assert!(
        output[0].contains("[ ]"),
        "empty task item should render with '[ ]', got: {output:?}"
    );
}

#[test]
fn task_item_with_non_paragraph_first_block_renders_checkbox() {
    // A task item whose first block is not a paragraph (here a nested code
    // block) must still render its checkbox on the marker line.
    let lines = render("- [ ] \n  ```\n  x\n  ```\n");
    let output: Vec<String> = lines.iter().map(line_text).collect();
    assert!(
        output[0].contains("[ ]"),
        "task item with non-paragraph first block should render '[ ]', got: {output:?}"
    );
}

#[test]
fn empty_task_item_in_middle_of_list_renders_checkbox() {
    let lines = render("- [ ] alpha\n- [ ] \n- [ ] beta\n");
    let output: Vec<String> = lines.iter().map(line_text).collect();
    assert_eq!(
        output.len(),
        3,
        "expected three rendered lines, got: {output:?}"
    );
    for line in &output {
        assert!(
            line.contains("[ ]"),
            "every task item should render with '[ ]', got: {output:?}"
        );
    }
}

#[test]
fn single_blank_between_list_items_is_rendered_as_a_blank_line() {
    // After 2 Enters in the editor the buffer holds an empty list item
    // separated from the previous content by a single blank line.  The
    // blank line must appear as a visible blank row in the rendered
    // output — without it the user can't see why pressing Enter twice
    // had any effect.
    use edamame::document::ParsedDoc;
    let theme = Box::leak(Box::new(Theme::default())) as &'static Theme;
    let pd = ParsedDoc::build("- a\n- b\n\n- \n- c\n", theme, true, 24);
    let texts: Vec<String> = (0..pd.line_count())
        .map(|i| {
            pd.lines[i]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.is_empty()),
        "expected a blank rendered line for the single source blank, got: {texts:?}"
    );
}

#[test]
fn loose_list_blank_rows_keep_reveal_1to1_with_source_lines() {
    // The raw reveal in `RenderedView` maps a block's rendered lines onto its
    // source lines by splitting the block text on `\n`.  A loose list now
    // keeps its inter-item blanks as its own rendered lines (instead of
    // fragmenting into virtual blank blocks), so the blank-emission must add
    // exactly one rendered line per blank source line — otherwise the reveal
    // desyncs.  Single-line items isolate that guarantee from the pre-existing
    // soft-break behavior (a wrapped continuation line renders joined, which
    // affects tight and loose lists alike and is out of scope here).
    use edamame::document::ParsedDoc;
    let theme = Box::leak(Box::new(Theme::default())) as &'static Theme;
    let src = "1. a\n2. b\n\n3. c\n\n4. d\n";
    let pd = ParsedDoc::build(src, theme, true, 40);
    let range = pd
        .source_map
        .original_range_for_byte(0)
        .expect("list block range");
    let block_src = &src[range.clone()];
    // Trailing newline(s) end the block's last line; the reveal backs past
    // them (`content_end_of_block`) and never indexes a trailing empty, so
    // compare against the block's *content* lines.
    let source_lines = block_src.trim_end_matches('\n').split('\n').count();
    let rendered = pd.source_map.rendered_lines_for_byte(0);
    assert_eq!(
        rendered.end - rendered.start,
        source_lines,
        "loose list must own one rendered line per source line (src {block_src:?})"
    );
}

#[test]
fn ordered_list_with_blank_gap_stays_one_loose_list_and_keeps_the_blank() {
    // A blank line in an ordered list makes it "loose" but keeps it a
    // single list: numbering comes from pulldown-cmark (auto-incremented),
    // so a source that restarts at `1.` after the blank still renders
    // sequentially (`3. c`) — matching CommonMark rather than the old
    // split-into-two-lists behavior.  The blank row is still preserved
    // between the items for legibility.
    use edamame::document::ParsedDoc;
    let theme = Box::leak(Box::new(Theme::default())) as &'static Theme;
    let pd = ParsedDoc::build("1. a\n2. b\n\n1. c\n", theme, true, 24);
    let texts: Vec<String> = (0..pd.line_count())
        .map(|i| {
            pd.lines[i]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.starts_with("3. c")),
        "expected the loose list to number sequentially (3. c), got: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.starts_with("1. c")),
        "tail should not restart at 1 in a single loose list, got: {texts:?}"
    );
    // The blank between item 2 and item 3 must still render as its own row.
    let b_idx = texts.iter().position(|t| t.starts_with("2. b")).unwrap();
    let c_idx = texts.iter().position(|t| t.starts_with("3. c")).unwrap();
    assert!(
        texts[b_idx + 1..c_idx].iter().any(|t| t.is_empty()),
        "expected a blank row between the two items, got: {texts:?}"
    );
}

#[test]
fn list_item_with_code_block_containing_blank_line_renders_without_gap() {
    // Regression: a blank line inside a fenced code block embedded in a
    // list item used to trigger `split_lists_on_blank_lines`, fragmenting
    // the surrounding list and leaving a visible gap before the next bullet.
    // The fix is verified at two layers — at the AST layer the list stays
    // intact (asserted in the parser's unit tests), and at the rendered
    // layer the second bullet sits directly after the first item's last
    // continuation paragraph with no synthesized separator row in between.
    let md = "- intro\n  ```toml\n  [a]\n\n  [b]\n  ```\n  trailing\n- next item\n";
    let lines = render(md);
    let texts: Vec<String> = lines.iter().map(line_text).collect();
    let trailing_idx = texts
        .iter()
        .position(|s| s.contains("trailing"))
        .expect("trailing continuation line missing");
    let next_idx = texts
        .iter()
        .position(|s| s.contains("next item"))
        .expect("next item line missing");
    assert_eq!(
        next_idx,
        trailing_idx + 1,
        "next bullet should sit on the line immediately after the trailing continuation, got: {texts:?}"
    );
}

#[test]
fn snapshot_horizontal_rule() {
    let lines = render("---\n");
    let output: Vec<String> = lines.iter().map(line_text).collect();
    insta::assert_debug_snapshot!(output);
}

// ── Footnotes ────────────────────────────────────────────────────────────────

#[test]
fn footnote_reference_renders_as_bracketed_marker() {
    let lines = render("Claim.[^1]\n\n[^1]: Source.\n");
    let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    // The reference collapses `[^1]` to the bracketed marker `[1]`; the raw
    // `[^1]` syntax is gone.
    assert!(text.contains("[1]"), "expected [1] marker, got: {text:?}");
    assert!(
        !text.contains("[^1]"),
        "raw reference syntax leaked: {text:?}"
    );
}

#[test]
fn adjacent_footnote_references_fuse_into_one_marker() {
    // `[^1][^2]` renders as a single comma-joined marker `[1,2]` rather
    // than two abutting brackets.
    let lines = render("Two.[^1][^2]\n\n[^1]: one.\n\n[^2]: two.\n");
    let para = line_text(&lines[0]);
    assert!(para.contains("[1,2]"), "expected [1,2], got: {para:?}");
    assert!(
        !para.contains("[1][2]"),
        "adjacent references should fuse: {para:?}"
    );
}

#[test]
fn three_adjacent_footnote_references_fuse_into_one_marker() {
    let lines = render("T.[^1][^2][^3]\n\n[^1]: a.\n\n[^2]: b.\n\n[^3]: c.\n");
    let para = line_text(&lines[0]);
    assert!(para.contains("[1,2,3]"), "expected [1,2,3], got: {para:?}");
}

#[test]
fn spaced_footnote_references_stay_separate_markers() {
    // Only *adjacent* references fuse — a space between them is its own
    // inline, so each keeps its own brackets.
    let lines = render("Two.[^1] [^2]\n\n[^1]: one.\n\n[^2]: two.\n");
    let para = line_text(&lines[0]);
    assert!(para.contains("[1] [2]"), "expected [1] [2], got: {para:?}");
}

#[test]
fn footnote_marker_is_plain_ascii() {
    // The marker used to be built from U+207D/U+207E superscript
    // parentheses, which most monospace fonts lack; a terminal falling back
    // to a proportional face drew the parenthesis over the digit.  Nothing
    // in the marker may leave Basic Latin.
    let lines = render("Claim.[^1][^note]\n\n[^1]: a.\n\n[^note]: b.\n");
    let para = line_text(&lines[0]);
    assert!(para.is_ascii(), "footnote marker left ASCII: {para:?}");
}

#[test]
fn footnote_definition_renders_with_back_link_and_number() {
    let lines = render("Claim.[^1]\n\n[^1]: The source text.\n");
    let def_line = lines
        .iter()
        .map(line_text)
        .find(|t| t.contains("The source text."))
        .expect("definition line missing");
    // The `  <label>.  ` leader (two leading spaces, raw label, period, two
    // trailing spaces — the rendered marker never diverges from the source
    // label) and the trailing `↩` back-link glyph at the very end.
    assert!(
        def_line.starts_with("  1.  "),
        "expected `  <label>.  ` leader: {def_line:?}"
    );
    assert!(
        def_line.trim_end().ends_with('↩'),
        "expected trailing back-link glyph: {def_line:?}"
    );
    // The glyph follows the body text, not the leader.
    let body_pos = def_line.find("The source text.").expect("body");
    let glyph_pos = def_line.find('↩').expect("glyph");
    assert!(
        glyph_pos > body_pos,
        "glyph should be after the body: {def_line:?}"
    );
}

#[test]
fn footnote_marker_matches_raw_label_without_renumbering() {
    // `[^3]` referenced before `[^1]` must keep their raw labels in the
    // rendered markers — no display resequencing.
    let lines = render("A[^3] B[^1]\n\n[^1]: one.\n\n[^3]: three.\n");
    let para = line_text(&lines[0]);
    let pos3 = para.find("[3]").expect("[3] marker");
    let pos1 = para.find("[1]").expect("[1] marker");
    assert!(
        pos3 < pos1,
        "markers follow source order, not a remap: {para:?}"
    );
}

#[test]
fn footnotes_render_in_place_not_only_at_end() {
    // A definition written mid-document renders where it appears, before
    // the following paragraph.
    let lines = render("A.[^1]\n\n[^1]: Mid.\n\nLater paragraph.\n");
    let texts: Vec<String> = lines.iter().map(line_text).collect();
    let def_idx = texts.iter().position(|t| t.contains("Mid.")).expect("def");
    let later_idx = texts
        .iter()
        .position(|t| t.contains("Later paragraph."))
        .expect("later");
    assert!(
        def_idx < later_idx,
        "definition should render before the later paragraph: {texts:?}"
    );
}

// ── Code-block column geometry ───────────────────────────────────────────────

/// Anti-drift pin between the renderer's literal pad (`format!(" {:<width$}",
/// …)`) and `markdown::code_layout`, which the cursor indicator, the
/// selection overlay and the mouse hit-test all map columns through.  If the
/// renderer's prefix ever changes, this fails rather than silently putting
/// the cursor beside its character again (issue #28).
#[test]
fn code_block_render_agrees_with_code_layout_column_map() {
    use edamame::markdown::code_layout::code_raw_col_to_rendered_col;

    for (md, raw_line, fenced) in [
        ("```rust\nlet x = 1;\n```\n", "let x = 1;", true),
        ("Intro.\n\n    let x = 1;\n", "    let x = 1;", false),
    ] {
        let lines = render(md);
        let rendered = lines
            .iter()
            .map(line_text)
            .find(|t| t.contains("let x = 1;"))
            .expect("code body row must render");
        let rendered_chars: Vec<char> = rendered.chars().collect();

        for (raw_col, expected) in raw_line.chars().enumerate() {
            // Columns inside an indented block's stripped indent have no
            // rendered cell of their own; they collapse onto the first.
            if !fenced && raw_col < 4 {
                continue;
            }
            let col = code_raw_col_to_rendered_col(raw_line, fenced, raw_col);
            assert_eq!(
                rendered_chars.get(col).copied(),
                Some(expected),
                "raw col {raw_col} of {raw_line:?} should render at col {col}, \
                 rendered row is {rendered:?}",
            );
        }
    }
}
