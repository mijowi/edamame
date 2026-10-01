use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Result of a background export job; an owned `String` error so it is trivially `Send`.
pub type ExportOutcome = Result<Exported, String>;

/// A finished export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    /// The file written.
    pub path: PathBuf,
    /// Images whose `src` the export removed rather than leave a reference behind
    /// ([`super::ImageHandling::Sealed`]), so the user learns the output is missing them.
    pub images_left_out: usize,
}

/// Reasons [`preflight`] may refuse to start an export.
#[derive(Debug, Error)]
pub enum PreflightError {
    /// The caller should confirm with the user and re-invoke with `overwrite = true`.
    #[error("output file already exists: {0}")]
    TargetExists(PathBuf),
    /// Library surface only: the binary guards on a saved `file_path` before exporting.
    #[allow(dead_code)]
    #[error("source document has no path; cannot derive an export target")]
    NoSourcePath,
}

/// Default export target next to the source: `notes/guide.md` + `"html"` (no leading dot)
/// yields `notes/guide.html`.
pub fn target_for_source(source: &Path, extension: &str) -> PathBuf {
    source.with_extension(extension)
}

/// Decide whether an export may proceed to `target`. The existence check is advisory (a
/// concurrent writer can race it), but the atomic write below bounds the damage.
pub fn preflight(target: &Path, overwrite: bool) -> Result<(), PreflightError> {
    if target.exists() && !overwrite {
        Err(PreflightError::TargetExists(target.to_path_buf()))
    } else {
        Ok(())
    }
}

/// Write `bytes` to `path` via a same-directory temp file and rename, so an interrupted
/// write never leaves a truncated export. The temp name is random and `O_EXCL`
/// (`NamedTempFile`), not a predictable sibling a symlink could be pre-planted under.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn target_for_source_swaps_extension() {
        let p = Path::new("notes/guide.md");
        assert_eq!(
            target_for_source(p, "html"),
            PathBuf::from("notes/guide.html")
        );
        assert_eq!(
            target_for_source(p, "pdf"),
            PathBuf::from("notes/guide.pdf")
        );
    }

    #[test]
    fn preflight_allows_new_target() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("fresh.html");
        assert!(preflight(&target, false).is_ok());
    }

    #[test]
    fn preflight_refuses_existing_target_without_overwrite() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("existing.html");
        std::fs::write(&target, b"old").unwrap();
        match preflight(&target, false) {
            Err(PreflightError::TargetExists(p)) => assert_eq!(p, target),
            other => panic!("expected TargetExists, got {other:?}"),
        }
    }

    #[test]
    fn preflight_allows_existing_target_with_overwrite() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("existing.html");
        std::fs::write(&target, b"old").unwrap();
        assert!(preflight(&target, true).is_ok());
    }

    #[test]
    fn write_atomically_creates_and_overwrites() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("out.html");
        write_atomically(&target, b"v1").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v1");
        write_atomically(&target, b"v2").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v2");
    }

    #[test]
    fn write_atomically_leaves_no_tmp_behind() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("out.html");
        write_atomically(&target, b"v1").unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], "out.html");
    }
}
