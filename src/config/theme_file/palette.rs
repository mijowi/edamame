use ratatui::style::Color;
use serde::{Deserialize, Serialize};

use super::color::ColorField;
use crate::config::theme::Palette;

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
    pub text: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_muted: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg_muted: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_elevated: Option<ColorField>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<ColorField>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ColorField>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<ColorField>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_add: Option<ColorField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_delete: Option<ColorField>,
}

impl PaletteFile {
    /// Resolve `self` against `Palette::default()`, returning a
    /// fully-populated palette.  Missing fields fall through to the
    /// compiled-in default so a partial `[palette]` section is valid.
    /// `light` is passed in explicitly because the flag lives at the
    /// top level of `ThemeFile`, not inside the palette table — taking
    /// it as a parameter avoids the foot-gun of resolving to a default
    /// here and patching it post-hoc.
    pub(super) fn resolve(&self, light: bool) -> Palette {
        let d = Palette::default();
        let pick = |opt: Option<ColorField>, fallback: Color| -> Color {
            opt.map(Color::from).unwrap_or(fallback)
        };
        Palette {
            text: pick(self.text, d.text),
            text_muted: pick(self.text_muted, d.text_muted),
            bg: pick(self.bg, d.bg),
            bg_muted: pick(self.bg_muted, d.bg_muted),
            surface: pick(self.surface, d.surface),
            surface_elevated: pick(self.surface_elevated, d.surface_elevated),
            primary: pick(self.primary, d.primary),
            secondary: pick(self.secondary, d.secondary),
            accent: pick(self.accent, d.accent),
            link: pick(self.link, d.link),
            success: pick(self.success, d.success),
            warning: pick(self.warning, d.warning),
            error: pick(self.error, d.error),
            code: pick(self.code, d.code),
            diff_add: pick(self.diff_add, d.diff_add),
            diff_delete: pick(self.diff_delete, d.diff_delete),
            light,
        }
    }
}

impl From<&Palette> for PaletteFile {
    fn from(p: &Palette) -> Self {
        Self {
            text: Some(p.text.into()),
            text_muted: Some(p.text_muted.into()),
            bg: Some(p.bg.into()),
            bg_muted: Some(p.bg_muted.into()),
            surface: Some(p.surface.into()),
            surface_elevated: Some(p.surface_elevated.into()),
            primary: Some(p.primary.into()),
            secondary: Some(p.secondary.into()),
            accent: Some(p.accent.into()),
            link: Some(p.link.into()),
            success: Some(p.success.into()),
            warning: Some(p.warning.into()),
            error: Some(p.error.into()),
            code: Some(p.code.into()),
            diff_add: Some(p.diff_add.into()),
            diff_delete: Some(p.diff_delete.into()),
        }
    }
}
