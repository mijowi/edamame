use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ropey::Rope;
use tempfile::NamedTempFile;

/// The newline convention a document uses on disk. The rope always holds pure `\n` (see
/// [`normalize_newlines`]); this records what to write back. The default is the host platform's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[cfg_attr(not(windows), default)]
    Lf,
    #[cfg_attr(windows, default)]
    Crlf,
}

impl LineEnding {
    /// Classify `text` by its **first** line break; text with none adopts the platform default.
    pub fn detect(text: &str) -> Self {
        match text.find('\n') {
            Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => LineEnding::Crlf,
            Some(_) => LineEnding::Lf,
            None => LineEnding::default(),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        }
    }
}

/// Collapse CRLF to the internal `\n`-only form. Every path that ingests document text (load,
/// reload, watcher, `--diff`) funnels through here. No reallocation when there is no `\r`; a lone
/// `\r` is left as-is.
pub(crate) fn normalize_newlines(text: String) -> String {
    if text.as_bytes().contains(&b'\r') {
        text.replace("\r\n", "\n")
    } else {
        text
    }
}

/// Expand `\n`-normalized `text` to `ending`'s convention: the outgoing clipboard counterpart of
/// [`normalize_newlines`], so a copy hands other applications the document's own newline style.
pub(crate) fn encode_newlines(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Lf => text.to_owned(),
        LineEnding::Crlf => text.replace('\n', "\r\n"),
    }
}

/// Stream `rope` to `w`, translating each `\n` to `ending`. Works chunk by chunk without a
/// translated `String`: `\n` is one byte, so it never straddles a chunk boundary.
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

// ── Save ──────────────────────────────────────────────────────────────────

/// The suffix of the backup a save makes beside an existing document before overwriting it, e.g.
/// `notes.md.a1B2c3.edamame-backup`. One is left behind only when a save is interrupted (the
/// process dies mid-write) or fails and cannot restore the original.
pub(crate) const BACKUP_SUFFIX: &str = ".edamame-backup";

/// Write `path` in place, guarded by a backup of its previous contents (Vim's `backupcopy=yes`).
///
/// The file is truncated and rewritten, never replaced, so symlinks, hard links, ownership,
/// permissions and the watcher's view of the file are untouched. Steps:
///
/// 1. An existing target is copied to a [`backup_existing`] beside it. A full disk or quota
///    aborts the save before the original is touched; any other backup failure (say, a read-only
///    directory) falls back to writing without one. A target that does not exist skips this.
/// 2. `write` fills the buffered file, which is then flushed and [`sync`]ed.
/// 3. On success the backup is deleted (dropped). On failure the original is [`restore`]d from
///    it; if that fails too, the backup is kept and its path leads the error, because callers
///    show only the outermost context.
///
/// Generic over `write` so tests can inject a mid-write failure.
fn write_with_backup<F>(path: &Path, write: F) -> Result<()>
where
    F: FnOnce(&mut BufWriter<File>) -> io::Result<()>,
{
    let ctx = || format!("Failed to write file: {}", path.display());
    let backup = backup_existing(path).with_context(ctx)?;
    // A failed open has truncated nothing, so it needs no restore.
    let file = File::create(path).with_context(ctx)?;
    let mut w = BufWriter::new(file);
    let written = write(&mut w)
        .and_then(|()| w.flush())
        .and_then(|()| sync(w.get_ref()));
    // Close the file before any restore reopens it.
    drop(w);
    let Err(err) = written else {
        return Ok(());
    };
    let err = anyhow::Error::new(err).context(ctx());
    let Some(backup) = backup else {
        return Err(err);
    };
    match restore(path, backup) {
        Ok(()) => Err(err),
        Err(RestoreFailed { error, kept }) => {
            let err = err.context(format!("could not restore the original: {error}"));
            Err(match kept {
                Some(kept) => err.context(format!(
                    "Failed to write file: {}; its previous contents are in {}",
                    path.display(),
                    kept.display()
                )),
                None => err,
            })
        }
    }
}

/// Copy an existing `path` to a fresh backup file beside it (beside the link target, for a
/// symlink, so the backup shares the document's filesystem and quota). `Ok(None)` when there is
/// nothing to back up, or the backup failed for a reason that does not stop the save. Only a
/// regular file is backed up: opening a FIFO to read it would block until a writer appeared.
fn backup_existing(path: &Path) -> io::Result<Option<NamedTempFile>> {
    let Ok(real) = std::fs::canonicalize(path) else {
        return Ok(None);
    };
    if !std::fs::metadata(&real).is_ok_and(|m| m.is_file()) {
        return Ok(None);
    }
    match copy_to_backup(&real) {
        Ok(backup) => Ok(Some(backup)),
        Err(e) if backup_failure_aborts_save(e.kind()) => Err(e),
        Err(e) => {
            tracing::debug!(target: "app", error = %e, path = %real.display(),
                "no backup before save; writing in place without one");
            Ok(None)
        }
    }
}

/// Out of space is the one backup failure that aborts the save: the in-place write would most
/// likely run out too, after the original was already truncated.
fn backup_failure_aborts_save(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded
    )
}

/// The backup is created `O_EXCL` under a random name, with owner-only permissions, and synced so
/// it is on disk before the original is truncated. A partial copy is deleted on drop.
fn copy_to_backup(real: &Path) -> io::Result<NamedTempFile> {
    let dir = real.parent().unwrap_or(Path::new("."));
    let mut prefix = real.file_name().unwrap_or_default().to_os_string();
    prefix.push(".");
    let mut backup = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(BACKUP_SUFFIX)
        .tempfile_in(dir)?;
    io::copy(&mut File::open(real)?, backup.as_file_mut())?;
    sync(backup.as_file())?;
    Ok(backup)
}

/// Why [`restore`] failed, and where the backup was kept (`None` only if keeping it failed too).
struct RestoreFailed {
    error: io::Error,
    kept: Option<PathBuf>,
}

/// Copy `backup` back over `path` in place, then delete it. On failure, keep it on disk.
fn restore(path: &Path, mut backup: NamedTempFile) -> std::result::Result<(), RestoreFailed> {
    let copied = backup.seek(SeekFrom::Start(0)).and_then(|_| {
        let mut file = OpenOptions::new().write(true).truncate(true).open(path)?;
        io::copy(backup.as_file_mut(), &mut file)?;
        sync(&file)
    });
    let Err(error) = copied else {
        return Ok(());
    };
    let kept = backup.into_temp_path().keep().ok();
    Err(RestoreFailed { error, kept })
}

/// `sync_all`, tolerating a filesystem that does not support it.
fn sync(file: &File) -> io::Result<()> {
    match file.sync_all() {
        Err(e) if e.kind() == io::ErrorKind::Unsupported => Ok(()),
        result => result,
    }
}

/// A `ropey::Rope` with file I/O and edit primitives. Positions are in chars (ropey's native
/// index). The rope always holds pure `\n`; `line_ending` records the on-disk convention.
#[derive(Debug, Clone)]
pub struct Buffer {
    rope: Rope,
    /// The file this buffer was loaded from or last saved to.
    path: Option<PathBuf>,
    line_ending: LineEnding,
    /// Bumped on every content mutation so consumers can invalidate cached derived data by a
    /// cheap comparison. Wrapping is fine: adjacent edits always differ.
    version: u64,
}

impl Buffer {
    pub fn new() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            line_ending: LineEnding::default(),
            version: 0,
        }
    }

    /// Pathless buffer from `text`; used for embedded manual pages and by tests.
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

    /// Pathless buffer over a pre-built rope (the diff new side), avoiding a `String` round-trip.
    pub fn from_rope(rope: Rope) -> Self {
        Self {
            rope,
            path: None,
            line_ending: LineEnding::default(),
            version: 0,
        }
    }

    /// Load from disk, detecting the line ending and normalizing the rope to `\n`.
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

    /// Empty buffer for a `path` that does not exist yet; saving creates it.
    pub fn for_new_file(path: &Path) -> Self {
        Self {
            rope: Rope::new(),
            path: Some(path.to_owned()),
            line_ending: LineEnding::default(),
            version: 0,
        }
    }

    /// Rebuild after an external edit, from bytes the watcher already read (no second disk hit).
    /// `version` continues from `previous_version` so the monotonic invariant survives the swap;
    /// `line_ending` is carried forward rather than re-detected, since the delivered bytes may
    /// already be normalized and must not flip a `Crlf` document to `Lf`.
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

    /// Write to the associated path; errors if there is none.
    pub fn save_file(&self) -> Result<()> {
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Buffer has no associated file path"))?;
        self.write_to_disk(path)
    }

    /// The write primitive behind every save path; see [`write_with_backup`].
    fn write_to_disk(&self, path: &Path) -> Result<()> {
        write_with_backup(path, |w| write_lines(&self.rope, self.line_ending, w))
    }

    /// Save to `path` and adopt it as the associated path. Overwrites **unconditionally**;
    /// callers confirm via [`Self::would_overwrite`] first.
    pub fn save_as(&mut self, path: &Path) -> Result<()> {
        self.write_to_disk(path)?;
        self.path = Some(path.to_owned());
        Ok(())
    }

    /// True when `path` exists and is not this buffer's own path (an in-place save is never an
    /// overwrite). Compared by path value, so a different spelling of the same file may prompt a
    /// harmless extra confirmation.
    pub fn would_overwrite(&self, path: &Path) -> bool {
        path.exists() && self.path.as_deref() != Some(path)
    }

    /// Write a snapshot to `path` without changing the associated path.
    pub fn save_copy(&self, path: &Path) -> Result<()> {
        self.write_to_disk(path)
    }

    // ── Query ─────────────────────────────────────────────────────

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub fn set_line_ending(&mut self, line_ending: LineEnding) {
        self.line_ending = line_ending;
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Line count, including the empty line after a trailing newline.
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// Line `idx` including its trailing newline; `None` when out of range.
    pub fn line(&self, idx: usize) -> Option<String> {
        if idx >= self.rope.len_lines() {
            return None;
        }
        Some(self.rope.line(idx).to_string())
    }

    pub fn contents(&self) -> String {
        self.rope.to_string()
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn line_to_char(&self, line_idx: usize) -> usize {
        self.rope.line_to_char(line_idx)
    }

    pub fn char_to_line(&self, char_idx: usize) -> usize {
        self.rope.char_to_line(char_idx)
    }

    pub fn byte_to_line(&self, byte_idx: usize) -> usize {
        self.rope.char_to_line(self.rope.byte_to_char(byte_idx))
    }

    /// Buffer line of the `raw_line_idx`-th line of a block starting at `block_byte_start`.
    pub fn block_line_to_buffer_line(&self, block_byte_start: usize, raw_line_idx: usize) -> usize {
        let block_start_char = self.rope.byte_to_char(block_byte_start);
        let block_start_line = self.rope.char_to_line(block_start_char);
        block_start_line + raw_line_idx
    }

    /// Mutation counter; see the `version` field.
    pub fn version(&self) -> u64 {
        self.version
    }

    // ── Edit ──────────────────────────────────────────────────────

    pub fn insert(&mut self, char_idx: usize, text: &str) {
        self.rope.insert(char_idx, text);
        self.version = self.version.wrapping_add(1);
    }

    /// Used by tests.
    #[allow(dead_code)]
    pub fn insert_char(&mut self, char_idx: usize, ch: char) {
        self.rope.insert_char(char_idx, ch);
        self.version = self.version.wrapping_add(1);
    }

    pub fn remove(&mut self, start: usize, end: usize) {
        self.rope.remove(start..end);
        self.version = self.version.wrapping_add(1);
    }

    /// Remove the char at `char_idx` if in bounds. Used by tests.
    #[allow(dead_code)]
    pub fn remove_char(&mut self, char_idx: usize) {
        if char_idx < self.rope.len_chars() {
            self.rope.remove(char_idx..char_idx + 1);
            self.version = self.version.wrapping_add(1);
        }
    }

    /// The chars `start..end` as a `String`.
    pub fn slice_to_string(&self, start: usize, end: usize) -> String {
        self.rope.slice(start..end).to_string()
    }

    /// The bytes `start..end` as a `String`, or `None` when out of bounds or mid-character.
    /// Exists so per-mouse-move callers (link hit-test) need not materialize `contents()`.
    pub fn byte_slice_to_string(&self, start: usize, end: usize) -> Option<String> {
        self.rope
            .get_byte_slice(start..end)
            .map(|slice| slice.to_string())
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Replace the rope wholesale (diff resolution), keeping `path` and bumping `version`. The
    /// caller refreshes `EditorState`'s derived state (parse, cursor block, cursor clamp).
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
        assert_eq!(b.byte_slice_to_string(0, 6).as_deref(), Some("héllo"));
        assert_eq!(
            b.byte_slice_to_string(0, b.len_bytes()).as_deref(),
            Some("héllo wörld")
        );
        // Mid-character and out-of-bounds decline rather than panic.
        assert_eq!(b.byte_slice_to_string(2, 6), None);
        assert_eq!(b.byte_slice_to_string(0, b.len_bytes() + 1), None);
    }

    #[test]
    fn new_buffer_is_empty() {
        let b = Buffer::new();
        assert_eq!(b.len_chars(), 0);
        assert_eq!(b.line_count(), 1);
    }

    #[test]
    fn would_overwrite_only_for_a_different_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let existing = dir.path().join("there.md");
        std::fs::write(&existing, "x").expect("seed");
        let missing = dir.path().join("absent.md");

        let mut b = buf("body");
        assert!(b.would_overwrite(&existing));
        assert!(!b.would_overwrite(&missing));

        b.path = Some(existing.clone());
        assert!(!b.would_overwrite(&existing));
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
        b.remove_char(2);
        assert_eq!(b.contents(), "helo");
    }

    #[test]
    fn remove_char_out_of_bounds_is_noop() {
        let mut b = buf("hi");
        b.remove_char(100);
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
        assert_eq!(b.line_to_char(1), 4);
        assert_eq!(b.line_to_char(2), 8);
        assert_eq!(b.char_to_line(5), 1);
    }

    #[test]
    fn save_copy_writes_to_path_but_does_not_change_buffer_path() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let original = dir.path().join("orig.md");
        std::fs::write(&original, "# Hello")?;
        let buf = Buffer::load_file(&original)?;

        let copy = dir.path().join("copy.md");
        buf.save_copy(&copy)?;

        assert_eq!(std::fs::read_to_string(&copy)?, "# Hello");
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
        assert_eq!(LineEnding::detect("a\nb\r\n"), LineEnding::Lf);
        assert_eq!(LineEnding::detect("\r\nx"), LineEnding::Crlf);
        assert_eq!(LineEnding::detect("no newline"), LineEnding::default());
    }

    #[test]
    fn normalize_newlines_strips_only_crlf_pairs() {
        assert_eq!(normalize_newlines("a\r\nb\r\n".to_owned()), "a\nb\n");
        assert_eq!(normalize_newlines("a\nb\n".to_owned()), "a\nb\n");
        assert_eq!(normalize_newlines("a\rb".to_owned()), "a\rb");
    }

    #[test]
    fn encode_newlines_widens_only_for_crlf() {
        assert_eq!(encode_newlines("a\nb\n", LineEnding::Lf), "a\nb\n");
        assert_eq!(encode_newlines("a\nb\n", LineEnding::Crlf), "a\r\nb\r\n");
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
        buf.save_file()?;
        assert_eq!(std::fs::read(&path)?, original);
        Ok(())
    }

    #[test]
    fn edits_to_a_crlf_buffer_still_save_as_crlf() -> Result<()> {
        // An inserted bare `\n` is translated on save too: no mixed endings on disk.
        let dir = tempfile::tempdir()?;
        let src = dir.path().join("crlf.md");
        std::fs::write(&src, "a\r\nb\r\n")?;
        let mut buf = Buffer::load_file(&src)?;

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
        let reloaded = Buffer::reload(&path, "p\r\nq\r\n", 7, LineEnding::Crlf);
        assert_eq!(reloaded.contents(), "p\nq\n");
        assert_eq!(reloaded.line_ending(), LineEnding::Crlf);
        assert_eq!(reloaded.version(), 8);
    }

    // ── Save backup ───────────────────────────────────────────────

    /// The names in `dir`, sorted.
    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn injected() -> io::Error {
        io::Error::other("injected write failure")
    }

    #[test]
    fn saving_over_a_file_leaves_no_backup_behind() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "old")?;
        let mut buf = Buffer::load_file(&path)?;
        buf.insert(0, "new ");
        buf.save_file()?;
        assert_eq!(std::fs::read_to_string(&path)?, "new old");
        assert_eq!(entries(dir.path()), ["doc.md"]);
        Ok(())
    }

    #[test]
    fn a_write_failing_midway_restores_the_original_byte_for_byte() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("doc.md");
        let original = b"# Title\r\n\r\nbody \xc3\xa9\r\n";
        std::fs::write(&path, original)?;

        let err = write_with_backup(&path, |w| {
            w.write_all(b"partial")?;
            w.flush()?;
            Err(injected())
        })
        .expect_err("injected failure");

        assert!(format!("{err:#}").contains("injected write failure"));
        assert_eq!(std::fs::read(&path)?, original);
        assert_eq!(entries(dir.path()), ["doc.md"]);
        Ok(())
    }

    #[test]
    fn a_failed_write_to_a_new_file_has_nothing_to_restore() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("new.md");
        write_with_backup(&path, |_| Err(injected())).expect_err("injected failure");
        assert_eq!(entries(dir.path()), ["new.md"]);
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn a_failed_restore_keeps_the_backup_and_names_it_in_the_error() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "precious")?;

        // Replace the target with a directory mid-write, so reopening it to restore fails.
        let err = write_with_backup(&path, |_| {
            std::fs::remove_file(&path)?;
            std::fs::create_dir(&path)?;
            Err(injected())
        })
        .expect_err("injected failure");

        let backups: Vec<String> = entries(dir.path())
            .into_iter()
            .filter(|n| n.starts_with("doc.md.") && n.ends_with(BACKUP_SUFFIX))
            .collect();
        assert_eq!(backups.len(), 1, "{backups:?}");
        let backup = dir.path().join(&backups[0]);
        assert_eq!(std::fs::read_to_string(&backup)?, "precious");
        // Callers show only the outermost message, so the path must be in it.
        assert!(
            err.to_string().contains(&backup.display().to_string()),
            "{err}"
        );
        Ok(())
    }

    #[test]
    fn only_running_out_of_space_aborts_a_save_at_the_backup_step() {
        assert!(backup_failure_aborts_save(io::ErrorKind::StorageFull));
        assert!(backup_failure_aborts_save(io::ErrorKind::QuotaExceeded));
        assert!(!backup_failure_aborts_save(io::ErrorKind::PermissionDenied));
        assert!(!backup_failure_aborts_save(
            io::ErrorKind::ReadOnlyFilesystem
        ));
    }

    #[test]
    fn hard_links_stay_shared_after_a_save() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("doc.md");
        let other = dir.path().join("link.md");
        std::fs::write(&path, "old")?;
        std::fs::hard_link(&path, &other)?;

        let mut buf = Buffer::load_file(&path)?;
        buf.insert(0, "new ");
        buf.save_file()?;
        assert_eq!(std::fs::read_to_string(&other)?, "new old");
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn saving_through_a_symlink_keeps_the_link() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let target = dir.path().join("real.md");
        let link = dir.path().join("link.md");
        std::fs::write(&target, "old")?;
        std::os::unix::fs::symlink(&target, &link)?;

        let mut buf = Buffer::load_file(&link)?;
        buf.insert(0, "new ");
        buf.save_file()?;
        assert!(std::fs::symlink_metadata(&link)?.file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&target)?, "new old");
        assert_eq!(entries(dir.path()), ["link.md", "real.md"]);
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn a_save_keeps_the_files_permissions_and_a_new_file_gets_the_default() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir()?;
        let mode = |p: &Path| std::fs::metadata(p).map(|m| m.permissions().mode() & 0o777);

        let existing = dir.path().join("doc.md");
        std::fs::write(&existing, "old")?;
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o640))?;
        Buffer::load_file(&existing)?.save_file()?;
        assert_eq!(mode(&existing)?, 0o640);

        let reference = dir.path().join("reference");
        File::create(&reference)?;
        let fresh = dir.path().join("new.md");
        Buffer::from_str("x").save_copy(&fresh)?;
        assert_eq!(mode(&fresh)?, mode(&reference)?);
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn a_read_only_directory_still_saves_without_a_backup() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir()?;
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "old")?;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555))?;

        let mut buf = Buffer::load_file(&path)?;
        buf.insert(0, "new ");
        let saved = buf.save_file();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))?;
        saved?;
        assert_eq!(std::fs::read_to_string(&path)?, "new old");
        assert_eq!(entries(dir.path()), ["doc.md"]);
        Ok(())
    }
}
