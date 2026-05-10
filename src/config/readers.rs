//! TOML readers for the three config files.
//!
//! Each reader is fail-soft: missing files become defaults, parse errors
//! and unknown keys are collected into a [`ConfigWarning`] vector that the
//! App surfaces in a startup modal.  See [`read_and_warn`] for the shared
//! read → deserialize-with-unknown-keys → warn loop.

use std::path::Path;
use std::str::FromStr;

use super::config::Config;
use super::keymap::{parse_key, Action, KeyBindingOverrides};
use super::theme::{Palette, Theme};
use super::theme_file::ThemeFile;
use super::warnings::{ConfigWarning, WarningKind};

/// Parse a TOML payload into `T`, recording unknown keys via
/// `serde_ignored`.  Returns the parsed struct and the list of
/// dotted-path keys that no field on `T` consumed.  Unlike
/// `toml::from_str`, success here doesn't imply a clean file — the
/// caller checks the returned `Vec` and pushes a warning if non-empty.
fn deserialize_with_unknown_keys<'de, T>(
    raw: &'de str,
) -> std::result::Result<(T, Vec<String>), toml::de::Error>
where
    T: serde::de::Deserialize<'de>,
{
    let mut unknown: Vec<String> = Vec::new();
    let de = toml::Deserializer::new(raw);
    let value = serde_ignored::deserialize(de, |path| unknown.push(path.to_string()))?;
    Ok((value, unknown))
}

/// Read `path`, deserialize into `T`, and emit any warnings into
/// `warnings`.  Missing → `on_missing()` (no warning); IO failure →
/// `on_parse_failure()` + `ParseError` warning; toml parse error →
/// `on_parse_failure()` + `ParseError` warning; unknown keys →
/// parsed value + `UnknownKeys` warning.  The two fallbacks are
/// separate so callers like [`read_theme_named`] can attach a
/// `tracing::warn!` to the missing-file path only.
fn read_and_warn<T, M, F>(
    path: &Path,
    warnings: &mut Vec<ConfigWarning>,
    on_missing: M,
    on_parse_failure: F,
) -> T
where
    T: serde::de::DeserializeOwned,
    M: FnOnce() -> T,
    F: FnOnce() -> T,
{
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return on_missing(),
        Err(e) => {
            warnings.push(ConfigWarning {
                path: path.to_path_buf(),
                kind: WarningKind::ParseError(format!("Failed to read file: {e}")),
            });
            return on_parse_failure();
        }
    };
    match deserialize_with_unknown_keys::<T>(&raw) {
        Ok((value, unknown)) => {
            if !unknown.is_empty() {
                warnings.push(ConfigWarning {
                    path: path.to_path_buf(),
                    kind: WarningKind::UnknownKeys(unknown),
                });
            }
            value
        }
        Err(e) => {
            warnings.push(ConfigWarning {
                path: path.to_path_buf(),
                kind: WarningKind::ParseError(e.to_string()),
            });
            on_parse_failure()
        }
    }
}

/// Read `config.toml`.  Missing → defaults; parse error → defaults +
/// `ParseError` warning; unknown keys → parsed value + `UnknownKeys`
/// warning.  IO errors other than NotFound also produce a `ParseError`
/// warning so the user always sees the failure path.
pub(super) fn read_main_config(path: &Path, warnings: &mut Vec<ConfigWarning>) -> Config {
    read_and_warn(path, warnings, Config::default, Config::default)
}

/// Read `keybindings.toml`.  In addition to the usual parse-error /
/// unknown-key paths, every entry is validated against the `Action`
/// enum and `parse_key`; bad entries are stripped and reported under a
/// single `InvalidKeybindings` warning so the live keymap only contains
/// usable bindings.
pub(super) fn read_keybindings(
    path: &Path,
    warnings: &mut Vec<ConfigWarning>,
) -> KeyBindingOverrides {
    let mut overrides: KeyBindingOverrides = read_and_warn(
        path,
        warnings,
        KeyBindingOverrides::default,
        KeyBindingOverrides::default,
    );
    let mut errors = Vec::new();
    overrides.0.retain(|action_str, key_str| {
        if let Err(e) = Action::from_str(action_str) {
            errors.push(format!("{action_str} = \"{key_str}\": {e}"));
            return false;
        }
        if let Err(e) = parse_key(key_str) {
            errors.push(format!("{action_str} = \"{key_str}\": {e}"));
            return false;
        }
        true
    });
    if !errors.is_empty() {
        warnings.push(ConfigWarning {
            path: path.to_path_buf(),
            kind: WarningKind::InvalidKeybindings(errors),
        });
    }
    overrides
}

/// Read the active theme file.  Missing-file semantics are unchanged
/// (compiled `Theme::default()` for absent default and named themes;
/// blank file stays blank by user choice).  Parse errors and unknown
/// keys flow through the warning vector the same way as `config.toml`.
pub(super) fn read_theme_named(
    config_dir: &Path,
    name: &str,
    warnings: &mut Vec<ConfigWarning>,
) -> ThemeFile {
    // Built-in themes always win on name collision: a user file
    // `themes/default.toml` is ignored if `default` is a built-in.
    // Custom user themes go through the disk path below.
    if let Some(palette) = Palette::builtin(name) {
        return (&Theme::from_palette(&palette)).into();
    }

    let path = config_dir.join("themes").join(format!("{name}.toml"));
    // ThemeFile fallback differs from `ThemeFile::default()`: a blank
    // file is a valid opt-out of styling, but a *missing* file means
    // we should render with the compiled palette.
    let theme_default = || (&Theme::default()).into();
    let on_missing = || {
        tracing::warn!(
            theme = name,
            path = %path.display(),
            "theme file not found; falling back to compiled defaults"
        );
        theme_default()
    };
    read_and_warn(&path, warnings, on_missing, theme_default)
}
