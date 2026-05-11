//! Editor-area dimming behind a modal.
//!
//! A modal renders on top of the editor; the editor stays visible
//! around it but should read as recessed so the user's eye lands on
//! the modal first.  Two strategies depending on terminal colour
//! depth:
//!
//! - **Truecolor terminals** ([`ColourDepth::TrueColor`]) — convert
//!   each cell's foreground and background to RGB, blend toward the
//!   theme's `default_bg` by [`BLEND_T`], and write the result back as
//!   `Color::Rgb`.  Preserves structure (headings, code blocks,
//!   tables stay legible as silhouettes) while clearly fading the
//!   editor.
//!
//! - **Anything else** — fall back to a [`Modifier::DIM`] sweep.  On
//!   Ansi256 we additionally force the foreground to `text_muted` so
//!   the dim is more pronounced than the modifier alone delivers
//!   (terminals often render `DIM` as ~10% drop in luminance, which is
//!   barely visible).
//!
//! The 256-entry ANSI palette LUT in [`ANSI_PALETTE`] is the standard
//! xterm 256-colour table; values are well-known so adjusting them is
//! not a priority.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use crate::config::Theme;
use crate::terminal::{Capabilities, ColourDepth};

/// Fraction of the way each cell colour is blended toward the theme's
/// `default_bg` on truecolor terminals.  0.0 = untouched, 1.0 = fully
/// erased.  Tune by editing this constant — empirically 0.5 reads as
/// "moderately recessed" without losing document structure.
const BLEND_T: f32 = 0.6;

/// Apply the dim effect to `area` of `buf` for the active terminal
/// capabilities.  `theme` supplies the blend target (`default_bg`) and
/// the Ansi256 fallback foreground (`text_muted`).
pub fn dim_area(buf: &mut Buffer, area: Rect, caps: &Capabilities, theme: &Theme) {
    match caps.colour_depth {
        ColourDepth::TrueColor => dim_truecolor(buf, area, theme),
        ColourDepth::Ansi256 => dim_ansi256(buf, area, theme),
        ColourDepth::Ansi16 | ColourDepth::NoColour => dim_modifier_only(buf, area),
    }
}

/// Truecolor sweep: blend each cell's fg/bg toward `default_bg` by
/// [`BLEND_T`].  Cells with `Color::Reset` foreground or background
/// keep that side untouched (we don't know the terminal's actual
/// default colour).
fn dim_truecolor(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let target = match color_to_rgb(theme.default_bg()) {
        Some(rgb) => rgb,
        // Theme's default_bg is `Reset` — fall back to the simple
        // sweep; we have no concrete blend target.
        None => return dim_modifier_only(buf, area),
    };
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            let Some(cell) = buf.cell_mut((x, y)) else {
                continue;
            };
            if let Some(fg) = color_to_rgb(cell.fg) {
                let blended = blend_toward(fg, target, BLEND_T);
                cell.fg = Color::Rgb(blended[0], blended[1], blended[2]);
            }
            if let Some(bg) = color_to_rgb(cell.bg) {
                let blended = blend_toward(bg, target, BLEND_T);
                cell.bg = Color::Rgb(blended[0], blended[1], blended[2]);
            }
        }
    }
}

/// Ansi256 sweep: replace fg with `text_muted` and insert
/// `Modifier::DIM`.  The foreground swap is what produces the visible
/// dim — the modifier alone is too subtle on most terminals to
/// communicate "behind a modal".
fn dim_ansi256(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let muted = theme.text_muted();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.fg = muted;
                cell.modifier.insert(Modifier::DIM);
            }
        }
    }
}

/// Plain `Modifier::DIM` sweep — the fallback for low-colour or
/// monochrome terminals where we have no useful colour to swap in.
fn dim_modifier_only(buf: &mut Buffer, area: Rect) {
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.modifier.insert(Modifier::DIM);
            }
        }
    }
}

/// Linear blend from `src` toward `target` by `t` ∈ `[0, 1]`.  Math
/// runs in u16 to avoid u8 overflow on the multiply; final clamp is
/// implicit because both operands are ≤ 255 and `t` is bounded.
fn blend_toward(src: [u8; 3], target: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| -> u8 {
        let af = a as f32;
        let bf = b as f32;
        (af + (bf - af) * t).round().clamp(0.0, 255.0) as u8
    };
    [
        mix(src[0], target[0]),
        mix(src[1], target[1]),
        mix(src[2], target[2]),
    ]
}

/// Convert a `Color` to its RGB triple where one is determinable.
/// Returns `None` for `Color::Reset` (terminal default — we don't
/// know its concrete RGB).
fn color_to_rgb(color: Color) -> Option<[u8; 3]> {
    match color {
        Color::Reset => None,
        Color::Black => Some(ANSI_PALETTE[0]),
        Color::Red => Some(ANSI_PALETTE[1]),
        Color::Green => Some(ANSI_PALETTE[2]),
        Color::Yellow => Some(ANSI_PALETTE[3]),
        Color::Blue => Some(ANSI_PALETTE[4]),
        Color::Magenta => Some(ANSI_PALETTE[5]),
        Color::Cyan => Some(ANSI_PALETTE[6]),
        Color::Gray => Some(ANSI_PALETTE[7]),
        Color::DarkGray => Some(ANSI_PALETTE[8]),
        Color::LightRed => Some(ANSI_PALETTE[9]),
        Color::LightGreen => Some(ANSI_PALETTE[10]),
        Color::LightYellow => Some(ANSI_PALETTE[11]),
        Color::LightBlue => Some(ANSI_PALETTE[12]),
        Color::LightMagenta => Some(ANSI_PALETTE[13]),
        Color::LightCyan => Some(ANSI_PALETTE[14]),
        Color::White => Some(ANSI_PALETTE[15]),
        Color::Rgb(r, g, b) => Some([r, g, b]),
        Color::Indexed(i) => Some(ANSI_PALETTE[i as usize]),
    }
}

/// Standard xterm 256-colour palette.
///
/// - `0..16` — system colours (terminal-themable, but we use the
///   widely-accepted defaults so the blend target is stable).
/// - `16..232` — 6×6×6 colour cube; index `16 + 36r + 6g + b` where
///   each component is one of `[0, 95, 135, 175, 215, 255]`.
/// - `232..256` — 24-step grayscale ramp from `8` to `238`.
const ANSI_PALETTE: [[u8; 3]; 256] = build_ansi_palette();

const fn build_ansi_palette() -> [[u8; 3]; 256] {
    let mut p = [[0u8; 3]; 256];
    // System colours 0..16.
    p[0] = [0, 0, 0];
    p[1] = [128, 0, 0];
    p[2] = [0, 128, 0];
    p[3] = [128, 128, 0];
    p[4] = [0, 0, 128];
    p[5] = [128, 0, 128];
    p[6] = [0, 128, 128];
    p[7] = [192, 192, 192];
    p[8] = [128, 128, 128];
    p[9] = [255, 0, 0];
    p[10] = [0, 255, 0];
    p[11] = [255, 255, 0];
    p[12] = [0, 0, 255];
    p[13] = [255, 0, 255];
    p[14] = [0, 255, 255];
    p[15] = [255, 255, 255];
    // 6×6×6 cube starting at index 16.
    let levels: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let mut r = 0;
    while r < 6 {
        let mut g = 0;
        while g < 6 {
            let mut b = 0;
            while b < 6 {
                let i = 16 + 36 * r + 6 * g + b;
                p[i] = [levels[r], levels[g], levels[b]];
                b += 1;
            }
            g += 1;
        }
        r += 1;
    }
    // Greyscale ramp 232..256.
    let mut k = 0;
    while k < 24 {
        let v = 8 + 10 * k as u8;
        p[232 + k] = [v, v, v];
        k += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_at_zero_is_identity() {
        assert_eq!(blend_toward([100, 50, 200], [0, 0, 0], 0.0), [100, 50, 200]);
    }

    #[test]
    fn blend_at_one_is_target() {
        assert_eq!(
            blend_toward([100, 50, 200], [33, 44, 55], 1.0),
            [33, 44, 55]
        );
    }

    #[test]
    fn blend_halfway_is_midpoint() {
        // 100 → 0 by half = 50; 50 → 100 by half = 75; etc.
        let r = blend_toward([100, 50, 200], [0, 100, 0], 0.5);
        assert_eq!(r, [50, 75, 100]);
    }

    #[test]
    fn blend_clamps_t_below_zero() {
        assert_eq!(
            blend_toward([100, 50, 200], [0, 0, 0], -1.0),
            [100, 50, 200]
        );
    }

    #[test]
    fn blend_clamps_t_above_one() {
        assert_eq!(
            blend_toward([100, 50, 200], [33, 44, 55], 2.0),
            [33, 44, 55]
        );
    }

    #[test]
    fn ansi_palette_known_indices() {
        // Spot-checks against the published xterm 256-colour table.
        assert_eq!(ANSI_PALETTE[0], [0, 0, 0]); // black
        assert_eq!(ANSI_PALETTE[15], [255, 255, 255]); // white
        assert_eq!(ANSI_PALETTE[16], [0, 0, 0]); // cube 0,0,0
        assert_eq!(ANSI_PALETTE[231], [255, 255, 255]); // cube 5,5,5
        assert_eq!(ANSI_PALETTE[232], [8, 8, 8]); // first grey
        assert_eq!(ANSI_PALETTE[255], [238, 238, 238]); // last grey
                                                        // 196 is the bright red commonly used for `error`.
        assert_eq!(ANSI_PALETTE[196], [255, 0, 0]);
        // 208 is orange (edamame's `primary`).
        assert_eq!(ANSI_PALETTE[208], [255, 135, 0]);
    }

    #[test]
    fn color_to_rgb_handles_indexed_and_rgb() {
        assert_eq!(color_to_rgb(Color::Reset), None);
        assert_eq!(color_to_rgb(Color::Indexed(196)), Some([255, 0, 0]));
        assert_eq!(color_to_rgb(Color::Rgb(10, 20, 30)), Some([10, 20, 30]));
        assert_eq!(color_to_rgb(Color::Red), Some(ANSI_PALETTE[1]));
    }

    #[test]
    fn dim_truecolor_blends_each_cell_toward_default_bg() {
        // 4-cell row of bright orange fg on near-black bg.  Run the
        // production `dim_truecolor` and verify every cell ends up at
        // the precomputed blend midpoint against the theme's
        // `default_bg`.
        use ratatui::style::Style;
        let theme = crate::config::Theme::default();
        let target = color_to_rgb(theme.default_bg()).expect("default theme has rgb-able bg");
        let expected_fg = blend_toward([200, 100, 0], target, BLEND_T);
        let expected_bg = blend_toward([20, 20, 20], target, BLEND_T);

        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::empty(area);
        for x in 0..4 {
            buf.cell_mut((x, 0)).unwrap().set_style(
                Style::default()
                    .fg(Color::Rgb(200, 100, 0))
                    .bg(Color::Rgb(20, 20, 20)),
            );
        }

        dim_truecolor(&mut buf, area, &theme);

        for x in 0..4 {
            let cell = buf.cell((x, 0)).unwrap();
            assert_eq!(
                cell.fg,
                Color::Rgb(expected_fg[0], expected_fg[1], expected_fg[2])
            );
            assert_eq!(
                cell.bg,
                Color::Rgb(expected_bg[0], expected_bg[1], expected_bg[2])
            );
        }
    }
}
