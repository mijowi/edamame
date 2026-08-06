//! Shared fixtures for binary unit tests under `src/app/`.
//!
//! Step 4 of `refactor-app.md`: the `phase9_flash_tests` and
//! `phase15_insert_table_tests` blocks that previously lived inline in
//! `app.rs` were relocated into the modules they exercise.  Both share
//! a `make_app()` constructor (and the table tests share an
//! `app_with_buffer` seed) — keeping the helpers in one place avoids
//! duplicating the `App::new` boilerplate across the per-module test
//! blocks.

use crate::config::{Config, KeyBindingOverrides, Theme};
use crate::document::Buffer;
use crate::terminal::Capabilities;

use super::App;

/// Build a default-config `App` with no file loaded.  Derived from
/// [`Capabilities::default`] so no terminal probing is performed —
/// safe to call from any test thread — but with `TrueColor` forced on.
///
/// Why truecolor: `Capabilities::default()` is the *minimal* profile
/// (16 colors), which triggers the startup indexed-color theme
/// substitution and puts a `ThemeDowngradeModal` on the stack.  A modal
/// on the stack absorbs input, so every unrelated test would be
/// dispatching into it.  Tests that care about the downgrade construct
/// their own capabilities — see `theme_downgrade_tests` below.
pub(crate) fn make_app() -> App {
    let caps = Capabilities {
        color_depth: crate::terminal::ColorDepth::TrueColor,
        ..Capabilities::default()
    };
    let theme_file = (&Theme::default()).into();
    App::new(
        Config::default(),
        KeyBindingOverrides::default(),
        theme_file,
        None,
        caps,
        Vec::new(),
    )
    .expect("build app")
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
    use crate::config::{Config, DiagramsEnabled, ImagesEnabled, KeyBindingOverrides, Theme};
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
        App::new(
            config,
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
        // A persisted `Always`, chosen on the user's truecolor terminal,
        // must not decode here — every pixel would quantize into the
        // 256-color cube — but must also survive in `config` so it takes
        // effect again when they go back.
        let caps = Capabilities {
            color_depth: ColorDepth::Ansi256,
            ..Capabilities::minimal()
        };
        let mut config = Config {
            theme: "Dracula".into(),
            ..Config::default()
        };
        config.images.enabled = ImagesEnabled::Always;
        config.diagrams.enabled = DiagramsEnabled::Always;
        let app = App::new(
            config,
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
        // Session-only: the persisted choice is untouched.
        assert_eq!(app.config.images.enabled, ImagesEnabled::Always);
        assert_eq!(app.config.diagrams.enabled, DiagramsEnabled::Always);
    }

    #[test]
    fn a_truecolor_terminal_still_renders_media() {
        let caps = Capabilities {
            color_depth: ColorDepth::TrueColor,
            ..Capabilities::minimal()
        };
        let mut config = Config::default();
        config.images.enabled = ImagesEnabled::Always;
        config.diagrams.enabled = DiagramsEnabled::Always;
        let app = App::new(
            config,
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
        // First visit to a terminal that also can't render the theme:
        // the capabilities notice absorbs the downgrade explanation, so
        // the standalone modal must not also be queued underneath it.
        let app = app_with_welcome(ColorDepth::Ansi256, "Dracula", false);
        assert_eq!(app.config.theme, "256 Dark");
        assert!(app.modal_stack.contains::<TerminalCapabilitiesModal>());
        assert!(!app.modal_stack.contains::<ThemeDowngradeModal>());
    }

    #[test]
    fn an_on_demand_welcome_is_escapable_but_a_first_run_one_is_not() {
        // The welcome modal force-sets images / diagrams to `Never`
        // below truecolor and Save persists that.  Reopening it from the
        // palette on a weak terminal must therefore have a no-op exit,
        // or merely looking at the surface would overwrite the choices
        // the user made on their capable terminal.  A genuine first run
        // has nothing to overwrite and keeps Save as the only exit.
        // `show_welcome = false` so the startup welcome isn't already on
        // the stack — `open_welcome_modal` no-ops when one is.
        let mut app = app_with_welcome(ColorDepth::Ansi256, "Dracula", false);
        app.open_welcome_modal();
        let top = app.modal_stack.top_mut().expect("welcome is on top");
        assert!(top.as_any().is::<WelcomeModal>());
        assert!(top.dismissable());

        // Built directly: on a first-run launch that also downgrades,
        // the welcome sits *under* the theme-downgrade modal, so it is
        // not the top of the stack.
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
        // The modal displays a forced `Never` below truecolor, but that
        // is a session fact enforced by `media_renderable`.  Writing it
        // would overwrite the `Always` the user chose on their capable
        // terminal — one `config.toml` is typically shared between both.
        let mut app = app_with_welcome(ColorDepth::Ansi256, "Dracula", false);
        app.config.images.enabled = ImagesEnabled::Always;
        app.config.diagrams.enabled = DiagramsEnabled::Always;

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
        assert_eq!(app.config.diagrams.enabled, DiagramsEnabled::Always);
        // The session still refuses to draw them.
        assert!(!app.effective_images_enabled());
        assert!(!app.effective_diagrams_enabled());
    }

    #[test]
    fn a_colorless_terminal_is_not_downgraded() {
        // `Theme::from_file(.., monochrome)` strips every color on a
        // `NoColor` terminal regardless of the active theme, so the swap
        // would be invisible and the modal explaining it pure noise.
        let app = app_with(ColorDepth::NoColor, "Dracula");
        assert_eq!(app.config.theme, "Dracula");
        assert!(app.config.theme_downgraded_from.is_none());
        assert!(!app.modal_stack.contains::<ThemeDowngradeModal>());
    }

    #[test]
    fn a_monochrome_theme_is_not_substituted() {
        // `Monochrome Dark` resolves every slot to `Color::Reset`, so it
        // is already correct at any depth — swapping it for an RGB-free
        // but *less* neutral palette, plus a modal explaining the swap,
        // would be pure noise.
        let app = app_with(ColorDepth::Ansi16, "Monochrome Dark");
        assert_eq!(app.config.theme, "Monochrome Dark");
        assert!(app.config.theme_downgraded_from.is_none());
        assert!(!app.modal_stack.contains::<ThemeDowngradeModal>());
    }

    #[test]
    fn an_already_indexed_theme_is_not_substituted() {
        // No notice for a user who already runs `256 Light` — and the
        // light choice must not be flipped to dark.
        let app = app_with(ColorDepth::Ansi16, "256 Light");
        assert_eq!(app.config.theme, "256 Light");
        assert!(app.config.theme_downgraded_from.is_none());
        assert!(!app.modal_stack.contains::<ThemeDowngradeModal>());
    }
}
