//! Zenburn — colors ported from the opencode Zenburn theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x3f3f3f);
    let ink = rgb(0xdcdccc);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x8cd0d3),
        secondary: rgb(0xdfaf8f),
        accent: rgb(0x93e0e3),
        link: rgb(0x94bff3),

        success: rgb(0x7f9f7f),
        warning: rgb(0xf0dfaf),
        error: rgb(0xcc9393),

        code: rgb(0xe0cf9f),

        diff_add: rgb(0x8fb28f),
        diff_delete: rgb(0xdca3a3),

        light: false,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
