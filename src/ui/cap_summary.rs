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
use ratatui::widgets::{Paragraph, Widget};

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
            (
                "unavailable — Ctrl-Shift-Z redo / Alt-Shift-Arrow table ops disabled".to_owned(),
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

/// Render a single capability row at `(x, y)` using the supplied
/// `ok_style` / `warn_style` for the value+mark span.
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
) {
    let value_style = if row.ok { ok_style } else { warn_style };
    let mark = if row.ok { "✓" } else { "✗" };
    let line = Line::from(vec![
        Span::raw("  • "),
        Span::styled(format!("{}: ", row.label), theme.modal_bg),
        Span::styled(row.value.clone(), value_style),
        Span::raw(" "),
        Span::styled(mark.to_owned(), value_style),
    ]);
    Paragraph::new(line).style(theme.modal_bg).render(
        Rect {
            x,
            y,
            width,
            height: 1,
        },
        buf,
    );
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
    fn no_row_states_a_consequence() {
        // Rows are descriptive; "disabled"/"turned off" belongs to the
        // consuming modal, which is the only layer that knows whether it
        // acts on the capability.  The keyboard row is the one exception —
        // it names the two chords the terminal genuinely cannot deliver.
        for depth in [
            ColorDepth::TrueColor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
            ColorDepth::NoColor,
        ] {
            let summary = CapSummary::from_caps(&caps(depth, Some(ImageProtocol::KittyGraphics)));
            for r in summary.rows.iter().filter(|r| r.label != "Keyboard") {
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
