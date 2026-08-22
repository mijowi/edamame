//! Shared helpers for RGB-based built-in themes.  Indexed-color
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

/// Push each channel away from the color's own mean by
/// `1.0 + amount`, increasing chroma without shifting hue.  Pure
/// greys (`r == g == b`) stay grey.  Non-RGB colors pass through
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

/// Relative luminance of a color in `0.0..=1.0`, using the standard
/// sRGB coefficients.  Only defined for `Color::Rgb`; indexed / named /
/// `Reset` colors return `None` because their real luminance depends on
/// the terminal palette and can't be known here.
pub fn luminance(c: Color) -> Option<f32> {
    let Color::Rgb(r, g, b) = c else { return None };
    Some((0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0)
}

/// Pick whichever of `a` / `b` contrasts more strongly with `bg`,
/// measured by absolute luminance difference.  Used to choose a legible
/// foreground for a colored fill (e.g. the selection highlight) so a
/// theme whose `accent` sits near its `text` luminance doesn't render
/// selected text as low-contrast mud.  When luminance can't be computed
/// (non-RGB colors) it returns `a`, preserving the caller's default.
pub fn best_contrast(bg: Color, a: Color, b: Color) -> Color {
    match (luminance(bg), luminance(a), luminance(b)) {
        (Some(l_bg), Some(l_a), Some(l_b)) => {
            if (l_a - l_bg).abs() >= (l_b - l_bg).abs() {
                a
            } else {
                b
            }
        }
        _ => a,
    }
}

/// WCAG relative-contrast ratio between two colors, in `1.0..=21.0`.
/// `None` when either side is not `Color::Rgb`, for the same reason
/// [`luminance`] declines: an indexed or named color's real value
/// depends on the terminal palette.
pub fn contrast_ratio(a: Color, b: Color) -> Option<f32> {
    // `luminance` is a plain channel-weighted average of *gamma-encoded*
    // channels, which is what `best_contrast`'s relative comparison
    // wants.  A WCAG ratio needs linearized channels, so this does its
    // own conversion rather than reusing that value.
    fn linear(c: Color) -> Option<f32> {
        let Color::Rgb(r, g, b) = c else { return None };
        let f = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        Some(0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b))
    }
    let (x, y) = (linear(a)?, linear(b)?);
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    Some((hi + 0.05) / (lo + 0.05))
}

/// How far toward `ink` a single lift step moves a color.  Small enough
/// that a color clearing the bar early keeps most of its hue.
const LEGIBILITY_STEP: f32 = 0.1;

/// Return `fg` if it already reaches `min` contrast against `bg`,
/// otherwise the same hue blended toward `ink` until it does.
///
/// Used for the `syntax_*` styles, whose foregrounds are derived from
/// palette slots chosen for their *role* rather than measured against
/// the code surface — a mid-tone accent that reads fine as a heading on
/// the page background can land near-invisible on a code block's wash.
/// Blending toward `ink` (the palette's own text color, legible on that
/// surface by construction) raises luminance separation while keeping
/// the hue that distinguishes one token class from another.
///
/// Returns `fg` unchanged when either color is non-RGB: [`blend`] is a
/// no-op there and stepping would spin without converging.  Indexed
/// built-ins therefore hand-pick their `syntax_*` colors, exactly as
/// they already hand-pick the heading ramp and `code_bg`.
pub fn legible_on(bg: Color, fg: Color, ink: Color, min: f32) -> Color {
    if !matches!(
        (bg, fg, ink),
        (Color::Rgb(..), Color::Rgb(..), Color::Rgb(..))
    ) {
        return fg;
    }
    let mut out = fg;
    let mut t = 0.0;
    while contrast_ratio(out, bg).is_some_and(|c| c < min) && t < 1.0 {
        t += LEGIBILITY_STEP;
        out = blend(fg, ink, t);
    }
    out
}

/// Chroma boost applied to derived chrome surfaces — picked so the
/// tint reads as "warm dark grey" / "cool dark grey" rather than as
/// a recognisable hue.  Bump cautiously; values above ~1.0 start to
/// look like colored panels rather than chrome.
const CHROME_SATURATION_BOOST: f32 = 1.0;

/// Build a chrome surface lifted from `bg` toward `ink` by `t`,
/// then nudged in saturation so the result keeps a hint of `bg`'s
/// underlying tint instead of reading as flat grey.  Used for the
/// `surface` / `surface_elevated` palette slots.
pub fn chrome(bg: Color, ink: Color, t: f32) -> Color {
    saturate(blend(bg, ink, t), CHROME_SATURATION_BOOST)
}
