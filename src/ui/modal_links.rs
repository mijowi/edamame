//! Inline hyperlinks inside a modal body: how a modal declares one,
//! and the wrap-aware geometry that makes it clickable.
//!
//! ## Why a modal owns its wrap when it carries links
//!
//! [`crate::ui::ModalView`] normally hands its whole body to a
//! `Paragraph` with `Wrap { trim: false }` and lets ratatui reflow it.
//! That is the cheapest correct thing for text nobody has to point at
//! — but ratatui's `WordWrapper` is private to `ratatui-widgets`, and
//! the only public window onto it (`Paragraph::line_count`, behind the
//! `unstable-rendered-line-info` feature, used by
//! [`crate::ui::scroll_container::wrapped_rows`]) answers a *count*.
//! There is no API that reports which screen row and column a given
//! span landed on, which is exactly what a hit-test needs.
//!
//! So a link-bearing modal pre-wraps its own body here, into one
//! [`WrappedRow`] per visual row, and hands `Paragraph` rows that are
//! already cut to width — with no `Wrap` set at all.  Link geometry
//! then falls out of the same pass that produced the rows, rather than
//! being reconstructed by a second walk that could disagree with the
//! first.  This mirrors how the editor already works: `RenderedView`
//! and `PreviewView` wrap through [`crate::ui::line_render`] and
//! hand ratatui one `Line` per physical row, which is what lets
//! [`crate::ui::link_view`] read link rects straight off that list.
//!
//! [`wrap_rows`] is a faithful port of `WordWrapper` specialized to
//! `trim: false` (the only mode modals use), so the two agree row for
//! row; `wrap_matches_ratatui_paragraph_wrapping` in the tests below
//! pins that by rendering the same body both ways and diffing the
//! cells.  Because the paint path sets no `Wrap`, a residual
//! disagreement would clip a trailing character on one row rather than
//! silently shifting a hit-test region onto the wrong text.
//!
//! ## Why link identity is structural, never inferred from style
//!
//! A link is named by its `(line_idx, span_idx)` coordinates into the
//! body, not by "the spans that look like links".  Sniffing for
//! `theme.link_text` or `Modifier::UNDERLINED` would be wrong twice
//! over: [`crate::ui::markdown_cheat_sheet`] styles the *illustrative*
//! snippets `[section](#heading-anchor)` and `[local file](./notes.md)`
//! with `theme.link_text` precisely because they depict links, and
//! `monochrome_dark` spends `UNDERLINED` on H2–H6 as well as on all
//! three link slots.  Either check would hand the user a clickable
//! affordance that resolves to nothing.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::docs::DocId;

/// Where a modal link goes: a section of the shipped manual.
///
/// Deliberately *not* [`crate::editor::link::LinkTarget`], which is
/// the vocabulary of links written inside a document.  Two of its
/// variants are meaningless from a modal — `Anchor` and `Footnote`
/// resolve against whatever document happens to sit behind the
/// overlay — and one is actively wrong: a `LocalFile` naming a manual
/// page (`keybindings.md`) is only redirected to the embedded set
/// while a manual page is *already* open, so from a modal it would
/// resolve against the process's working directory and open whichever
/// `keybindings.md` sits next to the user's shell.  Naming the
/// destination directly removes the possibility.
///
/// **A struct rather than an enum, because there is exactly one kind
/// of destination.**  It was briefly an enum with a second `Url` arm
/// for an external address handed to the OS browser — but no modal
/// ever declared one, so the arm was constructed only by tests.  A
/// modal that needs an external link should add the variant back
/// *with* its caller, and owes [`crate::app::App::follow_modal_link`]
/// a look while doing so: that method's diff-review refusal is
/// specifically about replacing the live document, which handing a URL
/// to the browser does not do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModalLinkTarget {
    /// The page to open.
    pub(crate) id: DocId,
    /// The section within it: a GFM slug, matched exactly — see
    /// [`crate::app::App::heading_line_for_fragment`].
    pub(crate) fragment: Option<&'static str>,
}

/// One hyperlink embedded in a modal's prose body.
///
/// `line_idx` / `span_idx` index the `body: &[Line]` the modal hands
/// [`crate::ui::ModalView`] — the whole span is the link, so a modal
/// that wants a link inside a sentence splits that sentence into three
/// spans and points at the middle one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModalLink {
    /// Index into the body's `Line`s.
    pub(crate) line_idx: usize,
    /// Index into that line's `spans`.
    pub(crate) span_idx: usize,
    /// Where following it goes.
    pub(crate) target: ModalLinkTarget,
    /// The visible text, kept for the flash shown when a link cannot
    /// be followed.
    pub(crate) label: String,
}

impl ModalLink {
    /// Declare the span at `(line_idx, span_idx)` as a link to `target`.
    pub(crate) fn new(
        line_idx: usize,
        span_idx: usize,
        target: ModalLinkTarget,
        label: impl Into<String>,
    ) -> Self {
        Self {
            line_idx,
            span_idx,
            target,
            label: label.into(),
        }
    }
}

/// One visual row of a pre-wrapped modal body.
#[derive(Debug, Clone, Default)]
pub(crate) struct WrappedRow {
    /// The row, already cut to the wrap width and ready to paint with
    /// no further reflow.
    pub(crate) line: Line<'static>,
    /// The body coordinate `(line_idx, span_idx)` behind each *display
    /// column* of `line`, one entry per column so a double-width
    /// grapheme contributes two.  Indexing by column rather than by
    /// grapheme is what lets [`link_rects`] hand back cell rects
    /// without re-measuring anything.
    pub(crate) origins: Vec<(usize, usize)>,
}

/// One grapheme on its way into a row, tagged with where it came from.
struct Piece {
    symbol: String,
    style: Style,
    origin: (usize, usize),
    width: u16,
}

impl Piece {
    fn is_whitespace(&self) -> bool {
        self.symbol.chars().all(char::is_whitespace)
    }
}

/// Wrap `body` to `width` columns exactly as `Paragraph` with
/// `Wrap { trim: false }` would, keeping each grapheme's origin.
///
/// A port of `ratatui_widgets::reflow::WordWrapper` with the `trim`
/// branches collapsed out (they are all dead at `trim: false`), so the
/// row breaks match ratatui's by construction rather than by
/// coincidence.  A `width` of 0 degrades to one row per body line —
/// the same shape `wrapped_rows` returns there — because there is no
/// sensible wrap and the caller is about to skip painting anyway.
pub(crate) fn wrap_rows(body: &[Line<'_>], width: u16) -> Vec<WrappedRow> {
    if width == 0 {
        return body
            .iter()
            .map(|line| WrappedRow {
                line: owned_line(line),
                origins: Vec::new(),
            })
            .collect();
    }

    let mut out = Vec::new();
    for (line_idx, line) in body.iter().enumerate() {
        wrap_one_line(line, line_idx, width, &mut out);
    }
    // `Paragraph` renders an empty body as nothing; an empty `body`
    // slice must not become a phantom row here either.
    out
}

/// Wrap a single body line, pushing one [`WrappedRow`] per visual row.
///
/// Mirrors `WordWrapper::process_input` for `trim == false`: a word is
/// flushed to the pending row when whitespace ends it or when the word
/// plus its leading whitespace cannot fit on a row of its own, and the
/// pending row is emitted once it reaches the width.  Trailing
/// whitespace that would spill past the row edge is dropped, which is
/// why a wrapped paragraph does not accumulate a ragged right margin.
fn wrap_one_line(line: &Line<'_>, line_idx: usize, width: u16, out: &mut Vec<WrappedRow>) {
    let mut pending_row: Vec<Piece> = Vec::new();
    let mut pending_word: Vec<Piece> = Vec::new();
    let mut pending_ws: std::collections::VecDeque<Piece> = std::collections::VecDeque::new();
    let mut row_width: u16 = 0;
    let mut word_width: u16 = 0;
    let mut ws_width: u16 = 0;
    let mut non_ws_previous = false;
    let start_len = out.len();

    for (span_idx, span) in line.spans.iter().enumerate() {
        for g in span.content.as_ref().graphemes(true) {
            let piece = Piece {
                symbol: g.to_owned(),
                // `Line::styled_graphemes` — which is what `Paragraph`
                // feeds `WordWrapper` — resolves each grapheme as
                // `line.style.patch(span.style)`, so the line-level
                // style has to be folded in here or a body line that
                // carries one loses it the moment the modal declares a
                // link.  A no-op for every modal today: none sets one.
                style: line.style.patch(span.style),
                origin: (line_idx, span_idx),
                width: UnicodeWidthStr::width(g) as u16,
            };
            // Ratatui skips a grapheme too wide to ever fit; without
            // the same skip the loop below could never drain it.
            if piece.width > width {
                continue;
            }
            let is_ws = piece.is_whitespace();
            let word_found = non_ws_previous && is_ws;
            let untrimmed_overflow =
                pending_row.is_empty() && word_width + ws_width + piece.width > width;

            if word_found || untrimmed_overflow {
                pending_row.extend(pending_ws.drain(..));
                row_width += ws_width;
                pending_row.append(&mut pending_word);
                row_width += word_width;
                ws_width = 0;
                word_width = 0;
            }

            let row_full = row_width >= width;
            let word_overflow = piece.width > 0 && row_width + ws_width + word_width >= width;
            if row_full || word_overflow {
                let mut remaining = width.saturating_sub(row_width);
                out.push(row_from(std::mem::take(&mut pending_row)));
                row_width = 0;
                // Whitespace that still fits before the edge is
                // consumed rather than carried onto the next row, so a
                // break does not indent the continuation.
                while let Some(front) = pending_ws.front() {
                    if front.width > remaining {
                        break;
                    }
                    ws_width -= front.width;
                    remaining -= front.width;
                    pending_ws.pop_front();
                }
                if is_ws && pending_ws.is_empty() {
                    continue;
                }
            }

            if is_ws {
                ws_width += piece.width;
                pending_ws.push_back(piece);
            } else {
                word_width += piece.width;
                pending_word.push(piece);
            }
            non_ws_previous = !is_ws;
        }
    }

    pending_row.extend(pending_ws.drain(..));
    pending_row.append(&mut pending_word);
    if !pending_row.is_empty() {
        out.push(row_from(pending_row));
    }
    // A body line that produced nothing (empty, or all-whitespace
    // consumed at a break) still occupies one row — blank lines are
    // load-bearing spacing in every modal body.
    if out.len() == start_len {
        out.push(WrappedRow::default());
    }
    // `WordWrapper` carries the source line's alignment onto every row
    // it wrapped into, and `Paragraph`'s no-wrap path (the one the
    // pre-wrapped rows take) honors `Line::alignment` too — so copying
    // it here is what keeps a centred body line centred once a link
    // switches the modal onto this path.  Left alone when the line sets
    // none, which is every modal body today.
    if let Some(alignment) = line.alignment {
        for row in &mut out[start_len..] {
            row.line = std::mem::take(&mut row.line).alignment(alignment);
        }
    }
}

/// Assemble a row from its pieces, merging runs that share a style and
/// an origin so the painted `Line` carries no more spans than it must.
fn row_from(pieces: Vec<Piece>) -> WrappedRow {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut origins: Vec<(usize, usize)> = Vec::new();
    let mut current: Option<(Style, (usize, usize), String)> = None;
    for p in pieces {
        for _ in 0..p.width {
            origins.push(p.origin);
        }
        match &mut current {
            Some((style, origin, text)) if *style == p.style && *origin == p.origin => {
                text.push_str(&p.symbol);
            }
            _ => {
                if let Some((style, _, text)) = current.take() {
                    spans.push(Span::styled(text, style));
                }
                current = Some((p.style, p.origin, p.symbol));
            }
        }
    }
    if let Some((style, _, text)) = current {
        spans.push(Span::styled(text, style));
    }
    WrappedRow {
        line: Line::from(spans),
        origins,
    }
}

/// Deep-copy a borrowed `Line` into an owned one, keeping its own
/// style and alignment alongside its spans' — the same fidelity
/// [`wrap_one_line`] keeps, so the zero-width path is not a second,
/// lossier copy.
fn owned_line(line: &Line<'_>) -> Line<'static> {
    let mut out = Line::from(
        line.spans
            .iter()
            .map(|s| Span::styled(s.content.as_ref().to_owned(), s.style))
            .collect::<Vec<_>>(),
    )
    .style(line.style);
    out.alignment = line.alignment;
    out
}

/// Column the first cell of `row` is painted at, given the width it is
/// painted into.
///
/// [`wrap_one_line`] carries a body line's [`Line::alignment`] onto
/// every row it wrapped into, and `Paragraph`'s no-wrap path — the one
/// the pre-wrapped rows take — honors it.  So a centred or
/// right-aligned row is painted *shifted*, while `origins` is built
/// left-to-right and knows nothing about it.  Without this offset the
/// two halves of this module disagree: the link paints in one place
/// and hit-tests in another.
///
/// `origins` carries one entry per display column, so its length is
/// the row's painted width exactly — no re-measuring.  `None`
/// alignment is Left, because `ModalView` sets none on its body
/// `Paragraph` and `Paragraph`'s own default is Left.
fn row_start_col(row: &WrappedRow, width: usize) -> usize {
    use ratatui::layout::Alignment;
    let row_width = row.origins.len();
    let slack = width.saturating_sub(row_width);
    match row.line.alignment {
        Some(Alignment::Center) => slack / 2,
        Some(Alignment::Right) => slack,
        Some(Alignment::Left) | None => 0,
    }
}

/// Absolute terminal rects for every link visible in the scrolled body.
///
/// One rect per row a link occupies, so a link wrapped across two rows
/// hit-tests on both — the same one-snapshot-per-row model
/// [`crate::ui::link_view`] uses for the editor.  Rows outside
/// `[scroll, scroll + area.height)` contribute nothing, which is what
/// makes a scrolled-away link unclickable without a second check.
///
/// A row's own alignment is honored via [`row_start_col`], so a link
/// declared on a centred or right-aligned body line hit-tests where it
/// is painted rather than where an unaligned row would have put it.
pub(crate) fn link_rects(
    rows: &[WrappedRow],
    links: &[ModalLink],
    area: Rect,
    scroll: u16,
) -> Vec<(usize, Rect)> {
    let mut out = Vec::new();
    if area.height == 0 || area.width == 0 {
        return out;
    }
    let first = scroll as usize;
    let last = first.saturating_add(area.height as usize).min(rows.len());
    for (row_idx, row) in rows.iter().enumerate().take(last).skip(first) {
        let y = area.y + (row_idx - first) as u16;
        let offset = row_start_col(row, area.width as usize);
        for (link_idx, link) in links.iter().enumerate() {
            let key = (link.line_idx, link.span_idx);
            let Some(start) = row.origins.iter().position(|o| *o == key) else {
                continue;
            };
            let end = row.origins.iter().rposition(|o| *o == key).unwrap_or(start);
            let start = start + offset;
            let end = end + offset;
            // Clip to the painted width: a row is cut to the wrap
            // width, but `area` can be narrower still when the body
            // block is centred inside a wider modal.
            if start >= area.width as usize {
                continue;
            }
            let width = (end + 1 - start).min(area.width as usize - start);
            out.push((
                link_idx,
                Rect {
                    x: area.x + start as u16,
                    y,
                    width: width as u16,
                    height: 1,
                },
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::{Paragraph, Widget, Wrap};
    use ratatui::Terminal;

    fn body() -> Vec<Line<'static>> {
        vec![
            Line::raw("The quick brown fox jumps over the lazy dog and keeps running onward"),
            Line::raw(""),
            Line::raw("  indented continuation text that is long enough to wrap at least once"),
            Line::raw("supercalifragilisticexpialidociousandthensomemoretoforceahardsplit"),
            Line::from(vec![
                Span::raw("See "),
                Span::raw("Terminal compatibility"),
                Span::raw(" for the list of terminals."),
            ]),
            // Alignment moves symbols, so the cell diff below checks
            // our carried-over alignment against ratatui's as well as
            // the row breaks.
            Line::raw("a centred paragraph that has to wrap somewhere")
                .alignment(ratatui::layout::Alignment::Center),
        ]
    }

    /// Render `lines` through ratatui's own `Wrap { trim: false }` and
    /// through our pre-wrap, and return both cell grids.
    fn both_renderings(
        lines: &[Line<'static>],
        width: u16,
        height: u16,
    ) -> (Vec<String>, Vec<String>) {
        let cells = |f: &dyn Fn(Rect, &mut ratatui::buffer::Buffer)| -> Vec<String> {
            let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
            term.draw(|frame| {
                let area = Rect::new(0, 0, width, height);
                f(area, frame.buffer_mut());
            })
            .unwrap();
            let buf = term.backend().buffer().clone();
            (0..height)
                .map(|y| {
                    (0..width)
                        .map(|x| buf[(x, y)].symbol().to_owned())
                        .collect::<String>()
                })
                .collect()
        };
        let native = cells(&|area, buf| {
            Paragraph::new(lines.to_vec())
                .wrap(Wrap { trim: false })
                .render(area, buf);
        });
        let ours = cells(&|area, buf| {
            let rows = wrap_rows(lines, area.width);
            let painted: Vec<Line<'static>> = rows.into_iter().map(|r| r.line).collect();
            Paragraph::new(painted).render(area, buf);
        });
        (native, ours)
    }

    #[test]
    fn wrap_matches_ratatui_paragraph_wrapping() {
        // The whole design rests on our port breaking rows exactly
        // where `WordWrapper` does; check it across widths that
        // exercise mid-word splits, whitespace-at-the-edge and the
        // hard-split path.
        for width in [12u16, 17, 20, 31, 40, 79] {
            let (native, ours) = both_renderings(&body(), width, 30);
            assert_eq!(native, ours, "wrap diverged at width {width}");
        }
    }

    /// A body line's own style is folded into every grapheme, exactly
    /// as `Line::styled_graphemes` does for the `Paragraph` path — so
    /// declaring a link cannot quietly strip the styling off a line
    /// that carries one.
    #[test]
    fn a_line_level_style_survives_the_pre_wrap() {
        use ratatui::style::{Color, Modifier};
        let base = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
        let lines = vec![Line::from(vec![
            Span::raw("plain "),
            Span::styled("green", Style::default().fg(Color::Green)),
        ])
        .style(base)];

        let rows = wrap_rows(&lines, 40);
        let spans = &rows[0].line.spans;
        assert_eq!(spans[0].style.fg, Some(Color::Red), "the line's own color");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        // A span's own color still wins; the line supplies the rest.
        assert_eq!(spans[1].style.fg, Some(Color::Green));
        assert!(
            spans[1].style.add_modifier.contains(Modifier::BOLD),
            "the line's modifier reaches a span that sets only a color"
        );
    }

    /// And its alignment: `Paragraph` honors `Line::alignment` on both
    /// paths, so a centred body line must stay centred once a link
    /// switches the modal onto the pre-wrapped one.
    #[test]
    fn a_line_alignment_survives_the_pre_wrap() {
        use ratatui::layout::Alignment;
        let lines = vec![
            Line::raw("alpha bravo charlie delta echo").alignment(Alignment::Center),
            Line::raw("left"),
        ];
        let rows = wrap_rows(&lines, 12);
        assert!(rows.len() > 2, "the first line wrapped");
        for row in &rows[..rows.len() - 1] {
            assert_eq!(
                row.line.alignment,
                Some(Alignment::Center),
                "every row a centred line wrapped into stays centred"
            );
        }
        assert_eq!(rows.last().expect("a row").line.alignment, None);
    }

    #[test]
    fn a_blank_body_line_still_occupies_one_row() {
        let rows = wrap_rows(&[Line::raw("a"), Line::raw(""), Line::raw("b")], 10);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].line.width(), 0);
    }

    #[test]
    fn origins_name_the_span_behind_every_column() {
        let lines = vec![Line::from(vec![Span::raw("ab"), Span::raw("cd")])];
        let rows = wrap_rows(&lines, 10);
        assert_eq!(
            rows[0].origins,
            vec![(0, 0), (0, 0), (0, 1), (0, 1)],
            "each column reports the span it came from"
        );
    }

    #[test]
    fn a_double_width_grapheme_claims_two_columns() {
        let lines = vec![Line::from(vec![Span::raw("東"), Span::raw("x")])];
        let rows = wrap_rows(&lines, 10);
        assert_eq!(rows[0].origins, vec![(0, 0), (0, 0), (0, 1)]);
    }

    #[test]
    fn a_link_rect_covers_exactly_its_span() {
        let lines = vec![Line::from(vec![
            Span::raw("See "),
            Span::raw("the docs"),
            Span::raw(" now"),
        ])];
        let rows = wrap_rows(&lines, 40);
        let links = vec![ModalLink::new(
            0,
            1,
            ModalLinkTarget {
                id: DocId::Index,
                fragment: None,
            },
            "the docs",
        )];
        let rects = link_rects(&rows, &links, Rect::new(3, 5, 40, 4), 0);
        assert_eq!(rects.len(), 1);
        let (idx, r) = rects[0];
        assert_eq!(idx, 0);
        // Body column 4 ("See " is four cells) offset by the area's x.
        assert_eq!((r.x, r.y, r.width, r.height), (3 + 4, 5, 8, 1));
    }

    #[test]
    fn a_wrapped_link_reports_one_rect_per_row() {
        let lines = vec![Line::from(vec![
            Span::raw("x "),
            Span::raw("alpha beta gamma"),
        ])];
        // Narrow enough that the link's own text spans two rows.
        let rows = wrap_rows(&lines, 12);
        let links = vec![ModalLink::new(
            0,
            1,
            ModalLinkTarget {
                id: DocId::Index,
                fragment: None,
            },
            "alpha beta gamma",
        )];
        let rects = link_rects(&rows, &links, Rect::new(0, 0, 12, 6), 0);
        assert!(
            rects.len() >= 2,
            "a link wrapping across rows hit-tests on each: {rects:?}"
        );
        assert!(rects.iter().all(|(i, _)| *i == 0));
    }

    #[test]
    fn a_scrolled_away_link_yields_no_rect() {
        let lines = vec![
            Line::raw("one"),
            Line::raw("two"),
            Line::from(vec![Span::raw("three")]),
        ];
        let rows = wrap_rows(&lines, 20);
        let links = vec![ModalLink::new(
            2,
            0,
            ModalLinkTarget {
                id: DocId::Index,
                fragment: None,
            },
            "three",
        )];
        // A one-row window parked on the first row cannot see it.
        let rects = link_rects(&rows, &links, Rect::new(0, 0, 20, 1), 0);
        assert!(rects.is_empty());
        // Scrolled onto it, it is hit-testable again.
        let rects = link_rects(&rows, &links, Rect::new(0, 0, 20, 1), 2);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].1.y, 0, "the row paints at the top of the window");
    }

    // ── Alignment ─────────────────────────────────────────────────

    /// Paint `lines` the way `ModalView`'s link path does — pre-wrapped
    /// rows through a `Paragraph` with no `Wrap` — and read back the
    /// cells each reported rect covers.
    ///
    /// This is the check that matters for alignment: it compares the
    /// geometry `link_rects` hands the click path against the cells
    /// ratatui actually painted, rather than against a second
    /// derivation of the same arithmetic.
    fn painted_link_text(lines: &[Line<'static>], links: &[ModalLink], width: u16) -> Vec<String> {
        let height = 6u16;
        let area = Rect::new(0, 0, width, height);
        let rows = wrap_rows(lines, width);
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|frame| {
            let painted: Vec<Line<'static>> = rows.iter().map(|r| r.line.clone()).collect();
            Paragraph::new(painted).render(area, frame.buffer_mut());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        link_rects(&rows, links, area, 0)
            .into_iter()
            .map(|(_, r)| {
                (r.x..r.x + r.width)
                    .map(|x| buf[(x, r.y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect()
    }

    /// `wrap_one_line` carries a body line's alignment onto every row it
    /// wrapped into, and `Paragraph`'s no-wrap path honors it — so
    /// `link_rects` has to shift with it.  Reading straight off the
    /// left-to-right `origins` would put a centred link's rect half the
    /// slack to the left of the text it names.
    #[test]
    fn a_centred_link_hit_tests_where_it_is_painted() {
        let lines = vec![Line::from(vec![
            Span::raw("See "),
            Span::raw("the docs"),
            Span::raw(" now"),
        ])
        .alignment(ratatui::layout::Alignment::Center)];
        let links = vec![ModalLink::new(
            0,
            1,
            ModalLinkTarget {
                id: DocId::Index,
                fragment: None,
            },
            "the docs",
        )];
        assert_eq!(painted_link_text(&lines, &links, 40), vec!["the docs"]);
    }

    #[test]
    fn a_right_aligned_link_hit_tests_where_it_is_painted() {
        let lines = vec![Line::from(vec![Span::raw("See "), Span::raw("the docs")])
            .alignment(ratatui::layout::Alignment::Right)];
        let links = vec![ModalLink::new(
            0,
            1,
            ModalLinkTarget {
                id: DocId::Index,
                fragment: None,
            },
            "the docs",
        )];
        assert_eq!(painted_link_text(&lines, &links, 40), vec!["the docs"]);
    }

    /// The unaligned case — every modal body today — must be untouched
    /// by the offset, which is zero for Left and for `None`.
    #[test]
    fn an_unaligned_link_is_unmoved_by_the_alignment_offset() {
        let lines = vec![Line::from(vec![
            Span::raw("See "),
            Span::raw("the docs"),
            Span::raw(" now"),
        ])];
        let links = vec![ModalLink::new(
            0,
            1,
            ModalLinkTarget {
                id: DocId::Index,
                fragment: None,
            },
            "the docs",
        )];
        assert_eq!(painted_link_text(&lines, &links, 40), vec!["the docs"]);
    }
}
