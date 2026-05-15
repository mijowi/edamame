//! Non-fatal warnings produced by config loading.

use std::path::PathBuf;

/// One problem detected while reading a config file.  Surfaced as a
/// scrollable modal at startup (and after the user-edited config returns
/// from the external editor), so typos and stale field names don't fail
/// silently.  Non-fatal: the loader still returns the best-effort parsed
/// value (or default) and the editor continues running.
#[derive(Debug, Clone)]
pub struct ConfigWarning {
    /// File the warning came from.  Display this verbatim — the user
    /// almost always wants to know which file to open.
    pub path: PathBuf,
    pub kind: WarningKind,
}

/// What went wrong with a single config file.  Each variant carries the
/// specific detail the modal renders so the body strings live near the
/// loader (single source of truth) rather than in the App.
#[derive(Debug, Clone)]
pub enum WarningKind {
    /// `toml::from_str` returned `Err`.  The string is the formatted
    /// `toml::de::Error` — already includes line and column.  We fall
    /// back to defaults for the file's data.
    ParseError(String),
    /// `serde_ignored` reported keys that no struct field consumed.
    /// Each entry is a dotted path (e.g. `editor.tab_widht`).  The
    /// parsed struct is still applied — only the unrecognised keys are
    /// silently dropped, so the warning is the user's only signal.
    UnknownKeys(Vec<String>),
    /// One or more entries in `keybindings.toml` referenced an unknown
    /// action name or an unparseable key string.  Bad entries are
    /// dropped from the live keymap; valid entries still take effect.
    InvalidKeybindings(Vec<String>),
    /// The active theme named in `config.toml` was neither a built-in
    /// nor a file in `themes/`.  The loader substitutes a built-in
    /// chosen by terminal capability (`Edamame` on true-color, `256
    /// Dark` otherwise) and rewrites `config.toml` so the warning
    /// doesn't re-surface on next launch.
    MissingTheme {
        /// The theme name read from `config.toml`.
        requested: String,
        /// The built-in substituted in its place.
        fallback: String,
    },
}
