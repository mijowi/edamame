//! `--doctor`: the diagnostic report a user pastes into a GitHub issue.
//!
//! System/terminal facts, then capability rows.  The capability half comes from
//! [`CapSummary::from_caps`] — the same builder the TUI renders — so the two can't disagree.
//!
//! **The probe needs a real terminal.**  `Capabilities::detect` writes escape sequences and reads
//! the replies off the tty, so under `edamame --doctor > report.txt` it would pollute the file and
//! report "no image support" for a terminal that has it.  [`run`] checks `IsTerminal` first and
//! falls back to [`Capabilities::env_only`], marking the two probe-derived rows
//! [`Status::Unknown`] rather than guessing.
//!
//! System facts are read from files, never a subprocess (`docs/dev/security-invariants.md`); anything
//! unresolvable degrades to a coarser answer, never an error.

use std::env;
use std::io::IsTerminal;

use anyhow::Result;

use super::help::VERSION;
use crate::config::Config;
use crate::terminal::{
    self, active_locale, is_windows_terminal, Capabilities, ColorDepth, TerminalSetup,
};
use crate::ui::cap_summary::{CapRow, CapSummary};

/// Value printed for any fact the environment doesn't carry.
const UNKNOWN: &str = "unknown";

/// Probe the terminal (when there is one) and print the report to stdout.  The not-a-terminal
/// notice goes to stdout too — it is part of what a reader of the report needs to know.
pub fn run() -> Result<()> {
    let interactive = std::io::stdout().is_terminal() && std::io::stdin().is_terminal();

    let caps = if interactive {
        // Same ordering constraint as `main`: the probe must run after the alternate screen is
        // up.  Nothing is drawn, so the report lands on the normal screen with scrollback intact.
        let TerminalSetup {
            terminal,
            keyboard_enhancement,
        } = terminal::setup()?;
        drop(terminal); // `--doctor` never draws a frame.

        let orig_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = terminal::restore();
            orig_hook(info);
        }));

        // The hook stays installed for the rest of the process: `take_hook` would swap in the
        // *default* hook, and a panic while printing should still leave the terminal usable.
        let caps = Capabilities::detect(keyboard_enhancement, sharp_scrolling());
        terminal::restore()?;
        caps
    } else {
        Capabilities::env_only()
    };

    print!("{}", report(&caps, interactive));
    Ok(())
}

/// The user's `images.sharp_scrolling` setting, so the reported image protocol matches the one the
/// TUI would pick (direct placement vs. the placeholder/iTerm2 path).  The load is file-only —
/// `persist_fallback = false` writes nothing — which keeps `--doctor` a pure read, and any failure
/// falls back to the default (on).
fn sharp_scrolling() -> bool {
    let truecolor = Capabilities::detect_color_depth_from_env() == ColorDepth::TrueColor;
    Config::load(truecolor, false)
        .map(|loaded| loaded.config.images.sharp_scrolling)
        .unwrap_or(true)
}

/// How confident the report is about one capability row.  [`CapRow`]'s two-state `ok` flag is
/// enough for the TUI, but the CLI has a third case it can't reach — output redirected away from a
/// terminal — and reporting that as a failure sends users chasing a feature they already have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Warn,
    Unknown,
}

impl Status {
    /// Leading marker, padded so the label column lines up across rows.
    fn marker(self) -> &'static str {
        match self {
            Self::Ok => "ok  ",
            Self::Warn => "warn",
            Self::Unknown => "?   ",
        }
    }
}

/// Substituted for the two probe-derived rows when there was no probe.  Deliberately names no
/// stream: [`run`] requires stdout *and* stdin to be terminals, so blaming stdout misleads in the
/// `echo | edamame --doctor` case.
const NOT_PROBED: &str = "unknown — needs an interactive terminal";

/// Build the full report text, including the trailing newline.  When `probed` is false the Images
/// and Keyboard rows — the only two that need the probe — drop to [`Status::Unknown`].
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

/// `Images` and `Keyboard` are the probe-derived pair; every other row is env-derived and keeps
/// its verdict whether or not the probe ran.
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
/// Every fact must describe the *machine*, never the person: the report is pasted into a public
/// issue tracker.  The config directory is deliberately absent — it is usually a username, and
/// carries little diagnostically.
fn system_facts() -> Vec<(&'static str, String)> {
    vec![
        ("OS:", format!("{} ({})", os_version(), env::consts::ARCH)),
        ("Terminal:", terminal_program()),
        ("TERM:", env_or_unset("TERM")),
        ("COLORTERM:", env_or_unset("COLORTERM")),
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

fn env_or_unset(key: &str) -> String {
    env_value_or_unset(env::var(key))
}

/// An empty value reads as unset, as it does to capability detection.  `unknown` is kept for the
/// one case that genuinely is: a value that is not valid Unicode.
fn env_value_or_unset(value: Result<String, env::VarError>) -> String {
    match value {
        Ok(v) if !v.is_empty() => v,
        Ok(_) | Err(env::VarError::NotPresent) => "unset".to_owned(),
        Err(env::VarError::NotUnicode(_)) => UNKNOWN.to_owned(),
    }
}

/// A human-readable OS name and version, read from the platform's metadata file rather than by
/// spawning `sw_vers` / `lsb_release`.  Falls back to `env::consts::OS` (always, on Windows).
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

/// Extract a `KEY=value` entry from an `/etc/os-release` body, stripping optional quotes.  An
/// empty value reads as absent.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
fn parse_os_release(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim().trim_matches(['"', '\'']).to_owned())
        .filter(|v| !v.is_empty())
}

/// Pull `<key>NAME</key><string>VALUE</string>` out of an XML plist.  A deliberate string scan
/// rather than an XML dependency: the shape is stable and a miss degrades to the bare OS name.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_plist_value(text: &str, key: &str) -> Option<String> {
    let rest = &text[text.find(&format!("<key>{key}</key>"))?..];
    let rest = &rest[rest.find("<string>")? + "<string>".len()..];
    let value = &rest[..rest.find("</string>")?];
    (!value.is_empty()).then(|| value.to_owned())
}

/// The terminal emulator's name and version, from the environment.  `$LC_TERMINAL` is iTerm2's
/// own marker, which — unlike `$TERM_PROGRAM` — survives ssh.  Windows Terminal sets neither and is
/// named from `$WT_SESSION`, without a version.  kitty, alacritty, and foot set none of these and
/// read as unset.
///
/// This is exactly the pair `Capabilities::fingerprint` omits so a version bump can't re-trigger
/// the new-terminal notice; here the version is the point.
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
        (None, _) if is_windows_terminal() => "Windows Terminal".to_owned(),
        (None, _) => "unset (TERM_PROGRAM, LC_TERMINAL)".to_owned(),
    }
}

/// The active locale and which variable supplied it — the same lookup the Unicode row is decided
/// by, so a puzzling row can be traced to its variable.
fn locale() -> String {
    locale_report(active_locale(), cfg!(windows))
}

/// Formats [`locale`]'s finding.  On native Windows the Unicode row comes from the UTF-16 console,
/// so a locale set there (Git Bash / MSYS sets `LANG`) is shown but marked as unused.
fn locale_report(found: Option<(&str, String)>, windows: bool) -> String {
    match (found, windows) {
        (Some((var, v)), false) => format!("{v} ({var})"),
        (Some((var, v)), true) => format!("{v} ({var}; unused on Windows)"),
        (None, false) => "unset (LC_ALL, LC_CTYPE, LANG)".to_owned(),
        (None, true) => "n/a (Windows console is UTF-16)".to_owned(),
    }
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

    /// Every test calling [`report`] takes the crate-wide env lock — the System section reads
    /// environment variables that other tests in this binary write.
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

    /// The capability half must be the summary builder's text verbatim, or the two surfaces drift.
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

    /// Just the `Terminal capabilities` rows: marker assertions must not see the `System`
    /// section, whose live-environment values can themselves contain a `?`.
    fn capability_section(text: &str) -> String {
        let (_, caps) = text
            .split_once("\nTerminal capabilities\n")
            .expect("capabilities section");
        caps.to_owned()
    }

    #[test]
    fn every_row_is_marked_ok_or_warn_when_probed() {
        let _lock = crate::test_env::env_lock();
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

    /// Without a live terminal the probe-derived rows must read unknown, not failed.
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
        // A malformed body must degrade to None, not panic on a slice.
        assert_eq!(
            parse_plist_value("<key>ProductVersion</key>", "ProductVersion"),
            None
        );
    }

    /// Every system fact must resolve to *something*: a blank value is worse than "unknown".
    #[test]
    fn no_system_fact_is_ever_blank() {
        let _lock = crate::test_env::env_lock();
        for (label, value) in system_facts() {
            assert!(!value.trim().is_empty(), "{label} resolved to nothing");
        }
    }

    /// The report is pasted into a public issue tracker, so no fact may carry a home-relative
    /// path.  The config-directory row was removed for exactly this reason.
    #[test]
    fn no_system_fact_leaks_the_home_directory() {
        let Some(home) = dirs::home_dir() else { return };
        let home = home.display().to_string();
        // A degenerate `/` home would match everything.
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

    /// Windows Terminal sets no `$TERM_PROGRAM`; the report must still name it.
    #[test]
    fn windows_terminal_is_named_from_wt_session() {
        use crate::test_env::{env_lock, EnvGuard};
        let _lock = env_lock();
        let _g1 = EnvGuard::unset("TERM_PROGRAM");
        let _g2 = EnvGuard::unset("LC_TERMINAL");
        let _g3 = EnvGuard::unset("TMUX");
        let _g4 = EnvGuard::unset("STY");
        let _g5 = EnvGuard::set("WT_SESSION", "5a5c6f4e-0000-4000-8000-000000000000");
        assert_eq!(terminal_program(), "Windows Terminal");
        let _g6 = EnvGuard::set("TERM_PROGRAM", "WezTerm");
        assert!(terminal_program().starts_with("WezTerm"));
    }

    #[test]
    fn unnamed_terminal_says_which_variables_were_missing() {
        use crate::test_env::{env_lock, EnvGuard};
        let _lock = env_lock();
        let _g1 = EnvGuard::unset("TERM_PROGRAM");
        let _g2 = EnvGuard::unset("LC_TERMINAL");
        let _g3 = EnvGuard::unset("WT_SESSION");
        assert_eq!(terminal_program(), "unset (TERM_PROGRAM, LC_TERMINAL)");
    }

    #[test]
    fn locale_on_windows_is_shown_but_marked_unused() {
        let lang = || Some(("LANG", "en_US.UTF-8".to_owned()));
        assert_eq!(locale_report(lang(), false), "en_US.UTF-8 (LANG)");
        assert_eq!(
            locale_report(lang(), true),
            "en_US.UTF-8 (LANG; unused on Windows)"
        );
        assert_eq!(locale_report(None, false), "unset (LC_ALL, LC_CTYPE, LANG)");
        assert!(locale_report(None, true).starts_with("n/a"));
    }

    /// An absent variable is a fact, not a gap in the report.
    #[test]
    fn absent_and_empty_variables_read_as_unset() {
        use std::env::VarError;
        assert_eq!(env_value_or_unset(Ok("xterm".into())), "xterm");
        assert_eq!(env_value_or_unset(Ok(String::new())), "unset");
        assert_eq!(env_value_or_unset(Err(VarError::NotPresent)), "unset");
        assert_eq!(
            env_value_or_unset(Err(VarError::NotUnicode(Default::default()))),
            UNKNOWN
        );
    }
}
