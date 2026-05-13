//! GitHub Dark — colors ported from the opencode GitHub theme (dark variant).

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x0d1117);
    let ink = rgb(0xc9d1d9);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0xd29922),
        secondary: rgb(0xa371f7),
        accent: rgb(0x39c5cf),
        link: rgb(0x58a6ff),

        success: rgb(0x3fb950),
        warning: rgb(0xe3b341),
        error: rgb(0xf85149),

        code: rgb(0xff7b72),

        diff_add: rgb(0x238636),
        diff_delete: rgb(0xda3633),

        light: false,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
