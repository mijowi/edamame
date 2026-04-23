//! User-authorable theme file format.
//!
//! `Theme` (in `src/config/theme.rs`) is the live in-memory style table used
//! by the renderer.  It's a flat struct of `ratatui::style::Style` values with
//! hardcoded defaults.  Users cannot edit it.
//!
//! This module supplies a parallel `ThemeFile` that can be round-tripped
//! through TOML.  Each style becomes a `StyleSpec` with `fg` / `bg` colours
//! and per-modifier booleans, so a theme entry reads naturally:
//!
//! ```toml
//! [h1]
//! fg = "magenta"
//! bold = true
//! ```
//!
//! On load the file is converted to a `Theme` via `Theme::from_file`.  On the
//! regeneration test (`#[ignore]` by default) we go the other direction —
//! `Theme::default()` → `ThemeFile` → TOML — so the shipped
//! `config/themes/default.toml` stays in sync with the compiled-in defaults
//! without hand-maintenance.
//!
//! `Color` uses ratatui's own serde impl (`ratatui/serde` feature), which
//! accepts named colours (`"magenta"`), hex (`"#ff00aa"`), and indexed 256
//! colours as a numeric string (`"236"`).  Bare integers are handled via
//! `ColorField`'s untagged deserializer so `bg = 236` (TOML integer) also
//! works — friendlier in TOML than forcing quotes.

use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

use super::theme::Theme;

// ── ColorField ────────────────────────────────────────────────────────────────

/// Deserializes a colour from TOML.  Accepts either:
///   * a string (`"magenta"`, `"#ff00aa"`, `"236"`)  — via ratatui's `Color`
///   * a bare integer (`236`)                        — as `Color::Indexed`
///
/// Both shapes exist because TOML distinguishes strings and integers, and
/// forcing users to quote `236` when they mean "palette index 236" is awkward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ColorField {
    Named(Color),
    Indexed(u8),
}

impl From<ColorField> for Color {
    fn from(c: ColorField) -> Self {
        match c {
            ColorField::Named(c) => c,
            ColorField::Indexed(i) => Self::Indexed(i),
        }
    }
}

impl From<Color> for ColorField {
    fn from(c: Color) -> Self {
        // Keep indexed palette entries as integers in the emitted TOML so the
        // regenerated file matches what a human would write.
        match c {
            Color::Indexed(i) => Self::Indexed(i),
            other => Self::Named(other),
        }
    }
}

// ── StyleSpec ─────────────────────────────────────────────────────────────────

/// TOML-friendly style record.  Absent modifier booleans default to `false`;
/// absent `fg` / `bg` mean "unset" (inherit from the terminal).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StyleSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<ColorField>,
    #[serde(skip_serializing_if = "is_false")]
    pub bold: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub italic: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub underlined: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub reversed: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub crossed_out: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub dim: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl From<&StyleSpec> for Style {
    fn from(spec: &StyleSpec) -> Self {
        let mut style = Self::default();
        if let Some(fg) = spec.fg {
            style = style.fg(fg.into());
        }
        if let Some(bg) = spec.bg {
            style = style.bg(bg.into());
        }
        let mut modifiers = Modifier::empty();
        if spec.bold {
            modifiers |= Modifier::BOLD;
        }
        if spec.italic {
            modifiers |= Modifier::ITALIC;
        }
        if spec.underlined {
            modifiers |= Modifier::UNDERLINED;
        }
        if spec.reversed {
            modifiers |= Modifier::REVERSED;
        }
        if spec.crossed_out {
            modifiers |= Modifier::CROSSED_OUT;
        }
        if spec.dim {
            modifiers |= Modifier::DIM;
        }
        if !modifiers.is_empty() {
            style = style.add_modifier(modifiers);
        }
        style
    }
}

impl From<&Style> for StyleSpec {
    fn from(style: &Style) -> Self {
        let m = style.add_modifier;
        Self {
            fg: style.fg.map(Into::into),
            bg: style.bg.map(Into::into),
            bold: m.contains(Modifier::BOLD),
            italic: m.contains(Modifier::ITALIC),
            underlined: m.contains(Modifier::UNDERLINED),
            reversed: m.contains(Modifier::REVERSED),
            crossed_out: m.contains(Modifier::CROSSED_OUT),
            dim: m.contains(Modifier::DIM),
        }
    }
}

// ── ThemeFile ─────────────────────────────────────────────────────────────────

/// Full set of theme entries as they appear in TOML.  One field per `Style`
/// field on `Theme`, plus the `task_strikethrough` boolean flag.
///
/// `#[serde(default)]` (not `deny_unknown_fields`) — users may edit themes
/// written by older binaries that didn't include a field, or newer binaries
/// that added a field, without the file failing to parse.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeFile {
    // Headings
    pub h1: StyleSpec,
    pub h1_rule: StyleSpec,
    pub h2: StyleSpec,
    pub h3: StyleSpec,
    pub h4: StyleSpec,
    pub h5: StyleSpec,
    pub h6: StyleSpec,

    // Inline formatting
    pub bold: StyleSpec,
    pub italic: StyleSpec,
    pub strikethrough: StyleSpec,
    pub highlight: StyleSpec,
    pub code_span: StyleSpec,
    pub link_text: StyleSpec,
    pub image_placeholder: StyleSpec,

    // Block elements
    pub code_block_border: StyleSpec,
    pub code_block_lang: StyleSpec,
    pub code_block_text: StyleSpec,
    pub blockquote_bar: StyleSpec,
    pub blockquote_text: StyleSpec,
    pub rule: StyleSpec,

    // List markers
    pub list_bullet: StyleSpec,
    pub list_number: StyleSpec,

    // Task list
    pub task_unchecked: StyleSpec,
    pub task_checked: StyleSpec,
    /// When true, checked-item text is rendered with strikethrough.
    pub task_strikethrough: bool,

    // Table
    pub table_border: StyleSpec,
    pub table_header: StyleSpec,
    pub table_cell: StyleSpec,

    // Status bar
    pub status_bar: StyleSpec,
    pub status_mode: StyleSpec,
    pub status_filename: StyleSpec,
    pub status_info: StyleSpec,
    pub status_modified: StyleSpec,
    pub status_selection: StyleSpec,

    // Hint line (Phase 9)
    pub hint_bar: StyleSpec,
    pub hint_chord: StyleSpec,
    pub hint_label: StyleSpec,

    // Transient messages (Phase 9)
    pub transient_info: StyleSpec,
    pub transient_success: StyleSpec,
    pub transient_warning: StyleSpec,
    pub transient_error: StyleSpec,

    // Modal popups
    pub modal_title: StyleSpec,
    pub modal_button_focused: StyleSpec,

    // General
    pub normal: StyleSpec,
    pub selection: StyleSpec,
    pub cursor: StyleSpec,
}

/// Convert a `ThemeFile` into the live `Theme` used by the renderer.
///
/// One macro per style field would be cleaner, but the flat 37-line body
/// makes the field → field wiring obvious on review and lets rust-analyzer
/// jump straight from a source site to the correct target.
impl From<&ThemeFile> for Theme {
    fn from(f: &ThemeFile) -> Self {
        Self {
            h1: (&f.h1).into(),
            h1_rule: (&f.h1_rule).into(),
            h2: (&f.h2).into(),
            h3: (&f.h3).into(),
            h4: (&f.h4).into(),
            h5: (&f.h5).into(),
            h6: (&f.h6).into(),

            bold: (&f.bold).into(),
            italic: (&f.italic).into(),
            strikethrough: (&f.strikethrough).into(),
            highlight: (&f.highlight).into(),
            code_span: (&f.code_span).into(),
            link_text: (&f.link_text).into(),
            image_placeholder: (&f.image_placeholder).into(),

            code_block_border: (&f.code_block_border).into(),
            code_block_lang: (&f.code_block_lang).into(),
            code_block_text: (&f.code_block_text).into(),
            blockquote_bar: (&f.blockquote_bar).into(),
            blockquote_text: (&f.blockquote_text).into(),
            rule: (&f.rule).into(),

            list_bullet: (&f.list_bullet).into(),
            list_number: (&f.list_number).into(),

            task_unchecked: (&f.task_unchecked).into(),
            task_checked: (&f.task_checked).into(),
            task_strikethrough: f.task_strikethrough,

            table_border: (&f.table_border).into(),
            table_header: (&f.table_header).into(),
            table_cell: (&f.table_cell).into(),

            status_bar: (&f.status_bar).into(),
            status_mode: (&f.status_mode).into(),
            status_filename: (&f.status_filename).into(),
            status_info: (&f.status_info).into(),
            status_modified: (&f.status_modified).into(),
            status_selection: (&f.status_selection).into(),

            hint_bar: (&f.hint_bar).into(),
            hint_chord: (&f.hint_chord).into(),
            hint_label: (&f.hint_label).into(),

            transient_info: (&f.transient_info).into(),
            transient_success: (&f.transient_success).into(),
            transient_warning: (&f.transient_warning).into(),
            transient_error: (&f.transient_error).into(),

            modal_title: (&f.modal_title).into(),
            modal_button_focused: (&f.modal_button_focused).into(),

            normal: (&f.normal).into(),
            selection: (&f.selection).into(),
            // `cursor` is a post-v1 theme field.  Theme files written by
            // earlier binaries have no `[cursor]` section, which serde fills
            // with `StyleSpec::default()` — a fully empty style that would
            // render the cursor invisible.  Fall back to the compiled
            // `REVERSED`-only default in that case so upgrading never hides
            // the cursor.  Users who want a non-default cursor must set at
            // least one field under `[cursor]`, which is the same contract
            // as every other style.
            cursor: {
                let s: Style = (&f.cursor).into();
                if s == Style::default() {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    s
                }
            },
        }
    }
}

impl From<&Theme> for ThemeFile {
    fn from(t: &Theme) -> Self {
        Self {
            h1: (&t.h1).into(),
            h1_rule: (&t.h1_rule).into(),
            h2: (&t.h2).into(),
            h3: (&t.h3).into(),
            h4: (&t.h4).into(),
            h5: (&t.h5).into(),
            h6: (&t.h6).into(),

            bold: (&t.bold).into(),
            italic: (&t.italic).into(),
            strikethrough: (&t.strikethrough).into(),
            highlight: (&t.highlight).into(),
            code_span: (&t.code_span).into(),
            link_text: (&t.link_text).into(),
            image_placeholder: (&t.image_placeholder).into(),

            code_block_border: (&t.code_block_border).into(),
            code_block_lang: (&t.code_block_lang).into(),
            code_block_text: (&t.code_block_text).into(),
            blockquote_bar: (&t.blockquote_bar).into(),
            blockquote_text: (&t.blockquote_text).into(),
            rule: (&t.rule).into(),

            list_bullet: (&t.list_bullet).into(),
            list_number: (&t.list_number).into(),

            task_unchecked: (&t.task_unchecked).into(),
            task_checked: (&t.task_checked).into(),
            task_strikethrough: t.task_strikethrough,

            table_border: (&t.table_border).into(),
            table_header: (&t.table_header).into(),
            table_cell: (&t.table_cell).into(),

            status_bar: (&t.status_bar).into(),
            status_mode: (&t.status_mode).into(),
            status_filename: (&t.status_filename).into(),
            status_info: (&t.status_info).into(),
            status_modified: (&t.status_modified).into(),
            status_selection: (&t.status_selection).into(),

            hint_bar: (&t.hint_bar).into(),
            hint_chord: (&t.hint_chord).into(),
            hint_label: (&t.hint_label).into(),

            transient_info: (&t.transient_info).into(),
            transient_success: (&t.transient_success).into(),
            transient_warning: (&t.transient_warning).into(),
            transient_error: (&t.transient_error).into(),

            modal_title: (&t.modal_title).into(),
            modal_button_focused: (&t.modal_button_focused).into(),

            normal: (&t.normal).into(),
            selection: (&t.selection).into(),
            cursor: (&t.cursor).into(),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: round-trip a `Theme` through the TOML serde layer and compare
    // every `Style` field for equality.  Uses the actual Style `PartialEq`
    // rather than a textual TOML diff so reordered fields don't cause spurious
    // failures.
    fn assert_theme_round_trip(original: &Theme) {
        let file: ThemeFile = original.into();
        let toml_str = toml::to_string(&file).expect("serialize ThemeFile");
        let parsed: ThemeFile = toml::from_str(&toml_str).expect("parse ThemeFile");
        let round_tripped: Theme = (&parsed).into();

        macro_rules! check {
            ($field:ident) => {
                assert_eq!(
                    original.$field, round_tripped.$field,
                    concat!("field `", stringify!($field), "` did not round-trip")
                );
            };
        }
        check!(h1);
        check!(h1_rule);
        check!(h2);
        check!(h3);
        check!(h4);
        check!(h5);
        check!(h6);
        check!(bold);
        check!(italic);
        check!(strikethrough);
        check!(highlight);
        check!(code_span);
        check!(link_text);
        check!(image_placeholder);
        check!(code_block_border);
        check!(code_block_lang);
        check!(code_block_text);
        check!(blockquote_bar);
        check!(blockquote_text);
        check!(rule);
        check!(list_bullet);
        check!(list_number);
        check!(task_unchecked);
        check!(task_checked);
        check!(table_border);
        check!(table_header);
        check!(table_cell);
        check!(status_bar);
        check!(status_mode);
        check!(status_filename);
        check!(status_info);
        check!(status_modified);
        check!(status_selection);
        check!(hint_bar);
        check!(hint_chord);
        check!(hint_label);
        check!(transient_info);
        check!(transient_success);
        check!(transient_warning);
        check!(transient_error);
        check!(modal_title);
        check!(modal_button_focused);
        check!(normal);
        check!(selection);
        check!(cursor);
        assert_eq!(
            original.task_strikethrough, round_tripped.task_strikethrough,
            "task_strikethrough did not round-trip"
        );
    }

    #[test]
    fn default_theme_round_trips() {
        assert_theme_round_trip(&Theme::default());
    }

    #[test]
    fn monochrome_theme_round_trips() {
        assert_theme_round_trip(&Theme::monochrome());
    }

    #[test]
    fn named_color_parses() {
        let toml = r#"[h1]
fg = "magenta"
bold = true
"#;
        let file: ThemeFile = toml::from_str(toml).unwrap();
        let style: Style = (&file.h1).into();
        assert_eq!(style.fg, Some(Color::Magenta));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn hex_color_parses() {
        let toml = r##"[link_text]
fg = "#00afff"
underlined = true
"##;
        let file: ThemeFile = toml::from_str(toml).unwrap();
        let style: Style = (&file.link_text).into();
        assert_eq!(style.fg, Some(Color::Rgb(0, 0xaf, 0xff)));
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn indexed_color_as_integer_parses() {
        // Bare TOML integer for indexed palette entries.
        let toml = r#"[code_span]
fg = "yellow"
bg = 236
"#;
        let file: ThemeFile = toml::from_str(toml).unwrap();
        let style: Style = (&file.code_span).into();
        assert_eq!(style.fg, Some(Color::Yellow));
        assert_eq!(style.bg, Some(Color::Indexed(236)));
    }

    #[test]
    fn indexed_color_as_string_parses() {
        // Same palette entry expressed as a string (ratatui's native format).
        let toml = r#"[code_span]
bg = "236"
"#;
        let file: ThemeFile = toml::from_str(toml).unwrap();
        let style: Style = (&file.code_span).into();
        assert_eq!(style.bg, Some(Color::Indexed(236)));
    }

    #[test]
    fn missing_fields_default_to_empty_style() {
        let file: ThemeFile = toml::from_str("").unwrap();
        let style: Style = (&file.h1).into();
        assert_eq!(style.fg, None);
        assert_eq!(style.bg, None);
        assert!(style.add_modifier.is_empty());
    }

    #[test]
    fn omitted_defaults_dont_serialize() {
        // `skip_serializing_if` keeps emitted TOML tight: a fully-default style
        // produces an empty table, not a noisy one with six `false` booleans.
        let spec = StyleSpec::default();
        let toml_str = toml::to_string(&spec).unwrap();
        assert_eq!(toml_str.trim(), "");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // Forward-compat: a theme file written by a future binary with extra
        // fields should still load cleanly on older binaries.
        let toml = r#"[h1]
fg = "red"

[some_future_style]
fg = "blue"
"#;
        let file: ThemeFile = toml::from_str(toml).unwrap();
        assert_eq!(file.h1.fg.map(Color::from), Some(Color::Red));
    }

    /// Regenerate `config/themes/default.toml` from the compiled-in default
    /// `Theme`.  Run with `cargo test -- --ignored regenerate_default_theme_toml`
    /// after changing `Theme::default()` and commit the updated TOML file.
    #[test]
    #[ignore]
    fn regenerate_default_theme_toml() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let out_dir = std::path::Path::new(manifest_dir)
            .join("config")
            .join("themes");
        std::fs::create_dir_all(&out_dir).expect("create config/themes/");
        let out_path = out_dir.join("default.toml");

        let file: ThemeFile = (&Theme::default()).into();
        let body = toml::to_string_pretty(&file).expect("serialize default ThemeFile");

        let contents = format!(
            "# edamame default theme — regenerate with\n\
             #   cargo test -- --ignored regenerate_default_theme_toml\n\
             # after changing `Theme::default()` in src/config/theme.rs.\n\n{body}"
        );
        std::fs::write(&out_path, contents).expect("write default.toml");
        eprintln!("wrote {}", out_path.display());
    }
}
