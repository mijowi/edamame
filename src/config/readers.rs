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

/// Discover user-droppable export stylesheets in `<config_dir>/export/`.
///
/// Returns every `.css` file in that folder, sorted by path.  The
/// `default.css.example` reference scaffolded on first run is deliberately
/// excluded by the `.css` extension filter — it's a fork-able template, not
/// a selectable stylesheet.  A missing or unreadable directory yields an
/// empty vector — callers fall back to the compiled-in `Builtin` stylesheet.
///
/// A `--no-config` run always yields the empty vector: the export folder
/// is part of the config directory this run has taken out of play, and
/// the compiled-in stylesheet is exactly the built-in default that flag
/// asks for.  See [`crate::config::persistence`].
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

    validate_custom_exports(path, config, warnings);
}

/// Warn once for every `[[export.custom]]` entry that could not produce a
/// working palette command.
///
/// **Reports, but does not remove.**  Unlike the range checks above there
/// is no default to fall back on, but *deleting* the entry from `config`
/// would be worse than the problem: the loader's result is written back on
/// the next `Config::save`, so a `retain` here erases the user's block from
/// their `config.toml` — the very lines the warning is asking them to fix.
/// The entry stays put and is instead excluded from the palette at the
/// point rows are built ([`crate::config::CustomExportEntry::config_problem`]
/// is the shared predicate), so a row that cannot run is never offered
/// while the config on disk is left intact.
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

    // Past this point every branch reads `themes/<name>.toml`, which a
    // `--no-config` run must not do.  Defense in depth: `list_theme_names`
    // offers built-ins only under the same gate, so a custom name should
    // never reach here — but if one does (a stale `config.theme` from a
    // caller that didn't come through the picker), fall back to the
    // capability-appropriate built-in rather than reading the file.
    //
    // `None`, not `Some(fallback)`: the second element asks the caller to
    // persist the rename, and this run neither wrote nor read that file.
    // No `MissingTheme` warning either — nothing is missing, it is
    // excluded, and a modal about it would be noise on every launch.
    if !super::persistence::config_reads_allowed() {
        let fallback = if truecolor {
            TRUECOLOR_FALLBACK_THEME
        } else {
            INDEXED_FALLBACK_THEME
        };
        tracing::debug!(
            theme = name,
            fallback,
            "--no-config: not reading a user theme file; using the built-in fallback"
        );
        let theme = Theme::builtin(fallback).expect("built-in fallback name is valid");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CustomExportEntry;

    /// Build a `Config` with the given custom-export entries, run it
    /// through validation, and report what survived plus the warnings.
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

    /// A well-formed entry warns about nothing and is left exactly as
    /// written.
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

    /// Each unusable shape warns once, naming its index — but the entry is
    /// **kept**, not removed.  Deleting it would erase the user's block on
    /// the next `Config::save`; the palette excludes it instead
    /// (`CustomExportEntry::config_problem`), so the config on disk is left
    /// intact for the user to fix.
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

    /// One bad entry must not cost the user a warning against the wrong
    /// line: the index the message carries is the offender's *own*
    /// position, which is what the palette builds its rows from, so an
    /// off-by-one would point the user at working config.  All three are
    /// kept — only the palette-offering is filtered.
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

    /// A tolerated-but-nonempty extension is not a problem, and the value
    /// actually used for the filename is normalized: whitespace and a
    /// single leading dot are stripped.
    #[test]
    fn output_extension_normalizes_dot_and_whitespace() {
        assert!(entry("PDF", &["pandoc"], " pdf ")
            .config_problem()
            .is_none());
        assert_eq!(entry("PDF", &["pandoc"], " pdf ").output_extension(), "pdf");
        assert_eq!(entry("PDF", &["pandoc"], ".pdf").output_extension(), "pdf");
        // Trims to empty → a real problem, caught before it can unname the file.
        assert!(entry("PDF", &["pandoc"], ".").config_problem().is_some());
    }

    /// The shipped `config/config.toml` is copied verbatim into every new
    /// user's config directory, so a typo in it greets a first-time user
    /// with a warning modal.  It must deserialize cleanly *and* leave no
    /// unknown keys — the same two checks `read_and_warn` performs at
    /// startup.
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

    /// Most of `config/config.toml` is *commented* examples, and the test
    /// above can't see any of them — it parses the file as shipped, where
    /// a key renamed out from under its `# key = value` line is just a
    /// comment.  The user who uncomments it is the one who finds out.
    ///
    /// So uncomment each example in turn and put it through the same
    /// deserialize-and-report-unknown-keys check, scoped to whichever
    /// table it sits under (tracking commented `# [section]` headers as
    /// well as live ones).  This is the config-side counterpart to
    /// `shipped_reference_keybindings_are_all_uncommentable`.
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

    /// The scanner above is only worth having if it fails on a stale
    /// example, so pin that directly: a key that no longer exists must be
    /// reported, and the live/commented section tracking must attribute it
    /// to the right table.
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

        // A real key, but filed under the wrong table — the same failure.
        let misplaced = "# [dev]\n# line_wrap = true\n";
        assert!(check_commented_examples(misplaced).is_err());

        // Prose and trailing-comment continuations must not be mistaken
        // for examples.
        let prose = "[editor]\n# Wrap long lines. Default: true.\n#     # a .css file.\n";
        assert_eq!(check_commented_examples(prose), Ok(0));

        // Bracketed *prose* must not be adopted as a section header — doing
        // so would check every example after it against a table that does
        // not exist.  `# [ ] a task` looks like one to a naive scan.
        let bracket_prose = "[editor]\n# [ ] a task\n# line_wrap = true\n";
        assert_eq!(check_commented_examples(bracket_prose), Ok(1));

        // A real commented-out header still is one, in both spellings.
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

    /// `[table]` / `[[array.of.tables]]` if `line` is exactly a TOML table
    /// header, else `None`.
    ///
    /// The bracket content must be a dotted run of bare-key characters,
    /// which is what separates a header from bracketed *prose* — a comment
    /// line like `# [ ] a task` or `# [see the note above]` would otherwise
    /// be adopted as the current section and every example after it checked
    /// against a table that does not exist.
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

    /// Uncomment every `# key = value` example in a reference config and
    /// check it against the live `Config` schema, scoped to whichever
    /// table it sits under.  Returns how many examples were checked, or
    /// the first failure.  See the caller for why this exists.
    ///
    /// Section tracking is a line scan, not a TOML parse, so it rests on
    /// the reference file's layout: a commented-out `# [section]` header
    /// claims every commented example below it until the next header of
    /// either kind.  A commented header dropped into the middle of a live
    /// table would therefore misattribute the examples that follow — but
    /// that misattribution *fails the test* rather than skipping a check,
    /// so the failure mode is a false alarm the author sees immediately,
    /// never a stale example slipping through.
    fn check_commented_examples(raw: &str) -> Result<usize, String> {
        let mut section = String::new();
        let mut checked = 0;

        for (lineno, line) in raw.lines().enumerate() {
            let trimmed = line.trim();
            // A live table header, or a commented-out one (`# [dev]`, which
            // is how the reference file presents a whole optional section).
            let candidate = trimmed.strip_prefix('#').map_or(trimmed, str::trim);
            if let Some(header) = table_header(candidate) {
                section = header;
                continue;
            }
            // Only commented lines are of interest; live ones are already
            // covered by the whole-file parse above.
            let Some(body) = trimmed.strip_prefix('#').map(str::trim) else {
                continue;
            };
            // Prose, not an example: `# ... some sentence ...`, or a
            // continuation of a previous line's trailing comment.
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

    /// Every keybinding the shipped `config/keybindings.toml` shows as a
    /// commented example must be one the user can actually uncomment.
    ///
    /// This is the file's whole purpose, and it has been wrong before: it
    /// used to present `Action = ""` as the way to leave something
    /// unbound, which is a parse error that silently drops the entry.
    /// Uncomment every `# Name = "chord"` line and put it through the same
    /// action-name and `parse_key` validation the loader uses.
    #[test]
    fn shipped_reference_keybindings_are_all_uncommentable() {
        let raw = include_str!("../../config/keybindings.toml");
        let mut checked = 0;
        for line in raw.lines() {
            let Some(body) = line.trim_start().strip_prefix('#') else {
                continue;
            };
            let body = body.trim();
            // Only consider lines shaped like a binding: `Name = "chord"`.
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

    /// The export folder lives inside the config directory, so a
    /// `--no-config` run must not enumerate it — the HTML-export modal
    /// falls back to the compiled-in stylesheet, which is what the flag
    /// asks for.  The second half proves the empty result came from the
    /// gate and not from an empty folder.
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

    /// Defense in depth behind `list_theme_names`: even handed a custom
    /// theme name directly, a disabled run reads no file and substitutes
    /// the capability-appropriate built-in — with no `MissingTheme`
    /// warning, because nothing is missing.
    #[test]
    fn a_user_theme_is_not_read_while_the_config_dir_is_disabled() {
        let _lock = crate::test_env::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(themes.join("mine.toml"), "[h1]\nfg = \"red\"\n").unwrap();

        // `ThemeFile` has no `PartialEq`; its TOML rendering is a faithful
        // stand-in and reports a readable diff on failure.
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

        // Ungated, the same call reads the file — so the assertions above
        // are about the gate, not about a misdirected path.
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
        // The scaffolded fork-able reference is not a `.css` and is excluded.
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
