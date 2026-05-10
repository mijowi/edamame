//! First-run scaffolding: write the three shipped default config files
//! into the user's config directory, only when each file is absent.

use std::path::Path;

/// Testable core of [`super::config::Config::ensure_default_files`]:
/// given the config directory (which may be a tempdir in tests), create
/// it plus the `themes/` subdirectory and write the shipped default
/// files if absent.  Never overwrites existing files.
///
/// Built-in themes (see [`super::theme::BUILTIN_THEMES`]) are compiled
/// into the binary and resolved before any disk read, so this function
/// does NOT write `themes/<builtin>.toml` files.  The `themes/`
/// directory is still created so an empty folder exists for users (or
/// future export actions) to drop custom theme files into.
pub(super) fn ensure_default_files_in(dir: &Path) {
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
}

fn write_if_absent(path: &Path, contents: &str) {
    if path.exists() {
        return;
    }
    if let Err(e) = std::fs::write(path, contents) {
        tracing::warn!(error = %e, path = %path.display(), "failed to write default file");
    }
}
