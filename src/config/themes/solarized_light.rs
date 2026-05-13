//! Solarized Light — colors ported from the opencode Solarized theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0xfdf6e3);
    let ink = rgb(0x586e75);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x268bd2),
        secondary: rgb(0x2aa198),
        accent: rgb(0xd33682),
        link: rgb(0x6c71c4),

        success: rgb(0x859900),
        warning: rgb(0xb58900),
        error: rgb(0xdc322f),

        code: rgb(0xcb4b16),

        diff_add: rgb(0xc6dc7a),
        diff_delete: rgb(0xf2a1a1),

        light: true,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
