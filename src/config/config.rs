use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::init::{ensure_default_files_in, REFERENCE_CONFIG_TOML};
use super::keymap::KeyBindingOverrides;
use super::persistence::config_writes_allowed;
use super::readers::{read_keybindings, read_main_config, read_theme_named};
pub use super::sections::{
    AppearanceMode, CustomExportEntry, DevConfig, DiagramsConfig, DiagramsEnabled, EditorConfig,
    ExportConfig, ImagesConfig, ImagesEnabled, ModalConfig, RemoteImagePolicy, TableConfig,
};
use super::theme::Theme;
use super::theme_file::ThemeFile;
pub use super::warnings::{ConfigWarning, WarningKind};

/// Top-level `config.toml` — editor/rendering settings and the name of the
/// active theme.  Keybinding overrides live in `keybindings.toml`; theme
/// style tables live in `themes/<name>.toml`.  See `LoadedConfig` for the
/// orchestrator that reads all three.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Name of the active theme.  Built-in names (see `BUILTIN_THEMES`)
    /// resolve to a compiled-in palette; any other name is loaded from
    /// `themes/<theme>.toml`.  Defaults to `"Edamame"`.  A missing file
    /// falls back to the compiled-in `Theme::default()` so the editor
    /// always has a working color table.
    pub theme: String,
    /// Session-only stash of the user's on-disk theme name, set when
    /// the startup indexed-color downgrade replaced [`Self::theme`]
    /// (see `App::new` and [`crate::config::theme::indexed_fallback_theme`]).
    ///
    /// `theme` itself carries the *effective* name so every consumer —
    /// status bar, theme picker's "(current)" marker, welcome modal,
    /// `apply_active_theme` — agrees with what is actually on screen.
    /// [`Config::save`] then writes this stashed name back in `theme`'s
    /// place, so a session that was downgraded for a weaker terminal
    /// never rewrites the theme the user chose for a better one.  Any
    /// *explicit* theme choice clears it via [`Config::set_theme`].
    ///
    /// `#[serde(skip)]` on both halves of the trip: it is never read
    /// from or written to disk.
    #[serde(skip)]
    pub theme_downgraded_from: Option<String>,
    /// User-selected appearance mode.  Filters the theme picker and
    /// governs which counterpart theme is previewed when the mode is
    /// toggled.  Does not by itself change the active theme — see
    /// [`AppearanceMode`].
    pub appearance: AppearanceMode,
    pub editor: EditorConfig,
    pub modal: ModalConfig,
    pub table: TableConfig,
    pub images: ImagesConfig,
    pub diagrams: DiagramsConfig,
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
            diagrams: DiagramsConfig::default(),
            export: ExportConfig::default(),
            dev: DevConfig::default(),
        }
    }
}

/// Result of reading the three on-disk config files.  Returned by
/// [`Config::load`] so `main` can pass each piece to its respective owner
/// (the `Config` to `App`, the keybinding overrides to `KeyMap::build`, the
/// `ThemeFile` to `Theme::from_file`).
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
    pub keybindings: KeyBindingOverrides,
    pub theme: ThemeFile,
    /// Non-fatal problems encountered while parsing the three files —
    /// the App surfaces these in a startup warning modal.  Empty on a
    /// clean load.
    pub warnings: Vec<ConfigWarning>,
}

impl Default for LoadedConfig {
    /// The "no config loaded" fallback — used by `main` when `Config::load`
    /// itself returns `Err` (e.g. malformed `config.toml`).  The theme is
    /// the ThemeFile equivalent of `Theme::default()`, NOT
    /// `ThemeFile::default()`, so the editor stays themed even when the
    /// user's config is unreadable.
    fn default() -> Self {
        Self {
            config: Config::default(),
            keybindings: KeyBindingOverrides::default(),
            theme: (&Theme::default()).into(),
            warnings: Vec::new(),
        }
    }
}

impl Config {
    /// Load config from the XDG config directory, falling back to built-in
    /// defaults for any missing keys and any missing files.
    ///
    /// Reads three files in sequence:
    ///   1. `config.toml`        — editor/modal/table/image + active theme name
    ///   2. `keybindings.toml`   — keybinding overrides
    ///   3. `themes/<name>.toml` — the active theme's style table
    ///
    /// Missing files are silently treated as empty (all-default).  Parse
    /// errors and unknown keys are collected into `LoadedConfig::warnings`
    /// — the load itself is fail-soft so a typo in one file never bricks
    /// the editor.  The App surfaces the warnings in a startup modal so
    /// the user sees the file, line (when available), and the offending
    /// key or message.
    ///
    /// Parameters:
    ///
    /// * `truecolor` — whether the terminal advertises 24-bit color.
    ///   Used only for the missing-theme-file fallback: when the active
    ///   theme name doesn't resolve to a built-in or a file in
    ///   `themes/`, we substitute `Edamame` (truecolor) or `256 Dark`
    ///   (indexed) so the editor still renders with a coherent palette
    ///   appropriate for the terminal.  See [`read_theme_named`] for
    ///   the full case table.
    ///
    /// * `persist_fallback` — when the missing-theme fallback fires,
    ///   whether to rewrite `config.toml` so `theme = <fallback>` and
    ///   the warning doesn't re-surface on the next launch.  `true` at
    ///   startup (the user inherited a stale theme name from an old
    ///   config or a renamed theme — silencing the perpetual warning
    ///   is a clear win).  `false` on the external-editor reload path
    ///   (the user just typed a theme name into `config.toml` and
    ///   overwriting it seconds after they saved would be hostile to a
    ///   legitimate "set the name now, install the theme later"
    ///   workflow).  The fallback is always applied in memory either
    ///   way; this flag only controls the on-disk side-effect.
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
        // Apply the missing-theme fallback in memory; optionally
        // persist it (see `persist_fallback` on the function doc).
        // Save failures are logged but non-fatal: the session runs
        // with the fallback theme in memory either way.
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
        Ok(LoadedConfig {
            config,
            keybindings,
            theme,
            warnings,
        })
    }

    /// Return the directory containing all config files:
    /// `$XDG_CONFIG_HOME/edamame`, falling back to `~/.config/edamame`.
    /// `None` when neither `XDG_CONFIG_HOME` nor `HOME` can be resolved.
    ///
    /// This is deliberately XDG on **every** platform, including macOS —
    /// `dirs::config_dir()` would resolve to `~/Library/Application
    /// Support` there.  edamame's config is hand-editable TOML that users
    /// symlink from a dotfiles repo, so it follows the terminal-tool
    /// convention (neovim, helix, alacritty, starship, …) rather than the
    /// macOS GUI-app one, and a single `~/.config/edamame` works across
    /// Linux and macOS unchanged.
    pub fn config_dir() -> Option<PathBuf> {
        resolve_config_dir(std::env::var_os("XDG_CONFIG_HOME"), dirs::home_dir())
    }

    /// Returns the path to the main config file (may not exist yet).
    pub fn config_path() -> Option<PathBuf> {
        Self::config_dir().map(|d| d.join("config.toml"))
    }

    /// Persist the current `Config` to `config.toml` only.
    ///
    /// Intentionally does NOT touch `keybindings.toml` or any theme file —
    /// those are user-authored and we must never overwrite them during an
    /// ordinary config save.  The `Config` struct only owns fields that
    /// belong in `config.toml`, so this invariant is type-enforced.
    ///
    /// **Comment / formatting preservation.**  When the file already
    /// exists, we merge the new values into the existing
    /// [`toml_edit::DocumentMut`] surgically, replacing each leaf value
    /// in place.  Comments, blank lines, key ordering, and the user's
    /// chosen quoting style all survive.  Keys absent from the user's
    /// file are inserted only if their new value differs from the
    /// compiled default — the shipped reference config follows the
    /// convention "uncommented lines deviate from defaults" and we
    /// honour that.  Keys present in the user's file are *always*
    /// updated, even when the new value equals the default, so a
    /// "change X then change it back" round-trip is reflected
    /// faithfully (we never silently drop a key the user explicitly
    /// chose to set).  See [`save_merge`] for the algorithm.
    ///
    /// First-write case (no existing file): we merge into the
    /// compiled-in annotated reference config instead, so the result is
    /// still fully commented.  In practice this branch is rarely hit
    /// because [`Self::ensure_default_files`] seeds that same file on
    /// first launch — it exists for the case where `config.toml` is
    /// deleted out from under a running session.
    ///
    /// Callers typically log the error and continue rather than making it
    /// fatal.
    /// Commit an *explicit* theme choice — the theme picker's
    /// selection and the export-theme modal's newly written theme.
    /// Clears [`Self::theme_downgraded_from`], so a user who deliberately
    /// picks a theme while the indexed-color downgrade is in effect gets
    /// that choice written to disk (their pick outranks our substitution)
    /// and is not silently reverted on the next save.
    ///
    /// Deliberately NOT used by the picker's live-preview writes: those
    /// are transient, and `Esc` restores the pre-open name, so clearing
    /// the stash there would drop the downgrade on a cancelled preview.
    pub fn set_theme(&mut self, name: String) {
        self.theme = name;
        self.theme_downgraded_from = None;
    }

    /// The config as it should appear on disk: identical to `self`
    /// except that a session-only indexed-color downgrade is undone, so
    /// `theme` carries the user's own choice.
    ///
    /// `save_merge` overwrites every key it finds in the existing
    /// document, so without this a downgraded session would rewrite
    /// `theme = "256 Dark"` over the theme the user picked on their
    /// truecolor terminal — one `config.toml` is typically shared
    /// between both machines.  Cloning is fine: saves are
    /// user-initiated and rare.
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

    /// A `--no-config` session returns `Ok(())` without writing: it never
    /// read the user's files, so it has no business rewriting them from
    /// compiled defaults.  Success rather than an error because nothing
    /// went wrong — callers that show the user a "saved" message append
    /// [`unpersisted_suffix`](crate::config::unpersisted_suffix) and say
    /// so.  See [`crate::config::persistence`] for why the gate is a
    /// process global rather than a field on this struct.
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

    /// Write each of the three default config files into the user's config
    /// directory **only if the file does not already exist**.  Safe to call
    /// on every startup: existing user-edited files are never touched.
    ///
    /// Errors during any one file are logged and skipped; they never fail
    /// startup.  Same posture as [`Config::save`].
    ///
    /// `truecolor` picks the `theme` value seeded into a freshly written
    /// `config.toml` — see [`ensure_default_files_in`].
    pub fn ensure_default_files(truecolor: bool) {
        let Some(dir) = Self::config_dir() else {
            tracing::warn!("no XDG config dir available; skipping default-file scaffolding");
            return;
        };
        ensure_default_files_in(&dir, truecolor);
    }

    /// Returns the path to the log directory.
    pub fn log_dir() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("edamame"))
    }

    /// Read a single named theme from `<config_dir>/themes/<name>.toml`.
    /// Returns the parsed [`ThemeFile`] alongside any non-fatal warnings
    /// (parse errors, unknown keys) so the caller can surface them in
    /// the same `ConfigWarningModal` used at startup.  Missing files
    /// fall back to `Theme::default()`'s `ThemeFile`, matching the
    /// startup loader.  Used by the live theme-change path so the
    /// settings overlay validates the chosen theme through the same
    /// pipeline `Config::load` uses.
    pub fn load_theme(name: &str, truecolor: bool) -> (ThemeFile, Vec<ConfigWarning>) {
        let mut warnings = Vec::new();
        let theme_file = match Self::config_dir() {
            // The fallback signal is only meaningful at startup (where
            // `Config::load` rewrites `config.toml`).  The live theme
            // switch goes through the settings overlay, which already
            // restricts its picker to themes listed by
            // `list_theme_names()` — a missing file here is an edge
            // case (e.g. the user deleted the file between launching
            // edamame and opening the overlay), and the substitution
            // is transient: it won't be persisted, so retrying
            // recovers the original choice as soon as the file
            // reappears.
            Some(dir) => read_theme_named(&dir, name, truecolor, &mut warnings).0,
            None => (&Theme::default()).into(),
        };
        (theme_file, warnings)
    }
}

// ── config directory resolution ───────────────────────────────────────────────

/// Pure core of [`Config::config_dir`] — takes the raw `XDG_CONFIG_HOME`
/// value and the home directory so it can be unit-tested without mutating
/// process environment (which would race across parallel tests).
fn resolve_config_dir(xdg: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    let base = match xdg {
        // An empty or relative value is invalid per the XDG spec; fall back
        // to `~/.config` rather than resolving against the cwd.
        Some(v) if Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => home?.join(".config"),
    };
    Some(base.join("edamame"))
}

// ── save: comment-preserving merge ────────────────────────────────────────────

/// Produce the TOML string to write for [`Config::save`].
///
/// If `path` exists, the user's file is parsed as a `DocumentMut`
/// and we merge in only the leaves that either (a) already appear
/// in the user's file (replace in place — preserving the row's
/// leading whitespace and trailing comment) or (b) differ from the
/// compiled default (insert at the natural location, creating
/// parent tables when needed).  See `merge_changed` for the leaf
/// algorithm.
///
/// If `path` doesn't exist we merge into the compiled-in annotated
/// reference config ([`REFERENCE_CONFIG_TOML`]) instead, so a save that
/// races a missing `config.toml` still produces a fully commented file.
/// Emitting a bare `toml::to_string_pretty` here would be a one-way
/// door: every later save merges faithfully into whatever is on disk,
/// so a single de-annotated write strips the documentation forever.
fn save_merge(config: &Config, path: &Path) -> Result<String> {
    use toml_edit::DocumentMut;

    let new_serialized =
        toml::to_string_pretty(config).context("Failed to serialize config to TOML")?;

    let existing_raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        // First-write path: fall back to the shipped annotated template
        // so the merge below still has comments to preserve.
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

/// Merge `new` into `existing`, leaving comments / decor untouched.
///
/// Algorithm, per `new` key:
///   - Both sides have a sub-table → recurse.
///   - Key is present in `existing` as a value → overwrite the
///     value, leaving its trailing-comment decor in place.
///   - Key is absent from `existing`:
///       * value matches `defaults` → skip (the convention is
///         "uncommented = deviation").
///       * value differs from `defaults` → insert.
///       * sub-table → recurse into a fresh `Table`; only attach
///         it to `existing` if any descendant leaf survived.
///   - Type mismatch (e.g. existing has a value where new has a
///     table) → replace wholesale; the user's structure is broken
///     anyway.
///   - Array-of-tables / `Item::None` → not yet driven from the
///     UI; we copy them through verbatim only when the user's file
///     already has them (handled by the existing-key branch).
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
                    // Section missing from the user's file.  Build a
                    // pruned copy that contains only deviations from
                    // defaults; attach it only if non-empty.
                    let mut pruned = Table::new();
                    merge_changed(&mut pruned, new_tbl, &default_tbl);
                    if !pruned.is_empty() {
                        existing.insert(key, Item::Table(pruned));
                    }
                }
            }
            Item::Value(new_val) => match existing.get_mut(key) {
                Some(Item::Value(exist_val)) => {
                    // Preserve the row's decor (leading whitespace +
                    // trailing comment) when overwriting the value.
                    // toml_edit stores decor *on* the value, so a
                    // naive `*exist_val = new_val.clone()` would drop
                    // the existing prefix/suffix and lose any "# this
                    // is the active theme"-style trailing comments.
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
                // Programmatic updates don't currently touch
                // `[[export.custom]]` — but if a future change does,
                // overwrite wholesale (we have no merge identity for
                // array elements).
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

/// Compare two `toml_edit::Value`s by their canonical TOML
/// representation.  `Value::to_string` includes the formatting
/// (quotes, separators) but not row decor, so this is a stable
/// equality on the *semantic* content of each value.  Used to
/// decide whether a leaf differs from the compiled default.
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

    /// The `--no-config` guarantee, enforced at the write site: a session
    /// that read nothing must write nothing, or a triage run that toggles
    /// one setting would overwrite the user's real `config.toml` with
    /// compiled defaults.
    ///
    /// Both halves matter — the second write proves the first assertion
    /// is about the gate and not about a misdirected path.
    #[test]
    fn save_writes_nothing_while_config_writes_are_suppressed() {
        let _lock = crate::test_env::env_lock();
        let dir = tempfile::tempdir().unwrap();
        // `save` resolves its own path from the environment, so point the
        // whole config dir at the tempdir for the duration of the test.
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

    #[test]
    fn config_dir_prefers_absolute_xdg_config_home() {
        let dir = resolve_config_dir(Some("/xdg".into()), Some(PathBuf::from("/home/u")));
        assert_eq!(dir, Some(PathBuf::from("/xdg/edamame")));
    }

    #[test]
    fn config_dir_falls_back_to_dot_config_on_every_platform() {
        // Unset, empty, and relative XDG values all fall back to `~/.config`
        // — including on macOS, where `dirs::config_dir()` would have
        // returned `~/Library/Application Support`.
        for xdg in [None, Some(OsString::from("")), Some(OsString::from("rel"))] {
            let dir = resolve_config_dir(xdg, Some(PathBuf::from("/home/u")));
            assert_eq!(dir, Some(PathBuf::from("/home/u/.config/edamame")));
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

    #[test]
    fn update_check_fields_default_and_round_trip() {
        let mut config = Config::default();
        // Opt-out, so a fresh install checks; the two bookkeeping
        // fields start empty so the first launch is always due and
        // nothing has been notified about yet.
        assert!(config.editor.check_for_updates);
        assert_eq!(config.editor.last_update_check, 0);
        assert_eq!(config.editor.update_notified_for, "");

        config.editor.check_for_updates = false;
        config.editor.last_update_check = 1_755_500_000;
        config.editor.update_notified_for = "v0.2.0".to_owned();
        let serialized = toml::to_string(&config).expect("serialize");
        let deserialized: Config = toml::from_str(&serialized).expect("deserialize");
        assert!(!deserialized.editor.check_for_updates);
        assert_eq!(deserialized.editor.last_update_check, 1_755_500_000);
        assert_eq!(deserialized.editor.update_notified_for, "v0.2.0");
    }

    #[test]
    fn seen_terminal_fingerprints_round_trip() {
        let mut config = Config::default();
        assert!(config.editor.seen_terminal_fingerprints.is_empty());
        config.editor.seen_terminal_fingerprints.push(
            "WezTerm|xterm-256color||truecolor|kitty|mouse=true|kbd=true|unicode=true".into(),
        );
        let serialized = toml::to_string(&config).expect("serialize");
        let deserialized: Config = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(
            deserialized.editor.seen_terminal_fingerprints,
            config.editor.seen_terminal_fingerprints
        );
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
        // `default` is the historical theme name (still referenced by
        // some user `config.toml` files written by older edamame
        // versions).  It isn't in `BUILTIN_THEMES`, so the loader hits
        // the missing-file path and substitutes the truecolor built-in
        // (`Edamame`) — the editor must still come up themed, and the
        // user gets a warning explaining the substitution.
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
        // Indexed-color terminals get `256 Dark` instead — the truecolor
        // fallback uses 24-bit RGB values that would degrade on a
        // 256-color emulator.
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
        // Distinct from the missing-file case: if the user deliberately
        // empties their custom theme file, they get an empty theme —
        // their choice.  Uses a non-built-in name so the disk file is
        // actually consulted (built-ins always win on name collision
        // and would otherwise short-circuit before the file read).
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
        // A user file at `themes/<builtin>.toml` must NOT override the
        // compiled-in built-in: the file is ignored entirely, even if
        // it contains valid overrides.  The user's escape hatch is to
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
        // The companion light palette resolves from the built-in
        // registry without any disk file.
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
        // `LoadedConfig::default()` is the fallback when `Config::load`
        // itself fails (e.g. malformed `config.toml`).  The editor must
        // stay themed even in that degraded state.
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
        // Type mismatch — `mouse_scroll_lines` expects an integer.
        std::fs::write(&path, "[editor]\nmouse_scroll_lines = \"oops\"\n").unwrap();
        let mut warnings = Vec::new();
        let config = read_main_config(&path, &mut warnings);
        // Falls back to defaults.
        assert_eq!(config.editor.mouse_scroll_lines, 1);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].path, path);
        match &warnings[0].kind {
            WarningKind::ParseError(msg) => {
                // toml's default error formatter includes a line number.
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
        // Bound is strict: exactly 1000 is rejected.
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
        // Top-level `bogus_top` lives ABOVE the `[editor]` table — TOML
        // associates the key with the most recent table header.
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

    #[test]
    fn read_keybindings_strips_invalid_entries_and_warns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybindings.toml");
        // First entry valid, second has unknown action, third has unparseable key.
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
        // Recognized key still applied.
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
        // Uses a non-built-in name so the disk file is consulted —
        // built-in names short-circuit before any read.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        let theme_path = dir.path().join("themes").join("custom.toml");
        // Invalid color value type.
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
        // Built-in themes are compiled in (see `BUILTIN_THEMES`), so
        // first-run scaffolding must not write `themes/<builtin>.toml`
        // — those files would be inert (the built-in always wins) and
        // misleading to a user who tried to edit them.  The themes
        // directory itself is still created so it exists for custom
        // theme files.
        let dir = tempfile::tempdir().unwrap();
        ensure_default_files_in(dir.path(), true);
        assert!(dir.path().join("config.toml").exists());
        assert!(dir.path().join("keybindings.toml").exists());
        assert!(dir.path().join("themes").is_dir());
        assert!(!dir.path().join("themes").join("default.toml").exists());
        // The export stylesheet folder is created (like `themes/`) and seeded
        // with a `.example` reference users can fork — but NOT a selectable
        // `default.css`, since the single built-in default is compiled in.
        assert!(dir.path().join("export").is_dir());
        let export = dir.path().join("export");
        assert!(
            !export.join("default.css").exists(),
            "no selectable default.css — would duplicate the compiled-in Builtin"
        );
        let css = std::fs::read_to_string(export.join("default.css.example"))
            .expect("default.css.example scaffolded");
        assert!(css.contains("markdown-body"), "bundled stylesheet body");
        // The `.example` reference is excluded from the modal's picker.
        assert!(
            crate::config::list_export_stylesheets(dir.path()).is_empty(),
            "the .example reference must not appear as a stylesheet pick"
        );
    }

    #[test]
    fn ensure_default_files_seeds_256_dark_without_truecolor() {
        // A first run on an indexed-color terminal (Apple Terminal and
        // friends) must not land on the truecolor default — its RGB palette
        // quantizes badly there.  The seeded file is still the annotated
        // reference; only the theme assignment differs.
        let dir = tempfile::tempdir().unwrap();
        ensure_default_files_in(dir.path(), false);
        let seeded = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(seeded.contains("theme = \"256 Dark\""));
        assert!(!seeded.contains("theme = \"Edamame\""));
        assert!(
            seeded.contains("# Name of the active theme."),
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

        // Simulate a user edit to config.toml.
        let config_path = dir.path().join("config.toml");
        let custom = "# user-edited\ntheme = \"light\"\n";
        std::fs::write(&config_path, custom).unwrap();

        // Second call must not touch the user's edit.
        ensure_default_files_in(dir.path(), true);
        let after = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(after, custom, "user-edited config was overwritten");
    }

    // ── save invariant ─────────────────────────────────────────────────────

    /// `Config::save()` (via `toml::to_string_pretty`) must not emit any
    /// `[keybindings]` / `[theme_file]` / style sections.  The fields that
    /// would have produced those sections no longer live on the struct, so
    /// round-trip serialization is the clearest way to assert the invariant.
    #[test]
    fn save_serialization_only_contains_config_fields() {
        let config = Config::default();
        let serialized = toml::to_string_pretty(&config).expect("serialize");
        assert!(!serialized.contains("[keybindings]"));
        // Check for any heading-style section that would indicate a theme
        // table leaked in:
        assert!(!serialized.contains("[h1]"));
        assert!(!serialized.contains("[h2]"));
        // Positive assertions — things that *should* be in the saved config:
        assert!(serialized.contains("theme ="));
        assert!(serialized.contains("[editor]"));
        assert!(serialized.contains("[modal]"));
        assert!(serialized.contains("[images]"));
        assert!(serialized.contains("[diagrams]"));
        assert!(serialized.contains("[export"));
        assert!(serialized.contains("[dev]"));
    }

    #[test]
    fn export_config_defaults_and_round_trip() {
        let config = Config::default();
        assert_eq!(config.export.html.stylesheet, "builtin");
        assert!(!config.export.html.inline_images);
        assert!(config.export.html.diagrams);
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

    // ── save_merge: comment-preserving in-place update ─────────────────────

    /// A user file with comments + the shipped "deviation" keys must
    /// round-trip through `save_merge` unchanged when nothing has
    /// changed.  No clutter is appended, comments survive verbatim.
    /// A session-only indexed-color downgrade must never reach disk:
    /// the same `config.toml` is typically shared (dotfiles) with a
    /// truecolor terminal where the user's own theme is correct.
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

    /// Without a stash the theme is written normally — the restore is
    /// scoped to the downgrade, not a blanket "never write theme".
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

    /// An explicit pick outranks the substitution and clears the stash,
    /// so it reaches disk like any other theme change.
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
        // No default-valued cruft injected into [editor]:
        assert!(!out.contains("transient_ms"));
        assert!(!out.contains("mouse_scroll_lines"));
    }

    /// Changing an existing key in the user's file replaces just
    /// the value — the trailing comment on the same line stays.
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

    /// A non-default value for a key not currently in the user's
    /// file gets inserted (so settings-overlay changes persist),
    /// but default-valued sibling keys do not — clutter avoidance.
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
        // Defaults that the UI didn't touch must NOT be appended:
        assert!(!out.contains("transient_ms = 1500"));
        assert!(!out.contains("max_width_cols = 80"));
    }

    /// First-write path: no existing file → merge into the shipped
    /// annotated reference config, so the emitted file keeps its
    /// documentation instead of being a bare serialization.
    #[test]
    fn save_merge_first_write_emits_annotated_reference() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config::default();
        let out = save_merge(&config, &path).expect("merge ok");
        let _round: Config = toml::from_str(&out).expect("parses");
        assert!(out.contains("theme ="));
        assert!(out.contains("# edamame configuration"));
        // Defaults the user never set stay commented-out reference rows,
        // exactly as in a scaffolded file.
        assert!(!out.contains("\ntransient_ms ="));
    }

    /// A save whose values deviate from the defaults still lands in an
    /// annotated file when `config.toml` was deleted out from under a
    /// running session — the regression that once stripped every
    /// comment permanently (each later save merges into whatever is on
    /// disk, so one bare write is a one-way door).
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
        assert!(out.contains("# Name of the active theme."));
    }

    /// If the user has an explicit non-default value in their file
    /// and the in-memory config has it back at the default, we
    /// still rewrite the existing line — never silently drop a key
    /// the user once chose to set explicitly.
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
