use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::init::ensure_default_files_in;
use super::keymap::KeyBindingOverrides;
use super::readers::{read_keybindings, read_main_config, read_theme_named};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorConfig {
    /// Number of spaces per tab stop.
    pub tab_width: usize,
    /// When true, code block lines that exceed the terminal width are wrapped.
    /// Default: false (long lines extend beyond the visible area without wrapping).
    pub code_block_wrap: bool,
    /// When true, long lines in the document wrap at the terminal width.
    /// Default: true.
    pub line_wrap: bool,
    /// When true, multiple consecutive blank lines in the source are rendered
    /// as multiple blank lines in the output.  Standard Markdown collapses them
    /// to a single blank line; this option preserves the author's intent.
    /// Default: true.
    pub preserve_blank_lines: bool,
    /// When true (default), pressing Up/Down in rendered/hybrid mode moves the
    /// cursor by **visual** lines (accounting for word-wrap), so the cursor
    /// stays at the same horizontal column on the screen.  When false, movement
    /// is by **logical** buffer lines (one `\n`-terminated line per step).
    pub visual_line_nav: bool,
    /// When true, the startup notice that lists unsupported terminal features
    /// is skipped.  Set by the `[Don't show this again]` button on the notice
    /// modal.
    pub suppress_capability_warnings: bool,
    /// Lines advanced per mouse-wheel tick.  Default 1 — users can bump this
    /// to 2 or 3 for a coarser, faster feel at the cost of fine-grained
    /// control.  The keyboard `ScrollUp` / `ScrollDown` actions always step
    /// by exactly one line and are not affected by this setting.
    pub mouse_scroll_lines: usize,
    /// Bottom-region layout.  `"two_line"` (default) renders a hint line
    /// above the persistent status line; `"compact"` collapses to just
    /// the status line, reachable hint chords via the `?` popover.
    pub status_bar: StatusBarLayout,
    /// Duration (milliseconds) that a non-sticky transient message
    /// overlays the hint line before auto-expiring.  Errors ignore this
    /// and remain visible until the user dismisses them with Escape.
    pub transient_ms: u64,
}

/// How the bottom status region is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusBarLayout {
    /// Two rows: hint line above, persistent status below.  Default.
    #[default]
    TwoLine,
    /// One row: persistent status only; hints via the `?` popover.
    Compact,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            tab_width: 4,
            code_block_wrap: false,
            line_wrap: true,
            preserve_blank_lines: true,
            visual_line_nav: true,
            suppress_capability_warnings: false,
            mouse_scroll_lines: 1,
            status_bar: StatusBarLayout::default(),
            transient_ms: 1500,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModalConfig {
    /// Which modal handler to use. Currently only "default" is supported.
    pub handler: String,
}

impl Default for ModalConfig {
    fn default() -> Self {
        Self {
            handler: "default".into(),
        }
    }
}

/// Table-editing configuration.
///
/// `show_buttons` governs whether the row/column buttons — the `⠿`
/// reorder grips, the `⇔` resize glyph, and the `✕` row/column delete
/// glyphs — are rendered and hit-tested.  Defaults to `true`: the
/// renderer still checks the terminal's detected `Capabilities::mouse`
/// flag before enabling the feature at runtime, so setting this to
/// `true` on a mouseless terminal is a no-op — `App::new` overrides it
/// to `false` when `capabilities.mouse` is absent so persisted config
/// stays faithful to what the user actually sees.
///
/// `row_striping` (Phase 13): when true, alternating data rows are filled
/// with `Theme::table_row_even` / `Theme::table_row_odd` to aid visual
/// scanning on wide tables.  Off by default so users who prefer plain
/// borders see no change.
///
/// `warn_on_width_injection` (Phase 13): when true, the first column-border
/// drag on a table without a `<!-- tui-columns: [...] -->` comment opens a
/// modal warning that committing the resize will inject the comment into
/// the Markdown source.  Set false (either via the modal's "Continue and
/// don't ask again" button or directly in `config.toml`) to skip the
/// warning on subsequent drags.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TableConfig {
    pub show_buttons: bool,
    pub row_striping: bool,
    pub warn_on_width_injection: bool,
}

impl Default for TableConfig {
    fn default() -> Self {
        Self {
            show_buttons: true,
            row_striping: true,
            warn_on_width_injection: true,
        }
    }
}

/// Policy for fetching images referenced by `http(s)://` URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteImagePolicy {
    /// Prompt the user the first time a document with remote images is opened.
    #[default]
    Ask,
    /// Always fetch remote images without prompting.
    Always,
    /// Never fetch remote images; always fall back to the placeholder.
    Never,
}

/// Master switch for inline image rendering.  `Ask` prompts the user the
/// first time a document with images is opened; `Always` renders without
/// prompting; `Never` keeps the `[Image: alt]` placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImagesEnabled {
    /// Prompt the user the first time a document with images is opened.
    #[default]
    Ask,
    /// Always render images inline.
    Always,
    /// Never render images — always fall back to the `[Image: alt]` placeholder.
    Never,
}

/// Image-rendering configuration.
///
/// `max_width` / `max_height` are ceilings in terminal cells; each image
/// reserves at most this many rows, and the inline renderer clamps to this
/// width so a single oversized image never takes over the viewport.  Values
/// are applied verbatim by `ratatui_image`'s `Resize::Fit` path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImagesConfig {
    /// Master switch — `"ask"` (default) prompts on first document with
    /// images, `"always"` renders without prompting, `"never"` always
    /// falls back to the placeholder.
    pub enabled: ImagesEnabled,
    /// Maximum width (in terminal cells) for a single image.
    pub max_width: usize,
    /// Maximum height (in terminal cells) for a single image.
    pub max_height: usize,
    /// Policy for fetching `http(s)://` images.
    pub remote_policy: RemoteImagePolicy,
}

impl Default for ImagesConfig {
    fn default() -> Self {
        Self {
            enabled: ImagesEnabled::Ask,
            max_width: 80,
            max_height: 24,
            remote_policy: RemoteImagePolicy::Ask,
        }
    }
}

/// Export configuration (Phase 16).
///
/// HTML is the single built-in export target; it doubles as the intermediate
/// format for user-defined custom commands that produce PDF, DOCX, etc. by
/// piping the generated HTML through an external tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportConfig {
    pub html: HtmlExportConfig,
    /// User-defined extra export entries that appear alongside
    /// `Export HTML` in the command palette.  Each runs an external
    /// command with `{html}` / `{out}` path substitution.
    pub custom: Vec<CustomExportEntry>,
}

/// HTML export settings.  `stylesheet = "builtin"` (the default) uses the
/// compiled-in CSS bundled with edamame.  Any other value is treated as a
/// filesystem path to a user stylesheet.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HtmlExportConfig {
    /// Either the sentinel `"builtin"` or an absolute / home-relative path
    /// to a user CSS file.  Read at export time; parse errors are surfaced
    /// to the user via the export error message.
    pub stylesheet: String,
    /// When true, local `![alt](relative/path.png)` references are read
    /// from disk at export time and embedded as `data:` URIs so the HTML
    /// is fully self-contained.  Default: false (keeps output compact and
    /// portable alongside the asset directory).
    pub inline_images: bool,
    /// Phase 17 — when true (the default), fenced ```mermaid code blocks
    /// are rendered to inline SVG via `mermaid-rs-renderer` and wrapped
    /// in a `<figure class="mermaid-diagram">`.  On render failure the
    /// block falls back to `<pre><code class="language-mermaid">` so the
    /// source is never lost.  Set false to force the code-block form
    /// (e.g. for pipelines that ship their own client-side mermaid.js).
    pub diagrams: bool,
}

impl Default for HtmlExportConfig {
    fn default() -> Self {
        Self {
            stylesheet: "builtin".into(),
            inline_images: false,
            diagrams: true,
        }
    }
}

/// A single user-configured custom-export entry.  Shows up in the
/// command palette as "Export <name>".  `command` is run verbatim with
/// two placeholders substituted:
///
/// * `{html}` — path to the just-generated HTML file (temp file owned
///   by the exporter; deleted after the command exits).
/// * `{out}` — path to the final output file (source-stem with the
///   configured `extension` appended).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomExportEntry {
    /// Human-readable label — appears as "Export <name>" in the palette.
    pub name: String,
    /// argv-style command.  Element 0 is the executable; remaining
    /// elements are arguments with `{html}` / `{out}` substitution.
    pub command: Vec<String>,
    /// Extension (no leading dot) for the output file.
    pub extension: String,
}

/// Developer/diagnostic settings.  Kept separate from `[editor]` because these
/// knobs govern logging and debug tooling, not editing behaviour.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DevConfig {
    /// When true, `tracing` logs are written to the XDG data dir (e.g.
    /// `~/.local/share/edamame/`).  Off by default so the TUI stays silent.
    pub logging: bool,
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
        // The `default` theme is allowed to be missing (first run, before
        // ensure_default_files has written it).  The fallback must be the
        // compiled `Theme::default()` — NOT an empty ThemeFile — otherwise
        // the app would render unstyled output on the very first launch.
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
        // empties `default.toml`, they get an empty theme — their choice.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        std::fs::write(dir.path().join("themes").join("default.toml"), "").unwrap();
        let mut warnings = Vec::new();
        let theme = read_theme_named(dir.path(), "default", &mut warnings);
        assert_eq!(theme.h1, super::super::theme_file::StyleSpec::default());
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
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        let theme_path = dir.path().join("themes").join("default.toml");
        // Invalid colour value type.
        std::fs::write(&theme_path, "[h1]\nfg = 42\nbold = \"oops\"\n").unwrap();
        let mut warnings = Vec::new();
        let theme = read_theme_named(dir.path(), "default", &mut warnings);
        let theme_out: Theme = (&theme).into();
        assert_eq!(theme_out.h1, Theme::default().h1);
        assert_eq!(warnings.len(), 1);
        assert!(matches!(warnings[0].kind, WarningKind::ParseError(_)));
    }

    // ── ensure_default_files ───────────────────────────────────────────────

    #[test]
    fn ensure_default_files_writes_three_files_on_first_run() {
        let dir = tempfile::tempdir().unwrap();
        ensure_default_files_in(dir.path());
        assert!(dir.path().join("config.toml").exists());
        assert!(dir.path().join("keybindings.toml").exists());
        assert!(dir.path().join("themes").join("default.toml").exists());
    }

    #[test]
    fn ensure_default_files_is_idempotent_and_preserves_user_edits() {
        let dir = tempfile::tempdir().unwrap();
        ensure_default_files_in(dir.path());

        // Simulate a user edit to the theme file.
        let theme_path = dir.path().join("themes").join("default.toml");
        let custom = "# user-edited\ntask_strikethrough = false\n";
        std::fs::write(&theme_path, custom).unwrap();

        // Second call must not touch the user's edit.
        ensure_default_files_in(dir.path());
        let after = std::fs::read_to_string(&theme_path).unwrap();
        assert_eq!(after, custom, "user-edited theme was overwritten");
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
