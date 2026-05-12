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
use crate::ui::{ThemePickerResponse, ThemePickerState, ThemePickerView};

pub struct ThemePickerModal {
    state: ThemePickerState,
    /// Theme that was active when the picker opened.  Cached so a
    /// preview can be rolled back on Esc or replaced by Enter without
    /// re-reading `config.theme` (which is mutated mid-flight by each
    /// preview).
    original: String,
}

impl ThemePickerModal {
    pub fn new(themes: Vec<String>, current: String) -> Self {
        Self {
            state: ThemePickerState::open(themes, current.clone()),
            original: current,
        }
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
                // commit, Esc will revert to `self.original`.
                if app.config.theme != name {
                    app.config.theme = name;
                    app.apply_active_theme();
                }
                ModalOutcome::Continue
            }
            ThemePickerResponse::Cancelled => {
                let original = self.original.clone();
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    if app.config.theme != original {
                        app.config.theme = original;
                        app.apply_active_theme();
                    }
                }))
            }
            ThemePickerResponse::Selected(name) => {
                let original = self.original.clone();
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    if app.config.theme != name {
                        app.config.theme = name;
                        app.apply_active_theme();
                    }
                    if app.config.theme != original {
                        app.save_config_with_flash("failed to persist theme change");
                    }
                }))
            }
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_state.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        super::types::close_if_esc_clicked(self.state.esc_button_rect, col, row)
    }

    fn as_any(&self) -> &dyn Any {
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
}
