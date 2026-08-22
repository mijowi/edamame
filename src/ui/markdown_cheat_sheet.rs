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
//! Tables are intentionally absent: they have a dedicated insert/edit
//! flow so hand-coding the pipe-grid form is rarely useful.  Footnotes
//! ARE listed — references and definitions are rendered and navigable.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::config::Theme;

/// Columns every example row is indented by, and so the margin a block
/// surface keeps on *both* sides of the body — see [`pad_surface_lines`].
const EXAMPLE_INDENT: usize = 2;

/// Build the styled cheat-sheet body for a body area of `avail`
/// columns, one logical row per line.  Returned `Line`s carry
/// theme-driven styling so the popover looks like preview/rendered mode
/// while still showing the raw Markdown syntax markers.
///
/// `avail` reaches only the surface rows (see [`pad_surface_lines`]).
/// Everything else is *content* and wraps like any other modal body: a
/// wrapped example is still readable, and clipping one would hide the
/// syntax the sheet exists to show.
pub fn body_lines(theme: &Theme, avail: u16) -> Vec<Line<'static>> {
    let (mut lines, surface_pad) = build(theme);
    pad_surface_lines(&mut lines, &surface_pad, avail);
    lines
}

/// The rows, plus the indices of the ones carrying a block surface and
/// the fill style each one's pad takes.  Split out from [`body_lines`]
/// so the width-dependent pass is separable from the content — and so a
/// test can tell a wash row from a content row that merely happens to
/// end in a coloured span (`==highlight==` does).
#[allow(clippy::vec_init_then_push)] // grouped pushes mirror the on-screen sections
fn build(theme: &Theme) -> (Vec<Line<'static>>, Vec<(usize, Style)>) {
    let mut out: Vec<Line<'static>> = Vec::new();
    // Indices of lines that carry a block *surface* — a background the row
    // is meant to be washed with rather than a color on its glyphs — paired
    // with the fill style their trailing pad should use.  A fenced-code or
    // Mermaid language row uses `code_block_lang` (lighter `surface` bg),
    // body and closing-fence rows `code_block_text` (darker `muted` bg),
    // mirroring the actual renderer, which paints the lang label on a
    // lighter surface than the body; a block-quote row uses the quote wash.
    // A trailing-padding pass at the end fills the modal body width, so each
    // surface reads as one rectangle instead of ending at its own text.
    let mut surface_pad: Vec<(usize, Style)> = Vec::new();

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

    // ── Footnotes ─────────────────────────────────────────────────────
    out.push(section(theme, "Footnotes"));
    out.push(Line::from(vec![
        Span::raw("  Reference a note"),
        Span::styled("[^1]", theme.footnote),
    ]));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("[^1]:", theme.footnote),
        Span::raw(" The footnote definition (written anywhere)."),
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
    // Both rows register for the trailing-pad pass: `blockquote_text` is a
    // background wash, so without it the two rows paint ragged rectangles of
    // different widths — and one derived from the editor `bg` rather than
    // the modal's `surface_elevated`, which makes the mismatch obvious.
    surface_pad.push((out.len(), theme.blockquote_text));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(">", theme.blockquote_bar),
        Span::styled(" quoted text spans", theme.blockquote_text),
    ]));
    surface_pad.push((out.len(), theme.blockquote_text));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(">", theme.blockquote_bar),
        Span::styled(" multiple lines.", theme.blockquote_text),
    ]));
    out.push(blank());

    // ── Code block ────────────────────────────────────────────────────
    out.push(section(theme, "Code block"));
    surface_pad.push((out.len(), theme.code_block_lang));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("```", theme.code_block_lang),
        Span::styled("rust", theme.code_block_lang),
    ]));
    surface_pad.push((out.len(), theme.code_block_text));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("fn main() {}", theme.code_block_text),
    ]));
    surface_pad.push((out.len(), theme.code_block_text));
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

    // ── Frontmatter ───────────────────────────────────────────────────
    out.push(section(theme, "Frontmatter"));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("---", theme.frontmatter_delimiter),
    ]));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("title:", theme.frontmatter_key),
        Span::styled(" My post", theme.frontmatter_value),
    ]));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("---", theme.frontmatter_delimiter),
    ]));
    out.push(Line::raw("  (or +++ … +++ for TOML)"));
    out.push(blank());

    // ── Hard line break ───────────────────────────────────────────────
    out.push(section(theme, "Hard line break"));
    // Both forms are CommonMark hard breaks and both export as `<br />`.
    // The qualifier is deliberate: `Renderer::render_paragraph` splits at
    // soft breaks as well, so on screen every source line already gets its
    // own row and the markers change nothing — they matter on export.
    out.push(Line::raw(
        "  Two spaces at end of line  ⏎   or a trailing  \\",
    ));
    out.push(Line::raw("  (this matters for exported HTML)"));
    out.push(blank());

    // ── Diagrams (Mermaid) ────────────────────────────────────────────
    out.push(section(theme, "Diagrams (Mermaid)"));
    surface_pad.push((out.len(), theme.code_block_lang));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("```", theme.code_block_lang),
        Span::styled("mermaid", theme.code_block_lang),
    ]));
    surface_pad.push((out.len(), theme.code_block_text));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("graph TD; A-->B;", theme.code_block_text),
    ]));
    surface_pad.push((out.len(), theme.code_block_text));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("```", theme.code_block_text),
    ]));

    (out, surface_pad)
}

/// Size each surface-carrying line to exactly the body width, so its
/// background reads as one rectangle — matching how
/// `Renderer::render_code_block` pads to `viewport_width` and how
/// `line_render`'s trailing-cell fill extends a block quote's wash to the
/// viewport edge.  Each entry pairs a row index with the fill style for
/// that row's pad — a code block's language row uses `code_block_lang`
/// (lighter), its body / fence rows `code_block_text` (darker), a quote row
/// `blockquote_text`.
///
/// The width is the widest *unregistered* row — which is what the modal
/// sizes itself to — but never more than the `avail` columns the body
/// will actually get.  A wash wider than that is the one row here that
/// must not wrap: it is a picture of a block, and a wrapped one repeats
/// its surface on a second, ragged row instead of continuing anything.
///
/// It stops [`EXAMPLE_INDENT`] columns short of that width, which is
/// where it *starts*: every example row on the sheet is indented by
/// that much, so a wash running flush to the body's right edge is a
/// rectangle with a margin down one side and none down the other —
/// visibly crowding the frame however much padding the modal has.
fn pad_surface_lines(lines: &mut [Line<'static>], surface_pad: &[(usize, Style)], avail: u16) {
    // A mask rather than a `contains` scan per row: this runs on every
    // frame, and both sequences are the length of the whole sheet.
    let mut is_surface = vec![false; lines.len()];
    for &(i, _) in surface_pad {
        is_surface[i] = true;
    }
    let natural: usize = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| !is_surface[*i])
        .map(|(_, l)| l.width())
        .max()
        .unwrap_or(0);
    let target_width = natural.min(avail as usize).saturating_sub(EXAMPLE_INDENT);

    for &(i, fill_style) in surface_pad {
        let line = &mut lines[i];
        let cur = line.width();
        match cur.cmp(&target_width) {
            std::cmp::Ordering::Less => line
                .spans
                .push(Span::styled(" ".repeat(target_width - cur), fill_style)),
            // Only reachable in a terminal too narrow for the example's
            // own text.  Truncating keeps the rectangle honest; the row
            // is a picture, so a lost tail costs less than a wrap.
            std::cmp::Ordering::Greater => truncate_line(line, target_width),
            std::cmp::Ordering::Equal => {}
        }
    }
}

/// Drop whatever of `line` sits past `width` display columns, span by
/// span.  A span straddling the boundary is cut on a char boundary.
fn truncate_line(line: &mut Line<'static>, width: usize) {
    let mut used = 0;
    let mut kept: Vec<Span<'static>> = Vec::with_capacity(line.spans.len());
    for span in line.spans.drain(..) {
        let w = span.content.width();
        if used + w <= width {
            used += w;
            kept.push(span);
            continue;
        }
        let room = width - used;
        if room > 0 {
            let mut text = String::new();
            let mut text_w = 0;
            for ch in span.content.chars() {
                let cw = ch.width().unwrap_or(0);
                if text_w + cw > room {
                    break;
                }
                text_w += cw;
                text.push(ch);
            }
            kept.push(Span::styled(text, span.style));
        }
        break;
    }
    line.spans = kept;
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

    /// A body area wide enough for the sheet's own natural width, so a
    /// test about content is not also a test about narrow layout.
    const WIDE: u16 = 120;

    fn joined(theme: &Theme) -> String {
        body_lines(theme, WIDE)
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
        assert!(s.contains("Footnotes"));
        assert!(s.contains("[^1]"));
        // Header anchor + local file + http examples in the Links section.
        assert!(s.contains("#heading-anchor"));
        assert!(s.contains("./notes.md"));
        assert!(s.contains("https://example.com"));
    }

    #[test]
    fn cheat_sheet_excludes_tables_but_includes_footnotes() {
        // Tables are surfaced through dedicated editing flows, not
        // hand-coded markdown, so they stay out of the sheet.  Footnotes
        // ARE rendered and navigable now, so they must appear.
        let theme = Theme::default();
        let s = joined(&theme);
        assert!(
            !s.contains("Tables"),
            "Tables should not appear in the cheat sheet"
        );
        assert!(s.contains("Footnotes"), "Footnotes section should appear");
        assert!(s.contains("[^"), "footnote markers should appear");
    }

    #[test]
    fn cheat_sheet_footnote_markers_use_footnote_style() {
        let theme = Theme::default();
        let lines = body_lines(&theme, WIDE);
        let marker = find_span(&lines, "[^1]").expect("footnote reference span");
        assert_eq!(marker.style, theme.footnote);
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

        let lines_a = body_lines(&a, WIDE);
        let lines_b = body_lines(&b, WIDE);
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
        let lines = body_lines(&theme, WIDE);
        let headings_label = find_span(&lines, "Headings").expect("Headings label");
        assert_eq!(headings_label.style, theme.modal_section_heading);
    }

    /// The rows carrying a block surface, at `avail` columns.
    fn surface_rows(avail: u16) -> Vec<Line<'static>> {
        let theme = Theme::default();
        let (_, surface_pad) = build(&theme);
        let lines = body_lines(&theme, avail);
        surface_pad.iter().map(|&(i, _)| lines[i].clone()).collect()
    }

    #[test]
    fn a_wash_never_exceeds_the_body_it_is_drawn_in() {
        // A wash is a picture of a block, so a wrap would repeat its
        // surface on a second ragged row rather than continue it.  It is
        // capped at the body width instead — and because that width is
        // the *padded* body, the wash stops inside the modal's padding
        // rather than at the frame edge.
        for avail in [20u16, 40, 64, 120] {
            for line in surface_rows(avail) {
                assert!(
                    line.width() <= avail as usize,
                    "{} > {avail}: {line:?}",
                    line.width()
                );
            }
        }
    }

    #[test]
    fn a_wash_keeps_the_same_margin_on_both_sides() {
        // It begins at the example indent, so it ends that far from the
        // body's right edge; flush against it, the rectangle reads as
        // crowding the frame no matter what the modal's padding is.
        let avail = 40;
        for line in surface_rows(avail) {
            assert_eq!(line.width(), avail as usize - EXAMPLE_INDENT, "{line:?}");
        }
    }

    #[test]
    fn only_the_washes_are_capped_and_the_content_still_wraps() {
        // The regression this guards: clipping the whole body to keep
        // the washes intact also truncated every example on the sheet,
        // which is the syntax it exists to show.  Content rows keep
        // their full width and let the modal wrap them.
        let theme = Theme::default();
        let (_, surface_pad) = build(&theme);
        let surfaces: Vec<usize> = surface_pad.iter().map(|&(i, _)| i).collect();
        let widest_content = body_lines(&theme, 40)
            .iter()
            .enumerate()
            .filter(|(i, _)| !surfaces.contains(i))
            .map(|(_, l)| l.width())
            .max()
            .unwrap();
        assert!(
            widest_content > 40,
            "content was truncated: {widest_content}"
        );
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
        let lines = body_lines(&theme, WIDE);
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
        let lines = body_lines(&theme, WIDE);

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
                max_other - EXAMPLE_INDENT,
                "code-block row not padded to the body width less its \
                 right-hand margin: {:?}",
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

    /// `blockquote_text` is a background wash, so the two quote rows need the
    /// same trailing-pad pass the code-block rows get.  Without it each row's
    /// wash stops at its own text — two ragged rectangles of different widths,
    /// in a color derived from the editor `bg` rather than the modal's
    /// `surface_elevated`, which is what makes the mismatch visible.
    #[test]
    fn block_quote_rows_are_padded_to_the_body_width() {
        let theme = Theme::default();
        let lines = body_lines(&theme, WIDE);

        let is_quote_line =
            |line: &Line<'_>| line.spans.iter().any(|s| s.style == theme.blockquote_text);
        let quote_lines: Vec<&Line<'_>> = lines.iter().filter(|l| is_quote_line(l)).collect();
        assert_eq!(quote_lines.len(), 2, "expected both block-quote rows");

        let width = quote_lines[0].width();
        for line in &quote_lines {
            assert_eq!(
                line.width(),
                width,
                "block-quote rows must share one width: {line:?}"
            );
            let last = line.spans.last().expect("non-empty quote row");
            assert!(
                last.content.chars().all(char::is_whitespace),
                "padded quote row should end in a whitespace fill span: {line:?}"
            );
            assert_eq!(
                last.style, theme.blockquote_text,
                "trailing fill should carry the quote wash: {line:?}"
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
