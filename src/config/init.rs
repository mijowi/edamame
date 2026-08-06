//! First-run scaffolding: write the shipped default config files
//! (`config.toml`, `keybindings.toml`, `export/default.css.example`) into
//! the user's config directory, only when each file is absent.

use std::path::Path;

use super::readers::{INDEXED_FALLBACK_THEME, TRUECOLOR_FALLBACK_THEME};

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
///
/// `truecolor` selects the seeded `theme` value: the reference config
/// ships [`TRUECOLOR_FALLBACK_THEME`], but on an indexed-color terminal
/// that palette quantizes badly, so a first run there is seeded with
/// [`INDEXED_FALLBACK_THEME`] instead — the same capability-appropriate
/// pair [`super::readers::read_theme_named`] falls back to.  Only the
/// first write is affected; an existing `config.toml` is never touched.
pub(super) fn ensure_default_files_in(dir: &Path, truecolor: bool) {
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

    write_if_absent(&dir.join("config.toml"), &seed_config_toml(truecolor));
    write_if_absent(
        &dir.join("keybindings.toml"),
        include_str!("../../config/keybindings.toml"),
    );
    write_if_absent(
        &export_dir.join("default.css.example"),
        include_str!("../../config/export/default.css"),
    );
}

/// The `config.toml` body to seed on first run.  Truecolor terminals get
/// the reference file verbatim; everything else gets it with the single
/// `theme = "<truecolor default>"` assignment rewritten to the
/// indexed-color built-in.  All comments and the rest of the file are
/// untouched, so the seeded file still reads as the annotated reference.
fn seed_config_toml(truecolor: bool) -> String {
    if truecolor {
        return REFERENCE_CONFIG_TOML.to_owned();
    }
    REFERENCE_CONFIG_TOML.replacen(
        &format!("theme = \"{TRUECOLOR_FALLBACK_THEME}\""),
        &format!("theme = \"{INDEXED_FALLBACK_THEME}\""),
        1,
    )
}

fn write_if_absent(path: &Path, contents: &str) {
    if path.exists() {
        return;
    }
    if let Err(e) = std::fs::write(path, contents) {
        tracing::warn!(error = %e, path = %path.display(), "failed to write default file");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_keeps_reference_verbatim_on_truecolor() {
        assert_eq!(seed_config_toml(true), REFERENCE_CONFIG_TOML);
    }

    #[test]
    fn seed_swaps_theme_for_indexed_terminals() {
        // Guards both the rewrite and the assumption it rests on: the
        // reference config must keep spelling the truecolor default as a
        // plain `theme = "…"` assignment, or the swap would silently no-op.
        assert!(REFERENCE_CONFIG_TOML.contains(&format!("theme = \"{TRUECOLOR_FALLBACK_THEME}\"")));
        let seeded = seed_config_toml(false);
        assert!(seeded.contains(&format!("theme = \"{INDEXED_FALLBACK_THEME}\"")));
        assert!(!seeded.contains(&format!("theme = \"{TRUECOLOR_FALLBACK_THEME}\"")));
    }
}
