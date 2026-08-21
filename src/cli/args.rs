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
use std::path::{Path, PathBuf};

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
    /// is far more likely a typo'd flag than an intent we should guess at.
    #[error("unexpected argument '{0}' — edamame opens one file at a time")]
    ExtraArgument(String),
    /// A bare `-` conventionally means stdin, which edamame does not
    /// read: the terminal capability probe needs stdin for itself.
    /// Rejecting it beats a confusing "no such file: -".
    #[error("reading from stdin is not supported")]
    StdinNotSupported,
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
    pub fn parse<I>(args: I) -> Result<Self, CliError>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut file: Option<PathBuf> = None;
        let mut opts = RunOpts::default();
        let mut help = false;
        let mut version = false;
        let mut doctor = false;
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
                Some("--no-config") => opts.no_config = true,
                Some("--log") => opts.log = true,
                Some("-") => return Err(CliError::StdinNotSupported),
                Some(other) => return Err(CliError::UnknownOption(other.to_owned())),
                None => {
                    if file.is_some() {
                        return Err(CliError::ExtraArgument(arg.to_string_lossy().into_owned()));
                    }
                    file = Some(PathBuf::from(arg));
                }
            }
        }

        Ok(if help {
            Self::Help
        } else if version {
            Self::Version
        } else if doctor {
            Self::Doctor
        } else {
            Self::Run { file, opts }
        })
    }
}

/// Split a `file.md#section` startup argument into the file to open and
/// the heading to land on — the command-line half of deep linking, so
/// `edamame docs/editing.md#images` opens the same place the in-document
/// link of that name does.
///
/// This is *not* done in [`Invocation::parse`], which is pure: a `#` is
/// a legal character in a file name, so the split has to ask the disk
/// before taking one away from a path. The literal path wins — a file
/// really named `notes#1.md` opens as itself and no anchor is reported.
/// Only when that path does not exist is the text after the last `#`
/// taken as a heading (and only when both halves are non-empty; a
/// trailing `#` names nothing, and a leading one is the whole file
/// name).
///
/// A non-UTF-8 argument is never split — it reaches `PathBuf` intact,
/// same as in the parser.
pub fn split_startup_anchor(arg: &Path) -> (PathBuf, Option<String>) {
    split_startup_anchor_with(arg, |p| p.exists())
}

/// [`split_startup_anchor`] with the disk lookup injected, so the rule
/// is unit-testable without laying files down.
fn split_startup_anchor_with(
    arg: &Path,
    exists: impl Fn(&Path) -> bool,
) -> (PathBuf, Option<String>) {
    let keep = || (arg.to_path_buf(), None);
    let Some(text) = arg.to_str() else {
        return keep();
    };
    // The *last* `#`, so a directory carrying one still resolves:
    // `~/notes#2024/index.md#intro`.
    let Some((path, fragment)) = text.rsplit_once('#') else {
        return keep();
    };
    if path.is_empty() || fragment.is_empty() || exists(arg) {
        return keep();
    }
    (PathBuf::from(path), Some(fragment.to_owned()))
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
    // ── Startup anchor (`file.md#section`) ────────────────────────────

    /// Nothing on disk under either name — the `#` is a deep link.
    fn split(arg: &str) -> (PathBuf, Option<String>) {
        split_startup_anchor_with(Path::new(arg), |_| false)
    }

    #[test]
    fn startup_argument_splits_off_its_section() {
        assert_eq!(
            split("docs/editing.md#images"),
            (PathBuf::from("docs/editing.md"), Some("images".to_owned()))
        );
    }

    #[test]
    fn startup_argument_without_a_section_is_untouched() {
        assert_eq!(
            split("docs/editing.md"),
            (PathBuf::from("docs/editing.md"), None)
        );
    }

    #[test]
    fn an_empty_half_is_not_a_section() {
        assert_eq!(split("notes.md#"), (PathBuf::from("notes.md#"), None));
        assert_eq!(split("#notes.md"), (PathBuf::from("#notes.md"), None));
    }

    #[test]
    fn only_the_last_hash_splits() {
        assert_eq!(
            split("notes#2024/index.md#intro"),
            (
                PathBuf::from("notes#2024/index.md"),
                Some("intro".to_owned())
            )
        );
    }

    #[test]
    fn a_file_really_named_with_a_hash_wins_over_the_split() {
        let arg = Path::new("notes#1.md");
        assert_eq!(
            split_startup_anchor_with(arg, |p| p == Path::new("notes#1.md")),
            (PathBuf::from("notes#1.md"), None)
        );
    }
}
