//! Shared fixtures for unit tests under `src/app/`: `App::new` boilerplate in one place.

use crate::config::{Config, KeyBindingOverrides, State, Theme};
use crate::document::Buffer;
use crate::terminal::Capabilities;

use super::App;

/// Build a default-config `App` with no file loaded.  Uses
/// [`Capabilities::default`] so no terminal probing happens, but forces `TrueColor`:
/// the default 16-color profile triggers the theme substitution and leaves a
/// `ThemeDowngradeModal` on the stack absorbing every unrelated test's input.
pub(crate) fn make_app() -> App {
    let caps = Capabilities {
        color_depth: crate::terminal::ColorDepth::TrueColor,
        ..Capabilities::default()
    };
    let theme_file = (&Theme::default()).into();
    App::new(
        Config::default(),
        State::default(),
        KeyBindingOverrides::default(),
        theme_file,
        None,
        caps,
        Vec::new(),
    )
    .expect("build app")
}

/// Close whatever modals `App::new` opened (the welcome, first of all), for a test whose
/// behavior depends on nothing else being open.
pub(crate) fn close_startup_modals(app: &mut App) {
    while app.modal_stack.pop().is_some() {}
}

/// Build an `App` seeded with `text` and the cursor at byte
/// `cursor_byte` (clamped to the buffer length).
pub(crate) fn app_with_buffer(text: &str, cursor_byte: usize) -> App {
    let mut app = make_app();
    app.editor.buffer = Buffer::from_str(text);
    app.editor.refresh_parsed();
    let total = app.editor.buffer.len_chars();
    let char_off = app
        .editor
        .buffer
        .rope()
        .byte_to_char(cursor_byte.min(app.editor.buffer.contents().len()));
    app.editor.cursor.offset = char_off.min(total);
    app.editor.update_cursor_block();
    app
}

#[cfg(test)]
mod theme_downgrade_tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::modal::types::{Modal, ModalOutcome};
    use crate::app::modal::{TerminalCapabilitiesModal, ThemeDowngradeModal, WelcomeModal};
    use crate::config::{Config, FiguresEnabled, ImagesEnabled, KeyBindingOverrides, State, Theme};
    use crate::terminal::{Capabilities, ColorDepth};

    use super::App;

    fn app_with(color_depth: ColorDepth, theme: &str) -> App {
        app_with_welcome(color_depth, theme, true)
    }

    /// `show_welcome = false` is what lets the capabilities notice
    /// through — the welcome suppresses it (`suppress_legacy_prompts`).
    fn app_with_welcome(color_depth: ColorDepth, theme: &str, show_welcome: bool) -> App {
        let caps = Capabilities {
            color_depth,
            ..Capabilities::minimal()
        };
        let mut config = Config {
            theme: theme.to_owned(),
            ..Config::default()
        };
        config.editor.show_welcome = show_welcome;
        // Record the running version: an empty `last_version_seen` with `show_welcome`
        // off would put a `PostUpgradeModal` on top of the stack under assertion.
        let state = State {
            last_version_seen: crate::app::update_check::INSTALLED_VERSION.to_owned(),
            ..State::default()
        };
        App::new(
            config,
            state,
            KeyBindingOverrides::default(),
            (&Theme::default()).into(),
            None,
            caps,
            Vec::new(),
        )
        .expect("build app")
    }

    #[test]
    fn indexed_terminal_substitutes_the_theme_and_notifies() {
        let app = app_with(ColorDepth::Ansi256, "Dracula");
        assert_eq!(app.config.theme, "256 Dark");
        assert_eq!(app.config.theme_downgraded_from.as_deref(), Some("Dracula"));
        assert!(app.modal_stack.contains::<ThemeDowngradeModal>());
    }

    #[test]
    fn truecolor_terminal_leaves_the_theme_alone() {
        let app = app_with(ColorDepth::TrueColor, "Dracula");
        assert_eq!(app.config.theme, "Dracula");
        assert!(app.config.theme_downgraded_from.is_none());
        assert!(!app.modal_stack.contains::<ThemeDowngradeModal>());
    }

    #[test]
    fn an_indexed_terminal_disables_media_without_touching_config() {
        // A persisted `Always` must not decode here, but must survive in `config` so it
        // takes effect again on a capable terminal.
        let caps = Capabilities {
            color_depth: ColorDepth::Ansi256,
            ..Capabilities::minimal()
        };
        let mut config = Config {
            theme: "Dracula".into(),
            ..Config::default()
        };
        config.images.enabled = ImagesEnabled::Always;
        config.figures.enabled = FiguresEnabled::Always;
        let app = App::new(
            config,
            State::default(),
            KeyBindingOverrides::default(),
            (&Theme::default()).into(),
            None,
            caps,
            Vec::new(),
        )
        .expect("build app");

        assert!(!app.effective_images_enabled());
        assert!(!app.effective_diagrams_enabled());
        assert!(!app.images_layout_enabled());
        assert!(!app.diagrams_layout_enabled());
        assert!(!app.editor.images_enabled);
        assert!(!app.editor.diagrams_enabled);
        assert_eq!(app.config.images.enabled, ImagesEnabled::Always);
        assert_eq!(app.config.figures.enabled, FiguresEnabled::Always);
    }

    #[test]
    fn a_truecolor_terminal_still_renders_media() {
        let caps = Capabilities {
            color_depth: ColorDepth::TrueColor,
            ..Capabilities::minimal()
        };
        let mut config = Config::default();
        config.images.enabled = ImagesEnabled::Always;
        config.figures.enabled = FiguresEnabled::Always;
        let app = App::new(
            config,
            State::default(),
            KeyBindingOverrides::default(),
            (&Theme::default()).into(),
            None,
            caps,
            Vec::new(),
        )
        .expect("build app");
        assert!(app.effective_images_enabled());
        assert!(app.effective_diagrams_enabled());
    }

    #[test]
    fn a_new_terminal_gets_one_notice_not_two() {
        // The capabilities notice absorbs the downgrade explanation, so the standalone
        // modal must not also be queued underneath it.
        let app = app_with_welcome(ColorDepth::Ansi256, "Dracula", false);
        assert_eq!(app.config.theme, "256 Dark");
        assert!(app.modal_stack.contains::<TerminalCapabilitiesModal>());
        assert!(!app.modal_stack.contains::<ThemeDowngradeModal>());
    }

    #[test]
    fn an_on_demand_welcome_is_escapable_but_a_first_run_one_is_not() {
        // The welcome force-sets media to `Never` below truecolor and Save persists that,
        // so an on-demand reopen needs a no-op exit or merely looking would overwrite the
        // user's capable-terminal choices.  `show_welcome = false` because
        // `open_welcome_modal` no-ops when a startup welcome is already on the stack.
        let mut app = app_with_welcome(ColorDepth::Ansi256, "Dracula", false);
        app.open_welcome_modal();
        let top = app.modal_stack.top_mut().expect("welcome is on top");
        assert!(top.as_any().is::<WelcomeModal>());
        assert!(top.dismissable());

        // Built directly: on a first-run launch that also downgrades, the welcome sits
        // *under* the theme-downgrade modal.
        let caps = Capabilities {
            color_depth: ColorDepth::Ansi256,
            ..Capabilities::minimal()
        };
        let first_run = WelcomeModal::from_state(&caps, &Config::default())
            .expect("show_welcome defaults to true");
        assert!(!first_run.dismissable());
    }

    #[test]
    fn saving_the_welcome_below_truecolor_leaves_persisted_media_alone() {
        // The forced `Never` is a session fact; writing it would overwrite the `Always`
        // chosen on a capable terminal sharing the same config.toml.  Isolated because
        // Save runs a real `Config::save`.
        let _iso = crate::test_env::config_isolation();
        let mut app = app_with_welcome(ColorDepth::Ansi256, "Dracula", false);
        app.config.images.enabled = ImagesEnabled::Always;
        app.config.figures.enabled = FiguresEnabled::Always;

        let mut modal = WelcomeModal::new(
            &Capabilities {
                color_depth: ColorDepth::Ansi256,
                ..Capabilities::minimal()
            },
            &app.config,
        );
        modal.focus_save_for_test();
        let outcome = modal.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut app,
            24,
            80,
        );
        match outcome {
            ModalOutcome::CloseAnd(f) => f(&mut app),
            _ => panic!("Save should close and persist"),
        }

        assert_eq!(app.config.images.enabled, ImagesEnabled::Always);
        assert_eq!(app.config.figures.enabled, FiguresEnabled::Always);
        // The session still refuses to draw them.
        assert!(!app.effective_images_enabled());
        assert!(!app.effective_diagrams_enabled());
    }

    #[test]
    fn a_colorless_terminal_is_not_downgraded() {
        // Every color is stripped on `NoColor` anyway, so the swap would be invisible.
        let app = app_with(ColorDepth::NoColor, "Dracula");
        assert_eq!(app.config.theme, "Dracula");
        assert!(app.config.theme_downgraded_from.is_none());
        assert!(!app.modal_stack.contains::<ThemeDowngradeModal>());
    }

    #[test]
    fn a_monochrome_theme_is_not_substituted() {
        // `Monochrome Dark` is `Color::Reset` throughout, so already correct at any depth.
        let app = app_with(ColorDepth::Ansi16, "Monochrome Dark");
        assert_eq!(app.config.theme, "Monochrome Dark");
        assert!(app.config.theme_downgraded_from.is_none());
        assert!(!app.modal_stack.contains::<ThemeDowngradeModal>());
    }

    #[test]
    fn an_already_indexed_theme_is_not_substituted() {
        // The light choice must not be flipped to dark.
        let app = app_with(ColorDepth::Ansi16, "256 Light");
        assert_eq!(app.config.theme, "256 Light");
        assert!(app.config.theme_downgraded_from.is_none());
        assert!(!app.modal_stack.contains::<ThemeDowngradeModal>());
    }
}
