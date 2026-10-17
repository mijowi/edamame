mod helix {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use edamame::config::Theme;
    use edamame::document::{Buffer, Selection};
    use edamame::editor::{helix_ops, EditorState, Mode};
    use edamame::input::helix::{helix_feed, HelixState, HelixSubMode};

    fn make_state(text: &str) -> EditorState {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let mut state = EditorState::new(Buffer::from_str(text), theme);
        state.mode = Mode::Rendered;
        state
    }

    fn key(state: &mut HelixState, editor: &mut EditorState, ch: char) {
        helix_feed(
            state,
            editor,
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            20,
            80,
        );
    }

    fn setup_selection(
        text: &str,
        range: (usize, usize),
        reverse: bool,
        select_mode: bool,
    ) -> (EditorState, HelixState) {
        let mut editor = make_state(text);
        let selection = if reverse {
            Selection {
                anchor: range.1,
                active: range.0,
            }
        } else {
            Selection {
                anchor: range.0,
                active: range.1,
            }
        };
        helix_ops::set_selection(&mut editor, selection);
        let mut helix = HelixState::default();
        if select_mode {
            key(&mut helix, &mut editor, 'v');
            assert_eq!(helix.sub_mode, HelixSubMode::Select);
        }
        (editor, helix)
    }

    /// Utility: edits can create/remove a marker or expose a list after joining lines.
    #[test]
    fn list_prefilter_preserves_renumbering_and_atomic_undo() {
        for (source, start, end, text, expected) in [
            ("1. one\n2. two\n3. three\n", 0, 7, "", "2. two\n3. three\n"),
            ("one\n3. three\n", 0, 0, "1. ", "1. one\n2. three\n"),
            ("X1. one\n3. three\n", 0, 1, "", "1. one\n2. three\n"),
            ("1. one\nX3. three\n", 7, 8, "", "1. one\n2. three\n"),
            ("1. one\n3. three\n", 7, 7, "X", "1. one\nX3. three\n"),
            ("X\n1. one\n3. three\n", 0, 2, "", "1. one\n2. three\n"),
            ("1. one\n\n3. three\n", 7, 8, "", "1. one\n2. three\n"),
            (
                "1. one\n    日\n3. three\n",
                11,
                12,
                "é",
                "1. one\n    é\n2. three\n",
            ),
            ("1. one\n3. three\n", 0, 0, "x\n", "x\n1. one\n3. three\n"),
            ("abc", 1, 2, "日", "a日c"),
        ] {
            let mut editor = make_state(source);
            helix_ops::actions::replace_range(&mut editor, start, end, text).unwrap();
            assert_eq!(editor.contents(), expected, "{source:?} / {text:?}");
            assert_eq!(editor.history.undo_depth(), 1);
            editor.history.undo(&mut editor.buffer).unwrap();
            assert_eq!(editor.contents(), source);
        }
    }

    #[test]
    fn selection_actions_action_matrix() {
        for select_mode in [false, true] {
            for reverse in [false, true] {
                let (mut editor, mut helix) =
                    setup_selection("a日b\ncde", (0, 6), reverse, select_mode);
                key(&mut helix, &mut editor, 'r');
                key(&mut helix, &mut editor, 'X');
                assert_eq!(editor.buffer.contents(), "XXX\nXXe");
                assert_eq!(editor.history.undo_depth(), 1);
                assert_eq!(helix.sub_mode, HelixSubMode::Normal);
            }
        }

        for select_mode in [false, true] {
            for reverse in [false, true] {
                let (mut editor, mut helix) =
                    setup_selection("a日b\ncde", (1, 6), reverse, select_mode);
                helix.register.text = "XY".to_owned();
                helix.register.linewise = true;
                key(&mut helix, &mut editor, 'R');
                assert_eq!(editor.buffer.contents(), "aXYe");
                assert_eq!(helix.register.text, "XY");
                assert!(helix.register.linewise);
                assert_eq!(editor.history.undo_depth(), 1);
                assert_eq!(helix.sub_mode, HelixSubMode::Normal);

                let (mut editor, mut helix) =
                    setup_selection("a日b\ncde", (1, 6), reverse, select_mode);
                let before = editor.buffer.contents();
                let selection = editor.selection;
                key(&mut helix, &mut editor, 'R');
                assert_eq!(editor.buffer.contents(), before);
                assert_eq!(editor.selection, selection);
                assert_eq!(editor.history.undo_depth(), 0);
                assert!(helix.register.text.is_empty());
                assert_eq!(helix.sub_mode, HelixSubMode::Normal);
            }
        }

        for select_mode in [false, true] {
            for reverse in [false, true] {
                let indent = " ".repeat(edamame::constants::INDENT_WIDTH);
                let (mut editor, mut helix) =
                    setup_selection("one\ntwo\nthree\n", (1, 6), reverse, select_mode);
                key(&mut helix, &mut editor, '>');
                assert_eq!(
                    editor.buffer.contents(),
                    format!("{indent}one\n{indent}two\nthree\n")
                );
                assert_eq!(editor.history.undo_depth(), 1);
                assert_eq!(helix.sub_mode, HelixSubMode::Normal);

                let (mut editor, mut helix) =
                    setup_selection("    one\n    two\nthree\n", (5, 10), reverse, select_mode);
                key(&mut helix, &mut editor, '<');
                assert_eq!(editor.buffer.contents(), "one\ntwo\nthree\n");
                assert_eq!(editor.history.undo_depth(), 1);
                assert_eq!(helix.sub_mode, HelixSubMode::Normal);
            }
        }

        for select_mode in [false, true] {
            for reverse in [false, true] {
                let (mut editor, mut helix) =
                    setup_selection("AbC\nkeep", (0, 3), reverse, select_mode);
                key(&mut helix, &mut editor, '~');
                assert_eq!(editor.buffer.contents(), "aBc\nkeep");
                assert_eq!(editor.history.undo_depth(), 1);
                assert_eq!(helix.sub_mode, HelixSubMode::Normal);
            }
        }

        for select_mode in [false, true] {
            for reverse in [false, true] {
                let (mut editor, mut helix) =
                    setup_selection("one\n  two\nthree\n", (1, 7), reverse, select_mode);
                key(&mut helix, &mut editor, 'J');
                assert_eq!(editor.buffer.contents(), "one two\nthree\n");
                assert_eq!(editor.history.undo_depth(), 1);
                assert_eq!(helix.sub_mode, HelixSubMode::Normal);

                let (mut editor, mut helix) =
                    setup_selection("single line\nnext", (1, 5), reverse, select_mode);
                key(&mut helix, &mut editor, 'J');
                assert_eq!(editor.buffer.contents(), "single line\nnext");
                assert_eq!(editor.history.undo_depth(), 0);
            }
        }

        for select_mode in [false, true] {
            for reverse in [false, true] {
                let (mut editor, mut helix) = setup_selection("abc", (0, 3), reverse, select_mode);
                let selection = editor.selection;
                let cursor = editor.cursor.offset;
                key(&mut helix, &mut editor, 'y');
                assert_eq!(helix.register.text, "abc");
                assert!(!helix.register.linewise);
                assert_eq!(editor.buffer.contents(), "abc");
                assert_eq!(editor.selection, selection);
                assert_eq!(editor.cursor.offset, cursor);
                assert_eq!(editor.history.undo_depth(), 0);
                assert_eq!(helix.sub_mode, HelixSubMode::Normal);
            }
        }

        for select_mode in [false, true] {
            for reverse in [false, true] {
                let (mut editor, mut helix) =
                    setup_selection("left right", (0, 4), reverse, select_mode);
                key(&mut helix, &mut editor, 'c');
                assert_eq!(editor.buffer.contents(), " right");
                assert!(editor.selection.is_none());
                assert_eq!(editor.history.undo_depth(), 1);
                assert_eq!(helix.sub_mode, HelixSubMode::Insert);
            }
        }
    }
    #[test]
    fn register_paste_internal_register() {
        for reverse in [false, true] {
            for (command, expected) in [('p', "a日XYb"), ('P', "aXY日b"), ('R', "aXYb")] {
                let (mut editor, mut helix) = setup_selection("a日b", (1, 2), reverse, false);
                helix.register.text = "XY".into();
                key(&mut helix, &mut editor, command);
                assert_eq!(editor.contents(), expected);
                assert_eq!(helix.register.text, "XY");
                assert_eq!(
                    editor.selection.unwrap().selected_text(&editor.buffer),
                    "XY"
                );
            }
        }
        for (source, range, command, expected, selected) in [
            ("a\nb", (0, 2), 'p', "a\nZ\nb", "Z\n"),
            ("a\nb", (2, 3), 'p', "a\nb\nZ", "\nZ"),
            ("a\nb", (2, 3), 'P', "a\nZ\nb", "Z\n"),
            ("", (0, 0), 'p', "Z\n", "Z\n"),
        ] {
            let (mut editor, mut helix) = setup_selection(source, range, false, false);
            helix.register.text = "Z".into();
            helix.register.linewise = true;
            key(&mut helix, &mut editor, command);
            assert_eq!(editor.contents(), expected);
            assert_eq!(
                editor.selection.unwrap().selected_text(&editor.buffer),
                selected
            );
        }
        let table = "| alpha | bravo |\n|---|---|\n| one | two |\n";
        let (mut rows, mut session) = setup_selection(table, (2, 7), false, false);
        session.register.text = "| new | row |\n".into();
        session.register.linewise = true;
        key(&mut session, &mut rows, 'p');
        assert_eq!(
            rows.contents(),
            "| alpha | bravo |\n|---|---|\n| new | row |\n| one | two |\n"
        );
        for (payload, linewise) in [("a|b", false), ("a\nb", false), ("plain", true)] {
            let (mut editor, mut helix) = setup_selection(table, (2, 7), false, false);
            helix.register.text = payload.into();
            helix.register.linewise = linewise;
            let result = helix_feed(
                &mut helix,
                &mut editor,
                KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
                20,
                80,
            );
            assert!(matches!(
                result,
                edamame::input::helix::HelixOutcome::Flash(_)
            ));
            assert_eq!(editor.contents(), table);
            assert_eq!(editor.history.undo_depth(), 0);
            assert_eq!(helix.register.text, payload);
        }
        let (mut editor, mut helix) = setup_selection("abc", (1, 2), false, false);
        key(&mut helix, &mut editor, 'p');
        key(&mut helix, &mut editor, 'R');
        assert_eq!(editor.contents(), "abc");
        assert_eq!(editor.history.undo_depth(), 0);
        helix.register.text = "Z".into();
        key(&mut helix, &mut editor, '2');
        key(&mut helix, &mut editor, 'p');
        assert_eq!(editor.contents(), "abZZc");
        let (mut editor, mut helix) = setup_selection("last", (0, 4), false, false);
        key(&mut helix, &mut editor, 'd');
        key(&mut helix, &mut editor, 'p');
        assert_eq!(editor.contents(), "last");
    }

    #[test]
    fn undo_redo_transaction_boundaries() {
        for (source, range, commands) in [
            ("one two", (0, 4), "d"),
            ("one two", (0, 4), "c"),
            ("one two", (0, 4), "rZ"),
            ("one two", (0, 4), "R"),
            ("one two", (0, 4), "p"),
            ("one two", (0, 4), "P"),
            ("one two", (0, 4), "2p"),
            ("one\ntwo", (0, 6), ">"),
            ("    one\n    two", (4, 10), "<"),
            ("one\ntwo", (0, 6), "J"),
            ("One TWO", (0, 7), "~"),
            ("1. a\n2. b\n3. c\n", (5, 10), "d"),
            ("| a | b |\n|---|---|\n| c | d |\n", (2, 3), "rZ"),
        ] {
            let (mut editor, mut helix) = setup_selection(source, range, false, false);
            helix.register.text = "Z".into();
            for c in commands.chars() {
                key(&mut helix, &mut editor, c);
            }
            let edited = editor.contents();
            assert_ne!(edited, source, "{commands}");
            assert_eq!(editor.history.undo_depth(), 1, "{commands}");
            if helix.sub_mode == HelixSubMode::Insert {
                helix_feed(
                    &mut helix,
                    &mut editor,
                    KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                    20,
                    80,
                );
            }
            key(&mut helix, &mut editor, 'u');
            assert_eq!(editor.contents(), source, "undo {commands}");
            assert!(editor.selection.unwrap().range().1 <= editor.buffer.len_chars());
            key(&mut helix, &mut editor, 'U');
            assert_eq!(editor.contents(), edited, "redo {commands}");
        }
        let (mut typing, mut session) = setup_selection("abc", (0, 1), false, false);
        key(&mut session, &mut typing, 'i');
        for c in "typed".chars() {
            assert_eq!(
                helix_feed(
                    &mut session,
                    &mut typing,
                    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                    20,
                    80
                ),
                edamame::input::helix::HelixOutcome::Passthrough
            );
            edamame::editor::edit_ops::apply(
                &mut typing,
                edamame::config::Action::InsertChar(c),
                20,
                80,
            );
        }
        assert_eq!(typing.history.undo_depth(), 1);
        helix_feed(
            &mut session,
            &mut typing,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            20,
            80,
        );
        key(&mut session, &mut typing, 'u');
        assert_eq!(typing.contents(), "abc");
        let (mut editor, mut helix) = setup_selection("abc", (0, 1), false, false);
        key(&mut helix, &mut editor, 'd');
        key(&mut helix, &mut editor, 'd');
        assert_eq!(editor.contents(), "c");
        assert_eq!(editor.history.undo_depth(), 2);
        key(&mut helix, &mut editor, 'u');
        assert_eq!(editor.contents(), "bc");
        key(&mut helix, &mut editor, 'u');
        assert_eq!(editor.contents(), "abc");
        let (mut editor, mut helix) = setup_selection("1. a\n2. b\n3. c\n", (5, 10), false, false);
        key(&mut helix, &mut editor, 'd');
        assert_eq!(editor.contents(), "1. a\n2. c\n");
    }
    // Utility boundary regression: added line separators count toward the paste budget.
    #[test]
    fn linewise_paste_checks_the_expanded_payload_before_allocating() {
        let (mut editor, mut helix) = setup_selection("keep", (0, 1), false, false);
        helix.register.text = "x".repeat(6711);
        helix.register.linewise = true;
        let error = helix_ops::paste::paste(&mut editor, &helix.register.text, true, true, 9999)
            .unwrap_err();
        assert_eq!(error, helix_ops::HelixError::PasteTooLarge);
        assert_eq!(error.to_string(), "Expanded paste exceeds 64 MiB");
        for digit in "9999".chars() {
            key(&mut helix, &mut editor, digit);
        }
        let outcome = helix_feed(
            &mut helix,
            &mut editor,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
            20,
            80,
        );
        assert_eq!(
            outcome,
            edamame::input::helix::HelixOutcome::Flash("Expanded paste exceeds 64 MiB".into())
        );
        assert_eq!(editor.contents(), "keep");
        assert_eq!(editor.history.undo_depth(), 0);
        assert_eq!(helix.register.text.len(), 6711);
    }
}
