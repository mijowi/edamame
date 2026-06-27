use std::path::{Path, PathBuf};

use thiserror::Error;

/// Result reported by any background export job.  Kept simple (owned
/// `String` for errors) so it crosses the `Send` boundary without having
/// to deal with `anyhow::Error`'s `!Sync` story.
pub type ExportOutcome = Result<PathBuf, String>;

/// Reasons [`preflight`] may refuse to start an export.
#[derive(Debug, Error)]
pub enum PreflightError {
    /// The output path already exists and the caller did not request an
    /// overwrite.  The caller is expected to surface a confirmation
    /// prompt and re-invoke with `overwrite = true` on approval.
    #[error("output file already exists: {0}")]
    TargetExists(PathBuf),
    /// The source has no associated path, so we cannot derive a default
    /// target filename next to it.  Only relevant when the caller relied
    /// on [`target_for_source`] — explicit targets never hit this path.
    /// Constructed only by callers that derive a target from a pathless
    /// source — library surface the binary doesn't reach (it guards on a
    /// saved `file_path` before exporting).
    #[allow(dead_code)]
    #[error("source document has no path; cannot derive an export target")]
    NoSourcePath,
}

/// Compute the default export target next to a source markdown file.
///
/// `source` is the `.md` path; `extension` is supplied without a leading
/// dot (`"html"`, `"pdf"`, …).  A source path of `notes/guide.md` with
/// `"html"` yields `notes/guide.html`.
///
/// `source.with_extension(...)` preserves the parent directory and
/// replaces the final extension, matching the "output next to
/// the source" behaviour.
pub fn target_for_source(source: &Path, extension: &str) -> PathBuf {
    source.with_extension(extension)
}

/// Decide whether an export may proceed to `target`.
///
/// Returns `Ok(())` when the file does not exist *or* `overwrite` is
/// true.  The file-system check is advisory — a concurrent writer can
/// still lose the race, but the exporter's atomic temp-file-and-rename
/// write means the worst case is "the user's confirmation modal was
/// based on slightly stale state".
pub fn preflight(target: &Path, overwrite: bool) -> Result<(), PreflightError> {
    if target.exists() && !overwrite {
        Err(PreflightError::TargetExists(target.to_path_buf()))
    } else {
        Ok(())
    }
}

/// Write `bytes` to `path` atomically: a sibling temp file is created
/// and renamed over `path` only after the write succeeds.  A partial or
/// interrupted write therefore never leaves a truncated export file at
/// the target path.
///
/// Used by every export backend, so failure modes are uniform.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "export".into());
    let tmp_name = format!(".{file_name}.edamame-export.tmp");
    let tmp_path = parent.join(tmp_name);

    std::fs::write(&tmp_path, bytes)?;
    std::fs::rename(&tmp_path, path)?;
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
