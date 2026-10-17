//! The single place the (view mode, modal sub-mode) → block-cursor style decision is made.
//!
//! The cursor reuses the typed status badge style without `BOLD`. Preview always uses its
//! browse style; Vim Insert in Raw uses the Raw style. Helix keeps its sub-mode color in Raw.

use ratatui::style::{Modifier, Style};

use crate::config::Theme;
use crate::editor::Mode;
use crate::input::{ModalInput, VimSubMode};
use crate::ui::status_bar::modal_badge;

/// The block-cursor style for the editor, given the current view `mode` and
/// the active modal session.
pub fn editor_cursor_style(theme: &Theme, mode: Mode, input: &ModalInput) -> Style {
    if mode == Mode::Preview {
        return unbold(theme.status_mode_preview);
    }
    if mode == Mode::Raw
        && matches!(input, ModalInput::Vim(state) if state.sub_mode == VimSubMode::Insert)
    {
        return unbold(theme.status_mode_raw);
    }
    let style = modal_badge(theme, input).map_or_else(
        || {
            if mode == Mode::Raw {
                theme.status_mode_raw
            } else {
                theme.status_mode_rendered
            }
        },
        |(_, style)| style,
    );
    unbold(style)
}

fn unbold(style: Style) -> Style {
    style.remove_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{HelixState, HelixSubMode, VimState};

    fn theme() -> Theme {
        Theme::default()
    }

    fn vim(sub_mode: VimSubMode) -> ModalInput {
        ModalInput::Vim(Box::new(VimState {
            sub_mode,
            ..VimState::default()
        }))
    }

    #[test]
    fn default_handler_follows_view_mode_chip() {
        let t = theme();
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, &ModalInput::Default).bg,
            t.status_mode_rendered.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Raw, &ModalInput::Default).bg,
            t.status_mode_raw.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Preview, &ModalInput::Default).bg,
            t.status_mode_preview.bg
        );
    }

    #[test]
    fn vim_modes_mirror_their_chip_color() {
        let t = theme();
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, &vim(VimSubMode::Normal)).bg,
            t.status_mode_vim_normal.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, &vim(VimSubMode::Insert)).bg,
            t.status_mode_vim_insert.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, &vim(VimSubMode::Visual)).bg,
            t.status_mode_vim_visual.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, &vim(VimSubMode::VisualLine)).bg,
            t.status_mode_vim_visual.bg
        );
    }

    #[test]
    fn operator_pending_reads_as_normal() {
        let t = theme();
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, &vim(VimSubMode::OperatorPending)).bg,
            t.status_mode_vim_normal.bg
        );
    }

    #[test]
    fn raw_view_only_overrides_insert() {
        let t = theme();
        for (sub_mode, expected) in [
            (VimSubMode::Normal, t.status_mode_vim_normal),
            (VimSubMode::OperatorPending, t.status_mode_vim_normal),
            (VimSubMode::Insert, t.status_mode_raw),
            (VimSubMode::Visual, t.status_mode_vim_visual),
            (VimSubMode::VisualLine, t.status_mode_vim_visual),
        ] {
            assert_eq!(
                editor_cursor_style(&t, Mode::Raw, &vim(sub_mode)),
                expected.remove_modifier(Modifier::BOLD)
            );
        }
    }

    // Utility regression: Preview wins over modal sub-modes; Helix keeps its Raw colors.
    #[test]
    fn helix_modes_keep_their_color_and_preview_hides_the_cursor() {
        let t = theme();
        for (sub_mode, expected) in [
            (HelixSubMode::Normal, t.status_mode_vim_normal),
            (HelixSubMode::Insert, t.status_mode_vim_insert),
            (HelixSubMode::Select, t.status_mode_vim_visual),
        ] {
            let input = ModalInput::Helix(Box::new(HelixState {
                sub_mode,
                ..HelixState::default()
            }));
            for mode in [Mode::Rendered, Mode::Raw] {
                assert_eq!(
                    editor_cursor_style(&t, mode, &input),
                    expected.remove_modifier(Modifier::BOLD)
                );
            }
            assert_eq!(
                editor_cursor_style(&t, Mode::Preview, &input),
                t.status_mode_preview.remove_modifier(Modifier::BOLD)
            );
        }
        for sub_mode in [
            VimSubMode::Normal,
            VimSubMode::Insert,
            VimSubMode::OperatorPending,
            VimSubMode::Visual,
            VimSubMode::VisualLine,
        ] {
            assert_eq!(
                editor_cursor_style(&t, Mode::Preview, &vim(sub_mode)),
                t.status_mode_preview.remove_modifier(Modifier::BOLD)
            );
        }
    }

    #[test]
    fn cursor_drops_the_chip_bold() {
        let t = theme();
        let style = editor_cursor_style(&t, Mode::Rendered, &vim(VimSubMode::Insert));
        assert!(!style.add_modifier.contains(Modifier::BOLD));
    }
}
