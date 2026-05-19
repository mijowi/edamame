//! Gruvbox (dark) — colors ported from the opencode Gruvbox theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x1d2021);
    let ink = rgb(0xebdbb2);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x83a598),
        secondary: rgb(0x8ec07c),
        accent: rgb(0xb16286),
        link: rgb(0xd3869b),

        success: rgb(0x98bb26),
        warning: rgb(0xfabd2f),
        error: rgb(0xfb4934),

        code: rgb(0xfe8019),

        diff_add: rgb(0x98971a),
        diff_delete: rgb(0xcc241d),

        light: false,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
