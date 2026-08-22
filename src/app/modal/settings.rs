//! Settings overlay.  Adapter wrapping
//! [`crate::ui::SettingsState`].
//!
//! Field changes drive [`crate::app::App::save_config_with_flash`];
//! the Theme row also drives [`crate::app::App::apply_active_theme`]
//! so the color palette updates live.  The "Open config.toml in
//! external editor" row sets a deferred flag that the run loop
//! drains — the editor invocation needs `&mut Terminal` which only
//! the run loop owns.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::sections::VIM_HANDLER;
use crate::config::Config;
use crate::ui::settings_overlay::{
    LABEL_BIG_H1, LABEL_BLINK_CURSOR, LABEL_SCROLL_SPEED, LABEL_SHOW_DIAGRAMS, LABEL_SHOW_IMAGES,
    LABEL_SHOW_REMOTE_IMAGES, LABEL_SYNTAX_HIGHLIGHTING, LABEL_VIM_MODE, LABEL_VISUAL_LINE_NAV,
};
use crate::ui::{ModalKind, SettingsResponse, SettingsState, SettingsView};

pub struct SettingsOverlayModal {
    state: SettingsState,
}

impl SettingsOverlayModal {
    pub fn new() -> Self {
        Self {
            state: SettingsState::new(),
        }
    }
}

/// Map a settings-overlay [`SettingsResponse`] to a [`ModalOutcome`],
/// running its App-side effects.  Shared by the key and click paths so a
/// mouse click on a row behaves exactly like operating it from the
/// keyboard.
fn resolve(app: &mut App, response: SettingsResponse) -> ModalOutcome {
    match response {
        SettingsResponse::Continue => ModalOutcome::Continue,
        SettingsResponse::Cancelled => ModalOutcome::Close,
        SettingsResponse::OpenInExternalEditor => {
            // The actual editor invocation needs the live `Terminal`
            // handle, owned by the run loop.  Record intent here and let
            // the loop drain the flag at the end of this iteration.
            ModalOutcome::CloseAnd(Box::new(|app| {
                app.pending_open_config_in_editor = true;
                app.needs_draw = true;
            }))
        }
        SettingsResponse::OpenConfigFolder => ModalOutcome::CloseAnd(Box::new(|app| {
            if let Some(dir) = Config::config_dir() {
                app.spawn_open_worker(dir.display().to_string());
            } else {
                app.notify("No config directory available", ModalKind::Error);
            }
            app.needs_draw = true;
        })),
        SettingsResponse::FieldChanged(label) => {
            app.save_config_with_flash("failed to persist settings overlay change");
            apply_live_update(label, app);
            ModalOutcome::Continue
        }
    }
}

/// Push a single settings-overlay change into App-owned cached
/// state.  Called from the `FieldChanged` arm above; extracted so
/// the live-update wiring can be unit-tested without going through
/// the full overlay key dispatch.
pub(crate) fn apply_live_update(label: &str, app: &mut App) {
    match label {
        LABEL_BIG_H1 => app.editor.set_big_h1(app.config.editor.big_h1),
        LABEL_SYNTAX_HIGHLIGHTING => app
            .editor
            .set_syntax_highlighting(app.config.editor.syntax_highlighting),
        LABEL_BLINK_CURSOR => app.editor.cursor_blink.apply_config(
            app.config.editor.cursor_blink,
            app.config.editor.cursor_blink_ms,
        ),
        LABEL_SCROLL_SPEED => app
            .mouse
            .set_wheel_step(app.config.editor.mouse_scroll_lines),
        LABEL_VISUAL_LINE_NAV => {
            app.editor.visual_line_nav = app.config.editor.visual_line_nav;
        }
        LABEL_VIM_MODE => {
            // The row cycle flipped `config.modal.handler`; rebuild the
            // live VimState (and resting mode) to match so vim editing
            // turns on/off immediately without a restart.
            app.set_vim_enabled(app.config.modal.handler == VIM_HANDLER);
        }
        // The image and diagram rows emit `FieldChanged` only on a real
        // value transition, so these handlers never run for a no-op cycle.
        LABEL_SHOW_IMAGES => app.apply_images_setting_change(),
        LABEL_SHOW_REMOTE_IMAGES => app.apply_remote_policy_change(),
        LABEL_SHOW_DIAGRAMS => app.apply_diagrams_setting_change(),
        _ => {}
    }
}

impl Default for SettingsOverlayModal {
    fn default() -> Self {
        Self::new()
    }
}

impl Modal for SettingsOverlayModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = SettingsView {
            theme: ctx.theme,
            config: ctx.config,
            cursor_visible: ctx.cursor_visible,
        };
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        let response = self.state.handle_key(&key, &mut app.config);
        resolve(app, response)
    }

    fn handle_paste(&mut self, text: &str) -> ModalOutcome {
        self.state.paste(text);
        ModalOutcome::Continue
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_state.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, app: &mut App) -> ModalOutcome {
        // An `esc` close-hint click dismisses, like every other modal.
        if super::types::esc_rect_hit(self.state.esc_button_rect, col, row) {
            return ModalOutcome::Close;
        }
        let response = self.state.handle_click(col, row, &mut app.config);
        resolve(app, response)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    //! Settings overlay App-level wiring.  The "Open config.toml in
    //! default editor" row defers the actual editor invocation to the
    //! run loop so it can drive the terminal suspend/resume.

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::{app_with_buffer, make_app};
    use crate::ui::settings_overlay::all_row_labels;

    /// Labels whose underlying config field is also cached on `App`
    /// (or a child) at startup and therefore needs an explicit push
    /// after a settings-overlay edit to take effect without
    /// restarting.  The rest of the rows are read live at render /
    /// use time.  Kept in sync with the row table by
    /// [`live_update_coverage_is_exhaustive`].
    const LIVE_UPDATE_LABELS: &[&str] = &[
        LABEL_BIG_H1,
        LABEL_BLINK_CURSOR,
        LABEL_SCROLL_SPEED,
        LABEL_SHOW_DIAGRAMS,
        LABEL_SHOW_IMAGES,
        LABEL_SHOW_REMOTE_IMAGES,
        LABEL_SYNTAX_HIGHLIGHTING,
        LABEL_VISUAL_LINE_NAV,
        LABEL_VIM_MODE,
    ];

    /// Labels that are read live (no App-side cache to push to) and
    /// therefore intentionally have no arm in [`apply_live_update`].
    /// Includes the two non-editable "open externally" rows and the
    /// blank divider.  Kept next to [`LIVE_UPDATE_LABELS`] so the
    /// pair must add up to every row in the overlay.
    const NON_LIVE_UPDATE_LABELS: &[&str] = &[
        crate::ui::settings_overlay::HEADER_NOTE,
        "",
        "Open config folder",
        "Open config.toml",
        "",
        "Autosave",
        "  Char limit",
        "Check for updates",
        "Diff when file changes",
        "Limit editor width",
        "Show line numbers",
        "Show table buttons",
    ];

    #[test]
    fn live_update_coverage_is_exhaustive() {
        // Every row in the settings overlay must appear in exactly
        // one of LIVE_UPDATE_LABELS or NON_LIVE_UPDATE_LABELS.
        // Adding a new row to `build_rows` without classifying it
        // here trips this test, forcing the author to decide whether
        // the new field needs an `apply_live_update` arm or is read
        // live at use time.
        let actual = all_row_labels();
        let mut classified: Vec<&str> = LIVE_UPDATE_LABELS
            .iter()
            .copied()
            .chain(NON_LIVE_UPDATE_LABELS.iter().copied())
            .collect();
        classified.sort();
        let mut sorted_actual = actual.clone();
        sorted_actual.sort();
        assert_eq!(
            sorted_actual, classified,
            "settings overlay rows changed; update LIVE_UPDATE_LABELS \
             and/or NON_LIVE_UPDATE_LABELS in src/app/modal/settings.rs"
        );
        // Guard against a label appearing in both lists.
        for label in LIVE_UPDATE_LABELS {
            assert!(
                !NON_LIVE_UPDATE_LABELS.contains(label),
                "{label:?} is in both LIVE_UPDATE_LABELS and NON_LIVE_UPDATE_LABELS"
            );
        }
    }

    #[test]
    fn live_update_pushes_big_h1_into_editor_cache() {
        let mut app = make_app();
        let original = app.editor.big_h1;
        app.config.editor.big_h1 = !original;
        apply_live_update(LABEL_BIG_H1, &mut app);
        assert_eq!(app.editor.big_h1, app.config.editor.big_h1);
        assert_ne!(app.editor.big_h1, original);
    }

    #[test]
    fn live_update_pushes_visual_line_nav_into_editor_cache() {
        let mut app = make_app();
        let original = app.editor.visual_line_nav;
        app.config.editor.visual_line_nav = !original;
        apply_live_update(LABEL_VISUAL_LINE_NAV, &mut app);
        assert_eq!(
            app.editor.visual_line_nav,
            app.config.editor.visual_line_nav
        );
        assert_ne!(app.editor.visual_line_nav, original);
    }

    #[test]
    fn live_update_pushes_scroll_speed_into_mouse_dispatcher() {
        let mut app = make_app();
        let new_step = app.config.editor.mouse_scroll_lines + 7;
        app.config.editor.mouse_scroll_lines = new_step;
        apply_live_update(LABEL_SCROLL_SPEED, &mut app);
        assert_eq!(app.mouse.wheel_step(), new_step);
    }

    #[test]
    fn live_update_toggles_vim_state_on_and_off() {
        let mut app = make_app();
        // make_app starts with the default handler → no vim state.
        assert!(app.vim.is_none());

        // Enable: the row cycle flips the handler, then the live update
        // builds the VimState.
        app.config.modal.handler = "vim".into();
        apply_live_update(LABEL_VIM_MODE, &mut app);
        assert!(app.vim.is_some(), "vim mode on builds VimState");

        // Disable: handler back to default, live update tears it down.
        app.config.modal.handler = "default".into();
        apply_live_update(LABEL_VIM_MODE, &mut app);
        assert!(app.vim.is_none(), "vim mode off clears VimState");
    }

    #[test]
    fn live_update_images_ask_queues_prompt_and_resets_session_answer() {
        let mut app = app_with_buffer("![a](img.png)\n", 0);
        // Simulate an earlier session-level decline, then a settings
        // change to `Ask`.
        app.session_images_enabled = Some(false);
        app.editor.images_enabled = false;
        app.config.images.enabled = crate::config::ImagesEnabled::Ask;
        apply_live_update(LABEL_SHOW_IMAGES, &mut app);
        assert_eq!(app.session_images_enabled, None);
        assert!(app.editor.images_enabled, "Ask reserves image rows again");
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::ImagesEnabledPromptModal>());
    }

    #[test]
    fn live_update_images_never_collapses_layout_and_drops_prompts() {
        let mut app = app_with_buffer("![a](img.png)\n", 0);
        app.config.images.enabled = crate::config::ImagesEnabled::Ask;
        apply_live_update(LABEL_SHOW_IMAGES, &mut app);
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::ImagesEnabledPromptModal>());
        // Flip to Never: layout collapses and the queued prompt is gone.
        app.config.images.enabled = crate::config::ImagesEnabled::Never;
        apply_live_update(LABEL_SHOW_IMAGES, &mut app);
        assert!(!app.editor.images_enabled);
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::ImagesEnabledPromptModal>());
    }

    #[test]
    fn live_update_images_always_enables_layout_without_prompt() {
        let mut app = app_with_buffer("![a](img.png)\n", 0);
        app.session_images_enabled = Some(false);
        app.editor.images_enabled = false;
        app.config.images.enabled = crate::config::ImagesEnabled::Always;
        apply_live_update(LABEL_SHOW_IMAGES, &mut app);
        assert!(app.editor.images_enabled);
        assert_eq!(app.session_images_enabled, None);
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::ImagesEnabledPromptModal>());
    }

    #[test]
    fn live_update_diagrams_ask_queues_prompt_and_resets_session_answer() {
        let mut app = app_with_buffer("```mermaid\ngraph TD;\n```\n", 0);
        app.session_diagrams_enabled = Some(false);
        app.editor.diagrams_enabled = false;
        app.config.diagrams.enabled = crate::config::DiagramsEnabled::Ask;
        apply_live_update(LABEL_SHOW_DIAGRAMS, &mut app);
        assert_eq!(app.session_diagrams_enabled, None);
        assert!(
            app.editor.diagrams_enabled,
            "Ask reserves diagram rows again"
        );
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::DiagramsEnabledPromptModal>());
    }

    #[test]
    fn live_update_diagrams_never_collapses_layout_and_drops_prompt() {
        let mut app = app_with_buffer("```mermaid\ngraph TD;\n```\n", 0);
        app.config.diagrams.enabled = crate::config::DiagramsEnabled::Ask;
        apply_live_update(LABEL_SHOW_DIAGRAMS, &mut app);
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::DiagramsEnabledPromptModal>());
        app.config.diagrams.enabled = crate::config::DiagramsEnabled::Never;
        apply_live_update(LABEL_SHOW_DIAGRAMS, &mut app);
        assert!(!app.editor.diagrams_enabled);
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::DiagramsEnabledPromptModal>());
    }

    #[test]
    fn live_update_diagrams_always_enables_layout_without_prompt() {
        let mut app = app_with_buffer("```mermaid\ngraph TD;\n```\n", 0);
        app.session_diagrams_enabled = Some(false);
        app.editor.diagrams_enabled = false;
        app.config.diagrams.enabled = crate::config::DiagramsEnabled::Always;
        apply_live_update(LABEL_SHOW_DIAGRAMS, &mut app);
        assert!(app.editor.diagrams_enabled);
        assert_eq!(app.session_diagrams_enabled, None);
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::DiagramsEnabledPromptModal>());
    }

    #[test]
    fn live_update_remote_ask_queues_prompt_and_resets_session_allow() {
        let mut app = app_with_buffer("![a](https://example.com/a.png)\n", 0);
        app.config.images.enabled = crate::config::ImagesEnabled::Always;
        app.session_allow_remote = true;
        app.config.images.remote_policy = crate::config::RemoteImagePolicy::Ask;
        apply_live_update(LABEL_SHOW_REMOTE_IMAGES, &mut app);
        assert!(!app.session_allow_remote);
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::RemoteImagePromptModal>());
    }

    #[test]
    fn live_update_remote_never_evicts_cached_remote_decodes() {
        let mut app = app_with_buffer("![a](https://example.com/a.png)\n", 0);
        app.editor.images.set_decoded(
            "https://example.com/a.png",
            image::DynamicImage::new_rgba8(1, 1),
        );
        app.session_allow_remote = true;
        app.config.images.remote_policy = crate::config::RemoteImagePolicy::Never;
        apply_live_update(LABEL_SHOW_REMOTE_IMAGES, &mut app);
        assert!(!app.session_allow_remote);
        assert!(
            app.editor
                .images
                .status("https://example.com/a.png")
                .is_none(),
            "remote decode evicted so the new policy re-resolves it"
        );
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::RemoteImagePromptModal>());
    }

    #[test]
    fn settings_overlay_open_external_sets_pending_flag_and_closes_overlay() {
        let mut app = make_app();
        app.open_settings_overlay();
        assert!(app.modal_stack.contains::<SettingsOverlayModal>());
        // Default focus is the first editable row; one Up skips the divider and lands
        // on the editor row.
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 40, 80);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);
        assert!(app.pending_open_config_in_editor);
        assert!(!app.modal_stack.contains::<SettingsOverlayModal>());
    }

    #[test]
    fn settings_overlay_open_config_folder_closes_overlay() {
        // The top-row "Open config folder" entry hands the path to the
        // OS file manager via `spawn_open_worker` and closes the
        // overlay.  No `pending_open_config_in_editor` flag is set —
        // that path is editor-only.  Default focus is the first editable row; two Up
        // presses (skipping the divider) reach the folder row.
        let mut app = make_app();
        app.open_settings_overlay();
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 40, 80);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 40, 80);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);
        assert!(!app.pending_open_config_in_editor);
        assert!(!app.modal_stack.contains::<SettingsOverlayModal>());
    }
}
