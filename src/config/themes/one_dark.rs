//! One Dark — colours ported from the opencode One Dark theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x1d2128);
    let ink = rgb(0xd0d8e8);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x61afef),
        secondary: rgb(0xc678dd),
        accent: rgb(0x56b6c2),
        link: rgb(0xd19a66),

        success: rgb(0x98c379),
        warning: rgb(0xe5c07b),
        error: rgb(0xe06c75),

        code: rgb(0x98c379),

        diff_add: rgb(0xaad482),
        diff_delete: rgb(0xe8828b),
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
