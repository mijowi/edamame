//! Markdown syntax cheat sheet — surfaced via the command palette entry
//! `Show Markdown Cheat Sheet`.
//!
//! The body is built as styled `Line`s drawn directly from the active
//! [`Theme`], so the cheat sheet visually matches preview / rendered
//! mode (bold for `**bold**`, code-span colours for `` `code` ``, link
//! colour for `[text](url)`, and so on).  We deliberately do *not*
//! parse the source as Markdown: the whole point of the sheet is to
//! show the raw syntax markers, which a real renderer would consume.
//!
//! Spans that carry no domain styling — indentation, separators between
//! examples, and otherwise plain rows — use [`Span::raw`] / [`Line::raw`]
//! so they inherit the surrounding `Paragraph` style (the modal's
//! `theme.status_bar` background).  Using `theme.normal` here would be
//! wrong: it explicitly resets `bg` to `Color::Reset`, which on a real
//! terminal repaints the cell with the default background and lets the
//! editor's dark fill bleed through the modal.
//!
//! Tables and footnotes are intentionally absent: tables have a
//! dedicated insert/edit flow (see Phase 15) so hand-coding the
//! pipe-grid form is rarely useful, and footnotes are not yet
//! implemented in the renderer.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::config::Theme;

/// Build the styled cheat-sheet body, one logical row per line.
/// Returned `Line`s carry theme-driven styling so the popover looks
/// like preview/rendered mode while still showing the raw Markdown
/// syntax markers.
#[allow(clippy::vec_init_then_push)] // grouped pushes mirror the on-screen sections
pub fn body_lines(theme: &Theme) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();

    // ── Headings ──────────────────────────────────────────────────────
    out.push(section(theme, "Headings"));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("# H1", theme.h1),
        Span::raw("   "),
        Span::styled("## H2", theme.h2),
        Span::raw("   "),
        Span::styled("### H3", theme.h3),
        Span::raw("   "),
        Span::styled("#### H4", theme.h4),
        Span::raw("   "),
        Span::styled("##### H5", theme.h5),
        Span::raw("   "),
        Span::styled("###### H6", theme.h6),
    ]));
    out.push(blank());

    // ── Inline ────────────────────────────────────────────────────────
    out.push(section(theme, "Inline"));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("**bold**", theme.bold),
        Span::raw("   "),
        Span::styled("_italic_", theme.italic),
        Span::raw("  "),
        Span::styled(
            "**_bold italic_**",
            theme.bold.add_modifier(Modifier::ITALIC),
        ),
        Span::raw("   "),
        Span::styled("`code`", theme.code_span),
        Span::raw("   "),
        Span::styled("~~strike~~", theme.strikethrough),
        Span::raw("   "),
        Span::styled("==highlight==", theme.highlight),
    ]));
    out.push(blank());

    // ── Lists ─────────────────────────────────────────────────────────
    out.push(section(theme, "Lists"));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("-", theme.list_bullet),
        Span::raw(" bullet                    "),
        Span::styled("1.", theme.list_number),
        Span::raw(" ordered"),
    ]));
    out.push(Line::from(vec![
        Span::raw("    "),
        Span::styled("-", theme.list_bullet),
        Span::raw(" nested                     "),
        Span::styled("2.", theme.list_number),
        Span::raw(" next"),
    ]));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("-", theme.list_bullet),
        Span::raw(" "),
        Span::styled("[ ]", theme.task_unchecked),
        Span::raw(" task   "),
        Span::styled("-", theme.list_bullet),
        Span::raw(" "),
        Span::styled("[x]", theme.task_checked),
        Span::styled(" done", task_done_text_style(theme)),
    ]));
    out.push(blank());

    // ── Links ─────────────────────────────────────────────────────────
    out.push(section(theme, "Links"));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("[section](#heading-anchor)", theme.link_text),
    ]));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("[local file](./notes.md)", theme.link_text),
    ]));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("[website](https://example.com)", theme.link_text),
    ]));
    out.push(blank());

    // ── Images ────────────────────────────────────────────────────────
    out.push(section(theme, "Images"));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("![alt text](./diagram.png)", theme.image_placeholder),
    ]));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "![alt text](https://example.com/photo.jpg)",
            theme.image_placeholder,
        ),
    ]));
    out.push(blank());

    // ── Block quote ───────────────────────────────────────────────────
    out.push(section(theme, "Block quote"));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(">", theme.blockquote_bar),
        Span::styled(" quoted text spans", theme.blockquote_text),
    ]));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(">", theme.blockquote_bar),
        Span::styled(" multiple lines.", theme.blockquote_text),
    ]));
    out.push(blank());

    // ── Code block ────────────────────────────────────────────────────
    out.push(section(theme, "Code block"));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("```", theme.code_block_border),
        Span::styled("rust", theme.code_block_lang),
    ]));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("fn main() {}", theme.code_block_text),
    ]));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("```", theme.code_block_border),
    ]));
    out.push(blank());

    // ── Horizontal rule ───────────────────────────────────────────────
    out.push(section(theme, "Horizontal rule"));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("---", theme.rule),
    ]));
    out.push(blank());

    // ── Hard line break ───────────────────────────────────────────────
    out.push(section(theme, "Hard line break"));
    out.push(Line::raw("  Two spaces at end of line  ⏎"));
    out.push(blank());

    // ── Diagrams (Mermaid) ────────────────────────────────────────────
    out.push(section(theme, "Diagrams (Mermaid)"));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("```", theme.code_block_border),
        Span::styled("mermaid", theme.code_block_lang),
    ]));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("graph TD; A-->B;", theme.code_block_text),
    ]));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("```", theme.code_block_border),
    ]));

    out
}

/// Empty row — uses `Line::raw` rather than a styled blank so the
/// modal's `status_bar` background fills the spacer.
fn blank() -> Line<'static> {
    Line::raw("")
}

/// Render a section heading row (e.g. `Headings`, `Inline`, `Lists`).
/// Reuses the H2 style so the popover's section dividers carry the
/// same visual weight as a real H2 in preview mode.
fn section(theme: &Theme, label: &'static str) -> Line<'static> {
    Line::from(Span::styled(label, theme.modal_section_heading))
}

/// Style applied to the *text* of a checked task list item.  Mirrors
/// the rendered behaviour of `RenderedView`: when
/// `theme.task_strikethrough` is true the text is crossed out;
/// otherwise it stays unstyled so the modal background shows through.
/// We deliberately do not start from `theme.normal` — that struct
/// resets `bg` to `Color::Reset`, which would punch through the
/// modal's `status_bar` fill.
fn task_done_text_style(theme: &Theme) -> Style {
    if theme.task_strikethrough {
        Style::default().add_modifier(Modifier::CROSSED_OUT)
    } else {
        Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(theme: &Theme) -> String {
        body_lines(theme)
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn cheat_sheet_includes_supported_features() {
        let theme = Theme::default();
        let s = joined(&theme);
        assert!(s.contains("- [ ]"));
        assert!(s.contains("~~strike~~"));
        assert!(s.contains("==highlight=="));
        assert!(s.contains("Mermaid"));
        assert!(s.contains("Links"));
        assert!(s.contains("Images"));
        // Header anchor + local file + http examples in the Links section.
        assert!(s.contains("#heading-anchor"));
        assert!(s.contains("./notes.md"));
        assert!(s.contains("https://example.com"));
    }

    #[test]
    fn cheat_sheet_excludes_unsupported_or_redundant_sections() {
        // Tables are surfaced through Phase 2 / Phase 15 dedicated
        // editing flows, not hand-coded markdown.  Footnotes are not
        // yet implemented in the renderer.  Both must stay out of the
        // cheat sheet so users aren't pointed at syntax we don't
        // honour.
        let theme = Theme::default();
        let s = joined(&theme);
        assert!(
            !s.contains("Tables"),
            "Tables should not appear in the cheat sheet"
        );
        assert!(!s.contains("Footnotes"), "Footnotes should not appear");
        assert!(!s.contains("[^"), "footnote markers should not appear");
    }

    #[test]
    fn cheat_sheet_excludes_html_passthrough() {
        // The renderer does not honour raw HTML, so the cheat sheet
        // should not advertise `<br>` / `<details>` / `<sub>`-style
        // tags.  This keeps user expectations honest.
        let theme = Theme::default();
        let s = joined(&theme);
        for token in &["<br>", "<details>", "<sub>", "<sup>"] {
            assert!(
                !s.contains(token),
                "cheat sheet leaks unrendered HTML token: {token}"
            );
        }
    }

    #[test]
    fn cheat_sheet_styles_track_theme() {
        // The whole point of the rewrite: spans pull their style from
        // the theme rather than carrying hardcoded colours.  Verify by
        // building the body with two themes and confirming the bold
        // span on the inline row picks up the theme's `bold` style.
        let a = Theme {
            bold: Style::default().add_modifier(Modifier::BOLD),
            ..Theme::default()
        };
        let b = Theme {
            bold: Style::default().add_modifier(Modifier::BOLD | Modifier::ITALIC),
            ..Theme::default()
        };

        let lines_a = body_lines(&a);
        let lines_b = body_lines(&b);
        let bold_a = find_span(&lines_a, "**bold**").expect("bold span in a");
        let bold_b = find_span(&lines_b, "**bold**").expect("bold span in b");
        assert_eq!(bold_a.style, a.bold);
        assert_eq!(bold_b.style, b.bold);
        assert_ne!(bold_a.style, bold_b.style);
    }

    #[test]
    fn section_headings_use_modal_section_heading_style() {
        // Section labels in the popover use `modal_section_heading`
        // so they read as a modal-internal divider rather than as a
        // document H2 floating on top of the modal surface.
        let theme = Theme::default();
        let lines = body_lines(&theme);
        let headings_label = find_span(&lines, "Headings").expect("Headings label");
        assert_eq!(headings_label.style, theme.modal_section_heading);
    }

    #[test]
    fn separator_spans_inherit_modal_background() {
        // Indentation / separator spans must NOT carry a bg of their
        // own — they need to inherit the modal's `status_bar` fill.
        // `theme.normal` sets `bg(Color::Reset)`, which paints the
        // terminal default and lets the editor's dark fill bleed
        // through the modal; using `Span::raw` (style == default)
        // keeps the modal background intact.
        let theme = Theme::default();
        let lines = body_lines(&theme);
        for span in lines.iter().flat_map(|l| l.spans.iter()) {
            if span.content.chars().all(char::is_whitespace) {
                assert_eq!(
                    span.style,
                    Style::default(),
                    "whitespace span carried a non-default style: {:?}",
                    span,
                );
            }
        }
    }

    fn find_span<'a>(lines: &'a [Line<'a>], needle: &str) -> Option<&'a Span<'a>> {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content == needle)
    }
}
