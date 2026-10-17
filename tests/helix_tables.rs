mod helix {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use edamame::config::{Action, Theme};
    use edamame::document::{Buffer, Selection};
    use edamame::editor::{helix_ops, EditorState, Mode};
    use edamame::input::helix::{
        helix_feed, HelixOutcome, HelixRegister, HelixState, HelixSubMode,
    };

    const TABLE: &str = "| alpha | bravo |\n|---|---|\n| one | two |\n";

    fn editor(text: &str, mode: Mode) -> EditorState {
        let mut editor = EditorState::new(
            Buffer::from_str(text),
            Box::leak(Box::new(Theme::default())),
        );
        editor.mode = mode;
        editor
    }

    fn key(editor: &mut EditorState, state: &mut HelixState, code: KeyCode) -> HelixOutcome {
        helix_feed(
            state,
            editor,
            KeyEvent::new(code, KeyModifiers::NONE),
            20,
            80,
        )
    }

    fn keys(editor: &mut EditorState, state: &mut HelixState, keys: &str) {
        for ch in keys.chars() {
            key(editor, state, KeyCode::Char(ch));
        }
    }

    fn select(editor: &mut EditorState, anchor: usize, active: usize) {
        helix_ops::set_selection(editor, Selection { anchor, active });
    }

    fn register(text: &str, linewise: bool) -> HelixState {
        HelixState {
            register: HelixRegister {
                text: text.to_owned(),
                linewise,
            },
            ..HelixState::default()
        }
    }

    fn assert_refused(editor: &mut EditorState, state: &mut HelixState, command: &str) {
        let before = editor.contents();
        let selection = editor.selection;
        let register = state.register.clone();
        let history = editor.history.undo_depth();
        let mut refused = false;
        for ch in command.chars() {
            let outcome = key(editor, state, KeyCode::Char(ch));
            if matches!(outcome, HelixOutcome::Flash(_)) {
                refused = true;
                break;
            }
        }
        assert!(refused, "{command} must explain its refusal");
        assert_eq!(editor.contents(), before, "{command}");
        assert_eq!(state.register, register, "{command} mutated register");
        assert_eq!(
            editor.history.undo_depth(),
            history,
            "{command} mutated history"
        );
        assert_eq!(editor.selection, selection, "{command} changed selection");
    }

    // Utility: typed refusal reasons preserve guard priority, messages and document state.
    #[test]
    fn typed_refusals_preserve_table_guards_and_messages() {
        use edamame::editor::helix_ops::{actions::Transform, HelixError};
        use edamame::editor::vim_ops::TableBreak;

        let header_end = TABLE.find('\n').unwrap() + 1;
        for (readonly, start, end, payload, expected, message) in [
            (
                true,
                0,
                header_end,
                "|",
                HelixError::ReadOnly,
                "Can't edit a read-only document",
            ),
            (
                false,
                0,
                header_end,
                "|",
                HelixError::CellPaste,
                "Can't paste table separators or newlines into a cell",
            ),
            (
                false,
                0,
                header_end,
                "",
                HelixError::TableRange(TableBreak::ProtectedRow),
                "Can't remove a table's header or alignment row",
            ),
            (
                false,
                3,
                10,
                "",
                HelixError::TableRange(TableBreak::CrossesCells),
                "Can't edit across table cells",
            ),
        ] {
            let mut editor = editor(TABLE, Mode::Rendered);
            select(&mut editor, start, end);
            editor.readonly = readonly;
            let before = (editor.selection, editor.cursor, editor.history.undo_depth());
            let error =
                helix_ops::actions::replace_range(&mut editor, start, end, payload).unwrap_err();
            assert_eq!(error, expected);
            assert_eq!(error.to_string(), message);
            assert_eq!(editor.contents(), TABLE);
            assert_eq!(
                (editor.selection, editor.cursor, editor.history.undo_depth()),
                before
            );
        }

        let mut editor = editor(TABLE, Mode::Rendered);
        select(&mut editor, 2, 5);
        let before = (editor.selection, editor.cursor, editor.history.undo_depth());
        let error = helix_ops::actions::transform(&mut editor, Transform::Indent, 1).unwrap_err();
        assert_eq!(error, HelixError::TableTransform);
        assert_eq!(
            error.to_string(),
            "Can't change table structure with this selection action"
        );
        let error = helix_ops::paste::paste(&mut editor, "invalid row", true, true, 1).unwrap_err();
        assert_eq!(error, HelixError::TablePaste);
        assert_eq!(error.to_string(), "Can't paste these lines into a table");
        assert_eq!(editor.contents(), TABLE);
        assert_eq!(
            (editor.selection, editor.cursor, editor.history.undo_depth()),
            before
        );
    }

    #[test]
    fn table_safety_cell_and_range_guards() {
        for (motion, count) in [
            ("h", 1),
            ("l", 99),
            ("w", 1),
            ("e", 1),
            ("b", 1),
            ("W", 1),
            ("E", 1),
            ("B", 1),
            ("f", 1),
            ("F", 1),
            ("t", 1),
            ("T", 1),
            ("Home", 1),
            ("End", 1),
            ("gh", 1),
            ("gl", 1),
            ("gs", 1),
        ] {
            let mut editor = editor(TABLE, Mode::Rendered);
            let mut state = HelixState::default();
            editor.place_cursor(5);
            for _ in 0..count {
                if matches!(motion, "Home" | "End") {
                    key(
                        &mut editor,
                        &mut state,
                        if motion == "Home" {
                            KeyCode::Home
                        } else {
                            KeyCode::End
                        },
                    );
                } else if motion == "gh" || motion == "gl" || motion == "gs" {
                    keys(&mut editor, &mut state, motion);
                } else if matches!(motion, "f" | "F" | "t" | "T") {
                    key(
                        &mut editor,
                        &mut state,
                        KeyCode::Char(motion.chars().next().unwrap()),
                    );
                    key(&mut editor, &mut state, KeyCode::Char('a'));
                } else {
                    keys(&mut editor, &mut state, motion);
                }
            }
            assert!(
                (2..8).contains(&editor.cursor.offset),
                "{motion}: {}",
                editor.cursor.offset
            );
            assert!(!editor
                .selection
                .unwrap()
                .selected_text(&editor.buffer)
                .contains('|'));
        }

        let mut extended = editor(TABLE, Mode::Rendered);
        let mut state = HelixState::default();
        select(&mut extended, 0, 5);
        state.sub_mode = HelixSubMode::Select;
        key(&mut extended, &mut state, KeyCode::Char('l'));
        assert!(extended
            .selection
            .unwrap()
            .selected_text(&extended.buffer)
            .contains('|'));
        assert_refused(&mut extended, &mut state, "d");

        for command in ["d", "c"] {
            let mut editor = editor(TABLE, Mode::Rendered);
            let mut state = register("|\n", true);
            select(&mut editor, 3, 10);
            assert_refused(&mut editor, &mut state, command);
        }
        for command in ["r|", "R", "p", "P", ">", "<", "~", "J"] {
            let mut editor = editor(TABLE, Mode::Rendered);
            let mut state = register("|\n", command == "R");
            if command == "~" {
                select(&mut editor, 3, 10);
            } else if command == "J" {
                let second_row = editor.buffer.line_to_char(2);
                select(&mut editor, 3, second_row);
            } else {
                select(&mut editor, 3, 5);
            }
            assert_refused(&mut editor, &mut state, command);
        }
        for command in ["xd", "Xd"] {
            let mut editor = editor(TABLE, Mode::Rendered);
            let mut state = register("cached", false);
            if command.starts_with('X') {
                editor.place_cursor(editor.buffer.line_to_char(1));
            }
            keys(&mut editor, &mut state, &command[..1]);
            let line = if command.starts_with('X') { 1 } else { 0 };
            assert_eq!(
                editor.selection.unwrap().selected_text(&editor.buffer),
                editor.buffer.line(line).unwrap()
            );
            assert_refused(&mut editor, &mut state, "d");
        }
        for (command, expected) in [
            ("d", "|  | bravo |\n|---|---|\n| one | two |\n"),
            ("rZ", "| ZZZZZ | bravo |\n|---|---|\n| one | two |\n"),
            ("~", "| ALPHA | bravo |\n|---|---|\n| one | two |\n"),
        ] {
            let mut editor = editor(TABLE, Mode::Rendered);
            select(&mut editor, 2, 7);
            keys(&mut editor, &mut HelixState::default(), command);
            assert_eq!(editor.contents(), expected);
            assert_eq!(editor.history.undo_depth(), 1);
        }

        let mut data = editor(TABLE, Mode::Rendered);
        let mut state = HelixState::default();
        data.place_cursor(data.buffer.line_to_char(2));
        keys(&mut data, &mut state, "xd");
        assert!(!data.contents().contains("| one | two |"));
        assert_eq!(state.register.text, "| one | two |\n");
        assert_eq!(data.history.undo_depth(), 1);

        let mut whole = editor(TABLE, Mode::Rendered);
        let mut state = HelixState::default();
        keys(&mut whole, &mut state, "%d");
        assert!(whole.contents().is_empty());
        assert_eq!(whole.history.undo_depth(), 1);
    }

    #[test]
    fn table_safety_raw_and_structural_insert() {
        for (command, expected) in [
            ('I', "| Xalpha | bravo |\n|---|---|\n| one | two |\n"),
            ('A', "| alphaX | bravo |\n|---|---|\n| one | two |\n"),
        ] {
            let mut editor = editor(TABLE, Mode::Rendered);
            let mut state = HelixState::default();
            editor.place_cursor(5);
            key(&mut editor, &mut state, KeyCode::Char(command));
            assert_eq!(editor.cursor.offset, if command == 'I' { 2 } else { 7 });
            assert_eq!(editor.contents(), TABLE);
            edamame::editor::edit_ops::apply(&mut editor, Action::InsertChar('X'), 20, 80);
            assert_eq!(editor.contents(), expected);
            assert_eq!(state.sub_mode, HelixSubMode::Insert);
        }

        for (command, expected) in [
            (
                'o',
                "| alpha | bravo |\n|---|---|\n| one | two |\n|   |   |\n",
            ),
            (
                'O',
                "| alpha | bravo |\n|---|---|\n|   |   |\n| one | two |\n",
            ),
        ] {
            let mut editor = editor(TABLE, Mode::Rendered);
            let mut state = HelixState::default();
            editor.place_cursor(editor.buffer.line_to_char(2) + 3);
            key(&mut editor, &mut state, KeyCode::Char(command));
            assert_eq!(editor.contents(), expected);
            assert_eq!(state.sub_mode, HelixSubMode::Insert);
            assert_eq!(editor.history.undo_depth(), 1);
        }

        for command in ['o', 'O'] {
            let mut editor = editor("| a |\n|---|\n", Mode::Raw);
            let mut state = HelixState::default();
            editor.place_cursor(2);
            key(&mut editor, &mut state, KeyCode::Char(command));
            assert_eq!(
                editor.contents(),
                if command == 'o' {
                    "| a |\n\n|---|\n"
                } else {
                    "\n| a |\n|---|\n"
                }
            );
            assert_eq!(state.sub_mode, HelixSubMode::Insert);
        }

        for (command, register_text, linewise) in [("r|", "cached", false), ("R", "|\n", true)] {
            let mut editor = editor("| alpha | bravo |\n|---|---|\n| one | two |\n", Mode::Raw);
            let mut state = register(register_text, linewise);
            select(&mut editor, 3, 8);
            keys(&mut editor, &mut state, command);
            assert_eq!(
                editor.contents(),
                if command == "r|" {
                    "| a|||||| bravo |\n|---|---|\n| one | two |\n"
                } else {
                    "| a|\n| bravo |\n|---|---|\n| one | two |\n"
                }
            );
            assert_eq!(editor.history.undo_depth(), 1);
        }

        for offset in [1, 2, 3] {
            let mut empty = editor("|  |\n|---|\n", Mode::Rendered);
            select(&mut empty, offset, offset);
            assert!(empty.selection.unwrap().is_empty());
            let mut state = register("kept", false);
            keys(&mut empty, &mut state, "y");
            assert_eq!(state.register.text, "kept");
            keys(&mut empty, &mut state, "c");
            assert!(empty.selection.is_none());
            assert_eq!(state.sub_mode, HelixSubMode::Insert);
        }
        let mut rendered = editor("|  |\n|---|\n", Mode::Rendered);
        let mut state = register("kept", false);
        select(&mut rendered, 2, 2);
        let original = rendered.contents();
        let history = rendered.history.undo_depth();
        let _ = key(&mut rendered, &mut state, KeyCode::Char('d'));
        assert_eq!(rendered.contents(), original);
        assert_eq!(rendered.history.undo_depth(), history);
        assert_eq!(state.register.text, "kept");
    }
}
