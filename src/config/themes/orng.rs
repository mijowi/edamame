//! Orng — colours ported from the opencode Orng theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x0a0a0a);
    let ink = rgb(0xeeeeee);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0xec5b2b),
        secondary: rgb(0xb87500),
        accent: rgb(0x6ba1e6),
        link: rgb(0x56b6c2),

        success: rgb(0x59c57c),
        warning: rgb(0xec5b2b),
        error: rgb(0xe06c75),

        code: rgb(0xb87500),

        diff_add: rgb(0x59c57c),
        diff_delete: rgb(0xe26a75),
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
