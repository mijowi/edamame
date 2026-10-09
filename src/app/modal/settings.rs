//! Settings overlay — adapter wrapping [`crate::ui::SettingsState`].
//!
//! Field changes drive [`crate::app::App::save_config_with_flash`].  The "Open config.toml in
//! external editor" row sets a deferred flag the run loop drains, since the editor invocation needs
//! the `&mut Terminal` only the run loop owns.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::sections::VIM_HANDLER;
use crate::config::Config;
use crate::ui::settings_overlay::{
    LABEL_BIG_H1, LABEL_BLINK_CURSOR, LABEL_MATH_PREVIEW, LABEL_OUTLINE, LABEL_REFLOW,
    LABEL_SCROLL_SPEED, LABEL_SHOW_DIAGRAMS, LABEL_SHOW_IMAGES, LABEL_SHOW_REMOTE_IMAGES,
    LABEL_SYNTAX_HIGHLIGHTING, LABEL_VIM_MODE, LABEL_VISUAL_LINE_NAV,
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

/// Map a [`SettingsResponse`] to a [`ModalOutcome`], running its App-side effects.  Shared by the
/// key and click paths so a click on a row behaves exactly like operating it from the keyboard.
fn resolve(app: &mut App, response: SettingsResponse) -> ModalOutcome {
    match response {
        SettingsResponse::Continue => ModalOutcome::Continue,
        SettingsResponse::Cancelled => ModalOutcome::Close,
        SettingsResponse::OpenInExternalEditor => {
            // Record intent only; the run loop owns the `Terminal` handle the editor needs.
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

/// Push a single settings-overlay change into App-owned cached state.  Separate from [`resolve`]
/// so the live-update wiring can be unit-tested without the full overlay key dispatch.
pub(crate) fn apply_live_update(label: &str, app: &mut App) {
    match label {
        LABEL_BIG_H1 => app.editor.set_big_h1(app.config.editor.big_h1),
        LABEL_REFLOW => app.editor.set_reflow(app.config.editor.reflow),
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
        LABEL_OUTLINE => {
            app.outline_focused = false;
            app.outline_browsing = false;
            app.explain_hidden_outline();
        }
        LABEL_VISUAL_LINE_NAV => {
            app.editor.visual_line_nav = app.config.editor.visual_line_nav;
        }
        LABEL_VIM_MODE => {
            // Rebuild the live VimState so vim editing turns on/off without a restart.
            app.set_vim_enabled(app.config.modal.handler == VIM_HANDLER);
        }
        // These rows emit `FieldChanged` only on a real transition, never for a no-op cycle.
        LABEL_SHOW_IMAGES => app.apply_images_setting_change(),
        LABEL_SHOW_REMOTE_IMAGES => app.apply_remote_policy_change(),
        LABEL_SHOW_DIAGRAMS => app.apply_diagrams_setting_change(),
        LABEL_MATH_PREVIEW => app.editor.set_math_preview(app.config.figures.math_preview),
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::{app_with_buffer, make_app};
    use crate::ui::settings_overlay::all_row_labels;

    /// Labels whose config field is also cached on `App` and so needs an explicit push to take
    /// effect without a restart.  Kept in sync by [`live_update_coverage_is_exhaustive`].
    const LIVE_UPDATE_LABELS: &[&str] = &[
        LABEL_BIG_H1,
        LABEL_REFLOW,
        LABEL_BLINK_CURSOR,
        LABEL_MATH_PREVIEW,
        LABEL_SCROLL_SPEED,
        LABEL_SHOW_DIAGRAMS,
        LABEL_SHOW_IMAGES,
        LABEL_SHOW_REMOTE_IMAGES,
        LABEL_SYNTAX_HIGHLIGHTING,
        LABEL_VISUAL_LINE_NAV,
        LABEL_VIM_MODE,
    ];

    /// Labels read live, with no arm in [`apply_live_update`].  Together with
    /// [`LIVE_UPDATE_LABELS`] this must account for every row in the overlay.
    const NON_LIVE_UPDATE_LABELS: &[&str] = &[
        crate::ui::settings_overlay::HEADER_NOTE,
        "",
        "Open config folder",
        "Open config.toml",
        "",
        "Autosave",
        "  Char limit",
        "Check for updates",
        "Daily tips",
        "Diff when file changes",
        "Limit editor width",
        "Show line numbers",
        "Show outline",
        "Show table buttons",
    ];

    #[test]
    fn live_update_coverage_is_exhaustive() {
        // A new row in `build_rows` trips this until it is classified as live-update or not.
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
    fn live_update_pushes_math_preview_into_editor_cache() {
        let mut app = make_app();
        let original = app.editor.math_preview;
        app.config.figures.math_preview = !original;
        apply_live_update(LABEL_MATH_PREVIEW, &mut app);
        assert_eq!(app.editor.math_preview, app.config.figures.math_preview);
        assert_ne!(app.editor.math_preview, original);
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

        app.config.modal.handler = "vim".into();
        apply_live_update(LABEL_VIM_MODE, &mut app);
        assert!(app.vim.is_some(), "vim mode on builds VimState");

        app.config.modal.handler = "default".into();
        apply_live_update(LABEL_VIM_MODE, &mut app);
        assert!(app.vim.is_none(), "vim mode off clears VimState");
    }

    #[test]
    fn live_update_images_ask_queues_prompt_and_resets_session_answer() {
        let mut app = app_with_buffer("![a](img.png)\n", 0);
        // An earlier session-level decline, then a settings change to `Ask`.
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
        app.config.figures.enabled = crate::config::FiguresEnabled::Ask;
        apply_live_update(LABEL_SHOW_DIAGRAMS, &mut app);
        assert_eq!(app.session_diagrams_enabled, None);
        assert!(
            app.editor.diagrams_enabled,
            "Ask reserves diagram rows again"
        );
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::FiguresEnabledPromptModal>());
    }

    #[test]
    fn live_update_diagrams_never_collapses_layout_and_drops_prompt() {
        let mut app = app_with_buffer("```mermaid\ngraph TD;\n```\n", 0);
        app.config.figures.enabled = crate::config::FiguresEnabled::Ask;
        apply_live_update(LABEL_SHOW_DIAGRAMS, &mut app);
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::FiguresEnabledPromptModal>());
        app.config.figures.enabled = crate::config::FiguresEnabled::Never;
        apply_live_update(LABEL_SHOW_DIAGRAMS, &mut app);
        assert!(!app.editor.diagrams_enabled);
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::FiguresEnabledPromptModal>());
    }

    #[test]
    fn live_update_diagrams_always_enables_layout_without_prompt() {
        let mut app = app_with_buffer("```mermaid\ngraph TD;\n```\n", 0);
        app.session_diagrams_enabled = Some(false);
        app.editor.diagrams_enabled = false;
        app.config.figures.enabled = crate::config::FiguresEnabled::Always;
        apply_live_update(LABEL_SHOW_DIAGRAMS, &mut app);
        assert!(app.editor.diagrams_enabled);
        assert_eq!(app.session_diagrams_enabled, None);
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::FiguresEnabledPromptModal>());
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
        // Default focus is the first editable row; one Up skips the divider to the editor row.
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 40, 80);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);
        assert!(app.pending_open_config_in_editor);
        assert!(!app.modal_stack.contains::<SettingsOverlayModal>());
    }

    #[test]
    fn settings_overlay_open_config_folder_closes_overlay() {
        // Two Up presses from the default focus (skipping the divider) reach the folder row.
        // It opens the OS file manager, so no `pending_open_config_in_editor` flag is set.
        let mut app = make_app();
        app.open_settings_overlay();
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 40, 80);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 40, 80);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);
        assert!(!app.pending_open_config_in_editor);
        assert!(!app.modal_stack.contains::<SettingsOverlayModal>());
    }
}
