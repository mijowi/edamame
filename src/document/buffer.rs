use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ropey::Rope;

/// The newline convention a document uses on disk.
///
/// The rope is always stored with pure `\n` line breaks (see
/// [`normalize_newlines`]); this records what the file used so a save can
/// reproduce it. New/empty buffers pick the host platform's default —
/// `Crlf` on Windows, `Lf` everywhere else — via [`LineEnding::default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// Unix `\n`.  The default off Windows.
    #[cfg_attr(not(windows), default)]
    Lf,
    /// Windows `\r\n`.  The default on Windows.
    #[cfg_attr(windows, default)]
    Crlf,
}

impl LineEnding {
    /// Classify `text` by its **first** line break: `\r\n` → [`Crlf`],
    /// a bare `\n` → [`Lf`]. A text with no line break at all adopts the
    /// platform default ([`LineEnding::default`]) so a one-line file saved
    /// with a newline appended matches its neighbors.
    ///
    /// [`Crlf`]: LineEnding::Crlf
    /// [`Lf`]: LineEnding::Lf
    pub fn detect(text: &str) -> Self {
        match text.find('\n') {
            // `find` gives a byte index; a preceding `\r` is one byte.
            Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => LineEnding::Crlf,
            Some(_) => LineEnding::Lf,
            None => LineEnding::default(),
        }
    }

    /// The byte sequence this convention writes for one line break.
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        }
    }
}

/// Collapse CRLF to the internal `\n`-only form.
///
/// The whole app models a document with pure `\n` line breaks, so every
/// path that ingests document text from disk (buffer load/reload, the
/// filesystem watcher, `--diff` reads) funnels through here. Returns the
/// input untouched — no reallocation — when it holds no `\r`, the common
/// Unix/macOS case. A lone `\r` (classic-Mac line ends, essentially
/// extinct) is left as-is: only the `\r` of a `\r\n` pair is removed.
pub(crate) fn normalize_newlines(text: String) -> String {
    if text.as_bytes().contains(&b'\r') {
        text.replace("\r\n", "\n")
    } else {
        text
    }
}

/// Expand the internal `\n`-only form back to `ending`'s convention.
///
/// The counterpart to [`normalize_newlines`] for the *outgoing* clipboard
/// boundary — copying document text to another application should hand it
/// the newline style the document uses, just as a save does (see
/// [`write_lines`]). `text` is assumed already `\n`-normalized (it comes
/// from the rope), so [`Lf`] returns it untouched and [`Crlf`] simply
/// widens each `\n` to `\r\n`.
///
/// [`Lf`]: LineEnding::Lf
/// [`Crlf`]: LineEnding::Crlf
pub(crate) fn encode_newlines(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Lf => text.to_owned(),
        LineEnding::Crlf => text.replace('\n', "\r\n"),
    }
}

/// Write `rope` to `w`, translating each `\n` to `ending`'s byte
/// sequence. Streams the rope chunk by chunk rather than materializing a
/// translated `String`: `\n` is a single byte and never straddles a
/// chunk boundary, so each chunk can be split independently.
fn write_lines<W: Write>(rope: &Rope, ending: LineEnding, w: &mut W) -> std::io::Result<()> {
    let crlf = ending == LineEnding::Crlf;
    for chunk in rope.chunks() {
        if !crlf {
            w.write_all(chunk.as_bytes())?;
            continue;
        }
        let mut rest = chunk;
        while let Some(i) = rest.find('\n') {
            w.write_all(&rest.as_bytes()[..i])?;
            w.write_all(b"\r\n")?;
            rest = &rest[i + 1..];
        }
        w.write_all(rest.as_bytes())?;
    }
    Ok(())
}

/// Wraps a `ropey::Rope` with file-level I/O and basic edit operations.
///
/// All positions and lengths are in Unicode scalar values (Rust `char`s),
/// matching ropey's native char-index API.
///
/// The rope always holds pure `\n` line breaks; [`line_ending`] records
/// the on-disk convention so a save reproduces it. See [`LineEnding`] and
/// [`normalize_newlines`].
///
/// [`line_ending`]: Buffer::line_ending
#[derive(Debug, Clone)]
pub struct Buffer {
    rope: Rope,
    /// The file this buffer was loaded from or last saved to.
    path: Option<PathBuf>,
    /// The newline convention to write on save. Detected on load,
    /// platform-default for a new buffer.
    line_ending: LineEnding,
    /// Monotonically-increasing counter bumped on every content mutation.
    /// Consumers that cache buffer-derived data (e.g. the raw-mode visual
    /// row cache in `EditorState`) compare this against their stored
    /// version to invalidate stale entries without rebuilding eagerly.
    /// Wraps on overflow — wrap-equality is fine because adjacent edits
    /// always differ, and wrap-around requires 2^64 mutations.
    version: u64,
}

impl Buffer {
    /// Create an empty buffer with no associated file.
    pub fn new() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            line_ending: LineEnding::default(),
            version: 0,
        }
    }

    /// Create a buffer pre-filled with `text` and no associated file.
    ///
    /// The pathless entry point: used by `App::load_doc_into_editor`
    /// for a page of the embedded manual, whose text lives in the
    /// binary rather than on disk, and by integration tests in
    /// `tests/`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Self {
        let line_ending = LineEnding::detect(text);
        let normalized = normalize_newlines(text.to_owned());
        Self {
            rope: Rope::from_str(&normalized),
            path: None,
            line_ending,
            version: 0,
        }
    }

    /// Create a buffer wrapping a pre-built rope with no associated
    /// file.  Used by the diff subsystem (`DiffState::new`) so the
    /// new-side text gets the same `Buffer` API as the main buffer
    /// without re-allocating the rope from a `String`.
    pub fn from_rope(rope: Rope) -> Self {
        Self {
            rope,
            path: None,
            line_ending: LineEnding::default(),
            version: 0,
        }
    }

    /// Load a file from disk into the buffer.
    ///
    /// Detects the file's line-ending convention (from its first line
    /// break) and normalizes the rope to pure `\n`; a later save
    /// reproduces the detected convention.
    pub fn load_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;
        let line_ending = LineEnding::detect(&content);
        Ok(Self {
            rope: Rope::from_str(&normalize_newlines(content)),
            path: Some(path.to_owned()),
            line_ending,
            version: 0,
        })
    }

    /// Empty buffer associated with `path`, used when the user names a
    /// file that does not yet exist.  Saving will create the file.
    pub fn for_new_file(path: &Path) -> Self {
        Self {
            rope: Rope::new(),
            path: Some(path.to_owned()),
            line_ending: LineEnding::default(),
            version: 0,
        }
    }

    /// Rebuild a buffer from a fresh on-disk read.  Used by
    /// `App::reload_buffer_from_disk` when an external edit replaces
    /// the file under us.  The new buffer's `version` starts at
    /// `previous_version.wrapping_add(1)` so the monotonic-version
    /// invariant other consumers rely on (e.g. the autosave
    /// edit-detection check in `tick_autosave`) is preserved across
    /// the buffer swap.  Unlike [`Self::load_file`], the bytes come
    /// from the caller — the watcher worker has already read them —
    /// so there is no second disk hit and no chance of racing the
    /// next watcher event.  `contents` is normalized to pure `\n` here
    /// (the watcher may hand it over verbatim); `line_ending` is passed in
    /// rather than re-detected — the caller carries the buffer's existing
    /// convention forward so an external rewrite does not flip a `Crlf`
    /// document to `Lf` merely because the delivered bytes were already
    /// normalized upstream.
    pub fn reload(
        path: &Path,
        contents: &str,
        previous_version: u64,
        line_ending: LineEnding,
    ) -> Self {
        Self {
            rope: Rope::from_str(&normalize_newlines(contents.to_owned())),
            path: Some(path.to_owned()),
            line_ending,
            version: previous_version.wrapping_add(1),
        }
    }

    /// Write the buffer contents to disk at the associated path.
    ///
    /// Returns an error if no path is set.
    pub fn save_file(&self) -> Result<()> {
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Buffer has no associated file path"))?;
        self.write_to_disk(path)
    }

    /// Stream the rope to `path`, translating `\n` to the buffer's
    /// [`line_ending`](Self::line_ending) on the way out.  The shared
    /// write primitive behind every save path; a `BufWriter` keeps the
    /// per-line `\r\n` translation from turning into a syscall per line.
    fn write_to_disk(&self, path: &Path) -> Result<()> {
        let ctx = || format!("Failed to write file: {}", path.display());
        let file = std::fs::File::create(path).with_context(ctx)?;
        let mut w = std::io::BufWriter::new(file);
        write_lines(&self.rope, self.line_ending, &mut w)
            .and_then(|()| w.flush())
            .with_context(ctx)?;
        Ok(())
    }

    /// Save to an explicit path and adopt it as the buffer's associated
    /// path.  The shared write primitive behind every "Save As" flow
    /// (command palette, a path-less `Save`, vim `:w <path>` / `:saveas`,
    /// and the file-deleted recovery flow) via
    /// [`crate::app::App::save_buffer_as`], plus integration tests in
    /// `tests/`.
    ///
    /// Overwrites `path` **unconditionally** — this is the low-level
    /// force primitive.  Callers are responsible for confirming an
    /// overwrite of a *different* existing file first (see
    /// [`Self::would_overwrite`] and the `OverwriteConfirmModal` flow);
    /// saving over the buffer's own path is a normal in-place save and
    /// needs no confirmation.
    pub fn save_as(&mut self, path: &Path) -> Result<()> {
        self.write_to_disk(path)?;
        self.path = Some(path.to_owned());
        Ok(())
    }

    /// True when writing to `path` would clobber a *different* existing
    /// file — i.e. `path` already exists and is not this buffer's own
    /// associated path.  Saving over the buffer's current file is a
    /// normal in-place save (never an "overwrite" in this sense), so a
    /// Save As to the seeded current path passes.  Callers use this to
    /// decide whether to confirm before [`Self::save_as`], which writes
    /// unconditionally.
    ///
    /// Comparison is by stored path value, so an unusual spelling of the
    /// same file (e.g. relative vs. absolute) may prompt a harmless extra
    /// confirmation — saying yes simply re-saves that same file.
    pub fn would_overwrite(&self, path: &Path) -> bool {
        path.exists() && self.path.as_deref() != Some(path)
    }

    /// Write the buffer contents to `path` without touching the
    /// buffer's associated path.  The user keeps editing the original
    /// file; `path` receives a snapshot of the current contents.
    pub fn save_copy(&self, path: &Path) -> Result<()> {
        self.write_to_disk(path)
    }

    // ── Query ─────────────────────────────────────────────────────

    /// The file path associated with this buffer, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The newline convention this buffer writes on save.
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Override the newline convention used for subsequent saves.
    pub fn set_line_ending(&mut self, line_ending: LineEnding) {
        self.line_ending = line_ending;
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

    /// Return the line index (0-indexed) that contains `byte_idx`.
    /// Convenience for the common `char_to_line(byte_to_char(b))` pair —
    /// callers that have a byte offset (block ranges, source-map
    /// lookups) avoid the double-call and the chance of getting the
    /// argument order wrong.
    pub fn byte_to_line(&self, byte_idx: usize) -> usize {
        self.rope.char_to_line(self.rope.byte_to_char(byte_idx))
    }

    /// Buffer line index for the `raw_line_idx`-th line within a block
    /// whose byte range starts at `block_byte_start`.
    pub fn block_line_to_buffer_line(&self, block_byte_start: usize, raw_line_idx: usize) -> usize {
        let block_start_char = self.rope.byte_to_char(block_byte_start);
        let block_start_line = self.rope.char_to_line(block_start_char);
        block_start_line + raw_line_idx
    }

    /// Monotonic version counter — increments on every content mutation.
    /// Consumers cache derived state keyed by `(version, ...)` so
    /// invalidation is a cheap `u64` comparison.
    pub fn version(&self) -> u64 {
        self.version
    }

    // ── Edit ──────────────────────────────────────────────────────

    /// Insert `text` at char offset `char_idx`.
    pub fn insert(&mut self, char_idx: usize, text: &str) {
        self.rope.insert(char_idx, text);
        self.version = self.version.wrapping_add(1);
    }

    /// Insert a single char at char offset `char_idx`.
    /// Used by tests in this crate.
    #[allow(dead_code)]
    pub fn insert_char(&mut self, char_idx: usize, ch: char) {
        self.rope.insert_char(char_idx, ch);
        self.version = self.version.wrapping_add(1);
    }

    /// Remove chars in the range `start..end` (char offsets).
    pub fn remove(&mut self, start: usize, end: usize) {
        self.rope.remove(start..end);
        self.version = self.version.wrapping_add(1);
    }

    /// Remove a single char at `char_idx` (if in bounds).
    /// Used by tests in this crate.
    #[allow(dead_code)]
    pub fn remove_char(&mut self, char_idx: usize) {
        if char_idx < self.rope.len_chars() {
            self.rope.remove(char_idx..char_idx + 1);
            self.version = self.version.wrapping_add(1);
        }
    }

    /// Return a slice of the buffer as a `String`, from `start` to `end`
    /// (char offsets, exclusive end).
    pub fn slice_to_string(&self, start: usize, end: usize) -> String {
        self.rope.slice(start..end).to_string()
    }

    /// Return the source between byte offsets `start..end` as a `String`,
    /// or `None` when the range is out of bounds or lands mid-character.
    ///
    /// The byte-addressed counterpart of [`Buffer::slice_to_string`], for
    /// the callers that already hold a byte range — a source-map block
    /// range, say.  The whole point is to *not* be `contents()`: a link
    /// hit-test runs on every mouse-move event, and materializing the
    /// document to slice one block out of it is O(document) per pointer
    /// report.  Non-panicking (ropey's `get_byte_slice`) so the caller
    /// keeps the defensive fallback it had when it was slicing a `String`.
    pub fn byte_slice_to_string(&self, start: usize, end: usize) -> Option<String> {
        self.rope
            .get_byte_slice(start..end)
            .map(|slice| slice.to_string())
    }

    /// Total length of the buffer in bytes.
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Replace the underlying rope wholesale while preserving the
    /// buffer's `path`.  Bumps `version` so downstream consumers
    /// (autosave detector, raw-view visual-row cache, parsed-doc
    /// invalidation) treat the swap as a fresh mutation.  Used by the
    /// diff-mode resolution path to swap the merged rope in place
    /// (§3, §6).  The caller is responsible for refreshing
    /// any derived state on `EditorState` (`refresh_parsed`,
    /// `update_cursor_block`, clamping the cursor).
    pub fn set_rope(&mut self, rope: Rope) {
        self.rope = rope;
        self.version = self.version.wrapping_add(1);
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
            line_ending: LineEnding::Lf,
            version: 0,
        }
    }

    #[test]
    fn byte_slice_to_string_extracts_a_range_and_declines_a_bad_one() {
        let b = buf("héllo wörld");
        // `é` is two bytes, so the byte range is not the char range.
        assert_eq!(b.byte_slice_to_string(0, 6).as_deref(), Some("héllo"));
        assert_eq!(
            b.byte_slice_to_string(0, b.len_bytes()).as_deref(),
            Some("héllo wörld")
        );
        // Mid-character and out-of-bounds both decline rather than panic —
        // the link hit-test relies on that for its defensive fallback.
        assert_eq!(b.byte_slice_to_string(2, 6), None);
        assert_eq!(b.byte_slice_to_string(0, b.len_bytes() + 1), None);
    }

    #[test]
    fn new_buffer_is_empty() {
        let b = Buffer::new();
        assert_eq!(b.len_chars(), 0);
        assert_eq!(b.line_count(), 1); // ropey always reports at least one line
    }

    #[test]
    fn would_overwrite_only_for_a_different_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let existing = dir.path().join("there.md");
        std::fs::write(&existing, "x").expect("seed");
        let missing = dir.path().join("absent.md");

        let mut b = buf("body");
        // Path-less buffer: any existing target is an overwrite; a
        // nonexistent one is not.
        assert!(b.would_overwrite(&existing));
        assert!(!b.would_overwrite(&missing));

        // Saving over the buffer's own file is never an "overwrite".
        b.path = Some(existing.clone());
        assert!(!b.would_overwrite(&existing));
        // …but a *different* existing file still is.
        let other = dir.path().join("other.md");
        std::fs::write(&other, "y").expect("seed");
        assert!(b.would_overwrite(&other));
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

    // ── Line endings ──────────────────────────────────────────────

    #[test]
    fn detect_classifies_by_first_line_break() {
        assert_eq!(LineEnding::detect("a\r\nb\r\n"), LineEnding::Crlf);
        assert_eq!(LineEnding::detect("a\nb\n"), LineEnding::Lf);
        // First break wins: a bare `\n` up front reads as Lf even when a
        // later line is CRLF.
        assert_eq!(LineEnding::detect("a\nb\r\n"), LineEnding::Lf);
        // A leading `\r\n` still counts (the `\r` sits at index 0's
        // predecessor of the first `\n`).
        assert_eq!(LineEnding::detect("\r\nx"), LineEnding::Crlf);
        // No line break at all → platform default.
        assert_eq!(LineEnding::detect("no newline"), LineEnding::default());
    }

    #[test]
    fn normalize_newlines_strips_only_crlf_pairs() {
        assert_eq!(normalize_newlines("a\r\nb\r\n".to_owned()), "a\nb\n");
        // Already-LF text is returned untouched (and does not reallocate,
        // though that we cannot assert directly).
        assert_eq!(normalize_newlines("a\nb\n".to_owned()), "a\nb\n");
        // A lone `\r` (not part of a pair) is preserved.
        assert_eq!(normalize_newlines("a\rb".to_owned()), "a\rb");
    }

    #[test]
    fn encode_newlines_widens_only_for_crlf() {
        // Lf is the identity on already-`\n` text.
        assert_eq!(encode_newlines("a\nb\n", LineEnding::Lf), "a\nb\n");
        // Crlf widens each `\n`.
        assert_eq!(encode_newlines("a\nb\n", LineEnding::Crlf), "a\r\nb\r\n");
        // Round-trips with normalize_newlines (the clipboard boundary).
        let lf = "one\ntwo\nthree";
        let crlf = encode_newlines(lf, LineEnding::Crlf);
        assert_eq!(normalize_newlines(crlf), lf);
    }

    #[test]
    fn load_detects_crlf_and_normalizes_rope() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("crlf.md");
        std::fs::write(&path, "# Title\r\n\r\nBody\r\n")?;

        let buf = Buffer::load_file(&path)?;
        assert_eq!(buf.line_ending(), LineEnding::Crlf);
        // The rope holds no `\r`: every consumer sees pure `\n`.
        assert_eq!(buf.contents(), "# Title\n\nBody\n");
        assert!(!buf.contents().contains('\r'));
        Ok(())
    }

    #[test]
    fn load_detects_lf() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("lf.md");
        std::fs::write(&path, "# Title\n\nBody\n")?;

        let buf = Buffer::load_file(&path)?;
        assert_eq!(buf.line_ending(), LineEnding::Lf);
        assert_eq!(buf.contents(), "# Title\n\nBody\n");
        Ok(())
    }

    #[test]
    fn save_reproduces_crlf_on_disk_while_rope_stays_lf() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let src = dir.path().join("crlf.md");
        std::fs::write(&src, "one\r\ntwo\r\n")?;

        let buf = Buffer::load_file(&src)?;
        let out = dir.path().join("out.md");
        buf.save_copy(&out)?;

        // Read back the raw bytes (not `read_to_string`, which would not
        // reveal the `\r`): CRLF was reproduced verbatim.
        let raw = std::fs::read(&out)?;
        assert_eq!(raw, b"one\r\ntwo\r\n");
        Ok(())
    }

    #[test]
    fn save_writes_lf_for_an_lf_buffer() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut buf = Buffer::from_str("one\ntwo\n");
        assert_eq!(buf.line_ending(), LineEnding::Lf);
        let out = dir.path().join("out.md");
        buf.save_as(&out)?;
        assert_eq!(std::fs::read(&out)?, b"one\ntwo\n");
        Ok(())
    }

    #[test]
    fn crlf_file_round_trips_through_load_and_save() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("doc.md");
        let original = b"a\r\nb\r\nc";
        std::fs::write(&path, original)?;

        let buf = Buffer::load_file(&path)?;
        // Save back in place.
        buf.save_file()?;
        assert_eq!(std::fs::read(&path)?, original);
        Ok(())
    }

    #[test]
    fn edits_to_a_crlf_buffer_still_save_as_crlf() -> Result<()> {
        // An inserted newline is a bare `\n` in the rope, but the save
        // translation applies to it too — no mixed endings on disk.
        let dir = tempfile::tempdir()?;
        let src = dir.path().join("crlf.md");
        std::fs::write(&src, "a\r\nb\r\n")?;
        let mut buf = Buffer::load_file(&src)?;

        // Insert "X\n" at the start.
        buf.insert(0, "X\n");
        let out = dir.path().join("out.md");
        buf.save_copy(&out)?;
        assert_eq!(std::fs::read(&out)?, b"X\r\na\r\nb\r\n");
        Ok(())
    }

    #[test]
    fn from_str_detects_and_normalizes() {
        let buf = Buffer::from_str("x\r\ny\r\n");
        assert_eq!(buf.line_ending(), LineEnding::Crlf);
        assert_eq!(buf.contents(), "x\ny\n");
    }

    #[test]
    fn new_and_empty_buffers_use_the_platform_default() {
        assert_eq!(Buffer::new().line_ending(), LineEnding::default());
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Buffer::for_new_file(&dir.path().join("new.md"));
        assert_eq!(f.line_ending(), LineEnding::default());
    }

    #[test]
    fn reload_carries_the_ending_forward_and_normalizes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("doc.md");
        // Watcher may hand over verbatim CRLF bytes; reload normalizes.
        let reloaded = Buffer::reload(&path, "p\r\nq\r\n", 7, LineEnding::Crlf);
        assert_eq!(reloaded.contents(), "p\nq\n");
        assert_eq!(reloaded.line_ending(), LineEnding::Crlf);
        assert_eq!(reloaded.version(), 8);
    }
}
