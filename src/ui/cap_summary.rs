//! Shared terminal-capability summary, rendered both inside the welcome
//! modal's capabilities section and as the entire body of the new-terminal
//! capabilities notice modal.  Captures one `CapRow` per capability the
//! editor cares about, each tagged with an `ok` flag the renderer uses to
//! pick a success/warning style and a ✓/✗ glyph.

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
            ColorDepth::Ansi256 => ("256 colors".to_owned(), true),
            ColorDepth::Ansi16 => ("16 colors — themes will look muted".to_owned(), false),
            ColorDepth::NoColor => ("none — plain text only".to_owned(), false),
        };
        let (images, images_ok) = match caps.image_protocol {
            Some(ImageProtocol::KittyGraphics) => ("Kitty graphics".to_owned(), true),
            Some(ImageProtocol::Sixel) => ("Sixel".to_owned(), true),
            Some(ImageProtocol::ITerm2) => ("iTerm2 inline images".to_owned(), true),
            Some(ImageProtocol::Halfblocks) => {
                ("Unicode half-blocks (low fidelity)".to_owned(), false)
            }
            None => ("not supported — placeholders only".to_owned(), false),
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
