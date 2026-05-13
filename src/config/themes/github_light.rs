//! GitHub Light — colours ported from the opencode GitHub theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0xffffff);
    let ink = rgb(0x24292f);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x0969da),
        secondary: rgb(0x8250df),
        accent: rgb(0x1b7c83),
        link: rgb(0xbc4c00),

        success: rgb(0x1a7f37),
        warning: rgb(0x9a6700),
        error: rgb(0xcf222e),

        code: rgb(0xbf3989),

        diff_add: rgb(0x1f883d),
        diff_delete: rgb(0xd1242f),

        light: true,
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
