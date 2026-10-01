use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use thiserror::Error;

use super::html::{render_html, HtmlExportOptions};
use super::runner::{write_atomically, ExportOutcome};
use crate::config::CustomExportEntry;

/// Errors from a custom-export run, flattened to a `String` when they cross the worker
/// boundary via [`ExportOutcome`].
#[derive(Debug, Error)]
pub enum CustomExportError {
    #[error("failed to create temporary HTML file: {0}")]
    TempFile(std::io::Error),
    #[error("render failed: {0}")]
    Render(String),
    /// The command could not be *started*.  Distinct from [`Self::NonZeroExit`] because
    /// the two send the user to different places ("install weasyprint" vs. "read the
    /// converter's error"); an `#[from] io::Error` once folded this into `TempFile`.
    #[error("failed to run export command '{program}': {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("export command exited with status {status}: {stderr}")]
    NonZeroExit { status: i32, stderr: String },
    #[error("export command terminated by signal before completing")]
    Signalled,
    #[error("failed to write export output to {path}: {source}")]
    WriteOutput {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("export command produced no output at {0}")]
    NoOutput(PathBuf),
    #[error("export command is empty; `command` must have at least one element")]
    EmptyCommand,
}

/// Render `markdown` to HTML on a worker thread, write it to a temp file, and run the
/// user's `entry.command` with `{html}` / `{out}` substituted into *every* argument.
///
/// The caller must have run [`crate::export::preflight`] on `target`; this clobbers an
/// existing file.  The temp HTML is deleted on return either way.  Slots in exactly where
/// [`crate::export::spawn_html_export`] would — same options, same `ExportDone` event.
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

    // `NamedTempFile` deletes on drop, so a failing converter leaves no stray files.
    let html_string = render_html(markdown, html_opts)
        .map_err(|e| CustomExportError::Render(format!("{e:#}")))?;

    // Absolute, because the command's cwd is the document's folder: a relative `{out}`
    // from `edamame docs/guide.md` would resolve to `…/docs/docs/guide.pdf`.  Every later
    // step uses the absolute forms so the mtime probe and the output agree on one location.
    let abs_target = absolutize(target);

    // The temp HTML goes in the *output* directory, not the system temp dir: converters
    // resolve a relative `src="images/logo.png"` against the input file's own location, so
    // an intermediate under `/tmp` silently drops every non-inlined image (the common
    // case — `inline_images` is off by default).
    let out_dir = abs_target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or_else(|| html_opts.source_dir.as_deref().map(absolutize))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let mut tmp = tempfile::Builder::new()
        .prefix(".edamame-export-")
        .suffix(".html")
        .tempfile_in(&out_dir)
        .map_err(CustomExportError::TempFile)?;
    tmp.write_all(html_string.as_bytes())
        .map_err(CustomExportError::TempFile)?;
    tmp.flush().map_err(CustomExportError::TempFile)?;
    let tmp_path = tmp.path().to_path_buf();

    // Stamp before running, to tell "the command wrote the file" from "a stale file from
    // a previous export was left untouched": on the overwrite path `exists()` alone would
    // report a no-op converter as success and open the old artifact.
    let target_before = target_stamp(&abs_target);

    let argv = substitute_command(&entry.command, &tmp_path, &abs_target);
    let (program, args) = argv.split_first().expect("non-empty checked above");

    // The cwd is the document's folder, so the HTML's own relative asset URLs resolve as
    // they did on screen; `{html}` / `{out}` are absolute, so it never moves the output.
    // Derived from `out_dir` rather than `html_opts.source_dir`: for a repo-root file the
    // modal's `target.parent()` is the *empty* path, and `current_dir("")` fails the spawn
    // with `NotFound`.  `out_dir` is always absolute and exists.
    let working_dir = out_dir.clone();

    let output = Command::new(program)
        .args(args)
        .current_dir(&working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|source| CustomExportError::Spawn {
            program: program.clone(),
            source,
        })?;

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

    // Some tools write `{out}`; others write to stdout.  "Produced a file" means the
    // target exists *and* changed during this run — otherwise fall back to stdout, and if
    // that is empty too, refuse rather than pass off a stale file as ours.
    let wrote_target = match target_stamp(&abs_target) {
        Some(after) => target_before != Some(after),
        None => false,
    };
    if !wrote_target {
        if output.stdout.is_empty() {
            return Err(CustomExportError::NoOutput(abs_target.clone()));
        }
        write_atomically(&abs_target, &output.stdout).map_err(|source| {
            CustomExportError::WriteOutput {
                path: abs_target.clone(),
                source,
            }
        })?;
    }

    Ok(abs_target)
}

/// Modification time *and* length of `p` — the "did this run write the file?" signal.
///
/// The length is not redundant: on a 1-second-granularity filesystem (exFAT, HFS+) a
/// re-export inside the pre-run stamp's tick reads as unchanged, and "unchanged" makes the
/// caller fall back to stdout — overwriting a converter's real output with its own log
/// text.  The pair narrows that to a same-tick rewrite of identical size.  Hashing would
/// close it entirely and is not worth reading a multi-megabyte PDF twice per export.
fn target_stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Resolve `p` to an absolute path without requiring it to exist (see `run_custom_export`
/// for why absolute).  `std::path::absolute` normalizes lexically with no I/O, unlike
/// `canonicalize`, so a not-yet-created target works — but it rejects the *empty* path,
/// which is what `target.parent()` yields for a bare filename, so that falls back to the
/// cwd (its true meaning).
fn absolutize(p: &Path) -> PathBuf {
    std::path::absolute(p)
        .or_else(|_| std::env::current_dir())
        .unwrap_or_else(|_| p.to_path_buf())
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
    // Used only by the `cfg(unix)` spawn tests; unused on Windows, which
    // `clippy -D warnings` against the msvc target rejects.
    #[cfg(unix)]
    use std::sync::mpsc;
    use tempfile::tempdir;

    fn html_opts(dir: &Path) -> HtmlExportOptions {
        HtmlExportOptions {
            stylesheet: crate::export::Stylesheet::Inline(String::new()),
            inline_images: false,
            source_dir: Some(dir.to_path_buf()),
            approved_outside: Vec::new(),
            title: None,
            render_figures: false,
        }
    }

    /// Regression: the weasyprint `FileNotFoundError` from a relative target.
    #[test]
    fn absolutize_makes_a_relative_path_absolute() {
        let abs = absolutize(Path::new("docs/guide.pdf"));
        assert!(
            abs.is_absolute(),
            "a relative target must absolutize: {abs:?}"
        );
        assert!(abs.ends_with("docs/guide.pdf"), "tail preserved: {abs:?}");
        // A real directory, not `/tmp/x.pdf`: a `/`-rooted literal is not absolute on
        // Windows.
        let dir = tempdir().unwrap();
        let already = dir.path().join("x.pdf");
        assert_eq!(absolutize(&already), already);
    }

    /// `target.parent()` is empty for a repo-root file, and an empty working directory
    /// fails the converter spawn with `NotFound`.
    #[test]
    fn absolutize_empty_path_is_the_cwd() {
        let a = absolutize(Path::new(""));
        assert!(
            a.is_absolute(),
            "empty must resolve to an absolute cwd: {a:?}"
        );
        assert_eq!(a, std::env::current_dir().unwrap());
    }

    /// Regression: an *empty* `source_dir`, as the modal derived for a root-level file,
    /// must not break the run.
    #[test]
    #[cfg(unix)]
    fn an_empty_source_dir_does_not_break_the_export() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("out.copy");
        let entry = CustomExportEntry {
            name: "copy".into(),
            command: vec!["cp".into(), "{html}".into(), "{out}".into()],
            extension: "copy".into(),
        };
        let opts = HtmlExportOptions {
            source_dir: Some(PathBuf::new()),
            ..html_opts(dir.path())
        };
        let (tx, rx) = mpsc::channel();
        spawn_custom_export(entry, "# hi\n".into(), target.clone(), opts, move |o| {
            tx.send(o).unwrap()
        });
        assert_eq!(rx.recv().unwrap().unwrap(), target);
        assert!(target.exists());
    }

    /// A spawn failure must name the program, not report "failed to create temporary HTML
    /// file" as the old blanket `#[from] io::Error` did.
    #[test]
    fn a_missing_executable_reports_a_spawn_failure() {
        let dir = tempdir().unwrap();
        let entry = CustomExportEntry {
            name: "missing".into(),
            command: vec!["edamame-no-such-converter-xyzzy".into(), "{out}".into()],
            extension: "out".into(),
        };
        let err = run_custom_export(
            &entry,
            "# hi\n",
            &dir.path().join("x.out"),
            &html_opts(dir.path()),
        )
        .unwrap_err();
        assert!(
            matches!(err, CustomExportError::Spawn { .. }),
            "expected a Spawn error, got {err:?}"
        );
        let text = format!("{err:#}");
        assert!(text.contains("failed to run export command"), "{text}");
        assert!(
            !text.contains("temporary HTML file"),
            "spawn failure must not be mislabeled as a tempfile error: {text}"
        );
    }

    /// The converter's cwd is not where the output lands — the property whose absence
    /// produced `…/docs/docs/guide.pdf`.
    #[test]
    #[cfg(unix)]
    fn output_lands_at_the_target_even_when_the_cwd_differs() {
        let dir = tempdir().unwrap();
        let out_dir = dir.path().join("out");
        let work_dir = dir.path().join("work");
        std::fs::create_dir(&out_dir).unwrap();
        std::fs::create_dir(&work_dir).unwrap();
        let target = out_dir.join("guide.copy");

        let entry = CustomExportEntry {
            name: "copy".into(),
            command: vec!["cp".into(), "{html}".into(), "{out}".into()],
            extension: "copy".into(),
        };
        let opts = HtmlExportOptions {
            source_dir: Some(work_dir.clone()),
            ..html_opts(dir.path())
        };
        let (tx, rx) = mpsc::channel();
        spawn_custom_export(entry, "# hi\n".into(), target.clone(), opts, move |o| {
            tx.send(o).unwrap()
        });
        let produced = rx.recv().unwrap().unwrap();
        assert_eq!(produced, target, "returned path is the resolved target");
        assert!(target.exists(), "output written at the target, not the cwd");
        assert!(!work_dir.join("guide.copy").exists());
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

    /// `cp` stands in for a format converter, so CI needs no pandoc / weasyprint.
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

    /// `cat {html}` never writes `{out}`, so only the stdout fallback can produce output.
    #[test]
    #[cfg(unix)]
    fn spawn_custom_export_captures_stdout_when_no_file_is_written() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("out.txt");
        let entry = CustomExportEntry {
            name: "stdout".into(),
            command: vec!["cat".into(), "{html}".into()],
            extension: "txt".into(),
        };
        let (tx, rx) = mpsc::channel();
        spawn_custom_export(
            entry,
            "# hello\n".into(),
            target.clone(),
            html_opts(dir.path()),
            move |outcome| tx.send(outcome).unwrap(),
        );
        assert_eq!(rx.recv().unwrap().unwrap(), target);
        let body = std::fs::read_to_string(&target).unwrap();
        assert!(body.contains("<h1>hello</h1>"), "stdout was captured");
    }

    /// A no-op converter over a target left by a *previous* export must not report
    /// success.  The old `!target.exists()` guard opened the stale artifact as fresh.
    #[test]
    #[cfg(unix)]
    fn spawn_custom_export_rejects_a_no_op_converter_over_a_stale_target() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("stale.out");
        std::fs::write(&target, b"old export").unwrap();

        let entry = CustomExportEntry {
            name: "noop".into(),
            // `true` exits 0 without touching the filesystem or stdout.
            command: vec!["true".into()],
            extension: "out".into(),
        };
        let (tx, rx) = mpsc::channel();
        spawn_custom_export(
            entry,
            "# hi\n".into(),
            target.clone(),
            html_opts(dir.path()),
            move |outcome| tx.send(outcome).unwrap(),
        );
        let err = rx.recv().unwrap().unwrap_err();
        assert!(
            err.contains("no output"),
            "expected a no-output error, got: {err}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old export");
    }

    /// Pinning the mtime simulates a coarse-granularity filesystem's same-tick rewrite;
    /// the length half of the stamp is what still reports the write.  See [`target_stamp`].
    #[test]
    fn the_write_check_notices_a_length_change_under_a_pinned_mtime() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("out.bin");
        std::fs::write(&target, b"old").unwrap();
        let before = target_stamp(&target).expect("the file exists");

        // Force the original mtime back so the timestamp half cannot see the write.
        std::fs::write(&target, b"a longer replacement").unwrap();
        let f = std::fs::File::options().write(true).open(&target).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(before.0))
            .unwrap();
        drop(f);

        let after = target_stamp(&target).expect("still there");
        assert_eq!(after.0, before.0, "mtime is pinned, as on a coarse fs");
        assert_ne!(after, before, "the length change still reports the write");
    }

    /// The converter records the `{html}` path it was handed; it must sit beside the
    /// target, so relative image paths resolve against the document's folder.
    #[test]
    #[cfg(unix)]
    fn intermediate_html_lives_beside_the_target() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("out.txt");
        let entry = CustomExportEntry {
            name: "record".into(),
            command: vec![
                "sh".into(),
                "-c".into(),
                "printf '%s' \"$1\" > \"$2\"".into(),
                "sh".into(),
                "{html}".into(),
                "{out}".into(),
            ],
            extension: "txt".into(),
        };
        let (tx, rx) = mpsc::channel();
        spawn_custom_export(
            entry,
            "# hi\n".into(),
            target.clone(),
            html_opts(dir.path()),
            move |outcome| tx.send(outcome).unwrap(),
        );
        assert_eq!(rx.recv().unwrap().unwrap(), target);
        let html_path = std::fs::read_to_string(&target).unwrap();
        assert_eq!(
            Path::new(&html_path).parent(),
            target.parent(),
            "the intermediate HTML must sit in the target's directory, got {html_path}"
        );
    }
}
