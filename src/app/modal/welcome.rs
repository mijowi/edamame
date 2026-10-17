//! First-run welcome modal: an adapter over [`crate::ui::WelcomeState`] that
//! holds the in-flight choices, routes the Theme button to
//! [`crate::app::App::open_theme_picker`], and persists on Save.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::Config;
use crate::terminal::Capabilities;
use crate::ui::{WelcomeResponse, WelcomeState, WelcomeView};

pub struct WelcomeModal {
    state: WelcomeState,
    fingerprint: String,
}

impl WelcomeModal {
    /// The first-run instance, or `None` once `config.editor.show_welcome` is
    /// off.
    pub fn from_state(caps: &Capabilities, config: &Config) -> Option<Self> {
        if !config.editor.show_welcome {
            return None;
        }
        // Not dismissable: on a first run Save is the only resolution, the
        // "Show on next launch" toggle stands in for Cancel, and there is no
        // prior choice to overwrite.
        Some(Self::build(caps, config, false))
    }

    /// The on-demand instance (`Action::OpenWelcome`, the capabilities notice's
    /// "Adjust settings"), built unconditionally and from the *live* `caps`, so
    /// reopening after a terminal change re-derives `full_color` /
    /// `image_capable`.
    ///
    /// Dismissable, unlike the first-run instance: the user already has choices
    /// on disk, and below truecolor `WelcomeState::new` forces images and
    /// diagrams to `Never`.  Without an `Esc` that writes nothing, merely
    /// *looking* at this surface from a weaker terminal would overwrite the
    /// settings chosen on a capable one.
    pub fn new(caps: &Capabilities, config: &Config) -> Self {
        Self::build(caps, config, true)
    }

    /// Park focus on Save, so a test needn't count Tab presses.
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
                config.figures.enabled,
                &config.modal.handler,
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
            // Plain `Close`, deliberately: no config write, and no fingerprint
            // seeding — an on-demand opening is not the first-visit notice and
            // shouldn't silence it.
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
            // As in `handle_key`: no config write, no fingerprint seeding.
            WelcomeResponse::Cancel => ModalOutcome::Close,
        }
    }

    fn kind(&self) -> ModalKind {
        ModalKind::Normal
    }

    fn dismissable(&self) -> bool {
        // Shared with the `Esc` arm and the rendered affordance.
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
    /// Build a `CloseAnd` outcome that writes the user's choices and persists.
    ///
    /// The three image fields are written only on an image-capable terminal (an
    /// image protocol *and* 24-bit color).  Elsewhere the `Never` that
    /// `WelcomeState::new` forces is a *session* fact — `App::media_renderable`
    /// refuses to decode there whatever `config` says — and persisting it would
    /// overwrite the `Always` chosen on a capable terminal sharing the same
    /// dotfile.  Same reasoning that keeps the indexed-color theme substitution
    /// out of `Config::save`, and what makes the modal safe to reopen on
    /// demand.
    fn save_outcome(&self) -> ModalOutcome {
        let images = self.state.images;
        let remote = self.state.remote;
        let diagrams = self.state.diagrams;
        let handler = self.state.handler.clone();
        let check_for_updates = self.state.check_for_updates;
        let dont_show_again = self.state.dont_show_again;
        let image_capable = self.state.image_capable;
        let fingerprint = self.fingerprint.clone();
        ModalOutcome::CloseAnd(Box::new(move |app| {
            // See the doc comment: forced-off values stay session-only.
            if image_capable {
                app.config.images.enabled = images;
                app.config.images.remote_policy = remote;
                app.config.figures.enabled = diagrams;
            }
            // Terminal-independent, and this both persists `modal.handler` and
            // updates the running session's modal-editing state.
            app.set_modal_handler(&handler);
            // Likewise: a network preference, not a rendering capability.
            app.config.editor.check_for_updates = check_for_updates;
            app.config.editor.show_welcome = !dont_show_again;
            // This modal already showed the capability summary, so seed the
            // seen set or the standalone notice fires on the next launch.  The
            // fingerprint now lives in `state.toml`, so this outcome writes both
            // files: the config settings above and the state below.
            if !app.state.seen_terminal_fingerprints.contains(&fingerprint) {
                app.state.seen_terminal_fingerprints.push(fingerprint);
            }
            app.save_config_with_flash("failed to persist welcome modal preferences");
            // The state write is best-effort: `save_config_with_flash` already flashed for the
            // config half, so a failure here only logs (a second flash would double up) and the
            // notice simply re-fires next launch.
            if let Err(e) = app.state.save() {
                tracing::warn!(error = %e, "failed to persist welcome modal seen-terminal state");
            }
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
    use crate::config::sections::HELIX_HANDLER;
    use crate::terminal::{Capabilities, ColorDepth};
    use crate::ui::WelcomeFocus;

    fn caps_full() -> Capabilities {
        Capabilities {
            color_depth: ColorDepth::TrueColor,
            ..Capabilities::default()
        }
    }

    /// `make_app()` plus the config-isolation guard: Save runs a real
    /// `Config::save`, which unguarded rewrites the developer's own config.
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
        // `save_outcome` captures four adjacent `bool`s into one closure, so
        // pin that this toggle lands in its own field.
        let (_iso, mut app) = isolated_app();
        assert!(app.config.editor.check_for_updates, "on by default");
        let mut modal = WelcomeModal::new(&caps_full(), &app.config);

        modal.state.focused = WelcomeFocus::CheckUpdates;
        modal
            .state
            .handle_key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(!modal.state.check_for_updates, "the row flipped");

        // The editing-mode pill sits directly above it and must be unaffected.
        let handler_before = app.config.modal.handler.clone();
        save(&mut modal, &mut app);
        assert!(!app.config.editor.check_for_updates);
        assert_eq!(app.config.modal.handler, handler_before);
    }

    #[test]
    fn save_leaves_the_update_check_on_when_untouched() {
        let (_iso, mut app) = isolated_app();
        let mut modal = WelcomeModal::new(&caps_full(), &app.config);
        save(&mut modal, &mut app);
        assert!(app.config.editor.check_for_updates);
    }

    #[test]
    fn save_persists_and_activates_helix_handler() {
        let (_iso, mut app) = isolated_app();
        let mut modal = WelcomeModal::new(&caps_full(), &app.config);
        modal.state.handler = HELIX_HANDLER.to_owned();
        save(&mut modal, &mut app);
        assert_eq!(app.config.modal.handler, HELIX_HANDLER);
        assert!(app.modal_input.helix().is_some());
    }

    #[test]
    fn the_update_check_choice_survives_a_weak_terminal() {
        // Images and figures are forced to `Never` below truecolor and not
        // persisted; a network preference must be written regardless.
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
