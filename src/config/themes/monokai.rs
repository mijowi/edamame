//! Monokai — colors ported from the opencode Monokai theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x1c1d18);
    let ink = rgb(0xf8f8f2);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0xae81ff),
        secondary: rgb(0xfd971f),
        accent: rgb(0xff54c0),
        link: rgb(0x66d9ef),

        success: rgb(0xa6e22e),
        warning: rgb(0xf4bf75),
        error: rgb(0xe04a4a),

        code: rgb(0xe6db74),

        diff_add: rgb(0x4d7f2a),
        diff_delete: rgb(0xf4477c),

        light: false,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
