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
use super::sections::{
    AUTOSAVE_IDLE_MS_DEFAULT, AUTOSAVE_IDLE_MS_MAX_EXCLUSIVE, AUTOSAVE_IDLE_MS_MIN_EXCLUSIVE,
};
use super::theme::Theme;
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
    // toml 1.x parses eagerly in `Deserializer::parse`, returning a
    // `Result`; a malformed document surfaces here rather than during
    // the `serde_ignored::deserialize` walk below.
    let de = toml::Deserializer::parse(raw)?;
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
    let mut config: Config = read_and_warn(path, warnings, Config::default, Config::default);
    validate_main_config(path, &mut config, warnings);
    config
}

/// Post-deserialization sanity checks for `config.toml`.  Each rule
/// resets the offending field to its default and pushes a
/// [`WarningKind::InvalidValue`] so the user sees that their requested
/// value didn't take effect.  Keep this list short — runtime-clamp at
/// the use site (see e.g. `MAX_WIDTH_COLS_MIN`) is preferred for fields
/// where any value still produces a sensible UI; this path is for
/// fields where an out-of-range value would be actively confusing
/// (autosave firing on every keystroke at `idle_ms = 0`, etc.).
fn validate_main_config(path: &Path, config: &mut Config, warnings: &mut Vec<ConfigWarning>) {
    let idle = config.editor.autosave_idle_ms;
    if idle <= AUTOSAVE_IDLE_MS_MIN_EXCLUSIVE || idle >= AUTOSAVE_IDLE_MS_MAX_EXCLUSIVE {
        config.editor.autosave_idle_ms = AUTOSAVE_IDLE_MS_DEFAULT;
        warnings.push(ConfigWarning {
            path: path.to_path_buf(),
            kind: WarningKind::InvalidValue {
                key: "editor.autosave_idle_ms".to_string(),
                message: format!(
                    "value {idle} is outside the supported range ({} < N < {}); \
                     using the default ({AUTOSAVE_IDLE_MS_DEFAULT}) instead",
                    AUTOSAVE_IDLE_MS_MIN_EXCLUSIVE, AUTOSAVE_IDLE_MS_MAX_EXCLUSIVE,
                ),
            },
        });
    }
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

/// Built-in theme name substituted when the active theme is missing
/// and `truecolor` is `true`.  Picked because Edamame is the project's
/// canonical truecolor palette.
pub const TRUECOLOR_FALLBACK_THEME: &str = "Edamame";
/// Built-in theme name substituted when the active theme is missing
/// and `truecolor` is `false`.  Indexed-color built-in that renders
/// faithfully on 256-color and even 16-color terminals.
pub const INDEXED_FALLBACK_THEME: &str = "256 Dark";

/// Read the active theme file.
///
/// Returns the parsed [`ThemeFile`] alongside an optional fallback
/// name: `Some(name)` means the requested theme was missing on disk
/// (and wasn't a built-in), so `name` was substituted in its place.
/// The caller is responsible for persisting the rename back to
/// `config.toml` so the substitution doesn't recur on next launch.
///
/// Semantics by case:
///   - name resolves to a built-in: return that built-in, `None`.
///   - file present, parses cleanly: return parsed file, `None`.
///   - file present, parse error: compiled default, `None`
///     (existing `ParseError` warning still fires).
///   - file present, blank: empty file, `None`
///     (user opt-out of styling).
///   - file absent: built-in fallback, `Some(name)`
///     + [`WarningKind::MissingTheme`].
///
/// `truecolor` selects between [`TRUECOLOR_FALLBACK_THEME`] and
/// [`INDEXED_FALLBACK_THEME`] for the missing-file case.
pub(super) fn read_theme_named(
    config_dir: &Path,
    name: &str,
    truecolor: bool,
    warnings: &mut Vec<ConfigWarning>,
) -> (ThemeFile, Option<String>) {
    // Built-in themes always win on name collision: a user file
    // `themes/default.toml` is ignored if `default` is a built-in.
    // Custom user themes go through the disk path below.
    if let Some(theme) = Theme::builtin(name) {
        return ((&theme).into(), None);
    }

    let path = config_dir.join("themes").join(format!("{name}.toml"));
    // Detect "missing" up-front so we can apply a capability-aware
    // built-in fallback and surface a `MissingTheme` warning, rather
    // than silently degrading to the compiled `Theme::default()`.
    // The blank-file and parse-error cases still flow through
    // `read_and_warn` below — those are deliberate user states, not
    // missing-file states.
    if !path.exists() {
        let fallback = if truecolor {
            TRUECOLOR_FALLBACK_THEME
        } else {
            INDEXED_FALLBACK_THEME
        };
        tracing::warn!(
            theme = name,
            path = %path.display(),
            fallback,
            "theme file not found; substituting built-in fallback"
        );
        warnings.push(ConfigWarning {
            path: path.clone(),
            kind: WarningKind::MissingTheme {
                requested: name.to_string(),
                fallback: fallback.to_string(),
            },
        });
        let theme = Theme::builtin(fallback).expect("built-in fallback name is valid");
        return ((&theme).into(), Some(fallback.to_string()));
    }

    // File exists — read it directly.  We bypass `read_and_warn`
    // here because its `on_missing` branch is unreachable after the
    // existence check above, and unread + reparse on parse failure
    // both fall back to the compiled `Theme::default()` (a blank
    // file is a valid opt-out of styling and still parses cleanly
    // into an empty `ThemeFile`).
    let theme_default = || ((&Theme::default()).into(), None);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) => {
            warnings.push(ConfigWarning {
                path: path.clone(),
                kind: WarningKind::ParseError(format!("Failed to read file: {e}")),
            });
            return theme_default();
        }
    };
    match deserialize_with_unknown_keys::<ThemeFile>(&raw) {
        Ok((value, unknown)) => {
            if !unknown.is_empty() {
                warnings.push(ConfigWarning {
                    path: path.clone(),
                    kind: WarningKind::UnknownKeys(unknown),
                });
            }
            (value, None)
        }
        Err(e) => {
            warnings.push(ConfigWarning {
                path,
                kind: WarningKind::ParseError(e.to_string()),
            });
            theme_default()
        }
    }
}
