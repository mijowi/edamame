//! Rainbow — a playful, unconstrained palette where every role
//! claims a different hue from across the wheel.  Not intended to
//! be tasteful; intended to be obvious.

use crate::config::theme::{Palette, Theme};

use super::util::{blend, chrome, rgb};

pub fn palette() -> Palette {
    let bg = rgb(0x140921);
    let ink = rgb(0xf2ece4);
    Palette {
        text: ink,
        text_muted: blend(ink, bg, 0.35),
        bg,
        bg_muted: blend(bg, ink, 0.06),
        surface: chrome(bg, ink, 0.12),
        surface_elevated: chrome(bg, ink, 0.18),

        primary: rgb(0xff5577),
        secondary: rgb(0xff9c3d),
        accent: rgb(0xb86bff),
        link: rgb(0x3a9eff),

        success: rgb(0x46d162),
        warning: rgb(0xf6c84a),
        error: rgb(0xe44747),

        code: rgb(0x4adcd0),

        diff_add: rgb(0x46d162),
        diff_delete: rgb(0xe44747),
    }
}

pub fn theme() -> Theme {
    Theme::from_palette(&palette())
}
