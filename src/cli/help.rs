//! `--help` and `--version` output.
//!
//! Deliberately a flag list and two links, not a manual: the shipped
//! documentation in `docs/` is the reference, and a help text that
//! restates it is a second copy to keep in sync.

/// The version edamame was compiled at — the single read of
/// `CARGO_PKG_VERSION` on the CLI path.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where `--help` and the unknown-argument error point the reader.
const DOCS_URL: &str = "https://github.com/mijowi/edamame/blob/main/docs/getting-started.md";
const ISSUES_URL: &str = "https://github.com/mijowi/edamame/issues";

/// The usage line, printed under a [`super::CliError`] on stderr.
pub const USAGE: &str =
    "USAGE:\n    edamame [OPTIONS] [FILE]\n    edamame --diff <OLD> <NEW>\n\nTry 'edamame --help'.";

/// `--version` output: `edamame 0.1.0`.
///
/// The bare `name version` form is the convention every tool that ships
/// in a bug report follows, and it is what `cargo --version`-style
/// scrapers expect.
pub fn version_line() -> String {
    format!("edamame {VERSION}")
}

/// `--help` output, including the trailing newline.
pub fn help_text() -> String {
    format!(
        "\
edamame {VERSION}
A fast TUI Markdown editor and viewer

USAGE:
    edamame [OPTIONS] [FILE]
    edamame --diff <OLD> <NEW>

ARGS:
    <FILE>           Markdown file to open (empty buffer if omitted)

OPTIONS:
    -h, --help       Print this help
    -V, --version    Print the installed version
        --doctor     Print version, system, and terminal diagnostics;
                     include this output when reporting an issue
        --diff       Review two Markdown files side by side, read-only.
                     A pair that isn't Markdown is skipped.  Intended as
                     a git difftool; see docs/editing.md
        --no-config  Ignore ~/.config/edamame for the whole run: built-in
                     defaults only, and no settings saved
        --log        Write a debug log for this run
        --           Treat every later argument as the file name

Documentation:   {DOCS_URL}
Report an issue: {ISSUES_URL}
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_is_name_then_version() {
        assert_eq!(version_line(), format!("edamame {VERSION}"));
    }

    /// Every flag the parser accepts must be discoverable from `--help`;
    /// a flag documented nowhere else is a flag nobody finds.
    #[test]
    fn help_lists_every_supported_flag() {
        let help = help_text();
        for flag in [
            "-h",
            "--help",
            "-V",
            "--version",
            "--doctor",
            "--diff",
            "--no-config",
            "--log",
        ] {
            assert!(help.contains(flag), "--help omits {flag}");
        }
        assert!(help.contains(DOCS_URL));
        assert!(help.contains(ISSUES_URL));
        assert!(help.ends_with('\n'), "help text must end in a newline");
    }
}
