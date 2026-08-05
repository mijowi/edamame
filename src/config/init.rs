//! First-run scaffolding: write the shipped default config files
//! (`config.toml`, `keybindings.toml`, `export/default.css.example`) into
//! the user's config directory, only when each file is absent.

use std::path::Path;

/// The annotated reference `config.toml` compiled into the binary.
///
/// Seeded on first run by [`ensure_default_files_in`], and used again as
/// the merge base by [`super::config::save_merge`] whenever the user's
/// file is missing at save time — otherwise a save that raced a deleted
/// `config.toml` would emit a bare serialization and strip every comment
/// permanently (each later save then faithfully merges into the
/// de-annotated file).
pub(super) const REFERENCE_CONFIG_TOML: &str = include_str!("../../config/config.toml");

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

    // The export stylesheet folder mirrors `themes/`: an (initially empty)
    // place for users to drop custom `.css` files, each of which becomes a
    // pick in the Export HTML modal.  The single built-in default is the
    // frozen compiled-in stylesheet (`export::html::BUILTIN_STYLESHEET`),
    // so we deliberately do NOT write a selectable `default.css` here — that
    // would surface a second, identical "default" in the picker.  Instead we
    // seed a `.example` reference (excluded from the picker by
    // `list_export_stylesheets`'s `.css` filter): a fork-able starting point
    // the user copies to `<name>.css` and edits.
    let export_dir = dir.join("export");
    if let Err(e) = std::fs::create_dir_all(&export_dir) {
        tracing::warn!(error = %e, dir = %export_dir.display(), "failed to create export dir");
        return;
    }

    write_if_absent(&dir.join("config.toml"), REFERENCE_CONFIG_TOML);
    write_if_absent(
        &dir.join("keybindings.toml"),
        include_str!("../../config/keybindings.toml"),
    );
    write_if_absent(
        &export_dir.join("default.css.example"),
        include_str!("../../config/export/default.css"),
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
