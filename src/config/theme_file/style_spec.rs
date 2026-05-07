use ratatui::style::{Modifier, Style};
use serde::{Deserialize, Serialize};

use super::color::ColorField;

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

/// Read a `StyleSpec`'s on/off modifier flag.
type ModifierReader = fn(&StyleSpec) -> bool;

/// Modifier flag table — the single source of truth that `StyleSpec`
/// modifier handling reads in both directions.  Adding a modifier means
/// adding one row here plus one bool field on `StyleSpec`.
const MODIFIER_FLAGS: &[(ModifierReader, Modifier)] = &[
    (|s| s.bold, Modifier::BOLD),
    (|s| s.italic, Modifier::ITALIC),
    (|s| s.underlined, Modifier::UNDERLINED),
    (|s| s.reversed, Modifier::REVERSED),
    (|s| s.crossed_out, Modifier::CROSSED_OUT),
    (|s| s.dim, Modifier::DIM),
];

impl StyleSpec {
    /// True when this spec carries no overrides — used by the merge
    /// step so that an empty `[h1]` section in TOML doesn't clobber
    /// the palette-derived default.
    pub(super) fn is_empty(&self) -> bool {
        self.fg.is_none() && self.bg.is_none() && MODIFIER_FLAGS.iter().all(|(read, _)| !read(self))
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
        let modifiers = MODIFIER_FLAGS
            .iter()
            .filter(|(read, _)| read(spec))
            .fold(Modifier::empty(), |acc, (_, m)| acc | *m);
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
