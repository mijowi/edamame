//! Catppuccin Latte — light variant of Catppuccin, with colors ported
//! from the opencode Catppuccin theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0xeff1f5);
    let ink = rgb(0x4c4f69);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x7287fd),
        secondary: rgb(0x04a5e5),
        accent: rgb(0xea76cb),
        link: rgb(0x1e66f5),

        success: rgb(0x40a02b),
        warning: rgb(0xdf8e1d),
        error: rgb(0xd20f39),

        code: rgb(0x8839ef),

        diff_add: rgb(0xa6d189),
        diff_delete: rgb(0xe78284),

        light: true,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
