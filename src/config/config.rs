use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::keymap::KeyBindingOverrides;
use super::theme::Theme;
use super::theme_file::ThemeFile;

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
    /// errors propagate so the user sees the line number of the typo.
    pub fn load() -> Result<LoadedConfig> {
        let dir = Self::config_dir();
        let config = match &dir {
            Some(d) => read_main_config(&d.join("config.toml"))?,
            None => Config::default(),
        };
        let keybindings = match &dir {
            Some(d) => read_keybindings(&d.join("keybindings.toml"))?,
            None => KeyBindingOverrides::default(),
        };
        let theme = match &dir {
            Some(d) => read_theme_named(d, &config.theme)?,
            None => ThemeFile::default(),
        };
        Ok(LoadedConfig {
            config,
            keybindings,
            theme,
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
}

// ── Readers ───────────────────────────────────────────────────────────────────

fn read_main_config(path: &Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(raw) => toml::from_str(&raw)
            .with_context(|| format!("Failed to parse config file: {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("Failed to read config file: {}", path.display())),
    }
}

fn read_keybindings(path: &Path) -> Result<KeyBindingOverrides> {
    match std::fs::read_to_string(path) {
        Ok(raw) => toml::from_str(&raw)
            .with_context(|| format!("Failed to parse keybindings file: {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KeyBindingOverrides::default()),
        Err(e) => {
            Err(e).with_context(|| format!("Failed to read keybindings file: {}", path.display()))
        }
    }
}

fn read_theme_named(config_dir: &Path, name: &str) -> Result<ThemeFile> {
    let path = config_dir.join("themes").join(format!("{name}.toml"));
    match std::fs::read_to_string(&path) {
        Ok(raw) => toml::from_str(&raw)
            .with_context(|| format!("Failed to parse theme file: {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A missing `default` is normal on first run (before
            // `ensure_default_files` has had a chance to write the shipped
            // theme).  A missing *named* theme (user asked for
            // `theme = "custom"` but didn't ship the file) is worth noting,
            // though still non-fatal.
            //
            // In both cases we return the ThemeFile equivalent of
            // `Theme::default()` — NOT `ThemeFile::default()`, which is an
            // all-empty style table that would render unthemed output.
            // Distinguishing "file absent" from "file present but empty" is
            // what lets a blank theme file opt in to no styling while a
            // missing file falls back to the compiled palette.
            if name != "default" {
                tracing::warn!(
                    theme = name,
                    path = %path.display(),
                    "theme file not found; falling back to compiled defaults"
                );
            }
            Ok((&Theme::default()).into())
        }
        Err(e) => Err(e).with_context(|| format!("Failed to read theme file: {}", path.display())),
    }
}

/// Testable core of [`Config::ensure_default_files`]: given the config
/// directory (which may be a tempdir in tests), create it plus the
/// `themes/` subdirectory and write the three shipped default files if
/// absent.  Never overwrites existing files.
fn ensure_default_files_in(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!(error = %e, dir = %dir.display(), "failed to create config dir");
        return;
    }
    let themes_dir = dir.join("themes");
    if let Err(e) = std::fs::create_dir_all(&themes_dir) {
        tracing::warn!(error = %e, dir = %themes_dir.display(), "failed to create themes dir");
        return;
    }

    write_if_absent(
        &dir.join("config.toml"),
        include_str!("../../config/config.toml"),
    );
    write_if_absent(
        &dir.join("keybindings.toml"),
        include_str!("../../config/keybindings.toml"),
    );
    write_if_absent(
        &themes_dir.join("default.toml"),
        include_str!("../../config/themes/default.toml"),
    );
}

fn write_if_absent(path: &Path, contents: &str) {
    if path.exists() {
        return;
    }
    if let Err(e) = std::fs::write(path, contents) {
        tracing::warn!(error = %e, path = %path.display(), "failed to write default file");
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusBarLayout {
    /// Two rows: hint line above, persistent status below.  Default.
    TwoLine,
    /// One row: persistent status only; hints via the `?` popover.
    Compact,
}

impl Default for StatusBarLayout {
    fn default() -> Self {
        Self::TwoLine
    }
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
/// `show_drag_handles` governs whether Phase 6's row/column drag handles
/// (`≡` gutter glyph on each data row and `⇔` glyphs above each column) are
/// rendered and hit-tested.  Defaults to `true`: the renderer still checks
/// the terminal's detected `Capabilities::mouse` flag before enabling the
/// feature at runtime, so setting this to `true` on a mouseless terminal is
/// a no-op — `App::new` overrides it to `false` when `capabilities.mouse` is
/// absent so persisted config stays faithful to what the user actually sees.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TableConfig {
    pub show_drag_handles: bool,
}

impl Default for TableConfig {
    fn default() -> Self {
        Self {
            show_drag_handles: true,
        }
    }
}

/// Policy for fetching images referenced by `http(s)://` URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteImagePolicy {
    /// Prompt the user the first time a document with remote images is opened.
    Ask,
    /// Always fetch remote images without prompting.
    Always,
    /// Never fetch remote images; always fall back to the placeholder.
    Never,
}

impl Default for RemoteImagePolicy {
    fn default() -> Self {
        Self::Ask
    }
}

/// Master switch for inline image rendering.  `Ask` prompts the user the
/// first time a document with images is opened; `Always` renders without
/// prompting; `Never` keeps the `[Image: alt]` placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImagesEnabled {
    /// Prompt the user the first time a document with images is opened.
    Ask,
    /// Always render images inline.
    Always,
    /// Never render images — always fall back to the `[Image: alt]` placeholder.
    Never,
}

impl Default for ImagesEnabled {
    fn default() -> Self {
        Self::Ask
    }
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
///   * `{html}` — path to the just-generated HTML file (temp file owned
///                by the exporter; deleted after the command exits).
///   * `{out}`  — path to the final output file (source-stem with the
///                configured `extension` appended).
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
        let config = read_main_config(&path).unwrap();
        assert_eq!(config.theme, "default");
        assert_eq!(config.editor.tab_width, 4);
    }

    #[test]
    fn read_keybindings_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybindings.toml");
        let binds = read_keybindings(&path).unwrap();
        assert!(binds.0.is_empty());
    }

    #[test]
    fn read_theme_missing_default_falls_back_to_compiled_theme() {
        // The `default` theme is allowed to be missing (first run, before
        // ensure_default_files has written it).  The fallback must be the
        // compiled `Theme::default()` — NOT an empty ThemeFile — otherwise
        // the app would render unstyled output on the very first launch.
        let dir = tempfile::tempdir().unwrap();
        let theme = read_theme_named(dir.path(), "default").unwrap();
        let expected: super::super::theme_file::ThemeFile = (&Theme::default()).into();
        assert_eq!(theme.h1, expected.h1);
        assert_eq!(theme.task_strikethrough, expected.task_strikethrough);
        // Convert through to Theme and verify it equals the compiled default.
        let theme_out: Theme = (&theme).into();
        assert_eq!(theme_out.h1, Theme::default().h1);
    }

    #[test]
    fn read_theme_missing_named_falls_back_to_compiled_theme() {
        // Same contract for a missing named theme: the warning path is
        // exercised internally; here we just assert the fallback is the
        // compiled `Theme::default()`, not an empty ThemeFile.
        let dir = tempfile::tempdir().unwrap();
        let theme = read_theme_named(dir.path(), "nonexistent").unwrap();
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
        let theme = read_theme_named(dir.path(), "default").unwrap();
        assert_eq!(theme.h1, super::super::theme_file::StyleSpec::default());
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
        let config = read_main_config(&path).unwrap();
        assert_eq!(config.theme, "solarized");
        assert_eq!(config.editor.tab_width, 2);
    }

    #[test]
    fn read_keybindings_parses_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybindings.toml");
        std::fs::write(&path, "Quit = \"ctrl+x\"\n").unwrap();
        let binds = read_keybindings(&path).unwrap();
        assert_eq!(binds.0.get("Quit"), Some(&"ctrl+x".to_string()));
    }

    #[test]
    fn read_theme_parses_from_named_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        let theme_path = dir.path().join("themes").join("custom.toml");
        std::fs::write(&theme_path, "[h1]\nfg = \"red\"\nbold = true\n").unwrap();
        let theme = read_theme_named(dir.path(), "custom").unwrap();
        assert!(theme.h1.bold);
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
