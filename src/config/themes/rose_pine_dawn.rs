//! Rosé Pine Dawn — light variant of Rosé Pine, with colours ported
//! from the opencode Rose Pine theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0xfaf4ed);
    let ink = rgb(0x575279);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x286983),
        secondary: rgb(0x907aa9),
        accent: rgb(0xd7827e),
        link: rgb(0x56949f),

        success: rgb(0x31748f),
        warning: rgb(0xea9d34),
        error: rgb(0xb4637a),

        code: rgb(0xd68a36),

        diff_add: rgb(0x557f6e),
        diff_delete: rgb(0xc87f7b),

        light: true,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
