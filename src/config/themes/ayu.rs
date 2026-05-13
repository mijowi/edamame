//! Ayu (dark) — colours ported from the opencode Ayu theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x09121d);
    let ink = rgb(0xd6dae0);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x3fb7e3),
        secondary: rgb(0xd2a6ff),
        accent: rgb(0xf2856f),
        link: rgb(0x4a9eea),

        success: rgb(0x78d05c),
        warning: rgb(0xe4a75c),
        error: rgb(0xf58572),

        code: rgb(0xaad94c),

        diff_add: rgb(0x59c57c),
        diff_delete: rgb(0xf58572),

        light: false,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
