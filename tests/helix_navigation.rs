mod helix {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use edamame::config::{Action, Theme};
    use edamame::document::{Buffer, Selection};
    use edamame::editor::{helix_ops, EditorState, Mode};
    use edamame::input::helix::{helix_feed, HelixPending, HelixState};

    fn make_state(text: &str, mode: Mode, width: usize, reflow: bool) -> EditorState {
        make_state_with_visual(text, mode, width, reflow, true)
    }

    fn make_state_with_visual(
        text: &str,
        mode: Mode,
        _width: usize,
        reflow: bool,
        visual_line_nav: bool,
    ) -> EditorState {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let mut state =
            EditorState::new_with_config(Buffer::from_str(text), theme, true, visual_line_nav, 24);
        state.mode = mode;
        state.set_reflow(reflow);
        state
    }

    fn at(state: &mut EditorState, offset: usize) {
        helix_ops::set_selection(state, Selection::new(offset));
    }

    fn send(state: &mut HelixState, editor: &mut EditorState, code: KeyCode, width: usize) {
        helix_feed(
            state,
            editor,
            KeyEvent::new(code, KeyModifiers::NONE),
            12,
            width,
        );
    }

    fn keys(state: &mut HelixState, editor: &mut EditorState, input: &str, width: usize) {
        for ch in input.chars() {
            send(state, editor, KeyCode::Char(ch), width);
        }
    }

    fn selected(editor: &EditorState) -> String {
        editor.selection.unwrap().selected_text(&editor.buffer)
    }

    #[test]
    fn navigation_movement_matrix() {
        for mode in [Mode::Raw, Mode::Rendered] {
            let mut editor = make_state("a日b\nsecond\nthird", mode, 20, false);
            at(&mut editor, 1);
            let mut helix = HelixState::default();
            send(&mut helix, &mut editor, KeyCode::Char('l'), 20);
            assert_eq!(selected(&editor), "b");
            send(&mut helix, &mut editor, KeyCode::Char('h'), 20);
            assert_eq!(selected(&editor), "日");
            send(&mut helix, &mut editor, KeyCode::Right, 20);
            assert_eq!(selected(&editor), "b");

            at(&mut editor, 4);
            send(&mut helix, &mut editor, KeyCode::Down, 20);
            assert_eq!(editor.buffer.char_to_line(editor.cursor.offset), 2);
            send(&mut helix, &mut editor, KeyCode::Up, 20);
            assert_eq!(editor.buffer.char_to_line(editor.cursor.offset), 1);

            at(&mut editor, 4);
            keys(&mut helix, &mut editor, "v", 20);
            send(&mut helix, &mut editor, KeyCode::Char('j'), 20);
            assert_eq!(editor.selection.unwrap().anchor, 4);
            assert_eq!(editor.buffer.char_to_line(editor.cursor.offset), 2);
        }

        let source = "  alpha\n  beta\n  gamma\n";
        let mut editor = make_state(source, Mode::Raw, 40, false);
        let mut helix = HelixState::default();
        at(&mut editor, source.find("beta").unwrap());
        send(&mut helix, &mut editor, KeyCode::Home, 40);
        assert_eq!(editor.cursor.offset, source.find("  beta").unwrap());
        send(&mut helix, &mut editor, KeyCode::End, 40);
        assert_eq!(selected(&editor), "a");

        at(&mut editor, source.find("beta").unwrap());
        keys(&mut helix, &mut editor, "gh", 40);
        assert_eq!(editor.cursor.offset, source.find("  beta").unwrap());
        keys(&mut helix, &mut editor, "gl", 40);
        assert_eq!(selected(&editor), "a");
        keys(&mut helix, &mut editor, "gs", 40);
        assert_eq!(selected(&editor), "b");

        at(&mut editor, source.find("beta").unwrap());
        keys(&mut helix, &mut editor, "gg", 40);
        assert_eq!(selected(&editor), "a");
        keys(&mut helix, &mut editor, "ge", 40);
        assert_eq!(selected(&editor), "g");
        at(&mut editor, source.find("beta").unwrap());
        keys(&mut helix, &mut editor, "2gg", 40);
        assert_eq!(selected(&editor), "b");

        let text = "first paragraph\ncontinues here\n\nlast";
        for mode in [Mode::Raw, Mode::Rendered] {
            for width in [8, 18, 40] {
                for visual_line_nav in [false, true] {
                    let mut editor =
                        make_state_with_visual(text, mode, width, true, visual_line_nav);
                    let mut oracle =
                        make_state_with_visual(text, mode, width, true, visual_line_nav);
                    at(&mut editor, 3);
                    at(&mut oracle, 3);
                    let mut helix = HelixState::default();

                    edamame::editor::edit_ops::apply(&mut oracle, Action::MoveDown, 12, width);
                    send(&mut helix, &mut editor, KeyCode::Down, width);
                    assert_eq!(editor.cursor.offset, oracle.cursor.offset);
                    assert_eq!(editor.cursor.preferred_col, oracle.cursor.preferred_col);

                    edamame::editor::edit_ops::apply(&mut oracle, Action::MoveUp, 12, width);
                    send(&mut helix, &mut editor, KeyCode::Up, width);
                    assert_eq!(editor.cursor.offset, oracle.cursor.offset);
                    assert_eq!(editor.cursor.preferred_col, oracle.cursor.preferred_col);
                }
            }
        }

        for width in [8, 18, 40] {
            let mut editor = make_state_with_visual(
                "alpha beta\ngamma delta\nomega",
                Mode::Raw,
                width,
                false,
                false,
            );
            at(&mut editor, 3);
            let mut helix = HelixState::default();
            keys(&mut helix, &mut editor, "2j", width);
            assert_eq!(editor.buffer.char_to_line(editor.cursor.offset), 2);
            keys(&mut helix, &mut editor, "2k", width);
            assert_eq!(editor.buffer.char_to_line(editor.cursor.offset), 0);
        }

        let mut editor = make_state("a\r\nb", Mode::Raw, 20, false);
        let mut helix = HelixState::default();
        keys(&mut helix, &mut editor, "gl", 20);
        assert_eq!(selected(&editor), "a");
        assert_eq!(editor.cursor.offset, 0);
    }

    #[test]
    fn navigation_bounded_counts() {
        let mut editor = make_state("a\nb\nc", Mode::Raw, 20, false);
        let mut helix = HelixState::default();
        keys(&mut helix, &mut editor, "999999999999gg", 20);
        assert_eq!(selected(&editor), "c");
        assert_eq!(helix.count, None);
        assert_eq!(helix.pending, HelixPending::None);

        at(&mut editor, 0);
        keys(&mut helix, &mut editor, "999999999999l", 20);
        assert_eq!(selected(&editor), "c");
        assert_eq!(helix.count, None);

        at(&mut editor, 0);
        keys(&mut helix, &mut editor, "2j", 20);
        assert_eq!(selected(&editor), "c");

        at(&mut editor, 0);
        keys(&mut helix, &mut editor, "2gqgg", 20);
        assert_eq!(selected(&editor), "a");
        assert_eq!(helix.count, None);
        assert_eq!(helix.pending, HelixPending::None);

        at(&mut editor, 0);
        keys(&mut helix, &mut editor, "2", 20);
        send(&mut helix, &mut editor, KeyCode::Esc, 20);
        send(&mut helix, &mut editor, KeyCode::Char('l'), 20);
        assert_eq!(selected(&editor), "\n");
        assert_eq!(helix.count, None);

        at(&mut editor, 0);
        keys(&mut helix, &mut editor, "2q", 20);
        send(&mut helix, &mut editor, KeyCode::Char('l'), 20);
        assert_eq!(selected(&editor), "\n");
        assert_eq!(helix.count, None);
    }

    // Utility regression: document line numbers stay u32 while repeated moves clamp at 9999.
    #[test]
    fn large_goto_line_count_and_motion_count_keep_their_distinct_bounds() {
        let source = vec!["x"; 15_001].join("\n");
        let mut editor = make_state(&source, Mode::Raw, 20, false);
        let mut helix = HelixState::default();
        keys(&mut helix, &mut editor, "15000gg", 20);
        assert_eq!(editor.buffer.char_to_line(editor.cursor.offset), 14_999);
        assert_eq!(selected(&editor), "x");
        assert_eq!(helix.count, None);
        assert_eq!(helix.pending, HelixPending::None);

        let mut editor = make_state(&"x".repeat(15_000), Mode::Raw, 20, false);
        keys(&mut helix, &mut editor, "15000l", 20);
        assert_eq!(editor.cursor.offset, 9_999);
        assert_eq!(selected(&editor), "x");
        assert_eq!(helix.count, None);
        assert_eq!(helix.pending, HelixPending::None);
    }

    // Utility regressions supplement navigation_movement_matrix and selection_actions.
    #[test]
    fn moving_then_vertical_uses_the_display_column() {
        let text =
            "one two three four five six seven eight\nabcdefghijklmno\nshort\nabcdefghijklmno";
        for mode in [Mode::Raw, Mode::Rendered] {
            for width in [8, 18] {
                for command in ["12l", "3w", "ff", "gl"] {
                    let mut editor = make_state(text, mode, width, true);
                    let mut state = HelixState::default();
                    keys(&mut state, &mut editor, command, width);
                    let expected = editor.current_visual_col(width);
                    assert_eq!(
                        editor.cursor.preferred_col, expected,
                        "{mode:?} {width} {command}"
                    );
                    let mut oracle = make_state(text, mode, width, true);
                    oracle.place_cursor(editor.cursor.offset);
                    oracle.cursor.preferred_col = expected;
                    edamame::editor::edit_ops::apply(&mut oracle, Action::MoveDown, 12, width);
                    keys(&mut state, &mut editor, "j", width);
                    assert_eq!(editor.cursor.offset, oracle.cursor.offset, "{command}j");
                }
            }
        }
    }

    // Utility regression: a yank must not lose the desired column on a short line.
    #[test]
    fn yank_preserves_the_vertical_target_column() {
        let mut editor = make_state_with_visual(
            "abcdefghijklmno\nx\nabcdefghijklmno",
            Mode::Raw,
            80,
            false,
            false,
        );
        let mut state = HelixState::default();
        at(&mut editor, 12);
        keys(&mut state, &mut editor, "j", 80);
        assert_eq!(editor.cursor.preferred_col, 12);
        keys(&mut state, &mut editor, "y", 80);
        assert_eq!(editor.cursor.preferred_col, 12);
        keys(&mut state, &mut editor, "j", 80);
        assert_eq!(editor.cursor.cell_col(&editor.buffer), 12);
    }
}
