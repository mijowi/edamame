mod helix {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use edamame::config::Theme;
    use edamame::document::{Buffer, Selection};
    use edamame::editor::vim_ops::Operator;
    use edamame::editor::{helix_ops, EditorState, Mode};
    use edamame::input::helix::{
        helix_feed, HelixOutcome, HelixPending, HelixRegister, HelixState, HelixSubMode,
    };

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn make_state(text: &str) -> EditorState {
        EditorState::new(Buffer::from_str(text), theme())
    }

    // Utility regression: shared shortcut classification agrees with actual reducer ownership.
    #[test]
    fn app_shortcuts_preserve_modes_modifiers_and_pending_resets() {
        use edamame::input::helix::is_app_shortcut;
        use KeyModifiers as M;

        // code, modifiers, shortcut in Normal/Select, shortcut in Insert, Insert delegates.
        let cases = [
            (KeyCode::Char('s'), M::CONTROL, true, true, true),
            (KeyCode::Char('s'), M::SUPER, true, true, true),
            (KeyCode::Char(';'), M::ALT, false, true, true),
            (KeyCode::Char(';'), M::ALT | M::SHIFT, false, true, true),
            (KeyCode::Char(';'), M::CONTROL | M::ALT, true, true, true),
            (KeyCode::Char(';'), M::SUPER | M::ALT, true, true, true),
            (KeyCode::Char('z'), M::ALT, true, true, true),
            (KeyCode::F(1), M::NONE, true, true, true),
            (KeyCode::PageUp, M::NONE, true, true, true),
            (KeyCode::PageDown, M::SHIFT, true, true, true),
            (KeyCode::Char('w'), M::NONE, false, false, true),
            (KeyCode::Char('W'), M::SHIFT, false, false, true),
            (KeyCode::Left, M::NONE, false, false, true),
            (KeyCode::Esc, M::NONE, false, false, false),
            (KeyCode::Esc, M::SHIFT, false, false, false),
            (KeyCode::Esc, M::CONTROL, true, true, true),
        ];
        for mode in [
            HelixSubMode::Normal,
            HelixSubMode::Select,
            HelixSubMode::Insert,
        ] {
            for (code, modifiers, normal_shortcut, insert_shortcut, insert_delegates) in cases {
                let key = KeyEvent::new(code, modifiers);
                let insert = mode == HelixSubMode::Insert;
                assert_eq!(
                    is_app_shortcut(key, mode),
                    if insert {
                        insert_shortcut
                    } else {
                        normal_shortcut
                    }
                );
                let mut editor = make_state("abc");
                let mut state = HelixState {
                    sub_mode: mode,
                    ..HelixState::default()
                };
                if !insert {
                    helix_ops::set_selection(
                        &mut editor,
                        Selection {
                            anchor: 0,
                            active: 2,
                        },
                    );
                    state.count = Some(2);
                    state.pending = HelixPending::G;
                }
                let outcome = helix_feed(&mut state, &mut editor, key, 20, 80);
                assert_eq!(
                    outcome == HelixOutcome::Passthrough,
                    if insert {
                        insert_delegates
                    } else {
                        normal_shortcut
                    },
                    "{mode:?}: {key:?}"
                );
                if !insert && normal_shortcut {
                    assert_eq!(state.count, None);
                    assert_eq!(state.pending, HelixPending::None);
                }
                if !insert && !normal_shortcut && modifiers.contains(M::ALT) {
                    assert_eq!(
                        editor.selection,
                        Some(Selection {
                            anchor: 2,
                            active: 0
                        })
                    );
                    assert_eq!(state.count, None);
                    assert_eq!(state.pending, HelixPending::None);
                }
            }
        }
    }

    #[test]
    fn selection_geometry_unicode_and_boundaries() {
        let mut state = make_state("e\u{301}👨‍👩‍👧‍👦日\r\nx");
        helix_ops::set_selection(
            &mut state,
            Selection {
                anchor: 1,
                active: 8,
            },
        );
        let selection = state.selection.unwrap();
        assert_eq!(selection.range(), (0, 9));
        assert_eq!(state.cursor.offset, 2);
        assert_eq!(selection.selected_text(&state.buffer), "e\u{301}👨‍👩‍👧‍👦");

        helix_ops::set_selection(
            &mut state,
            Selection {
                anchor: 8,
                active: 1,
            },
        );
        assert_eq!(state.selection.unwrap().range(), (0, 9));
        assert_eq!(state.cursor.offset, 0);

        helix_ops::set_selection(
            &mut state,
            Selection {
                anchor: 99,
                active: 99,
            },
        );
        assert_eq!(
            state.selection.unwrap().range(),
            (state.buffer.len_chars() - 1, state.buffer.len_chars())
        );

        let mut empty = make_state("");
        helix_ops::normalize_selection(&mut empty);
        assert_eq!(empty.selection.unwrap().range(), (0, 0));
    }

    #[test]
    fn selection_geometry_collapse_and_flip() {
        let mut state = make_state("a👨‍👩‍👧‍👦z");
        helix_ops::set_selection(
            &mut state,
            Selection {
                anchor: 1,
                active: 9,
            },
        );
        let text = state.selection.unwrap().selected_text(&state.buffer);
        helix_ops::flip_selection(&mut state);
        assert_eq!(state.selection.unwrap().selected_text(&state.buffer), text);
        assert_eq!(state.cursor.offset, 1);
        helix_ops::collapse_selection(&mut state);
        assert_eq!(state.selection.unwrap().selected_text(&state.buffer), "👨‍👩‍👧‍👦");
        assert_eq!(state.buffer.contents(), "a👨‍👩‍👧‍👦z");

        let mut wide = make_state("日a");
        helix_ops::set_selection(
            &mut wide,
            Selection {
                anchor: 2,
                active: 1,
            },
        );
        assert_eq!(wide.cursor.offset, 1);
        assert_eq!(wide.cursor.preferred_col, 2);
    }

    #[test]
    fn selection_geometry_range_conversion_utilities() {
        let buffer = Buffer::from_str("e\u{301}x\r\ny");
        assert_eq!(helix_ops::normalize_range(&buffer, (1, 4)), (0, 4));
        assert_eq!(
            helix_ops::cursor_offset(
                &buffer,
                Selection {
                    anchor: 2,
                    active: 4
                }
            ),
            3
        );
        assert_eq!(
            helix_ops::cursor_offset(
                &buffer,
                Selection {
                    anchor: 4,
                    active: 2
                }
            ),
            2
        );

        let multiline = Buffer::from_str("abc\n👨‍👩‍👧‍👦z");
        assert_eq!(helix_ops::normalize_range(&multiline, (2, 6)), (2, 11));
    }

    /// Utility: compare every character position with whole-string Unicode segmentation.
    #[test]
    fn grapheme_snapping_across_rope_chunks() {
        use unicode_segmentation::UnicodeSegmentation;

        let texts = [
            String::new(),
            "a\r\nb\rc\n日\u{2028}z".into(),
            format!("{}e{}z", "a".repeat(1023), "\u{301}".repeat(700)),
            format!("{}{}z", "x".repeat(1021), "👨‍👩‍👧‍👦".repeat(200)),
            format!("{}{}z", "x".repeat(1022), "🇫".repeat(701)),
            format!("{}{}z", "x".repeat(1022), "क्‍".repeat(350)),
        ];
        for text in texts {
            let buffer = Buffer::from_rope(ropey::Rope::from_str(&text));
            let mut boundaries = vec![0];
            let mut at = 0;
            for grapheme in text.graphemes(true) {
                at += grapheme.chars().count();
                boundaries.push(at);
            }
            for at in 0..=buffer.len_chars() {
                let lo = boundaries[boundaries.partition_point(|&b| b <= at) - 1];
                let hi = boundaries[boundaries.partition_point(|&b| b < at)];
                assert_eq!(
                    helix_ops::normalize_range(&buffer, (at, at)),
                    (lo, hi),
                    "at {at}"
                );
            }
        }
    }

    /// Utility coverage for the shared Helix selection operator entry point.
    #[test]
    fn selection_operator_delete_change_and_yank() {
        let mut deleted = make_state("a日b");
        helix_ops::set_selection(
            &mut deleted,
            Selection {
                anchor: 1,
                active: 2,
            },
        );
        let result = helix_ops::apply_operator(&mut deleted, Operator::Delete, false).unwrap();
        assert_eq!(result.register_text, "日");
        assert_eq!(deleted.buffer.contents(), "ab");
        assert_eq!(
            deleted.selection.unwrap().selected_text(&deleted.buffer),
            "b"
        );
        assert_eq!(deleted.history.undo_depth(), 1);

        let mut changed = make_state("first second");
        helix_ops::set_selection(
            &mut changed,
            Selection {
                anchor: 0,
                active: 5,
            },
        );
        let result = helix_ops::apply_operator(&mut changed, Operator::Change, false).unwrap();
        assert_eq!(result.register_text, "first");
        assert!(result.enter_insert);
        assert_eq!(changed.buffer.contents(), " second");
        assert!(changed.selection.is_none());

        let mut yanked = make_state("日x");
        helix_ops::set_selection(
            &mut yanked,
            Selection {
                anchor: 2,
                active: 0,
            },
        );
        let prior_selection = yanked.selection;
        let prior_cursor = yanked.cursor.offset;
        let result = helix_ops::apply_operator(&mut yanked, Operator::Yank, false).unwrap();
        assert_eq!(result.register_text, "日x");
        assert_eq!(yanked.buffer.contents(), "日x");
        assert_eq!(yanked.selection, prior_selection);
        assert_eq!(yanked.cursor.offset, prior_cursor);
        assert_eq!(yanked.history.undo_depth(), 0);
    }

    /// Utility coverage for empty and read-only operator refusal paths.
    #[test]
    fn selection_operator_empty_and_readonly_are_noops() {
        let mut empty = make_state("");
        helix_ops::set_selection(&mut empty, Selection::new(0));
        let result = helix_ops::apply_operator(&mut empty, Operator::Delete, false).unwrap();
        assert!(result.register_text.is_empty());
        assert_eq!(empty.buffer.contents(), "");
        assert_eq!(empty.history.undo_depth(), 0);

        let mut readonly = make_state("keep");
        helix_ops::set_selection(
            &mut readonly,
            Selection {
                anchor: 0,
                active: 2,
            },
        );
        readonly.readonly = true;
        let selection = readonly.selection;
        let cursor = readonly.cursor.offset;
        assert!(helix_ops::apply_operator(&mut readonly, Operator::Change, false).is_err());
        assert_eq!(readonly.buffer.contents(), "keep");
        assert_eq!(readonly.selection, selection);
        assert_eq!(readonly.cursor.offset, cursor);
        assert_eq!(readonly.history.undo_depth(), 0);
    }

    /// Utility coverage for table range refusal and permitted complete table rows.
    #[test]
    fn selection_operator_table_guards() {
        let table = "| alpha | bravo |\n|---|---|\n| one | two |\n";
        let mut header = make_state(table);
        header.mode = Mode::Rendered;
        let header_end = header.buffer.line_to_char(1);
        helix_ops::set_selection(
            &mut header,
            Selection {
                anchor: 0,
                active: header_end,
            },
        );
        let original = header.buffer.contents();
        let mut helix = HelixState {
            register: HelixRegister {
                text: "cached".to_owned(),
                linewise: false,
            },
            ..HelixState::default()
        };
        let outcome = helix_feed(
            &mut helix,
            &mut header,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            20,
            80,
        );
        assert!(matches!(outcome, HelixOutcome::Flash(_)));
        assert_eq!(header.buffer.contents(), original);
        assert_eq!(header.history.undo_depth(), 0);
        assert_eq!(helix.register.text, "cached");

        let mut across_cells = make_state(table);
        across_cells.mode = Mode::Rendered;
        let h = table.find("alpha").unwrap();
        let i = table.find("bravo").unwrap();
        helix_ops::set_selection(
            &mut across_cells,
            Selection {
                anchor: h,
                active: i + 1,
            },
        );
        assert!(helix_ops::apply_operator(&mut across_cells, Operator::Change, false).is_err());
        assert_eq!(across_cells.buffer.contents(), table);
        assert_eq!(across_cells.history.undo_depth(), 0);

        let mut data_row = make_state(table);
        data_row.mode = Mode::Rendered;
        let row_start = data_row.buffer.line_to_char(2);
        let row_end = data_row.buffer.line_to_char(3);
        helix_ops::set_selection(
            &mut data_row,
            Selection {
                anchor: row_start,
                active: row_end,
            },
        );
        assert!(helix_ops::apply_operator(&mut data_row, Operator::Delete, false).is_ok());
        assert!(!data_row.buffer.contents().contains("| one | two |"));

        let mut whole_table = make_state(table);
        whole_table.mode = Mode::Rendered;
        let table_len = whole_table.buffer.len_chars();
        helix_ops::set_selection(
            &mut whole_table,
            Selection {
                anchor: 0,
                active: table_len,
            },
        );
        assert!(helix_ops::apply_operator(&mut whole_table, Operator::Delete, false).is_ok());
        assert!(whole_table.buffer.contents().is_empty());
    }

    /// Utility coverage for the d/c/y key path and its session-local register.
    #[test]
    fn selection_operator_keys_update_helix_register() {
        let key = |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
        let mut editor = make_state("abc");
        helix_ops::set_selection(
            &mut editor,
            Selection {
                anchor: 0,
                active: 1,
            },
        );
        let mut helix = HelixState::default();
        assert!(matches!(
            helix_feed(&mut helix, &mut editor, key('y'), 20, 80),
            edamame::input::helix::HelixOutcome::Consumed
        ));
        assert_eq!(helix.register.text, "a");
        assert_eq!(editor.selection.unwrap().selected_text(&editor.buffer), "a");
        assert_eq!(helix.sub_mode, HelixSubMode::Normal);
        assert!(matches!(
            helix_feed(&mut helix, &mut editor, key('c'), 20, 80),
            edamame::input::helix::HelixOutcome::Consumed
        ));
        assert_eq!(editor.buffer.contents(), "bc");
        assert_eq!(helix.register.text, "a");
        assert_eq!(helix.sub_mode, HelixSubMode::Insert);
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        helix_feed(&mut helix, &mut editor, esc, 20, 80);
        assert!(matches!(
            helix_feed(&mut helix, &mut editor, key('d'), 20, 80),
            edamame::input::helix::HelixOutcome::Consumed
        ));
        assert_eq!(editor.buffer.contents(), "c");
        assert_eq!(editor.selection.unwrap().selected_text(&editor.buffer), "c");
        assert_eq!(helix.register.text, "b");
        assert_eq!(helix.sub_mode, HelixSubMode::Normal);
    }

    fn feed_keys(state: &mut HelixState, editor: &mut EditorState, keys: &str) {
        for ch in keys.chars() {
            let key = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
            helix_feed(state, editor, key, 20, 80);
        }
    }

    fn feed_alt_semicolon(state: &mut HelixState, editor: &mut EditorState) {
        let key = KeyEvent::new(KeyCode::Char(';'), KeyModifiers::ALT);
        helix_feed(state, editor, key, 20, 80);
    }

    fn selected_text(editor: &EditorState) -> String {
        editor.selection.unwrap().selected_text(&editor.buffer)
    }

    #[test]
    fn word_selection_select_then_act() {
        let mut editor = make_state("one two three");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "w");
        assert_eq!(selected_text(&editor), "one ");
        feed_keys(&mut helix, &mut editor, "d");
        assert_eq!(editor.buffer.contents(), "two three");
        assert_eq!(helix.sub_mode, HelixSubMode::Normal);

        let mut editor = make_state("one two three");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "e");
        assert_eq!(selected_text(&editor), "one");
        feed_keys(&mut helix, &mut editor, "c");
        assert_eq!(editor.buffer.contents(), " two three");
        assert_eq!(helix.sub_mode, HelixSubMode::Insert);
    }

    #[test]
    fn word_selection_direction_and_extension() {
        for (motion, offset) in [('w', 0), ('e', 0), ('b', 10), ('W', 0), ('E', 0), ('B', 10)] {
            for select in [false, true] {
                let mut counted = make_state("one two three");
                counted.place_cursor(offset);
                let mut repeated = make_state("one two three");
                repeated.place_cursor(offset);
                let mut a = HelixState::default();
                let mut b = HelixState::default();
                let prefix = if select { "v" } else { "" };
                feed_keys(&mut a, &mut counted, &format!("{prefix}2{motion}"));
                feed_keys(&mut b, &mut repeated, &format!("{prefix}{motion}{motion}"));
                assert_eq!(
                    counted.selection, repeated.selection,
                    "count {prefix}{motion}"
                );
            }
        }
        let mut editor = make_state("one two three");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "2w");
        assert_eq!(selected_text(&editor), "two ");

        let mut editor = make_state("one two three");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "v2w");
        assert_eq!(selected_text(&editor), "one two ");

        let mut editor = make_state("one two three");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "ww");
        assert_eq!(selected_text(&editor), "two ");

        let mut editor = make_state("one two three");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "vww");
        assert_eq!(selected_text(&editor), "one two ");

        let mut editor = make_state("one two three");
        editor.place_cursor(6);
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "b");
        assert_eq!(selected_text(&editor), "two");

        let mut editor = make_state("one two three");
        editor.place_cursor(4);
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "b");
        assert_eq!(selected_text(&editor), "one ");

        let mut editor = make_state("one two three");
        editor.place_cursor(10);
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "2b");
        assert_eq!(selected_text(&editor), "two ");

        let mut editor = make_state("one two three");
        editor.place_cursor(10);
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "v2b");
        assert_eq!(selected_text(&editor), "two thr");

        let mut editor = make_state("one two");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "ee");
        assert_eq!(selected_text(&editor), " two");

        let mut editor = make_state("one two");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "vee");
        assert_eq!(selected_text(&editor), "one two");

        let mut editor = make_state("one-two three");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "W");
        assert_eq!(selected_text(&editor), "one-two ");

        let mut editor = make_state("one-two three");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "vW");
        assert_eq!(selected_text(&editor), "one-two ");

        let mut editor = make_state("one-two three");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "E");
        assert_eq!(selected_text(&editor), "one-two");

        let mut editor = make_state("one-two three");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "vE");
        assert_eq!(selected_text(&editor), "one-two");

        let mut editor = make_state("one-two three");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "w");
        assert_eq!(selected_text(&editor), "one");
        feed_keys(&mut helix, &mut editor, "w");
        assert_eq!(selected_text(&editor), "-");

        let mut editor = make_state("one-two three");
        editor.place_cursor(8);
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "B");
        assert_eq!(selected_text(&editor), "one-two ");

        let mut editor = make_state("one-two three");
        editor.place_cursor(8);
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "vB");
        assert_eq!(selected_text(&editor), "one-two t");

        let mut editor = make_state("one\ntwo");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "w");
        assert_eq!(selected_text(&editor), "one");
        feed_keys(&mut helix, &mut editor, "w");
        assert_eq!(selected_text(&editor), "two");

        let mut editor = make_state("one\ntwo");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "vww");
        assert_eq!(selected_text(&editor), "one\ntwo");

        let mut editor = make_state("e\u{301}lan 👨‍👩‍👧‍👦日");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "w");
        assert_eq!(selected_text(&editor), "e\u{301}lan ");
        let selection = editor.selection.unwrap();
        assert_eq!(selection.range(), (0, 6));

        let mut editor = make_state("one");
        editor.place_cursor(2);
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "w");
        let once = editor.selection;
        feed_keys(&mut helix, &mut editor, "w");
        assert_eq!(editor.selection, once);

        let mut editor = make_state("one two");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "v");
        feed_alt_semicolon(&mut helix, &mut editor);
        feed_keys(&mut helix, &mut editor, "w");
        assert_eq!(selected_text(&editor), "one ");
    }
    #[test]
    fn line_document_selection_line_and_document_matrix() {
        for mode in [Mode::Raw, Mode::Rendered] {
            for (keys, expected) in [
                ("x", "a\n"),
                ("xx", "a\nb\n"),
                ("2x", "a\nb\n"),
                ("X", "a\n"),
                ("%", "a\nb\nlast"),
            ] {
                let mut editor = make_state("a\nb\nlast");
                editor.mode = mode;
                let mut helix = HelixState::default();
                feed_keys(&mut helix, &mut editor, keys);
                assert_eq!(selected_text(&editor), expected, "{mode:?} {keys}");
            }
            for keys in ["xd", "xyd"] {
                let mut editor = make_state("a\nb\nlast");
                editor.mode = mode;
                let mut helix = HelixState::default();
                feed_keys(&mut helix, &mut editor, keys);
                assert_eq!(editor.contents(), "b\nlast");
                assert_eq!(helix.register.text, "a\n");
                assert!(helix.register.linewise);
            }
            let mut editor = make_state("a\nb\nlast");
            editor.mode = mode;
            helix_ops::set_selection(
                &mut editor,
                Selection {
                    anchor: 8,
                    active: 4,
                },
            );
            let mut helix = HelixState::default();
            feed_keys(&mut helix, &mut editor, "X");
            assert_eq!(selected_text(&editor), "last");
            assert!(editor.selection.unwrap().anchor > editor.selection.unwrap().active);
            feed_keys(&mut helix, &mut editor, "%d");
            assert_eq!(editor.contents(), "");
            edamame::editor::edit_ops::apply(&mut editor, edamame::config::Action::Undo, 20, 80);
            assert_eq!(editor.contents(), "a\nb\nlast");
        }
        let mut empty_line = make_state("\nnext");
        helix_ops::normalize_selection(&mut empty_line);
        let before = empty_line.selection;
        feed_keys(&mut HelixState::default(), &mut empty_line, "X");
        assert_eq!(empty_line.selection, before);
    }

    #[test]
    fn character_find_find_matrix() {
        for select in [false, true] {
            for (keys, origin, expected) in [
                ("fb", 0, "a b"),
                ("tb", 0, "a "),
                ("2fb", 0, "a b\nb"),
                ("Fa", 4, "a b\nb"),
                ("Ta", 4, " b\nb"),
                ("2Fb", 6, "b\nb c"),
            ] {
                let mut editor = make_state("a b\nb c");
                editor.place_cursor(origin);
                let mut helix = HelixState::default();
                if select {
                    feed_keys(&mut helix, &mut editor, "v");
                }
                feed_keys(&mut helix, &mut editor, keys);
                assert_eq!(selected_text(&editor), expected, "{select} {keys}");
            }
        }
        let mut editor = make_state("a e\u{301} 日");
        let mut helix = HelixState::default();
        feed_keys(&mut helix, &mut editor, "fe");
        assert_eq!(selected_text(&editor), "a e\u{301}");
        let before = editor.selection;
        feed_keys(&mut helix, &mut editor, "fz");
        assert_eq!(editor.selection, before);
        feed_keys(&mut helix, &mut editor, "f");
        helix_feed(
            &mut helix,
            &mut editor,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            20,
            80,
        );
        feed_keys(&mut helix, &mut editor, "i");
        assert_eq!(helix.sub_mode, HelixSubMode::Insert);
        let table = "| alpha | bravo |\n|---|---|\n| one | two |\n";
        let mut editor = make_state(table);
        editor.mode = Mode::Rendered;
        editor.place_cursor(2);
        helix_ops::normalize_selection(&mut editor);
        let before = editor.selection;
        feed_keys(&mut HelixState::default(), &mut editor, "fb");
        assert_eq!(editor.selection, before);
    }
    #[test]
    fn discoverability_staged_multiple_selection_goal() {
        let manual = include_str!("../docs/helix-mode.md");
        let design = include_str!("../docs/dev/helix-mode.md");
        for page in [manual, design] {
            let future = page.split("## Future multiple selections").nth(1).unwrap();
            for contract in [
                "primary selection",
                "overlap",
                "remap",
                "validate every range",
                "one undo transaction",
            ] {
                assert!(
                    future.contains(contract),
                    "missing future contract: {contract}"
                );
            }
        }
        assert!(manual.contains("Only one selection is available today"));
        assert!(manual.contains("separate change"));
        assert!(design.contains("No collection or pretend one-element multiselection loop"));
    }
    // Utility regression: modified Escape is a global chord outside a prompt.
    #[test]
    fn modified_escape_keeps_global_passthrough() {
        for modifier in [
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
            KeyModifiers::SUPER,
        ] {
            let mut editor = make_state("abc");
            let mut state = HelixState::default();
            let result = helix_feed(
                &mut state,
                &mut editor,
                KeyEvent::new(KeyCode::Esc, modifier),
                20,
                80,
            );
            assert_eq!(result, edamame::input::helix::HelixOutcome::Passthrough);
        }
    }
}
