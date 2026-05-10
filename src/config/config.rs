use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::init::ensure_default_files_in;
use super::keymap::KeyBindingOverrides;
use super::readers::{read_keybindings, read_main_config, read_theme_named};
pub use super::sections::{
    CustomExportEntry, DevConfig, EditorConfig, ExportConfig, ImagesConfig, ImagesEnabled,
    ModalConfig, RemoteImagePolicy, StatusBarLayout, TableConfig,
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
    /// Name of the active theme, resolved to `themes/<theme>.toml` at load.
    /// Defaults to `"default"`.  A missing file falls back to the compiled-in
    /// `Theme::default()` so the editor always has a working colour table.
    pub theme: String,
    pub editor: EditorConfig,
    pub modal: ModalConfig,
    pub table: TableConfig,
    pub images: ImagesConfig,
    pub export: ExportConfig,
    pub dev: DevConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "default".into(),
            editor: EditorConfig::default(),
            modal: ModalConfig::default(),
            table: TableConfig::default(),
            images: ImagesConfig::default(),
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
    pub fn load() -> Result<LoadedConfig> {
        let dir = Self::config_dir();
        let mut warnings = Vec::new();
        let config = match &dir {
            Some(d) => read_main_config(&d.join("config.toml"), &mut warnings),
            None => Config::default(),
        };
        let keybindings = match &dir {
            Some(d) => read_keybindings(&d.join("keybindings.toml"), &mut warnings),
            None => KeyBindingOverrides::default(),
        };
        let theme = match &dir {
            Some(d) => read_theme_named(d, &config.theme, &mut warnings),
            None => ThemeFile::default(),
        };
        Ok(LoadedConfig {
            config,
            keybindings,
            theme,
            warnings,
        })
    }

    /// Return the directory containing all config files (e.g.
    /// `~/.config/edamame`).  `None` when no XDG / HOME can be resolved.
    pub fn config_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("edamame"))
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
    /// Callers typically log the error and continue rather than making it
    /// fatal.
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()
            .context("Could not determine config directory (missing XDG_CONFIG_HOME/HOME)")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory: {}", parent.display())
            })?;
        }
        let serialized =
            toml::to_string_pretty(self).context("Failed to serialize config to TOML")?;
        std::fs::write(&path, serialized)
            .with_context(|| format!("Failed to write config file: {}", path.display()))?;
        Ok(())
    }

    /// Write each of the three default config files into the user's config
    /// directory **only if the file does not already exist**.  Safe to call
    /// on every startup: existing user-edited files are never touched.
    ///
    /// Errors during any one file are logged and skipped; they never fail
    /// startup.  Same posture as [`Config::save`].
    pub fn ensure_default_files() {
        let Some(dir) = Self::config_dir() else {
            tracing::warn!("no XDG config dir available; skipping default-file scaffolding");
            return;
        };
        ensure_default_files_in(&dir);
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
    pub fn load_theme(name: &str) -> (ThemeFile, Vec<ConfigWarning>) {
        let mut warnings = Vec::new();
        let theme_file = match Self::config_dir() {
            Some(dir) => read_theme_named(&dir, name, &mut warnings),
            None => (&Theme::default()).into(),
        };
        (theme_file, warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = Config::default();
        assert_eq!(config.editor.tab_width, 4);
        assert!(!config.dev.logging);
        assert_eq!(config.modal.handler, "default");
        assert_eq!(config.theme, "default");
    }

    #[test]
    fn config_round_trips_toml() {
        let config = Config::default();
        let serialized = toml::to_string(&config).expect("serialize");
        let deserialized: Config = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(deserialized.editor.tab_width, config.editor.tab_width);
        assert_eq!(deserialized.modal.handler, config.modal.handler);
        assert_eq!(deserialized.theme, config.theme);
    }

    #[test]
    fn partial_toml_falls_back_to_defaults() {
        let toml = "[dev]\nlogging = true\n";
        let config: Config = toml::from_str(toml).expect("deserialize");
        assert!(config.dev.logging);
        assert_eq!(config.editor.tab_width, 4); // default
        assert_eq!(config.modal.handler, "default"); // default
        assert_eq!(config.theme, "default"); // default
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
    fn suppress_capability_warnings_round_trips() {
        let mut config = Config::default();
        assert!(!config.editor.suppress_capability_warnings);
        config.editor.suppress_capability_warnings = true;
        let serialized = toml::to_string(&config).expect("serialize");
        let deserialized: Config = toml::from_str(&serialized).expect("deserialize");
        assert!(deserialized.editor.suppress_capability_warnings);
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
        assert_eq!(config.theme, "default");
        assert_eq!(config.editor.tab_width, 4);
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
    fn read_theme_missing_default_falls_back_to_compiled_theme() {
        // The `default` theme is a built-in (see `BUILTIN_THEMES`) and
        // is resolved from compiled-in code without any disk read at
        // all — the user's themes directory may legitimately be empty.
        // The result must equal the compiled `Theme::default()`.
        let dir = tempfile::tempdir().unwrap();
        let mut warnings = Vec::new();
        let theme = read_theme_named(dir.path(), "default", &mut warnings);
        let expected: super::super::theme_file::ThemeFile = (&Theme::default()).into();
        assert_eq!(theme.h1, expected.h1);
        assert_eq!(theme.task_strikethrough, expected.task_strikethrough);
        // Convert through to Theme and verify it equals the compiled default.
        let theme_out: Theme = (&theme).into();
        assert_eq!(theme_out.h1, Theme::default().h1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn read_theme_missing_named_falls_back_to_compiled_theme() {
        // Same contract for a missing named theme: the warning path is
        // exercised internally; here we just assert the fallback is the
        // compiled `Theme::default()`, not an empty ThemeFile.
        let dir = tempfile::tempdir().unwrap();
        let mut warnings = Vec::new();
        let theme = read_theme_named(dir.path(), "nonexistent", &mut warnings);
        let theme_out: Theme = (&theme).into();
        assert_eq!(theme_out.h1, Theme::default().h1);
    }

    #[test]
    fn read_theme_empty_file_stays_empty() {
        // Distinct from the missing-file case: if the user deliberately
        // empties their custom theme file, they get an empty theme —
        // their choice.  Uses a non-built-in name so the disk file is
        // actually consulted (built-ins always win on name collision
        // and would otherwise short-circuit before the file read).
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        std::fs::write(dir.path().join("themes").join("custom.toml"), "").unwrap();
        let mut warnings = Vec::new();
        let theme = read_theme_named(dir.path(), "custom", &mut warnings);
        assert_eq!(theme.h1, super::super::theme_file::StyleSpec::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn read_theme_builtin_wins_over_user_file() {
        // A user file at `themes/<builtin>.toml` must NOT override the
        // compiled-in built-in: the file is ignored entirely, even if
        // it contains valid overrides.  The user's escape hatch is to
        // pick a different name.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        std::fs::write(
            dir.path().join("themes").join("default.toml"),
            "[h1]\nfg = \"red\"\n",
        )
        .unwrap();
        let mut warnings = Vec::new();
        let theme = read_theme_named(dir.path(), "default", &mut warnings);
        let theme_out: Theme = (&theme).into();
        assert_eq!(theme_out.h1, Theme::default().h1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn read_theme_resolves_light_builtin() {
        // The companion light palette resolves from the built-in
        // registry without any disk file.
        use super::super::themes::default_light;
        let dir = tempfile::tempdir().unwrap();
        let mut warnings = Vec::new();
        let theme = read_theme_named(dir.path(), "light", &mut warnings);
        let theme_out: Theme = (&theme).into();
        let expected = Theme::from_palette(&default_light::palette());
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
        std::fs::write(&path, "theme = \"solarized\"\n\n[editor]\ntab_width = 2\n").unwrap();
        let mut warnings = Vec::new();
        let config = read_main_config(&path, &mut warnings);
        assert_eq!(config.theme, "solarized");
        assert_eq!(config.editor.tab_width, 2);
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
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        let theme_path = dir.path().join("themes").join("custom.toml");
        std::fs::write(&theme_path, "[h1]\nfg = \"red\"\nbold = true\n").unwrap();
        let mut warnings = Vec::new();
        let theme = read_theme_named(dir.path(), "custom", &mut warnings);
        assert!(theme.h1.bold);
        assert!(warnings.is_empty());
    }

    // ── Warning paths ──────────────────────────────────────────────────────

    #[test]
    fn read_main_config_parse_error_warns_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Type mismatch — `tab_width` expects an integer.
        std::fs::write(&path, "[editor]\ntab_width = \"oops\"\n").unwrap();
        let mut warnings = Vec::new();
        let config = read_main_config(&path, &mut warnings);
        // Falls back to defaults.
        assert_eq!(config.editor.tab_width, 4);
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
    fn read_main_config_unknown_key_warns_but_keeps_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Top-level `bogus_top` lives ABOVE the `[editor]` table — TOML
        // associates the key with the most recent table header.
        std::fs::write(
            &path,
            "bogus_top = true\n\n[editor]\ntab_width = 2\ntab_widht = 8\n",
        )
        .unwrap();
        let mut warnings = Vec::new();
        let config = read_main_config(&path, &mut warnings);
        assert_eq!(config.editor.tab_width, 2);
        assert_eq!(warnings.len(), 1);
        match &warnings[0].kind {
            WarningKind::UnknownKeys(keys) => {
                assert!(
                    keys.iter().any(|k| k == "editor.tab_widht"),
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
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        let theme_path = dir.path().join("themes").join("custom.toml");
        std::fs::write(&theme_path, "[h1]\nfg = \"red\"\n\n[h7]\nfg = \"blue\"\n").unwrap();
        let mut warnings = Vec::new();
        let theme = read_theme_named(dir.path(), "custom", &mut warnings);
        // Recognised key still applied.
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
        // Uses a non-built-in name so the disk file is consulted —
        // built-in names short-circuit before any read.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        let theme_path = dir.path().join("themes").join("custom.toml");
        // Invalid colour value type.
        std::fs::write(&theme_path, "[h1]\nfg = 42\nbold = \"oops\"\n").unwrap();
        let mut warnings = Vec::new();
        let theme = read_theme_named(dir.path(), "custom", &mut warnings);
        let theme_out: Theme = (&theme).into();
        assert_eq!(theme_out.h1, Theme::default().h1);
        assert_eq!(warnings.len(), 1);
        assert!(matches!(warnings[0].kind, WarningKind::ParseError(_)));
    }

    // ── ensure_default_files ───────────────────────────────────────────────

    #[test]
    fn ensure_default_files_writes_config_and_keybindings_but_not_themes() {
        // Built-in themes are compiled in (see `BUILTIN_THEMES`), so
        // first-run scaffolding must not write `themes/<builtin>.toml`
        // — those files would be inert (the built-in always wins) and
        // misleading to a user who tried to edit them.  The themes
        // directory itself is still created so it exists for custom
        // theme files.
        let dir = tempfile::tempdir().unwrap();
        ensure_default_files_in(dir.path());
        assert!(dir.path().join("config.toml").exists());
        assert!(dir.path().join("keybindings.toml").exists());
        assert!(dir.path().join("themes").is_dir());
        assert!(!dir.path().join("themes").join("default.toml").exists());
    }

    #[test]
    fn ensure_default_files_is_idempotent_and_preserves_user_edits() {
        let dir = tempfile::tempdir().unwrap();
        ensure_default_files_in(dir.path());

        // Simulate a user edit to config.toml.
        let config_path = dir.path().join("config.toml");
        let custom = "# user-edited\ntheme = \"light\"\n";
        std::fs::write(&config_path, custom).unwrap();

        // Second call must not touch the user's edit.
        ensure_default_files_in(dir.path());
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
}
