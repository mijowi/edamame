//! First-run scaffolding: write the three shipped default config files
//! into the user's config directory, only when each file is absent.

use std::path::Path;

use super::theme_file::default_theme_toml;

/// Testable core of [`super::config::Config::ensure_default_files`]:
/// given the config directory (which may be a tempdir in tests), create
/// it plus the `themes/` subdirectory and write the three shipped
/// default files if absent.  Never overwrites existing files.
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
    // `default.toml` is generated from `Theme::default()` /
    // `Palette::default()` rather than shipped as a checked-in file, so
    // the on-disk default can never drift from the compiled-in one.
    write_if_absent(&themes_dir.join("default.toml"), &default_theme_toml());
}

fn write_if_absent(path: &Path, contents: &str) {
    if path.exists() {
        return;
    }
    if let Err(e) = std::fs::write(path, contents) {
        tracing::warn!(error = %e, path = %path.display(), "failed to write default file");
    }
}
