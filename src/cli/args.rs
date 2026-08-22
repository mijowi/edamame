//! Argument parsing: `Vec<OsString>` → [`Invocation`].
//!
//! [`Invocation::parse`] is a pure function of its argument iterator, so
//! every rule below is unit-testable without touching the environment.
//!
//! Arguments are taken as [`OsString`], not `String`: `std::env::args()`
//! *panics* on a non-UTF-8 argument, which on Linux is a perfectly legal
//! file name.  Flags are matched only after a successful `to_str()`, so a
//! path that isn't valid UTF-8 falls through to the positional arm and
//! reaches `PathBuf` intact.

use std::ffi::OsString;
use std::path::PathBuf;

/// What the command line asked edamame to do.
///
/// The informational variants carry no data — they print and exit.
/// Only [`Invocation::Run`] continues into terminal setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Start the editor.
    Run {
        /// File to open; `None` starts an empty, unnamed buffer.
        file: Option<PathBuf>,
        opts: RunOpts,
    },
    /// Print the flag list and exit.
    Help,
    /// Print `edamame <version>` and exit.
    Version,
    /// Print the diagnostic report and exit.
    Doctor,
    /// Open a read-only side-by-side review of two files
    /// (`--diff <old> <new>`), for use as a `git difftool`.
    ///
    /// Deliberately a variant of its own rather than a `RunOpts` flag:
    /// it takes *two* paths where a run takes one, it opens no file for
    /// editing, and it never starts the filesystem watcher — the paths
    /// git hands us are usually temp files it deletes the moment we
    /// exit.
    Diff {
        /// The "before" side — git's `$LOCAL`.
        old: PathBuf,
        /// The "after" side — git's `$REMOTE`.
        new: PathBuf,
        opts: RunOpts,
    },
}

/// Flags that modify a normal editor run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunOpts {
    /// `--no-config`: ignore `~/.config/edamame` entirely — no
    /// scaffolding, no reads, and (via `Config::persist`) no writes.
    pub no_config: bool,
    /// `--log`: force `[dev] logging = true` for this run without
    /// editing `config.toml`.
    pub log: bool,
}

/// A command line edamame can't act on.  Printed to stderr alongside
/// [`super::USAGE`], with exit status 2.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CliError {
    #[error("unknown option '{0}'")]
    UnknownOption(String),
    /// edamame opens one file per process; a second positional argument
    /// is far more likely a typo'd flag than an intent we should guess
    /// at.  The parenthetical is not padding: `--diff` takes two, and
    /// this same error is what a three-path `--diff` gets, where the
    /// bare rule would read as a flat contradiction of the command the
    /// user just ran.
    #[error("unexpected argument '{0}' — edamame opens one file at a time (two with --diff)")]
    ExtraArgument(String),
    /// A bare `-` conventionally means stdin, which edamame does not
    /// read: the terminal capability probe needs stdin for itself.
    /// Rejecting it beats a confusing "no such file: -".
    #[error("reading from stdin is not supported")]
    StdinNotSupported,
    /// `--diff` is the one mode that takes two paths, and neither has a
    /// sensible default: guessing (the open file? the working tree?)
    /// would silently review something other than what git asked for.
    #[error("--diff needs exactly two files: edamame --diff <old> <new>")]
    DiffNeedsTwoFiles,
}

impl Invocation {
    /// Parse arguments **excluding** `argv[0]`.
    ///
    /// Informational flags outrank a run: `--help` wins over everything,
    /// then `--version`, then `--doctor`.  A file argument alongside one
    /// of them is accepted and ignored rather than rejected — the user
    /// asked a question, and answering it is more useful than a lecture
    /// about argument order.
    ///
    /// `--` ends flag parsing, so a file genuinely named `--doctor` is
    /// reachable as `edamame -- --doctor`.
    ///
    /// Positionals are collected into a list rather than a single slot,
    /// because `--diff` takes two and may appear *after* them
    /// (`edamame a.md b.md --diff`).  The "one file at a time" rule is
    /// therefore enforced at the end, once the flags are known, instead
    /// of at the second positional — which also means a stray extra file
    /// no longer suppresses `--help`, matching how the informational
    /// flags already outrank everything else.
    pub fn parse<I>(args: I) -> Result<Self, CliError>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut files: Vec<PathBuf> = Vec::new();
        // The first positional past the first, remembered verbatim so a
        // non-`--diff` run can name *it* in the error rather than
        // whichever argument happened to overflow the list.
        let mut extra: Option<String> = None;
        let mut opts = RunOpts::default();
        let mut help = false;
        let mut version = false;
        let mut doctor = false;
        let mut diff = false;
        let mut positional_only = false;

        for arg in args {
            // A non-UTF-8 argument can never match a flag, so skip
            // straight to the positional arm and keep the bytes intact.
            let flag = if positional_only {
                None
            } else {
                arg.to_str().filter(|s| s.starts_with('-'))
            };

            match flag {
                Some("--") => positional_only = true,
                Some("-h" | "--help") => help = true,
                Some("-V" | "--version") => version = true,
                Some("--doctor") => doctor = true,
                Some("--diff") => diff = true,
                Some("--no-config") => opts.no_config = true,
                Some("--log") => opts.log = true,
                Some("-") => return Err(CliError::StdinNotSupported),
                Some(other) => return Err(CliError::UnknownOption(other.to_owned())),
                None => {
                    // Two is the most any mode accepts, so a third can
                    // never become valid however the flags parse.
                    if files.len() >= 2 {
                        return Err(CliError::ExtraArgument(arg.to_string_lossy().into_owned()));
                    }
                    if files.len() == 1 {
                        extra = Some(arg.to_string_lossy().into_owned());
                    }
                    files.push(PathBuf::from(arg));
                }
            }
        }

        if help {
            return Ok(Self::Help);
        }
        if version {
            return Ok(Self::Version);
        }
        if doctor {
            return Ok(Self::Doctor);
        }
        if diff {
            let mut it = files.into_iter();
            let (Some(old), Some(new)) = (it.next(), it.next()) else {
                return Err(CliError::DiffNeedsTwoFiles);
            };
            return Ok(Self::Diff { old, new, opts });
        }
        if let Some(extra) = extra {
            return Err(CliError::ExtraArgument(extra));
        }
        Ok(Self::Run {
            file: files.pop(),
            opts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Invocation, CliError> {
        Invocation::parse(args.iter().map(OsString::from))
    }

    fn run(args: &[&str]) -> (Option<PathBuf>, RunOpts) {
        match parse(args).expect("parses") {
            Invocation::Run { file, opts } => (file, opts),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn no_arguments_starts_an_empty_buffer() {
        assert_eq!(run(&[]), (None, RunOpts::default()));
    }

    #[test]
    fn a_bare_path_is_the_file_to_open() {
        let (file, opts) = run(&["notes.md"]);
        assert_eq!(file, Some(PathBuf::from("notes.md")));
        assert_eq!(opts, RunOpts::default());
    }

    #[test]
    fn run_flags_combine_with_a_file_in_any_order() {
        let expected = RunOpts {
            no_config: true,
            log: true,
        };
        for args in [
            ["--no-config", "--log", "notes.md"],
            ["notes.md", "--no-config", "--log"],
            ["--log", "notes.md", "--no-config"],
        ] {
            let (file, opts) = run(&args);
            assert_eq!(file, Some(PathBuf::from("notes.md")), "{args:?}");
            assert_eq!(opts, expected, "{args:?}");
        }
    }

    #[test]
    fn informational_flags_have_both_spellings() {
        assert_eq!(parse(&["--help"]), Ok(Invocation::Help));
        assert_eq!(parse(&["-h"]), Ok(Invocation::Help));
        assert_eq!(parse(&["--version"]), Ok(Invocation::Version));
        assert_eq!(parse(&["-V"]), Ok(Invocation::Version));
        assert_eq!(parse(&["--doctor"]), Ok(Invocation::Doctor));
    }

    /// Help outranks version outranks doctor, and a stray file argument
    /// never turns a question into an error.
    #[test]
    fn informational_flags_win_over_a_run() {
        assert_eq!(
            parse(&["--doctor", "--version", "--help"]),
            Ok(Invocation::Help)
        );
        assert_eq!(parse(&["--doctor", "--version"]), Ok(Invocation::Version));
        assert_eq!(parse(&["notes.md", "--doctor"]), Ok(Invocation::Doctor));
        assert_eq!(
            parse(&["--no-config", "--version"]),
            Ok(Invocation::Version)
        );
    }

    /// `--` is the escape hatch for a file whose name looks like a flag.
    #[test]
    fn double_dash_ends_flag_parsing() {
        let (file, opts) = run(&["--log", "--", "--doctor"]);
        assert_eq!(file, Some(PathBuf::from("--doctor")));
        assert!(opts.log, "flags before `--` still apply");

        // Everything after `--` is positional, so a second one is an
        // ordinary extra-argument error rather than a repeated separator.
        assert_eq!(
            parse(&["--", "a.md", "--"]),
            Err(CliError::ExtraArgument("--".to_owned()))
        );
    }

    #[test]
    fn unknown_flags_and_extra_files_are_rejected() {
        assert_eq!(
            parse(&["--doctorr"]),
            Err(CliError::UnknownOption("--doctorr".to_owned()))
        );
        assert_eq!(
            parse(&["-x"]),
            Err(CliError::UnknownOption("-x".to_owned()))
        );
        // Bundled short flags are deliberately not supported.
        assert_eq!(
            parse(&["-hV"]),
            Err(CliError::UnknownOption("-hV".to_owned()))
        );
        assert_eq!(
            parse(&["a.md", "b.md"]),
            Err(CliError::ExtraArgument("b.md".to_owned()))
        );
    }

    // ── --diff ───────────────────────────────────────────────────

    fn diff(args: &[&str]) -> (PathBuf, PathBuf, RunOpts) {
        match parse(args).expect("parses") {
            Invocation::Diff { old, new, opts } => (old, new, opts),
            other => panic!("expected Diff, got {other:?}"),
        }
    }

    #[test]
    fn diff_takes_two_paths_in_git_difftool_order() {
        let (old, new, opts) = diff(&["--diff", "left.md", "right.md"]);
        assert_eq!(old, PathBuf::from("left.md"));
        assert_eq!(new, PathBuf::from("right.md"));
        assert_eq!(opts, RunOpts::default());
    }

    /// The flag may trail its operands: a `difftool.<tool>.cmd` is a
    /// shell string users reorder freely, and git itself appends nothing.
    #[test]
    fn diff_accepts_the_flag_in_any_position() {
        for args in [
            ["--diff", "a.md", "b.md"],
            ["a.md", "--diff", "b.md"],
            ["a.md", "b.md", "--diff"],
        ] {
            let (old, new, _) = diff(&args);
            assert_eq!((old, new), (PathBuf::from("a.md"), PathBuf::from("b.md")));
        }
    }

    #[test]
    fn diff_combines_with_the_run_flags() {
        let (_, _, opts) = diff(&["--diff", "--no-config", "a.md", "--log", "b.md"]);
        assert_eq!(
            opts,
            RunOpts {
                no_config: true,
                log: true
            }
        );
    }

    /// Neither side has a defensible default, so a short command line is
    /// an error rather than a guess.
    #[test]
    fn diff_with_fewer_than_two_files_is_an_error() {
        assert_eq!(parse(&["--diff"]), Err(CliError::DiffNeedsTwoFiles));
        assert_eq!(
            parse(&["--diff", "only.md"]),
            Err(CliError::DiffNeedsTwoFiles)
        );
    }

    #[test]
    fn diff_still_rejects_a_third_file() {
        assert_eq!(
            parse(&["--diff", "a.md", "b.md", "c.md"]),
            Err(CliError::ExtraArgument("c.md".to_owned()))
        );
    }

    /// `--diff` is a run, so the informational flags still outrank it.
    #[test]
    fn informational_flags_win_over_a_diff() {
        assert_eq!(
            parse(&["--diff", "a.md", "b.md", "--help"]),
            Ok(Invocation::Help)
        );
        assert_eq!(parse(&["--diff", "--version"]), Ok(Invocation::Version));
    }

    /// Two positionals are only legal under `--diff`; without it the
    /// second is still the "one file at a time" error, and it names the
    /// second file rather than whichever argument overflowed the list.
    #[test]
    fn two_files_without_diff_still_name_the_second_one() {
        assert_eq!(
            parse(&["a.md", "b.md"]),
            Err(CliError::ExtraArgument("b.md".to_owned()))
        );
        assert_eq!(
            parse(&["a.md", "b.md", "c.md"]),
            Err(CliError::ExtraArgument("c.md".to_owned()))
        );
    }

    #[test]
    fn a_bare_dash_is_refused_rather_than_opened_as_a_file() {
        assert_eq!(parse(&["-"]), Err(CliError::StdinNotSupported));
    }

    /// `std::env::args()` panics on a non-UTF-8 argument; we take
    /// `OsString` so a legal Linux file name survives to `PathBuf`.
    #[cfg(unix)]
    #[test]
    fn non_utf8_file_names_survive() {
        use std::os::unix::ffi::OsStringExt;

        let raw = OsString::from_vec(vec![b'n', 0xff, b'.', b'm', b'd']);
        let parsed = Invocation::parse([raw.clone()]).expect("parses");
        assert_eq!(
            parsed,
            Invocation::Run {
                file: Some(PathBuf::from(raw)),
                opts: RunOpts::default(),
            }
        );
    }
}
