use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::keymap::KeyBindingOverrides;

/// Top-level configuration loaded from `~/.config/edamame/config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub editor: EditorConfig,
    pub theme: ThemeConfig,
    pub keybindings: KeyBindingOverrides,
    pub modal: ModalConfig,
    pub table: TableConfig,
    pub image: ImageConfig,
}

impl Config {
    /// Load config from the XDG config path, falling back to built-in defaults for
    /// any missing keys.
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if let Some(ref p) = path {
            if p.exists() {
                let raw = std::fs::read_to_string(p)
                    .with_context(|| format!("Failed to read config file: {}", p.display()))?;
                let config: Config = toml::from_str(&raw)
                    .with_context(|| format!("Failed to parse config file: {}", p.display()))?;
                return Ok(config);
            }
        }
        // No config file found — use defaults.
        Ok(Config::default())
    }

    /// Returns the path to the user config file (may not exist yet).
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("edamame").join("config.toml"))
    }

    /// Persist the current config to disk at `config_path()`.
    ///
    /// Creates the parent directory if needed.  Returns an error when the
    /// path can't be determined or the write fails; callers typically log
    /// the error and continue rather than making it fatal.
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

    /// Returns the path to the log directory.
    pub fn log_dir() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("edamame"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorConfig {
    /// Number of spaces per tab stop.
    pub tab_width: usize,
    /// When true, write structured logs to the data directory.
    pub dev_mode: bool,
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
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            tab_width: 4,
            dev_mode: false,
            code_block_wrap: false,
            line_wrap: true,
            preserve_blank_lines: true,
            visual_line_nav: true,
            suppress_capability_warnings: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    // Theme colour overrides — full theming is a deferred feature.
    // The Theme struct in theme.rs provides all defaults.
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

/// Image-rendering configuration.
///
/// `max_width` / `max_height` are ceilings in terminal cells; each image
/// reserves at most this many rows, and the inline renderer clamps to this
/// width so a single oversized image never takes over the viewport.  Values
/// are applied verbatim by `ratatui_image`'s `Resize::Fit` path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageConfig {
    /// Master switch — set to `false` to disable all image rendering.
    pub enabled: bool,
    /// Maximum width (in terminal cells) for a single image.
    pub max_width: usize,
    /// Maximum height (in terminal cells) for a single image.
    pub max_height: usize,
    /// Policy for fetching `http(s)://` images.
    pub remote_policy: RemoteImagePolicy,
}

impl Default for ImageConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_width: 80,
            max_height: 24,
            remote_policy: RemoteImagePolicy::Ask,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = Config::default();
        assert_eq!(config.editor.tab_width, 4);
        assert!(!config.editor.dev_mode);
        assert_eq!(config.modal.handler, "default");
    }

    #[test]
    fn config_round_trips_toml() {
        let config = Config::default();
        let serialized = toml::to_string(&config).expect("serialize");
        let deserialized: Config = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(deserialized.editor.tab_width, config.editor.tab_width);
        assert_eq!(deserialized.modal.handler, config.modal.handler);
    }

    #[test]
    fn partial_toml_falls_back_to_defaults() {
        let toml = "[editor]\ndev_mode = true\n";
        let config: Config = toml::from_str(toml).expect("deserialize");
        assert!(config.editor.dev_mode);
        assert_eq!(config.editor.tab_width, 4); // default
        assert_eq!(config.modal.handler, "default"); // default
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
}
