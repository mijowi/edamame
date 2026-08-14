//! `--doctor`: the diagnostic report a user pastes into a GitHub issue.
//!
//! Two sections — the system and terminal facts that identify *where*
//! edamame is running, then the capability rows that say what that
//! terminal can do.  The capability half is not re-derived here: it comes
//! from [`CapSummary::from_caps`], the same builder the in-app
//! capabilities notice and welcome modal render, so the CLI and the TUI
//! can never disagree about what was detected.
//!
//! **The probe needs a real terminal.**  `Capabilities::detect` writes
//! escape sequences and reads the replies off the tty, so under
//! `edamame --doctor > report.txt` it would both pollute the file and
//! report "no image support" for a terminal that has it.  [`run`]
//! therefore checks `IsTerminal` first and falls back to
//! [`Capabilities::env_only`], marking the two probe-derived rows
//! [`Status::Unknown`] rather than guessing.
//!
//! System facts are read from files, never a subprocess: `--doctor` is a
//! diagnostic path, and spawning processes is a hardened area of this
//! codebase (`docs/security.md`).  Anything that can't be resolved
//! degrades to a coarser answer, never to an error.

use std::env;
use std::io::IsTerminal;

use anyhow::Result;

use super::help::VERSION;
use crate::terminal::{self, Capabilities, TerminalSetup};
use crate::ui::cap_summary::{CapRow, CapSummary};

/// Value printed for any fact the environment doesn't carry.
const UNKNOWN: &str = "unknown";

/// Probe the terminal (when there is one), then print the report to
/// stdout.
///
/// The report goes to stdout so it can be redirected or piped; the
/// not-a-terminal notice goes with it rather than to stderr, because it
/// is part of what the reader of the report needs to know.
pub fn run() -> Result<()> {
    let interactive = std::io::stdout().is_terminal() && std::io::stdin().is_terminal();

    let caps = if interactive {
        // Same ordering constraint as `main`: the probe must run after
        // the alternate screen is up.  Nothing is drawn — we enter,
        // probe, and leave — so the report lands on the user's normal
        // screen with their scrollback intact.
        let TerminalSetup {
            terminal,
            keyboard_enhancement,
        } = terminal::setup()?;
        // The `Terminal` is unused: `--doctor` never draws a frame.
        // Dropping it here rather than holding it across the probe keeps
        // the borrow-free shape obvious.
        drop(terminal);

        let orig_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = terminal::restore();
            orig_hook(info);
        }));

        // The hook stays installed for the rest of the (very short)
        // process: `take_hook` would swap in the *default* hook rather
        // than the original, and a panic while printing the report
        // should still leave the terminal usable.
        let caps = Capabilities::detect(keyboard_enhancement);
        terminal::restore()?;
        caps
    } else {
        Capabilities::env_only()
    };

    print!("{}", report(&caps, interactive));
    Ok(())
}

/// How confident the report is about one capability row.
///
/// [`CapRow`] carries a two-state `ok` flag, which is the right model for
/// the TUI (where every row was genuinely probed).  The CLI has a third
/// case the TUI can't reach — output redirected away from a terminal —
/// and reporting that as a ✗ would send users chasing a missing feature
/// their terminal actually has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Warn,
    Unknown,
}

impl Status {
    /// Leading marker, padded to a fixed width so the label column lines
    /// up across rows.
    fn marker(self) -> &'static str {
        match self {
            Self::Ok => "ok  ",
            Self::Warn => "warn",
            Self::Unknown => "?   ",
        }
    }
}

/// Value substituted for the two rows that need a live probe when there
/// isn't one.
///
/// Deliberately doesn't name a stream: [`run`] requires stdout *and*
/// stdin to be terminals (it writes escape sequences to one and reads
/// the replies from the other), so blaming stdout was wrong for the
/// `echo | edamame --doctor` case — where stdout plainly *is* a
/// terminal, and the user has no way to work out that redirected stdin
/// is what stopped the probe.
const NOT_PROBED: &str = "unknown — needs an interactive terminal";

/// Build the full report text, including the trailing newline.
///
/// `probed` is false when [`run`] skipped the escape-sequence probe;
/// the Images and Keyboard rows are the only two that depend on it
/// (color, mouse, and unicode are env-derived either way), so they —
/// and only they — are downgraded to [`Status::Unknown`].
pub fn report(caps: &Capabilities, probed: bool) -> String {
    let mut out = format!("edamame {VERSION}\n\nSystem\n");
    for (label, value) in system_facts() {
        out.push_str(&format!("  {label:<11} {value}\n"));
    }

    out.push_str("\nTerminal capabilities\n");
    for row in CapSummary::from_caps(caps).rows {
        let status = row_status(&row, probed);
        let value = if status == Status::Unknown {
            NOT_PROBED.to_owned()
        } else {
            row.value
        };
        let label = format!("{}:", row.label);
        out.push_str(&format!("  {} {label:<10} {value}\n", status.marker()));
    }
    out
}

/// Resolve one capability row's status.  `Images` and `Keyboard` are the
/// probe-derived pair; every other row is env-derived and keeps its
/// detected verdict whether or not the probe ran.
fn row_status(row: &CapRow, probed: bool) -> Status {
    match row.label {
        "Images" | "Keyboard" if !probed => Status::Unknown,
        _ if row.ok => Status::Ok,
        _ => Status::Warn,
    }
}

// ── System facts ─────────────────────────────────────────────────────────────

/// The `System` section as ordered `(label, value)` pairs.
///
/// Every fact here describes the *machine*, never the person using it:
/// the report exists to be pasted into a public issue tracker, and
/// someone doing that has no reason to scan it for their own identity
/// first.  The config directory is deliberately absent for that reason —
/// it is a username in the common case, and all it carries diagnostically
/// is "this path is or isn't the default", which is worth asking about by
/// hand on the rare issue where it matters.
fn system_facts() -> Vec<(&'static str, String)> {
    vec![
        ("OS:", format!("{} ({})", os_version(), env::consts::ARCH)),
        ("Terminal:", terminal_program()),
        ("TERM:", env_or_unknown("TERM")),
        ("COLORTERM:", env_or_unknown("COLORTERM")),
        ("Locale:", locale()),
        (
            "tmux:",
            if env::var_os("TMUX").is_some() {
                "yes".to_owned()
            } else {
                "no".to_owned()
            },
        ),
    ]
}

fn env_or_unknown(key: &str) -> String {
    env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| UNKNOWN.to_owned())
}

/// A human-readable OS name and version.
///
/// Read from the platform's own metadata file rather than by spawning
/// `sw_vers` / `lsb_release`, and falling back to the compile-time
/// `env::consts::OS` when that file is missing or shaped unexpectedly.
/// Windows has no equivalent file, so it reports the bare OS name.
fn os_version() -> String {
    #[cfg(target_os = "macos")]
    {
        const PLIST: &str = "/System/Library/CoreServices/SystemVersion.plist";
        if let Ok(text) = std::fs::read_to_string(PLIST) {
            if let Some(v) = parse_plist_value(&text, "ProductVersion") {
                return format!("macOS {v}");
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(text) = std::fs::read_to_string("/etc/os-release") {
            if let Some(name) = parse_os_release(&text, "PRETTY_NAME") {
                return name;
            }
        }
    }
    env::consts::OS.to_owned()
}

/// Extract a `KEY=value` entry from an `/etc/os-release` body, stripping
/// the optional surrounding quotes.  Comments and unrelated keys are
/// skipped; a key that appears with an empty value reads as absent.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
fn parse_os_release(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim().trim_matches(['"', '\'']).to_owned())
        .filter(|v| !v.is_empty())
}

/// Pull `<key>NAME</key><string>VALUE</string>` out of an XML plist.
///
/// A deliberate string scan rather than an XML dependency: this file's
/// shape has been stable for the entire life of macOS, the value is a
/// plain string, and a miss degrades to the bare OS name.  Same posture
/// as `app::update_check::parse_tag_name`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_plist_value(text: &str, key: &str) -> Option<String> {
    let rest = &text[text.find(&format!("<key>{key}</key>"))?..];
    let rest = &rest[rest.find("<string>")? + "<string>".len()..];
    let value = &rest[..rest.find("</string>")?];
    (!value.is_empty()).then(|| value.to_owned())
}

/// The terminal emulator's name and version, from the environment.
///
/// `$TERM_PROGRAM` / `$TERM_PROGRAM_VERSION` are set by iTerm2, Apple
/// Terminal, WezTerm, Ghostty, and VS Code; `$LC_TERMINAL` is iTerm2's
/// own marker, which — unlike `$TERM_PROGRAM` — survives ssh.  kitty,
/// alacritty, and foot set none of them and read as unknown; the `TERM`
/// row below usually names them anyway.
///
/// Note this is exactly the pair `Capabilities::fingerprint` leaves out
/// on purpose (a version bump must not re-trigger the new-terminal
/// notice).  Here the version is the point.
fn terminal_program() -> String {
    let name = env::var("TERM_PROGRAM")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| env::var("LC_TERMINAL").ok().filter(|v| !v.is_empty()));
    let version = env::var("TERM_PROGRAM_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            env::var("LC_TERMINAL_VERSION")
                .ok()
                .filter(|v| !v.is_empty())
        });
    match (name, version) {
        (Some(n), Some(v)) => format!("{n} {v}"),
        (Some(n), None) => n,
        (None, _) => UNKNOWN.to_owned(),
    }
}

/// The active locale and which variable supplied it — the same
/// precedence `capabilities::detect_unicode_full` walks, so a user
/// puzzled by a ✗ Unicode row can see exactly which variable decided it.
fn locale() -> String {
    for var in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Ok(v) = env::var(var) {
            if !v.is_empty() {
                return format!("{v} ({var})");
            }
        }
    }
    format!("{UNKNOWN} (LC_ALL, LC_CTYPE, LANG all unset)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{ColorDepth, ImageProtocol};

    fn caps(depth: ColorDepth, image: Option<ImageProtocol>) -> Capabilities {
        Capabilities {
            color_depth: depth,
            image_protocol: image,
            mouse: true,
            unicode_full: true,
            keyboard_enhancement: true,
            ..Capabilities::minimal()
        }
    }

    /// Every test that calls [`report`] takes the crate-wide env lock:
    /// the System section reads `TERM_PROGRAM`, `XDG_CONFIG_HOME` and
    /// friends, which env-mutating tests elsewhere in the binary write.
    #[test]
    fn report_opens_with_the_version_and_carries_both_sections() {
        let _lock = crate::test_env::env_lock();
        let text = report(
            &caps(ColorDepth::TrueColor, Some(ImageProtocol::KittyGraphics)),
            true,
        );
        assert!(text.starts_with(&format!("edamame {VERSION}\n")));
        assert!(text.contains("\nSystem\n"));
        assert!(text.contains("\nTerminal capabilities\n"));
        assert!(text.ends_with('\n'));
    }

    /// The capability half must be the summary builder's text verbatim —
    /// if the CLI restated it, the two surfaces would drift.
    #[test]
    fn capability_values_come_from_the_shared_summary() {
        let _lock = crate::test_env::env_lock();
        let caps = caps(ColorDepth::TrueColor, Some(ImageProtocol::KittyGraphics));
        let text = report(&caps, true);
        for row in CapSummary::from_caps(&caps).rows {
            assert!(
                text.contains(&row.value),
                "{} row value missing from the report: {:?}",
                row.label,
                row.value
            );
        }
    }

    /// Just the `Terminal capabilities` rows.
    ///
    /// Assertions about status markers must not see the `System`
    /// section: its values come from the live environment, not from the
    /// fixture, so a `?` in a `TERM_PROGRAM_VERSION`, a `PRETTY_NAME`,
    /// or a `LANG` would fail a test that is about the marker column.
    fn capability_section(text: &str) -> String {
        let (_, caps) = text
            .split_once("\nTerminal capabilities\n")
            .expect("capabilities section");
        caps.to_owned()
    }

    #[test]
    fn every_row_is_marked_ok_or_warn_when_probed() {
        let _lock = crate::test_env::env_lock();
        // Truecolor + Kitty + mouse + kbd + UTF-8 is the all-green case.
        let text = report(
            &caps(ColorDepth::TrueColor, Some(ImageProtocol::KittyGraphics)),
            true,
        );
        let rows = capability_section(&text);
        assert_eq!(rows.matches("  ok   ").count(), 5, "{rows}");
        assert!(
            !rows.contains('?'),
            "nothing is unknown when probed: {rows}"
        );

        // 256-color with no image protocol degrades Color and Images only.
        let text = report(&caps(ColorDepth::Ansi256, None), true);
        assert_eq!(
            capability_section(&text).matches("  warn ").count(),
            2,
            "{text}"
        );
    }

    /// Without a live terminal the two probe-derived rows must read as
    /// unknown, not as failures — a redirected report otherwise tells the
    /// user their terminal lacks images it actually supports.
    #[test]
    fn unprobed_rows_are_unknown_not_failures() {
        let _lock = crate::test_env::env_lock();
        let text = report(&caps(ColorDepth::TrueColor, None), false);
        assert_eq!(text.matches(NOT_PROBED).count(), 2, "{text}");
        for label in ["Images:", "Keyboard:"] {
            let line = text
                .lines()
                .find(|l| l.contains(label))
                .unwrap_or_else(|| panic!("no {label} row"));
            assert!(line.contains('?'), "{line}");
            assert!(line.contains(NOT_PROBED), "{line}");
        }
        // Env-derived rows are unaffected by the missing probe.
        let color = text
            .lines()
            .find(|l| l.contains("Color:"))
            .expect("color row");
        assert!(color.contains("ok"), "{color}");
        assert!(!color.contains(NOT_PROBED), "{color}");
    }

    #[test]
    fn os_release_parsing_strips_quotes_and_skips_other_keys() {
        let text = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nID=ubuntu\n";
        assert_eq!(
            parse_os_release(text, "PRETTY_NAME").as_deref(),
            Some("Ubuntu 24.04.1 LTS")
        );
        assert_eq!(parse_os_release(text, "ID").as_deref(), Some("ubuntu"));
        assert_eq!(parse_os_release(text, "VERSION_ID"), None);
        // An empty value is as good as absent — the caller falls back.
        assert_eq!(parse_os_release("PRETTY_NAME=\"\"\n", "PRETTY_NAME"), None);
    }

    #[test]
    fn plist_parsing_finds_the_string_after_its_key() {
        let text = "\
<dict>
\t<key>ProductName</key>
\t<string>macOS</string>
\t<key>ProductVersion</key>
\t<string>15.6</string>
</dict>";
        assert_eq!(
            parse_plist_value(text, "ProductVersion").as_deref(),
            Some("15.6")
        );
        assert_eq!(
            parse_plist_value(text, "ProductName").as_deref(),
            Some("macOS")
        );
        assert_eq!(parse_plist_value(text, "ProductBuildVersion"), None);
        // A malformed body degrades to None rather than panicking on a slice.
        assert_eq!(
            parse_plist_value("<key>ProductVersion</key>", "ProductVersion"),
            None
        );
    }

    /// Every system fact must resolve to *something* — the section is
    /// what identifies the reporter's machine, so a silently empty value
    /// is worse than "unknown".
    ///
    /// Takes the crate-wide env lock: this reads `TERM_PROGRAM`,
    /// `XDG_CONFIG_HOME` and friends, which env-mutating tests in
    /// `terminal::capabilities` and `config::config` write concurrently.
    #[test]
    fn no_system_fact_is_ever_blank() {
        let _lock = crate::test_env::env_lock();
        for (label, value) in system_facts() {
            assert!(!value.trim().is_empty(), "{label} resolved to nothing");
        }
    }

    /// The report is written to be pasted into a public issue tracker,
    /// so it describes the machine and never the person.  The config
    /// directory was the one row that did — `/Users/<name>/…` — and it
    /// is deliberately gone; a row that reintroduces a home-relative
    /// path fails here.
    #[test]
    fn no_system_fact_leaks_the_home_directory() {
        let Some(home) = dirs::home_dir() else { return };
        let home = home.display().to_string();
        // A degenerate `/` home would match everything; nothing to test.
        if home.len() < 2 {
            return;
        }
        for (label, value) in system_facts() {
            assert!(
                !value.contains(&home),
                "{label} carries the user's home directory: {value:?}"
            );
        }
    }
}
