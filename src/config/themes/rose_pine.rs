//! Rosé Pine — colors ported from the opencode Rosé Pine theme.
//! Opencode's Rosé Pine palette has no `info` color; the `link` slot
//! uses a saturated blue so links read as conventional link affordances
//! rather than competing with the foam-cyan primary or the iris-purple
//! secondary.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x191724);
    let ink = rgb(0xe0def4);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x9ccfd8),
        secondary: rgb(0xc4a7e7),
        accent: rgb(0xebbcba),
        link: rgb(0x569fd6),

        success: rgb(0x6aa687),
        warning: rgb(0xf6c177),
        error: rgb(0xeb6f92),

        code: rgb(0xea9d34),

        diff_add: rgb(0x4d8a6c),
        diff_delete: rgb(0xb4637a),

        light: false,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
