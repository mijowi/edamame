//! Phase 10 — markdown syntax cheat sheet.
//!
//! Static `&str` fixture covering the syntax `edamame` actually
//! renders.  Surfaced via the command palette entry
//! `Show Markdown Cheat Sheet`.  Not parsed at runtime — the cheat
//! sheet is internal documentation, not user-facing content, so the
//! literal Markdown source is fine.
//!
//! Tables and footnotes are intentionally absent: tables have a
//! dedicated insert/edit flow (see Phase 15) so hand-coding the
//! pipe-grid form is rarely useful, and footnotes are not yet
//! implemented in the renderer.

/// The full cheat-sheet body, one logical row per line.  Rendered as
/// a [`crate::ui::ModalView`] body — newlines split rows, no Markdown
/// reflow happens.  Kept terse so the popover fits comfortably in a
/// 24-row terminal.
pub const MARKDOWN_CHEAT_SHEET: &str = "\
Headings
  # H1   ## H2   ### H3   #### H4   ##### H5   ###### H6

Inline
  **bold**   _italic_  **_bold italic_**   `code`   ~~strike~~   ==highlight==

Lists
  - bullet                    1. ordered
    - nested                     2. next
  - [ ] task   - [x] done

Links
  [section](#heading-anchor)
  [local file](./notes.md)
  [website](https://example.com)

Images
  ![alt text](./diagram.png)
  ![alt text](https://example.com/photo.jpg)

Block quote
  > quoted text spans
  > multiple lines.

Code block
  ```rust
  fn main() {}
  ```

Horizontal rule
  ---

Hard line break
  Two spaces at end of line  ⏎

Diagrams (Mermaid)
  ```mermaid
  graph TD; A-->B;
  ```
";

/// Split [`MARKDOWN_CHEAT_SHEET`] into the per-line `Vec<String>`
/// expected by [`crate::ui::ModalView`].  Trailing newline is skipped
/// so the modal doesn't show a blank row at the bottom.
pub fn body_lines() -> Vec<String> {
    MARKDOWN_CHEAT_SHEET.lines().map(|s| s.to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cheat_sheet_includes_supported_features() {
        let s = MARKDOWN_CHEAT_SHEET;
        assert!(s.contains("- [ ]"));
        assert!(s.contains("~~strike~~"));
        assert!(s.contains("==highlight=="));
        assert!(s.contains("Mermaid"));
        assert!(s.contains("Links"));
        assert!(s.contains("Images"));
        // Header anchor + local file + http examples in the Links section.
        assert!(s.contains("#heading-anchor"));
        assert!(s.contains("./notes.md"));
        assert!(s.contains("https://example.com"));
    }

    #[test]
    fn cheat_sheet_excludes_unsupported_or_redundant_sections() {
        // Tables are surfaced through Phase 2 / Phase 15 dedicated
        // editing flows, not hand-coded markdown.  Footnotes are not
        // yet implemented in the renderer.  Both must stay out of the
        // cheat sheet so users aren't pointed at syntax we don't
        // honour.
        let s = MARKDOWN_CHEAT_SHEET;
        assert!(
            !s.contains("Tables"),
            "Tables should not appear in the cheat sheet"
        );
        assert!(!s.contains("Footnotes"), "Footnotes should not appear");
        assert!(!s.contains("[^"), "footnote markers should not appear");
    }

    #[test]
    fn cheat_sheet_excludes_html_passthrough() {
        // The renderer does not honour raw HTML, so the cheat sheet
        // should not advertise `<br>` / `<details>` / `<sub>`-style
        // tags.  This keeps user expectations honest.
        let s = MARKDOWN_CHEAT_SHEET;
        for token in &["<br>", "<details>", "<sub>", "<sup>"] {
            assert!(
                !s.contains(token),
                "cheat sheet leaks unrendered HTML token: {token}"
            );
        }
    }

    #[test]
    fn body_lines_drops_trailing_blank_row() {
        // `MARKDOWN_CHEAT_SHEET` ends with `\n`; `lines()` discards the
        // implicit empty trailing element, so the last visible row is
        // the closing fence of the mermaid block.
        let lines = body_lines();
        assert_eq!(lines.last().map(String::as_str), Some("  ```"));
    }
}
