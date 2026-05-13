//! Gruvbox Light — colors ported from the opencode Gruvbox theme,
//! drawing on the canonical Gruvbox palette for slots that opencode
//! collapses into a single hue.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0xfbf1c7);
    let ink = rgb(0x3c3836);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x076678),
        secondary: rgb(0x8f3f71),
        accent: rgb(0xd65d0e),
        link: rgb(0x427b58),

        success: rgb(0x79740e),
        warning: rgb(0xb57614),
        error: rgb(0x9d0006),

        code: rgb(0xaf3a03),

        diff_add: rgb(0x98971a),
        diff_delete: rgb(0xcc241d),

        light: true,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
