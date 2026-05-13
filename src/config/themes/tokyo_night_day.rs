//! Tokyo Night Day — light variant of Tokyo Night, with colours ported
//! from the opencode Tokyonight theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0xe1e2e7);
    let ink = rgb(0x273153);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x2e7de9),
        secondary: rgb(0x9854f1),
        accent: rgb(0xb15c00),
        link: rgb(0x007197),

        success: rgb(0x587539),
        warning: rgb(0x8c6c3e),
        error: rgb(0xc94060),

        code: rgb(0x0f4b6e),

        diff_add: rgb(0x4f8f7b),
        diff_delete: rgb(0xd05f7c),

        light: true,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
