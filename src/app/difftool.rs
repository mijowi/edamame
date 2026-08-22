//! Helpers for the `--diff` difftool presentation: reading the two
//! sides, deciding whether to render them as Markdown, and naming the
//! session in the status bar.
//!
//! They live here rather than in `main.rs` for one reason: `main`
//! deliberately declares no modules (see its header comment), so a
//! function defined there is reachable by neither `cargo test --lib`
//! nor the integration tests.  [`diff_label`] in particular is four
//! branches of path handling — a rename, a delete, an add, and the
//! degenerate pair — and every one of them is a string a user reads.

use std::path::Path;

use anyhow::{Context, Result};

use super::nav::is_markdown_path;

/// Exit status used when a difftool session is ended by the user.
///
/// 128 + `SIGINT`, the conventional status for an interrupted program —
/// and on Unix what we actually exit with is that signal itself, so this
/// is only reached on a platform with no process groups to signal.
const EXIT_INTERRUPTED: i32 = 130;

/// Read one side of a `--diff` pair.
///
/// git passes `/dev/null` for the missing side of an add or a delete,
/// which reads as empty — exactly the right input, so no special case.
///
/// The error carries the path because both sides are usually temp files
/// the user never typed: "failed to read" without a name is unactionable
/// in the middle of a `git difftool` walk.  A non-UTF-8 file (git will
/// happily invoke a difftool on a binary) fails here rather than being
/// rendered as mojibake.
pub fn read_side(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {} for review", path.display()))
}

/// Whether a `--diff` pair is Markdown, and so whether there is a
/// review to open at all.
///
/// **Either side naming a Markdown file is enough.** git passes
/// `/dev/null` for the missing half of an add or a delete, so asking
/// only one side would decline every added and every deleted file.
///
/// The negative case is the reason this exists, and it is what lets the
/// documented `git difftool` recipe drop its pathspec: git invokes a
/// difftool on *every* changed file, and edamame has nothing to offer a
/// `.rs` or a `.png`.  Rendering one as Markdown would turn a `#`
/// comment into a heading; rendering it as plain text is worse than the
/// colored diff `git diff` already prints, and costs an `Esc` per file
/// to page past.  `main` declines the pair instead — before reading
/// either side, so a binary file is turned away by its name rather than
/// by a UTF-8 decode error.
pub fn is_markdown_pair(old: &Path, new: &Path) -> bool {
    is_markdown_path(old) || is_markdown_path(new)
}

/// Status-bar label for a difftool session (see `App::diff_label`).
///
/// The new side's file name, because git difftool reproduces the
/// repository's directory structure under its temp dir — so even a
/// commit-to-commit review carries the real basename.  Falls back to the
/// old side for a delete (where the new side is `/dev/null`), and names
/// both when they differ, which is what a rename looks like.
///
/// `null` is dropped as a name rather than a path, so it also covers the
/// `/dev/null` git substitutes on both an add and a delete.  A real file
/// named `null` is therefore labelled by its other side — a trade taken
/// deliberately: the git case is routine and the collision is not.
pub fn diff_label(old: &Path, new: &Path) -> String {
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| n != "null")
    };
    match (name(old), name(new)) {
        (Some(o), Some(n)) if o != n => format!("{o} → {n}"),
        (_, Some(n)) => n,
        (Some(o), None) => o,
        (None, None) => "[diff]".to_owned(),
    }
}

/// True when *git* is running us as an external diff over a list of
/// paths, rather than our having been typed by hand.
///
/// **Despite the name, this is not a `git difftool` test, and cannot be
/// made into one.**  `GIT_DIFF_PATH_TOTAL` (and its companion
/// `GIT_DIFF_PATH_COUNTER`) are exported by git's external-diff
/// machinery, not by `git difftool--helper` — so a plain `git diff`
/// running a gitattributes `diff.<driver>.command` sets them too, which
/// is the second recipe in `docs/editing.md`.  Only `GIT_DIFFTOOL_EXTCMD`
/// is difftool's own, and only under the `-x` form.  Measured on git
/// 2.50.1:
///
/// | invocation | `PATH_TOTAL` | `EXTCMD` |
/// |---|---|---|
/// | `git diff` + gitattributes driver | set | unset |
/// | `git difftool -x <cmd>` | set | set |
/// | `git difftool -t <tool>` | set | unset |
///
/// Rows one and three are therefore indistinguishable, and the check
/// cannot be narrowed to `EXTCMD`: the `-t` form — the alias recipe in
/// the docs — sets none.  Either variable is enough.
///
/// This gates [`stop_walk`], and the breadth is *right* rather than
/// merely tolerable, because the fact the gate needs is the one all
/// three rows share: git is driving us over a list of paths, so ending
/// our process group ends the list.  What it rules out is the case that
/// would be indefensible — a hand-typed `edamame --diff a.md b.md`,
/// where the group is whatever shell or script invoked us and nothing
/// there asked to be interrupted.
pub fn under_git_difftool() -> bool {
    is_difftool_env(
        std::env::var_os("GIT_DIFFTOOL_EXTCMD").is_some(),
        std::env::var_os("GIT_DIFF_PATH_TOTAL").is_some(),
    )
}

/// The decision behind [`under_git_difftool`], split out so it can be
/// tested without touching the process environment (see
/// `crate::test_env`).
fn is_difftool_env(extcmd: bool, path_total: bool) -> bool {
    extcmd || path_total
}

/// End a `git difftool` walk the way `Ctrl-C` would have, and never
/// return.
///
/// **Why a signal rather than an exit code.** A diff tool's only channel
/// back to git is its exit status, and `git difftool--helper` discards
/// it: the script ends in a bare `exit 0` unless
/// `GIT_DIFFTOOL_TRUST_EXIT_CODE` is `true`, which is not git's default.
/// So a tool that merely exits — with any status — cannot stop the walk,
/// and the user is left pressing `Esc` once per remaining file.
///
/// What *does* stop it is the signal a `Ctrl-C` at the terminal would have
/// raised — and a full-screen program is the one thing that prevents the
/// terminal raising it, since raw mode clears `ISIG` and the byte arrives as
/// a keystroke instead. So edamame delivers it by hand when the user quits.
/// git is in our process group, dies alongside us, cleans up its temp
/// directory on the way out (it installs handlers for exactly this), and
/// prints nothing — no `fatal: external diff died`, which is what an
/// exit-code stop produces and what makes a deliberate quit read as a
/// crash.
///
/// The key that gets here is edamame's ordinary `Quit` binding (`Ctrl-Q` by
/// default, rebindable like any other). `Ctrl-C` is deliberately *not* a
/// second door: it is `Action::Copy` everywhere in edamame, and a mode that
/// quietly redefined it would be the inconsistency, not the convenience.
///
/// **The caller must have restored the terminal first.** We are about to
/// die from a signal, so no cleanup after this point runs.
///
/// Gated on [`under_git_difftool`] by its caller — which establishes
/// that git is driving us over a list of paths, not that the driver is
/// `difftool` specifically (see there).  Ungated, a hand-typed
/// `edamame --diff` would signal whatever shell or script invoked it.
pub fn stop_walk() -> ! {
    #[cfg(unix)]
    // SAFETY: `signal` and `kill` are async-signal-safe and take no
    // pointers.  Resetting the disposition first means our own copy of
    // the signal terminates us rather than being ignored if anything in
    // the process has installed a handler; `kill(0, …)` addresses our
    // process group, which is the tty's foreground group — precisely the
    // set `Ctrl-C` would have reached.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::kill(0, libc::SIGINT);
    }
    // Unreachable on Unix: a signal sent to oneself is delivered before
    // `kill` returns.  On a platform without process groups this is the
    // whole implementation, and a non-zero status at least stops a walk
    // that was run with `--trust-exit-code`.
    std::process::exit(EXIT_INTERRUPTED);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn label(old: &str, new: &str) -> String {
        diff_label(&PathBuf::from(old), &PathBuf::from(new))
    }

    /// The ordinary case: git difftool preserves the basename under its
    /// temp dir, so both sides agree and the label is just the file.
    #[test]
    fn a_modified_file_is_labelled_by_its_name() {
        assert_eq!(
            label("/tmp/git-difftool.a/left/docs/x.md", "docs/x.md"),
            "x.md"
        );
    }

    /// `/dev/null` on either side is git's missing half, not a name.
    #[test]
    fn an_add_or_delete_is_labelled_by_the_side_that_exists() {
        assert_eq!(label("/dev/null", "docs/new.md"), "new.md");
        assert_eq!(label("docs/gone.md", "/dev/null"), "gone.md");
    }

    /// Differing basenames are what a rename looks like, and both halves
    /// are worth showing.
    #[test]
    fn a_rename_names_both_sides() {
        assert_eq!(label("old/a.md", "new/b.md"), "a.md → b.md");
    }

    /// Neither side yields a name (both `/dev/null`, or paths ending in
    /// `..`): a placeholder beats an empty status bar.
    #[test]
    fn a_nameless_pair_falls_back_to_a_placeholder() {
        assert_eq!(label("/dev/null", "/dev/null"), "[diff]");
    }

    /// Either side is enough, so an added Markdown file — whose old side
    /// is `/dev/null` — is still reviewed.
    #[test]
    fn is_markdown_pair_accepts_either_side() {
        let md = PathBuf::from("notes.md");
        let devnull = PathBuf::from("/dev/null");
        assert!(is_markdown_pair(&devnull, &md));
        assert!(is_markdown_pair(&md, &devnull));
        assert!(is_markdown_pair(
            &PathBuf::from("a.markdown"),
            &PathBuf::from("b.MD")
        ));
    }

    /// A source file is declined outright, so a `git difftool` walk can
    /// be run without a pathspec.
    #[test]
    fn is_markdown_pair_refuses_a_non_markdown_pair() {
        assert!(!is_markdown_pair(
            &PathBuf::from("src/main.rs"),
            &PathBuf::from("src/main.rs")
        ));
        assert!(!is_markdown_pair(
            &PathBuf::from("/dev/null"),
            &PathBuf::from("deploy.sh")
        ));
    }

    /// Either variable is enough, and `PATH_TOTAL` has to be one of
    /// them: it is the *only* signal both `git difftool -t` and an
    /// attribute-driven `git diff` raise (see `under_git_difftool` for
    /// the measured table).
    #[test]
    fn a_difftool_walk_is_recognised_from_either_variable() {
        assert!(is_difftool_env(true, false), "only -x sets EXTCMD");
        assert!(
            is_difftool_env(false, true),
            "-t and a gitattributes driver set PATH_TOTAL alone",
        );
        assert!(is_difftool_env(true, true));
    }

    /// A hand-typed `edamame --diff a.md b.md` must not be taken for a
    /// walk — `stop_walk` signals the process group, which outside one
    /// may be a shell script that never asked to be interrupted.
    #[test]
    fn a_hand_typed_invocation_is_not_a_difftool_walk() {
        assert!(!is_difftool_env(false, false));
    }

    #[test]
    fn read_side_reads_dev_null_as_an_empty_side() {
        assert_eq!(read_side(Path::new("/dev/null")).unwrap(), "");
    }

    #[test]
    fn read_side_names_the_path_it_could_not_read() {
        let err = read_side(Path::new("/nonexistent/edamame-difftool-fixture.md"))
            .expect_err("missing path must fail");
        assert!(
            format!("{err}").contains("edamame-difftool-fixture.md"),
            "the error must name the file: {err}",
        );
    }
}
