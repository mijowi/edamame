//! Edamame — the namesake theme.  Earth tones over a warm walnut
//! bg, anchored by a vivid edamame-pod green primary.  Distinct
//! from Everforest: warmer bg (no cool blue-grey cast), brighter
//! primary, and explicitly conventional blue / yellow / red for
//! link / warning / error rather than the muted everforest set.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x2e2c29);
    let ink = rgb(0xf0e7d7);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x7acf29),
        secondary: rgb(0xd4a773),
        accent: rgb(0xdb8265),
        link: rgb(0x6ba8d4),

        success: rgb(0x80b848),
        warning: rgb(0xe2bc52),
        error: rgb(0xcd5c3f),

        code: rgb(0xcca54a),

        diff_add: rgb(0x7eaa54),
        diff_delete: rgb(0xb24e36),

        light: false,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
