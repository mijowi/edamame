use markdown_tui::config::Theme;
/// Snapshot tests for the Markdown renderer.
///
/// Each test parses a Markdown string, renders it, and checks the result with
/// `insta::assert_debug_snapshot!`.  Run `cargo insta review` after any change
/// that alters rendered output to review and accept updated snapshots.
use markdown_tui::markdown::parser::parse;
use markdown_tui::markdown::renderer::Renderer;

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
    // Code block now renders with a coloured background, no box border.
    // First line should be the code content (with leading space padding).
    let first_text = line_text(&lines[0]);
    assert!(
        first_text.contains("hello"),
        "Expected code content, got: {first_text:?}"
    );
    // There should be no box border characters.
    assert!(
        !first_text.contains('╭'),
        "Unexpected box border: {first_text:?}"
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
fn snapshot_horizontal_rule() {
    let lines = render("---\n");
    let output: Vec<String> = lines.iter().map(line_text).collect();
    insta::assert_debug_snapshot!(output);
}
