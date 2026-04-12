use std::fmt;

/// The editor's rendering and interaction mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Read-only preview. No cursor shown, no raw Markdown visible.
    /// Files open in this mode.
    #[default]
    Preview,

    /// Hybrid rendered/raw editing. Cursor visible; the active line (or active
    /// table cell) is shown as raw Markdown while the rest is rendered.
    /// Entered on click or first keystroke (not scroll).
    Rendered,

    /// Entire document shown as plain Markdown text. Standard text editing.
    Raw,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::Preview => f.write_str("PREVIEW"),
            Mode::Rendered => f.write_str("EDIT"),
            Mode::Raw => f.write_str("RAW"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_display() {
        assert_eq!(Mode::Preview.to_string(), "PREVIEW");
        assert_eq!(Mode::Rendered.to_string(), "EDIT");
        assert_eq!(Mode::Raw.to_string(), "RAW");
    }

    #[test]
    fn mode_default_is_preview() {
        assert_eq!(Mode::default(), Mode::Preview);
    }
}
