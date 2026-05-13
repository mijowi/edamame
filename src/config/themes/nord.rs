//! Nord — colours ported from the opencode Nord theme, with classic
//! Nord aurora hues filled in for slots opencode doesn't expose.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x2e3440);
    let ink = rgb(0xe5e9f0);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x88c0d0),
        secondary: rgb(0x5e81ac),
        accent: rgb(0xb48ead),
        link: rgb(0x81a1c1),

        success: rgb(0xa3be8c),
        warning: rgb(0xebcb8b),
        error: rgb(0xbf616a),

        code: rgb(0xd08770),

        diff_add: rgb(0x8fbcbb),
        diff_delete: rgb(0xd57780),

        light: false,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
