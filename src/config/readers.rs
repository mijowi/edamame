//! TOML readers for the three config files.
//!
//! Each reader is fail-soft: missing files become defaults, parse errors
//! and unknown keys are collected into a [`ConfigWarning`] vector that the
//! App surfaces in a startup modal.  See [`read_and_warn`] for the shared
//! read → deserialize-with-unknown-keys → warn loop.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use super::config::Config;
use super::keymap::{parse_key, Action, KeyBindingOverrides};
use super::sections::{
    AUTOSAVE_IDLE_MS_DEFAULT, AUTOSAVE_IDLE_MS_MAX_EXCLUSIVE, AUTOSAVE_IDLE_MS_MIN_EXCLUSIVE,
};
use super::theme::Theme;
use super::theme_file::ThemeFile;
use super::warnings::{ConfigWarning, WarningKind};

/// Every `.css` file in `<config_dir>/export/`, sorted by path.  The scaffolded
/// `default.css.example` is deliberately excluded by the extension filter — it's a
/// fork-able template, not a selectable stylesheet.
///
/// Empty for a missing directory or a `--no-config` run (see
/// [`crate::config::persistence`]); callers then use the compiled-in stylesheet.
pub fn list_export_stylesheets(config_dir: &Path) -> Vec<PathBuf> {
    if !super::persistence::config_reads_allowed() {
        return Vec::new();
    }
    let export_dir = config_dir.join("export");
    let Ok(entries) = std::fs::read_dir(&export_dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("css"))
        })
        .collect();
    files.sort();
    files
}

/// Parse TOML into `T`, also returning the dotted-path keys no field on `T` consumed.
/// Unlike `toml::from_str`, success here doesn't imply a clean file — the caller checks
/// the returned `Vec` and warns if it is non-empty.
fn deserialize_with_unknown_keys<'de, T>(
    raw: &'de str,
) -> std::result::Result<(T, Vec<String>), toml::de::Error>
where
    T: serde::de::Deserialize<'de>,
{
    let mut unknown: Vec<String> = Vec::new();
    // toml 1.x parses eagerly here, so a malformed document surfaces before the
    // `serde_ignored` walk below.
    let de = toml::Deserializer::parse(raw)?;
    let value = serde_ignored::deserialize(de, |path| unknown.push(path.to_string()))?;
    Ok((value, unknown))
}

/// Read `path`, deserialize into `T`, and push any warnings.  Missing → `on_missing()`
/// with no warning; IO or parse failure → `on_parse_failure()` + `ParseError`; unknown
/// keys → the parsed value + `UnknownKeys`.  The two fallbacks are separate so callers
/// can treat the missing-file path differently.
///
/// `ignored` lists dotted keys to drop silently from the unknown set — keys a struct field no
/// longer consumes but which are known-legacy and migrated elsewhere, so they must not raise a
/// warning modal (see the derived list in [`read_main_config`]).
fn read_and_warn<T, M, F>(
    path: &Path,
    warnings: &mut Vec<ConfigWarning>,
    ignored: &[&str],
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
        Ok((value, mut unknown)) => {
            unknown.retain(|k| !ignored.contains(&k.as_str()));
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

/// Read `config.toml` via [`read_and_warn`], then apply [`validate_main_config`].
pub(super) fn read_main_config(path: &Path, warnings: &mut Vec<ConfigWarning>) -> Config {
    // The bookkeeping keys migrated to `state.toml` are filtered out of the unknown-key warning,
    // so an un-migrated file (including one a `--no-config` run can't rewrite) never warns about
    // keys edamame itself moved.  Derived from `BOOKKEEPING_KEYS_BARE` and prefixed with the
    // `editor.` table, so the two never drift.
    let ignored: Vec<String> = super::config::BOOKKEEPING_KEYS_BARE
        .iter()
        .map(|k| format!("editor.{k}"))
        .collect();
    let ignored: Vec<&str> = ignored.iter().map(String::as_str).collect();
    let mut config: Config =
        read_and_warn(path, warnings, &ignored, Config::default, Config::default);
    validate_main_config(path, &mut config, warnings);
    config
}

/// Post-deserialization sanity checks: reset the offending field and push a
/// [`WarningKind::InvalidValue`].  Keep this list short — prefer a runtime clamp at the
/// use site (e.g. `MAX_WIDTH_COLS_MIN`); this path is only for values that would be
/// actively confusing (autosave firing on every keystroke at `idle_ms = 0`).
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

    validate_custom_exports(path, config, warnings);
}

/// Warn once per unusable `[[export.custom]]` entry.
///
/// **Reports, but does not remove.**  The loader's result is written back on the next
/// `Config::save`, so a `retain` here would erase the very lines the warning asks the user
/// to fix.  The palette instead filters rows on
/// [`crate::config::CustomExportEntry::config_problem`].
fn validate_custom_exports(path: &Path, config: &mut Config, warnings: &mut Vec<ConfigWarning>) {
    for (index, entry) in config.export.custom.iter().enumerate() {
        if let Some(message) = entry.config_problem() {
            warnings.push(ConfigWarning {
                path: path.to_path_buf(),
                kind: WarningKind::InvalidValue {
                    key: format!("export.custom[{index}]"),
                    message: format!("{message}; this export is not offered in the palette"),
                },
            });
        }
    }
}

/// Read `keybindings.toml`.  Beyond the usual parse paths, every entry is validated
/// against `Action` and `parse_key`; bad entries are stripped and reported under one
/// `InvalidKeybindings` warning, so the live keymap holds only usable bindings.
pub(super) fn read_keybindings(
    path: &Path,
    warnings: &mut Vec<ConfigWarning>,
) -> KeyBindingOverrides {
    let mut overrides: KeyBindingOverrides = read_and_warn(
        path,
        warnings,
        &[],
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

/// Substituted when the active theme is missing and `truecolor` is `true`.
pub const TRUECOLOR_FALLBACK_THEME: &str = "Edamame";
/// Substituted when the active theme is missing and `truecolor` is `false`; renders
/// faithfully on 256-color and even 16-color terminals.
pub const INDEXED_FALLBACK_THEME: &str = "256 Dark";

/// The built-in substituted when no theme file is read: [`TRUECOLOR_FALLBACK_THEME`] on a
/// 24-bit terminal, [`INDEXED_FALLBACK_THEME`] otherwise.
pub fn fallback_theme_name(truecolor: bool) -> &'static str {
    if truecolor {
        TRUECOLOR_FALLBACK_THEME
    } else {
        INDEXED_FALLBACK_THEME
    }
}

/// The built-in [`fallback_theme_name`] names.
pub fn fallback_theme(truecolor: bool) -> Theme {
    Theme::builtin(fallback_theme_name(truecolor)).expect("built-in fallback name is valid")
}

/// Read the active theme file.
///
/// `Some(name)` in the second slot means the requested theme was missing on disk (and
/// wasn't a built-in) and `name` was substituted; the caller must persist that rename to
/// `config.toml` so the substitution doesn't recur.  Every other case yields `None`,
/// including a parse error (the same built-in, unpersisted) and a blank file (a valid opt-out of
/// styling).  `truecolor` picks between the two fallback constants.
pub(super) fn read_theme_named(
    config_dir: &Path,
    name: &str,
    truecolor: bool,
    warnings: &mut Vec<ConfigWarning>,
) -> (ThemeFile, Option<String>) {
    // Built-ins win on name collision: `themes/default.toml` is ignored if `default` is one.
    if let Some(theme) = Theme::builtin(name) {
        return ((&theme).into(), None);
    }

    // Past this point every branch reads `themes/<name>.toml`, which a `--no-config` run
    // must not do.  Defense in depth behind `list_theme_names`, for a stale `config.theme`
    // that didn't come through the picker.  `None`, not `Some(fallback)`: nothing was read,
    // so there is no rename to persist — and no `MissingTheme` warning, since the file is
    // excluded rather than missing.
    if !super::persistence::config_reads_allowed() {
        let fallback = fallback_theme_name(truecolor);
        tracing::debug!(
            theme = name,
            fallback,
            "--no-config: not reading a user theme file; using the built-in fallback"
        );
        return ((&fallback_theme(truecolor)).into(), None);
    }

    let path = config_dir.join("themes").join(format!("{name}.toml"));
    // Detect "missing" up front so it gets a capability-aware built-in and a
    // `MissingTheme` warning, and a rename to persist, unlike a file that fails to parse.
    if !path.exists() {
        let fallback = fallback_theme_name(truecolor);
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
        return (
            (&fallback_theme(truecolor)).into(),
            Some(fallback.to_string()),
        );
    }

    // Bypasses `read_and_warn`: its `on_missing` branch is unreachable after the
    // existence check above.
    let theme_fallback = || ((&fallback_theme(truecolor)).into(), None);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) => {
            warnings.push(ConfigWarning {
                path: path.clone(),
                kind: WarningKind::ParseError(format!("Failed to read file: {e}")),
            });
            return theme_fallback();
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
            theme_fallback()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CustomExportEntry;

    /// Validate the given custom-export entries; report what survived plus the warnings.
    fn validated(entries: Vec<CustomExportEntry>) -> (Vec<CustomExportEntry>, Vec<ConfigWarning>) {
        let mut config = Config::default();
        config.export.custom = entries;
        let mut warnings = Vec::new();
        validate_custom_exports(Path::new("config.toml"), &mut config, &mut warnings);
        (config.export.custom, warnings)
    }

    fn entry(name: &str, command: &[&str], extension: &str) -> CustomExportEntry {
        CustomExportEntry {
            name: name.to_owned(),
            command: command.iter().map(|s| (*s).to_owned()).collect(),
            extension: extension.to_owned(),
        }
    }

    #[test]
    fn a_usable_custom_export_survives_untouched() {
        let good = entry("PDF", &["pandoc", "{html}", "-o", "{out}"], "pdf");
        let (kept, warnings) = validated(vec![good.clone()]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, good.name);
        assert_eq!(kept[0].command, good.command);
        assert_eq!(kept[0].extension, good.extension);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// Kept, not removed: deleting would erase the user's block on the next `Config::save`.
    #[test]
    fn every_unusable_custom_export_is_reported_but_kept() {
        for (label, bad) in [
            ("no name", entry("", &["pandoc"], "pdf")),
            ("blank name", entry("   ", &["pandoc"], "pdf")),
            ("no command", entry("PDF", &[], "pdf")),
            ("no extension", entry("PDF", &["pandoc"], "")),
            ("blank extension", entry("PDF", &["pandoc"], "  ")),
            ("dot-only extension", entry("PDF", &["pandoc"], ".")),
            ("path in extension", entry("PDF", &["pandoc"], "../out.pdf")),
        ] {
            let (kept, warnings) = validated(vec![bad]);
            assert_eq!(kept.len(), 1, "{label} should be kept, not removed");
            assert_eq!(warnings.len(), 1, "{label} should warn exactly once");
            match &warnings[0].kind {
                WarningKind::InvalidValue { key, .. } => {
                    assert_eq!(key, "export.custom[0]", "{label}")
                }
                other => panic!("{label}: expected InvalidValue, got {other:?}"),
            }
        }
    }

    /// The index in the message is the offender's own position — the palette builds rows
    /// from it, so an off-by-one would point the user at working config.
    #[test]
    fn a_bad_entry_is_reported_at_its_own_index_without_taking_its_neighbours() {
        let (kept, warnings) = validated(vec![
            entry("PDF", &["pandoc"], "pdf"),
            entry("broken", &[], "docx"),
            entry("DOCX", &["pandoc"], "docx"),
        ]);
        assert_eq!(kept.len(), 3, "no entry is removed");
        assert_eq!(warnings.len(), 1);
        match &warnings[0].kind {
            WarningKind::InvalidValue { key, .. } => assert_eq!(key, "export.custom[1]"),
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn output_extension_normalizes_dot_and_whitespace() {
        assert!(entry("PDF", &["pandoc"], " pdf ")
            .config_problem()
            .is_none());
        assert_eq!(entry("PDF", &["pandoc"], " pdf ").output_extension(), "pdf");
        assert_eq!(entry("PDF", &["pandoc"], ".pdf").output_extension(), "pdf");
        assert!(entry("PDF", &["pandoc"], ".").config_problem().is_some());
    }

    /// The shipped `config/config.toml` is copied verbatim into every new user's config
    /// directory, so a typo in it greets a first-time user with a warning modal.
    #[test]
    fn shipped_reference_config_loads_without_warnings() {
        let raw = super::super::init::REFERENCE_CONFIG_TOML;
        let (_config, unknown): (Config, Vec<String>) = deserialize_with_unknown_keys(raw)
            .expect("the shipped config/config.toml must parse as a Config");
        assert!(
            unknown.is_empty(),
            "config/config.toml documents keys that no longer exist: {unknown:?}"
        );
    }

    /// The test above parses the file as shipped, where a key renamed out from under its
    /// `# key = value` line is just a comment — the user who uncomments it finds out.  So
    /// uncomment each example in turn and check it against the live schema.
    #[test]
    fn shipped_reference_config_examples_are_all_uncommentable() {
        let checked = check_commented_examples(super::super::init::REFERENCE_CONFIG_TOML)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            checked > 20,
            "expected the reference config to carry commented examples; only found \
             {checked} — has the format changed?"
        );
    }

    /// The scanner is only worth having if it fails on a stale example; pin that directly.
    #[test]
    fn commented_example_scanner_rejects_a_key_that_no_longer_exists() {
        let good = "[editor]\n# line_wrap = true\n# [dev]\n# logging = false\n";
        assert_eq!(check_commented_examples(good), Ok(2));

        let stale = "[editor]\n# line_wrap_renamed = true\n";
        let err = check_commented_examples(stale).unwrap_err();
        assert!(
            err.contains("line_wrap_renamed") && err.contains("[editor]"),
            "unhelpful failure message: {err}"
        );

        let misplaced = "# [dev]\n# line_wrap = true\n";
        assert!(check_commented_examples(misplaced).is_err());

        let prose = "[editor]\n# Wrap long lines. Default: true.\n#     # a .css file.\n";
        assert_eq!(check_commented_examples(prose), Ok(0));

        // Bracketed prose must not be adopted as a section header.
        let bracket_prose = "[editor]\n# [ ] a task\n# line_wrap = true\n";
        assert_eq!(check_commented_examples(bracket_prose), Ok(1));

        assert_eq!(
            table_header("[export.html]").as_deref(),
            Some("[export.html]")
        );
        assert_eq!(
            table_header("[[export.custom]]").as_deref(),
            Some("[[export.custom]]")
        );
        assert_eq!(table_header("[see the note above]"), None);
        assert_eq!(table_header("[]"), None);
        assert_eq!(table_header("[editor"), None);
    }

    /// `[table]` / `[[array.of.tables]]` if `line` is exactly a TOML table header.
    ///
    /// The bracket content must be a dotted run of bare-key characters — that is what
    /// separates a header from bracketed prose (`# [ ] a task`), which would otherwise be
    /// adopted as the current section.
    fn table_header(line: &str) -> Option<String> {
        let inner = line
            .strip_prefix("[[")
            .and_then(|l| l.strip_suffix("]]"))
            .or_else(|| line.strip_prefix('[').and_then(|l| l.strip_suffix(']')))?;
        let bare = !inner.is_empty()
            && inner.split('.').all(|seg| {
                !seg.is_empty()
                    && seg
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            });
        bare.then(|| line.to_owned())
    }

    /// Uncomment every `# key = value` example and check it against the live `Config`
    /// schema, scoped to its table.  Returns how many were checked, or the first failure.
    ///
    /// Section tracking is a line scan, not a TOML parse: a commented `# [section]` header
    /// claims every commented example below it until the next header.  A commented header
    /// inside a live table would misattribute what follows — but that *fails* the test,
    /// so the failure mode is a visible false alarm, never a skipped check.
    fn check_commented_examples(raw: &str) -> Result<usize, String> {
        let mut section = String::new();
        let mut checked = 0;

        for (lineno, line) in raw.lines().enumerate() {
            let trimmed = line.trim();
            // A live table header, or a commented-out one (`# [dev]`).
            let candidate = trimmed.strip_prefix('#').map_or(trimmed, str::trim);
            if let Some(header) = table_header(candidate) {
                section = header;
                continue;
            }
            let Some(body) = trimmed.strip_prefix('#').map(str::trim) else {
                continue;
            };
            let Some((key, _)) = body.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if key.is_empty()
                || !key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            {
                continue;
            }

            let doc = format!("{section}\n{body}\n");
            let (_config, unknown): (Config, Vec<String>) = deserialize_with_unknown_keys(&doc)
                .map_err(|e| {
                    format!(
                        "config.toml line {}: uncommenting `{body}` under `{section}` \
                         does not parse: {e}",
                        lineno + 1
                    )
                })?;
            if !unknown.is_empty() {
                return Err(format!(
                    "config.toml line {}: `{key}` under `{section}` is no longer a real \
                     setting (reported unknown: {unknown:?})",
                    lineno + 1
                ));
            }
            checked += 1;
        }

        Ok(checked)
    }

    /// Every commented example in `config/keybindings.toml` must be one the user can
    /// actually uncomment.  It has been wrong before: it used to present `Action = ""` as
    /// the way to leave something unbound, which is a parse error that drops the entry.
    #[test]
    fn shipped_reference_keybindings_are_all_uncommentable() {
        let raw = include_str!("../../config/keybindings.toml");
        let mut checked = 0;
        for line in raw.lines() {
            let Some(body) = line.trim_start().strip_prefix('#') else {
                continue;
            };
            let body = body.trim();
            let Some((name, value)) = body.split_once('=') else {
                continue;
            };
            let (name, value) = (name.trim(), value.trim());
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
                continue;
            }
            let Some(chord) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
                continue;
            };
            Action::from_str(name)
                .unwrap_or_else(|_| panic!("keybindings.toml names unknown action `{name}`"));
            parse_key(chord).unwrap_or_else(|_| {
                panic!("keybindings.toml shows `{name} = \"{chord}\"`, which does not parse")
            });
            checked += 1;
        }
        assert!(
            checked > 10,
            "expected the reference keybindings file to carry example bindings; \
             only found {checked} — has the format changed?"
        );
    }

    /// The second half proves the empty result came from the gate, not an empty folder.
    #[test]
    fn export_stylesheets_are_not_listed_while_the_config_dir_is_disabled() {
        let _lock = crate::test_env::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let export = dir.path().join("export");
        std::fs::create_dir_all(&export).unwrap();
        std::fs::write(export.join("mine.css"), "").unwrap();

        {
            let _disabled = crate::config::persistence::SuppressGuard::new();
            assert!(list_export_stylesheets(dir.path()).is_empty());
        }
        assert_eq!(list_export_stylesheets(dir.path()).len(), 1);
    }

    /// Even handed a custom theme name directly, a disabled run reads no file and warns
    /// nothing — excluded is not missing.
    #[test]
    fn a_user_theme_is_not_read_while_the_config_dir_is_disabled() {
        let _lock = crate::test_env::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(themes.join("mine.toml"), "[h1]\nfg = \"red\"\n").unwrap();

        // `ThemeFile` has no `PartialEq`; its TOML rendering stands in.
        let render = |f: &ThemeFile| toml::to_string(f).expect("theme file serialises");
        let builtin = render(&(&Theme::builtin(TRUECOLOR_FALLBACK_THEME).unwrap()).into());

        let mut warnings = Vec::new();
        {
            let _disabled = crate::config::persistence::SuppressGuard::new();
            let (file, fallback) = read_theme_named(dir.path(), "mine", true, &mut warnings);
            assert_eq!(fallback, None, "nothing to persist — the file went unread");
            assert!(warnings.is_empty(), "excluded is not missing: {warnings:?}");
            assert_eq!(render(&file), builtin);
        }

        // Ungated, the same call reads the file — so the above is about the gate.
        let (file, fallback) = read_theme_named(dir.path(), "mine", true, &mut warnings);
        assert_eq!(fallback, None);
        assert_ne!(
            render(&file),
            builtin,
            "the user theme should have been read"
        );
    }

    #[test]
    fn list_export_stylesheets_finds_css_sorted_and_ignores_others() {
        let _lock = crate::test_env::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let export = dir.path().join("export");
        std::fs::create_dir_all(&export).unwrap();
        std::fs::write(export.join("zebra.css"), "").unwrap();
        std::fs::write(export.join("default.css"), "").unwrap();
        std::fs::write(export.join("notes.txt"), "").unwrap();
        std::fs::write(export.join("UPPER.CSS"), "").unwrap();
        std::fs::write(export.join("default.css.example"), "").unwrap();

        let found = list_export_stylesheets(dir.path());
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["UPPER.CSS", "default.css", "zebra.css"]);
    }

    #[test]
    fn list_export_stylesheets_missing_dir_is_empty() {
        let _lock = crate::test_env::env_lock();
        let dir = tempfile::tempdir().unwrap();
        assert!(list_export_stylesheets(dir.path()).is_empty());
    }
}
