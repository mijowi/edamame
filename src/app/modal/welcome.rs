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
        Some(Self {
            state: WelcomeState::new(
                caps,
                config.images.enabled,
                config.images.remote_policy,
                config.diagrams.enabled,
                config.modal.handler == "vim",
            ),
            fingerprint: caps.fingerprint(),
        })
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
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.handle_wheel(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        match self.state.handle_click(col, row) {
            WelcomeResponse::Continue => ModalOutcome::Continue,
            WelcomeResponse::OpenThemePicker => {
                ModalOutcome::ContinueAnd(Box::new(|app| app.open_theme_picker()))
            }
            WelcomeResponse::Save => self.save_outcome(),
        }
    }

    fn kind(&self) -> ModalKind {
        ModalKind::Normal
    }

    fn dismissable(&self) -> bool {
        // No esc-cancels — Save is the only resolution.
        false
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
    /// the config and persists.  Image-related fields are written ONLY
    /// when the terminal actually has an image protocol — otherwise the
    /// existing config values (defaults to `Ask`) are preserved so the
    /// preference travels with the user to a future image-capable
    /// terminal.
    fn save_outcome(&self) -> ModalOutcome {
        let images = self.state.images;
        let remote = self.state.remote;
        let diagrams = self.state.diagrams;
        let use_vim = self.state.use_vim;
        let dont_show_again = self.state.dont_show_again;
        let image_capable = self.state.image_capable;
        let fingerprint = self.fingerprint.clone();
        ModalOutcome::CloseAnd(Box::new(move |app| {
            if image_capable {
                app.config.images.enabled = images;
                app.config.images.remote_policy = remote;
                app.config.diagrams.enabled = diagrams;
            }
            // Vim is terminal-independent, so apply it unconditionally —
            // this both persists `modal.handler` and activates / clears
            // the running session's modal-editing state.
            app.set_vim_enabled(use_vim);
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
