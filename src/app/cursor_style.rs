//! The single place the (view mode, modal sub-mode) → block-cursor style decision is made.
//!
//! The cursor always mirrors the status chip: every branch reads a `status_mode_*` field
//! (minus `BOLD`), so chip and cursor can never drift.  RAW is signalled only in INSERT;
//! NORMAL / VISUAL keep their sub-mode color in every view, matching the chip.

use ratatui::style::{Modifier, Style};

use crate::config::Theme;
use crate::editor::Mode;
use crate::input::ModeFace;
use crate::ui::status_bar::mode_face_style;

/// The block-cursor style for the editor, given the current view `mode` and
/// the active modal session's face (`None` for the default handler).
pub fn editor_cursor_style(theme: &Theme, mode: Mode, face: Option<ModeFace>) -> Style {
    if mode == Mode::Preview {
        return unbold(theme.status_mode_preview);
    }
    match face {
        None | Some(ModeFace::Insert) if mode == Mode::Raw => unbold(theme.status_mode_raw),
        None => unbold(theme.status_mode_rendered),
        Some(face) => unbold(mode_face_style(theme, face)),
    }
}

fn unbold(style: Style) -> Style {
    style.remove_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::default()
    }

    #[test]
    fn default_handler_follows_view_mode_chip() {
        let t = theme();
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, None).bg,
            t.status_mode_rendered.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Raw, None).bg,
            t.status_mode_raw.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Preview, None).bg,
            t.status_mode_preview.bg
        );
    }

    #[test]
    fn modal_faces_mirror_their_chip_color() {
        let t = theme();
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, Some(ModeFace::Normal)).bg,
            t.status_mode_vim_normal.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, Some(ModeFace::Insert)).bg,
            t.status_mode_vim_insert.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, Some(ModeFace::Select)).bg,
            t.status_mode_vim_visual.bg
        );
    }

    #[test]
    fn raw_view_only_overrides_insert() {
        let t = theme();
        assert_eq!(
            editor_cursor_style(&t, Mode::Raw, Some(ModeFace::Insert)).bg,
            t.status_mode_raw.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Raw, Some(ModeFace::Normal)).bg,
            t.status_mode_vim_normal.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Raw, Some(ModeFace::Select)).bg,
            t.status_mode_vim_visual.bg
        );
    }

    #[test]
    fn cursor_drops_the_chip_bold() {
        let t = theme();
        let style = editor_cursor_style(&t, Mode::Rendered, Some(ModeFace::Insert));
        assert!(!style.add_modifier.contains(Modifier::BOLD));
    }
}
