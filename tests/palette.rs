//! Phase 10 — integration tests for the command palette and
//! configuration overlays.
//!
//! These bypass the live event loop: they construct an `EditorState`
//! and a `PaletteState` directly, dispatch the same sequence of
//! actions a user would, and assert on observable state.  Where the
//! plan calls for `App`-level checks we drive the relevant `App`
//! method directly.

use edamame::config::{Action, KeyBindingOverrides, KeyMap, Theme};
use edamame::document::Buffer;
use edamame::editor::{edit_ops, EditorState};
use edamame::ui::{KeybindsResponse, KeybindsState, PaletteResponse, PaletteState, SettingsState};

const VP: usize = 40;
const VW: usize = 80;

fn theme() -> &'static Theme {
    // SAFETY: Box::leak intentionally produces a `&'static Theme` for
    // the test duration.  The same pattern is used across other
    // integration tests in this crate.
    Box::leak(Box::new(Theme::default()))
}

fn state(text: &str) -> EditorState {
    EditorState::new(Buffer::from_str(text), theme())
}

fn keymap() -> KeyMap {
    KeyMap::build(&KeyBindingOverrides::default()).unwrap()
}

#[test]
fn palette_save_dispatch_produces_same_buffer_state_as_keyboard_save() {
    // Two parallel editor states.  Edit identically; one path saves
    // via the keyboard `Action::Save`; the other selects "Save File"
    // from the palette.  The post-state must match exactly.
    let path_keyboard = tempfile::NamedTempFile::new().unwrap();
    let path_palette = tempfile::NamedTempFile::new().unwrap();

    let mut keyboard_editor = state("hello world");
    let mut palette_editor = state("hello world");
    // `save_as` writes the current contents to `path` and records the
    // path on the Buffer.  Subsequent `Action::Save` calls will reuse
    // it — that's the path our test assertion compares against.
    keyboard_editor
        .buffer
        .save_as(path_keyboard.path())
        .unwrap();
    palette_editor.buffer.save_as(path_palette.path()).unwrap();
    keyboard_editor.dirty = true;
    palette_editor.dirty = true;

    // Keyboard path: dispatch `Action::Save` directly.
    edit_ops::apply(&mut keyboard_editor, Action::Save, VP, VW);

    // Palette path: open palette, type "save f" (the trailing " f"
    // disambiguates "Save file" from "Save a copy"), press Enter.
    let mut palette = PaletteState::open(&keymap());
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for c in "save f".chars() {
        palette.handle_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let response = palette.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let action = match response {
        PaletteResponse::Selected(a) => a,
        other => panic!("expected Selected, got {other:?}"),
    };
    assert_eq!(action, Action::Save);
    edit_ops::apply(&mut palette_editor, action, VP, VW);

    // Both saves should have produced identical buffer state and
    // identical on-disk content.
    let keyboard_disk = std::fs::read_to_string(path_keyboard.path()).unwrap();
    let palette_disk = std::fs::read_to_string(path_palette.path()).unwrap();
    assert_eq!(keyboard_disk, palette_disk);
    assert_eq!(keyboard_editor.dirty, palette_editor.dirty);
    assert_eq!(
        keyboard_editor.buffer.contents(),
        palette_editor.buffer.contents()
    );
}

#[test]
fn settings_overlay_field_change_response_is_emitted_once() {
    // The overlay's contract: cycling a toggle returns exactly one
    // `FieldChanged` response per keystroke.  Subsequent navigation
    // keystrokes (Down arrow etc.) must NOT re-emit the response.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use edamame::config::Config;
    use edamame::ui::SettingsResponse;

    let mut state = SettingsState::new();
    let mut config = Config::default();

    // Walk down past the "Open externally" rows + "Use hint line" to
    // reach the "Hint duration" row, which opens an inline
    // editor on Enter (a numeric field is the cleanest test of the
    // confirm path because the alternative — cycling a bool — fires
    // FieldChanged on its own).  Use the row label table to find it.
    let target_label = "Hint duration";
    while !state.focused_row_label_eq(target_label) {
        let resp = state.handle_key(
            &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &mut config,
        );
        assert!(matches!(resp, SettingsResponse::Continue));
    }

    // Enter opens the inline editor; Enter again confirms.  Edit
    // "1500" → "2500".
    state.handle_key(
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut config,
    );
    for _ in 0..4 {
        state.handle_key(
            &KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &mut config,
        );
    }
    for c in "2500".chars() {
        state.handle_key(
            &KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            &mut config,
        );
    }
    let resp = state.handle_key(
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut config,
    );
    assert!(
        matches!(resp, SettingsResponse::FieldChanged(_)),
        "expected FieldChanged, got {resp:?}"
    );
    assert_eq!(config.editor.transient_ms, 2500);

    // Subsequent Down must NOT re-emit FieldChanged.
    let resp = state.handle_key(
        &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &mut config,
    );
    assert!(matches!(resp, SettingsResponse::Continue));
}

/// Tiny inherent-impl helper for the test above.  The settings state
/// doesn't expose its row table publicly, so we add a small focus-
/// label query method behind a trait-extension shim that lives only
/// in this test crate.
trait SettingsStateExt {
    fn focused_row_label_eq(&self, label: &str) -> bool;
}

impl SettingsStateExt for SettingsState {
    fn focused_row_label_eq(&self, label: &str) -> bool {
        // Reach into the public state by re-exercising the overlay's
        // observable behaviour: a focused-row check is implicit in
        // the order rows scroll past on Down.  We cheat slightly by
        // mirroring the curated row order here — the overlay's own
        // `rows_match_curated_list` test guards against drift.
        // An empty entry mirrors the non-focusable divider so indices
        // line up with `state.focused` even though Down skips it.
        const ORDER: &[&str] = &[
            "Open config folder",
            "Open config.toml in default editor",
            "",
            "Appearance",
            "Use hint line",
            "Hint duration",
            "Use visual line navigation",
            "Scroll speed",
            "Show images",
            "Show remote images",
            "Show table buttons",
            "Export inlined images",
            "Export diagrams as SVG",
        ];
        ORDER.get(self.focused).is_some_and(|l| *l == label)
    }
}

#[test]
fn keybinds_overlay_conflict_is_rejected_and_reports_existing_action() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut keymap = keymap();
    let mut overrides = KeyBindingOverrides::default();
    let mut state = KeybindsState::open(&keymap);

    // Focus the row for `Save`.  `focus_action` is the public surface
    // for jumping to a known row in the category-grouped layout.
    assert!(state.focus_action(&Action::Save), "Save in overlay");

    // Open the inline editor and replace the seeded chord with `ctrl+q`
    // (currently bound to Quit).
    state.handle_key(
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut keymap,
        &mut overrides,
    );
    for _ in 0..6 {
        state.handle_key(
            &KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &mut keymap,
            &mut overrides,
        );
    }
    for c in "ctrl+q".chars() {
        state.handle_key(
            &KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            &mut keymap,
            &mut overrides,
        );
    }
    let response = state.handle_key(
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut keymap,
        &mut overrides,
    );

    assert!(
        matches!(response, KeybindsResponse::Continue),
        "conflict must yield Continue, got {response:?}"
    );

    // Save's binding must be unchanged; Quit must still be bound to ctrl+q.
    assert_eq!(
        keymap.first_key_for(&Action::Save).as_deref(),
        Some("Ctrl-S")
    );
    assert_eq!(
        keymap.first_key_for(&Action::Quit).as_deref(),
        Some("Ctrl-Q")
    );
    // Overrides are not mutated on conflict — important for the
    // "rejected edit doesn't leak to disk" guarantee.
    assert!(
        overrides.0.is_empty(),
        "overrides was mutated despite the conflict"
    );

    // The overlay surfaces the same conflict message inline so the
    // user sees it without needing to look at the hint line.
    assert!(state
        .last_error
        .as_deref()
        .is_some_and(|e| e.contains("Quit") && e.contains("ctrl+q")));
}

#[test]
fn keybinds_overlay_rebind_round_trips_through_save() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut keymap = keymap();
    let mut overrides = KeyBindingOverrides::default();
    let mut state = KeybindsState::open(&keymap);
    assert!(state.focus_action(&Action::Save));
    state.handle_key(
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut keymap,
        &mut overrides,
    );
    for _ in 0..6 {
        state.handle_key(
            &KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &mut keymap,
            &mut overrides,
        );
    }
    for c in "f9".chars() {
        state.handle_key(
            &KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            &mut keymap,
            &mut overrides,
        );
    }
    let resp = state.handle_key(
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut keymap,
        &mut overrides,
    );
    assert!(matches!(resp, KeybindsResponse::Rebound { .. }));

    // Persist + reload — round-trip through TOML.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keybindings.toml");
    overrides.save_to(&path).unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("Save"), "{raw}");
    assert!(raw.contains("\"f9\""), "{raw}");

    let reloaded: KeyBindingOverrides = toml::from_str(&raw).unwrap();
    assert_eq!(reloaded.0.get("Save").map(String::as_str), Some("f9"));
}
