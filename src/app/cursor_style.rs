//! Resolve the editor's block-cursor color for the current frame.
//!
//! The cursor color signals context, and it always **mirrors the status
//! chip** — every branch reads a `status_mode_*` field (minus the badge's
//! `BOLD`, since a one-cell cursor reads better unbolded), so the chip and
//! cursor can never drift.  For the default (non-vim) handler the color
//! follows the view mode (`status_mode_preview` / `status_mode_rendered` /
//! `status_mode_raw`).  When the vim handler is active it follows the vim
//! sub-mode (`status_mode_vim_*`).  RAW is signalled only in INSERT: an
//! INSERT cursor in the full Raw view takes `status_mode_raw` (warning);
//! NORMAL / VISUAL keep their sub-mode color in every view, matching the
//! chip (which never shows a `(RAW)` suffix).
//!
//! This is the single place the (view mode, vim sub-mode) → cursor-style
//! decision is made; the views receive the resolved `Style` and never pick a
//! cursor color themselves.

use ratatui::style::{Modifier, Style};

use crate::config::Theme;
use crate::editor::Mode;
use crate::input::VimSubMode;

/// The block-cursor style for the editor, given the current view `mode` and
/// the active vim sub-mode (`None` for the default handler).
pub fn editor_cursor_style(theme: &Theme, mode: Mode, vim: Option<VimSubMode>) -> Style {
    // Preview is browse-only; the cursor is essentially never drawn, so it
    // keeps the muted Preview chip color regardless of handler.
    if mode == Mode::Preview {
        return unbold(theme.status_mode_preview);
    }
    match vim {
        // Default handler: color by view mode, mirroring the mode chip.
        None => {
            if mode == Mode::Raw {
                unbold(theme.status_mode_raw)
            } else {
                unbold(theme.status_mode_rendered)
            }
        }
        Some(VimSubMode::Normal | VimSubMode::OperatorPending) => {
            unbold(theme.status_mode_vim_normal)
        }
        // INSERT mirrors the chip in Rendered view, but drops to the raw
        // warning color in the full Raw view — the only place the RAW
        // distinction is surfaced.
        Some(VimSubMode::Insert) => {
            if mode == Mode::Raw {
                unbold(theme.status_mode_raw)
            } else {
                unbold(theme.status_mode_vim_insert)
            }
        }
        Some(VimSubMode::Visual | VimSubMode::VisualLine) => unbold(theme.status_mode_vim_visual),
    }
}

/// Drop `BOLD` from a chip style so it reads as a uniform, unbolded cursor.
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
    fn vim_modes_mirror_their_chip_color() {
        let t = theme();
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, Some(VimSubMode::Normal)).bg,
            t.status_mode_vim_normal.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, Some(VimSubMode::Insert)).bg,
            t.status_mode_vim_insert.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, Some(VimSubMode::Visual)).bg,
            t.status_mode_vim_visual.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, Some(VimSubMode::VisualLine)).bg,
            t.status_mode_vim_visual.bg
        );
    }

    #[test]
    fn operator_pending_reads_as_normal() {
        let t = theme();
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, Some(VimSubMode::OperatorPending)).bg,
            t.status_mode_vim_normal.bg
        );
    }

    #[test]
    fn raw_view_only_overrides_insert() {
        let t = theme();
        // INSERT in Raw view → warning (the RAW signal).
        assert_eq!(
            editor_cursor_style(&t, Mode::Raw, Some(VimSubMode::Insert)).bg,
            t.status_mode_raw.bg
        );
        // NORMAL / VISUAL keep their sub-mode color even in Raw view.
        assert_eq!(
            editor_cursor_style(&t, Mode::Raw, Some(VimSubMode::Normal)).bg,
            t.status_mode_vim_normal.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Raw, Some(VimSubMode::Visual)).bg,
            t.status_mode_vim_visual.bg
        );
    }

    #[test]
    fn cursor_drops_the_chip_bold() {
        let t = theme();
        let style = editor_cursor_style(&t, Mode::Rendered, Some(VimSubMode::Insert));
        assert!(!style.add_modifier.contains(Modifier::BOLD));
    }
}
