//! Search-and-replace modal.  Adapter wrapping
//! [`crate::ui::SearchModalState`].
//!
//! On confirm the modal closes and the App starts the search flow via
//! [`crate::app::App::enter_search_flow`]; a query with zero matches
//! never enters the flow (the App flashes and stays put).  Re-opening
//! over an active flow is handled by `App::open_search_modal`, which
//! pre-fills this modal with the flow's terms.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{SearchModalResponse, SearchModalState, SearchModalView};

pub struct SearchReplaceModal {
    state: SearchModalState,
}

impl SearchReplaceModal {
    pub fn new(query: String, replace: String) -> Self {
        Self {
            state: SearchModalState::new(query, replace),
        }
    }
}

impl Modal for SearchReplaceModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = SearchModalView {
            theme: ctx.theme,
            cursor_visible: ctx.cursor_visible,
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
            SearchModalResponse::Continue => ModalOutcome::Continue,
            SearchModalResponse::Cancelled => ModalOutcome::Close,
            SearchModalResponse::Search { query, replace } => {
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    app.enter_search_flow(query, replace);
                }))
            }
        }
    }

    fn handle_paste(&mut self, text: &str) -> ModalOutcome {
        self.state.paste(text);
        ModalOutcome::Continue
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
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
    //! Exercise the App-level search flow end to end: modal open via
    //! the action, term entry, flow entry, and the zero-match guard.

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::app_with_buffer;
    use crate::config::Action;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_str(app: &mut crate::app::App, s: &str) {
        for c in s.chars() {
            app.dispatch_modal_key(key(KeyCode::Char(c)), 24, 80);
        }
    }

    #[test]
    fn open_search_dispatch_opens_modal_and_enter_starts_flow() {
        let mut app = app_with_buffer("alpha beta alpha\n", 0);
        app.dispatch_action(Action::OpenSearch, 24, 80);
        assert!(app.modal_stack.contains::<SearchReplaceModal>());
        type_str(&mut app, "alpha");
        app.dispatch_modal_key(key(KeyCode::Enter), 24, 80);
        assert!(!app.modal_stack.contains::<SearchReplaceModal>());
        let search = app.editor.search.as_ref().expect("flow active");
        assert_eq!(search.matches.len(), 2);
        assert!(!search.is_replace_flow());
    }

    #[test]
    fn zero_matches_flashes_and_does_not_enter_flow() {
        let mut app = app_with_buffer("alpha beta\n", 0);
        app.dispatch_action(Action::OpenSearch, 24, 80);
        type_str(&mut app, "gamma");
        app.dispatch_modal_key(key(KeyCode::Enter), 24, 80);
        assert!(app.editor.search.is_none());
        assert!(app.transient.is_some(), "expected a no-matches flash");
    }

    #[test]
    fn replace_field_text_selects_replace_flow() {
        let mut app = app_with_buffer("alpha beta alpha\n", 0);
        app.dispatch_action(Action::OpenSearch, 24, 80);
        type_str(&mut app, "alpha");
        app.dispatch_modal_key(key(KeyCode::Tab), 24, 80);
        type_str(&mut app, "delta");
        app.dispatch_modal_key(key(KeyCode::Enter), 24, 80);
        let search = app.editor.search.as_ref().expect("flow active");
        assert!(search.is_replace_flow());
        assert_eq!(search.replace.as_deref(), Some("delta"));
    }

    #[test]
    fn esc_dismisses_without_entering_flow() {
        let mut app = app_with_buffer("alpha\n", 0);
        app.dispatch_action(Action::OpenSearch, 24, 80);
        type_str(&mut app, "alpha");
        app.dispatch_modal_key(key(KeyCode::Esc), 24, 80);
        assert!(!app.modal_stack.contains::<SearchReplaceModal>());
        assert!(app.editor.search.is_none());
    }

    #[test]
    fn open_search_during_active_flow_reopens_prefilled() {
        let mut app = app_with_buffer("alpha beta alpha\n", 0);
        app.enter_search_flow("alpha".to_owned(), Some("delta".to_owned()));
        assert!(app.editor.search.is_some());
        // Ctrl+F mid-flow routes through the search gate and re-opens
        // the modal with the current terms.
        app.dispatch_action(Action::OpenSearch, 24, 80);
        assert!(app.editor.search.is_none(), "flow torn down");
        let modal = app
            .modal_stack
            .find_first_mut::<SearchReplaceModal>()
            .expect("modal re-opened");
        assert_eq!(modal.state.query, "alpha");
        assert_eq!(modal.state.replace, "delta");
    }
}
