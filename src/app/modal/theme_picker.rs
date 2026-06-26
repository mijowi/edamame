//! Theme picker modal.  Adapter that wraps
//! [`crate::ui::ThemePickerState`].  Selecting a theme writes
//! `config.theme`, saves the config, and reapplies the palette live —
//! mirroring the path the settings overlay used to take when its Theme
//! row was cycled.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::theme::{list_theme_names_for_mode, resolve_theme_for_mode_switch};
use crate::config::AppearanceMode;
use crate::ui::{ThemePickerResponse, ThemePickerState, ThemePickerView};

pub struct ThemePickerModal {
    state: ThemePickerState,
    /// Theme that was active when the picker opened.  Cached so a
    /// preview can be rolled back on Esc or replaced by Enter without
    /// re-reading `config.theme` (which is mutated mid-flight by each
    /// preview).
    original_theme: String,
    /// Appearance mode active when the picker opened — restored on
    /// Cancelled alongside `original_theme`.
    original_mode: AppearanceMode,
    /// Last theme the user had active under Dark while this picker
    /// session was open.  Populated lazily on mode toggle so flipping
    /// back to Dark restores the most-recently-active dark theme rather
    /// than falling through to the counterpart / default rules.
    remembered_dark: Option<String>,
    /// Mirror of `remembered_dark` for Light.
    remembered_light: Option<String>,
}

impl ThemePickerModal {
    pub fn new(themes: Vec<String>, current: String, mode: AppearanceMode) -> Self {
        let (remembered_dark, remembered_light) = match mode {
            AppearanceMode::Dark => (Some(current.clone()), None),
            AppearanceMode::Light => (None, Some(current.clone())),
        };
        Self {
            state: ThemePickerState::open(themes, current.clone(), mode),
            original_theme: current,
            original_mode: mode,
            remembered_dark,
            remembered_light,
        }
    }

    fn remembered_for(&self, mode: AppearanceMode) -> Option<&str> {
        match mode {
            AppearanceMode::Dark => self.remembered_dark.as_deref(),
            AppearanceMode::Light => self.remembered_light.as_deref(),
        }
    }

    fn remember(&mut self, mode: AppearanceMode, theme: String) {
        match mode {
            AppearanceMode::Dark => self.remembered_dark = Some(theme),
            AppearanceMode::Light => self.remembered_light = Some(theme),
        }
    }

    /// Compute the result of switching to `target` and apply the
    /// modal-state half of the transition (remembered-tracking,
    /// re-filter the list, re-focus, reset query).  Returns the theme
    /// name that should become live in `app.config.theme` — the caller
    /// applies it to `App` separately because `handle_click` has no
    /// `&mut App` and must defer that half into a `ContinueAnd`
    /// callback.  The keyboard handler runs both halves inline.
    fn switch_mode(&mut self, target: AppearanceMode) -> String {
        let prev_mode = self.state.mode;
        let prev_theme = self.state.current_theme().to_owned();
        self.remember(prev_mode, prev_theme.clone());
        let preview = self
            .remembered_for(target)
            .map(str::to_owned)
            .unwrap_or_else(|| resolve_theme_for_mode_switch(&prev_theme, target));
        let themes = list_theme_names_for_mode(target);
        self.state.replace_themes(themes, &preview, target);
        preview
    }
}

impl Modal for ThemePickerModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = ThemePickerView {
            theme: ctx.theme,
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
        match self.state.handle_key(&key) {
            ThemePickerResponse::Continue => ModalOutcome::Continue,
            ThemePickerResponse::Preview(name) => {
                // Live-preview: swap the palette in place without
                // touching disk.  The modal stays open; Enter will
                // commit, Esc will revert to `self.original_theme` /
                // `self.original_mode`.
                if app.config.theme != name {
                    app.config.theme = name;
                    app.apply_active_theme();
                }
                ModalOutcome::Continue
            }
            ThemePickerResponse::ModeChanged(mode) => {
                // Modal-state half (remembered-tracking, re-filter,
                // re-focus) runs synchronously; app-state half (theme
                // swap, appearance write, palette reapply) runs inline
                // because we already hold `&mut App` here.  The click
                // path runs the same two halves in the same order via
                // `ContinueAnd` because it lacks `&mut App`.
                let preview = self.switch_mode(mode);
                if app.config.theme != preview {
                    app.config.theme = preview;
                }
                app.config.appearance = mode;
                app.apply_active_theme();
                ModalOutcome::Continue
            }
            ThemePickerResponse::Cancelled => {
                let original_theme = self.original_theme.clone();
                let original_mode = self.original_mode;
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    let changed = app.config.theme != original_theme
                        || app.config.appearance != original_mode;
                    app.config.theme = original_theme;
                    app.config.appearance = original_mode;
                    if changed {
                        app.apply_active_theme();
                    }
                }))
            }
            ThemePickerResponse::Selected(name) => {
                let original_theme = self.original_theme.clone();
                let original_mode = self.original_mode;
                let selected_mode = self.state.mode;
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    let theme_changed = app.config.theme != name;
                    let mode_changed = app.config.appearance != selected_mode;
                    if theme_changed {
                        app.config.theme = name.clone();
                    }
                    app.config.appearance = selected_mode;
                    if theme_changed || mode_changed {
                        app.apply_active_theme();
                    }
                    if name != original_theme || selected_mode != original_mode {
                        app.save_config_with_flash("failed to persist theme change");
                    }
                }))
            }
        }
    }

    fn handle_paste(&mut self, text: &str) -> ModalOutcome {
        // Paste can only yield `Continue` or a live `Preview` (it just
        // grows the filter); reuse the same theme-swap as the keyboard
        // `Preview` arm.  Selection / cancellation never originate here.
        if let ThemePickerResponse::Preview(name) = self.state.paste(text) {
            return ModalOutcome::ContinueAnd(Box::new(move |app| {
                if app.config.theme != name {
                    app.config.theme = name;
                    app.apply_active_theme();
                }
            }));
        }
        ModalOutcome::Continue
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_state.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        if let Some(target) = self.state.toggle_hit(col, row) {
            // Mirror the keyboard `ModeChanged` flow: modal-state half
            // runs synchronously (we already have `&mut self`), app
            // half is deferred into `ContinueAnd` because
            // `handle_click` doesn't take an `App` reference.
            let preview = self.switch_mode(target);
            return ModalOutcome::ContinueAnd(Box::new(move |app| {
                if app.config.theme != preview {
                    app.config.theme = preview;
                }
                app.config.appearance = target;
                app.apply_active_theme();
            }));
        }
        super::types::close_if_esc_clicked(self.state.esc_button_rect, col, row)
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
    use super::*;
    use crate::app::test_utils::make_app;

    #[test]
    fn open_theme_picker_seeds_state() {
        let mut app = make_app();
        app.open_theme_picker();
        assert!(app.modal_stack.contains::<ThemePickerModal>());
    }

    #[test]
    fn switch_mode_round_trip_restores_original_theme() {
        // Open on Edamame (Dark) → toggle Light (snapshots Edamame as
        // dark, resolves a light preview) → toggle back to Dark must
        // restore Edamame, not fall through to the counterpart rule.
        let dark_list = vec!["Edamame".to_owned(), "256 Dark".to_owned()];
        let mut modal = ThemePickerModal::new(dark_list, "Edamame".into(), AppearanceMode::Dark);
        let _ = modal.switch_mode(AppearanceMode::Light);
        let back = modal.switch_mode(AppearanceMode::Dark);
        assert_eq!(back, "Edamame");
    }

    #[test]
    fn switch_mode_remembers_intermediate_previews() {
        // While on Dark, user previews "256 Dark" (state.last_previewed
        // updates).  Toggle Light → toggle Dark must restore "256 Dark",
        // not "Edamame".
        let dark_list = vec!["Edamame".to_owned(), "256 Dark".to_owned()];
        let mut modal = ThemePickerModal::new(dark_list, "Edamame".into(), AppearanceMode::Dark);
        modal.state.handle_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Down,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(modal.state.current_theme(), "256 Dark");
        let _ = modal.switch_mode(AppearanceMode::Light);
        let back = modal.switch_mode(AppearanceMode::Dark);
        assert_eq!(back, "256 Dark");
    }
}
