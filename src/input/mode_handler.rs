use crossterm::event::KeyEvent;

use crate::config::Action;
use crate::editor::EditorState;

pub mod default;

/// A keybinding handler for an input mode. Implementations can inspect
/// the current `EditorState` (including mode) to return context-sensitive
/// `Action`s.
///
/// The default (non-modal) implementation lives in `default.rs`. A Vim
/// implementation is a deferred feature.
pub trait ModeHandler {
    fn handle(&mut self, event: KeyEvent, state: &EditorState) -> Option<Action>;
}
