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
    TempFile(std::io::Error),
    #[error("render failed: {0}")]
    Render(String),
    /// The command could not be *started* — a missing executable, or a
    /// bad working directory.  Kept distinct from [`Self::NonZeroExit`]
    /// (the command ran and failed) because the two send the user to
    /// completely different places: "install weasyprint / fix the path"
    /// vs. "read the converter's own error".  Previously an
    /// `#[from] io::Error` folded this into `TempFile`, so a failed spawn
    /// was reported as "failed to create temporary HTML file".
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
///
/// The caller is `crate::app::modal::export`, which reaches here for an
/// [`ExportJob::Custom`](crate::app::modal::export::ExportJob) exactly
/// where it would otherwise call [`crate::export::spawn_html_export`] —
/// same options, same `ExportDone` completion event.
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

    // The paths handed to the converter are made **absolute** first.  We
    // run the command with its working directory set to the document's
    // folder (so a relative `src="images/logo.png"` *inside* the HTML
    // resolves the way it did on screen), which means a relative `{html}`
    // / `{out}` would be resolved against *that* directory rather than the
    // process cwd — so a document opened by a relative path (`edamame
    // docs/guide.md`) reaches weasyprint as `docs/guide.pdf`, is opened
    // against `…/docs`, and dies with `…/docs/docs/guide.pdf: No such file
    // or directory`.  Absolute paths are cwd-independent and sidestep it
    // entirely; every later step below uses the absolute forms too, so the
    // mtime probe and the output all agree on one location.
    let abs_target = absolutize(target);

    // The temp HTML is written *into the output directory*, not the
    // system temp dir.  Converters (weasyprint, pandoc, …) resolve a
    // relative `src="images/logo.png"` against the input file's own
    // location, so an intermediate under `/tmp` would look for
    // `/tmp/images/…` and silently drop every non-inlined image — the
    // common case, since `inline_images` is off by default.  Placing it
    // beside the document (which is where `abs_target` sits) makes those
    // paths resolve exactly as they did on screen.  It is absolute because
    // `abs_target` is, so `tmp_path` — and thus `{html}` — is absolute too.
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

    // Stamp the target *before* running the converter.  Step 3 uses this
    // to tell "the command wrote the file" apart from "the command left a
    // stale file from a previous export untouched" — on the overwrite path
    // the file is present going in, so `exists()` alone would report a
    // no-op converter as a success and open the old artifact.
    let target_before = target_stamp(&abs_target);

    // 2. Run the command in the source document's directory (the output
    //    directory), so relative references inside the HTML resolve the
    //    same way they did for the preview.  The `{html}` / `{out}`
    //    arguments are absolute, so this cwd affects only the HTML's own
    //    relative asset URLs, never where the output lands.
    let argv = substitute_command(&entry.command, &tmp_path, &abs_target);
    let (program, args) = argv.split_first().expect("non-empty checked above");

    // The document's own directory, which is exactly `out_dir` (the target
    // sits beside the source, so `abs_target`'s parent *is* the source's).
    // Deriving it here rather than from `html_opts.source_dir` avoids a
    // trap: for a repo-root file opened by a bare relative path
    // (`edamame README.md`), the modal's `target.parent()` is an *empty*
    // path, and `current_dir("")` fails the spawn with `NotFound` — the
    // bug that surfaced (misleadingly) as "failed to create temporary HTML
    // file".  `out_dir` is always an absolute, existing directory.
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

    // 3. Some tools write directly to `{out}`; others write to stdout
    //    and we're expected to capture it.  "The converter produced a
    //    file" means the target exists *and* was written during this run
    //    — a fresh file, or an existing one whose mtime advanced.  If it
    //    didn't, fall back to stdout; if that's empty too, the converter
    //    produced nothing and we must not report the stale file as ours.
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

/// Modification time *and* length of `p`, or `None` if it does not exist.
///
/// The pair is the "did this run write the file?" signal: a converter that
/// writes `{out}` advances one or both.  Length is not redundant with the
/// timestamp — mtime granularity is a property of the filesystem, and on a
/// 1-second one (exFAT/FAT32 on removable media, HFS+) a re-export landing
/// in the same tick as the pre-run stamp reads as unchanged.  That matters
/// because the caller answers "unchanged" by falling back to the captured
/// stdout, so a converter that both writes `{out}` and logs to stdout
/// would have its real output overwritten by its own log text.  Comparing
/// the length as well narrows that to a same-tick rewrite that is also
/// byte-identical in *size* — the residual, and the reason this is a pair
/// rather than a timestamp.  Hashing the file would close it completely
/// and is not worth reading a multi-megabyte PDF twice per export.
fn target_stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Resolve `p` to an absolute path without requiring it to exist.
///
/// The converter runs with its working directory set to the document's
/// folder, so any relative `{html}` / `{out}` argument would be resolved
/// against *that* directory rather than the process cwd — turning a
/// document opened as `edamame docs/guide.md` into a doomed
/// `…/docs/docs/guide.pdf` write.  Absolute paths are cwd-independent.
/// `std::path::absolute` normalizes lexically and needs no I/O (unlike
/// `canonicalize`), so it works for a target that does not exist yet.  It
/// rejects an *empty* path, though — which is exactly what `target.parent()`
/// yields for a bare filename — so an empty input falls back to the current
/// directory (its true meaning), and only a genuinely unavailable cwd
/// leaves `p` unchanged.
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
    use std::sync::mpsc;
    use tempfile::tempdir;

    fn html_opts(dir: &Path) -> HtmlExportOptions {
        HtmlExportOptions {
            stylesheet: crate::export::Stylesheet::Inline(String::new()),
            inline_images: false,
            source_dir: Some(dir.to_path_buf()),
            title: None,
            render_diagrams: false,
        }
    }

    /// The exact regression from the weasyprint `FileNotFoundError`: a
    /// relative target must be resolved to an absolute path before it is
    /// handed to the converter, since the converter's cwd is the document's
    /// folder, not the process cwd.
    #[test]
    fn absolutize_makes_a_relative_path_absolute() {
        let abs = absolutize(Path::new("docs/guide.pdf"));
        assert!(
            abs.is_absolute(),
            "a relative target must absolutize: {abs:?}"
        );
        assert!(abs.ends_with("docs/guide.pdf"), "tail preserved: {abs:?}");
        // An already-absolute path passes through unchanged.
        assert_eq!(
            absolutize(Path::new("/tmp/x.pdf")),
            PathBuf::from("/tmp/x.pdf")
        );
    }

    /// `absolutize("")` is the cwd, not the empty path.  `target.parent()`
    /// is empty for a repo-root file (`edamame README.md`), and an empty
    /// working directory fails the converter spawn with `NotFound`.
    #[test]
    fn absolutize_empty_path_is_the_cwd() {
        let a = absolutize(Path::new(""));
        assert!(
            a.is_absolute(),
            "empty must resolve to an absolute cwd: {a:?}"
        );
        assert_eq!(a, std::env::current_dir().unwrap());
    }

    /// The exact README bug: an *empty* `source_dir` (what the modal used
    /// to derive from a root-level file's `target.parent()`) must not break
    /// the run.  The working directory now comes from the target's own
    /// (absolute) parent, so the converter spawns and writes normally.  The
    /// absolute target keeps the test out of the repo's own tree.
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
            source_dir: Some(PathBuf::new()), // the empty path, as before the fix
            ..html_opts(dir.path())
        };
        let (tx, rx) = mpsc::channel();
        spawn_custom_export(entry, "# hi\n".into(), target.clone(), opts, move |o| {
            tx.send(o).unwrap()
        });
        assert_eq!(rx.recv().unwrap().unwrap(), target);
        assert!(target.exists());
    }

    /// A command that cannot be started reports a *spawn* failure naming
    /// the program, not the misleading "failed to create temporary HTML
    /// file" the old `#[from] io::Error` produced for every `?` in the
    /// function.
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

    /// The converter's working directory (the document folder, for the sake
    /// of the HTML's own relative asset URLs) is *not* where the output
    /// lands: `{out}` is absolute, so a converter run with a cwd different
    /// from the target's directory still writes to the target.  This is the
    /// property whose absence produced `…/docs/docs/guide.pdf`.
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
        // The converter runs in `work_dir`, a sibling of the target's dir.
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
        // Nothing leaked into the converter's working directory.
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

    /// A converter that writes to stdout instead of `{out}` is captured.
    /// `cat {html}` prints the rendered HTML; with no `{out}` the file
    /// never gets written by the command, so the stdout fallback is what
    /// produces the output.
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

    /// A no-op converter (exits 0, writes nothing) over a target left by a
    /// *previous* export must not report success: the file is present, but
    /// this run did not produce it.  Before the mtime check, the
    /// `!target.exists()` guard skipped straight to `Ok`, opening the
    /// stale artifact as if it were fresh.
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
        // The stale file is left exactly as it was.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old export");
    }

    /// The write check is `(mtime, len)`, not mtime alone: on a
    /// coarse-granularity filesystem a converter can rewrite the target
    /// inside the same mtime tick as the pre-run stamp, and the caller
    /// reads "unchanged" as "fall back to stdout" — which would overwrite
    /// the converter's real output with its log text.  Pinning the mtime
    /// simulates that tick; the differing length is what still reports the
    /// write.
    #[test]
    fn the_write_check_notices_a_length_change_under_a_pinned_mtime() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("out.bin");
        std::fs::write(&target, b"old").unwrap();
        let before = target_stamp(&target).expect("the file exists");

        // Rewrite with different content, then force the original mtime
        // back so the timestamp half of the stamp cannot see the write.
        std::fs::write(&target, b"a longer replacement").unwrap();
        let f = std::fs::File::options().write(true).open(&target).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(before.0))
            .unwrap();
        drop(f);

        let after = target_stamp(&target).expect("still there");
        assert_eq!(after.0, before.0, "mtime is pinned, as on a coarse fs");
        assert_ne!(after, before, "the length change still reports the write");
    }

    /// The intermediate HTML is written into the *output* directory, not
    /// the system temp dir, so a converter reading it resolves relative
    /// image paths against the document's folder.  The converter here
    /// records the `{html}` path it was handed; it must live beside the
    /// target.
    #[test]
    #[cfg(unix)]
    fn intermediate_html_lives_beside_the_target() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("out.txt");
        let entry = CustomExportEntry {
            name: "record".into(),
            // Write the html path we were given into {out}.
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
