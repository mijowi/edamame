//! Utility rendering regressions supplement the discoverability scenario in app::helix.
use edamame::config::Theme;
use edamame::document::{Buffer, Selection};
use edamame::editor::{helix_ops, EditorState, Mode};
use edamame::ui::raw_view::{RawView, RawViewState};
use edamame::ui::{RenderedView, RenderedViewState};
use ratatui::{backend::TestBackend, Terminal};
use unicode_width::UnicodeWidthStr;

#[test]
fn selected_graphemes_have_exact_painted_cells_in_both_views_and_orientations() {
    for mode in [Mode::Raw, Mode::Rendered] {
        for reverse in [false, true] {
            let theme = Box::leak(Box::new(Theme::default()));
            let mut editor = EditorState::new(Buffer::from_str("ab日e\u{301}f\n"), theme);
            editor.mode = mode;
            editor.terminal_focused = false;
            helix_ops::set_selection(
                &mut editor,
                if reverse {
                    Selection {
                        anchor: 5,
                        active: 1,
                    }
                } else {
                    Selection {
                        anchor: 1,
                        active: 5,
                    }
                },
            );
            assert_eq!(
                editor.selection.unwrap().selected_text(&editor.buffer),
                "b日e\u{301}"
            );
            let mut terminal = Terminal::new(TestBackend::new(10, 2)).unwrap();
            terminal
                .draw(|frame| {
                    if mode == Mode::Raw {
                        frame.render_stateful_widget(
                            RawView {
                                state: &editor,
                                theme,
                                visual_kind: None,
                                cursor_style: theme.status_mode_vim_normal,
                            },
                            frame.area(),
                            &mut RawViewState::default(),
                        );
                    } else {
                        frame.render_stateful_widget(
                            RenderedView {
                                state: &editor,
                                theme,
                                visual_kind: None,
                                cursor_style: theme.status_mode_vim_normal,
                                show_table_buttons: false,
                                drop_indicator: None,
                            },
                            frame.area(),
                            &mut RenderedViewState::default(),
                        );
                    }
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            // Ratatui resets wide-glyph continuation cells; the terminal paints their lead style.
            let mut painted = ['.'; 10];
            for x in 0..10u16 {
                let cell = &buffer[(x, 0)];
                if Some(cell.bg) == theme.selection.bg {
                    for column in painted
                        .iter_mut()
                        .skip(x as usize)
                        .take(cell.symbol().width())
                    {
                        *column = '^';
                    }
                }
            }
            let mask: String = painted.iter().collect();
            assert_eq!(mask, ".^^^^.....", "{mode:?} {reverse}");
            let text: String = (0..10).map(|x| buffer[(x, 0)].symbol()).collect();
            let name = format!(
                "helix_selection_{}_{}",
                if mode == Mode::Raw { "raw" } else { "rendered" },
                if reverse { "backward" } else { "forward" }
            );
            insta::assert_snapshot!(name, format!("{text}\n{mask}"));
        }
    }
}
