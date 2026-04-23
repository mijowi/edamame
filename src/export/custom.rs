use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use thiserror::Error;

use super::html::{render_html, HtmlExportOptions};
use super::runner::{write_atomically, ExportOutcome};
use crate::config::CustomExportEntry;

/// Errors that bubble out of a custom-export run.  Wrapped in a plain
/// `String` when sent across the worker-thread boundary via
/// [`ExportOutcome`].
#[derive(Debug, Error)]
pub enum CustomExportError {
    #[error("failed to create temporary HTML file: {0}")]
    TempFile(#[from] std::io::Error),
    #[error("render failed: {0}")]
    Render(String),
    #[error("export command exited with status {status}: {stderr}")]
    NonZeroExit { status: i32, stderr: String },
    #[error("export command terminated by signal before completing")]
    Signalled,
    #[error("export command produced no output at {0}")]
    NoOutput(PathBuf),
    #[error("export command is empty; `command` must have at least one element")]
    EmptyCommand,
}

/// Spawn a worker thread that:
///   1. Renders `markdown` to HTML (same pipeline as the built-in HTML export).
///   2. Writes the HTML to a temp file.
///   3. Runs the user's `entry.command` with `{html}` / `{out}` substitution.
///   4. Renames the resulting output into place.
///
/// Substitution is applied to *every* string in `command`, so a user can
/// write `["pandoc", "{html}", "-o", "{out}"]` or
/// `["sh", "-c", "weasyprint {html} {out}"]` — whatever their tool needs.
///
/// The caller is expected to have run [`crate::export::preflight`] on
/// `target` first; this function clobbers an existing file if one is
/// present.  The temp HTML file is dropped (and deleted) when the
/// function returns, whether the command succeeded or not.
pub fn spawn_custom_export(
    entry: CustomExportEntry,
    markdown: String,
    target: PathBuf,
    html_opts: HtmlExportOptions,
    on_done: impl FnOnce(ExportOutcome) + Send + 'static,
) {
    std::thread::spawn(move || {
        let result = run_custom_export(&entry, &markdown, &target, &html_opts);
        on_done(result.map_err(|e| format!("{e:#}")));
    });
}

fn run_custom_export(
    entry: &CustomExportEntry,
    markdown: &str,
    target: &Path,
    html_opts: &HtmlExportOptions,
) -> Result<PathBuf, CustomExportError> {
    if entry.command.is_empty() {
        return Err(CustomExportError::EmptyCommand);
    }

    // 1. Render the intermediate HTML to a tempfile the external tool
    //    can read.  `NamedTempFile` deletes on drop, so a failing
    //    converter never leaves stray files behind.
    let html_string = render_html(markdown, html_opts)
        .map_err(|e| CustomExportError::Render(format!("{e:#}")))?;

    let mut tmp = tempfile::Builder::new()
        .prefix("edamame-export-")
        .suffix(".html")
        .tempfile()?;
    tmp.write_all(html_string.as_bytes())?;
    tmp.flush()?;
    let tmp_path = tmp.path().to_path_buf();

    // 2. Run the command in the source document's directory when we
    //    have one, so relative references inside the HTML resolve the
    //    same way they did for the preview.  Fall back to the temp
    //    file's parent otherwise.
    let argv = substitute_command(&entry.command, &tmp_path, target);
    let (program, args) = argv.split_first().expect("non-empty checked above");

    let working_dir = html_opts
        .source_dir
        .clone()
        .unwrap_or_else(|| tmp_path.parent().unwrap_or(Path::new(".")).to_path_buf());

    let output = Command::new(program)
        .args(args)
        .current_dir(&working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(match output.status.code() {
            Some(code) => CustomExportError::NonZeroExit {
                status: code,
                stderr,
            },
            None => CustomExportError::Signalled,
        });
    }

    // 3. Some tools write directly to `{out}`; others write to stdout
    //    and we're expected to capture it.  Cover both: if the
    //    command produced no file at `target` but did write to stdout,
    //    fall back to atomically writing stdout.
    if !target.exists() {
        if output.stdout.is_empty() {
            return Err(CustomExportError::NoOutput(target.to_path_buf()));
        }
        write_atomically(target, &output.stdout)?;
    }

    Ok(target.to_path_buf())
}

fn substitute_command(command: &[String], html_path: &Path, out_path: &Path) -> Vec<String> {
    let html_str = html_path.to_string_lossy();
    let out_str = out_path.to_string_lossy();
    command
        .iter()
        .map(|arg| arg.replace("{html}", &html_str).replace("{out}", &out_str))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tempfile::tempdir;

    fn html_opts(dir: &Path) -> HtmlExportOptions {
        HtmlExportOptions {
            stylesheet: crate::export::Stylesheet::Inline(String::new()),
            inline_images: false,
            source_dir: Some(dir.to_path_buf()),
            title: None,
        }
    }

    #[test]
    fn substitute_command_replaces_html_and_out_tokens() {
        let command: [String; 4] = [
            "pandoc".into(),
            "{html}".into(),
            "-o".into(),
            "{out}".into(),
        ];
        let argv = substitute_command(&command, Path::new("/tmp/a.html"), Path::new("/tmp/b.pdf"));
        assert_eq!(argv, vec!["pandoc", "/tmp/a.html", "-o", "/tmp/b.pdf"]);
    }

    #[test]
    fn substitute_command_tolerates_missing_tokens() {
        let command: [String; 2] = ["echo".into(), "hello".into()];
        let argv = substitute_command(&command, Path::new("/tmp/a.html"), Path::new("/tmp/b.pdf"));
        assert_eq!(argv, vec!["echo", "hello"]);
    }

    #[test]
    fn empty_command_is_rejected() {
        let dir = tempdir().unwrap();
        let entry = CustomExportEntry {
            name: "empty".into(),
            command: vec![],
            extension: "out".into(),
        };
        let err = run_custom_export(
            &entry,
            "hi",
            &dir.path().join("x.out"),
            &html_opts(dir.path()),
        )
        .unwrap_err();
        assert!(matches!(err, CustomExportError::EmptyCommand));
    }

    /// Use `cp` (POSIX) to copy the rendered HTML to the target path,
    /// which is a realistic stand-in for a format converter without
    /// needing pandoc / weasyprint installed in CI.
    #[test]
    #[cfg(unix)]
    fn spawn_custom_export_runs_cp_successfully() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("out.copy");
        let entry = CustomExportEntry {
            name: "copy".into(),
            command: vec!["cp".into(), "{html}".into(), "{out}".into()],
            extension: "copy".into(),
        };
        let (tx, rx) = mpsc::channel();
        spawn_custom_export(
            entry,
            "# hello\n".into(),
            target.clone(),
            html_opts(dir.path()),
            move |outcome| tx.send(outcome).unwrap(),
        );
        let outcome = rx.recv().unwrap();
        assert_eq!(outcome.unwrap(), target);
        let body = std::fs::read_to_string(&target).unwrap();
        assert!(body.contains("<h1>hello</h1>"));
    }

    #[test]
    #[cfg(unix)]
    fn spawn_custom_export_reports_non_zero_exit() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("never.out");
        let entry = CustomExportEntry {
            name: "fail".into(),
            // `false` exits non-zero without touching the filesystem.
            command: vec!["false".into()],
            extension: "out".into(),
        };
        let (tx, rx) = mpsc::channel();
        spawn_custom_export(
            entry,
            "".into(),
            target.clone(),
            html_opts(dir.path()),
            move |outcome| tx.send(outcome).unwrap(),
        );
        let outcome = rx.recv().unwrap();
        let err = outcome.unwrap_err();
        assert!(
            err.contains("status"),
            "expected non-zero-exit error text, got: {err}"
        );
        assert!(!target.exists());
    }
}
