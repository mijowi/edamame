use std::collections::HashMap;
use std::fmt;
use std::path::Path;
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
    // ── Cursor movement ─────────────────────────────────────────────
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
    // ── Editing ────────────────────────────────────────────────────
    InsertChar(char),
    InsertTab,
    Newline,
    DeleteCharBack,
    DeleteCharForward,
    DeleteWordBack,
    DeleteWordForward,
    DeleteLine,
    // ── Clipboard ──────────────────────────────────────────────────
    Cut,
    Copy,
    Paste,
    // ── Selection ──────────────────────────────────────────────────
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    SelectAll,
    // ── History ────────────────────────────────────────────────────
    Undo,
    Redo,
    // ── File operations ────────────────────────────────────────────
    Save,
    /// Write the current buffer to a different path without changing
    /// the buffer's associated path — subsequent `Save`s still write
    /// to the original file.  Distinct from a hypothetical "Save As"
    /// (which would *also* switch the buffer to the new path).
    SaveCopy,
    Open,
    // ── Mode transitions ───────────────────────────────────────────
    EnterEditMode,
    ExitToPreview,
    ToggleRawMode,
    // ── App control ────────────────────────────────────────────────
    Quit,
    // ── List editing ───────────────────────────────────────────────
    ToggleCheckbox,
    // ── Table editing ──────────────────────────────────────────────
    // Cell navigation. Tab/Shift+Tab/Enter outside a table retain their
    // normal behaviour; edit_ops redirects them when the cursor is inside
    // a table (Phase 2 implementation).
    TableNextCell,
    TablePrevCell,
    TableNextRow,
    TablePrevRow,
    // Row/column reorder (Alt+Arrow).
    TableMoveRowUp,
    TableMoveRowDown,
    TableMoveColumnLeft,
    TableMoveColumnRight,
    // Row/column insertion (Alt+Shift+Arrow).
    TableInsertRowAbove,
    TableInsertRowBelow,
    TableInsertColumnLeft,
    TableInsertColumnRight,
    // Row/column deletion.
    TableDeleteRow,
    TableDeleteColumn,
    // Shift+Enter inside a cell inserts a literal `<br>` (GFM supports this
    // as the canonical way to get multi-line cells).  Outside a table it
    // falls through to `Newline`.
    TableInsertBreak,
    // ── Link navigation (Phase 8) ──────────────────────────────────
    /// Follow the link at the cursor's rope offset (if any).  In
    /// Preview mode users reach links via mouse click; in Rendered /
    /// Raw mode this action is the keyboard equivalent.  Handled by
    /// the `App`, not `edit_ops`, so the dispatch happens against UI
    /// state (nav stack, in-flight worker threads).
    FollowLinkUnderCursor,
    /// Pop the navigation history: move the current (path, scroll,
    /// cursor, mode) onto the forward stack and restore the most
    /// recent back-entry.  App-level.
    NavigateBack,
    /// Mirror of [`Action::NavigateBack`] operating on the forward
    /// stack.
    NavigateForward,
    /// Open the cheat-sheet popover listing every keybinding grouped
    /// by category.  Intentionally unbound by default — the overlay
    /// is reached only via the command palette (Phase 10) so it
    /// doesn't consume a dedicated key that would collide with
    /// typing inside a cell / paragraph / list.
    ShowCheatSheet, // Configurable!
    /// Open the fuzzy-searchable command palette (Ctrl-P).  Lists
    /// every bound action and routes the chosen one through the
    /// normal `edit_ops::apply` path.
    ShowCommandPalette,
    /// Show the static Markdown syntax cheat sheet (CommonMark + GFM
    /// tables / task lists / strikethrough / footnotes).
    ShowMarkdownCheatSheet,
    /// Open the settings overlay — edits `[editor] / [modal] / [table]
    /// / [images] / [export]` keys in `config.toml` in place.
    OpenSettings,
    /// Open the keybinds overlay — edits `keybindings.toml` with
    /// conflict detection.
    OpenKeybinds,
    /// Reveal the active config directory in the OS file manager / open
    /// it via `open::that`.
    OpenConfigFolder,
    /// Palette entry for the built-in HTML exporter.  Wired
    /// up here so the palette's `Suggested` list can reference it.
    ExportHtml,
    /// Re-reads the file from disk, discarding in-memory edits.
    ReloadFromDisk,
    /// Save the current buffer and open it in `$VISUAL` / `$EDITOR`
    /// (falling back to the OS handler).  Reuses the same suspend /
    /// resume flow the settings overlay uses for `config.toml`.  The
    /// buffer is reloaded from disk after the editor exits so any
    /// external edits are picked up.
    OpenInExternalEditor,
    /// Toggle the in-memory `config.table.show_buttons` flag.
    /// Intentionally does NOT persist to `config.toml` — the user can
    /// flip handles for the current session without committing the
    /// change.
    ToggleTableButtons,
    /// Open the rows/columns modal that inserts a fresh
    /// GFM pipe table at the cursor.  Requires the cursor to be on
    /// a blank line; the App-level handler flashes an error
    /// when that pre-flight fails.
    InsertTable,
    /// Open the edamame GitHub repository.
    OpenGitHub,
}

/// Drive `Display for Action` and `FromStr for Action` from a single
/// list of unit variant names.  Every payload-bearing variant has to
/// be named explicitly outside the macro: those go into the `Display`
/// `match` only (FromStr can't reconstruct them without their
/// payload).
macro_rules! action_variants {
    ($( $variant:ident ),* $(,)?) => {
        impl fmt::Display for Action {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let s: &str = match self {
                    $( Action::$variant => stringify!($variant), )*
                    Action::InsertChar(_) => "InsertChar",
                };
                f.write_str(s)
            }
        }

        impl FromStr for Action {
            type Err = KeyMapError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $( stringify!($variant) => Ok(Action::$variant), )*
                    other => Err(KeyMapError::UnknownAction(other.to_owned())),
                }
            }
        }
    };
}

action_variants! {
    ScrollUp, ScrollDown, ScrollPageUp, ScrollPageDown, ScrollToTop, ScrollToBottom,
    MoveLeft, MoveRight, MoveUp, MoveDown,
    MoveWordLeft, MoveWordRight,
    MoveLineStart, MoveLineEnd,
    MoveDocStart, MoveDocEnd,
    InsertTab, Newline,
    DeleteCharBack, DeleteCharForward, DeleteWordBack, DeleteWordForward, DeleteLine,
    Cut, Copy, Paste,
    SelectLeft, SelectRight, SelectUp, SelectDown, SelectAll,
    Undo, Redo,
    Save, SaveCopy, Open,
    EnterEditMode, ExitToPreview, ToggleRawMode, Quit,
    ToggleCheckbox,
    TableNextCell, TablePrevCell, TableNextRow, TablePrevRow,
    TableMoveRowUp, TableMoveRowDown, TableMoveColumnLeft, TableMoveColumnRight,
    TableInsertRowAbove, TableInsertRowBelow,
    TableInsertColumnLeft, TableInsertColumnRight,
    TableDeleteRow, TableDeleteColumn,
    TableInsertBreak,
    FollowLinkUnderCursor, OpenGitHub,
    NavigateBack, NavigateForward,
    ShowCheatSheet, ShowCommandPalette, ShowMarkdownCheatSheet,
    OpenSettings, OpenKeybinds, OpenConfigFolder,
    ExportHtml, ReloadFromDisk, OpenInExternalEditor,
    ToggleTableButtons, InsertTable,
}

// ─── Key parsing ─────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum KeyMapError {
    #[error("unknown action name: '{0}'")]
    UnknownAction(String),
    #[error("unparseable key string: '{0}'")]
    UnparseableKey(String),
    #[error("'{key}' is already bound to {action}")]
    ConflictingBinding { key: String, action: String },
}

/// Parse a single token as a `KeyModifiers` flag, or return `None` if it
/// is not a modifier name.
fn parse_modifier(part: &str) -> Option<KeyModifiers> {
    match part {
        "ctrl" => Some(KeyModifiers::CONTROL),
        "alt" => Some(KeyModifiers::ALT),
        "shift" => Some(KeyModifiers::SHIFT),
        _ => None,
    }
}

/// Parse a `key_part` token (everything after the modifiers) into a
/// `KeyCode`, or return `None` if the token is not a recognised key.
fn parse_key_code(key_part: &str) -> Option<KeyCode> {
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
        "space" => KeyCode::Char(' '),
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
        _ => {
            // Single Unicode scalar value — anything else is unparseable.
            let mut chars = key_part.chars();
            let first = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(first)
        }
    };
    Some(code)
}

/// Parse a human-readable key string such as `"ctrl+q"`, `"up"`, `"page_up"`,
/// `"ctrl+shift+z"` into a crossterm `KeyEvent`.
pub fn parse_key(s: &str) -> Result<KeyEvent, KeyMapError> {
    let lower = s.to_lowercase();
    let parts: Vec<&str> = lower.split('+').collect();

    let mut modifiers = KeyModifiers::NONE;
    let mut key_part = "";

    let last_idx = parts.len().saturating_sub(1);
    for (i, part) in parts.iter().enumerate() {
        if let Some(m) = parse_modifier(part) {
            modifiers |= m;
        } else if i == last_idx {
            key_part = part;
        } else {
            return Err(KeyMapError::UnparseableKey(s.to_owned()));
        }
    }

    if key_part.is_empty() {
        return Err(KeyMapError::UnparseableKey(s.to_owned()));
    }

    let code = parse_key_code(key_part).ok_or_else(|| KeyMapError::UnparseableKey(s.to_owned()))?;
    Ok(KeyEvent::new(code, modifiers))
}

/// Glyph-style label for a non-character `KeyCode` (compact form used in
/// the bottom-region hint line).  Returns `None` for `KeyCode::Char` —
/// callers handle character keys directly.
fn keycode_glyph(code: KeyCode) -> Option<&'static str> {
    Some(match code {
        KeyCode::Up => "↑",
        KeyCode::Down => "↓",
        KeyCode::Left => "←",
        KeyCode::Right => "→",
        KeyCode::Enter => "↵",
        // BackTab is the terminal's representation of Shift+Tab — collapse
        // to the canonical `⇧⇥` glyph so it reads the same regardless of
        // which form the source `KeyEvent` used.
        KeyCode::Tab | KeyCode::BackTab => "⇥",
        KeyCode::Backspace => "⌫",
        KeyCode::Delete => "Del",
        KeyCode::Esc => "Esc",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "PgUp",
        KeyCode::PageDown => "PgDn",
        KeyCode::Insert => "Ins",
        _ => return None,
    })
}

/// Word-style label for a non-character `KeyCode` (long form used in the
/// keybinds overlay and cheat sheet).
fn keycode_word(code: KeyCode) -> Option<&'static str> {
    Some(match code {
        KeyCode::Up => "Up",
        KeyCode::Down => "Down",
        KeyCode::Left => "Left",
        KeyCode::Right => "Right",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "PgUp",
        KeyCode::PageDown => "PgDn",
        KeyCode::Enter => "Enter",
        KeyCode::Backspace => "Backspace",
        KeyCode::Delete => "Delete",
        KeyCode::Esc => "Esc",
        KeyCode::Tab => "Tab",
        KeyCode::BackTab => "Shift-Tab",
        KeyCode::Insert => "Insert",
        _ => return None,
    })
}

/// Render the key-code portion of a chord using `lookup` for the named
/// (non-Char) keys.  `KeyCode::Char` always renders as its uppercase form
/// for ASCII letters or as itself otherwise, with `' '` shown as `Space`.
fn format_keycode(code: KeyCode, lookup: fn(KeyCode) -> Option<&'static str>) -> String {
    if let Some(s) = lookup(code) {
        return s.to_owned();
    }
    match code {
        KeyCode::Char(' ') => "Space".to_owned(),
        KeyCode::Char(c) if c.is_ascii_alphabetic() => c.to_ascii_uppercase().to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{:?}", other),
    }
}

/// Render `ev` as a compact glyph-based chord string suitable for the
/// bottom-region hint line, where horizontal space is at a premium.
/// Modifiers collapse to single characters (`^` / `⌥` / `⇧`) and
/// non-printable keys use Unicode glyphs (`↑` / `↓` / `←` / `→` / `↵`
/// / `⇥` / `⌫`).  Mirrors [`format_key`] for everything else.
///
/// This is the inverse of how the hint line *used* to hardcode chord
/// glyphs — by going through this formatter, the displayed chord
/// always tracks whatever the live `KeyMap` has bound for the action.
pub fn format_key_compact(ev: &KeyEvent) -> String {
    let mut out = String::new();
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        out.push('^');
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        out.push('⌥');
    }
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT) || ev.code == KeyCode::BackTab;
    if shift {
        out.push('⇧');
    }
    out.push_str(&format_keycode(ev.code, keycode_glyph));
    out
}

/// Render `ev` as a human-readable key string roughly matching what
/// [`parse_key`] accepts.  Used by the Phase 9 cheat-sheet popover to
/// display bindings; the inverse of `parse_key` is good enough here
/// even if it's not strictly round-tripping (e.g. we emit `Ctrl-C`
/// rather than `ctrl+c` for readability).
pub fn format_key(ev: &KeyEvent) -> String {
    let mut parts: Vec<String> = Vec::new();
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("Ctrl".into());
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        parts.push("Alt".into());
    }
    if ev.modifiers.contains(KeyModifiers::SHIFT) {
        parts.push("Shift".into());
    }
    parts.push(format_keycode(ev.code, keycode_word));
    parts.join("-")
}

// ─── KeyBindingOverrides ──────────────────────────────────────────────────────

/// The `[keybindings]` section of config.toml. Maps action name strings to key
/// strings. Unknown action names are an error at startup.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyBindingOverrides(pub HashMap<String, String>);

impl KeyBindingOverrides {
    /// Persist the overrides to `path` as TOML.  Used by the Phase 10
    /// keybinds overlay so a rebind takes effect immediately and
    /// survives the next startup.  Returns the underlying I/O / TOML
    /// error verbatim — callers typically log + flash on failure
    /// rather than treating it as fatal.
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        use anyhow::Context;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory: {}", parent.display())
            })?;
        }
        let toml_str =
            toml::to_string_pretty(self).context("Failed to serialize keybindings to TOML")?;
        std::fs::write(path, toml_str)
            .with_context(|| format!("Failed to write keybindings file: {}", path.display()))?;
        Ok(())
    }
}

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

    /// Return the first key (in insertion order, which HashMap does not
    /// guarantee but is fine for an approximate lookup) bound to
    /// `action`, formatted as a human-readable string.  Used by the
    /// Phase 9 cheat-sheet popover to surface the current binding for
    /// each known action.  When multiple keys are bound, only one is
    /// returned — callers that need every binding should iterate
    /// `bindings()` themselves.
    pub fn first_key_for(&self, action: &Action) -> Option<String> {
        self.bindings
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(k, _)| format_key(k))
    }

    /// Like [`KeyMap::first_key_for`] but returns the raw `KeyEvent` so
    /// callers can apply their own formatter (e.g. the bottom-region
    /// hint line uses `format_key_compact` instead of `format_key`).
    pub fn first_key_event_for(&self, action: &Action) -> Option<KeyEvent> {
        self.bindings
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(k, _)| *k)
    }

    /// Rebind `action` to `new_key` (parsed from `parse_key`-style
    /// syntax).  If `new_key` is already bound to a *different*
    /// action, returns `Err` and leaves the keymap unchanged — that's
    /// the conflict-detection contract from Phase 10's keybinds
    /// overlay.  On success, the overrides table is updated to keep
    /// the on-disk shape in sync with the in-memory keymap.
    ///
    /// Existing bindings of the same `action` are removed so the
    /// caller doesn't have to track stale mappings.
    pub fn rebind(
        &mut self,
        action: &Action,
        new_key: &str,
        overrides: &mut KeyBindingOverrides,
    ) -> Result<(), KeyMapError> {
        let parsed = parse_key(new_key)?;
        if let Some(existing) = self.bindings.get(&parsed) {
            if existing != action {
                return Err(KeyMapError::ConflictingBinding {
                    key: new_key.to_owned(),
                    action: existing.to_string(),
                });
            }
            // Same action already bound to the same key — no-op.
            return Ok(());
        }
        // Drop any prior key bound to this action so we don't end up
        // with two chords for the same action sticking around.
        self.bindings.retain(|_, a| a != action);
        self.bindings.insert(parsed, action.clone());
        overrides.0.insert(action.to_string(), new_key.to_owned());
        Ok(())
    }

    /// Look up the action bound to a key event, if any.
    pub fn action_for(&self, event: &KeyEvent) -> Option<&Action> {
        // Normalize: strip `state` and force `kind: Press` so the kitty
        // keyboard protocol (which reports KEYPAD / CAPS_LOCK state flags)
        // does not prevent HashMap lookup. `KeyEvent`'s PartialEq/Hash
        // compare all four fields, and `parse_key` always produces events
        // with `state: EMPTY, kind: Press`.
        let normalized = KeyEvent::new(event.code, event.modifiers);
        if let Some(action) = self.bindings.get(&normalized) {
            return Some(action);
        }
        // Some terminals report Shift+Tab as `KeyCode::BackTab` (with or
        // without the SHIFT modifier set).  Normalize it to the canonical
        // `Tab + SHIFT` form produced by `parse_key("shift+tab")` so bindings
        // match regardless of which representation the terminal emits.
        if event.code == KeyCode::BackTab {
            let fallback = KeyEvent::new(KeyCode::Tab, event.modifiers | KeyModifiers::SHIFT);
            return self.bindings.get(&fallback);
        }
        None
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
        // Ctrl+A is SelectAll (typical GUI editor convention).  Unix shell
        // users who want move-line-start can still use Home.
        bind!("ctrl+a", Action::SelectAll);
        bind!("ctrl+e", Action::MoveLineEnd);
        bind!("ctrl+home", Action::MoveDocStart);
        bind!("ctrl+end", Action::MoveDocEnd);

        // Explicit scrolling (works in all modes)
        bind!("page_up", Action::ScrollPageUp);
        bind!("page_down", Action::ScrollPageDown);
        bind!("home", Action::ScrollToTop);
        bind!("end", Action::ScrollToBottom);

        // Editing
        bind!("enter", Action::Newline);
        bind!("tab", Action::InsertTab);
        bind!("backspace", Action::DeleteCharBack);
        bind!("delete", Action::DeleteCharForward);
        bind!("ctrl+backspace", Action::DeleteWordBack);
        bind!("ctrl+delete", Action::DeleteWordForward);
        bind!("ctrl+d", Action::DeleteLine);

        // History
        bind!("ctrl+z", Action::Undo);
        bind!("ctrl+shift+z", Action::Redo);

        // Clipboard
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

        // Selection — Shift+Arrow extends the selection.
        bind!("shift+left", Action::SelectLeft);
        bind!("shift+right", Action::SelectRight);
        bind!("shift+up", Action::SelectUp);
        bind!("shift+down", Action::SelectDown);
        bind!("ctrl+shift+a", Action::SelectAll);

        // List
        bind!("ctrl+space", Action::ToggleCheckbox);

        // Table editing (Phase 2) — org-mode-style Alt+Arrow scheme.
        // Arrow direction = operation direction; Shift promotes "reorder" to
        // "insert" on that side. Cell navigation (Tab / Shift+Tab / Enter) is
        // handled via context dispatch in edit_ops when the cursor is inside
        // a table — they remain bound to InsertTab / Newline by default.
        bind!("alt+up", Action::TableMoveRowUp);
        bind!("alt+down", Action::TableMoveRowDown);
        bind!("alt+left", Action::TableMoveColumnLeft);
        bind!("alt+right", Action::TableMoveColumnRight);
        bind!("alt+shift+up", Action::TableInsertRowAbove);
        bind!("alt+shift+down", Action::TableInsertRowBelow);
        bind!("alt+shift+left", Action::TableInsertColumnLeft);
        bind!("alt+shift+right", Action::TableInsertColumnRight);
        bind!("alt+backspace", Action::TableDeleteRow);
        bind!("alt+shift+backspace", Action::TableDeleteColumn);
        // Shift+Tab moves to the previous cell when the cursor is inside a
        // table; it is a no-op elsewhere.  Tab / Enter remain bound to
        // InsertTab / Newline so that context dispatch in edit_ops can decide
        // whether to insert text or move between cells.
        bind!("shift+tab", Action::TablePrevCell);
        // Shift+Enter inserts a literal `<br>` when the cursor is inside a
        // table cell; outside a table it has no binding and the default
        // Shift+Enter behaviour (same as Enter) applies.
        bind!("shift+enter", Action::TableInsertBreak);

        // Phase 8 — link navigation.  Alt+Left / Alt+Right are NOT bound to
        // NavigateBack/NavigateForward here: those keys remain bound to
        // TableMoveColumnLeft / TableMoveColumnRight so tables keep their
        // column-reorder semantics, and the `App` dispatches them to
        // NavigateBack/Forward only when the cursor is outside any table.
        // Users can still rebind NavigateBack/Forward to any key via the
        // keybindings config.
        bind!("ctrl+enter", Action::FollowLinkUnderCursor);

        // Phase 9 note — `Action::ShowCheatSheet` is intentionally
        // left unbound.  The cheat-sheet overlay is accessible only
        // via the command palette (Phase 10), not a dedicated key.
        // `?` would collide with typing text in cells / paragraphs,
        // and surfacing a separate help key (F1 etc.) would duplicate
        // the command-palette surface for no real gain.

        // Phase 10 — command palette.  Ctrl-P is the primary chord
        // (also surfaced on the bottom-region hint line as `^P Menu`).
        // The other Phase 10 actions (`ShowMarkdownCheatSheet`,
        // `OpenSettings`, `OpenKeybinds`, `OpenConfigFolder`,
        // `ExportHtml`, `ReloadFromDisk`) are intentionally unbound:
        // they are reached only via the palette, so the user can
        // search-and-execute without memorising a chord per overlay.
        bind!("ctrl+p", Action::ShowCommandPalette);

        // Phase 15 — graduation chord for the Insert Table command.
        // Tables can't be authored from Rendered mode without this
        // flow, so a discoverable keybind sits next to the palette
        // entry.
        bind!("ctrl+shift+t", Action::InsertTable);

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
    fn backtab_maps_to_shift_tab_binding() {
        // Some terminals emit Shift+Tab as `KeyCode::BackTab` instead of the
        // canonical `Tab + SHIFT` form.  `action_for` must match either way.
        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let backtab_no_mod = KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(km.action_for(&backtab_no_mod), Some(&Action::TablePrevCell));
        let backtab_shift = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(km.action_for(&backtab_shift), Some(&Action::TablePrevCell));
    }

    #[test]
    fn action_lookup_ignores_kitty_state_flags() {
        // The kitty keyboard protocol attaches non-default `state` flags
        // (e.g. KEYPAD) to events. `action_for` must look past those.
        use crossterm::event::{KeyEventKind, KeyEventState};
        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let ctrl_q_with_state = KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::KEYPAD,
        };
        assert_eq!(km.action_for(&ctrl_q_with_state), Some(&Action::Quit));
    }

    #[test]
    fn parse_key_variants() {
        assert!(parse_key("up").is_ok());
        assert!(parse_key("page_down").is_ok());
        assert!(parse_key("ctrl+s").is_ok());
        assert!(parse_key("ctrl+shift+z").is_ok());
        assert!(parse_key("escape").is_ok());
        assert!(parse_key("space").is_ok());
        assert!(parse_key("ctrl+space").is_ok());
    }

    #[test]
    fn ctrl_space_maps_to_toggle_checkbox() {
        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let key = parse_key("ctrl+space").unwrap();
        assert_eq!(km.action_for(&key), Some(&Action::ToggleCheckbox));
    }
}
