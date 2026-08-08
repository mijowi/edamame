//! Shared terminal-capability summary, rendered both inside the welcome
//! modal's capabilities section and as the entire body of the new-terminal
//! capabilities notice modal.  Captures one `CapRow` per capability the
//! editor cares about, each tagged with an `ok` flag the renderer uses to
//! pick a success/warning style and a ✓/✗ glyph.
//!
//! Rows are **descriptive**: each states what was detected, never what
//! edamame does about it.  The consequence belongs to the consuming modal,
//! because it differs between them — the capabilities notice is purely
//! informational (it only records the terminal fingerprint), while the
//! welcome modal actually writes `images` / `diagrams` on save.  Each owns
//! its own sentence for that (`"Items marked ✗ will be disabled…"` and
//! `welcome::NO_TRUECOLOR_HINT` respectively).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};

use crate::config::Theme;
use crate::terminal::{Capabilities, ColorDepth, ImageProtocol};

/// One row in the capability summary — a label, a human-readable value
/// describing what was detected, and an `ok` flag for styling.
#[derive(Debug, Clone)]
pub struct CapRow {
    pub label: &'static str,
    pub value: String,
    pub ok: bool,
}

/// Pre-built capability summary captured at modal construction.  The
/// `Vec<CapRow>` is in display order; iterate to render rows.
#[derive(Debug, Clone)]
pub struct CapSummary {
    pub rows: Vec<CapRow>,
}

impl CapSummary {
    /// Build the full capability summary from a `Capabilities` snapshot.
    /// Includes color, images, mouse, keyboard enhancement, and unicode —
    /// the five capabilities that meaningfully affect what the editor can
    /// show or how it behaves.
    pub fn from_caps(caps: &Capabilities) -> Self {
        let (color, color_ok) = match caps.color_depth {
            ColorDepth::TrueColor => ("truecolor (24-bit)".to_owned(), true),
            // Anything short of 24-bit is a warning, not an "ok": the
            // built-in themes and every decoded image are authored in RGB,
            // and an indexed terminal quantizes both.
            ColorDepth::Ansi256 => ("256 colors (no 24-bit color)".to_owned(), false),
            ColorDepth::Ansi16 => ("16 colors (no 24-bit color)".to_owned(), false),
            ColorDepth::NoColor => ("none — plain text only".to_owned(), false),
        };
        // Below 24-bit color a native protocol is present but unusable —
        // every decoded pixel would quantize into the 256-color cube — so
        // the row reports the gate rather than the protocol.  Keying off
        // `full_color` here is what keeps this row from contradicting the
        // Color row above it (a green ✓ under a ✗ color warning).
        let (images, images_ok) = match (caps.image_protocol, caps.full_color()) {
            (Some(_), false) => (
                "protocol detected, but needs 24-bit color".to_owned(),
                false,
            ),
            (Some(ImageProtocol::KittyGraphics), true) => ("Kitty graphics".to_owned(), true),
            (Some(ImageProtocol::Sixel), true) => ("Sixel".to_owned(), true),
            (Some(ImageProtocol::ITerm2), true) => ("iTerm2 inline images".to_owned(), true),
            (Some(ImageProtocol::Halfblocks), true) => {
                ("Unicode half-blocks (low fidelity)".to_owned(), false)
            }
            (None, _) => ("not supported — placeholders only".to_owned(), false),
        };
        let (mouse, mouse_ok) = if caps.mouse {
            ("enabled".to_owned(), true)
        } else {
            ("not supported".to_owned(), false)
        };
        let (kbd, kbd_ok) = if caps.keyboard_enhancement {
            ("Kitty keyboard protocol".to_owned(), true)
        } else {
            // Deliberately not a list of chords.  The legacy control-byte
            // encoding can carry neither a shifted modifier combination nor
            // `Ctrl` with a non-alphabetic key, which takes out Ctrl-`,
            // Ctrl-Enter, Ctrl-Backspace / Delete, Ctrl-Shift-Z / -T,
            // Shift-Enter and the Alt-Shift-Arrow table ops — too many to
            // enumerate without going stale, and the shape of the limit is
            // the useful part.  Affected chords never reach the app at all
            // (the terminal itself beeps); all of them stay reachable from
            // the command palette.
            (
                "legacy encoding — some Ctrl / Alt / Shift chords can't be sent".to_owned(),
                false,
            )
        };
        let (uni, uni_ok) = if caps.unicode_full {
            ("UTF-8 locale".to_owned(), true)
        } else {
            (
                "non-UTF-8 locale — some glyphs may not render".to_owned(),
                false,
            )
        };

        Self {
            rows: vec![
                CapRow {
                    label: "Color",
                    value: color,
                    ok: color_ok,
                },
                CapRow {
                    label: "Images",
                    value: images,
                    ok: images_ok,
                },
                CapRow {
                    label: "Mouse",
                    value: mouse,
                    ok: mouse_ok,
                },
                CapRow {
                    label: "Keyboard",
                    value: kbd,
                    ok: kbd_ok,
                },
                CapRow {
                    label: "Unicode",
                    value: uni,
                    ok: uni_ok,
                },
            ],
        }
    }

    /// True iff every captured capability is in its "ok" state.
    pub fn all_ok(&self) -> bool {
        self.rows.iter().all(|r| r.ok)
    }
}

/// The prose explaining that edamame substituted an indexed-color
/// theme for this session, as one wrapped paragraph.
///
/// Shared verbatim by the two modals that can deliver it — the
/// new-terminal capabilities notice (when the substitution and a
/// first-visit both happen on the same launch, the notice absorbs this
/// text and the standalone modal is suppressed, so the user reads one
/// explanation rather than two) and
/// [`crate::app::modal::ThemeDowngradeModal`] (every other case).
///
/// Emitted as *paragraph* `Line`s, not pre-broken display lines:
/// `ModalView` wraps its body with `Wrap { trim: false }` and sizes it
/// with `wrapped_rows`, so hand-splitting would double-wrap at narrow
/// widths and leave ragged short rows at wide ones.
pub fn theme_downgrade_lines(
    configured: &str,
    substituted: &str,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let opener = "This terminal does not support 24-bit color.";
    vec![Line::from(vec![
        Span::styled(opener, Style::default().fg(theme.palette.warning)),
        Span::raw(format!(
            " Your selected theme, {configured}, cannot be displayed correctly by this \
             terminal. edamame switched to the {substituted} theme for this session. \
             Your saved theme is unchanged — {configured} will be displayed in a terminal \
             with 24-bit color support."
        )),
    ])]
}

/// Build one body `Line` per capability row, themed for inclusion in a
/// `ModalView` body slice.  Used by the standalone capabilities-notice
/// modal, which doesn't drive the welcome modal's bespoke scroll
/// container.
pub fn build_cap_lines(rows: &[CapRow], theme: &Theme) -> Vec<Line<'static>> {
    let ok_style = Style::default().fg(theme.palette.success);
    let warn_style = Style::default().fg(theme.palette.warning);
    rows.iter()
        .map(|row| {
            let value_style = if row.ok { ok_style } else { warn_style };
            let mark = if row.ok { "✓" } else { "✗" };
            Line::from(vec![
                Span::raw("  "),
                Span::styled(mark.to_owned(), value_style),
                Span::raw(format!("  {}: ", row.label)),
                Span::styled(row.value.clone(), value_style),
            ])
        })
        .collect()
}

/// Build the welcome-modal form of a capability row.  The single
/// derivation of the row's text, shared by [`render_cap_row`] and
/// [`cap_row_height`] so the height a caller reserves and the height the
/// painter fills can never disagree.
fn cap_row_line(row: &CapRow, label_style: Style, value_style: Style) -> Line<'static> {
    let mark = if row.ok { "✓" } else { "✗" };
    Line::from(vec![
        Span::raw("  • "),
        Span::styled(format!("{}: ", row.label), label_style),
        Span::styled(row.value.clone(), value_style),
        Span::raw(" "),
        Span::styled(mark.to_owned(), value_style),
    ])
}

/// How many terminal rows [`render_cap_row`] needs for `row` at `width`.
///
/// A row value is prose of unbounded length (the Keyboard row's degraded
/// text is the long one), so it wraps rather than truncating — which means
/// the welcome modal's body-height trace has to ask rather than assume one
/// row per capability.  Styling cannot change the wrap, so this measures
/// with plain styles.
pub fn cap_row_height(row: &CapRow, width: u16) -> u16 {
    let line = cap_row_line(row, Style::default(), Style::default());
    crate::ui::scroll_container::wrapped_rows(std::slice::from_ref(&line), width).max(1)
}

/// Render a single capability row at `(x, y)` using the supplied
/// `ok_style` / `warn_style` for the value+mark span.  Wraps within
/// `width`; returns the number of rows consumed, which is always
/// [`cap_row_height`] for the same `row` and `width`.
#[allow(clippy::too_many_arguments)]
pub fn render_cap_row(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    row: &CapRow,
    theme: &Theme,
    ok_style: Style,
    warn_style: Style,
) -> u16 {
    let value_style = if row.ok { ok_style } else { warn_style };
    let line = cap_row_line(row, theme.modal_bg, value_style);
    let height = cap_row_height(row, width);
    Paragraph::new(line)
        .style(theme.modal_bg)
        .wrap(Wrap { trim: false })
        .render(
            Rect {
                x,
                y,
                width,
                height,
            },
            buf,
        );
    height
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(color_depth: ColorDepth, image_protocol: Option<ImageProtocol>) -> Capabilities {
        Capabilities {
            color_depth,
            image_protocol,
            ..Capabilities::minimal()
        }
    }

    fn row<'a>(summary: &'a CapSummary, label: &str) -> &'a CapRow {
        summary
            .rows
            .iter()
            .find(|r| r.label == label)
            .expect("row present")
    }

    #[test]
    fn images_row_reports_the_color_gate_below_truecolor() {
        // The contradiction guard: a native protocol on an indexed
        // terminal must not read as a green ✓ underneath a ✗ Color row.
        for depth in [ColorDepth::Ansi256, ColorDepth::Ansi16, ColorDepth::NoColor] {
            let summary = CapSummary::from_caps(&caps(depth, Some(ImageProtocol::KittyGraphics)));
            let images = row(&summary, "Images");
            assert!(!images.ok, "{depth:?}: protocol is unusable without 24-bit");
            assert!(images.value.contains("24-bit color"), "{depth:?}");
            assert!(!row(&summary, "Color").ok, "{depth:?}");
        }
    }

    #[test]
    fn images_row_names_the_protocol_on_truecolor() {
        let summary = CapSummary::from_caps(&caps(
            ColorDepth::TrueColor,
            Some(ImageProtocol::KittyGraphics),
        ));
        let images = row(&summary, "Images");
        assert!(images.ok);
        assert_eq!(images.value, "Kitty graphics");
    }

    #[test]
    fn halfblocks_stay_degraded_on_truecolor() {
        // Halfblocks were already a ✗ before the color gate existed;
        // folding `full_color` into the match must not upgrade them.
        let summary = CapSummary::from_caps(&caps(
            ColorDepth::TrueColor,
            Some(ImageProtocol::Halfblocks),
        ));
        let images = row(&summary, "Images");
        assert!(!images.ok);
        assert!(images.value.contains("half-blocks"));
    }

    #[test]
    fn a_long_row_value_wraps_instead_of_truncating() {
        // The welcome modal renders cap rows at a fixed CONTENT_WIDTH, so
        // a value longer than the remaining budget used to be silently
        // clipped mid-word (the degraded Keyboard row lost its tail).
        // `render_cap_row` wraps and reports its height; the painter and
        // the modal's height trace both key off `cap_row_height`.
        let row = CapRow {
            label: "Keyboard",
            value: "x".repeat(120),
            ok: false,
        };
        assert!(
            cap_row_height(&row, 64) > 1,
            "a value past the width budget must wrap, not truncate"
        );

        let theme = Box::leak(Box::new(Theme::default()));
        let mut buf = Buffer::empty(Rect::new(0, 0, 64, 8));
        let used = render_cap_row(
            &mut buf,
            0,
            0,
            64,
            &row,
            theme,
            Style::default(),
            Style::default(),
        );
        assert_eq!(
            used,
            cap_row_height(&row, 64),
            "painter height must match the height callers reserve"
        );
        // Every `x` survived somewhere in the painted band.
        let painted: String = (0..used)
            .flat_map(|r| (0..64).map(move |c| (c, r)))
            .map(|(c, r)| buf[(c, r)].symbol().to_owned())
            .collect();
        assert_eq!(
            painted.matches('x').count(),
            120,
            "wrapped row dropped characters: {painted:?}"
        );
    }

    #[test]
    fn no_row_states_a_consequence() {
        // Rows are descriptive; "disabled"/"turned off" belongs to the
        // consuming modal, which is the only layer that knows whether it
        // acts on the capability.  The Keyboard row used to be exempt
        // because it said "disabled"; it now describes the encoding limit
        // ("can't be sent"), so the invariant covers every row.
        for depth in [
            ColorDepth::TrueColor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
            ColorDepth::NoColor,
        ] {
            let summary = CapSummary::from_caps(&caps(depth, Some(ImageProtocol::KittyGraphics)));
            for r in &summary.rows {
                assert!(
                    !r.value.contains("disabled") && !r.value.contains("turned off"),
                    "{depth:?} {}: {:?} states a consequence",
                    r.label,
                    r.value
                );
            }
        }
    }
}
