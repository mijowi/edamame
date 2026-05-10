// Facade pattern: this file re-exports types from `src/config/config.rs`
// so call sites can write `use crate::config::Config` instead of
// `crate::config::config::Config`.  See CLAUDE.md "Module Facade Pattern".
#[allow(clippy::module_inception)]
pub mod config;
pub mod init;
pub mod keymap;
pub mod readers;
pub mod sections;
pub mod theme;
pub mod theme_file;
pub mod themes;
pub mod warnings;

// `pub use` re-exports through the facade. Rustc reports `CustomExportEntry`
// as "unused" because the inner `pub mod config` shadows the parent name in
// dead-code analysis, but removing it breaks resolution in `src/export/`.
#[allow(unused_imports)]
pub use config::{
    Config, ConfigWarning, CustomExportEntry, ImagesEnabled, LoadedConfig, RemoteImagePolicy,
    StatusBarLayout, WarningKind,
};
pub use keymap::{Action, KeyBindingOverrides, KeyMap, KeyMapError};
pub use theme::Theme;
pub use theme_file::ThemeFile;
