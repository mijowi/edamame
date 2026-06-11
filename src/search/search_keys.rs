//! Single source of truth for the search-flow key bindings.
//!
//! Like the diff-review keys (`input::mode_handler::diff_keys`), the
//! search-flow keys are *not* routed through the runtime [`KeyMap`]:
//! the flow bindings must win over the global keymap (e.g. `Tab` →
//! `InsertTab`).  Behavior ([`search_action_for`], consumed by
//! `mode_handler::default` while `EditorState::search` is `Some`) and
//! display ([`search_hint`], consumed by the hint bar and keybinds
//! overlay) derive from the one `SEARCH_BINDINGS` table here, so the
//! advertised chord can never disagree with the key that fires.
//!
//! [`KeyMap`]: crate::config::KeyMap

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::Action;

/// How a binding's modifiers are matched against an incoming event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModMatch {
    /// The event modifiers must equal this set exactly.
    Exact(KeyModifiers),
    /// Match regardless of modifiers — `Esc` to exit, and
    /// `Shift-Tab`/`BackTab`, which some terminals report with and some
    /// without the `SHIFT` flag.
    Any,
}

/// One search-flow binding: the key, the action it maps to, and the
/// glyph shown for it in the UI.
struct SearchBinding {
    key: KeyCode,
    mods: ModMatch,
    action: Action,
    /// Display glyph.  An empty string marks an *alias* row — a second
    /// key for the same action — which the behavior path honors but the
    /// UI must not list a second time.
    glyph: &'static str,
}

/// The search-flow key map.  Ordered for first-match-wins in
/// [`search_action_for`]; the canonical (non-alias) glyph for each
/// action is the first row whose `glyph` is non-empty.
const SEARCH_BINDINGS: &[SearchBinding] = &[
    // `Esc` exits the flow regardless of modifiers.
    SearchBinding {
        key: KeyCode::Esc,
        mods: ModMatch::Any,
        action: Action::SearchExit,
        glyph: "Esc",
    },
    SearchBinding {
        key: KeyCode::Tab,
        mods: ModMatch::Exact(KeyModifiers::NONE),
        action: Action::SearchNext,
        glyph: "Tab",
    },
    // `BackTab` carries the `SHIFT` flag in some terminals and not in
    // others, so it matches any modifiers.
    SearchBinding {
        key: KeyCode::BackTab,
        mods: ModMatch::Any,
        action: Action::SearchPrev,
        glyph: "⇧Tab",
    },
    // Alias: terminals that report Shift-Tab as `Tab + SHIFT`.
    SearchBinding {
        key: KeyCode::Tab,
        mods: ModMatch::Exact(KeyModifiers::SHIFT),
        action: Action::SearchPrev,
        glyph: "",
    },
    SearchBinding {
        key: KeyCode::Char('r'),
        mods: ModMatch::Exact(KeyModifiers::NONE),
        action: Action::SearchReplace,
        glyph: "r",
    },
    SearchBinding {
        key: KeyCode::Char('a'),
        mods: ModMatch::Exact(KeyModifiers::NONE),
        action: Action::SearchReplaceAll,
        glyph: "a",
    },
];

impl ModMatch {
    fn matches(self, event_mods: KeyModifiers) -> bool {
        match self {
            ModMatch::Exact(m) => event_mods == m,
            ModMatch::Any => true,
        }
    }
}

/// Resolve a key event to its search-flow [`Action`], or `None` when no
/// flow binding matches (the caller then falls through to the global
/// keymap).  First match in `SEARCH_BINDINGS` wins.
pub fn search_action_for(event: &KeyEvent) -> Option<Action> {
    SEARCH_BINDINGS
        .iter()
        .find(|b| b.key == event.code && b.mods.matches(event.modifiers))
        .map(|b| b.action.clone())
}

/// The display glyph for `action`'s search-flow binding — the canonical
/// (non-alias) key.  Returns `""` for actions with no flow binding so
/// callers can interpolate it unconditionally.
pub fn search_hint(action: &Action) -> &'static str {
    SEARCH_BINDINGS
        .iter()
        .find(|b| &b.action == action && !b.glyph.is_empty())
        .map(|b| b.glyph)
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn replaying_each_binding_yields_its_action() {
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        assert_eq!(
            search_action_for(&ev(KeyCode::Esc, none)),
            Some(Action::SearchExit)
        );
        assert_eq!(
            search_action_for(&ev(KeyCode::Esc, KeyModifiers::CONTROL)),
            Some(Action::SearchExit)
        );
        assert_eq!(
            search_action_for(&ev(KeyCode::Tab, none)),
            Some(Action::SearchNext)
        );
        assert_eq!(
            search_action_for(&ev(KeyCode::BackTab, shift)),
            Some(Action::SearchPrev)
        );
        assert_eq!(
            search_action_for(&ev(KeyCode::Tab, shift)),
            Some(Action::SearchPrev)
        );
        assert_eq!(
            search_action_for(&ev(KeyCode::Char('r'), none)),
            Some(Action::SearchReplace)
        );
        assert_eq!(
            search_action_for(&ev(KeyCode::Char('a'), none)),
            Some(Action::SearchReplaceAll)
        );
    }

    #[test]
    fn unbound_keys_fall_through_to_the_global_keymap() {
        assert_eq!(
            search_action_for(&ev(KeyCode::Char('x'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            search_action_for(&ev(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn hints_resolve_to_canonical_glyphs() {
        assert_eq!(search_hint(&Action::SearchNext), "Tab");
        // Prev's glyph is the BackTab row, not the empty Tab+Shift alias.
        assert_eq!(search_hint(&Action::SearchPrev), "⇧Tab");
        assert_eq!(search_hint(&Action::SearchReplace), "r");
        assert_eq!(search_hint(&Action::SearchReplaceAll), "a");
        assert_eq!(search_hint(&Action::SearchExit), "Esc");
        assert_eq!(search_hint(&Action::OpenSearch), "");
    }
}
