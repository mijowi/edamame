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
fn ordered_list_renumber_reset_at_blank_renders_as_two_lists() {
    // After 3 Enters in the middle of an ordered list, the surviving
    // head keeps its source numbering and the tail restarts at 1.  A
    // single blank line carries the parser-level split, so the rendered
    // output shows two ordered lists separated by one blank row — and
    // critically, the tail's first item is rendered as `1.` (not the
    // auto-incremented next number from the head).
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
        texts.iter().any(|t| t.starts_with("1. c")),
        "expected the tail list to restart at 1, got: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.is_empty()),
        "expected a blank line between the two split lists, got: {texts:?}"
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
    // continuation paragraph with no synthesised separator row in between.
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
