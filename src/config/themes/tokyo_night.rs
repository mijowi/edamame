//! Tokyo Night — colours ported from the opencode Tokyo Night theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x1a1b26);
    let ink = rgb(0xc0caf5);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x7aa2f7),
        secondary: rgb(0xbb9af7),
        accent: rgb(0xff9e64),
        link: rgb(0x5ba0e6),

        success: rgb(0x9ece6a),
        warning: rgb(0xe0af68),
        error: rgb(0xf7768e),

        code: rgb(0x9ece6a),

        diff_add: rgb(0x41a6b5),
        diff_delete: rgb(0xc34043),

        light: false,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
