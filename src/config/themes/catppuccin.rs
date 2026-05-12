//! Catppuccin Mocha — colours ported from the opencode Catppuccin theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x18182a);
    let ink = rgb(0xcdd6f4);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0xcba6f7),
        secondary: rgb(0x94e2d5),
        accent: rgb(0xf38ba8),
        link: rgb(0x89b4fa),

        success: rgb(0xa6d189),
        warning: rgb(0xf9e2af),
        error: rgb(0xf38ba8),

        code: rgb(0xa6e3a1),

        diff_add: rgb(0x94e2d5),
        diff_delete: rgb(0xf38ba8),
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
