mod helix {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use edamame::config::Theme;
    use edamame::document::{Buffer, Selection};
    use edamame::editor::{helix_ops, EditorState, Mode};
    use edamame::input::helix::{helix_feed, HelixState};
    use edamame::search::SearchState;

    fn send(editor: &mut EditorState, state: &mut HelixState, code: KeyCode) {
        helix_feed(
            state,
            editor,
            KeyEvent::new(code, KeyModifiers::NONE),
            20,
            80,
        );
    }
    fn keys(editor: &mut EditorState, state: &mut HelixState, text: &str) {
        for c in text.chars() {
            send(editor, state, KeyCode::Char(c));
        }
    }
    fn selected(editor: &EditorState) -> String {
        editor.selection.unwrap().selected_text(&editor.buffer)
    }

    // Utility: domain errors retain search causes without changing the selected document.
    #[test]
    fn typed_search_failures_preserve_causes_and_selection() {
        use edamame::editor::helix_ops::HelixError;
        use edamame::search::{EscapeError, SearchError};

        let mut editor = EditorState::new(
            Buffer::from_str("alpha"),
            Box::leak(Box::new(Theme::default())),
        );
        editor.mode = Mode::Raw;
        helix_ops::collapse_selection(&mut editor);
        let before = (editor.selection, editor.cursor, editor.history.undo_depth());
        for (query, expected, message) in [
            (None, HelixError::NoSearchPattern, "No search pattern"),
            (
                Some(""),
                HelixError::Search(SearchError::Empty),
                "Search term cannot be empty",
            ),
            (
                Some(r"\z"),
                HelixError::Search(SearchError::Escape(EscapeError::Unsupported('z'))),
                r"Unsupported escape: \z (use \\ for a literal backslash)",
            ),
            (Some("missing"), HelixError::NoMatches, "No matches"),
            (None, HelixError::NoMatches, "No matches"),
        ] {
            let result = match query {
                Some(query) => helix_ops::search::start(&mut editor, query.into(), true, false),
                None => helix_ops::search::next(&mut editor, true, 1, false),
            };
            let error = result.unwrap_err();
            assert_eq!(error, expected);
            assert_eq!(error.to_string(), message);
            assert_eq!(editor.contents(), "alpha");
            assert_eq!(
                (editor.selection, editor.cursor, editor.history.undo_depth()),
                before
            );
        }
    }

    #[test]
    fn search_selection_matches_and_extension() {
        let mut editor = EditorState::new(
            Buffer::from_str("é foo FOO\nfoo end"),
            Box::leak(Box::new(Theme::default())),
        );
        editor.mode = Mode::Raw;
        let mut state = HelixState::default();
        keys(&mut editor, &mut state, "/foo");
        send(&mut editor, &mut state, KeyCode::Enter);
        assert_eq!(selected(&editor), "foo");
        assert_eq!(editor.selection.unwrap().range(), (2, 5));
        keys(&mut editor, &mut state, "n");
        assert_eq!(selected(&editor), "FOO");
        keys(&mut editor, &mut state, "n");
        assert_eq!(editor.selection.unwrap().range(), (10, 13));
        keys(&mut editor, &mut state, "n");
        assert_eq!(editor.selection.unwrap().range(), (2, 5));
        keys(&mut editor, &mut state, "N");
        assert_eq!(editor.selection.unwrap().range(), (10, 13));
        keys(&mut editor, &mut state, "/FOO");
        send(&mut editor, &mut state, KeyCode::Enter);
        assert_eq!(selected(&editor), "FOO");
        assert_eq!(editor.search.as_ref().unwrap().matches.len(), 1);
        keys(&mut editor, &mut state, "?foo");
        send(&mut editor, &mut state, KeyCode::Enter);
        assert_eq!(editor.selection.unwrap().range(), (2, 5));
        keys(&mut editor, &mut state, "vn");
        assert_eq!(selected(&editor), "foo FOO");
        let before = editor.selection;
        keys(&mut editor, &mut state, "/absent");
        send(&mut editor, &mut state, KeyCode::Enter);
        assert_eq!(editor.selection, before);
        keys(&mut editor, &mut state, "/FOO\\nfoo");
        send(&mut editor, &mut state, KeyCode::Enter);
        assert_eq!(selected(&editor), "foo FOO\nfoo");
        send(&mut editor, &mut state, KeyCode::Esc);
        helix_ops::set_selection(
            &mut editor,
            Selection {
                anchor: 6,
                active: 13,
            },
        );
        helix_ops::actions::replace_selection(&mut editor, "gone").unwrap();
        keys(&mut editor, &mut state, "n");
        assert!(editor.search.as_ref().unwrap().matches.is_empty());
        assert_eq!(
            editor.selection.unwrap().selected_text(&editor.buffer),
            "gone"
        );
        assert!(editor
            .search
            .as_ref()
            .unwrap()
            .is_fresh(editor.buffer.version()));
    }

    // Utility regression for the shared range-extension geometry.
    #[test]
    fn selection_extension_preserves_orientation_and_fixed_grapheme() {
        let mut editor = EditorState::new(
            Buffer::from_str("abcdef"),
            Box::leak(Box::new(Theme::default())),
        );
        editor.mode = Mode::Raw;
        helix_ops::set_selection(
            &mut editor,
            Selection {
                anchor: 1,
                active: 3,
            },
        );
        helix_ops::select_cursor(&mut editor, 5, true);
        assert_eq!(
            editor.selection,
            Some(Selection {
                anchor: 1,
                active: 6
            })
        );
        helix_ops::select_cursor(&mut editor, 0, true);
        assert_eq!(
            editor.selection,
            Some(Selection {
                anchor: 2,
                active: 0
            })
        );
        assert_eq!(selected(&editor), "ab");

        editor.search = Some(SearchState::new("bcde".into(), None).unwrap());
        let contents = editor.contents();
        let version = editor.buffer.version();
        editor
            .search
            .as_mut()
            .unwrap()
            .ensure_fresh(&contents, version);
        helix_ops::set_selection(
            &mut editor,
            Selection {
                anchor: 4,
                active: 2,
            },
        );
        editor.search.as_mut().unwrap().focused_idx = 0;
        helix_ops::search::select_focused(&mut editor, true).unwrap();
        assert_eq!(
            editor.selection,
            Some(Selection {
                anchor: 5,
                active: 1
            })
        );
        assert_eq!(selected(&editor), "bcde");

        for (text, mode, at, target, expected) in [
            ("", Mode::Raw, 0, 0, (0, 0)),
            ("abc", Mode::Raw, 0, usize::MAX, (0, 3)),
            ("|  |\n|---|\n", Mode::Rendered, 2, 2, (2, 2)),
        ] {
            let mut editor = EditorState::new(
                Buffer::from_str(text),
                Box::leak(Box::new(Theme::default())),
            );
            editor.mode = mode;
            editor.place_cursor(at);
            helix_ops::set_selection(&mut editor, Selection::new(at));
            helix_ops::select_cursor(&mut editor, target, true);
            assert_eq!(editor.selection.unwrap().range(), expected);
        }
    }

    // Utility regression for the shared range-extension geometry.
    #[test]
    fn selection_extension_snaps_inside_a_long_unicode_grapheme() {
        let combining = "\u{301}".repeat(80);
        let text = format!("xa{combining}z");
        let mut editor = EditorState::new(
            Buffer::from_str(&text),
            Box::leak(Box::new(Theme::default())),
        );
        editor.mode = Mode::Raw;
        helix_ops::set_selection(
            &mut editor,
            Selection {
                anchor: 0,
                active: 1,
            },
        );
        helix_ops::select_cursor(&mut editor, 40, true);
        assert_eq!(
            editor.selection,
            Some(Selection {
                anchor: 0,
                active: 82
            })
        );
        assert_eq!(selected(&editor), format!("xa{combining}"));

        helix_ops::set_selection(
            &mut editor,
            Selection {
                anchor: 82,
                active: 0,
            },
        );
        editor.search = Some(SearchState::new("\u{301}".into(), None).unwrap());
        editor
            .search
            .as_mut()
            .unwrap()
            .ensure_fresh(&text, editor.buffer.version());
        helix_ops::search::select_focused(&mut editor, true).unwrap();
        assert_eq!(
            editor.selection,
            Some(Selection {
                anchor: 1,
                active: 82
            })
        );
        assert_eq!(selected(&editor), format!("a{combining}"));
    }

    // Utility regression for the shared range-extension geometry.
    #[test]
    fn search_then_select_navigation_uses_the_shared_extension_geometry() {
        let mut editor = EditorState::new(
            Buffer::from_str("x foo!"),
            Box::leak(Box::new(Theme::default())),
        );
        editor.mode = Mode::Raw;
        let mut state = HelixState::default();
        keys(&mut editor, &mut state, "/foo");
        send(&mut editor, &mut state, KeyCode::Enter);
        assert_eq!(editor.selection.unwrap().range(), (2, 5));
        keys(&mut editor, &mut state, "vl");
        assert_eq!(
            editor.selection,
            Some(Selection {
                anchor: 2,
                active: 6
            })
        );
        assert_eq!(selected(&editor), "foo!");
    }
}
