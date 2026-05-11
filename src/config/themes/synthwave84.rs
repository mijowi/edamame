//! SynthWave '84 — colours ported from the opencode SynthWave '84 theme.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x1a1726);
    let ink = rgb(0xffffff);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0x36f9f6),
        secondary: rgb(0xb084eb),
        accent: rgb(0xb084eb),
        link: rgb(0xff8b39),

        success: rgb(0x72f1b8),
        warning: rgb(0xfede5d),
        error: rgb(0xfe4450),

        code: rgb(0x72f1b8),

        diff_add: rgb(0x97f1d8),
        diff_delete: rgb(0xff5e5b),
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
