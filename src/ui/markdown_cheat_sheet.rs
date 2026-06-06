//! Markdown syntax cheat sheet — surfaced via the command palette entry
//! `Show Markdown Cheat Sheet`.
//!
//! The body is built as styled `Line`s drawn directly from the active
//! [`Theme`], so the cheat sheet visually matches preview / rendered
//! mode (bold for `**bold**`, code-span colors for `` `code` ``, link
//! color for `[text](url)`, and so on).  We deliberately do *not*
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
//! dedicated insert/edit flow so hand-coding the
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
    // Indices of lines that belong to a fenced-code or Mermaid block, paired
    // with the fill style their trailing pad should use.  The language row
    // uses `code_block_lang` (lighter `surface` bg); body and closing-fence
    // rows use `code_block_text` (darker `muted` bg) — mirroring the actual
    // renderer, which paints the lang label on a lighter surface than the
    // body.  A trailing-padding pass at the end fills the modal body width.
    let mut code_block_pad: Vec<(usize, Style)> = Vec::new();

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
    code_block_pad.push((out.len(), theme.code_block_lang));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("```", theme.code_block_lang),
        Span::styled("rust", theme.code_block_lang),
    ]));
    code_block_pad.push((out.len(), theme.code_block_text));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("fn main() {}", theme.code_block_text),
    ]));
    code_block_pad.push((out.len(), theme.code_block_text));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("```", theme.code_block_text),
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
    code_block_pad.push((out.len(), theme.code_block_lang));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("```", theme.code_block_lang),
        Span::styled("mermaid", theme.code_block_lang),
    ]));
    code_block_pad.push((out.len(), theme.code_block_text));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("graph TD; A-->B;", theme.code_block_text),
    ]));
    code_block_pad.push((out.len(), theme.code_block_text));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("```", theme.code_block_text),
    ]));

    pad_code_block_lines(&mut out, &code_block_pad, theme);
    out
}

/// Pad each code-block line with a trailing space run so the surface
/// background fills the modal's body width, matching how
/// `Renderer::render_code_block` pads to `viewport_width`.  Each entry
/// pairs a row index with the fill style for that row's pad — the
/// language row uses `code_block_lang` (lighter), body/fence rows use
/// `code_block_text` (darker).  The target width is the widest non-
/// code-block row; the modal sizes itself to that width, so
/// post-padding the code-block lines exactly fill the body area without
/// changing the modal's overall width.
fn pad_code_block_lines(
    lines: &mut [Line<'static>],
    code_block_pad: &[(usize, Style)],
    _theme: &Theme,
) {
    let code_indices: Vec<usize> = code_block_pad.iter().map(|(i, _)| *i).collect();
    let target_width: usize = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| !code_indices.contains(i))
        .map(|(_, l)| l.width())
        .max()
        .unwrap_or(0);

    for &(i, fill_style) in code_block_pad {
        let line = &mut lines[i];
        let cur = line.width();
        if cur < target_width {
            line.spans
                .push(Span::styled(" ".repeat(target_width - cur), fill_style));
        }
    }
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
        // Tables are surfaced through dedicated
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
        // the theme rather than carrying hardcoded colors.  Verify by
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
        // Indentation / separator spans must NOT carry a fg / modifier of
        // their own — they need to inherit the modal's `status_bar` fill.
        // `theme.normal` sets `bg(Color::Reset)`, which paints the
        // terminal default and lets the editor's dark fill bleed
        // through the modal; using `Span::raw` (style == default)
        // keeps the modal background intact.  Whitespace spans MAY
        // carry an explicit bg, however — that's how the code-block
        // sections fill their surface color out to the modal width.
        let theme = Theme::default();
        let lines = body_lines(&theme);
        for span in lines.iter().flat_map(|l| l.spans.iter()) {
            if span.content.chars().all(char::is_whitespace) {
                // The hazard guarded here is `theme.normal`'s
                // `bg(Color::Reset)`, which would punch through the
                // modal fill.  Either no bg at all (inherits the
                // modal's `status_bar`) or an explicit theme bg is
                // fine — fg / modifier on whitespace is invisible.
                let bg_ok = match span.style.bg {
                    None => true,
                    Some(ratatui::style::Color::Reset) => false,
                    Some(_) => true,
                };
                assert!(
                    bg_ok,
                    "whitespace span resets the bg, would bleed through modal: {:?}",
                    span,
                );
            }
        }
    }

    #[test]
    fn code_block_lines_fill_to_body_width() {
        // The Code block and Diagrams sections should pad each row out
        // with `code_block_text` so the surface background fills the
        // modal body — matching the actual renderer's behaviour.  All
        // padded code-block rows must be the same width as the widest
        // non-code-block row (the modal sizes itself to that width).
        let theme = Theme::default();
        let lines = body_lines(&theme);

        // Find rows that have a span styled with code_block_border /
        // code_block_lang / code_block_text — i.e. the code-block rows.
        let is_code_block_line = |line: &Line<'_>| {
            line.spans.iter().any(|s| {
                s.style == theme.code_block_border
                    || s.style == theme.code_block_lang
                    || s.style == theme.code_block_text
            })
        };

        let max_other = lines
            .iter()
            .filter(|l| !is_code_block_line(l))
            .map(|l| l.width())
            .max()
            .unwrap();

        let code_lines: Vec<&Line<'_>> = lines.iter().filter(|l| is_code_block_line(l)).collect();
        assert!(!code_lines.is_empty(), "expected code-block rows present");
        for line in &code_lines {
            assert_eq!(
                line.width(),
                max_other,
                "code-block row not padded to body width: {:?}",
                line,
            );
            // The trailing span on each padded row should be the surface
            // fill; otherwise the right-hand columns won't show the code
            // background.
            let last = line.spans.last().expect("non-empty code-block row");
            assert!(
                last.content.chars().all(char::is_whitespace),
                "padded row should end in a whitespace fill span: {:?}",
                line,
            );
            assert!(
                last.style == theme.code_block_text || last.style == theme.code_block_lang,
                "trailing fill should use a code-block surface style: {:?}",
                line,
            );
        }
    }

    fn find_span<'a>(lines: &'a [Line<'a>], needle: &str) -> Option<&'a Span<'a>> {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content == needle)
    }
}
