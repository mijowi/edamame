use ratatui::style::Color;
use serde::{Deserialize, Serialize};

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
