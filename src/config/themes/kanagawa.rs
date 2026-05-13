//! Kanagawa — colours ported from the opencode Kanagawa theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x1f1f28);
    let ink = rgb(0xdcd7ba);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x7e9cd8),
        secondary: rgb(0x957fb8),
        accent: rgb(0xd27e99),
        link: rgb(0x7fb4ca),

        success: rgb(0x98bb6c),
        warning: rgb(0xd7a657),
        error: rgb(0xe82424),

        code: rgb(0x76946a),

        diff_add: rgb(0xa9d977),
        diff_delete: rgb(0xf24a4a),

        light: false,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
