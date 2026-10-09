use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::init::{ensure_default_files_in, REFERENCE_CONFIG_TOML};
use super::keymap::KeyBindingOverrides;
use super::persistence::config_writes_allowed;
use super::readers::{read_keybindings, read_main_config, read_theme_named};
pub use super::sections::{
    AppearanceMode, CustomExportEntry, DevConfig, EditorConfig, ExportConfig, FiguresConfig,
    FiguresEnabled, ImagesConfig, ImagesEnabled, ModalConfig, RemoteImagePolicy, TableConfig,
};
use super::state::State;
use super::theme::Theme;
use super::theme_file::ThemeFile;
pub use super::warnings::{ConfigWarning, WarningKind};

/// Top-level `config.toml` — editor/rendering settings and the active theme's name.
/// Keybinding overrides live in `keybindings.toml`, theme style tables in
/// `themes/<name>.toml`; [`LoadedConfig`] reads all three.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Active theme: a `BUILTIN_THEMES` name resolves to a compiled-in palette, anything
    /// else to `themes/<theme>.toml`.  A missing file falls back to `Theme::default()`.
    pub theme: String,
    /// Session-only stash of the user's on-disk theme name, set when the startup
    /// indexed-color downgrade replaced [`Self::theme`].
    ///
    /// `theme` carries the *effective* name so every consumer agrees with what is on
    /// screen; [`Config::save`] writes this stashed name back in its place, so a session
    /// downgraded for a weaker terminal never rewrites the theme chosen for a better one.
    /// An explicit choice clears it via [`Config::set_theme`].
    #[serde(skip)]
    pub theme_downgraded_from: Option<String>,
    /// Filters the theme picker and picks the counterpart previewed on a mode toggle;
    /// does not by itself change the active theme.
    pub appearance: AppearanceMode,
    pub editor: EditorConfig,
    pub modal: ModalConfig,
    pub table: TableConfig,
    pub images: ImagesConfig,
    /// Consent gate for inline rendering of *figures* — ```mermaid
    /// diagrams **and** `$$...$$` display math, which share one image
    /// pipeline and one consent switch.
    ///
    /// The section was `[diagrams]` before display math existed:
    /// `alias = "diagrams"` keeps old configs loading, and [`Config::load`]
    /// rewrites the header to `[figures]` on first launch (see
    /// [`migrate_legacy_config_keys`]).  The parallel `[export.html].figures`
    /// key gates the *export* side of the same pipeline.
    #[serde(alias = "diagrams")]
    pub figures: FiguresConfig,
    pub export: ExportConfig,
    pub dev: DevConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "Edamame".into(),
            theme_downgraded_from: None,
            appearance: AppearanceMode::default(),
            editor: EditorConfig::default(),
            modal: ModalConfig::default(),
            table: TableConfig::default(),
            images: ImagesConfig::default(),
            figures: FiguresConfig::default(),
            export: ExportConfig::default(),
            dev: DevConfig::default(),
        }
    }
}

/// The three on-disk config files as read by [`Config::load`], so `main` can hand each
/// piece to its owner.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
    pub keybindings: KeyBindingOverrides,
    pub theme: ThemeFile,
    /// Machine-written bookkeeping from `state.toml`, loaded (and migrated out of a legacy
    /// `config.toml`) alongside the config.  Ignored on the external-editor reload — the user
    /// edited `config.toml`, not this.
    pub state: State,
    /// Non-fatal parse problems, surfaced by the App in a startup warning modal.
    pub warnings: Vec<ConfigWarning>,
}

impl Default for LoadedConfig {
    /// The "no config loaded" fallback, used when [`Config::load`] itself errors.  The
    /// theme is the `ThemeFile` equivalent of `Theme::default()`, *not*
    /// `ThemeFile::default()`, so the editor stays themed with an unreadable config.
    fn default() -> Self {
        Self {
            config: Config::default(),
            keybindings: KeyBindingOverrides::default(),
            theme: (&Theme::default()).into(),
            state: State::default(),
            warnings: Vec::new(),
        }
    }
}

impl Config {
    /// Read `config.toml`, `keybindings.toml`, and `themes/<name>.toml` from the XDG
    /// config directory, defaulting for anything missing.
    ///
    /// Fail-soft: parse errors and unknown keys become `LoadedConfig::warnings` rather
    /// than errors, so a typo in one file never bricks the editor.
    ///
    /// `truecolor` matters only to the missing-theme fallback, choosing `Edamame` or
    /// `256 Dark` — see [`read_theme_named`] for the case table.  `persist_fallback`
    /// controls only the on-disk side-effect of that fallback: `true` at startup (silence
    /// a perpetual warning from a stale theme name), `false` on the external-editor
    /// reload, where the user may have just typed a name for a theme they have yet to
    /// install.
    pub fn load(truecolor: bool, persist_fallback: bool) -> Result<LoadedConfig> {
        let dir = Self::config_dir();
        let mut warnings = Vec::new();
        let mut config = match &dir {
            Some(d) => read_main_config(&d.join("config.toml"), &mut warnings),
            None => Config::default(),
        };
        let keybindings = match &dir {
            Some(d) => read_keybindings(&d.join("keybindings.toml"), &mut warnings),
            None => KeyBindingOverrides::default(),
        };
        let (theme, fallback) = match &dir {
            Some(d) => read_theme_named(d, &config.theme, truecolor, &mut warnings),
            None => (ThemeFile::default(), None),
        };
        if let Some(name) = fallback {
            config.theme = name;
            if persist_fallback {
                if let Err(e) = config.save() {
                    tracing::warn!(
                        error = %e,
                        "failed to persist theme fallback to config.toml",
                    );
                }
            }
        }
        // Migrate the on-disk `config.toml` so it matches the current spelling.  Gated on the same
        // persist flag as the theme-fallback write above and on config writes being allowed (test
        // isolation, `--no-config`); each step is a no-op when there is nothing to do, so an
        // up-to-date config never triggers a write.  The session already loaded correctly whether
        // or not this runs — the `alias` on `Config::diagrams` covers the rename, and the four
        // bookkeeping keys are filtered out of the unknown-key warning by `read_main_config`.
        if persist_fallback && config_writes_allowed() {
            if let Some(d) = &dir {
                let config_path = d.join("config.toml");
                // Order matters: seed `state.toml` from the OLD `config.toml` values *before* the
                // migration strips them.  A no-op once `state.toml` exists.
                seed_state_from_legacy_config(&config_path);
                // Rename [diagrams] → [figures] AND strip the four bookkeeping keys.
                migrate_config_file_in_place(&config_path);
            }
        }
        // Read `state.toml` after any seed above wrote it.
        let state = State::load();
        Ok(LoadedConfig {
            config,
            keybindings,
            theme,
            state,
            warnings,
        })
    }

    /// `$XDG_CONFIG_HOME/edamame`, else `~/.config/edamame`; `None` when neither resolves.
    ///
    /// Deliberately XDG on **every** platform, macOS included (where `dirs::config_dir()`
    /// would give `~/Library/Application Support`): the config is hand-editable TOML users
    /// symlink from a dotfiles repo, so it follows the terminal-tool convention and one
    /// path works across Linux and macOS unchanged.
    pub fn config_dir() -> Option<PathBuf> {
        resolve_config_dir(std::env::var_os("XDG_CONFIG_HOME"), dirs::home_dir())
    }

    /// Returns the path to the main config file (may not exist yet).
    pub fn config_path() -> Option<PathBuf> {
        Self::config_dir().map(|d| d.join("config.toml"))
    }

    /// Commit an *explicit* theme choice, clearing [`Self::theme_downgraded_from`] so a
    /// theme picked while the indexed-color downgrade is in effect outranks the
    /// substitution and reaches disk.
    ///
    /// Deliberately NOT used by the picker's live-preview writes: `Esc` restores the
    /// pre-open name, so clearing the stash there would drop the downgrade on a cancel.
    pub fn set_theme(&mut self, name: String) {
        self.theme = name;
        self.theme_downgraded_from = None;
    }

    /// `self` with a session-only indexed-color downgrade undone, so `theme` carries the
    /// user's own choice.  `save_merge` overwrites every key it finds, so without this a
    /// downgraded session would write `256 Dark` over the theme picked on a truecolor
    /// terminal — one `config.toml` is typically shared between both.
    fn as_written(&self) -> Config {
        match &self.theme_downgraded_from {
            Some(original) => Config {
                theme: original.clone(),
                theme_downgraded_from: None,
                ..self.clone()
            },
            None => self.clone(),
        }
    }

    /// Persist to `config.toml` only — never `keybindings.toml` or a theme file, which
    /// are user-authored.  The merge in [`save_merge`] preserves comments and formatting.
    ///
    /// A `--no-config` session returns `Ok(())` without writing: it never read the user's
    /// files, so it has no business rewriting them from compiled defaults.  Success rather
    /// than an error because nothing went wrong — callers append
    /// [`unpersisted_suffix`](crate::config::unpersisted_suffix) to their "saved" message.
    pub fn save(&self) -> Result<()> {
        if !config_writes_allowed() {
            return Ok(());
        }
        let path = Self::config_path()
            .context("Could not determine config directory (missing XDG_CONFIG_HOME/HOME)")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory: {}", parent.display())
            })?;
        }
        let output = save_merge(&self.as_written(), &path)?;
        std::fs::write(&path, output)
            .with_context(|| format!("Failed to write config file: {}", path.display()))?;
        Ok(())
    }

    /// Write each default config file **only if it does not already exist**, so this is
    /// safe on every startup.  Per-file errors are logged and skipped.  `truecolor` picks
    /// the `theme` seeded into a freshly written `config.toml`.
    pub fn ensure_default_files(truecolor: bool) {
        let Some(dir) = Self::config_dir() else {
            tracing::warn!("no XDG config dir available; skipping default-file scaffolding");
            return;
        };
        ensure_default_files_in(&dir, truecolor);
    }

    /// edamame's directory under the platform data dir: `$XDG_DATA_HOME/edamame` or
    /// `~/.local/share/edamame` on Linux, `~/Library/Application Support/edamame` on macOS,
    /// `%APPDATA%\edamame` on Windows.  The single home for machine-written files — the tracing
    /// log ([`Self::log_dir`]) and [`State`]'s `state.toml`.
    pub fn data_dir() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("edamame"))
    }

    /// Returns the path to the log directory.
    pub fn log_dir() -> Option<PathBuf> {
        Self::data_dir()
    }

    /// Read one named theme from `themes/<name>.toml`, with any non-fatal warnings for
    /// the caller to surface in the same modal startup uses.  A missing file falls back to
    /// `Theme::default()`.  The live theme-change path, so the settings overlay validates
    /// through the same pipeline as [`Config::load`].
    pub fn load_theme(name: &str, truecolor: bool) -> (ThemeFile, Vec<ConfigWarning>) {
        let mut warnings = Vec::new();
        let theme_file = match Self::config_dir() {
            // The fallback signal is dropped: it only matters at startup, where
            // `Config::load` rewrites `config.toml`.  Here the substitution is transient,
            // so retrying recovers the original choice once the file reappears.
            Some(dir) => read_theme_named(&dir, name, truecolor, &mut warnings).0,
            None => (&Theme::default()).into(),
        };
        (theme_file, warnings)
    }
}

// ── config directory resolution ───────────────────────────────────────────────

/// Pure core of [`Config::config_dir`], taking its inputs as arguments so it is testable
/// without mutating the process environment.
fn resolve_config_dir(xdg: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    let base = match xdg {
        // An empty or relative value is invalid per the XDG spec; use `~/.config` rather
        // than resolving against the cwd.
        Some(v) if Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => home?.join(".config"),
    };
    Some(base.join("edamame"))
}

// ── save: comment-preserving merge ────────────────────────────────────────────

/// The TOML string [`Config::save`] writes: the user's file with only changed leaves
/// merged in (see [`merge_changed`]), so comments, blank lines, key order, and quoting all
/// survive.
///
/// With no file on disk the merge target is the compiled-in annotated
/// [`REFERENCE_CONFIG_TOML`] instead.  Emitting a bare `toml::to_string_pretty` would be a
/// one-way door — every later save merges into whatever is on disk, so one de-annotated
/// write strips the documentation forever.
fn save_merge(config: &Config, path: &Path) -> Result<String> {
    use toml_edit::DocumentMut;

    let new_serialized =
        toml::to_string_pretty(config).context("Failed to serialize config to TOML")?;

    let existing_raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        // First-write path: the shipped template, so the merge has comments to preserve.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => REFERENCE_CONFIG_TOML.to_string(),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("Failed to read existing config: {}", path.display()));
        }
    };

    let mut existing_doc: DocumentMut = existing_raw.parse().with_context(|| {
        format!(
            "Failed to parse existing config for in-place update: {}",
            path.display()
        )
    })?;
    // Rename any legacy section (`[diagrams]` → `[figures]`) before the
    // merge, so the in-memory values — serialized under the new name —
    // land in the user's own section instead of appending a second one.
    migrate_legacy_config_keys(&mut existing_doc);
    let new_doc: DocumentMut = new_serialized
        .parse()
        .context("internal error: serialized config failed to re-parse")?;
    let default_serialized =
        toml::to_string_pretty(&Config::default()).context("Failed to serialize default config")?;
    let default_doc: DocumentMut = default_serialized
        .parse()
        .context("internal error: default config failed to re-parse")?;

    merge_changed(
        existing_doc.as_table_mut(),
        new_doc.as_table(),
        default_doc.as_table(),
    );

    Ok(existing_doc.to_string())
}

// ── config-key migration ──────────────────────────────────────────────────────

/// Machine-written bookkeeping that once lived in `[editor]` and now lives in `state.toml` (see
/// [`State`]).  Stripped from `config.toml` by [`migrate_legacy_config_keys`], and — prefixed with
/// the `editor.` table — filtered out of the unknown-key warning by `read_main_config`, which
/// derives its dotted list from this one so there is a single source of truth.
pub(super) const BOOKKEEPING_KEYS_BARE: &[&str] = &[
    "seen_terminal_fingerprints",
    "last_update_check",
    "update_notified_for",
    "last_version_seen",
];

/// Bring `doc` up to the current `config.toml` spelling in place: rename the legacy `diagrams`
/// keys to `figures`, and strip the four [`BOOKKEEPING_KEYS_BARE`] that migrated to `state.toml`.
/// Returns `true` when anything changed.
///
/// The rename (when display math joined the consent gate) touches two places: the top-level
/// `[diagrams]` section and the `[export.html].diagrams` toggle; if both names exist at a level
/// (a hand-edited file), the new one wins.  Going through toml_edit keeps each entry's position
/// and comments, and handles `[ diagrams ]` with spaces and the nested key that a raw-text swap
/// could not.
fn migrate_legacy_config_keys(doc: &mut toml_edit::DocumentMut) -> bool {
    let mut changed = rename_table_key(doc.as_table_mut(), "diagrams", "figures");
    // The parallel `[export.html].diagrams` toggle migrates in place too; absent / non-table
    // `export`/`html` just means nothing to do.
    if let Some(export_html) = doc
        .get_mut("export")
        .and_then(toml_edit::Item::as_table_like_mut)
        .and_then(|export| export.get_mut("html"))
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        changed |= rename_table_key(export_html, "diagrams", "figures");
    }
    // Strip the bookkeeping keys now owned by `state.toml`; seeding has already read their values
    // (see `seed_state_from_legacy_config`), so removing them here is safe.
    if let Some(editor) = doc
        .get_mut("editor")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        for key in BOOKKEEPING_KEYS_BARE {
            changed |= remove_table_key(editor, key);
        }
    }
    changed
}

/// Remove `key` from one table in place, keeping the rest of the table's decor.  Returns whether
/// anything was removed.  The strip counterpart to [`rename_table_key`].
fn remove_table_key(table: &mut dyn toml_edit::TableLike, key: &str) -> bool {
    table.remove(key).is_some()
}

/// Seed `state.toml` from an existing `config.toml`'s legacy `[editor]` bookkeeping, once.
///
/// A no-op when [`State::path`] already exists (the migration is one-time, and an existing
/// `state.toml` must never be clobbered by a stale `config.toml`), when `config_path` is missing
/// or unparseable, or when the file carries none of the keys (a fresh install stays fileless until
/// edamame writes bookkeeping of its own).  Reads the raw toml_edit document because the four keys
/// no longer exist as `EditorConfig` fields.  Fail-soft: any write error is logged, not propagated.
/// The strip in [`migrate_config_file_in_place`] then runs regardless, so a failed seed drops the
/// values from disk — but they are re-derivable bookkeeping, so the cost is at worst a repeated
/// notice, never lost configuration.
fn seed_state_from_legacy_config(config_path: &Path) {
    // Gate on the real state-file location (the data dir), not the config dir.
    if State::path().is_none_or(|p| p.exists()) {
        return;
    }
    let Ok(raw) = std::fs::read_to_string(config_path) else {
        return;
    };
    let Ok(doc) = raw.parse::<toml_edit::DocumentMut>() else {
        return;
    };
    let Some(editor) = doc.get("editor").and_then(toml_edit::Item::as_table_like) else {
        return;
    };

    let mut state = State::default();
    let mut found = false;
    if let Some(v) = editor.get("last_update_check").and_then(|i| i.as_integer()) {
        state.last_update_check = v.max(0) as u64;
        found = true;
    }
    if let Some(v) = editor.get("update_notified_for").and_then(|i| i.as_str()) {
        state.update_notified_for = v.to_owned();
        found = true;
    }
    if let Some(v) = editor.get("last_version_seen").and_then(|i| i.as_str()) {
        state.last_version_seen = v.to_owned();
        found = true;
    }
    if let Some(arr) = editor
        .get("seen_terminal_fingerprints")
        .and_then(|i| i.as_array())
    {
        state.seen_terminal_fingerprints = arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        found = true;
    }
    if !found {
        return;
    }
    if let Err(e) = state.save() {
        tracing::warn!(error = %e, "failed to seed state.toml from legacy config");
    }
}

/// Rename key `from` to `to` within one table, in place, keeping value and decor.  No-op when
/// `from` is absent; when both exist, `to` is kept and `from` dropped.  Works on any
/// [`toml_edit::TableLike`] so the document table and the nested `[export.html]` share it.
fn rename_table_key(table: &mut dyn toml_edit::TableLike, from: &str, to: &str) -> bool {
    if !table.contains_key(from) {
        return false;
    }
    if table.contains_key(to) {
        table.remove(from);
    } else if let Some(item) = table.remove(from) {
        table.insert(to, item);
    }
    true
}

/// Rewrite `path` in place if it still uses a legacy section name (see
/// [`migrate_legacy_config_keys`]).  A no-op when the file is missing, unparseable, or already
/// current.  Failures are logged and non-fatal — the `alias` on [`Config`]'s field means the
/// session loaded correctly regardless.
fn migrate_config_file_in_place(path: &Path) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(mut doc) = raw.parse::<toml_edit::DocumentMut>() else {
        return;
    };
    if !migrate_legacy_config_keys(&mut doc) {
        return;
    }
    match std::fs::write(path, doc.to_string()) {
        Ok(()) => {
            tracing::info!(path = %path.display(), "migrated legacy keys in config.toml")
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "failed to migrate legacy config keys")
        }
    }
}

/// Merge `new` into `existing`, leaving comments and decor untouched.
///
/// Per key: sub-tables recurse; a key already present is overwritten in place (even when
/// it now equals the default, so a "change it and change it back" round-trip is faithful);
/// a key absent from `existing` is inserted only when it deviates from `defaults`, honoring
/// the reference config's "uncommented = deviation" convention.  A type mismatch is
/// replaced wholesale.
fn merge_changed(
    existing: &mut toml_edit::Table,
    new: &toml_edit::Table,
    defaults: &toml_edit::Table,
) {
    use toml_edit::{Item, Table};

    for (key, new_item) in new.iter() {
        let default_item = defaults.get(key);
        match new_item {
            Item::Table(new_tbl) => {
                let default_tbl: Table = default_item
                    .and_then(|i| i.as_table())
                    .cloned()
                    .unwrap_or_default();
                if let Some(Item::Table(exist_tbl)) = existing.get_mut(key) {
                    merge_changed(exist_tbl, new_tbl, &default_tbl);
                } else {
                    // Section missing from the user's file: attach a copy pruned to
                    // deviations, and only if anything survived.
                    let mut pruned = Table::new();
                    merge_changed(&mut pruned, new_tbl, &default_tbl);
                    if !pruned.is_empty() {
                        existing.insert(key, Item::Table(pruned));
                    }
                }
            }
            Item::Value(new_val) => match existing.get_mut(key) {
                Some(Item::Value(exist_val)) => {
                    // toml_edit stores decor *on* the value, so a naive assignment would
                    // drop the row's leading whitespace and trailing comment.
                    let prev_decor = exist_val.decor().clone();
                    let mut replacement = new_val.clone();
                    *replacement.decor_mut() = prev_decor;
                    *exist_val = replacement;
                }
                Some(_) => {
                    existing.insert(key, Item::Value(new_val.clone()));
                }
                None => {
                    let is_default = default_item
                        .and_then(|i| i.as_value())
                        .map(|d| value_canonically_equal(d, new_val))
                        .unwrap_or(false);
                    if !is_default {
                        existing.insert(key, Item::Value(new_val.clone()));
                    }
                }
            },
            Item::ArrayOfTables(arr) => {
                // Overwrite wholesale: there is no merge identity for array elements.
                if existing.contains_key(key)
                    || default_item.is_none_or(|d| {
                        d.as_array_of_tables()
                            .is_none_or(|d_arr| d_arr.to_string() != arr.to_string())
                    })
                {
                    existing.insert(key, Item::ArrayOfTables(arr.clone()));
                }
            }
            Item::None => {}
        }
    }
}

/// Equality on a value's canonical TOML text, which includes formatting but not row decor
/// — used to decide whether a leaf differs from the compiled default.
fn value_canonically_equal(a: &toml_edit::Value, b: &toml_edit::Value) -> bool {
    a.to_string().trim() == b.to_string().trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::persistence::SuppressGuard;

    #[test]
    fn default_config_is_valid() {
        let config = Config::default();
        assert_eq!(config.editor.mouse_scroll_lines, 1);
        assert!(!config.dev.logging);
        assert_eq!(config.modal.handler, "default");
        assert_eq!(config.theme, "Edamame");
    }

    /// The `--no-config` guarantee at the write site: a session that read nothing must
    /// write nothing.  The second write proves the first assertion is about the gate and
    /// not a misdirected path.
    #[test]
    fn save_writes_nothing_while_config_writes_are_suppressed() {
        let _lock = crate::test_env::env_lock();
        let dir = tempfile::tempdir().unwrap();
        // `save` resolves its own path from the environment.
        let _xdg = crate::test_env::EnvGuard::set("XDG_CONFIG_HOME", dir.path());

        let path = dir.path().join("edamame/config.toml");
        let config = Config {
            theme: "Nord".to_owned(),
            ..Config::default()
        };

        {
            let _suppressed = SuppressGuard::new();
            assert!(config.save().is_ok(), "a suppressed save is not a failure");
            assert!(!path.exists(), "--no-config must not create {path:?}");
        }

        config.save().expect("save ok");
        assert!(path.exists());
        assert!(std::fs::read_to_string(&path).unwrap().contains("Nord"));
    }

    /// An absolute root for the platform under test.  `/xdg` is only
    /// absolute on Unix — on Windows it lacks a drive letter, so
    /// [`resolve_config_dir`]'s `is_absolute` check would (correctly)
    /// reject it and the test would fail for the wrong reason.
    fn abs_root(name: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!(r"C:\{name}"))
        } else {
            PathBuf::from(format!("/{name}"))
        }
    }

    #[test]
    fn config_dir_prefers_absolute_xdg_config_home() {
        let xdg = abs_root("xdg");
        let dir = resolve_config_dir(Some(xdg.clone().into()), Some(abs_root("home")));
        assert_eq!(dir, Some(xdg.join("edamame")));
    }

    #[test]
    fn config_dir_falls_back_to_dot_config_on_every_platform() {
        // Unset, empty, and relative XDG values all fall back to `~/.config` — macOS
        // included, where `dirs::config_dir()` would give `~/Library/Application Support`.
        let home = abs_root("home");
        for xdg in [None, Some(OsString::from("")), Some(OsString::from("rel"))] {
            let dir = resolve_config_dir(xdg, Some(home.clone()));
            assert_eq!(dir, Some(home.join(".config").join("edamame")));
        }
    }

    #[test]
    fn config_dir_is_none_without_home_or_xdg() {
        assert_eq!(resolve_config_dir(None, None), None);
    }

    #[test]
    fn config_round_trips_toml() {
        let config = Config::default();
        let serialized = toml::to_string(&config).expect("serialize");
        let deserialized: Config = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(
            deserialized.editor.mouse_scroll_lines,
            config.editor.mouse_scroll_lines
        );
        assert_eq!(deserialized.modal.handler, config.modal.handler);
        assert_eq!(deserialized.theme, config.theme);
    }

    #[test]
    fn partial_toml_falls_back_to_defaults() {
        let toml = "[dev]\nlogging = true\n";
        let config: Config = toml::from_str(toml).expect("deserialize");
        assert!(config.dev.logging);
        assert_eq!(config.editor.mouse_scroll_lines, 1); // default
        assert_eq!(config.modal.handler, "default"); // default
        assert_eq!(config.theme, "Edamame"); // default
    }

    #[test]
    fn mouse_scroll_lines_default_is_one_and_round_trips() {
        let mut config = Config::default();
        assert_eq!(config.editor.mouse_scroll_lines, 1);
        config.editor.mouse_scroll_lines = 3;
        let serialized = toml::to_string(&config).expect("serialize");
        let deserialized: Config = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(deserialized.editor.mouse_scroll_lines, 3);
    }

    /// `check_for_updates` is a real user setting and stays on `EditorConfig`; the three
    /// bookkeeping fields it used to sit beside moved to `state.toml` (see `config::state`).
    #[test]
    fn check_for_updates_defaults_on_and_round_trips() {
        let mut config = Config::default();
        assert!(config.editor.check_for_updates);
        config.editor.check_for_updates = false;
        let serialized = toml::to_string(&config).expect("serialize");
        let deserialized: Config = toml::from_str(&serialized).expect("deserialize");
        assert!(!deserialized.editor.check_for_updates);
    }

    /// The switch is a real user setting and lives in `config.toml`; the positions it gates are
    /// machine bookkeeping in `state.toml`, and none of them is a `Config` field.
    #[test]
    fn remember_cursor_defaults_on_and_round_trips() {
        let mut config = Config::default();
        assert!(config.editor.remember_cursor);
        config.editor.remember_cursor = false;
        let serialized = toml::to_string(&config).expect("serialize");
        let deserialized: Config = toml::from_str(&serialized).expect("deserialize");
        assert!(!deserialized.editor.remember_cursor);
    }

    #[test]
    fn theme_name_round_trips() {
        let toml = r#"theme = "catppuccin"

[editor]
"#;
        let config: Config = toml::from_str(toml).expect("deserialize");
        assert_eq!(config.theme, "catppuccin");
    }

    // ── Readers ────────────────────────────────────────────────────────────

    #[test]
    fn read_main_config_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut warnings = Vec::new();
        let config = read_main_config(&path, &mut warnings);
        assert_eq!(config.theme, "Edamame");
        assert_eq!(config.editor.mouse_scroll_lines, 1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn read_keybindings_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybindings.toml");
        let mut warnings = Vec::new();
        let binds = read_keybindings(&path, &mut warnings);
        assert!(binds.0.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn read_theme_missing_default_falls_back_to_edamame_on_truecolor() {
        let _lock = crate::test_env::env_lock();
        // `default` is a historical name still in some older `config.toml` files.  Not a
        // built-in, so the loader takes the missing-file path and substitutes `Edamame`.
        let dir = tempfile::tempdir().unwrap();
        let mut warnings = Vec::new();
        let (theme, fallback) = read_theme_named(dir.path(), "default", true, &mut warnings);
        assert_eq!(fallback.as_deref(), Some("Edamame"));
        let theme_out: Theme = (&theme).into();
        assert_eq!(theme_out.h1, Theme::builtin("Edamame").unwrap().h1);
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            &warnings[0].kind,
            WarningKind::MissingTheme { requested, fallback }
                if requested == "default" && fallback == "Edamame"
        ));
    }

    #[test]
    fn read_theme_missing_named_falls_back_to_256_dark_without_truecolor() {
        let _lock = crate::test_env::env_lock();
        // The truecolor fallback's RGB values would degrade on a 256-color emulator.
        let dir = tempfile::tempdir().unwrap();
        let mut warnings = Vec::new();
        let (theme, fallback) = read_theme_named(dir.path(), "nonexistent", false, &mut warnings);
        assert_eq!(fallback.as_deref(), Some("256 Dark"));
        let theme_out: Theme = (&theme).into();
        assert_eq!(theme_out.h1, Theme::builtin("256 Dark").unwrap().h1);
    }

    #[test]
    fn read_theme_empty_file_stays_empty() {
        let _lock = crate::test_env::env_lock();
        // Distinct from the missing-file case: an emptied file is the user's choice.  The
        // name is non-built-in so the disk file is actually consulted.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        std::fs::write(dir.path().join("themes").join("custom.toml"), "").unwrap();
        let mut warnings = Vec::new();
        let (theme, fallback) = read_theme_named(dir.path(), "custom", true, &mut warnings);
        assert_eq!(theme.h1, super::super::theme_file::StyleSpec::default());
        assert!(fallback.is_none());
        assert!(warnings.is_empty());
    }

    #[test]
    fn read_theme_builtin_wins_over_user_file() {
        let _lock = crate::test_env::env_lock();
        // A file at `themes/<builtin>.toml` is ignored entirely; the escape hatch is to
        // pick a different name.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        std::fs::write(
            dir.path().join("themes").join("256 Dark.toml"),
            "[h1]\nfg = \"red\"\n",
        )
        .unwrap();
        let mut warnings = Vec::new();
        let (theme, fallback) = read_theme_named(dir.path(), "256 Dark", true, &mut warnings);
        let theme_out: Theme = (&theme).into();
        assert_eq!(theme_out.h1, Theme::default().h1);
        assert!(fallback.is_none());
        assert!(warnings.is_empty());
    }

    #[test]
    fn read_theme_resolves_light_builtin() {
        let _lock = crate::test_env::env_lock();
        use super::super::themes::light_256;
        let dir = tempfile::tempdir().unwrap();
        let mut warnings = Vec::new();
        let (theme, _) = read_theme_named(dir.path(), "256 Light", true, &mut warnings);
        let theme_out: Theme = (&theme).into();
        let expected = Theme::from_palette(&light_256::palette());
        assert_eq!(theme_out.h1, expected.h1);
        assert_eq!(theme_out.normal, expected.normal);
        assert!(warnings.is_empty());
    }

    #[test]
    fn loaded_config_default_has_themed_default_not_empty() {
        // The editor must stay themed even when `Config::load` itself failed.
        let fallback = LoadedConfig::default();
        let theme: Theme = (&fallback.theme).into();
        assert_eq!(theme.h1, Theme::default().h1);
    }

    #[test]
    fn read_main_config_parses_valid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "theme = \"solarized\"\n\n[editor]\nmouse_scroll_lines = 2\n",
        )
        .unwrap();
        let mut warnings = Vec::new();
        let config = read_main_config(&path, &mut warnings);
        assert_eq!(config.theme, "solarized");
        assert_eq!(config.editor.mouse_scroll_lines, 2);
        assert!(warnings.is_empty());
    }

    #[test]
    fn read_keybindings_parses_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybindings.toml");
        std::fs::write(&path, "Quit = \"ctrl+x\"\n").unwrap();
        let mut warnings = Vec::new();
        let binds = read_keybindings(&path, &mut warnings);
        assert_eq!(binds.0.get("Quit"), Some(&"ctrl+x".to_string()));
        assert!(warnings.is_empty());
    }

    #[test]
    fn read_theme_parses_from_named_file() {
        let _lock = crate::test_env::env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        let theme_path = dir.path().join("themes").join("custom.toml");
        std::fs::write(&theme_path, "[h1]\nfg = \"red\"\nbold = true\n").unwrap();
        let mut warnings = Vec::new();
        let (theme, _) = read_theme_named(dir.path(), "custom", true, &mut warnings);
        assert!(theme.h1.bold);
        assert!(warnings.is_empty());
    }

    // ── Warning paths ──────────────────────────────────────────────────────

    #[test]
    fn read_main_config_parse_error_warns_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[editor]\nmouse_scroll_lines = \"oops\"\n").unwrap();
        let mut warnings = Vec::new();
        let config = read_main_config(&path, &mut warnings);
        assert_eq!(config.editor.mouse_scroll_lines, 1);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].path, path);
        match &warnings[0].kind {
            WarningKind::ParseError(msg) => {
                assert!(msg.contains("line 2") || msg.contains('2'), "{msg}");
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn read_main_config_rejects_autosave_idle_below_floor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[editor]\nautosave_idle_ms = 500\n").unwrap();
        let mut warnings = Vec::new();
        let config = read_main_config(&path, &mut warnings);
        assert_eq!(
            config.editor.autosave_idle_ms,
            EditorConfig::default().autosave_idle_ms,
            "out-of-range value must be replaced with the default",
        );
        assert_eq!(warnings.len(), 1);
        match &warnings[0].kind {
            WarningKind::InvalidValue { key, message } => {
                assert_eq!(key, "editor.autosave_idle_ms");
                assert!(
                    message.contains("500"),
                    "msg should cite bad value: {message}"
                );
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn read_main_config_rejects_autosave_idle_at_floor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[editor]\nautosave_idle_ms = 1000\n").unwrap();
        let mut warnings = Vec::new();
        let config = read_main_config(&path, &mut warnings);
        assert_eq!(
            config.editor.autosave_idle_ms,
            EditorConfig::default().autosave_idle_ms,
        );
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn read_main_config_rejects_autosave_idle_at_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[editor]\nautosave_idle_ms = 600000\n").unwrap();
        let mut warnings = Vec::new();
        let config = read_main_config(&path, &mut warnings);
        assert_eq!(
            config.editor.autosave_idle_ms,
            EditorConfig::default().autosave_idle_ms,
        );
        assert_eq!(warnings.len(), 1);
        match &warnings[0].kind {
            WarningKind::InvalidValue { .. } => {}
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn read_main_config_accepts_autosave_idle_in_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[editor]\nautosave_idle_ms = 2500\n").unwrap();
        let mut warnings = Vec::new();
        let config = read_main_config(&path, &mut warnings);
        assert_eq!(config.editor.autosave_idle_ms, 2500);
        assert!(warnings.is_empty());
    }

    #[test]
    fn read_main_config_unknown_key_warns_but_keeps_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // `bogus_top` sits above `[editor]`, and TOML associates a key with the most
        // recent table header.
        std::fs::write(
            &path,
            "bogus_top = true\n\n[editor]\nmouse_scroll_lines = 2\nmouse_scroll_linez = 8\n",
        )
        .unwrap();
        let mut warnings = Vec::new();
        let config = read_main_config(&path, &mut warnings);
        assert_eq!(config.editor.mouse_scroll_lines, 2);
        assert_eq!(warnings.len(), 1);
        match &warnings[0].kind {
            WarningKind::UnknownKeys(keys) => {
                assert!(
                    keys.iter().any(|k| k == "editor.mouse_scroll_linez"),
                    "missing nested key: {keys:?}"
                );
                assert!(
                    keys.iter().any(|k| k == "bogus_top"),
                    "missing top-level key: {keys:?}"
                );
            }
            other => panic!("expected UnknownKeys, got {other:?}"),
        }
    }

    /// The four legacy bookkeeping keys (now in `state.toml`) are filtered out of the unknown-key
    /// warning, but a genuinely unknown key beside them still warns — so the filter stays narrow.
    #[test]
    fn legacy_bookkeeping_keys_do_not_warn_but_real_unknowns_still_do() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[editor]\n\
             last_update_check = 1\n\
             update_notified_for = \"v1\"\n\
             last_version_seen = \"0.1\"\n\
             seen_terminal_fingerprints = [\"a\"]\n\
             mouse_scroll_linez = 8\n",
        )
        .unwrap();
        let mut warnings = Vec::new();
        let _config = read_main_config(&path, &mut warnings);
        assert_eq!(
            warnings.len(),
            1,
            "only the genuine unknown key should warn: {warnings:?}"
        );
        match &warnings[0].kind {
            WarningKind::UnknownKeys(keys) => {
                assert_eq!(keys, &vec!["editor.mouse_scroll_linez".to_string()]);
            }
            other => panic!("expected UnknownKeys, got {other:?}"),
        }
    }

    #[test]
    fn read_keybindings_strips_invalid_entries_and_warns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybindings.toml");
        std::fs::write(
            &path,
            "Quit = \"ctrl+x\"\nQuitt = \"ctrl+y\"\nSave = \"banana+z\"\n",
        )
        .unwrap();
        let mut warnings = Vec::new();
        let binds = read_keybindings(&path, &mut warnings);
        assert_eq!(binds.0.get("Quit"), Some(&"ctrl+x".to_string()));
        assert!(!binds.0.contains_key("Quitt"));
        assert!(!binds.0.contains_key("Save"));
        assert_eq!(warnings.len(), 1);
        match &warnings[0].kind {
            WarningKind::InvalidKeybindings(errs) => {
                assert_eq!(errs.len(), 2);
                assert!(errs.iter().any(|e| e.contains("Quitt")));
                assert!(errs.iter().any(|e| e.contains("Save")));
            }
            other => panic!("expected InvalidKeybindings, got {other:?}"),
        }
    }

    #[test]
    fn read_theme_unknown_key_warns_but_keeps_value() {
        let _lock = crate::test_env::env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        let theme_path = dir.path().join("themes").join("custom.toml");
        std::fs::write(&theme_path, "[h1]\nfg = \"red\"\n\n[h7]\nfg = \"blue\"\n").unwrap();
        let mut warnings = Vec::new();
        let (theme, _) = read_theme_named(dir.path(), "custom", true, &mut warnings);
        assert_eq!(
            theme.h1.fg,
            Some(super::super::theme_file::ColorField::Named(
                ratatui::style::Color::Red
            ))
        );
        assert_eq!(warnings.len(), 1);
        match &warnings[0].kind {
            WarningKind::UnknownKeys(keys) => assert!(keys.iter().any(|k| k.starts_with("h7"))),
            other => panic!("expected UnknownKeys, got {other:?}"),
        }
    }

    #[test]
    fn read_theme_parse_error_warns_and_falls_back_to_compiled_theme() {
        let _lock = crate::test_env::env_lock();
        // Non-built-in name so the disk file is actually consulted.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        let theme_path = dir.path().join("themes").join("custom.toml");
        std::fs::write(&theme_path, "[h1]\nfg = 42\nbold = \"oops\"\n").unwrap();
        let mut warnings = Vec::new();
        let (theme, fallback) = read_theme_named(dir.path(), "custom", true, &mut warnings);
        let theme_out: Theme = (&theme).into();
        assert_eq!(theme_out.h1, Theme::default().h1);
        assert!(fallback.is_none());
        assert_eq!(warnings.len(), 1);
        assert!(matches!(warnings[0].kind, WarningKind::ParseError(_)));
    }

    // ── ensure_default_files ───────────────────────────────────────────────

    #[test]
    fn ensure_default_files_writes_config_and_keybindings_but_not_themes() {
        let _lock = crate::test_env::env_lock();
        // A scaffolded `themes/<builtin>.toml` would be inert (the built-in always wins)
        // and misleading to edit.  The directory itself is still created.
        let dir = tempfile::tempdir().unwrap();
        ensure_default_files_in(dir.path(), true);
        assert!(dir.path().join("config.toml").exists());
        assert!(dir.path().join("keybindings.toml").exists());
        assert!(dir.path().join("themes").is_dir());
        assert!(!dir.path().join("themes").join("default.toml").exists());
        // Seeded with a `.example` to fork, but no selectable `default.css` — the one
        // default is compiled in.
        assert!(dir.path().join("export").is_dir());
        let export = dir.path().join("export");
        assert!(
            !export.join("default.css").exists(),
            "no selectable default.css — would duplicate the compiled-in Builtin"
        );
        let css = std::fs::read_to_string(export.join("default.css.example"))
            .expect("default.css.example scaffolded");
        assert!(css.contains("markdown-body"), "bundled stylesheet body");
        assert!(
            crate::config::list_export_stylesheets(dir.path()).is_empty(),
            "the .example reference must not appear as a stylesheet pick"
        );
    }

    #[test]
    fn ensure_default_files_seeds_256_dark_without_truecolor() {
        // The truecolor default's RGB palette quantizes badly on an indexed terminal.
        // Only the theme assignment differs; the seeded file is still the reference.
        let dir = tempfile::tempdir().unwrap();
        ensure_default_files_in(dir.path(), false);
        let seeded = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(seeded.contains("theme = \"256 Dark\""));
        assert!(!seeded.contains("theme = \"Edamame\""));
        assert!(
            seeded.contains("# Active theme"),
            "annotations survive the swap"
        );
        let mut warnings = Vec::new();
        let config = read_main_config(&dir.path().join("config.toml"), &mut warnings);
        assert_eq!(config.theme, "256 Dark");
        assert!(warnings.is_empty());
    }

    #[test]
    fn ensure_default_files_is_idempotent_and_preserves_user_edits() {
        let dir = tempfile::tempdir().unwrap();
        ensure_default_files_in(dir.path(), true);

        let config_path = dir.path().join("config.toml");
        let custom = "# user-edited\ntheme = \"light\"\n";
        std::fs::write(&config_path, custom).unwrap();

        ensure_default_files_in(dir.path(), true);
        let after = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(after, custom, "user-edited config was overwritten");
    }

    // ── save invariant ─────────────────────────────────────────────────────

    /// A save must never emit `[keybindings]` / `[theme_file]` / style sections; those
    /// fields no longer live on the struct, and a round-trip is the clearest assertion.
    #[test]
    fn save_serialization_only_contains_config_fields() {
        let config = Config::default();
        let serialized = toml::to_string_pretty(&config).expect("serialize");
        assert!(!serialized.contains("[keybindings]"));
        assert!(!serialized.contains("[h1]"));
        assert!(!serialized.contains("[h2]"));
        assert!(serialized.contains("theme ="));
        assert!(serialized.contains("[editor]"));
        assert!(serialized.contains("[modal]"));
        assert!(serialized.contains("[images]"));
        // The screen-consent section is `[figures]` (renamed from
        // `[diagrams]`; see the field's serde rename).
        assert!(serialized.contains("[figures]"));
        assert!(!serialized.contains("[diagrams]"));
        assert!(serialized.contains("[export"));
        assert!(serialized.contains("[dev]"));
    }

    #[test]
    fn export_config_defaults_and_round_trip() {
        let config = Config::default();
        assert_eq!(config.export.html.stylesheet, "builtin");
        assert!(!config.export.html.inline_images);
        assert!(config.export.html.figures);
        assert!(config.export.custom.is_empty());

        let toml_str = r#"
[[export.custom]]
name = "PDF (weasyprint)"
command = ["weasyprint", "{html}", "{out}"]
extension = "pdf"
"#;
        let config: Config = toml::from_str(toml_str).expect("deserialize");
        assert_eq!(config.export.custom.len(), 1);
        assert_eq!(config.export.custom[0].name, "PDF (weasyprint)");
        assert_eq!(config.export.custom[0].extension, "pdf");
        assert_eq!(config.export.custom[0].command.len(), 3);
    }

    /// A config written before display-math export existed uses
    /// `[export.html].diagrams`; the `alias` keeps it loading onto the
    /// renamed `figures` field.
    #[test]
    fn legacy_export_diagrams_key_loads_via_alias() {
        let config: Config =
            toml::from_str("[export.html]\ndiagrams = false\n").expect("legacy export key parses");
        assert!(
            !config.export.html.figures,
            "legacy [export.html].diagrams must map onto figures"
        );
    }

    // ── save_merge: comment-preserving in-place update ─────────────────────

    /// An unchanged config round-trips verbatim: comments survive, no clutter appended.
    /// A session-only downgrade must never reach disk: the same `config.toml` is usually
    /// shared with a truecolor terminal where the user's own theme is correct.
    #[test]
    fn downgraded_theme_is_not_written_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "theme = \"Dracula\"\n").unwrap();
        let config = Config {
            theme: "256 Dark".into(),
            theme_downgraded_from: Some("Dracula".into()),
            ..Config::default()
        };
        let out = save_merge(&config.as_written(), &path).expect("merge ok");
        assert!(out.contains("Dracula"), "user's theme must survive: {out}");
        assert!(!out.contains("256 Dark"), "downgrade must not leak: {out}");
    }

    /// Without a stash the theme is written normally; the restore is scoped to the
    /// downgrade.
    #[test]
    fn undowngraded_theme_is_written_normally() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "theme = \"Dracula\"\n").unwrap();
        let config = Config {
            theme: "Nord".into(),
            ..Config::default()
        };
        let out = save_merge(&config.as_written(), &path).expect("merge ok");
        assert!(out.contains("Nord"), "{out}");
    }

    /// An explicit pick clears the stash and reaches disk like any other theme change.
    #[test]
    fn set_theme_clears_the_downgrade_stash() {
        let mut config = Config {
            theme: "256 Dark".into(),
            theme_downgraded_from: Some("Dracula".into()),
            ..Config::default()
        };
        config.set_theme("256 Light".into());
        assert_eq!(config.theme, "256 Light");
        assert!(config.theme_downgraded_from.is_none());
        assert_eq!(config.as_written().theme, "256 Light");
    }

    #[test]
    fn outline_preference_defaults_off_and_round_trips_through_merge() {
        let missing: Config = toml::from_str("[editor]\n").unwrap();
        assert!(!missing.editor.show_outline);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[editor]\n# retained\nshow_outline = false\n").unwrap();
        let mut config = Config::default();
        config.editor.show_outline = true;
        let merged = save_merge(&config, &path).unwrap();
        assert!(merged.contains("# retained"));
        assert!(merged.contains("show_outline = true"));
        let restored: Config = toml::from_str(&merged).unwrap();
        assert!(restored.editor.show_outline);
    }

    #[test]
    fn save_merge_unchanged_config_preserves_file_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let annotated = "\
# top-of-file comment that must survive
theme = \"Edamame\" # trailing comment on theme
appearance = \"dark\"

[editor]
# code_block_wrap = false

[table]
show_buttons = true
";
        std::fs::write(&path, annotated).unwrap();
        let config = Config::default();
        let out = save_merge(&config, &path).expect("merge ok");
        assert!(out.contains("# top-of-file comment that must survive"));
        assert!(out.contains("# trailing comment on theme"));
        assert!(out.contains("# code_block_wrap = false"));
        assert!(!out.contains("transient_ms"));
        assert!(!out.contains("mouse_scroll_lines"));
    }

    /// A flagged-but-malformed `[[export.custom]]` block must survive an ordinary save: a
    /// bookkeeping save fires within seconds of launch, and erasing the block would delete
    /// the very lines the startup warning asked the user to fix.
    #[test]
    fn save_merge_preserves_a_custom_export_block_flagged_by_the_validator() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "theme = \"Nord\"\n\n\
             # my PDF converter\n\
             [[export.custom]]\n\
             name = \"PDF\"\n\
             command = [\"weasyprint\", \"{html}\", \"{out}\"]\n\
             # extension is missing on purpose — the validator warns but keeps it\n",
        )
        .unwrap();

        // Validation is non-mutating, so the in-memory config still carries the entry.
        let config = Config {
            theme: "Nord".to_owned(),
            export: ExportConfig {
                custom: vec![CustomExportEntry {
                    name: "PDF".to_owned(),
                    command: vec!["weasyprint".into(), "{html}".into(), "{out}".into()],
                    extension: String::new(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        let out = save_merge(&config, &path).expect("merge ok");
        assert!(
            out.contains("[[export.custom]]") && out.contains("name = \"PDF\""),
            "the user's custom-export block must not be erased by a save:\n{out}"
        );
    }

    /// Changing an existing key replaces just the value; its trailing comment stays.
    #[test]
    fn save_merge_replaces_existing_value_in_place_preserving_decor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let annotated = "\
theme = \"Edamame\" # active theme
appearance = \"dark\"
";
        std::fs::write(&path, annotated).unwrap();
        let config = Config {
            theme: "catppuccin".to_string(),
            ..Config::default()
        };
        let out = save_merge(&config, &path).expect("merge ok");
        assert!(out.contains("theme = \"catppuccin\""));
        assert!(out.contains("# active theme"));
    }

    // ── [diagrams] → [figures] rename + migration ─────────────────────

    /// An old config written with `[diagrams]` still loads: the `alias`
    /// on the field keeps deserialization working so an un-migrated file
    /// (read-only, `--no-config`) runs correctly.
    #[test]
    fn legacy_diagrams_section_deserializes_via_alias() {
        let config: Config =
            toml::from_str("[diagrams]\nenabled = \"never\"\n").expect("legacy config parses");
        assert_eq!(config.figures.enabled, FiguresEnabled::Never);
    }

    /// The current section name is `[figures]`: a serialized config uses
    /// it, not the legacy `[diagrams]`.
    #[test]
    fn config_serializes_the_figures_section_name() {
        let config = Config {
            figures: FiguresConfig {
                enabled: FiguresEnabled::Always,
                ..FiguresConfig::default()
            },
            ..Config::default()
        };
        let out = toml::to_string_pretty(&config).expect("serialize");
        assert!(out.contains("[figures]"), "expected [figures] in:\n{out}");
        assert!(!out.contains("[diagrams]"), "legacy name leaked:\n{out}");
    }

    /// The unit rename preserves the section's value and drops the old key.
    #[test]
    fn migrate_legacy_config_keys_renames_diagrams_to_figures() {
        use toml_edit::DocumentMut;
        // A realistic multi-section file: the rename must touch only the
        // header, keep the section *in place* (between [images] and
        // [export.html]), and preserve every comment around it.
        let src = "theme = \"Nord\"\n\n\
                   [images]\nenabled = \"always\"\n\n\
                   # ── Diagrams ──\n[diagrams]\n# master switch\nenabled = \"always\" # mine\n\n\
                   [export.html]\ndiagrams = true\n";
        let mut doc: DocumentMut = src.parse().unwrap();
        assert!(migrate_legacy_config_keys(&mut doc));
        let out = doc.to_string();
        assert!(out.contains("[figures]"), "not renamed:\n{out}");
        // Both levels migrate: the top-level header AND the nested export
        // toggle, with no stray `diagrams` spelling left anywhere.
        assert!(!out.contains("[diagrams]"), "old header kept:\n{out}");
        assert!(
            out.contains("figures = true"),
            "nested export toggle not migrated:\n{out}"
        );
        assert!(
            !out.contains("diagrams = true"),
            "legacy export toggle survived:\n{out}"
        );
        // Comments and value survive verbatim.
        assert!(
            out.contains("# ── Diagrams ──"),
            "header comment lost:\n{out}"
        );
        assert!(out.contains("# master switch"), "body comment lost:\n{out}");
        assert!(
            out.contains("enabled = \"always\" # mine"),
            "value/comment lost:\n{out}"
        );
        // Position preserved: [figures] stays between [images] and [export.html].
        let f = out.find("[figures]").unwrap();
        assert!(out.find("[images]").unwrap() < f && f < out.find("[export.html]").unwrap());
        // Idempotent: a file already on the new name is untouched.
        let mut current: DocumentMut = "[figures]\nenabled = \"ask\"\n".parse().unwrap();
        assert!(!migrate_legacy_config_keys(&mut current));
    }

    /// A save over a legacy file migrates the header AND lands the
    /// in-memory value in the migrated section (not a second `[figures]`).
    #[test]
    fn save_merge_migrates_a_legacy_diagrams_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "theme = \"Edamame\"\n\n[diagrams]\nenabled = \"always\"\n",
        )
        .unwrap();
        let config = Config {
            figures: FiguresConfig {
                enabled: FiguresEnabled::Always,
                ..FiguresConfig::default()
            },
            ..Config::default()
        };
        let out = save_merge(&config, &path).expect("merge ok");
        assert!(out.contains("[figures]"), "not migrated:\n{out}");
        assert!(
            !out.contains("[diagrams]"),
            "duplicate/legacy section:\n{out}"
        );
        assert!(out.contains("enabled = \"always\""), "value lost:\n{out}");
        // Round-trips back to the same value.
        let round: Config = toml::from_str(&out).expect("parses");
        assert_eq!(round.figures.enabled, FiguresEnabled::Always);
    }

    /// The eager in-place migration rewrites the file and is a no-op the
    /// second time.
    #[test]
    fn migrate_config_file_in_place_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[diagrams]\nenabled = \"never\"\n").unwrap();
        migrate_config_file_in_place(&path);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("[figures]") && !after.contains("[diagrams]"));
        migrate_config_file_in_place(&path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            after,
            "second run changed the file"
        );
    }

    // ── bookkeeping → state.toml migration ─────────────────────────────

    /// The migration strips the four `[editor]` bookkeeping keys while leaving real settings and
    /// their comments untouched, and is idempotent on an already-clean file.
    #[test]
    fn strip_removes_the_four_editor_keys_preserving_others() {
        use toml_edit::DocumentMut;
        let src = "theme = \"Nord\"\n\n\
                   [editor]\n\
                   # keep this\n\
                   line_wrap = true # trailing\n\
                   last_update_check = 1\n\
                   update_notified_for = \"v1\"\n\
                   last_version_seen = \"0.1\"\n\
                   seen_terminal_fingerprints = [\"a\"]\n";
        let mut doc: DocumentMut = src.parse().unwrap();
        assert!(migrate_legacy_config_keys(&mut doc));
        let out = doc.to_string();
        for k in BOOKKEEPING_KEYS_BARE {
            assert!(!out.contains(k), "bookkeeping key `{k}` survived:\n{out}");
        }
        assert!(out.contains("line_wrap = true # trailing"), "{out}");
        assert!(out.contains("# keep this"), "{out}");
        // Already clean → no change.
        let mut clean: DocumentMut = "[editor]\nline_wrap = true\n".parse().unwrap();
        assert!(!migrate_legacy_config_keys(&mut clean));
    }

    /// Seeding copies the legacy values into `state.toml`, then is a one-time no-op that never
    /// clobbers the now-authoritative state file.  Linux-only: `dirs::data_dir()` honors
    /// `XDG_DATA_HOME` there, so `State`'s real read/write can be redirected to a tempdir.
    #[cfg(target_os = "linux")]
    #[test]
    fn seed_copies_legacy_values_then_is_idempotent() {
        let _lock = crate::test_env::env_lock();
        let cfg_dir = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let _xdg = crate::test_env::EnvGuard::set("XDG_DATA_HOME", data_dir.path());

        let config_path = cfg_dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "theme = \"Nord\"\n\n[editor]\n\
             last_update_check = 123\n\
             last_version_seen = \"0.1.9\"\n\
             seen_terminal_fingerprints = [\"fp1\"]\n",
        )
        .unwrap();

        seed_state_from_legacy_config(&config_path);
        let state = State::load();
        assert_eq!(state.last_update_check, 123);
        assert_eq!(state.last_version_seen, "0.1.9");
        assert_eq!(state.seen_terminal_fingerprints, vec!["fp1".to_string()]);

        // A different config no longer reaches state: the file exists, so seeding stands down.
        std::fs::write(&config_path, "[editor]\nlast_version_seen = \"9.9.9\"\n").unwrap();
        seed_state_from_legacy_config(&config_path);
        assert_eq!(State::load().last_version_seen, "0.1.9");
    }

    /// End to end through `Config::load`: a legacy `config.toml` migrates its bookkeeping into
    /// `state.toml`, the keys are stripped from `config.toml` (comments intact), the returned
    /// `LoadedConfig` carries the state, and no unknown-key warning fires.  Linux-only for the
    /// same `XDG_DATA_HOME` reason as the seed tests.
    #[cfg(target_os = "linux")]
    #[test]
    fn config_load_migrates_legacy_bookkeeping_end_to_end() {
        let _lock = crate::test_env::env_lock();
        let cfg_dir = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let _xcfg = crate::test_env::EnvGuard::set("XDG_CONFIG_HOME", cfg_dir.path());
        let _xdata = crate::test_env::EnvGuard::set("XDG_DATA_HOME", data_dir.path());

        let config_path = cfg_dir.path().join("edamame/config.toml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            "theme = \"Edamame\"\n\n[editor]\n\
             # a real setting\n\
             line_wrap = true\n\
             last_update_check = 777\n\
             last_version_seen = \"0.1.2\"\n\
             seen_terminal_fingerprints = [\"fp\"]\n",
        )
        .unwrap();

        let loaded = Config::load(true, true).expect("load ok");
        assert_eq!(loaded.state.last_update_check, 777);
        assert_eq!(loaded.state.last_version_seen, "0.1.2");
        assert_eq!(
            loaded.state.seen_terminal_fingerprints,
            vec!["fp".to_string()]
        );
        assert!(
            loaded.warnings.is_empty(),
            "no warning: {:?}",
            loaded.warnings
        );

        let on_disk = std::fs::read_to_string(&config_path).unwrap();
        for key in BOOKKEEPING_KEYS_BARE {
            assert!(!on_disk.contains(key), "`{key}` not stripped:\n{on_disk}");
        }
        assert!(
            on_disk.contains("# a real setting"),
            "comment lost:\n{on_disk}"
        );
        assert!(
            on_disk.contains("line_wrap = true"),
            "setting lost:\n{on_disk}"
        );
        assert!(data_dir.path().join("edamame/state.toml").exists());
    }

    /// A fresh install (no bookkeeping keys in config) writes no `state.toml`.
    #[cfg(target_os = "linux")]
    #[test]
    fn seed_writes_nothing_when_no_legacy_keys_present() {
        let _lock = crate::test_env::env_lock();
        let cfg_dir = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let _xdg = crate::test_env::EnvGuard::set("XDG_DATA_HOME", data_dir.path());

        let config_path = cfg_dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "theme = \"Nord\"\n\n[editor]\nline_wrap = true\n",
        )
        .unwrap();
        seed_state_from_legacy_config(&config_path);
        assert!(!data_dir.path().join("edamame/state.toml").exists());
    }

    /// A non-default value for an absent key is inserted; default-valued siblings are not.
    #[test]
    fn save_merge_inserts_non_default_skips_default_when_key_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let annotated = "\
theme = \"Edamame\"
appearance = \"dark\"

[editor]
# mouse_scroll_lines = 1
";
        std::fs::write(&path, annotated).unwrap();
        let mut config = Config::default();
        config.editor.mouse_scroll_lines = 3;
        let out = save_merge(&config, &path).expect("merge ok");
        assert!(out.contains("# mouse_scroll_lines = 1"));
        assert!(out.contains("mouse_scroll_lines = 3"));
        assert!(!out.contains("transient_ms = 1500"));
        assert!(!out.contains("max_width_cols = 80"));
    }

    /// With no existing file the merge target is the annotated reference config, so the
    /// emitted file keeps its documentation.
    #[test]
    fn save_merge_first_write_emits_annotated_reference() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config::default();
        let out = save_merge(&config, &path).expect("merge ok");
        let _round: Config = toml::from_str(&out).expect("parses");
        assert!(out.contains("theme ="));
        assert!(out.contains("# edamame configuration"));
        assert!(!out.contains("\ntransient_ms ="));
    }

    /// Deviating values still land in an annotated file when `config.toml` was deleted
    /// under a running session — one bare write would strip the comments permanently.
    #[test]
    fn save_merge_first_write_keeps_comments_with_non_default_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config {
            theme: "Dracula".into(),
            modal: ModalConfig {
                handler: "vim".into(),
            },
            ..Default::default()
        };
        let out = save_merge(&config, &path).expect("merge ok");
        let round: Config = toml::from_str(&out).expect("parses");
        assert_eq!(round.theme, "Dracula");
        assert_eq!(round.modal.handler, "vim");
        assert!(out.contains("# edamame configuration"));
        assert!(out.contains("# Active theme"));
    }

    /// A key the user set explicitly is rewritten even when it is back at the default —
    /// never silently dropped.
    #[test]
    fn save_merge_overwrites_existing_key_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "theme = \"Edamame\"\nappearance = \"dark\"\n\n[editor]\nmouse_scroll_lines = 3 # explicit\n",
        )
        .unwrap();
        let config = Config::default(); // mouse_scroll_lines = 1
        let out = save_merge(&config, &path).expect("merge ok");
        assert!(out.contains("mouse_scroll_lines = 1"));
        assert!(out.contains("# explicit"));
    }
}
