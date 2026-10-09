use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{Action, KeyMap};
use crate::editor::{EditorState, Mode};

use super::diff_keys;
use super::ModeHandler;

/// The default (non-modal) keybinding handler: the `KeyMap` first, then a printable character as
/// `InsertChar` (in any mode — `edit_ops` handles the Preview transition), then `None`.
pub struct DefaultHandler<'k> {
    keymap: &'k KeyMap,
}

impl<'k> DefaultHandler<'k> {
    pub fn new(keymap: &'k KeyMap) -> Self {
        Self { keymap }
    }
}

impl<'k> ModeHandler for DefaultHandler<'k> {
    fn handle(&mut self, event: KeyEvent, state: &EditorState) -> Option<Action> {
        // Diff Review's bare keys win over the global keymap, which would otherwise turn Tab
        // into `InsertTab`.
        if state.mode == Mode::Diff {
            if let Some(action) = diff_review_handle(&event) {
                return Some(action);
            }
        }

        // The search flow claims its keys the same way.  A *capturing* replace flow takes the
        // full set; a navigate flow takes only `Tab`/`Shift+Tab`/`Esc`, so `r`/`a` and every
        // printable key fall through to normal editing with the highlights left in place.
        if let Some(search) = state.search.as_ref() {
            if let Some(action) = crate::search::search_action_for(&event) {
                let capturing = search.is_replace_flow();
                if capturing
                    || matches!(
                        action,
                        Action::SearchNext | Action::SearchPrev | Action::SearchExit
                    )
                {
                    return Some(action);
                }
            }
        }

        if let Some(action) = self.keymap.action_for(&event) {
            // A Ctrl-* chord must not implicitly leave Preview — reading and copying there should
            // not be interrupted by `Ctrl+Z` / `Ctrl+D` / `Ctrl+Left` dropping into edit mode.
            if state.mode == Mode::Preview
                && event.modifiers.contains(KeyModifiers::CONTROL)
                && !preview_safe_action(action)
            {
                return None;
            }
            return Some(action.clone());
        }

        // The `ctrl+backspace` binding matches only `Backspace` with exactly `CONTROL`; these are
        // the other encodings terminals use (see [`is_ctrl_backspace`]).
        if is_ctrl_backspace(&event) {
            if state.mode == Mode::Preview {
                return None;
            }
            return Some(Action::DeleteWordBack);
        }

        if let KeyCode::Char(ch) = event.code {
            let only_shift =
                event.modifiers == KeyModifiers::NONE || event.modifiers == KeyModifiers::SHIFT;
            if only_shift {
                return Some(Action::InsertChar(ch));
            }
        }

        None
    }
}

/// Actions a Ctrl-* chord may run from Preview.  Everything else is suppressed so browsing is
/// never interrupted by an accidental drop into edit mode.
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
            // Overlay openers pop a modal that absorbs later input, so they change nothing.
            | Action::ShowCommandPalette
            | Action::ShowMarkdownCheatSheet
            | Action::BrowseTips
            // The manual replaces the document rather than popping a modal, but starts no edit
            // and the dirty guard protects anything unsaved.
            | Action::OpenDoc(_)
            | Action::ShowAbout
            | Action::CheckForUpdates
            | Action::OpenSettings
            | Action::OpenWelcome
            | Action::OpenKeybinds
            | Action::SwitchTheme
            | Action::CreateCustomTheme
            | Action::ExportHtml
            | Action::OpenConfigFolder
            // Palette-only by default but bindable; neither needs an editing mode.
            | Action::OpenInExternalEditor
            | Action::ToggleTableButtons
            // The persisted setting toggles are likewise config-only flips.
            | Action::ToggleBigH1
            | Action::ToggleLineNumbers
            | Action::ToggleBlinkCursor
            | Action::ToggleOutline
            | Action::FocusOutline
            | Action::ToggleAutosave
            | Action::ToggleVisualLineNav
            | Action::ToggleVimMode
            | Action::ToggleLimitWidth
            | Action::ToggleDiffOnChange
            // The remaining three open a modal that touches the buffer only on submit; the
            // section picker's cursor motion is benign in Preview, where no cursor is drawn.
            | Action::InsertTable
            | Action::SaveAs
            | Action::GoToSection
            | Action::OpenSearch
    )
}

/// Map a bare key to its diff-Review action; `None` falls through to the global keymap.
///
/// Hard-coded rather than a second `KeyMap` because these bindings must beat the global `Tab` →
/// `InsertTab`.  The table itself is `diff_keys::DIFF_REVIEW_BINDINGS`, shared with the hint bar,
/// keybinds overlay, decision divider and diff-intro modal so glyphs can't drift from behavior.
fn diff_review_handle(event: &KeyEvent) -> Option<Action> {
    diff_keys::diff_action_for(event)
}

/// Does this event represent Ctrl+Backspace in some terminal's encoding?
///
/// - kitty keyboard protocol: `Backspace` + CONTROL
/// - xterm / macOS Terminal / older Alacritty: raw `\x08` (Ctrl+H in ASCII)
///   with or without the CONTROL modifier (crossterm may or may not set it)
/// - urxvt, modern Alacritty without kitty protocol: `\x7f` + CONTROL
/// - Some terminals translate Ctrl+Backspace to Ctrl+H directly: `h`/`H` + CONTROL
pub(crate) fn is_ctrl_backspace(event: &KeyEvent) -> bool {
    let has_ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    match event.code {
        KeyCode::Backspace if has_ctrl => true,
        KeyCode::Char('\x08') => true,
        KeyCode::Char('\x7f') if has_ctrl => true,
        KeyCode::Char('h') | KeyCode::Char('H') if has_ctrl => true,
        _ => false,
    }
}

/// Does this event represent Ctrl+Delete?  Unlike Ctrl+Backspace it has no ASCII control-code
/// encoding, so every terminal reports `Delete` + CONTROL.
pub(crate) fn is_ctrl_delete(event: &KeyEvent) -> bool {
    matches!(event.code, KeyCode::Delete) && event.modifiers.contains(KeyModifiers::CONTROL)
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

    // ── Read-only documentation pages ─────────────────────────────

    fn readonly_state(mode: Mode) -> EditorState {
        let mut s = state(mode);
        s.readonly = true;
        s
    }

    /// A read-only page has **no bespoke key table**.  A vim-flavored scroll table (`j`/`k`,
    /// `Ctrl-D`/`Ctrl-U`, `Space`) briefly lived here; this guards against its return.
    #[test]
    fn a_read_only_page_resolves_keys_through_the_ordinary_keymap() {
        let km = keymap();
        let mut h = DefaultHandler::new(&km);
        let readonly = readonly_state(Mode::Preview);
        let editable = state(Mode::Preview);
        for (code, mods) in [
            (KeyCode::Char('j'), KeyModifiers::NONE),
            (KeyCode::Char('k'), KeyModifiers::NONE),
            (KeyCode::Char('g'), KeyModifiers::NONE),
            (KeyCode::Char(' '), KeyModifiers::NONE),
            (KeyCode::Char('d'), KeyModifiers::CONTROL),
            (KeyCode::Char('u'), KeyModifiers::CONTROL),
            (KeyCode::Home, KeyModifiers::NONE),
            (KeyCode::End, KeyModifiers::NONE),
        ] {
            let ev = KeyEvent::new(code, mods);
            assert_eq!(
                h.handle(ev, &readonly),
                h.handle(ev, &editable),
                "{code:?} must resolve the same on a read-only page"
            );
        }
    }

    /// "Press any key to edit" is unchanged; read-only merely refuses downstream.
    #[test]
    fn an_ordinary_preview_still_edits_on_a_letter_key() {
        let km = keymap();
        let mut h = DefaultHandler::new(&km);
        let st = state(Mode::Preview);
        assert_eq!(
            h.handle(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), &st),
            Some(Action::InsertChar('j'))
        );
    }

    /// Falling through is what keeps every bound chord working.
    #[test]
    fn unbound_keys_still_reach_the_keymap_on_a_read_only_page() {
        let km = keymap();
        let mut h = DefaultHandler::new(&km);
        let st = readonly_state(Mode::Preview);
        assert_eq!(
            h.handle(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                &st
            ),
            Some(Action::Quit)
        );
    }

    /// The handler knows nothing about `readonly`: a read-only document rests in Preview, where
    /// `enter_edit_if_preview` refuses the transition silently, so the synthesized action is
    /// produced and then goes nowhere.  One guard instead of two.
    #[test]
    fn a_read_only_page_no_longer_suppresses_the_keymap_bypassing_arms() {
        let km = keymap();
        let mut h = DefaultHandler::new(&km);
        let st = readonly_state(Mode::Preview);
        assert_eq!(
            h.handle(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE), &st),
            Some(Action::InsertChar('z'))
        );
        // A `Ctrl-Backspace` encoding the keymap misses: dropped by the Preview guard, not by
        // `readonly`.
        let ev = KeyEvent::new(
            KeyCode::Backspace,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(h.handle(ev, &st), None);
        assert_eq!(
            h.handle(ev, &readonly_state(Mode::Rendered)),
            Some(Action::DeleteWordBack)
        );
    }

    #[test]
    fn bound_chords_still_resolve_on_a_read_only_page() {
        // The App's `readonly_safe_action` decides which of them may actually run.
        let km = keymap();
        let mut h = DefaultHandler::new(&km);
        let st = readonly_state(Mode::Rendered);
        let ev = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert_eq!(h.handle(ev, &st), Some(Action::Quit));
    }

    #[test]
    fn an_ordinary_document_still_types() {
        let km = keymap();
        let mut h = DefaultHandler::new(&km);
        let st = state(Mode::Rendered);
        assert_eq!(
            h.handle(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE), &st),
            Some(Action::InsertChar('a'))
        );
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
        // Ctrl+A is bound to MoveLineStart.
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
        // Undo / DeleteLine / Cut / Paste would all otherwise enter edit mode.
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
