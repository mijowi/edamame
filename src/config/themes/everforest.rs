//! Everforest — colors ported from the opencode Everforest theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x2d353b);
    let ink = rgb(0xd3c6aa);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0xd699b6),
        secondary: rgb(0xa7c080),
        accent: rgb(0xe69875),
        link: rgb(0x7fbbb3),

        success: rgb(0x5ebf76),
        warning: rgb(0xdbbc7f),
        error: rgb(0xe67e80),

        code: rgb(0xb8db87),

        diff_add: rgb(0x58a35a),
        diff_delete: rgb(0xe26a75),

        light: false,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
