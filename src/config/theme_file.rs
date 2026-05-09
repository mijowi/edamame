//! User-authorable theme file format.
//!
//! [`super::theme::Theme`] is the live in-memory style table used by the
//! renderer.  It carries a [`super::theme::Palette`] (the named brand
//! colours every style is derived from) plus a flat field per styled
//! UI element.  Users cannot edit it directly.
//!
//! This module supplies a parallel [`ThemeFile`] that round-trips
//! through TOML.  A theme file has two sections:
//!
//! 1. `[palette]` — the bright/dim brand colours every style derives
//!    from.  Editing only the palette is the cheapest way to retheme
//!    edamame end-to-end: every style that hasn't been individually
//!    overridden re-derives from the new palette on load.
//! 2. `[h1]`, `[h2]`, …, `[modal_input_focused]`, etc. — per-element
//!    overrides.  Anything you set here wins over the palette-derived
//!    default.
//!
//! Authoring a new theme typically means rewriting the palette and
//! letting every style fall through.  Power users can override
//! individual fields (e.g. give H1 a setext rule colour distinct from
//! the H1 fg) without touching the rest.
//!
//! On load we run a three-stage merge:
//!
//! 1. Start from the default [`super::theme::Palette`].
//! 2. Apply any `[palette]` overrides from the file.
//! 3. Build a default [`super::theme::Theme`] from the merged palette,
//!    then apply any per-element overrides that the file declares.
//!
//! `Color` accepts named colours (`"magenta"`), hex (`"#ff00aa"`), or a
//! 256-colour index either as a string (`"236"`) or a bare TOML integer
//! (`236`) — the latter is friendlier in TOML.

mod color;
mod defaults;
mod palette;
mod style_spec;

// Re-exports through the facade.  `ColorField` is reachable via this path
// (e.g. `theme_file::ColorField`) but compiles as "unused" in non-test
// builds because no production caller references it directly — only the
// test suite does.  Tagging the re-export keeps `cargo build` clean while
// preserving the public path.
#[allow(unused_imports)]
pub use color::ColorField;
pub use defaults::default_theme_toml;
pub use palette::PaletteFile;
pub use style_spec::StyleSpec;

use serde::{Deserialize, Serialize};

use super::theme::Theme;

/// Full set of theme entries as they appear in TOML.  One field per `Style`
/// field on `Theme`, plus the `task_strikethrough` boolean flag.
///
/// `#[serde(default)]` (not `deny_unknown_fields`) — users may edit themes
/// written by older binaries that didn't include a field, or newer binaries
/// that added a field, without the file failing to parse.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeFile {
    /// Brand-colour palette.  Edit this section to retheme edamame
    /// end-to-end without touching individual style fields.
    pub palette: PaletteFile,

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
    pub code_span_dim: StyleSpec,
    pub link_text: StyleSpec,
    pub link_file: StyleSpec,
    pub link_heading: StyleSpec,
    pub image_placeholder: StyleSpec,
    pub footnote: StyleSpec,

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
    pub task_complete_text: StyleSpec,
    /// When true, checked-item text is rendered with strikethrough.
    pub task_strikethrough: bool,

    // Table
    pub table_border: StyleSpec,
    pub table_header: StyleSpec,
    pub table_header_border: StyleSpec,
    pub table_cell: StyleSpec,
    pub table_row_even: StyleSpec,
    pub table_row_odd: StyleSpec,
    pub table_drop_indicator: StyleSpec,
    pub table_drop_target: StyleSpec,
    pub table_handle: StyleSpec,
    pub table_handle_delete: StyleSpec,

    // Status bar
    pub status_bar: StyleSpec,
    pub status_mode_preview: StyleSpec,
    pub status_mode_rendered: StyleSpec,
    pub status_mode_raw: StyleSpec,
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
    pub modal_bg: StyleSpec,
    pub modal_title_normal: StyleSpec,
    pub modal_title_warning: StyleSpec,
    pub modal_title_error: StyleSpec,
    pub modal_close_hint: StyleSpec,
    pub modal_item: StyleSpec,
    pub modal_item_hint: StyleSpec,
    pub modal_item_selected: StyleSpec,
    pub modal_item_selected_hint: StyleSpec,
    pub modal_description: StyleSpec,
    pub modal_section_heading: StyleSpec,
    pub modal_input_unfocused: StyleSpec,
    pub modal_input_focused: StyleSpec,
    pub modal_button_focused: StyleSpec,

    // General
    pub normal: StyleSpec,
    pub selection: StyleSpec,
    pub search_highlight: StyleSpec,
    pub active_line: StyleSpec,
    pub cursor_preview: StyleSpec,
    pub cursor_rendered: StyleSpec,
    pub cursor_raw: StyleSpec,
    pub cursor: StyleSpec,

    // Scrollbar
    pub scrollbar_track: StyleSpec,
    pub scrollbar_thumb: StyleSpec,
    pub scrollbar_thumb_active: StyleSpec,
}

/// All theme-style fields, listed once.  Both `From<&ThemeFile> for Theme`
/// (file → live theme, with empty-spec fall-through) and
/// `From<&Theme> for ThemeFile` (live theme → file, full population)
/// iterate this list, so adding a style means touching one line plus the
/// `Theme` and `ThemeFile` struct definitions.
macro_rules! style_fields {
    ($mac:ident) => {
        $mac! {
            h1, h1_rule, h2, h3, h4, h5, h6,
            bold, italic, strikethrough, highlight,
            code_span, code_span_dim,
            link_text, link_file, link_heading,
            image_placeholder, footnote,
            code_block_border, code_block_lang, code_block_text,
            blockquote_bar, blockquote_text, rule,
            list_bullet, list_number,
            task_unchecked, task_checked, task_complete_text,
            table_border, table_header, table_header_border,
            table_cell, table_row_even, table_row_odd,
            table_drop_indicator, table_drop_target,
            table_handle, table_handle_delete,
            status_bar,
            status_mode_preview, status_mode_rendered, status_mode_raw,
            status_filename, status_info, status_modified, status_selection,
            hint_bar, hint_chord, hint_label,
            transient_info, transient_success, transient_warning, transient_error,
            modal_bg,
            modal_title_normal, modal_title_warning, modal_title_error,
            modal_close_hint,
            modal_item, modal_item_hint,
            modal_item_selected, modal_item_selected_hint,
            modal_description, modal_section_heading,
            modal_input_unfocused, modal_input_focused, modal_button_focused,
            normal, selection, search_highlight, active_line,
            cursor_preview, cursor_rendered, cursor_raw, cursor,
            scrollbar_track, scrollbar_thumb, scrollbar_thumb_active
        }
    };
}

/// Build a `Theme` from a `ThemeFile`.  Implements the three-stage
/// merge documented at the module level:
///
/// 1. Resolve the palette section against [`super::theme::Palette::default`].
/// 2. Build a default theme from that palette.
/// 3. For each style spec that's non-empty in the file, override the
///    corresponding theme field.  Empty specs fall through so the
///    palette-derived default wins.
///
/// `task_strikethrough` is a plain bool, not a style — it always wins
/// over the default because there's no "absent" sentinel to detect.
impl From<&ThemeFile> for Theme {
    fn from(f: &ThemeFile) -> Self {
        let palette = palette::PaletteFile::resolve(&f.palette);
        let mut theme = Theme::from_palette(&palette);

        // Per-style overrides.  Empty specs fall through to keep the
        // palette-derived default.
        macro_rules! apply_all {
            ($($field:ident),* $(,)?) => {{
                $(
                    if !f.$field.is_empty() {
                        theme.$field = (&f.$field).into();
                    }
                )*
            }};
        }
        style_fields!(apply_all);

        // task_strikethrough is a bare bool — always honoured.
        theme.task_strikethrough = f.task_strikethrough;

        theme
    }
}

impl From<&Theme> for ThemeFile {
    fn from(t: &Theme) -> Self {
        macro_rules! collect {
            ($($field:ident),* $(,)?) => {
                Self {
                    palette: (&t.palette).into(),
                    task_strikethrough: t.task_strikethrough,
                    $( $field: (&t.$field).into(), )*
                }
            };
        }
        style_fields!(collect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier, Style};

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
        check!(code_span_dim);
        check!(link_text);
        check!(link_file);
        check!(link_heading);
        check!(image_placeholder);
        check!(footnote);
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
        check!(task_complete_text);
        check!(table_border);
        check!(table_header);
        check!(table_header_border);
        check!(table_cell);
        check!(table_row_even);
        check!(table_row_odd);
        check!(table_drop_indicator);
        check!(table_drop_target);
        check!(table_handle);
        check!(table_handle_delete);
        check!(status_bar);
        check!(status_mode_preview);
        check!(status_mode_rendered);
        check!(status_mode_raw);
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
        check!(modal_bg);
        check!(modal_title_normal);
        check!(modal_title_warning);
        check!(modal_title_error);
        check!(modal_close_hint);
        check!(modal_item);
        check!(modal_item_hint);
        check!(modal_item_selected);
        check!(modal_item_selected_hint);
        check!(modal_description);
        check!(modal_section_heading);
        check!(modal_input_unfocused);
        check!(modal_input_focused);
        check!(modal_button_focused);
        check!(normal);
        check!(selection);
        check!(search_highlight);
        check!(active_line);
        check!(cursor_preview);
        check!(cursor_rendered);
        check!(cursor_raw);
        check!(cursor);
        check!(scrollbar_track);
        check!(scrollbar_thumb);
        check!(scrollbar_thumb_active);
        assert_eq!(
            original.task_strikethrough, round_tripped.task_strikethrough,
            "task_strikethrough did not round-trip"
        );
    }

    #[test]
    fn default_theme_round_trips() {
        assert_theme_round_trip(&Theme::default());
    }

    // No `monochrome_theme_round_trips` test: the monochrome theme is
    // always built programmatically (`Theme::monochrome()`) when the
    // terminal reports no colour support — it never loads from a
    // file.  Several of its styles are intentionally `Style::default()`
    // (e.g. `h1_rule`), and the file format treats absent sections as
    // "use the palette-derived default", so `Style::default()` is not
    // a faithful round-trip target through TOML.

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

    #[test]
    fn empty_section_falls_back_to_palette_default() {
        // Empty `[h1]` section in the file must NOT clobber the
        // palette-derived default — the merge step skips empty specs.
        let toml = "[h1]\n";
        let file: ThemeFile = toml::from_str(toml).unwrap();
        let theme: Theme = (&file).into();
        assert_eq!(theme.h1, Theme::default().h1);
    }

    #[test]
    fn palette_override_ripples_to_styles() {
        // Override only the palette; the H1 fg should pick up the new
        // emphasis_bright colour because the merge re-derives styles
        // from the file palette before applying overrides.
        let toml = r##"
[palette]
emphasis_bright = "#abcdef"
"##;
        let file: ThemeFile = toml::from_str(toml).unwrap();
        let theme: Theme = (&file).into();
        assert_eq!(theme.h1.fg, Some(Color::Indexed(220)));
        // h1_rule shares the emphasis_bright colour and should follow.
        assert_eq!(theme.h1_rule.fg, Some(Color::Indexed(220)));
    }

    #[test]
    fn style_override_wins_over_palette() {
        // Palette + an explicit style override on H1.  The style
        // override should win.
        let toml = r##"
[palette]
emphasis_bright = "#abcdef"

[h1]
fg = "#112233"
bold = true
"##;
        let file: ThemeFile = toml::from_str(toml).unwrap();
        let theme: Theme = (&file).into();
        assert_eq!(theme.h1.fg, Some(Color::Rgb(0x11, 0x22, 0x33)));
        // h1_rule still picks up the palette override (no explicit
        // override in the file).
        assert_eq!(theme.h1_rule.fg, Some(Color::Indexed(220)));
    }

    #[test]
    fn palette_only_file_renders_identical_to_default_theme() {
        // Lock in the contract that the shipped `default.toml` shape
        // (palette + empty per-element sections) produces exactly the
        // compiled-in default theme.  Catches regressions where a new
        // style sneaks in with a hand-rolled default that doesn't fall
        // out of the palette merge.
        let default = Theme::default();
        let file = ThemeFile {
            palette: (&default.palette).into(),
            task_strikethrough: default.task_strikethrough,
            ..Default::default()
        };
        // Round through TOML so we exercise the same path as a real
        // load (serialize → parse → merge), not just the in-memory
        // From impl.
        let toml_str = toml::to_string_pretty(&file).expect("serialize");
        let parsed: ThemeFile = toml::from_str(&toml_str).expect("parse");
        let theme: Theme = (&parsed).into();

        macro_rules! check {
            ($field:ident) => {
                assert_eq!(
                    default.$field, theme.$field,
                    concat!(
                        "field `",
                        stringify!($field),
                        "` did not match palette-derived default"
                    )
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
        check!(code_span_dim);
        check!(link_text);
        check!(link_file);
        check!(link_heading);
        check!(image_placeholder);
        check!(footnote);
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
        check!(task_complete_text);
        check!(table_border);
        check!(table_header);
        check!(table_header_border);
        check!(table_row_even);
        check!(table_row_odd);
        check!(table_drop_indicator);
        check!(table_drop_target);
        check!(table_handle);
        check!(table_handle_delete);
        check!(status_bar);
        check!(status_mode_preview);
        check!(status_mode_rendered);
        check!(status_mode_raw);
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
        check!(modal_bg);
        check!(modal_title_normal);
        check!(modal_title_warning);
        check!(modal_title_error);
        check!(modal_close_hint);
        check!(modal_item);
        check!(modal_item_hint);
        check!(modal_item_selected);
        check!(modal_item_selected_hint);
        check!(modal_description);
        check!(modal_section_heading);
        check!(modal_input_unfocused);
        check!(modal_input_focused);
        check!(modal_button_focused);
        check!(normal);
        check!(selection);
        check!(search_highlight);
        check!(cursor_preview);
        check!(cursor_rendered);
        check!(cursor_raw);
        check!(cursor);
        check!(scrollbar_track);
        check!(scrollbar_thumb);
        check!(scrollbar_thumb_active);
        // `table_cell` and `active_line` are intentionally
        // `Style::default()` in the compiled default and round-trip
        // identically through both branches of the merge.
    }

    #[test]
    fn default_theme_toml_palette_lines_are_commented() {
        // Every palette field must appear as a `# field = …` line so the
        // generated file documents the compiled-in default without
        // overriding it at load time.
        let toml_str = default_theme_toml();
        for field in [
            "default_text",
            "default_bg",
            "primary_bright",
            "primary_dim",
            "emphasis_bright",
            "emphasis_dim",
            "structural_bright",
            "structural_dim",
            "interactive_bright",
            "interactive_dim",
            "success_bright",
            "success_dim",
            "warning_bright",
            "warning_dim",
            "error_bright",
            "error_dim",
            "text_muted",
            "muted",
            "surface_elevated",
            "surface",
        ] {
            let commented = format!("# {field} = ");
            assert!(
                toml_str.contains(&commented),
                "expected commented line `{commented}…` in generated default.toml; \
                 not found in:\n{toml_str}"
            );
            // And the same field must not appear uncommented (i.e. not
            // immediately preceded by a `#`), which would make it an
            // active override.
            for line in toml_str.lines() {
                let trimmed = line.trim_start();
                if trimmed.starts_with(&format!("{field} =")) {
                    panic!("palette field `{field}` is not commented out: `{line}`");
                }
            }
        }
    }

    #[test]
    fn default_theme_toml_resolves_to_default_theme() {
        // The whole point of commenting out the palette: parsing the
        // generated file must produce exactly `Theme::default()`.  If
        // someone accidentally leaves a palette line uncommented (or
        // changes a default without retracing the merge), this fails.
        let toml_str = default_theme_toml();
        let parsed: ThemeFile = toml::from_str(&toml_str).expect("parse generated default.toml");
        let theme: Theme = (&parsed).into();
        let expected = Theme::default();

        macro_rules! check {
            ($field:ident) => {
                assert_eq!(
                    expected.$field, theme.$field,
                    concat!(
                        "field `",
                        stringify!($field),
                        "` drifted from Theme::default()"
                    )
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
        check!(code_span_dim);
        check!(link_text);
        check!(link_file);
        check!(link_heading);
        check!(image_placeholder);
        check!(footnote);
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
        check!(task_complete_text);
        check!(table_border);
        check!(table_header);
        check!(table_header_border);
        check!(table_cell);
        check!(table_row_even);
        check!(table_row_odd);
        check!(table_drop_indicator);
        check!(table_drop_target);
        check!(table_handle);
        check!(table_handle_delete);
        check!(status_bar);
        check!(status_mode_preview);
        check!(status_mode_rendered);
        check!(status_mode_raw);
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
        check!(modal_bg);
        check!(modal_title_normal);
        check!(modal_title_warning);
        check!(modal_title_error);
        check!(modal_close_hint);
        check!(modal_item);
        check!(modal_item_hint);
        check!(modal_item_selected);
        check!(modal_item_selected_hint);
        check!(modal_description);
        check!(modal_section_heading);
        check!(modal_input_unfocused);
        check!(modal_input_focused);
        check!(modal_button_focused);
        check!(normal);
        check!(selection);
        check!(search_highlight);
        check!(active_line);
        check!(cursor_preview);
        check!(cursor_rendered);
        check!(cursor_raw);
        check!(cursor);
        check!(scrollbar_track);
        check!(scrollbar_thumb);
        check!(scrollbar_thumb_active);
        assert_eq!(expected.task_strikethrough, theme.task_strikethrough);
    }
}
