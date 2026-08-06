//! Theme picker modal.  Drives a
//! [`SearchableList<String>`](crate::ui::searchable_list::SearchableList) of
//! theme names plus an appearance toggle.  Live-preview swaps the palette in
//! place as focus moves ([`ListEvent::FocusChanged`]); selecting a theme
//! writes `config.theme`, saves the config, and reapplies the palette.

use std::any::Any;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::theme::{list_theme_names_for_mode, resolve_theme_for_mode_switch};
use crate::config::AppearanceMode;
use crate::ui::searchable_list::{ListEvent, SearchableList};
use crate::ui::theme_picker::{build_theme_list, render_theme_picker};

pub struct ThemePickerModal {
    list: SearchableList<String>,
    /// Theme that was active when the picker opened — drives the `current`
    /// suffix and the focused-row preselect.
    current: String,
    /// Live appearance mode (flips with the toggle).
    mode: AppearanceMode,
    /// Theme active when the picker opened.  Restored on Esc/cancel.
    original_theme: String,
    /// Appearance mode active when the picker opened.
    original_mode: AppearanceMode,
    /// Most-recently-active theme under Dark while this session was open.
    remembered_dark: Option<String>,
    /// Mirror of `remembered_dark` for Light.
    remembered_light: Option<String>,
    esc_button_rect: Option<Rect>,
    toggle_rect: Option<Rect>,
}

impl ThemePickerModal {
    pub fn new(themes: Vec<String>, current: String, mode: AppearanceMode) -> Self {
        let (remembered_dark, remembered_light) = match mode {
            AppearanceMode::Dark => (Some(current.clone()), None),
            AppearanceMode::Light => (None, Some(current.clone())),
        };
        Self {
            list: build_theme_list(themes, &current),
            current: current.clone(),
            mode,
            original_theme: current,
            original_mode: mode,
            remembered_dark,
            remembered_light,
            esc_button_rect: None,
            toggle_rect: None,
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

    /// The theme currently driving the live preview (focused row).
    fn current_theme(&self) -> &str {
        self.list
            .focused_item()
            .map(String::as_str)
            .unwrap_or(&self.original_theme)
    }

    /// Compute + apply the modal-state half of a mode switch: remember the
    /// outgoing theme, rebuild the list for `target`, re-focus the preview.
    /// Returns the theme name that should become live; the caller applies it
    /// to `App`.
    fn switch_mode(&mut self, target: AppearanceMode) -> String {
        let prev_mode = self.mode;
        let prev_theme = self.current_theme().to_owned();
        self.remember(prev_mode, prev_theme.clone());
        let preview = self
            .remembered_for(target)
            .map(str::to_owned)
            .unwrap_or_else(|| resolve_theme_for_mode_switch(&prev_theme, target));
        self.list.set_items(list_theme_names_for_mode(target));
        let want = preview.clone();
        self.list.focus_matching(|t| *t == want);
        self.mode = target;
        preview
    }

    fn toggle_hit(&self, col: u16, row: u16) -> Option<AppearanceMode> {
        let r = self.toggle_rect?;
        let inside = col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height;
        inside.then(|| self.mode.opposite())
    }

    /// Close + restore the theme and mode active when the picker opened.
    fn revert_outcome(&self) -> ModalOutcome {
        let original_theme = self.original_theme.clone();
        let original_mode = self.original_mode;
        ModalOutcome::CloseAnd(Box::new(move |app| {
            let changed =
                app.config.theme != original_theme || app.config.appearance != original_mode;
            app.config.theme = original_theme;
            app.config.appearance = original_mode;
            if changed {
                app.apply_active_theme();
            }
        }))
    }

    /// Close + commit the chosen theme + mode, persisting if anything changed.
    fn select_outcome(&self, name: String) -> ModalOutcome {
        let original_theme = self.original_theme.clone();
        let original_mode = self.original_mode;
        let selected_mode = self.mode;
        ModalOutcome::CloseAnd(Box::new(move |app| {
            let theme_changed = app.config.theme != name;
            let mode_changed = app.config.appearance != selected_mode;
            // Commit through `set_theme` unconditionally — even when the
            // name is unchanged.  On an indexed-color session the user
            // may be confirming the substituted theme itself, and that
            // confirmation is exactly what clears the downgrade stash so
            // the choice reaches disk.
            let downgrade_cleared = app.config.theme_downgraded_from.is_some();
            app.config.set_theme(name.clone());
            app.config.appearance = selected_mode;
            if theme_changed || mode_changed {
                app.apply_active_theme();
            }
            // Save when anything the user can see changed, or when the
            // stash was just cleared — otherwise the cleared downgrade
            // would only reach disk on some later, unrelated save.
            if name != original_theme || selected_mode != original_mode || downgrade_cleared {
                app.save_config_with_flash("failed to persist theme change");
            }
        }))
    }
}

impl Modal for ThemePickerModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let layout = render_theme_picker(
            &mut self.list,
            area,
            frame.buffer_mut(),
            ctx.theme,
            ctx.cursor_visible,
            self.mode,
            &self.current,
        );
        self.esc_button_rect = layout.esc_rect;
        self.toggle_rect = layout.toggle_rect;
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        // Arrow keys flip the appearance toggle; Tab is deliberately inert so
        // users don't generalize "Tab toggles switches" to other modals.
        match key.code {
            KeyCode::Left | KeyCode::Right => {
                let preview = self.switch_mode(self.mode.opposite());
                if app.config.theme != preview {
                    app.config.theme = preview;
                }
                app.config.appearance = self.mode;
                app.apply_active_theme();
                return ModalOutcome::Continue;
            }
            KeyCode::Tab | KeyCode::BackTab => return ModalOutcome::Continue,
            _ => {}
        }

        match self.list.handle_key(&key) {
            ListEvent::Continue => ModalOutcome::Continue,
            ListEvent::FocusChanged(i) => {
                let name = self.list.items()[i].clone();
                if app.config.theme != name {
                    app.config.theme = name;
                    app.apply_active_theme();
                }
                ModalOutcome::Continue
            }
            ListEvent::Cancelled => self.revert_outcome(),
            ListEvent::Submitted(i) => {
                let name = self.list.items()[i].clone();
                self.select_outcome(name)
            }
        }
    }

    fn handle_paste(&mut self, text: &str) -> ModalOutcome {
        if let ListEvent::FocusChanged(i) = self.list.paste(text) {
            let name = self.list.items()[i].clone();
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
        self.list.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        if let Some(target) = self.toggle_hit(col, row) {
            let preview = self.switch_mode(target);
            return ModalOutcome::ContinueAnd(Box::new(move |app| {
                if app.config.theme != preview {
                    app.config.theme = preview;
                }
                app.config.appearance = target;
                app.apply_active_theme();
            }));
        }
        if super::types::esc_rect_hit(self.esc_button_rect, col, row) {
            return self.revert_outcome();
        }
        match self.list.handle_click(col, row) {
            ListEvent::Submitted(i) => {
                let name = self.list.items()[i].clone();
                self.select_outcome(name)
            }
            _ => ModalOutcome::Continue,
        }
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
    use crossterm::event::KeyModifiers;

    #[test]
    fn open_theme_picker_seeds_state() {
        let mut app = make_app();
        app.open_theme_picker();
        assert!(app.modal_stack.contains::<ThemePickerModal>());
    }

    #[test]
    fn switch_mode_round_trip_restores_original_theme() {
        let dark_list = vec!["Edamame".to_owned(), "256 Dark".to_owned()];
        let mut modal = ThemePickerModal::new(dark_list, "Edamame".into(), AppearanceMode::Dark);
        let _ = modal.switch_mode(AppearanceMode::Light);
        let back = modal.switch_mode(AppearanceMode::Dark);
        assert_eq!(back, "Edamame");
    }

    #[test]
    fn switch_mode_remembers_intermediate_previews() {
        let dark_list = vec!["Edamame".to_owned(), "256 Dark".to_owned()];
        let mut modal = ThemePickerModal::new(dark_list, "Edamame".into(), AppearanceMode::Dark);
        modal
            .list
            .handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(modal.current_theme(), "256 Dark");
        let _ = modal.switch_mode(AppearanceMode::Light);
        let back = modal.switch_mode(AppearanceMode::Dark);
        assert_eq!(back, "256 Dark");
    }
}
