//! Terminal-capability summary shared by the welcome modal and the new-terminal capabilities
//! notice: one `CapRow` per capability, with an `ok` flag driving the ✓/✗ styling.
//!
//! Rows are **descriptive**: they state what was detected, never what edamame does about it.
//! The consequence differs per consuming modal (the notice is informational; the welcome modal
//! writes `images` / `diagrams` on save), so each modal owns its own sentence for that.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};

use crate::config::Theme;
use crate::terminal::{Capabilities, ColorDepth, ImageProtocol};

/// One capability row: label, detected value, and an `ok` flag for styling.
#[derive(Debug, Clone)]
pub struct CapRow {
    pub label: &'static str,
    pub value: String,
    pub ok: bool,
}

/// Capability summary captured at modal construction, rows in display order.
#[derive(Debug, Clone)]
pub struct CapSummary {
    pub rows: Vec<CapRow>,
}

impl CapSummary {
    /// Build the summary (color, images, mouse, keyboard, unicode) from a snapshot.
    pub fn from_caps(caps: &Capabilities) -> Self {
        let (color, color_ok) = match caps.color_depth {
            ColorDepth::TrueColor => ("truecolor (24-bit)".to_owned(), true),
            // Anything short of 24-bit is a warning: themes and images are authored in RGB.
            ColorDepth::Ansi256 => ("256 colors (no 24-bit color)".to_owned(), false),
            ColorDepth::Ansi16 => ("16 colors (no 24-bit color)".to_owned(), false),
            ColorDepth::NoColor => ("none — plain text only".to_owned(), false),
        };
        // Below 24-bit color a native protocol is unusable, so the row reports the color gate
        // rather than the protocol (and never contradicts the Color row with a ✓ under a ✗).
        // `Halfblocks` must be matched *before* that gate: it is `Picker`'s fallback for "no
        // protocol detected", not a detection, so reporting it as "protocol detected" would
        // promise a terminal like Terminal.app images it can never show.
        let (images, images_ok) = match (caps.image_protocol, caps.full_color()) {
            (None, _) | (Some(ImageProtocol::Halfblocks), false) => {
                ("not supported — placeholders only".to_owned(), false)
            }
            (Some(_), false) => (
                "protocol detected, but needs 24-bit color".to_owned(),
                false,
            ),
            (Some(ImageProtocol::KittyGraphics), true) => ("Kitty graphics".to_owned(), true),
            (Some(ImageProtocol::Sixel), true) => ("Sixel".to_owned(), true),
            (Some(ImageProtocol::ITerm2), true) => ("iTerm2 inline images".to_owned(), true),
            (Some(ImageProtocol::KittyDirect), true) => ("kitty direct placement".to_owned(), true),
            (Some(ImageProtocol::Halfblocks), true) => {
                ("Unicode half-blocks (low fidelity)".to_owned(), false)
            }
        };
        let (mouse, mouse_ok) = if caps.mouse {
            ("enabled".to_owned(), true)
        } else {
            ("not supported".to_owned(), false)
        };
        let (kbd, kbd_ok) = if caps.keyboard_enhancement {
            ("Kitty keyboard protocol".to_owned(), true)
        } else {
            // Deliberately not a list of chords: too many to enumerate without going stale.
            // Affected chords never reach the app; all stay reachable from the command palette.
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

/// The paragraph explaining that an indexed-color theme was substituted for this session.
/// Shared by the capabilities notice (which absorbs it on a first visit) and
/// [`crate::app::modal::ThemeDowngradeModal`].  Emitted as one paragraph `Line`, not
/// pre-broken rows: `ModalView` wraps and sizes it itself.
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

/// One `ModalView` body `Line` per capability row (the capabilities-notice form).
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

/// The welcome-modal form of a row; the single derivation shared by [`render_cap_row`] and
/// [`cap_row_height`] so reserved and painted heights agree.
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

/// Rows [`render_cap_row`] needs for `row` at `width`.  Values wrap rather than truncate, so
/// the welcome modal's height trace must ask rather than assume one row per capability.
pub fn cap_row_height(row: &CapRow, width: u16) -> u16 {
    let line = cap_row_line(row, Style::default(), Style::default());
    crate::ui::scroll_container::wrapped_rows(std::slice::from_ref(&line), width).max(1)
}

/// Render one row at `(x, y)`, wrapping within `width`; returns the rows consumed, always
/// equal to [`cap_row_height`].
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
        for depth in [ColorDepth::Ansi256, ColorDepth::Ansi16, ColorDepth::NoColor] {
            let summary = CapSummary::from_caps(&caps(depth, Some(ImageProtocol::KittyGraphics)));
            let images = row(&summary, "Images");
            assert!(!images.ok, "{depth:?}: protocol is unusable without 24-bit");
            assert!(images.value.contains("24-bit color"), "{depth:?}");
            assert!(!row(&summary, "Color").ok, "{depth:?}");
        }
    }

    /// `Halfblocks` is the picker's "nothing detected" fallback (Terminal.app), so the row
    /// must read like a terminal with no image support.
    #[test]
    fn halfblocks_below_truecolor_reads_as_no_image_support() {
        for depth in [ColorDepth::Ansi256, ColorDepth::Ansi16, ColorDepth::NoColor] {
            let summary = CapSummary::from_caps(&caps(depth, Some(ImageProtocol::Halfblocks)));
            let images = row(&summary, "Images");
            assert!(!images.ok, "{depth:?}");
            assert!(
                !images.value.contains("protocol detected"),
                "{depth:?}: half-blocks are a fallback, not a detected protocol: {:?}",
                images.value
            );
            assert!(
                !images.value.contains("half-block"),
                "{depth:?}: half-blocks need 24-bit color to display: {:?}",
                images.value
            );
            assert_eq!(
                images.value,
                row(&CapSummary::from_caps(&caps(depth, None)), "Images").value,
                "{depth:?}: must read identically to a terminal with no image support"
            );
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
        // Regression: the degraded Keyboard row used to be clipped mid-word at CONTENT_WIDTH.
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
        // The consequence belongs to the consuming modal (see the module doc).
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
