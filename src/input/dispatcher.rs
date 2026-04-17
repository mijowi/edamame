use crossterm::event::{Event, KeyEventKind};

use crate::config::Action;
use crate::editor::EditorState;

use super::modal::ModalHandler;

/// Translates raw crossterm `Event`s into `Action`s by delegating to the
/// active `ModalHandler`.
///
/// Only `KeyPress` events reach the handler; `KeyRelease` and `KeyRepeat`
/// events are filtered out at this layer.
pub struct InputDispatcher<H: ModalHandler> {
    handler: H,
}

impl<H: ModalHandler> InputDispatcher<H> {
    pub fn new(handler: H) -> Self {
        Self { handler }
    }

    /// Translate a crossterm `Event` into an `Action`, if any.
    pub fn dispatch(&mut self, event: Event, state: &EditorState) -> Option<Action> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handler.handle(key, state),
            _ => None,
        }
    }
}
