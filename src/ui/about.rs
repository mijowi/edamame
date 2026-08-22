//! About-page body content — surfaced via the command palette entry
//! `About edamame`.
//!
//! Pure content builder in the [`super::markdown_cheat_sheet`] mold:
//! the modal adapter (`crate::app::modal::about`) owns the timing and
//! calls [`body_lines`] each frame with plain values, so this module
//! stays free of any `app`-layer dependency and is testable as a
//! function of its inputs.  [`super::update_check`] follows the same
//! rule for the release-status body that used to live here.
//!
//! Spans with no domain styling use [`Span::raw`] / `Line::raw` so they
//! inherit the modal `Paragraph`'s background — see the cheat sheet
//! module for why `theme.normal` would be wrong here.
//!
//! Layout invariant: the body's max line width and its line count are
//! identical for every tagline rotation, so the modal frame never
//! resizes while open.  The taglines are word-wrapped here rather than
//! left to the `Paragraph` wrap, which keeps the content width narrow
//! enough for an 80-column terminal with room to spare, and every
//! rotation is padded out to the row count the *longest* tagline needs
//! at that width.
//!
//! The body is also built for the width it will be shown at, because
//! both halves of it break badly when the terminal is narrower than
//! they are: the pod is an ASCII block whose shape survives nothing,
//! and centring pads every row to the content width, which the
//! `Paragraph` then wraps into a ragged second row.  [`body_lines`]
//! therefore takes the columns available to it, drops the art when it
//! does not fit, and never pads a row past that width.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::config::Theme;

/// Rotating expansions of the e.d.a.m.a.m.e. acronym.  The modal flips
/// to the next entry every few seconds while the About page is open.
pub const TAGLINES: &[&str] = &[
    "Enough Developers Are Making A Markdown Editor",
    "Engineered Despite Already Many Available Markdown Editors",
    "Enterprise-Driven Architecture for Markdown Authoring/Management System",
    "Ergonomic, Dependency-Averse Markdown Authoring Made Easy",
    "'Ello, Doesn't Anyone's Mum 'Ave More Eggs",
    "Embarrassingly Duplicative, Absolutely Mediocre, Amateur Markdown Editor",
    "Exceptionally Derivative And Mostly Awful Markdown Editor",
    "Error: Data Already Mangled — Another Markdown Editor",
    "Eventually, Developers All Make A Markdown Editor",
    "Every Damn Agent's Markdown Annoys Me Endlessly",
    "Even Dad And Mom Are Markdown Editing",
    "Extremely Disappointed At Many Aggravating Markdown Editors",
    "Ego Demands Acrimoniously Making A Markdown Editor",
];

/// A shaded edamame pod: stem at the top, three bulging beans with
/// pinched necks between them, leaning top-right to bottom-left with
/// a gentle curve.  Shading runs `. : - = +` from the lit left edge
/// to the shadowed right.  Centered as a block (every row padded to
/// the art's own width first) so per-line centering can't shear the
/// shape.
const ART: &[&str] = &[
    r",o;",
    r" ,H.",
    r" .lc.",
    r" .l:c.",
    r" 'lll-;+",
    r" 'llll--:,..",
    r" ;OOclll::;+,.",
    r"  OCCCC;;;;::+,",
    r"  'CCCluucc;;;:;",
    r"   'CcCllll;;;:::,._",
    r"    ';cCCCcuuu;;;:;;:',_",
    r"      'clllllclluc;;;::'..",
    r"        'cclCllluucc;;;:::,,,.._",
    r"           'CCllllooocc;;;;;:::::+..",
    r"            'CCCccllCcloouucc;;;::::+',",
    r"               'CCCccllooooc:ooooucc;;;:++:,.",
    r"                   'OCCcclccclllllccuuu;:,.",
    r"                        ''c:;:ccc:wq;'''",
];

/// The pod is always edamame-green, independent of the active theme's
/// palette.
const ART_STYLE: Style = Style::new().fg(Color::Green);

const TITLE: &str = "e.d.a.m.a.m.e.";
const SUBTITLE: &str = "A Markdown editor";
const AUTHOR: &str = "Created by mijowi";

/// Preferred wrap width for the rotating tagline.  Chosen so the widest
/// version row (not the taglines) decides the content width, and the
/// modal stays comfortably inside an 80-column terminal.  A narrower
/// terminal wraps them tighter.
const TAGLINE_WRAP: usize = 44;

/// Display width of the pod, and so the narrowest body that can show it.
fn art_width() -> usize {
    ART.iter().map(|l| l.width()).max().unwrap_or(0)
}

/// Build the About body for a body area of `avail` columns.
/// `tagline_idx` is a free-running counter (wrapped here, so callers
/// pass raw tick counts); `installed` is the bare Cargo version
/// (`0.1.0`).
///
/// The pod is dropped when `avail` cannot hold it: it is one block of
/// ASCII, so a wrap does not shorten it, it shears it — and a sheared
/// pod is worse than no pod.  Nothing else here has that property, so
/// nothing else is dropped; the remaining rows simply centre in a
/// narrower column.
///
/// Release information deliberately does not appear on this page — see
/// `crate::app::modal::about` for why it moved to its own modal.
pub fn body_lines(
    theme: &Theme,
    tagline_idx: usize,
    installed: &str,
    avail: u16,
) -> Vec<Line<'static>> {
    let tagline = TAGLINES[tagline_idx % TAGLINES.len()];
    let avail = (avail as usize).max(1);

    // The labelled form where it fits, the bare number where it does
    // not: this is the one row that must survive any width, since it is
    // the only fact on the page a user opens the About box to check.
    let labelled = format!("Installed version: v{installed}");
    let installed_row = if labelled.width() <= avail {
        labelled
    } else {
        format!("v{installed}")
    };
    let version_width = installed_row.width();

    let art_width = art_width();
    let show_art = art_width <= avail;

    // One stable content width, so the frame doesn't resize as the
    // tagline rotates: the widest row that is not itself a tagline,
    // capped at what the terminal can actually show.
    let natural = TAGLINE_WRAP
        .max(if show_art { art_width } else { 0 })
        .max(TITLE.width())
        .max(SUBTITLE.width())
        .max(version_width)
        .max(AUTHOR.width());
    let width = natural.min(avail);
    let tagline_wrap = TAGLINE_WRAP.min(width);

    let mut out: Vec<Line<'static>> = Vec::new();
    if show_art {
        for art_line in ART {
            // Pad to the art block's own width first so every row gets
            // the same centering offset and the pod keeps its shape.
            out.push(centered(
                Span::styled(pad_to((*art_line).to_owned(), art_width), ART_STYLE),
                width,
            ));
        }
        out.push(Line::raw(""));
    }
    out.push(centered(Span::styled(TITLE, theme.h1), width));
    out.push(centered(Span::raw(SUBTITLE), width));
    out.push(Line::raw(""));
    let mut tagline_rows = wrap_words(tagline, tagline_wrap);
    tagline_rows.resize(tagline_rows_at(tagline_wrap), String::new());
    for row in tagline_rows {
        out.push(centered(Span::styled(row, theme.modal_description), width));
    }
    out.push(Line::raw(""));
    out.push(centered(
        Span::raw(pad_to(installed_row, version_width.min(width))),
        width,
    ));
    out.push(Line::raw(""));
    out.push(centered(Span::styled(AUTHOR, theme.text_muted()), width));
    out
}

/// Rows every tagline is padded out to at `wrap` columns: what the
/// longest one needs.  Derived rather than fixed at two, because a
/// narrow terminal wraps the long expansions onto a third and fourth
/// row and the modal must not change height as they rotate.
fn tagline_rows_at(wrap: usize) -> usize {
    TAGLINES
        .iter()
        .map(|t| wrap_words(t, wrap).len())
        .max()
        .unwrap_or(1)
}

/// Greedy word-wrap of `text` into rows of at most `width` cells.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.width() + 1 + word.width() > width {
            rows.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        rows.push(current);
    }
    rows
}

/// Right-pad `s` with spaces to display-width `width`.
fn pad_to(mut s: String, width: usize) -> String {
    let pad = width.saturating_sub(s.width());
    s.extend(std::iter::repeat_n(' ', pad));
    s
}

/// Center `span` in a `width`-cell line by padding both sides with raw
/// spaces.  Padding both sides (not just the left) keeps every line at
/// the full content width, so the widest line — and with it the modal
/// frame — never changes as the tagline rotates.
fn centered(span: Span<'static>, width: usize) -> Line<'static> {
    let pad = width.saturating_sub(span.width());
    let left = pad / 2;
    Line::from(vec![
        Span::raw(" ".repeat(left)),
        span,
        Span::raw(" ".repeat(pad - left)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn flat(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `flat` with all whitespace (including the wrap-induced line
    /// breaks) collapsed to single spaces, for matching taglines that
    /// may span two rows.
    fn normalized(lines: &[Line<'_>]) -> String {
        flat(lines).split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// A terminal wide enough for everything, so a test that is not
    /// about narrow layouts doesn't have to pick a number.
    const WIDE: u16 = 100;

    #[test]
    fn tagline_index_selects_and_wraps() {
        let body = normalized(&body_lines(theme(), 4, "0.1.0", WIDE));
        assert!(body.contains(TAGLINES[4]), "{body}");
        let wrapped = normalized(&body_lines(theme(), TAGLINES.len() + 4, "0.1.0", WIDE));
        assert!(wrapped.contains(TAGLINES[4]), "{wrapped}");
        // A tagline longer than the wrap width still appears whole.
        let long = normalized(&body_lines(theme(), 2, "0.1.0", WIDE));
        assert!(long.contains(TAGLINES[2]), "{long}");
    }

    #[test]
    fn a_narrow_terminal_drops_the_pod_rather_than_shearing_it() {
        // One column short of the pod is enough: it is a block, so it
        // has no useful partial form.
        let art = art_width() as u16;
        let with = body_lines(theme(), 0, "0.1.0", art);
        let without = body_lines(theme(), 0, "0.1.0", art - 1);
        assert!(flat(&with).contains("OCCCC"));
        assert!(!flat(&without).contains("OCCCC"));
        // …and what is left still says what the page is for.
        let text = normalized(&without);
        assert!(text.contains(TITLE), "{text}");
        assert!(text.contains("Installed version: v0.1.0"), "{text}");
    }

    #[test]
    fn no_row_is_padded_past_the_width_it_was_built_for() {
        // Centring pads both sides, so a body built for more columns
        // than it gets is wrapped by the `Paragraph` into a ragged
        // second row — the bug that made the page unreadable narrow.
        for avail in [20u16, 30, 46, 60] {
            let body = body_lines(theme(), 2, "0.1.0", avail);
            let max = body.iter().map(|l| l.width()).max().unwrap();
            assert!(max <= avail as usize, "{max} > {avail}");
        }
    }

    #[test]
    fn the_size_stays_stable_across_rotations_at_every_width() {
        // The height guarantee has to survive a narrow wrap too: a long
        // expansion needs more rows there, so every rotation is padded
        // to the longest one's count rather than a fixed two.
        for avail in [24u16, 40, 60, WIDE] {
            let sizes: Vec<(usize, usize)> = (0..TAGLINES.len())
                .map(|i| {
                    let body = body_lines(theme(), i, "0.1.0", avail);
                    (body.iter().map(|l| l.width()).max().unwrap(), body.len())
                })
                .collect();
            assert!(sizes.windows(2).all(|w| w[0] == w[1]), "{avail}: {sizes:?}");
        }
    }

    #[test]
    fn every_rotation_keeps_the_same_size() {
        // The modal frame is sized from the widest body line and the
        // row count; if either varied across taglines the About box
        // would resize every flip.
        let sizes: Vec<(usize, usize)> = (0..TAGLINES.len())
            .map(|i| {
                let body = body_lines(theme(), i, "0.1.0", WIDE);
                (body.iter().map(|l| l.width()).max().unwrap(), body.len())
            })
            .collect();
        assert!(sizes.windows(2).all(|w| w[0] == w[1]), "{sizes:?}");
    }

    #[test]
    fn content_fits_an_80_column_terminal() {
        // Widest body line + the modal chrome's horizontal padding
        // must stay inside a standard 80-column terminal, otherwise
        // the body wraps and the centering shears (the original bug).
        let body = body_lines(theme(), 2, "0.1.0", WIDE);
        let max = body.iter().map(|l| l.width()).max().unwrap();
        assert!(max <= 72, "body width {max} leaves no room for chrome");
    }

    #[test]
    fn art_block_keeps_its_shape_when_centered() {
        // Every art row must receive the same left offset — per-row
        // centering would shear the pod.
        let body = body_lines(theme(), 0, "0.1.0", WIDE);
        let offsets: Vec<usize> = body[..ART.len()]
            .iter()
            .map(|l| l.spans[0].content.len())
            .collect();
        assert!(offsets.windows(2).all(|w| w[0] == w[1]), "{offsets:?}");
    }
}
