//! Dracula — colours ported from the opencode Dracula theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x181224);
    let ink = rgb(0xf8f8f2);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0xff79c6),
        secondary: rgb(0x7ecbff),
        accent: rgb(0x5fe874),
        link: rgb(0x6bb8ff),

        success: rgb(0x50fa7b),
        warning: rgb(0xffb86c),
        error: rgb(0xff5555),

        code: rgb(0xf1fa8c),

        diff_add: rgb(0x2fb27d),
        diff_delete: rgb(0xff6b81),
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
