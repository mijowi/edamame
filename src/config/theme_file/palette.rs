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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_dim: Option<ColorField>,
}

impl PaletteFile {
    /// Resolve `self` against `Palette::default()`, returning a
    /// fully-populated palette.  Missing fields fall through to the
    /// compiled-in default so a partial `[palette]` section is valid.
    pub(super) fn resolve(&self) -> Palette {
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
            code_bright: pick(self.code, d.code_bright),
            code_dim: pick(self.code_dim, d.code_dim),
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
            code: Some(p.code_bright.into()),
            code_dim: Some(p.code_dim.into()),
        }
    }
}
