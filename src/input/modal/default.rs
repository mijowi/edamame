use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{Action, KeyMap};
use crate::editor::{EditorState, Mode};

use super::ModalHandler;

/// The default (non-modal) keybinding handler.
///
/// Priority:
/// 1. If the key is in the `KeyMap`, return the bound action.
/// 2. If the key is a printable character (no non-Shift modifier), return
///    `InsertChar` when in Rendered or Raw mode (or when transitioning from
///    Preview — the editor state machine handles the mode switch).
/// 3. Otherwise return `None`.
pub struct DefaultHandler<'k> {
    keymap: &'k KeyMap,
}

impl<'k> DefaultHandler<'k> {
    pub fn new(keymap: &'k KeyMap) -> Self {
        Self { keymap }
    }
}

impl<'k> ModalHandler for DefaultHandler<'k> {
    fn handle(&mut self, event: KeyEvent, state: &EditorState) -> Option<Action> {
        // 1. Check the keymap first (explicit bindings take priority).
        if let Some(action) = self.keymap.action_for(&event) {
            // Preview-mode guard: Ctrl-* chords must not cause an implicit
            // transition into edit mode — users want to read and copy in
            // Preview without `Ctrl+Z`, `Ctrl+D`, `Ctrl+Left`, etc.
            // exiting it on them.  Drop any Ctrl-bound action that isn't on
            // the Preview-safe allow-list.
            if state.mode == Mode::Preview
                && event.modifiers.contains(KeyModifiers::CONTROL)
                && !preview_safe_action(action)
            {
                return None;
            }
            return Some(action.clone());
        }

        // 2. Ctrl+Backspace fallbacks. Different terminals encode this chord
        //    wildly differently; none of the encodings below is covered by the
        //    `ctrl+backspace` binding (which matches `KeyCode::Backspace` with
        //    exactly `CONTROL`).  We also match on `modifiers.contains(CONTROL)`
        //    rather than strict equality so combinations like Ctrl+Shift+BS also
        //    delete a word back.
        if is_ctrl_backspace(&event) {
            // Same Preview guard — ctrl+backspace is destructive, no-op in
            // Preview.
            if state.mode == Mode::Preview {
                return None;
            }
            return Some(Action::DeleteWordBack);
        }

        // 3. Printable character → InsertChar (in any mode; edit_ops handles
        //    the preview → rendered transition).
        if let KeyCode::Char(ch) = event.code {
            let only_shift =
                event.modifiers == KeyModifiers::NONE || event.modifiers == KeyModifiers::SHIFT;
            if only_shift {
                return Some(Action::InsertChar(ch));
            }
        }

        None
    }

    fn name(&self) -> &'static str {
        "default"
    }
}

/// Actions that are allowed to run in Preview mode when triggered by a
/// Ctrl-* key chord.  Everything else gets suppressed by the handler so
/// Preview-mode users can safely read and copy without their clipboard /
/// quit / selection chords accidentally dropping them into edit mode.
fn preview_safe_action(action: &Action) -> bool {
    matches!(
        action,
        Action::Quit
            | Action::Copy
            | Action::SelectAll
            | Action::Save
            | Action::Open
            | Action::ToggleRawMode
            | Action::EnterEditMode
            | Action::ExitToPreview
            | Action::ScrollUp
            | Action::ScrollDown
            | Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::ScrollToTop
            | Action::ScrollToBottom
            // Phase 10 — overlay-opening actions are read-only:
            // they pop a modal that absorbs subsequent input.
            // Suppressing them in Preview would leave Ctrl-P unable
            // to launch the command palette while the user is just
            // browsing.
            | Action::ShowCommandPalette
            | Action::ShowMarkdownCheatSheet
            | Action::ShowCheatSheet
            | Action::OpenSettings
            | Action::OpenKeybinds
            | Action::OpenConfigFolder
            // Both are palette-only by default but a user may bind a
            // chord — allow them to fire from Preview without first
            // entering edit mode.  `OpenInExternalEditor` saves and
            // suspends the TUI; `ToggleTableButtons` is a tiny
            // in-memory flip — neither needs the buffer to be in an
            // editing mode.
            | Action::OpenInExternalEditor
            | Action::ToggleTableButtons
    )
}

/// Does this event represent Ctrl+Backspace in some terminal's encoding?
///
/// - kitty keyboard protocol: `Backspace` + CONTROL
/// - xterm / macOS Terminal / older Alacritty: raw `\x08` (Ctrl+H in ASCII)
///   with or without the CONTROL modifier (crossterm may or may not set it)
/// - urxvt, modern Alacritty without kitty protocol: `\x7f` + CONTROL
/// - Some terminals translate Ctrl+Backspace to Ctrl+H directly: `h`/`H` + CONTROL
fn is_ctrl_backspace(event: &KeyEvent) -> bool {
    let has_ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    match event.code {
        KeyCode::Backspace if has_ctrl => true,
        KeyCode::Char('\x08') => true,
        KeyCode::Char('\x7f') if has_ctrl => true,
        KeyCode::Char('h') | KeyCode::Char('H') if has_ctrl => true,
        _ => false,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{KeyBindingOverrides, KeyMap};
    use crate::document::Buffer;
    use crate::editor::{EditorState, Mode};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn keymap() -> KeyMap {
        KeyMap::build(&KeyBindingOverrides::default()).unwrap()
    }

    fn state(mode: Mode) -> EditorState {
        let theme = Box::leak(Box::new(crate::config::Theme::default()));
        let mut s = EditorState::new(Buffer::new(), theme);
        s.mode = mode;
        s
    }

    #[test]
    fn ctrl_q_returns_quit() {
        let km = keymap();
        let mut handler = DefaultHandler::new(&km);
        let event = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert_eq!(
            handler.handle(event, &state(Mode::Preview)),
            Some(Action::Quit)
        );
    }

    #[test]
    fn printable_char_returns_insert() {
        let km = keymap();
        let mut handler = DefaultHandler::new(&km);
        let event = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(
            handler.handle(event, &state(Mode::Rendered)),
            Some(Action::InsertChar('a'))
        );
    }

    #[test]
    fn ctrl_char_not_insert() {
        let km = keymap();
        let mut handler = DefaultHandler::new(&km);
        // Ctrl+A is bound to MoveLineStart, not InsertChar.
        let event = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        let action = handler.handle(event, &state(Mode::Rendered));
        assert_ne!(action, Some(Action::InsertChar('a')));
    }

    #[test]
    fn ctrl_c_returns_copy() {
        let km = keymap();
        let mut handler = DefaultHandler::new(&km);
        let event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            handler.handle(event, &state(Mode::Rendered)),
            Some(Action::Copy)
        );
    }

    #[test]
    fn preview_ctrl_c_and_ctrl_a_still_fire() {
        let km = keymap();
        let mut handler = DefaultHandler::new(&km);
        // Clipboard and select-all are allowed in Preview.
        assert_eq!(
            handler.handle(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &state(Mode::Preview)
            ),
            Some(Action::Copy)
        );
        assert_eq!(
            handler.handle(
                KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
                &state(Mode::Preview)
            ),
            Some(Action::SelectAll)
        );
        // Quit must always be allowed.
        assert_eq!(
            handler.handle(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                &state(Mode::Preview)
            ),
            Some(Action::Quit)
        );
    }

    #[test]
    fn preview_suppresses_non_safelisted_ctrl_chords() {
        let km = keymap();
        let mut handler = DefaultHandler::new(&km);
        // Ctrl+Z (Undo) and Ctrl+D (DeleteLine) would normally enter edit
        // mode; in Preview they must drop out so the user stays in read mode.
        for ch in ['z', 'd', 'x', 'v'] {
            let event = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
            assert_eq!(
                handler.handle(event, &state(Mode::Preview)),
                None,
                "ctrl+{ch} should be suppressed in Preview mode",
            );
        }
    }
}
