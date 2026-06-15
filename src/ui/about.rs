//! About-page body content — surfaced via the command palette entry
//! `About edamame`.
//!
//! Pure content builder in the [`super::markdown_cheat_sheet`] mold:
//! the modal adapter (`crate::app::modal::about`) owns the timing and
//! release-check state and calls [`body_lines`] each frame with plain
//! values, so this module stays free of any `app`-layer dependency and
//! is testable as a function of its inputs.
//!
//! Spans with no domain styling use [`Span::raw`] / `Line::raw` so they
//! inherit the modal `Paragraph`'s background — see the cheat sheet
//! module for why `theme.normal` would be wrong here.
//!
//! Layout invariant: the body's max line width and its line count are
//! identical for every tagline rotation and both spinner/resolved
//! release states, so the modal frame never resizes while open.  The
//! taglines are word-wrapped here (to [`TAGLINE_WRAP`], always emitting
//! [`TAGLINE_ROWS`] rows) rather than left to the `Paragraph` wrap,
//! which keeps the content width narrow enough for an 80-column
//! terminal with room to spare.

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

/// Braille spinner shown on the "Current release" row while the
/// GitHub release check is in flight.
pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
const AUTHOR: &str = "Created by gorgonian";

/// Wrap width for the rotating tagline.  Chosen so the widest version
/// row (not the taglines) decides the content width, and the modal
/// stays comfortably inside an 80-column terminal.
const TAGLINE_WRAP: usize = 44;

/// Every tagline renders as exactly this many rows (short ones get a
/// trailing blank) so the modal height is stable across rotations.
const TAGLINE_ROWS: usize = 2;

/// Build the About body.  `tagline_idx` and `spinner_frame` are free-
/// running counters (wrapped here, so callers pass raw tick counts);
/// `installed` is the bare Cargo version (`0.1.0`); `release` is the
/// resolved "Current release" text, or `None` while the fetch is still
/// in flight (renders the spinner).
pub fn body_lines(
    theme: &Theme,
    tagline_idx: usize,
    spinner_frame: usize,
    installed: &str,
    release: Option<&str>,
) -> Vec<Line<'static>> {
    let tagline = TAGLINES[tagline_idx % TAGLINES.len()];

    // Version rows, padded to a common width so their left edges stay
    // aligned once each row is centered independently.
    let installed_row = format!("Installed version: v{installed}");
    let release_row = match release {
        Some(text) => format!("Current release:   {text}"),
        None => format!(
            "Current release:   {}",
            SPINNER_FRAMES[spinner_frame % SPINNER_FRAMES.len()]
        ),
    };
    let version_width = installed_row.width().max(release_row.width());

    // Fixed content width: taglines wrap at TAGLINE_WRAP, so the
    // widest static row anchors the modal at one stable size instead
    // of resizing on every rotation.
    let art_width = ART.iter().map(|l| l.width()).max().unwrap_or(0);
    let width = TAGLINE_WRAP
        .max(art_width)
        .max(TITLE.width())
        .max(SUBTITLE.width())
        .max(version_width)
        .max(AUTHOR.width());

    let mut out: Vec<Line<'static>> = Vec::new();
    for art_line in ART {
        // Pad to the art block's own width first so every row gets the
        // same centering offset and the pod keeps its shape.
        out.push(centered(
            Span::styled(pad_to((*art_line).to_owned(), art_width), ART_STYLE),
            width,
        ));
    }
    out.push(Line::raw(""));
    out.push(centered(Span::styled(TITLE, theme.h1), width));
    out.push(centered(Span::raw(SUBTITLE), width));
    out.push(Line::raw(""));
    let mut tagline_rows = wrap_words(tagline, TAGLINE_WRAP);
    tagline_rows.resize(TAGLINE_ROWS, String::new());
    for row in tagline_rows {
        out.push(centered(Span::styled(row, theme.modal_description), width));
    }
    out.push(Line::raw(""));
    out.push(centered(
        Span::raw(pad_to(installed_row, version_width)),
        width,
    ));
    out.push(centered(
        Span::raw(pad_to(release_row, version_width)),
        width,
    ));
    out.push(Line::raw(""));
    out.push(centered(Span::styled(AUTHOR, theme.text_muted()), width));
    out
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

    #[test]
    fn pending_release_renders_spinner_frame() {
        let body = flat(&body_lines(theme(), 0, 2, "0.1.0", None));
        assert!(body.contains(SPINNER_FRAMES[2]), "{body}");
        // The spinner index wraps rather than panicking.
        let body = flat(&body_lines(
            theme(),
            0,
            SPINNER_FRAMES.len() + 1,
            "0.1.0",
            None,
        ));
        assert!(body.contains(SPINNER_FRAMES[1]), "{body}");
    }

    #[test]
    fn tagline_index_selects_and_wraps() {
        let body = normalized(&body_lines(theme(), 4, 0, "0.1.0", None));
        assert!(body.contains(TAGLINES[4]), "{body}");
        let wrapped = normalized(&body_lines(theme(), TAGLINES.len() + 4, 0, "0.1.0", None));
        assert!(wrapped.contains(TAGLINES[4]), "{wrapped}");
        // A tagline longer than the wrap width still appears whole.
        let long = normalized(&body_lines(theme(), 2, 0, "0.1.0", None));
        assert!(long.contains(TAGLINES[2]), "{long}");
    }

    #[test]
    fn every_rotation_keeps_the_same_size() {
        // The modal frame is sized from the widest body line and the
        // row count; if either varied across taglines the About box
        // would resize every flip.
        let sizes: Vec<(usize, usize)> = (0..TAGLINES.len())
            .map(|i| {
                let body = body_lines(theme(), i, 0, "0.1.0", None);
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
        let body = body_lines(theme(), 2, 0, "0.1.0", Some("v10.20.30 (update available)"));
        let max = body.iter().map(|l| l.width()).max().unwrap();
        assert!(max <= 72, "body width {max} leaves no room for chrome");
    }

    #[test]
    fn art_block_keeps_its_shape_when_centered() {
        // Every art row must receive the same left offset — per-row
        // centering would shear the pod.
        let body = body_lines(theme(), 0, 0, "0.1.0", None);
        let offsets: Vec<usize> = body[..ART.len()]
            .iter()
            .map(|l| l.spans[0].content.len())
            .collect();
        assert!(offsets.windows(2).all(|w| w[0] == w[1]), "{offsets:?}");
    }
}
