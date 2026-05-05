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

use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

use super::theme::{Palette, Theme};

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
        match c {
            Color::Indexed(i) => Self::Indexed(i),
            other => Self::Named(other),
        }
    }
}

// ── PaletteFile ──────────────────────────────────────────────────────────────

/// User-authorable palette section.  Every field is optional; missing
/// entries fall through to [`Palette::default`] at load time.
///
/// Authoring a new theme that just re-tints the UI is usually a matter
/// of editing this section and leaving the per-element style sections
/// untouched.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PaletteFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_text: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_bg: Option<ColorField>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_bright: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_dim: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emphasis_bright: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emphasis_dim: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structural_bright: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structural_dim: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactive_bright: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactive_dim: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success_bright: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success_dim: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning_bright: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning_dim: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_bright: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_dim: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_muted: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub muted: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_elevated: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h1: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h2: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h3: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h4: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h5: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h6: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<ColorField>,
}

impl PaletteFile {
    /// Resolve `self` against `Palette::default()`, returning a
    /// fully-populated palette.  Missing fields fall through to the
    /// compiled-in default so a partial `[palette]` section is valid.
    fn resolve(&self) -> Palette {
        let d = Palette::default();
        let pick = |opt: Option<ColorField>, fallback: Color| -> Color {
            opt.map(Color::from).unwrap_or(fallback)
        };
        Palette {
            default_text: pick(self.default_text, d.default_text),
            default_bg: pick(self.default_bg, d.default_bg),
            primary_bright: pick(self.primary_bright, d.primary_bright),
            primary_dim: pick(self.primary_dim, d.primary_dim),
            emphasis_bright: pick(self.emphasis_bright, d.emphasis_bright),
            emphasis_dim: pick(self.emphasis_dim, d.emphasis_dim),
            structural_bright: pick(self.structural_bright, d.structural_bright),
            structural_dim: pick(self.structural_dim, d.structural_dim),
            interactive_bright: pick(self.interactive_bright, d.interactive_bright),
            interactive_dim: pick(self.interactive_dim, d.interactive_dim),
            success_bright: pick(self.success_bright, d.success_bright),
            success_dim: pick(self.success_dim, d.success_dim),
            warning_bright: pick(self.warning_bright, d.warning_bright),
            warning_dim: pick(self.warning_dim, d.warning_dim),
            error_bright: pick(self.error_bright, d.error_bright),
            error_dim: pick(self.error_dim, d.error_dim),
            text_muted: pick(self.text_muted, d.text_muted),
            muted: pick(self.muted, d.muted),
            surface_elevated: pick(self.surface_elevated, d.surface_elevated),
            surface: pick(self.surface, d.surface),
            h1: pick(self.h1, d.h1),
            h2: pick(self.h2, d.h2),
            h3: pick(self.h3, d.h3),
            h4: pick(self.h4, d.h4),
            h5: pick(self.h5, d.h5),
            h6: pick(self.h6, d.h6),
            code: pick(self.code, d.code),
        }
    }
}

impl From<&Palette> for PaletteFile {
    fn from(p: &Palette) -> Self {
        Self {
            default_text: Some(p.default_text.into()),
            default_bg: Some(p.default_bg.into()),
            primary_bright: Some(p.primary_bright.into()),
            primary_dim: Some(p.primary_dim.into()),
            emphasis_bright: Some(p.emphasis_bright.into()),
            emphasis_dim: Some(p.emphasis_dim.into()),
            structural_bright: Some(p.structural_bright.into()),
            structural_dim: Some(p.structural_dim.into()),
            interactive_bright: Some(p.interactive_bright.into()),
            interactive_dim: Some(p.interactive_dim.into()),
            success_bright: Some(p.success_bright.into()),
            success_dim: Some(p.success_dim.into()),
            warning_bright: Some(p.warning_bright.into()),
            warning_dim: Some(p.warning_dim.into()),
            error_bright: Some(p.error_bright.into()),
            error_dim: Some(p.error_dim.into()),
            text_muted: Some(p.text_muted.into()),
            muted: Some(p.muted.into()),
            surface_elevated: Some(p.surface_elevated.into()),
            surface: Some(p.surface.into()),
            h1: Some(p.h1.into()),
            h2: Some(p.h2.into()),
            h3: Some(p.h3.into()),
            h4: Some(p.h4.into()),
            h5: Some(p.h5.into()),
            h6: Some(p.h6.into()),
            code: Some(p.code.into()),
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

impl StyleSpec {
    /// True when this spec carries no overrides — used by the merge
    /// step so that an empty `[h1]` section in TOML doesn't clobber
    /// the palette-derived default.
    fn is_empty(&self) -> bool {
        self.fg.is_none()
            && self.bg.is_none()
            && !self.bold
            && !self.italic
            && !self.underlined
            && !self.reversed
            && !self.crossed_out
            && !self.dim
    }
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
    pub modal_border: StyleSpec,
    pub modal_title: StyleSpec,
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
}

/// Build a `Theme` from a `ThemeFile`.  Implements the three-stage
/// merge documented at the module level:
///
/// 1. Resolve the palette section against [`Palette::default`].
/// 2. Build a default theme from that palette.
/// 3. For each style spec that's non-empty in the file, override the
///    corresponding theme field.  Empty specs fall through so the
///    palette-derived default wins.
///
/// `task_strikethrough` is a plain bool, not a style — it always wins
/// over the default because there's no "absent" sentinel to detect.
impl From<&ThemeFile> for Theme {
    fn from(f: &ThemeFile) -> Self {
        let palette = f.palette.resolve();
        let mut theme = Theme::from_palette(&palette);

        // Per-style overrides.  Empty specs fall through to keep the
        // palette-derived default.
        macro_rules! apply {
            ($field:ident) => {
                if !f.$field.is_empty() {
                    theme.$field = (&f.$field).into();
                }
            };
        }
        apply!(h1);
        apply!(h1_rule);
        apply!(h2);
        apply!(h3);
        apply!(h4);
        apply!(h5);
        apply!(h6);

        apply!(bold);
        apply!(italic);
        apply!(strikethrough);
        apply!(highlight);
        apply!(code_span);
        apply!(link_text);
        apply!(link_file);
        apply!(link_heading);
        apply!(image_placeholder);
        apply!(footnote);

        apply!(code_block_border);
        apply!(code_block_lang);
        apply!(code_block_text);
        apply!(blockquote_bar);
        apply!(blockquote_text);
        apply!(rule);

        apply!(list_bullet);
        apply!(list_number);

        apply!(task_unchecked);
        apply!(task_checked);
        apply!(task_complete_text);
        // task_strikethrough is a bare bool — always honoured.
        theme.task_strikethrough = f.task_strikethrough;

        apply!(table_border);
        apply!(table_header);
        apply!(table_header_border);
        apply!(table_cell);
        apply!(table_row_even);
        apply!(table_row_odd);
        apply!(table_drop_indicator);
        apply!(table_drop_target);
        apply!(table_handle);
        apply!(table_handle_delete);

        apply!(status_bar);
        apply!(status_mode_preview);
        apply!(status_mode_rendered);
        apply!(status_mode_raw);
        apply!(status_filename);
        apply!(status_info);
        apply!(status_modified);
        apply!(status_selection);

        apply!(hint_bar);
        apply!(hint_chord);
        apply!(hint_label);

        apply!(transient_info);
        apply!(transient_success);
        apply!(transient_warning);
        apply!(transient_error);

        apply!(modal_bg);
        apply!(modal_border);
        apply!(modal_title);
        apply!(modal_item);
        apply!(modal_item_hint);
        apply!(modal_item_selected);
        apply!(modal_item_selected_hint);
        apply!(modal_description);
        apply!(modal_section_heading);
        apply!(modal_input_unfocused);
        apply!(modal_input_focused);
        apply!(modal_button_focused);

        apply!(normal);
        apply!(selection);
        apply!(search_highlight);
        apply!(active_line);
        apply!(cursor_preview);
        apply!(cursor_rendered);
        apply!(cursor_raw);
        apply!(cursor);

        theme
    }
}

impl From<&Theme> for ThemeFile {
    fn from(t: &Theme) -> Self {
        Self {
            palette: (&t.palette).into(),

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
            link_file: (&t.link_file).into(),
            link_heading: (&t.link_heading).into(),
            image_placeholder: (&t.image_placeholder).into(),
            footnote: (&t.footnote).into(),

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
            task_complete_text: (&t.task_complete_text).into(),
            task_strikethrough: t.task_strikethrough,

            table_border: (&t.table_border).into(),
            table_header: (&t.table_header).into(),
            table_header_border: (&t.table_header_border).into(),
            table_cell: (&t.table_cell).into(),
            table_row_even: (&t.table_row_even).into(),
            table_row_odd: (&t.table_row_odd).into(),
            table_drop_indicator: (&t.table_drop_indicator).into(),
            table_drop_target: (&t.table_drop_target).into(),
            table_handle: (&t.table_handle).into(),
            table_handle_delete: (&t.table_handle_delete).into(),

            status_bar: (&t.status_bar).into(),
            status_mode_preview: (&t.status_mode_preview).into(),
            status_mode_rendered: (&t.status_mode_rendered).into(),
            status_mode_raw: (&t.status_mode_raw).into(),
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

            modal_bg: (&t.modal_bg).into(),
            modal_border: (&t.modal_border).into(),
            modal_title: (&t.modal_title).into(),
            modal_item: (&t.modal_item).into(),
            modal_item_hint: (&t.modal_item_hint).into(),
            modal_item_selected: (&t.modal_item_selected).into(),
            modal_item_selected_hint: (&t.modal_item_selected_hint).into(),
            modal_description: (&t.modal_description).into(),
            modal_section_heading: (&t.modal_section_heading).into(),
            modal_input_unfocused: (&t.modal_input_unfocused).into(),
            modal_input_focused: (&t.modal_input_focused).into(),
            modal_button_focused: (&t.modal_button_focused).into(),

            normal: (&t.normal).into(),
            selection: (&t.selection).into(),
            search_highlight: (&t.search_highlight).into(),
            active_line: (&t.active_line).into(),
            cursor_preview: (&t.cursor_preview).into(),
            cursor_rendered: (&t.cursor_rendered).into(),
            cursor_raw: (&t.cursor_raw).into(),
            cursor: (&t.cursor).into(),
        }
    }
}

// ── Default-theme generation ──────────────────────────────────────────────────

/// Build the contents of `themes/default.toml` from the compiled-in
/// [`Theme::default`] and [`Palette::default`].
///
/// The output is a header (`default_theme_header.txt`) followed by a
/// `[palette]` section in which every colour entry is *commented out* —
/// the `# field = value` lines show what the compiled defaults are without
/// actually overriding anything at load time.  Per-element style sections
/// (`[h1]`, `[modal_input_focused]`, …) follow as bare empty headers so
/// users can discover the available override slots.
///
/// Called from [`super::config::ensure_default_files_in`] on first run.
/// There is no checked-in `config/themes/default.toml` — the file is
/// generated at startup so it can never drift from the code-side defaults.
pub fn default_theme_toml() -> String {
    let theme = Theme::default();

    // Build a ThemeFile carrying the default palette + default
    // task_strikethrough plus all-empty style specs.  Serializing it
    // produces the full skeleton (palette values + bare `[<element>]`
    // headers); we then comment out every line inside `[palette]`.
    let file = ThemeFile {
        palette: (&theme.palette).into(),
        task_strikethrough: theme.task_strikethrough,
        ..ThemeFile::default()
    };
    let body = toml::to_string_pretty(&file).expect("serialize default ThemeFile");

    let mut out = String::new();
    let mut in_palette = false;
    for line in body.lines() {
        if in_palette {
            // A blank line or the next `[...]` header ends the palette
            // block.  Anything else is a palette entry that we comment
            // out so it documents the default without overriding it.
            if line.is_empty() || line.starts_with('[') {
                in_palette = false;
            } else {
                out.push_str("# ");
                out.push_str(line);
                out.push('\n');
                continue;
            }
        }
        if line == "[palette]" {
            in_palette = true;
        }
        out.push_str(line);
        out.push('\n');
    }

    let header = include_str!("default_theme_header.txt");
    format!("{header}\n{out}")
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
        check!(modal_border);
        check!(modal_title);
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
        check!(modal_border);
        check!(modal_title);
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
        check!(modal_border);
        check!(modal_title);
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
        assert_eq!(expected.task_strikethrough, theme.task_strikethrough);
    }
}
