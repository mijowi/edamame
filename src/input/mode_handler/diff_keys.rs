//! Single source of truth for the Diff Review sub-mode key bindings.
//!
//! Diff-review keys are *not* routed through the runtime [`KeyMap`]:
//! the review bindings must win over the global keymap (e.g. `Tab` →
//! `InsertTab`), and the layered keymap a future Edit sub-mode would
//! use does not exist yet.  Rather than hard-code the same `y` / `n` /
//! `Tab` … mapping in
//! the input handler *and* re-spell every glyph again in the hint bar,
//! the keybinds overlay, the decision divider, and the diff-intro
//! modal, all of those derive from the one `DIFF_REVIEW_BINDINGS`
//! table here:
//!
//! - [`diff_action_for`] turns a key event into its [`Action`] (the
//!   behavior — consumed by `mode_handler::default`).
//! - [`diff_hint`] returns the display glyph for an [`Action`] (the
//!   UI — consumed by the hint bar, overlay, divider, and modal).
//!
//! Behavior and display can therefore never drift: there is exactly
//! one place that says "accept is `y`".
//!
//! [`KeyMap`]: crate::config::KeyMap

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::Action;

/// How a binding's modifiers are matched against an incoming event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModMatch {
    /// The event modifiers must equal this set exactly.
    Exact(KeyModifiers),
    /// Match regardless of modifiers — used for keys whose meaning is
    /// modifier-independent (`Esc` to exit, `Shift-Tab`/`BackTab` which
    /// some terminals report with and some without the `SHIFT` flag).
    Any,
}

/// One diff-review binding: the key that triggers it, the action it
/// maps to, and the glyph shown for it in the UI.
struct DiffBinding {
    key: KeyCode,
    mods: ModMatch,
    action: Action,
    /// Display glyph (`"y"`, `"Tab"`, `"⇧Tab"`, `"⌫"`, `"Esc"`).  An
    /// empty string marks an *alias* row — a second key that triggers
    /// the same action (`Shift-Tab` → Prev, `Enter` → Edit) — which the
    /// behavior path honors but the UI must not list a second time.
    glyph: &'static str,
}

/// The diff-review key map.  Ordered for first-match-wins in
/// [`diff_action_for`]; the canonical (non-alias) glyph for each action
/// is the first row whose `glyph` is non-empty.
const DIFF_REVIEW_BINDINGS: &[DiffBinding] = &[
    // `Esc` exits regardless of modifiers (matches the legacy handler).
    DiffBinding {
        key: KeyCode::Esc,
        mods: ModMatch::Any,
        action: Action::DiffExit,
        glyph: "Esc",
    },
    DiffBinding {
        key: KeyCode::Tab,
        mods: ModMatch::Exact(KeyModifiers::NONE),
        action: Action::DiffNext,
        glyph: "Tab",
    },
    // `BackTab` carries the `SHIFT` flag in some terminals and not in
    // others, so it matches any modifiers.
    DiffBinding {
        key: KeyCode::BackTab,
        mods: ModMatch::Any,
        action: Action::DiffPrev,
        glyph: "⇧Tab",
    },
    // Alias: terminals that report Shift-Tab as `Tab + SHIFT`.
    DiffBinding {
        key: KeyCode::Tab,
        mods: ModMatch::Exact(KeyModifiers::SHIFT),
        action: Action::DiffPrev,
        glyph: "",
    },
    DiffBinding {
        key: KeyCode::Char('y'),
        mods: ModMatch::Exact(KeyModifiers::NONE),
        action: Action::DiffAcceptHunk,
        glyph: "y",
    },
    DiffBinding {
        key: KeyCode::Char('n'),
        mods: ModMatch::Exact(KeyModifiers::NONE),
        action: Action::DiffRejectHunk,
        glyph: "n",
    },
    DiffBinding {
        key: KeyCode::Char('Y'),
        mods: ModMatch::Exact(KeyModifiers::SHIFT),
        action: Action::DiffAcceptAll,
        glyph: "Y",
    },
    DiffBinding {
        key: KeyCode::Char('N'),
        mods: ModMatch::Exact(KeyModifiers::SHIFT),
        action: Action::DiffRejectAll,
        glyph: "N",
    },
    DiffBinding {
        key: KeyCode::Backspace,
        mods: ModMatch::Exact(KeyModifiers::NONE),
        action: Action::DiffResetHunk,
        glyph: "⌫",
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

/// Resolve a key event to its diff-review [`Action`], or `None` when no
/// review binding matches (the caller then falls through to the global
/// keymap).  First match in `DIFF_REVIEW_BINDINGS` wins.
pub fn diff_action_for(event: &KeyEvent) -> Option<Action> {
    DIFF_REVIEW_BINDINGS
        .iter()
        .find(|b| b.key == event.code && b.mods.matches(event.modifiers))
        .map(|b| b.action.clone())
}

/// The display glyph for `action`'s diff-review binding — the canonical
/// (non-alias) key.  Returns `""` for actions with no review binding so
/// callers can interpolate it unconditionally.
pub fn diff_hint(action: &Action) -> &'static str {
    DIFF_REVIEW_BINDINGS
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

    /// Every binding (including aliases) must resolve to its action —
    /// this locks the table-driven matcher against the legacy
    /// hand-written `match` it replaced.
    #[test]
    fn replaying_each_binding_yields_its_action() {
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        assert_eq!(
            diff_action_for(&ev(KeyCode::Esc, none)),
            Some(Action::DiffExit)
        );
        // Esc exits regardless of modifiers.
        assert_eq!(
            diff_action_for(&ev(KeyCode::Esc, KeyModifiers::CONTROL)),
            Some(Action::DiffExit)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Tab, none)),
            Some(Action::DiffNext)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::BackTab, shift)),
            Some(Action::DiffPrev)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Tab, shift)),
            Some(Action::DiffPrev)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Char('y'), none)),
            Some(Action::DiffAcceptHunk)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Char('n'), none)),
            Some(Action::DiffRejectHunk)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Char('Y'), shift)),
            Some(Action::DiffAcceptAll)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Char('N'), shift)),
            Some(Action::DiffRejectAll)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Backspace, none)),
            Some(Action::DiffResetHunk)
        );
    }

    /// `i` and `Enter` used to enter an unimplemented in-diff Edit
    /// sub-mode that only flashed "coming soon".  They are unbound now,
    /// so they fall through to the global keymap like any other key —
    /// `docs/editing.md` documents the diff keys without them.  Binding
    /// them again means implementing the feature first.
    #[test]
    fn edit_sub_mode_keys_are_unbound() {
        let none = KeyModifiers::NONE;
        assert_eq!(diff_action_for(&ev(KeyCode::Char('i'), none)), None);
        assert_eq!(diff_action_for(&ev(KeyCode::Enter, none)), None);
    }

    /// Unbound keys fall through (so the global keymap gets a look-in).
    #[test]
    fn unbound_key_returns_none() {
        assert_eq!(
            diff_action_for(&ev(KeyCode::Char('x'), KeyModifiers::NONE)),
            None
        );
        // Plain `Y`/`N` without SHIFT don't bulk-decide.
        assert_eq!(
            diff_action_for(&ev(KeyCode::Char('Y'), KeyModifiers::NONE)),
            None
        );
    }

    /// The hint glyph resolves to the canonical key, skipping aliases.
    #[test]
    fn hints_resolve_to_canonical_glyph() {
        assert_eq!(diff_hint(&Action::DiffAcceptHunk), "y");
        assert_eq!(diff_hint(&Action::DiffRejectHunk), "n");
        assert_eq!(diff_hint(&Action::DiffAcceptAll), "Y");
        assert_eq!(diff_hint(&Action::DiffRejectAll), "N");
        assert_eq!(diff_hint(&Action::DiffNext), "Tab");
        // Prev's glyph is the BackTab row, not the empty Tab+Shift alias.
        assert_eq!(diff_hint(&Action::DiffPrev), "⇧Tab");
        assert_eq!(diff_hint(&Action::DiffResetHunk), "⌫");
        assert_eq!(diff_hint(&Action::DiffExit), "Esc");
        // An action with no review binding yields an empty glyph.
        assert_eq!(diff_hint(&Action::Quit), "");
    }
}
