use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ─── Action ──────────────────────────────────────────────────────────────────

/// Every command the editor can execute. The full enum is defined upfront so
/// keybindings are stable across phases; unimplemented variants are simply
/// no-ops until their phase is implemented.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    // ── Navigation / scrolling ─────────────────────────────────────
    ScrollUp,
    ScrollDown,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToTop,
    ScrollToBottom,
    // ── Cursor movement (Phase 1) ──────────────────────────────────
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveWordLeft,
    MoveWordRight,
    MoveLineStart,
    MoveLineEnd,
    MoveDocStart,
    MoveDocEnd,
    // ── Editing (Phase 1) ──────────────────────────────────────────
    InsertChar(char),
    InsertTab,
    Newline,
    DeleteCharBack,
    DeleteCharForward,
    DeleteWordBack,
    DeleteWordForward,
    DeleteLine,
    // ── Clipboard (Phase 1) ────────────────────────────────────────
    Cut,
    Copy,
    Paste,
    // ── Selection (Phase 1) ────────────────────────────────────────
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    SelectAll,
    // ── History (Phase 1) ──────────────────────────────────────────
    Undo,
    Redo,
    // ── File operations ────────────────────────────────────────────
    Save,
    Open,
    // ── Mode transitions ───────────────────────────────────────────
    EnterEditMode,
    ExitToPreview,
    ToggleRawMode,
    // ── App control ────────────────────────────────────────────────
    Quit,
    // ── List editing (Phase 3) ─────────────────────────────────────
    ToggleCheckbox,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Action::ScrollUp => "ScrollUp",
            Action::ScrollDown => "ScrollDown",
            Action::ScrollPageUp => "ScrollPageUp",
            Action::ScrollPageDown => "ScrollPageDown",
            Action::ScrollToTop => "ScrollToTop",
            Action::ScrollToBottom => "ScrollToBottom",
            Action::MoveLeft => "MoveLeft",
            Action::MoveRight => "MoveRight",
            Action::MoveUp => "MoveUp",
            Action::MoveDown => "MoveDown",
            Action::MoveWordLeft => "MoveWordLeft",
            Action::MoveWordRight => "MoveWordRight",
            Action::MoveLineStart => "MoveLineStart",
            Action::MoveLineEnd => "MoveLineEnd",
            Action::MoveDocStart => "MoveDocStart",
            Action::MoveDocEnd => "MoveDocEnd",
            Action::InsertChar(_) => "InsertChar",
            Action::InsertTab => "InsertTab",
            Action::Newline => "Newline",
            Action::DeleteCharBack => "DeleteCharBack",
            Action::DeleteCharForward => "DeleteCharForward",
            Action::DeleteWordBack => "DeleteWordBack",
            Action::DeleteWordForward => "DeleteWordForward",
            Action::DeleteLine => "DeleteLine",
            Action::Cut => "Cut",
            Action::Copy => "Copy",
            Action::Paste => "Paste",
            Action::SelectLeft => "SelectLeft",
            Action::SelectRight => "SelectRight",
            Action::SelectUp => "SelectUp",
            Action::SelectDown => "SelectDown",
            Action::SelectAll => "SelectAll",
            Action::Undo => "Undo",
            Action::Redo => "Redo",
            Action::Save => "Save",
            Action::Open => "Open",
            Action::EnterEditMode => "EnterEditMode",
            Action::ExitToPreview => "ExitToPreview",
            Action::ToggleRawMode => "ToggleRawMode",
            Action::Quit => "Quit",
            Action::ToggleCheckbox => "ToggleCheckbox",
        };
        f.write_str(s)
    }
}

impl FromStr for Action {
    type Err = KeyMapError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ScrollUp" => Ok(Action::ScrollUp),
            "ScrollDown" => Ok(Action::ScrollDown),
            "ScrollPageUp" => Ok(Action::ScrollPageUp),
            "ScrollPageDown" => Ok(Action::ScrollPageDown),
            "ScrollToTop" => Ok(Action::ScrollToTop),
            "ScrollToBottom" => Ok(Action::ScrollToBottom),
            "MoveLeft" => Ok(Action::MoveLeft),
            "MoveRight" => Ok(Action::MoveRight),
            "MoveUp" => Ok(Action::MoveUp),
            "MoveDown" => Ok(Action::MoveDown),
            "MoveWordLeft" => Ok(Action::MoveWordLeft),
            "MoveWordRight" => Ok(Action::MoveWordRight),
            "MoveLineStart" => Ok(Action::MoveLineStart),
            "MoveLineEnd" => Ok(Action::MoveLineEnd),
            "MoveDocStart" => Ok(Action::MoveDocStart),
            "MoveDocEnd" => Ok(Action::MoveDocEnd),
            "InsertTab" => Ok(Action::InsertTab),
            "Newline" => Ok(Action::Newline),
            "DeleteCharBack" => Ok(Action::DeleteCharBack),
            "DeleteCharForward" => Ok(Action::DeleteCharForward),
            "DeleteWordBack" => Ok(Action::DeleteWordBack),
            "DeleteWordForward" => Ok(Action::DeleteWordForward),
            "DeleteLine" => Ok(Action::DeleteLine),
            "Cut" => Ok(Action::Cut),
            "Copy" => Ok(Action::Copy),
            "Paste" => Ok(Action::Paste),
            "SelectLeft" => Ok(Action::SelectLeft),
            "SelectRight" => Ok(Action::SelectRight),
            "SelectUp" => Ok(Action::SelectUp),
            "SelectDown" => Ok(Action::SelectDown),
            "SelectAll" => Ok(Action::SelectAll),
            "Undo" => Ok(Action::Undo),
            "Redo" => Ok(Action::Redo),
            "Save" => Ok(Action::Save),
            "Open" => Ok(Action::Open),
            "EnterEditMode" => Ok(Action::EnterEditMode),
            "ExitToPreview" => Ok(Action::ExitToPreview),
            "ToggleRawMode" => Ok(Action::ToggleRawMode),
            "Quit" => Ok(Action::Quit),
            "ToggleCheckbox" => Ok(Action::ToggleCheckbox),
            other => Err(KeyMapError::UnknownAction(other.to_owned())),
        }
    }
}

// ─── Key parsing ─────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum KeyMapError {
    #[error("unknown action name: '{0}'")]
    UnknownAction(String),
    #[error("unparseable key string: '{0}'")]
    UnparseableKey(String),
}

/// Parse a human-readable key string such as `"ctrl+q"`, `"up"`, `"page_up"`,
/// `"ctrl+shift+z"` into a crossterm `KeyEvent`.
pub fn parse_key(s: &str) -> Result<KeyEvent, KeyMapError> {
    let lower = s.to_lowercase();
    let parts: Vec<&str> = lower.split('+').collect();

    let mut modifiers = KeyModifiers::NONE;
    let mut key_part = "";

    for (i, part) in parts.iter().enumerate() {
        match *part {
            "ctrl" => modifiers |= KeyModifiers::CONTROL,
            "alt" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            _ => {
                if i == parts.len() - 1 {
                    key_part = part;
                } else {
                    return Err(KeyMapError::UnparseableKey(s.to_owned()));
                }
            }
        }
    }

    if key_part.is_empty() {
        return Err(KeyMapError::UnparseableKey(s.to_owned()));
    }

    let code = match key_part {
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "page_up" | "pageup" => KeyCode::PageUp,
        "page_down" | "pagedown" => KeyCode::PageDown,
        "enter" | "return" => KeyCode::Enter,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "escape" | "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "insert" => KeyCode::Insert,
        "f1" => KeyCode::F(1),
        "f2" => KeyCode::F(2),
        "f3" => KeyCode::F(3),
        "f4" => KeyCode::F(4),
        "f5" => KeyCode::F(5),
        "f6" => KeyCode::F(6),
        "f7" => KeyCode::F(7),
        "f8" => KeyCode::F(8),
        "f9" => KeyCode::F(9),
        "f10" => KeyCode::F(10),
        "f11" => KeyCode::F(11),
        "f12" => KeyCode::F(12),
        // Single character
        c if c.chars().count() == 1 => {
            let ch = c.chars().next().unwrap();
            KeyCode::Char(ch)
        }
        _ => return Err(KeyMapError::UnparseableKey(s.to_owned())),
    };

    Ok(KeyEvent::new(code, modifiers))
}

// ─── KeyBindingOverrides ──────────────────────────────────────────────────────

/// The `[keybindings]` section of config.toml. Maps action name strings to key
/// strings. Unknown action names are an error at startup.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyBindingOverrides(pub HashMap<String, String>);

// ─── KeyMap ───────────────────────────────────────────────────────────────────

/// Maps `KeyEvent`s to `Action`s. Built from compiled-in defaults, then
/// overridden by the user's `[keybindings]` config.
#[derive(Debug, Clone)]
pub struct KeyMap {
    /// Primary map: key event → action.
    bindings: HashMap<KeyEvent, Action>,
}

impl KeyMap {
    /// Build a `KeyMap` with compiled-in defaults, then apply any overrides
    /// from config. Returns an error if any override contains an unknown action
    /// name or an unparseable key string.
    pub fn build(overrides: &KeyBindingOverrides) -> Result<Self, KeyMapError> {
        let mut map = Self::default_bindings();

        for (action_str, key_str) in &overrides.0 {
            let action = Action::from_str(action_str)?;
            let key = parse_key(key_str)?;
            map.bindings.insert(key, action);
        }

        Ok(map)
    }

    /// Look up the action bound to a key event, if any.
    pub fn action_for(&self, event: &KeyEvent) -> Option<&Action> {
        self.bindings.get(event)
    }

    /// Build the compiled-in default bindings.
    fn default_bindings() -> Self {
        let mut b: HashMap<KeyEvent, Action> = HashMap::new();

        macro_rules! bind {
            ($key:expr, $action:expr) => {
                if let Ok(k) = parse_key($key) {
                    b.insert(k, $action);
                }
            };
        }

        // Quit — Ctrl-Q only. Ctrl-C is Copy (see below).
        bind!("ctrl+q", Action::Quit);

        // Scrolling / cursor movement
        // Arrow keys → cursor movement in all modes; MoveUp/Down act as
        // ScrollUp/ScrollDown when in Preview mode (handled in app).
        bind!("up", Action::MoveUp);
        bind!("down", Action::MoveDown);
        bind!("left", Action::MoveLeft);
        bind!("right", Action::MoveRight);
        bind!("ctrl+left", Action::MoveWordLeft);
        bind!("ctrl+right", Action::MoveWordRight);
        bind!("ctrl+a", Action::MoveLineStart);
        bind!("ctrl+e", Action::MoveLineEnd);
        bind!("ctrl+home", Action::MoveDocStart);
        bind!("ctrl+end", Action::MoveDocEnd);

        // Explicit scrolling (works in all modes)
        bind!("page_up", Action::ScrollPageUp);
        bind!("page_down", Action::ScrollPageDown);
        bind!("home", Action::ScrollToTop);
        bind!("end", Action::ScrollToBottom);

        // Editing (Phase 1)
        bind!("enter", Action::Newline);
        bind!("tab", Action::InsertTab);
        bind!("backspace", Action::DeleteCharBack);
        bind!("delete", Action::DeleteCharForward);
        bind!("ctrl+backspace", Action::DeleteWordBack);
        bind!("ctrl+delete", Action::DeleteWordForward);
        bind!("ctrl+d", Action::DeleteLine);

        // History (Phase 1)
        bind!("ctrl+z", Action::Undo);
        bind!("ctrl+y", Action::Redo);

        // Clipboard (Phase 1)
        // Ctrl-C → Copy (not Quit). The app intercepts Ctrl-C in crossterm
        // raw mode before it can generate SIGINT, so this is safe.
        bind!("ctrl+c", Action::Copy);
        bind!("ctrl+x", Action::Cut);
        bind!("ctrl+v", Action::Paste);

        // File operations
        bind!("ctrl+s", Action::Save);
        bind!("ctrl+o", Action::Open);

        // Mode transitions
        bind!("escape", Action::ExitToPreview);
        bind!("ctrl+`", Action::ToggleRawMode);

        // Selection (Phase 1) — Shift+Arrow extends the selection.
        bind!("shift+left", Action::SelectLeft);
        bind!("shift+right", Action::SelectRight);
        bind!("shift+up", Action::SelectUp);
        bind!("shift+down", Action::SelectDown);
        bind!("ctrl+shift+a", Action::SelectAll);

        // List (Phase 3)
        bind!("ctrl+space", Action::ToggleCheckbox);

        Self { bindings: b }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_keymap_has_quit() {
        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let key = parse_key("ctrl+q").unwrap();
        assert_eq!(km.action_for(&key), Some(&Action::Quit));
    }

    #[test]
    fn ctrl_c_is_copy_not_quit() {
        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let key = parse_key("ctrl+c").unwrap();
        assert_eq!(km.action_for(&key), Some(&Action::Copy));
    }

    #[test]
    fn override_changes_binding() {
        let mut overrides = KeyBindingOverrides::default();
        overrides.0.insert("Quit".into(), "ctrl+x".into());
        let km = KeyMap::build(&overrides).unwrap();
        let key = parse_key("ctrl+x").unwrap();
        assert_eq!(km.action_for(&key), Some(&Action::Quit));
    }

    #[test]
    fn unknown_action_is_error() {
        let mut overrides = KeyBindingOverrides::default();
        overrides.0.insert("TypoAction".into(), "ctrl+x".into());
        assert!(KeyMap::build(&overrides).is_err());
    }

    #[test]
    fn unparseable_key_is_error() {
        let mut overrides = KeyBindingOverrides::default();
        overrides.0.insert("Quit".into(), "superkey+q".into());
        assert!(KeyMap::build(&overrides).is_err());
    }

    #[test]
    fn parse_key_variants() {
        assert!(parse_key("up").is_ok());
        assert!(parse_key("page_down").is_ok());
        assert!(parse_key("ctrl+s").is_ok());
        assert!(parse_key("ctrl+shift+z").is_ok());
        assert!(parse_key("escape").is_ok());
    }
}
