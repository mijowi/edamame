//! First-run welcome modal.  Adapter that wraps
//! [`crate::ui::WelcomeState`] — owns the in-flight tri-state choices,
//! routes Theme-button presses to [`crate::app::App::open_theme_picker`]
//! (which stacks the picker on top of this modal and pops back to it on
//! close), and persists the chosen settings on Save.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::sections::VIM_HANDLER;
use crate::config::Config;
use crate::terminal::Capabilities;
use crate::ui::{WelcomeResponse, WelcomeState, WelcomeView};

pub struct WelcomeModal {
    state: WelcomeState,
    fingerprint: String,
}

impl WelcomeModal {
    /// Construct the welcome modal from detected capabilities and the
    /// current config.  Returns `None` when the user has dismissed the
    /// welcome via the "Don't show this again" toggle on a previous
    /// run (`config.editor.show_welcome` is false).
    pub fn from_state(caps: &Capabilities, config: &Config) -> Option<Self> {
        if !config.editor.show_welcome {
            return None;
        }
        // Not dismissable: on a genuine first run Save is the only
        // resolution, and the "Show on next launch" toggle stands in for
        // Cancel.  Nothing is at risk — there is no prior choice to
        // overwrite.
        Some(Self::build(caps, config, false))
    }

    /// Construct the welcome modal unconditionally, ignoring
    /// `config.editor.show_welcome`.  Used by the on-demand paths —
    /// `Action::OpenWelcome` and the capabilities notice's
    /// "Adjust settings" button — where the user explicitly asked for
    /// this surface, so the first-run gate doesn't apply.  Because the
    /// state is rebuilt from the *live* `caps`, reopening it after a
    /// terminal change re-derives `full_color` / `image_capable` and
    /// re-applies the below-truecolor forcing.
    ///
    /// Dismissable, unlike the first-run instance.  Reopening carries a
    /// risk the first run doesn't: the user already has choices on disk,
    /// and below truecolor `WelcomeState::new` forces images and
    /// diagrams to `Never` while [`Self::save_outcome`] persists that
    /// forcing.  Without an `Esc` that writes nothing, merely *looking*
    /// at this surface from a weaker terminal would overwrite the
    /// settings chosen on a capable one.
    pub fn new(caps: &Capabilities, config: &Config) -> Self {
        Self::build(caps, config, true)
    }

    /// Park focus on the Save button so a test can activate it without
    /// depending on how many Tab presses the current row set requires.
    #[cfg(test)]
    pub(crate) fn focus_save_for_test(&mut self) {
        self.state.focused = crate::ui::WelcomeFocus::Save;
    }

    fn build(caps: &Capabilities, config: &Config, dismissable: bool) -> Self {
        Self {
            state: WelcomeState::new(
                caps,
                config.images.enabled,
                config.images.remote_policy,
                config.diagrams.enabled,
                config.modal.handler == VIM_HANDLER,
                config.editor.check_for_updates,
            )
            .with_dismissable(dismissable),
            fingerprint: caps.fingerprint(),
        }
    }
}

impl Modal for WelcomeModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = WelcomeView {
            theme: ctx.theme,
            theme_name: &ctx.config.theme,
        };
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        match self.state.handle_key(&key) {
            WelcomeResponse::Continue => ModalOutcome::Continue,
            WelcomeResponse::OpenThemePicker => {
                ModalOutcome::ContinueAnd(Box::new(|app| app.open_theme_picker()))
            }
            WelcomeResponse::Save => self.save_outcome(),
            // Plain `Close`, deliberately: no config write, and no
            // fingerprint seeding either — an on-demand opening is not
            // the first-visit notice and shouldn't silence it.
            WelcomeResponse::Cancel => ModalOutcome::Close,
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.handle_wheel(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        match self.state.handle_click(col, row) {
            WelcomeResponse::Continue => ModalOutcome::Continue,
            WelcomeResponse::OpenThemePicker => {
                ModalOutcome::ContinueAnd(Box::new(|app| app.open_theme_picker()))
            }
            WelcomeResponse::Save => self.save_outcome(),
            // Plain `Close`, deliberately: no config write, and no
            // fingerprint seeding either — an on-demand opening is not
            // the first-visit notice and shouldn't silence it.
            WelcomeResponse::Cancel => ModalOutcome::Close,
        }
    }

    fn kind(&self) -> ModalKind {
        ModalKind::Normal
    }

    fn dismissable(&self) -> bool {
        // Single source of truth with the `Esc` arm and the rendered
        // `esc` affordance — all three read `state.dismissable`.  False
        // on a first run (Save is the only resolution), true on every
        // on-demand opening.
        self.state.dismissable
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl WelcomeModal {
    /// Build a `CloseAnd` outcome that writes the user's choices into
    /// the config and persists.  Image-related fields follow the three
    /// cases `WelcomeState` distinguishes:
    ///
    /// - **Image-capable** (an image protocol *and* 24-bit color, see
    ///   `WelcomeState::image_capable`) — all three fields are the user's
    ///   choice and all three are written.
    /// - **Not image-capable** (below truecolor, or truecolor with no
    ///   image protocol) — none of the three is written.  The displayed
    ///   `Never` that `WelcomeState::new` forces below truecolor is a
    ///   *session* fact, enforced by `App::media_renderable`, which
    ///   refuses to decode there regardless of what `config` says.
    ///   Persisting it would be both redundant and destructive: one
    ///   `config.toml` is typically shared (dotfiles) with a capable
    ///   terminal, and writing `Never` would overwrite the `Always` the
    ///   user chose there — the same reasoning that keeps the
    ///   indexed-color theme substitution out of `Config::save`.  So the
    ///   existing values travel intact to a future capable terminal.
    ///
    /// This is why the modal can be safely reopened on demand (see
    /// `WelcomeState::dismissable`): neither Save nor Esc can now
    /// downgrade a config because of the terminal it was opened on.
    fn save_outcome(&self) -> ModalOutcome {
        let images = self.state.images;
        let remote = self.state.remote;
        let diagrams = self.state.diagrams;
        let use_vim = self.state.use_vim;
        let check_for_updates = self.state.check_for_updates;
        let dont_show_again = self.state.dont_show_again;
        let image_capable = self.state.image_capable;
        let fingerprint = self.fingerprint.clone();
        ModalOutcome::CloseAnd(Box::new(move |app| {
            // Only an image-capable terminal writes the media fields;
            // see the doc comment for why the forced-off values below
            // truecolor must stay session-only.
            if image_capable {
                app.config.images.enabled = images;
                app.config.images.remote_policy = remote;
                app.config.diagrams.enabled = diagrams;
            }
            // Vim is terminal-independent, so apply it unconditionally —
            // this both persists `modal.handler` and activates / clears
            // the running session's modal-editing state.
            app.set_vim_enabled(use_vim);
            // Likewise terminal-independent: an update check is a
            // network preference, not a rendering capability, so it is
            // never part of the below-truecolor forcing above.
            app.config.editor.check_for_updates = check_for_updates;
            app.config.editor.show_welcome = !dont_show_again;
            // The welcome modal already showed the capability summary for
            // this terminal, so seed the seen-fingerprints set with it —
            // otherwise the standalone capabilities notice would fire on
            // the very next launch.
            if !app
                .config
                .editor
                .seen_terminal_fingerprints
                .contains(&fingerprint)
            {
                app.config
                    .editor
                    .seen_terminal_fingerprints
                    .push(fingerprint);
            }
            app.save_config_with_flash("failed to persist welcome modal preferences");
            app.dispatch_image_decodes();
            app.editor.refresh_parsed();
        }))
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::make_app;
    use crate::terminal::{Capabilities, ColorDepth};
    use crate::ui::WelcomeFocus;

    fn caps_full() -> Capabilities {
        Capabilities {
            color_depth: ColorDepth::TrueColor,
            ..Capabilities::default()
        }
    }

    /// A `make_app()` plus the config-isolation guard, as one call.
    /// Save runs a real `Config::save`, and nothing redirects
    /// `~/.config/edamame` in a test run — unguarded, these tests
    /// rewrite the developer's own config file.  Returned as a tuple so
    /// the guard outlives the test body.
    fn isolated_app() -> (crate::test_env::ConfigIsolation, crate::app::App) {
        let iso = crate::test_env::config_isolation();
        let app = make_app();
        (iso, app)
    }

    /// Drive Save and run the resulting closure against `app`.
    fn save(modal: &mut WelcomeModal, app: &mut crate::app::App) {
        modal.focus_save_for_test();
        let outcome = modal.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            app,
            24,
            80,
        );
        match outcome {
            ModalOutcome::CloseAnd(f) => f(app),
            _ => panic!("Save should close and persist"),
        }
    }

    #[test]
    fn save_persists_the_check_for_updates_choice() {
        // `save_outcome` captures four adjacent `bool`s by name into one
        // closure, so this pins that the update toggle lands in its own
        // field rather than a neighbour's.
        let (_iso, mut app) = isolated_app();
        assert!(app.config.editor.check_for_updates, "on by default");
        let mut modal = WelcomeModal::new(&caps_full(), &app.config);

        modal.state.focused = WelcomeFocus::CheckUpdates;
        modal
            .state
            .handle_key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(!modal.state.check_for_updates, "the row flipped");

        // The vim toggle sits directly above it in the same capture
        // block and must be unaffected.
        let vim_before = app.config.modal.handler.clone();
        save(&mut modal, &mut app);
        assert!(!app.config.editor.check_for_updates);
        assert_eq!(app.config.modal.handler, vim_before);
    }

    #[test]
    fn save_leaves_the_update_check_on_when_untouched() {
        let (_iso, mut app) = isolated_app();
        let mut modal = WelcomeModal::new(&caps_full(), &app.config);
        save(&mut modal, &mut app);
        assert!(app.config.editor.check_for_updates);
    }

    #[test]
    fn the_update_check_choice_survives_a_weak_terminal() {
        // Images and diagrams are force-set to `Never` below truecolor
        // and deliberately not persisted.  An update check is a network
        // preference, not a rendering capability, so it must be written
        // whatever the terminal can draw.
        let (_iso, mut app) = isolated_app();
        let caps = Capabilities {
            color_depth: ColorDepth::Ansi256,
            ..Capabilities::minimal()
        };
        let mut modal = WelcomeModal::new(&caps, &app.config);
        modal.state.focused = WelcomeFocus::CheckUpdates;
        modal
            .state
            .handle_key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        save(&mut modal, &mut app);
        assert!(!app.config.editor.check_for_updates);
    }
}
