//! Scenario tests for the Helix application lifecycle.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::config::{Action, Config};
use crate::document::{Buffer, Selection};
use crate::editor::Mode;
use crate::input::helix::HelixPending;
use crate::input::{HelixSubMode, ModalInput};
use crate::ui::{SettingsResponse, SettingsState};

use super::event_loop::DocDims;
use super::modal::{Modal, ModalOutcome, WelcomeModal};
use super::App;

// Utility fixture: no configuration writes, terminal, watcher or OS clipboard access.
fn app(handler: &str, text: &str) -> App {
    let mut config = Config::default();
    config.modal.handler = handler.to_owned();
    let mut app = App::new(
        config,
        crate::config::State::default(),
        crate::config::KeyBindingOverrides::default(),
        (&crate::config::Theme::default()).into(),
        None,
        crate::terminal::Capabilities::minimal(),
        Vec::new(),
    )
    .unwrap();
    while app.modal_stack.pop().is_some() {}
    app.editor.replace_buffer(Buffer::from_str(text));
    app.sync_modal_suspension();
    app
}

#[test]
fn selection_geometry_external_edits_and_pointer() {
    let _iso = crate::test_env::config_isolation();
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    let mut app = app("helix", "abcdefghij");
    app.editor.mode = Mode::Raw;
    app.capabilities.mouse = true;
    for (kind, column) in [
        (MouseEventKind::Down(MouseButton::Left), 3),
        (MouseEventKind::Drag(MouseButton::Left), 6),
        (MouseEventKind::Up(MouseButton::Left), 6),
    ] {
        app.dispatch_mouse_event(
            MouseEvent {
                kind,
                column,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            &dims(),
        );
    }
    assert_eq!(
        app.editor
            .selection
            .unwrap()
            .selected_text(&app.editor.buffer),
        "def"
    );
    assert_eq!(app.editor.cursor.offset, 5);
    assert_eq!(app.editor.cursor.preferred_col, 5);
    app.dispatch_action(Action::DeleteCharForward, 20, 80);
    assert_eq!(app.editor.buffer.contents(), "abcghij");
    assert_eq!(
        app.editor
            .selection
            .unwrap()
            .selected_text(&app.editor.buffer),
        "g"
    );
    app.dispatch_action(Action::Undo, 20, 80);
    assert_eq!(app.editor.buffer.contents(), "abcdefghij");
    assert_eq!(app.editor.selection.unwrap().range(), (6, 7));
    app.dispatch_action(Action::Redo, 20, 80);
    assert_eq!(app.editor.buffer.contents(), "abcghij");
    assert_eq!(app.editor.selection.unwrap().range(), (3, 4));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.md");
    std::fs::write(&path, "Z").unwrap();
    app.load_file_into_editor(path).unwrap();
    assert_eq!(app.editor.selection.unwrap().range(), (0, 1));
    assert_eq!(
        app.editor
            .selection
            .unwrap()
            .selected_text(&app.editor.buffer),
        "Z"
    );
    app.modal_input.helix_mut().unwrap().sub_mode = HelixSubMode::Select;
    app.reload_buffer_from_disk("e\u{301} renewed\r\n".to_owned());
    assert_eq!(
        app.modal_input.helix().unwrap().sub_mode,
        HelixSubMode::Normal
    );
    assert_eq!(app.editor.selection.unwrap().range(), (0, 2));
    app.enter_search_flow("renewed".into(), None);
    assert_eq!(
        app.editor
            .selection
            .unwrap()
            .selected_text(&app.editor.buffer),
        "renewed"
    );
    app.dispatch_action(Action::SearchNext, 20, 80);
    assert_eq!(
        app.editor
            .selection
            .unwrap()
            .selected_text(&app.editor.buffer),
        "renewed"
    );
    app.dispatch_paste("updated".into(), &dims());
    assert_eq!(app.editor.contents(), "e\u{301} updated\n");
    key(&mut app, KeyCode::Char('n'));
    assert_eq!(
        app.editor
            .selection
            .unwrap()
            .selected_text(&app.editor.buffer),
        "updated"
    );
    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Char('x'));
    app.dispatch_action(Action::MoveRight, 20, 80);
    assert!(app.modal_input.helix().unwrap().prompt.is_none());
    let mut extending = self::app("helix", "a first second");
    key(&mut extending, KeyCode::Char('v'));
    extending.enter_search_flow("second".into(), None);
    assert_eq!(
        extending
            .editor
            .selection
            .unwrap()
            .selected_text(&extending.editor.buffer),
        "a first second"
    );
    let before = extending.editor.selection;
    extending.enter_search_flow("absent".into(), None);
    assert_eq!(extending.editor.selection, before);
}

fn dims() -> DocDims {
    DocDims {
        doc_height: 20,
        doc_width: 80,
        doc_area: Rect::new(0, 0, 80, 20),
    }
}

fn key(app: &mut App, code: KeyCode) {
    let map = app.ensure_keymap_clone();
    app.dispatch_single_key(
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
        &map,
        &dims(),
    );
}

#[test]
fn handler_selection_startup_and_live_switch() {
    let _iso = crate::test_env::config_isolation();
    let mut app = app("helix", "one two");
    assert_eq!(app.editor.mode, Mode::Rendered);
    assert_eq!(
        app.modal_input.helix().unwrap().sub_mode,
        HelixSubMode::Normal
    );
    let original = app.editor.buffer.contents();
    let mut settings = SettingsState::new();
    // The editing handler is the final row, reached through the public keyboard path.
    for _ in 0..40 {
        settings.handle_key(
            &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &mut app.config,
        );
    }
    if let SettingsResponse::FieldChanged(label) = settings.handle_key(
        &KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut app.config,
    ) {
        super::modal::settings::apply_live_update(label, &mut app);
    } else {
        panic!("editing mode pill must change")
    }
    assert_eq!(app.config.modal.handler, "default");
    app.dispatch_action(Action::ToggleHelixMode, 20, 80);
    assert!(app.modal_input.helix().is_some());
    assert_eq!(app.config.modal.handler, "helix");
    assert_eq!(
        app.transient.as_ref().unwrap().text,
        format!("Editing mode: Helix{}", crate::config::NOT_PERSISTED_NOTE)
    );
    app.dispatch_action(Action::ToggleHelixMode, 20, 80);
    assert!(matches!(&app.modal_input, ModalInput::Default));
    assert_eq!(app.config.modal.handler, "default");
    assert_eq!(
        app.transient.as_ref().unwrap().text,
        format!("Editing mode: Default{}", crate::config::NOT_PERSISTED_NOTE)
    );
    app.dispatch_action(Action::ToggleVimMode, 20, 80);
    assert!(app.modal_input.vim().is_some());
    assert_eq!(app.config.modal.handler, "vim");
    assert_eq!(
        app.transient.as_ref().unwrap().text,
        format!("Editing mode: Vim{}", crate::config::NOT_PERSISTED_NOTE)
    );
    app.dispatch_action(Action::ToggleVimMode, 20, 80);
    assert!(matches!(&app.modal_input, ModalInput::Default));
    assert_eq!(
        app.transient.as_ref().unwrap().text,
        format!("Editing mode: Default{}", crate::config::NOT_PERSISTED_NOTE)
    );
    app.dispatch_action(Action::ToggleHelixMode, 20, 80);
    assert!(app.modal_input.helix().is_some());
    assert_eq!(app.config.modal.handler, "helix");
    assert_eq!(
        app.transient.as_ref().unwrap().text,
        format!("Editing mode: Helix{}", crate::config::NOT_PERSISTED_NOTE)
    );
    let round: Config = toml::from_str(&toml::to_string(&app.config).unwrap()).unwrap();
    assert_eq!(round.modal.handler, "helix");
    // The welcome's public Save path must preserve and apply the chosen handler.
    let mut welcome = WelcomeModal::new(&app.capabilities, &app.config);
    app.modal_input = ModalInput::Default;
    welcome.focus_save_for_test();
    let result = welcome.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut app,
        20,
        80,
    );
    match result {
        ModalOutcome::CloseAnd(save) => save(&mut app),
        _ => panic!("save outcome"),
    }
    assert!(app.modal_input.helix().is_some());
    app.set_modal_handler("vim");
    assert!(app.modal_input.vim().is_some());
    app.set_modal_handler("default");
    assert!(!app.modal_input.is_active());
    assert_eq!(app.editor.buffer.contents(), original);
}

#[test]
fn handler_selection_compatibility() {
    let _iso = crate::test_env::config_isolation();
    assert_eq!(Config::default().modal.handler, "default");
    for handler in ["default", "unknown", "vim", "helix"] {
        let mut app = app(handler, "text");
        assert_eq!(app.modal_input.vim().is_some(), handler == "vim");
        assert_eq!(app.modal_input.helix().is_some(), handler == "helix");
        if handler == "helix" {
            app.dispatch_action(Action::ToggleVimMode, 20, 80);
            assert!(app.modal_input.vim().is_some());
            assert_eq!(app.config.modal.handler, "vim");
        }
    }
}

#[test]
fn interaction_modes_transitions() {
    let _iso = crate::test_env::config_isolation();
    let mut app = app("helix", "abc");
    for view in [Mode::Raw, Mode::Rendered] {
        app.editor.mode = view;
        key(&mut app, KeyCode::Char('v'));
        assert_eq!(
            app.modal_input.helix().unwrap().sub_mode,
            HelixSubMode::Select
        );
        let selected = app.editor.selection;
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.editor.selection, selected);
        key(&mut app, KeyCode::Char('v'));
        key(&mut app, KeyCode::Char('v'));
        assert_eq!(
            app.modal_input.helix().unwrap().sub_mode,
            HelixSubMode::Normal
        );
        key(&mut app, KeyCode::Char('i'));
        assert!(app.editor.selection.is_none());
        key(&mut app, KeyCode::Char('Z'));
        key(&mut app, KeyCode::Esc);
        assert_eq!(
            app.modal_input.helix().unwrap().sub_mode,
            HelixSubMode::Normal
        );
        assert_eq!(app.editor.mode, view);
        assert!(app.editor.selection.is_some());
        app.dispatch_action(Action::ExitToPreview, 20, 80);
        assert_eq!(app.editor.mode, view);
    }
    assert_eq!(app.editor.buffer.contents(), "ZZabc");
}

#[test]
fn interaction_modes_unsupported_commands() {
    let _iso = crate::test_env::config_isolation();
    let mut app = app("helix", "abc");
    for select in [false, true] {
        if select {
            key(&mut app, KeyCode::Char('v'));
        }
        for code in ['s', 'S', 'C', 'm', '.', 'q', 'Q', '|', '!'] {
            key(&mut app, KeyCode::Char(code));
            assert_eq!(app.editor.buffer.contents(), "abc");
        }
        key(&mut app, KeyCode::Esc);
    }
    key(&mut app, KeyCode::Char('i'));
    assert_eq!(
        app.modal_input.helix().unwrap().sub_mode,
        HelixSubMode::Insert
    );
}

#[test]
fn dispatch_isolation_readonly_and_navigation() {
    let _iso = crate::test_env::config_isolation();
    let mut app = app("helix", "original");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.md");
    std::fs::write(&path, "original").unwrap();
    app.load_file_into_editor(path.clone()).unwrap();
    app.editor.selection = Some(Selection {
        anchor: 0,
        active: 5,
    });
    app.editor.place_cursor(4);
    app.modal_input.helix_mut().unwrap().register.text = "remember".into();
    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Char('o'));
    app.open_doc_page(crate::docs::DocId::Keybindings, None, 20, 80);
    assert!(app.editor.readonly);
    assert!(!app.modal_input.is_active());
    assert_eq!(app.editor.mode, Mode::Preview);
    let manual = app.editor.buffer.contents();
    for action in [
        Action::DeleteCharForward,
        Action::Paste,
        Action::Save,
        Action::EnterEditMode,
        Action::ToggleRawMode,
    ] {
        app.dispatch_action(action, 20, 80);
    }
    app.dispatch_paste("do not write".into(), &dims());
    app.save_from_helix(true);
    assert!(!app.should_quit);
    assert_eq!(app.editor.buffer.contents(), manual);
    assert_eq!(std::fs::read_to_string(path.clone()).unwrap(), "original");
    app.dispatch_action(Action::NavigateBack, 20, 80);
    assert!(!app.editor.readonly);
    assert_eq!(app.modal_input.helix().unwrap().register.text, "remember");
    assert!(app.modal_input.helix().unwrap().prompt.is_none());
    assert_eq!(
        app.modal_input.helix().unwrap().sub_mode,
        HelixSubMode::Normal
    );
    assert_eq!(app.editor.selection.unwrap().range(), (4, 5));
    assert_eq!(app.editor.cursor.offset, 4);
    assert_eq!(app.editor.mode, Mode::Rendered);
}

#[test]
fn register_paste_system_clipboard() {
    let _iso = crate::test_env::config_isolation();
    use std::sync::{Arc, Mutex};
    struct Clipboard {
        writes: Arc<Mutex<Vec<String>>>,
    }
    impl crate::clipboard::ClipboardSource for Clipboard {
        fn read_text(&mut self) -> crate::clipboard::TextRead {
            crate::clipboard::TextRead::Text("外\r\nZ".into())
        }
        fn write_text(&mut self, text: String) {
            self.writes.lock().unwrap().push(text);
        }
        fn can_read_bitmaps(&self) -> bool {
            false
        }
        fn bitmap_reader(&mut self) -> crate::clipboard::BitmapReader {
            Box::new(|| None)
        }
    }
    for reverse in [false, true] {
        let mut app = app("helix", "abcdef");
        app.editor.mode = Mode::Raw;
        let writes = Arc::new(Mutex::new(Vec::new()));
        app.clipboard = Box::new(Clipboard {
            writes: writes.clone(),
        });
        app.modal_input.helix_mut().unwrap().register.text = "internal".into();
        crate::editor::helix_ops::set_selection(
            &mut app.editor,
            if reverse {
                Selection {
                    anchor: 4,
                    active: 1,
                }
            } else {
                Selection {
                    anchor: 1,
                    active: 4,
                }
            },
        );
        let selected = app.editor.selection;
        app.dispatch_action(Action::Copy, 20, 80);
        assert_eq!(*writes.lock().unwrap(), ["bcd"]);
        assert_eq!(app.editor.selection, selected);
        app.dispatch_action(Action::Cut, 20, 80);
        assert_eq!(app.editor.contents(), "aef");
        assert_eq!(*writes.lock().unwrap(), ["bcd", "bcd"]);
        app.dispatch_action(Action::Paste, 20, 80);
        assert_eq!(app.editor.contents(), "a外\nZf");
        assert_eq!(
            app.editor
                .selection
                .unwrap()
                .selected_text(&app.editor.buffer),
            "外\nZ"
        );
        assert_eq!(app.modal_input.helix().unwrap().register.text, "internal");
        app.dispatch_paste("term".into(), &dims());
        assert_eq!(app.editor.contents(), "atermf");
        assert_eq!(
            app.editor
                .selection
                .unwrap()
                .selected_text(&app.editor.buffer),
            "term"
        );
        key(&mut app, KeyCode::Char('i'));
        app.dispatch_action(Action::Paste, 20, 80);
        assert_eq!(app.editor.contents(), "a外\nZtermf");
        assert_eq!(
            app.modal_input.helix().unwrap().sub_mode,
            HelixSubMode::Insert
        );
        assert!(app.editor.selection.is_none());
    }
    let table = "| a | b |\n|---|---|\n| c | d |\n";
    let mut app = app("helix", table);
    crate::editor::helix_ops::set_selection(
        &mut app.editor,
        Selection {
            anchor: 2,
            active: 3,
        },
    );
    let before = app.editor.history.undo_depth();
    app.dispatch_paste("evil|\nrow".into(), &dims());
    assert_eq!(app.editor.contents(), table);
    assert_eq!(app.editor.history.undo_depth(), before);
    assert!(app.transient.is_some());
}

#[test]
fn insert_markdown_insert_positions() {
    let _iso = crate::test_env::config_isolation();
    for mode in [Mode::Raw, Mode::Rendered] {
        for reverse in [false, true] {
            for (command, expected_offset) in [('i', 1), ('a', 9)] {
                let mut app = app("helix", "abc\ndef\nghi");
                app.editor.mode = mode;
                let selection = if reverse {
                    Selection {
                        anchor: 9,
                        active: 1,
                    }
                } else {
                    Selection {
                        anchor: 1,
                        active: 9,
                    }
                };
                crate::editor::helix_ops::set_selection(&mut app.editor, selection);
                key(&mut app, KeyCode::Char(command));
                assert_eq!(app.editor.contents(), "abc\ndef\nghi");
                assert_eq!(app.editor.cursor.offset, expected_offset);
                assert!(app.editor.selection.is_none());
                assert_eq!(
                    app.modal_input.helix().unwrap().sub_mode,
                    HelixSubMode::Insert
                );
                key(&mut app, KeyCode::Esc);
                let selected = app
                    .editor
                    .selection
                    .unwrap()
                    .selected_text(&app.editor.buffer);
                assert_eq!(
                    selected.chars().count(),
                    1,
                    "{command}, {mode:?}, reverse={reverse}"
                );
                assert_eq!(
                    app.modal_input.helix().unwrap().sub_mode,
                    HelixSubMode::Normal
                );
            }

            for (command, offset) in [('I', 17), ('A', 22)] {
                let mut app = app("helix", "  alpha\n  beta\n  gamma");
                app.editor.mode = mode;
                let selection = if reverse {
                    Selection {
                        anchor: 20,
                        active: 18,
                    }
                } else {
                    Selection {
                        anchor: 18,
                        active: 20,
                    }
                };
                crate::editor::helix_ops::set_selection(&mut app.editor, selection);
                key(&mut app, KeyCode::Char(command));
                assert_eq!(
                    app.editor.cursor.offset, offset,
                    "{command}, {mode:?}, reverse={reverse}"
                );
                assert_eq!(app.editor.contents(), "  alpha\n  beta\n  gamma");
            }

            for (command, expected) in [
                ('o', "top\nmid\nbottom\n\nlast"),
                ('O', "\ntop\nmid\nbottom\nlast"),
            ] {
                let mut app = app("helix", "top\nmid\nbottom\nlast");
                app.editor.mode = mode;
                let selection = if reverse {
                    Selection {
                        anchor: 14,
                        active: 2,
                    }
                } else {
                    Selection {
                        anchor: 2,
                        active: 14,
                    }
                };
                crate::editor::helix_ops::set_selection(&mut app.editor, selection);
                key(&mut app, KeyCode::Char(command));
                assert_eq!(
                    app.editor.contents(),
                    expected,
                    "{command}, {mode:?}, reverse={reverse}"
                );
                assert_eq!(
                    app.modal_input.helix().unwrap().sub_mode,
                    HelixSubMode::Insert
                );
                assert_eq!(app.editor.selection, None);
            }
        }
    }

    let mut eof = app("helix", "abc");
    crate::editor::helix_ops::set_selection(
        &mut eof.editor,
        Selection {
            anchor: 0,
            active: 3,
        },
    );
    key(&mut eof, KeyCode::Char('a'));
    key(&mut eof, KeyCode::Esc);
    assert_eq!(
        eof.editor
            .selection
            .unwrap()
            .selected_text(&eof.editor.buffer),
        "c"
    );
}

#[test]
fn insert_markdown_existing_editing_features() {
    let _iso = crate::test_env::config_isolation();
    let mut list = app("helix", "- first\n- second");
    crate::editor::helix_ops::set_selection(
        &mut list.editor,
        Selection {
            anchor: 15,
            active: 16,
        },
    );
    key(&mut list, KeyCode::Char('a'));
    key(&mut list, KeyCode::Char('x'));
    key(&mut list, KeyCode::Backspace);
    assert_eq!(list.editor.contents(), "- first\n- second");
    key(&mut list, KeyCode::Enter);
    assert_eq!(list.editor.contents(), "- first\n- second\n- ");
    key(&mut list, KeyCode::Char('x'));
    list.dispatch_paste("y".into(), &dims());
    assert_eq!(list.editor.contents(), "- first\n- second\n- xy");

    let mut list_tab = app("helix", "- first\n- second");
    crate::editor::helix_ops::set_selection(
        &mut list_tab.editor,
        Selection {
            anchor: 10,
            active: 11,
        },
    );
    key(&mut list_tab, KeyCode::Char('i'));
    key(&mut list_tab, KeyCode::Tab);
    assert_eq!(list_tab.editor.contents(), "- first\n    - second");

    let mut list_open = app("helix", "- item\n- next");
    crate::editor::helix_ops::set_selection(
        &mut list_open.editor,
        Selection {
            anchor: 2,
            active: 3,
        },
    );
    key(&mut list_open, KeyCode::Char('o'));
    assert_eq!(list_open.editor.contents(), "- item\n- \n- next");
    let mut list_above = app("helix", "- item\n- next");
    crate::editor::helix_ops::set_selection(
        &mut list_above.editor,
        Selection {
            anchor: 2,
            active: 3,
        },
    );
    key(&mut list_above, KeyCode::Char('O'));
    assert_eq!(list_above.editor.contents(), "- \n- item\n- next");

    let mut table = app("helix", "| a | b |\n|---|---|\n| c | d |\n");
    crate::editor::helix_ops::set_selection(
        &mut table.editor,
        Selection {
            anchor: 2,
            active: 3,
        },
    );
    key(&mut table, KeyCode::Char('a'));
    key(&mut table, KeyCode::Char('x'));
    key(&mut table, KeyCode::Backspace);
    assert_eq!(table.editor.contents(), "| a | b |\n|---|---|\n| c | d |\n");
    table.dispatch_paste("z".into(), &dims());
    assert_eq!(
        table.editor.contents(),
        "| az | b |\n|---|---|\n| c | d |\n"
    );

    let mut table_tab = app("helix", "| a | b |\n|---|---|\n| c | d |\n");
    crate::editor::helix_ops::set_selection(
        &mut table_tab.editor,
        Selection {
            anchor: 2,
            active: 3,
        },
    );
    key(&mut table_tab, KeyCode::Char('i'));
    key(&mut table_tab, KeyCode::Tab);
    table_tab.dispatch_paste("z".into(), &dims());
    assert_eq!(
        table_tab.editor.contents(),
        "| a | bz |\n|---|---|\n| c | d |\n"
    );

    let mut table_open = app("helix", "| a | b |\n|---|---|\n| c | d |\n");
    crate::editor::helix_ops::set_selection(
        &mut table_open.editor,
        Selection {
            anchor: 22,
            active: 23,
        },
    );
    key(&mut table_open, KeyCode::Char('o'));
    assert_eq!(
        table_open.editor.contents(),
        "| a | b |\n|---|---|\n| c | d |\n|   |   |\n"
    );
    assert_eq!(
        table_open.modal_input.helix().unwrap().sub_mode,
        HelixSubMode::Insert
    );
}

// Utility regression for global actions sharing the selection/table guard.
#[test]
fn table_global_range_actions_utility() {
    let _iso = crate::test_env::config_isolation();
    let source = "| alpha | bravo |\n|---|---|\n| one | two |\n";
    for action in [
        Action::DeleteCharBack,
        Action::DeleteCharForward,
        Action::Cut,
        Action::DeleteLine,
        Action::BoldSelection,
        Action::ItalicizeSelection,
        Action::InlineCodeSelection,
        Action::StrikethroughSelection,
        Action::HighlightSelection,
        Action::InsertImage,
        Action::InsertLink,
    ] {
        let mut app = app("helix", source);
        crate::editor::helix_ops::set_selection(
            &mut app.editor,
            Selection {
                anchor: 2,
                active: 13,
            },
        );
        app.modal_input.helix_mut().unwrap().register.text = "keep".into();
        app.dispatch_action(action.clone(), 20, 80);
        assert_eq!(app.editor.contents(), source, "{action:?}");
        assert_eq!(app.editor.history.undo_depth(), 0, "{action:?}");
        assert_eq!(app.modal_input.helix().unwrap().register.text, "keep");
        assert!(app.transient.is_some(), "{action:?}");
    }
    for action in [Action::InsertChar('|'), Action::Newline] {
        let mut app = app("helix", source);
        crate::editor::helix_ops::set_selection(
            &mut app.editor,
            Selection {
                anchor: 2,
                active: 3,
            },
        );
        app.dispatch_action(action, 20, 80);
        assert_eq!(app.editor.contents(), source);
        assert_eq!(app.editor.history.undo_depth(), 0);
    }
    for (action, offset) in [(Action::DeleteWordForward, 8), (Action::DeleteWordBack, 10)] {
        let mut app = app("helix", source);
        crate::editor::helix_ops::set_selection(&mut app.editor, Selection::new(offset));
        app.dispatch_action(action.clone(), 20, 80);
        assert_eq!(app.editor.contents(), source, "{action:?}");
        assert_eq!(app.editor.history.undo_depth(), 0);
    }
    let surrounded = format!("before\n\n{source}\nafter");
    let mut tab = app("helix", &surrounded);
    crate::editor::helix_ops::set_selection(
        &mut tab.editor,
        Selection {
            anchor: "before\n\n".chars().count() + 2,
            active: surrounded.chars().count() - 1,
        },
    );
    tab.dispatch_action(Action::InsertTab, 20, 80);
    assert_eq!(tab.editor.contents(), surrounded);
    assert_eq!(tab.editor.history.undo_depth(), 0);
    assert!(tab.transient.is_some());
    let mut table_tab = app("helix", source);
    crate::editor::helix_ops::set_selection(&mut table_tab.editor, Selection::new(2));
    table_tab.dispatch_action(Action::InsertTab, 20, 80);
    assert_eq!(table_tab.editor.contents(), source);
    assert!(table_tab.editor.cursor.offset >= 10);
    let mut list_tab = app("helix", "- first\n- second\n");
    crate::editor::helix_ops::set_selection(&mut list_tab.editor, Selection::new(10));
    list_tab.dispatch_action(Action::InsertTab, 20, 80);
    assert_eq!(list_tab.editor.contents(), "- first\n    - second\n");
    let mut plain_tab = app("helix", "abc");
    crate::editor::helix_ops::set_selection(&mut plain_tab.editor, Selection::new(1));
    plain_tab.dispatch_action(Action::InsertTab, 20, 80);
    assert_eq!(plain_tab.editor.contents(), "a    c");
    plain_tab.dispatch_action(Action::Undo, 20, 80);
    assert_eq!(plain_tab.editor.contents(), "abc");
    for (action, prefix) in [(Action::InsertImage, "!"), (Action::InsertLink, "")] {
        let mut snippet = app("helix", "alpha beta");
        crate::editor::helix_ops::set_selection(
            &mut snippet.editor,
            Selection {
                anchor: 0,
                active: 5,
            },
        );
        snippet.dispatch_action(action, 20, 80);
        assert_eq!(
            snippet.editor.contents(),
            format!("{prefix}[alpha](file path or URL) beta")
        );
        assert_eq!(
            snippet
                .editor
                .selection
                .unwrap()
                .selected_text(&snippet.editor.buffer),
            "file path or URL"
        );
        assert_eq!(snippet.editor.history.undo_depth(), 1);
        snippet.dispatch_action(Action::Undo, 20, 80);
        assert_eq!(snippet.editor.contents(), "alpha beta");
    }
    let mut valid = app("helix", source);
    crate::editor::helix_ops::set_selection(
        &mut valid.editor,
        Selection {
            anchor: 2,
            active: 7,
        },
    );
    valid.dispatch_action(Action::BoldSelection, 20, 80);
    assert!(valid.editor.contents().starts_with("| **alpha** |"));
    assert_eq!(
        valid
            .editor
            .selection
            .unwrap()
            .selected_text(&valid.editor.buffer),
        "alpha"
    );
    valid.dispatch_action(Action::Undo, 20, 80);
    assert_eq!(valid.editor.contents(), source);
}

#[test]
fn search_selection_prompt_capture() {
    let _iso = crate::test_env::config_isolation();
    use crate::config::{KeyBindingOverrides, KeyMap};
    let mut app = app("helix", "one\ntwo and one\ntwo");
    crate::editor::helix_ops::set_selection(
        &mut app.editor,
        Selection {
            anchor: 8,
            active: 4,
        },
    );
    let selection = app.editor.selection;
    let cursor = app.editor.cursor;
    let scroll = app.editor.scroll;
    key(&mut app, KeyCode::Char('/'));
    app.dispatch_paste("one\ntwo".into(), &dims());
    assert_eq!(app.modal_input.cmdline().unwrap().input, "one\\ntwo");
    let map = app.ensure_keymap_clone();
    app.dispatch_single_key(
        Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
        &map,
        &dims(),
    );
    assert!(app.modal_stack.is_empty());
    assert_eq!(app.editor.contents(), "one\ntwo and one\ntwo");
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.editor.selection, selection);
    assert_eq!(app.editor.cursor, cursor);
    assert_eq!(app.editor.scroll, scroll);
    key(&mut app, KeyCode::Char('/'));
    app.editor.kill_ring = "one\ntwo".into();
    let mut overrides = KeyBindingOverrides::default();
    overrides.0.insert("Paste".into(), "ctrl+p".into());
    let map = KeyMap::build(&overrides).unwrap();
    app.dispatch_single_key(
        Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
        &map,
        &dims(),
    );
    assert_eq!(app.modal_input.cmdline().unwrap().input, "one\\ntwo");
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        app.editor
            .selection
            .unwrap()
            .selected_text(&app.editor.buffer),
        "one\ntwo"
    );
    key(&mut app, KeyCode::Char('/'));
    app.dispatch_paste("é".repeat(2048), &dims());
    let line = app.modal_input.cmdline().unwrap();
    assert_eq!(
        line.input.chars().count(),
        crate::input::helix::state::PROMPT_CHAR_CAP
    );
    key(&mut app, KeyCode::Char('x'));
    assert_eq!(
        app.modal_input.cmdline().unwrap().input.chars().count(),
        1024
    );
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.editor.contents(), "one\ntwo and one\ntwo");
    key(&mut app, KeyCode::Char('/'));
    app.dispatch_paste("\n".repeat(2048), &dims());
    let line = app.modal_input.cmdline().unwrap();
    assert_eq!(line.input.chars().count(), 1024);
    assert!(!line.input.contains(['\n', '\r']));
}

// Utility regression: Escape and capture share restoration, with App's stricter context gates.
#[test]
fn prompt_snapshot_cancellation_checks_context_and_version() {
    let _iso = crate::test_env::config_isolation();
    for context in ["escape", "capture", "readonly", "diff", "replace", "stale"] {
        let mut app = app("helix", "one\nsecond\nthird");
        crate::editor::helix_ops::set_selection(
            &mut app.editor,
            Selection {
                anchor: 8,
                active: 4,
            },
        );
        key(&mut app, KeyCode::Char('/'));
        let origin = app
            .modal_input
            .helix()
            .unwrap()
            .prompt
            .as_ref()
            .unwrap()
            .origin;
        app.editor.selection = Some(Selection {
            anchor: 1,
            active: 3,
        });
        app.editor.cursor = crate::document::Cursor {
            offset: 2,
            preferred_col: 8,
        };
        app.editor.scroll = 1;
        match context {
            "readonly" => app.editor.readonly = true,
            "diff" => app.editor.mode = Mode::Diff,
            "replace" => {
                app.editor.search = Some(
                    crate::search::SearchState::new("one".into(), Some("two".into())).unwrap(),
                );
            }
            "stale" => app.editor.buffer.insert(app.editor.buffer.len_chars(), "!"),
            _ => {}
        }
        let current = (app.editor.selection, app.editor.cursor, app.editor.scroll);
        if context == "escape" {
            key(&mut app, KeyCode::Esc);
        } else {
            app.modal_input.helix_mut().unwrap().pending = HelixPending::G;
            app.cancel_helix_pending();
            assert_eq!(app.modal_input.helix().unwrap().pending, HelixPending::None);
        }
        let expected = if matches!(context, "escape" | "capture") {
            (origin.selection, origin.cursor, origin.scroll)
        } else {
            current
        };
        assert_eq!(
            (app.editor.selection, app.editor.cursor, app.editor.scroll),
            expected,
            "{context}"
        );
        assert!(app.modal_input.helix().unwrap().prompt.is_none());
    }
}

fn command(app: &mut App, command: &str) {
    key(app, KeyCode::Char(':'));
    for c in command.chars() {
        key(app, KeyCode::Char(c));
    }
    key(app, KeyCode::Enter);
}

fn modal_key(app: &mut App, code: KeyCode) {
    app.dispatch_modal_key(KeyEvent::new(code, KeyModifiers::NONE), 20, 80);
}

fn save_path(app: &mut App, path: &std::path::Path) {
    for _ in 0..1024 {
        modal_key(app, KeyCode::Backspace);
    }
    app.dispatch_modal_paste(path.to_str().unwrap());
    modal_key(app, KeyCode::Enter);
}

#[test]
fn file_commands_save_quit_guards() {
    let _iso = crate::test_env::config_isolation();
    use super::modal::{OverwriteConfirmModal, QuitConfirmModal, SaveAsModal};
    let dir = tempfile::tempdir().unwrap();
    for name in ["w", "write", "wq", "write-quit"] {
        let path = dir.path().join(format!("{name}.md"));
        std::fs::write(&path, "original").unwrap();
        let mut app = app("helix", "");
        app.load_file_into_editor(path.clone()).unwrap();
        key(&mut app, KeyCode::Char('i'));
        key(&mut app, KeyCode::Char('Z'));
        key(&mut app, KeyCode::Esc);
        command(&mut app, name);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Zoriginal");
        assert_eq!(app.should_quit, matches!(name, "wq" | "write-quit"));
        assert!(!app.editor.dirty);
    }
    for name in ["q", "quit"] {
        let mut clean = app("helix", "clean");
        command(&mut clean, name);
        assert!(clean.should_quit);
        let mut dirty = app("helix", "unsaved");
        dirty.editor.dirty = true;
        command(&mut dirty, name);
        assert!(!dirty.should_quit);
        assert!(dirty.modal_stack.contains::<QuitConfirmModal>());
        modal_key(&mut dirty, KeyCode::Esc);
        assert!(!dirty.should_quit);
        assert_eq!(dirty.editor.contents(), "unsaved");
    }
    let mut failed = app("helix", "");
    failed
        .editor
        .replace_buffer(Buffer::for_new_file(dir.path()));
    failed.editor.buffer.insert(0, "keep");
    failed.editor.dirty = true;
    failed.sync_modal_suspension();
    command(&mut failed, "wq");
    assert!(!failed.should_quit);
    assert!(failed.editor.dirty);
    assert_eq!(failed.editor.contents(), "keep");
    assert!(failed
        .transient
        .as_ref()
        .unwrap()
        .text
        .contains("Save failed"));

    let mut unnamed = app("helix", "new");
    unnamed.editor.dirty = true;
    command(&mut unnamed, "wq");
    assert!(unnamed.modal_stack.contains::<SaveAsModal>());
    assert!(!unnamed.should_quit);
    save_path(&mut unnamed, &dir.path().join("missing/notes.md"));
    assert!(unnamed.modal_stack.contains::<SaveAsModal>());
    assert!(!unnamed.should_quit);
    modal_key(&mut unnamed, KeyCode::Esc);
    assert!(!unnamed.should_quit);
    assert!(unnamed.editor.buffer.path().is_none());
    command(&mut unnamed, "write-quit");
    let destination = dir.path().join("new.md");
    save_path(&mut unnamed, &destination);
    assert_eq!(std::fs::read_to_string(destination).unwrap(), "new");
    assert!(unnamed.should_quit);

    let existing = dir.path().join("existing.md");
    std::fs::write(&existing, "old").unwrap();
    let mut overwrite = app("helix", "replacement");
    overwrite.editor.dirty = true;
    command(&mut overwrite, "wq");
    save_path(&mut overwrite, &existing);
    assert!(overwrite.modal_stack.contains::<OverwriteConfirmModal>());
    assert!(!overwrite.should_quit);
    modal_key(&mut overwrite, KeyCode::Esc);
    assert!(!overwrite.should_quit);
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), "old");
    command(&mut overwrite, "wq");
    save_path(&mut overwrite, &existing);
    modal_key(&mut overwrite, KeyCode::Enter);
    assert!(overwrite.should_quit);
    assert_eq!(std::fs::read_to_string(existing).unwrap(), "replacement");
}

#[test]
fn file_commands_command_capture_and_rejection() {
    let _iso = crate::test_env::config_isolation();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.md");
    std::fs::write(&source, "original").unwrap();
    let marker = dir.path().join("must-not-exist");
    for command_text in [
        "q!".to_owned(),
        "w!".into(),
        "wq!".into(),
        "quit!".into(),
        "write-quit!".into(),
        "x".into(),
        "s/foo/bar/".into(),
        format!("w {}", marker.display()),
        format!("wq {}", marker.display()),
        format!("!touch {}", marker.display()),
        "w | q".into(),
        "unknown".into(),
    ] {
        let mut app = app("helix", "");
        app.load_file_into_editor(source.clone()).unwrap();
        command(&mut app, &command_text);
        assert!(!app.should_quit, "{command_text}");
        assert!(app.modal_stack.is_empty(), "{command_text}");
        assert_eq!(app.editor.contents(), "original");
        assert_eq!(app.editor.history.undo_depth(), 0);
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "original");
        assert!(!marker.exists());
        assert!(app.transient.as_ref().unwrap().text.contains("Unsupported"));
        assert!(!app.pending_open_file_in_editor);
        assert!(app.pending_open_theme_in_editor.is_none());
    }
    let mut app = app("helix", "original");
    let before = app.editor.selection;
    key(&mut app, KeyCode::Char(':'));
    app.dispatch_paste("w!\n path".into(), &dims());
    assert_eq!(app.modal_input.cmdline().unwrap().input, "w! path");
    let map = app.ensure_keymap_clone();
    app.dispatch_single_key(
        Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
        &map,
        &dims(),
    );
    assert!(!app.should_quit);
    assert!(app.modal_stack.is_empty());
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.editor.selection, before);
    assert_eq!(app.editor.contents(), "original");
    assert!(app.modal_input.helix().unwrap().prompt.is_none());
    key(&mut app, KeyCode::Char('x'));
    key(&mut app, KeyCode::Char(':'));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('y'));
    assert!(app.modal_input.helix().unwrap().register.linewise);
}

#[test]
fn dispatch_isolation_priority_and_batches() {
    let _iso = crate::test_env::config_isolation();
    fn event(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, mods))
    }
    fn run(app: &mut App, events: Vec<Event>, batched: bool) {
        let map = app.ensure_keymap_clone();
        if batched {
            // Fixed viewport: construct the backend without a terminal query or any draw.
            let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
            let mut terminal = ratatui::Terminal::with_options(
                backend,
                ratatui::TerminalOptions {
                    viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 80, 24)),
                },
            )
            .unwrap();
            let (_tx, rx) = std::sync::mpsc::channel();
            app.dispatch_key_batch(events, &dims(), &mut terminal, &rx);
            while let Some(event) = app.pending_events.pop_front() {
                app.dispatch_single_key(event, &map, &dims());
            }
        } else {
            for event in events {
                app.dispatch_single_key(event, &map, &dims());
            }
        }
    }
    let plain = |c| event(KeyCode::Char(c), KeyModifiers::NONE);
    let esc = || event(KeyCode::Esc, KeyModifiers::NONE);
    for events in [
        vec![
            plain('i'),
            plain('a'),
            plain('b'),
            esc(),
            plain('w'),
            plain('d'),
            plain('u'),
            plain('U'),
            plain('v'),
            plain('l'),
            esc(),
        ],
        vec![
            plain('2'),
            plain('g'),
            event(KeyCode::Char('p'), KeyModifiers::CONTROL),
            esc(),
            plain('i'),
            plain('Z'),
            esc(),
        ],
        vec![
            plain('/'),
            event(KeyCode::Char('v'), KeyModifiers::CONTROL),
            event(KeyCode::Enter, KeyModifiers::NONE),
            plain('n'),
            plain('N'),
        ],
    ] {
        let mut single = app("helix", "one two one");
        let mut batch = app("helix", "one two one");
        single.editor.kill_ring = "one".into();
        batch.editor.kill_ring = "one".into();
        run(&mut single, events.clone(), false);
        run(&mut batch, events, true);
        assert_eq!(batch.editor.contents(), single.editor.contents());
        assert_eq!(batch.editor.selection, single.editor.selection);
        assert_eq!(batch.editor.cursor, single.editor.cursor);
        assert_eq!(batch.modal_input, single.modal_input);
        assert_eq!(
            batch.editor.history.undo_depth(),
            single.editor.history.undo_depth()
        );
        assert!(batch.modal_stack.is_empty());
        assert!(batch.pending_events.is_empty());
    }
    let mut modal = app("helix", "one two");
    let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    let mut terminal = ratatui::Terminal::with_options(
        backend,
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 80, 24)),
        },
    )
    .unwrap();
    let (_tx, rx) = std::sync::mpsc::channel();
    let remainder = vec![esc(), plain('i'), plain('Z'), esc()];
    let mut events = vec![
        plain('2'),
        plain('g'),
        event(KeyCode::Char('p'), KeyModifiers::CONTROL),
    ];
    events.extend(remainder.clone());
    modal.dispatch_key_batch(events, &dims(), &mut terminal, &rx);
    assert_eq!(
        modal.pending_events.iter().cloned().collect::<Vec<_>>(),
        remainder
    );
    assert!(!modal.modal_stack.is_empty());
    assert_eq!(modal.modal_input.helix().unwrap().count, None);
    assert_eq!(
        modal.modal_input.helix().unwrap().pending,
        HelixPending::None
    );

    let mut mapped = app("helix", "one");
    mapped
        .keybindings
        .0
        .insert("ToggleHelixMode".into(), "ctrl+h".into());
    mapped.keymap = None;
    run(
        &mut mapped,
        vec![event(KeyCode::Char('h'), KeyModifiers::CONTROL), plain('Z')],
        true,
    );
    assert_eq!(mapped.editor.contents(), "Zone");
    assert!(!mapped.modal_input.is_active());

    let mut search = app("helix", "one two one");
    key(&mut search, KeyCode::Char('2'));
    key(&mut search, KeyCode::Char('f'));
    search.enter_search_flow("one".into(), Some("changed".into()));
    key(&mut search, KeyCode::Esc);
    assert!(!search.search_flow_captures());
    assert_eq!(
        search.modal_input.helix().unwrap().pending,
        HelixPending::None
    );
    assert!(search.editor.selection.is_some());
    key(&mut search, KeyCode::Char('i'));
    assert_eq!(
        search.modal_input.helix().unwrap().sub_mode,
        HelixSubMode::Insert
    );

    let mut diff = app("helix", "old\n");
    key(&mut diff, KeyCode::Char('2'));
    key(&mut diff, KeyCode::Char('g'));
    diff.editor
        .enter_diff_mode(crate::diff::DiffState::new("old\n", "new\n").unwrap());
    key(&mut diff, KeyCode::Tab);
    assert_eq!(diff.editor.mode, Mode::Diff);
    assert_eq!(diff.modal_input.helix().unwrap().count, None);
    assert_eq!(
        diff.modal_input.helix().unwrap().pending,
        HelixPending::None
    );
    diff.editor.exit_diff_mode();
    key(&mut diff, KeyCode::Char('i'));
    assert_eq!(
        diff.modal_input.helix().unwrap().sub_mode,
        HelixSubMode::Insert
    );
}

#[test]
fn discoverability_ui_and_embedded_manual() {
    let _iso = crate::test_env::config_isolation();
    use crate::ui::status_bar::{StatusBar, StatusBarState};
    use ratatui::{backend::TestBackend, Terminal};
    for (submode, label) in [
        (HelixSubMode::Normal, "HELIX NORMAL"),
        (HelixSubMode::Insert, "HELIX INSERT"),
        (HelixSubMode::Select, "HELIX SELECT"),
    ] {
        let mut app = app("helix", "ab日e\u{301}f\n");
        app.modal_input.helix_mut().unwrap().sub_mode = submode;
        let style = match submode {
            HelixSubMode::Normal => app.theme.status_mode_vim_normal,
            HelixSubMode::Insert => app.theme.status_mode_vim_insert,
            HelixSubMode::Select => app.theme.status_mode_vim_visual,
        };
        assert_eq!(app.modal_input.mode_label(), Some(label));
        for mode in [Mode::Raw, Mode::Rendered] {
            assert_eq!(
                super::cursor_style::editor_cursor_style(app.theme, mode, &app.modal_input).bg,
                style.bg
            );
            app.editor.mode = mode;
            let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
            terminal
                .draw(|frame| {
                    frame.render_widget(
                        StatusBar {
                            theme: app.theme,
                            state: StatusBarState {
                                mode,
                                filename: "notes.md",
                                line_count: 2,
                                scroll_total: 2,
                                viewport_rows: 1,
                                modified: false,
                                scroll: 0,
                                cursor_line: Some(1),
                                cursor_col: Some(1),
                                section_path: Vec::new(),
                                diff_progress: None,
                                modal_input: &app.modal_input,
                            },
                        },
                        frame.area(),
                    )
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = (0..80).map(|x| buffer[(x, 0)].symbol()).collect();
            assert!(text.contains(label));
            assert_eq!(buffer[(0, 0)].bg, style.bg.unwrap());
        }
        match app.hint_content() {
            crate::ui::HintContent::Chords(hints) => {
                assert!(!hints
                    .chords
                    .iter()
                    .any(|c| c.label == "Preview" || c.label == "Clear selection"));
                assert!(hints.chords.iter().any(|c| c.label
                    == if submode == HelixSubMode::Insert {
                        "Normal"
                    } else {
                        "Edit"
                    }));
            }
            _ => panic!("Helix hints"),
        }
    }
    let id = crate::docs::DocId::from_slug("helix-mode.md").expect("embedded Helix manual");
    assert_eq!(
        crate::docs::ALL_DOCS.iter().filter(|p| p.id == id).count(),
        1
    );
    assert!(crate::docs::DocId::Index
        .source()
        .contains("[Helix mode](helix-mode.md)"));
    let mut app = app("helix", "notes");
    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Char('x'));
    match app.hint_content() {
        crate::ui::HintContent::CommandLine {
            prefix,
            text,
            cursor,
            ..
        } => {
            assert_eq!(prefix, '/');
            assert_eq!(text, "x");
            assert_eq!(cursor, 1);
        }
        _ => panic!("visible prompt"),
    }
    key(&mut app, KeyCode::Esc);
    app.open_doc_page(id, None, 20, 80);
    assert!(app.editor.readonly);
    assert!(app.editor.contents().contains("Select, then act"));
    let manual = app.editor.contents();
    assert!(manual.contains("basic selection-first editing with one selection"));
    assert!(manual.contains("## Scope and limitations"));
    assert!(manual.contains("## Unsupported Helix commands"));
    assert!(manual.contains("An unrecognized prefix does not capture the rest of a sequence"));
    for (sequence, expected, mode) in [
        ("mi(", "(abc", HelixSubMode::Insert),
        ("ma\"", "a\"bc", HelixSubMode::Insert),
        ("md\"", "bc", HelixSubMode::Normal),
        ("gd", "abc", HelixSubMode::Normal),
    ] {
        assert!(manual.contains(&format!("`{sequence}`")));
        let mut example = self::app("helix", "abc");
        for ch in sequence.chars() {
            key(&mut example, KeyCode::Char(ch));
        }
        assert_eq!(example.editor.contents(), expected, "{sequence}");
        assert_eq!(
            example.modal_input.helix().unwrap().sub_mode,
            mode,
            "{sequence}"
        );
    }
}
