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
}
