use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ropey::Rope;

/// Wraps a `ropey::Rope` with file-level I/O and basic edit operations.
///
/// All positions and lengths are in Unicode scalar values (Rust `char`s),
/// matching ropey's native char-index API.
#[derive(Debug, Clone)]
pub struct Buffer {
    rope: Rope,
    /// The file this buffer was loaded from or last saved to.
    path: Option<PathBuf>,
}

impl Buffer {
    /// Create an empty buffer with no associated file.
    pub fn new() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
        }
    }

    /// Create a buffer pre-filled with `text` and no associated file.
    /// Used by integration tests in `tests/`.
    #[allow(dead_code, clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            path: None,
        }
    }

    /// Load a file from disk into the buffer.
    pub fn load_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;
        Ok(Self {
            rope: Rope::from_str(&content),
            path: Some(path.to_owned()),
        })
    }

    /// Write the buffer contents to disk at the associated path.
    ///
    /// Returns an error if no path is set.
    pub fn save_file(&self) -> Result<()> {
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Buffer has no associated file path"))?;
        let content = self.rope.to_string();
        std::fs::write(path, content)
            .with_context(|| format!("Failed to write file: {}", path.display()))?;
        Ok(())
    }

    /// Save to an explicit path and update the buffer's associated path.
    /// Used by integration tests in `tests/`.
    #[allow(dead_code)]
    pub fn save_as(&mut self, path: &Path) -> Result<()> {
        let content = self.rope.to_string();
        std::fs::write(path, &content)
            .with_context(|| format!("Failed to write file: {}", path.display()))?;
        self.path = Some(path.to_owned());
        Ok(())
    }

    /// Write the buffer contents to `path` without touching the
    /// buffer's associated path.  The user keeps editing the original
    /// file; `path` receives a snapshot of the current contents.
    pub fn save_copy(&self, path: &Path) -> Result<()> {
        let content = self.rope.to_string();
        std::fs::write(path, &content)
            .with_context(|| format!("Failed to write file: {}", path.display()))?;
        Ok(())
    }

    // ── Query ─────────────────────────────────────────────────────

    /// The file path associated with this buffer, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Total number of Unicode scalar values in the buffer.
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Total number of lines (including a trailing empty line if the buffer
    /// ends with a newline).
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// Return the content of line `idx` (0-indexed) as an owned `String`.
    ///
    /// Returns `None` if `idx >= line_count()`.
    pub fn line(&self, idx: usize) -> Option<String> {
        if idx >= self.rope.len_lines() {
            return None;
        }
        Some(self.rope.line(idx).to_string())
    }

    /// Return the entire buffer contents as a `String`.
    pub fn contents(&self) -> String {
        self.rope.to_string()
    }

    /// Return a reference to the underlying rope for read-only access.
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Return the char offset for the start of `line_idx` (0-indexed).
    pub fn line_to_char(&self, line_idx: usize) -> usize {
        self.rope.line_to_char(line_idx)
    }

    /// Return the line index (0-indexed) that contains `char_idx`.
    pub fn char_to_line(&self, char_idx: usize) -> usize {
        self.rope.char_to_line(char_idx)
    }

    // ── Edit ──────────────────────────────────────────────────────

    /// Insert `text` at char offset `char_idx`.
    pub fn insert(&mut self, char_idx: usize, text: &str) {
        self.rope.insert(char_idx, text);
    }

    /// Insert a single char at char offset `char_idx`.
    /// Used by tests in this crate.
    #[allow(dead_code)]
    pub fn insert_char(&mut self, char_idx: usize, ch: char) {
        self.rope.insert_char(char_idx, ch);
    }

    /// Remove chars in the range `start..end` (char offsets).
    pub fn remove(&mut self, start: usize, end: usize) {
        self.rope.remove(start..end);
    }

    /// Remove a single char at `char_idx` (if in bounds).
    /// Used by tests in this crate.
    #[allow(dead_code)]
    pub fn remove_char(&mut self, char_idx: usize) {
        if char_idx < self.rope.len_chars() {
            self.rope.remove(char_idx..char_idx + 1);
        }
    }

    /// Return a slice of the buffer as a `String`, from `start` to `end`
    /// (char offsets, exclusive end).
    pub fn slice_to_string(&self, start: usize, end: usize) -> String {
        self.rope.slice(start..end).to_string()
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str) -> Buffer {
        Buffer {
            rope: Rope::from_str(text),
            path: None,
        }
    }

    #[test]
    fn new_buffer_is_empty() {
        let b = Buffer::new();
        assert_eq!(b.len_chars(), 0);
        assert_eq!(b.line_count(), 1); // ropey always reports at least one line
    }

    #[test]
    fn insert_and_length() {
        let mut b = Buffer::new();
        b.insert(0, "hello");
        assert_eq!(b.len_chars(), 5);
        assert_eq!(b.contents(), "hello");
    }

    #[test]
    fn insert_char() {
        let mut b = buf("hllo");
        b.insert_char(1, 'e');
        assert_eq!(b.contents(), "hello");
    }

    #[test]
    fn remove_range() {
        let mut b = buf("hello world");
        b.remove(5, 11);
        assert_eq!(b.contents(), "hello");
    }

    #[test]
    fn remove_char_in_bounds() {
        let mut b = buf("hello");
        b.remove_char(2); // remove first 'l'
        assert_eq!(b.contents(), "helo");
    }

    #[test]
    fn remove_char_out_of_bounds_is_noop() {
        let mut b = buf("hi");
        b.remove_char(100); // must not panic
        assert_eq!(b.contents(), "hi");
    }

    #[test]
    fn line_count_and_line() {
        let b = buf("line1\nline2\nline3");
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line(0).unwrap(), "line1\n");
        assert_eq!(b.line(1).unwrap(), "line2\n");
        assert_eq!(b.line(2).unwrap(), "line3");
        assert!(b.line(3).is_none());
    }

    #[test]
    fn slice_to_string() {
        let b = buf("hello world");
        assert_eq!(b.slice_to_string(6, 11), "world");
    }

    #[test]
    fn line_to_char_and_char_to_line() {
        let b = buf("abc\ndef\nghi");
        assert_eq!(b.line_to_char(0), 0);
        assert_eq!(b.line_to_char(1), 4); // after "abc\n"
        assert_eq!(b.line_to_char(2), 8); // after "abc\ndef\n"
        assert_eq!(b.char_to_line(5), 1); // 'd' is on line 1
    }

    #[test]
    fn save_copy_writes_to_path_but_does_not_change_buffer_path() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let original = dir.path().join("orig.md");
        std::fs::write(&original, "# Hello")?;
        let buf = Buffer::load_file(&original)?;

        let copy = dir.path().join("copy.md");
        buf.save_copy(&copy)?;

        // The copy was written.
        assert_eq!(std::fs::read_to_string(&copy)?, "# Hello");
        // The buffer's associated path is unchanged — that's the
        // semantic difference from `save_as`.
        assert_eq!(buf.path(), Some(original.as_path()));
        Ok(())
    }

    #[test]
    fn load_and_save_file() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("test.md");
        std::fs::write(&path, "# Hello\n\nWorld")?;

        let buf = Buffer::load_file(&path)?;
        assert!(buf.contents().contains("Hello"));

        let path2 = dir.path().join("out.md");
        let mut buf2 = buf.clone();
        buf2.save_as(&path2)?;

        let buf3 = Buffer::load_file(&path2)?;
        assert_eq!(buf3.contents(), buf.contents());

        Ok(())
    }
}
