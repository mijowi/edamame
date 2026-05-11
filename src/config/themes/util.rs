//! Shared helpers for RGB-based built-in themes.  Indexed-colour
//! built-ins (`dark_256`, `light_256`) hand-pick their tints from the
//! 6×6×6 cube; everything else uses these to derive surface tones and
//! muted text from a base `bg` / `ink` pair.

use ratatui::style::Color;

/// Build an RGB [`Color`] from a packed `0xRRGGBB` literal — keeps the
/// per-theme palette tables readable as a column of hex codes.
pub fn rgb(hex: u32) -> Color {
    Color::Rgb(
        ((hex >> 16) & 0xff) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
    )
}

/// Linearly blend `a` toward `b` by `t` (0.0 = `a`, 1.0 = `b`).
/// Only defined for `Color::Rgb`; other variants return `a` unchanged.
pub fn blend(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let mix = |x: u8, y: u8| {
                (x as f32 * (1.0 - t) + y as f32 * t)
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
        }
        _ => a,
    }
}

/// Push each channel away from the colour's own mean by
/// `1.0 + amount`, increasing chroma without shifting hue.  Pure
/// greys (`r == g == b`) stay grey.  Non-RGB colours pass through
/// unchanged.
pub fn saturate(c: Color, amount: f32) -> Color {
    let Color::Rgb(r, g, b) = c else { return c };
    let avg = (r as f32 + g as f32 + b as f32) / 3.0;
    let push = |x: u8| {
        let d = x as f32 - avg;
        (avg + d * (1.0 + amount)).round().clamp(0.0, 255.0) as u8
    };
    Color::Rgb(push(r), push(g), push(b))
}

/// Chroma boost applied to derived chrome surfaces — picked so the
/// tint reads as "warm dark grey" / "cool dark grey" rather than as
/// a recognisable hue.  Bump cautiously; values above ~1.0 start to
/// look like coloured panels rather than chrome.
const CHROME_SATURATION_BOOST: f32 = 1.0;

/// Build a chrome surface lifted from `bg` toward `ink` by `t`,
/// then nudged in saturation so the result keeps a hint of `bg`'s
/// underlying tint instead of reading as flat grey.  Used for the
/// `surface` / `surface_elevated` palette slots.
pub fn chrome(bg: Color, ink: Color, t: f32) -> Color {
    saturate(blend(bg, ink, t), CHROME_SATURATION_BOOST)
}
