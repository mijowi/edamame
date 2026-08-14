//! The single "is `~/.config/edamame` in play at all?" gate.
//!
//! `--no-config` promises a session that neither reads nor writes
//! `~/.config/edamame`.  Both halves are one fact about this invocation,
//! so both come from **one** flag that every read and write site asks,
//! rather than a value threaded through the data.
//!
//! It lives in a process-global `AtomicBool` because that is what the
//! fact actually is: a property of *this invocation*, decided once from
//! the command line before the first config file is touched, and true
//! for every site for the rest of the process.  An earlier design
//! carried it as a `Config` field, which failed the moment
//! `App::open_config_in_editor` replaced `self.config` with a freshly
//! deserialized one — the flag reverted to its serde default and the
//! guarantee silently lapsed mid-session.  A global cannot be
//! overwritten by a reload.
//!
//! **Writes.**  Four sites write into the config directory, and all four
//! ask [`config_writes_allowed`]:
//!
//! - [`Config::save`](super::config::Config::save) — `config.toml`
//! - the keybinds overlay's `[ Save ]` — `keybindings.toml`
//! - the export-theme modal — `themes/<name>.toml`
//! - `App::open_config_in_editor`, which both seeds `config.toml` and
//!   reloads from it
//!
//! `Config::ensure_default_files` is the one exception, and only because
//! `main` never calls it under `--no-config` in the first place.
//!
//! **Reads.**  Skipping the startup load is *not* enough on its own: the
//! config directory is read again mid-session by surfaces that enumerate
//! what the user has dropped into it, and those run long after `main` has
//! made its branch.  Three sites ask [`config_reads_allowed`]:
//!
//! - [`list_theme_names`](super::theme::list_theme_names) — the theme
//!   picker, the settings overlay's theme cycle, and the export-theme
//!   source list all build from it, so a `--no-config` run would
//!   otherwise still offer (and load) `themes/*.toml`
//! - [`list_export_stylesheets`](super::readers::list_export_stylesheets)
//!   — the HTML-export stylesheet list
//! - `read_theme_named`, the load path behind those names
//!
//! A new reader or writer owes the matching check.

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether `~/.config/edamame` participates in this run at all.  Starts
/// `true`; only [`disable_config_dir`] ever clears it, and nothing sets
/// it back — `--no-config` is a property of the whole invocation.
static CONFIG_DIR_IN_USE: AtomicBool = AtomicBool::new(true);

/// What a "saved" message says instead when the write was suppressed.
///
/// The setting *is* live for the session — only the disk write was
/// skipped — so the wording says exactly that.  A single const because
/// three different flashes need to say it identically.
pub const NOT_PERSISTED_NOTE: &str = " (not saved: --no-config)";

/// Take `~/.config/edamame` out of play for the rest of the process, in
/// both directions.
///
/// Called once from `main` when `--no-config` is present, before any
/// config file is read or written.  There is deliberately no way to
/// re-enable: a mid-session reversal is the bug this design exists to
/// prevent.
pub fn disable_config_dir() {
    CONFIG_DIR_IN_USE.store(false, Ordering::Relaxed);
}

/// The write half of the gate: may the caller write into
/// `~/.config/edamame`?
///
/// `Relaxed` is sufficient — the value is written once at startup on the
/// main thread, long before any other thread exists, and every later
/// access is a read.
pub fn config_writes_allowed() -> bool {
    CONFIG_DIR_IN_USE.load(Ordering::Relaxed)
}

/// The read half of the gate: may the caller read from
/// `~/.config/edamame`?
///
/// Separate from [`config_writes_allowed`] for the call site's sake —
/// the two read the same flag, and always will, but a reader guarded by
/// a function with "writes" in its name reads as a mistake.
pub fn config_reads_allowed() -> bool {
    CONFIG_DIR_IN_USE.load(Ordering::Relaxed)
}

/// Suffix for a flash that reports a settings change: empty on an
/// ordinary run, [`NOT_PERSISTED_NOTE`] when writes are suppressed.
///
/// Callers phrase the sentence so that both readings are true — the
/// change took effect either way, and only the persistence differs.
pub fn unpersisted_suffix() -> &'static str {
    if config_writes_allowed() {
        ""
    } else {
        NOT_PERSISTED_NOTE
    }
}

/// Test-only scoped suppression.
///
/// The gate is process-global, so a test that flips it must exclude
/// every other test that could touch config *or* read the gate — and
/// that exclusion has to outlast the guard itself, because the usual
/// shape of these tests is "assert nothing was written, then drop the
/// guard and assert the same call *does* write".  The second half runs
/// with the gate back at `true` and would fail if another test's guard
/// were live at that moment.
///
/// So the serialization is [`crate::test_env::env_lock`], the crate-wide
/// lock those tests already hold for the whole test body (they point
/// `XDG_CONFIG_HOME` at a tempdir to prove nothing was written), rather
/// than a second mutex of this module's own.  One lock also means there
/// is no lock *order* to get wrong.
///
/// **Callers must hold `env_lock` for the whole test**, not just across
/// the guard's lifetime.
#[cfg(test)]
pub(crate) struct SuppressGuard;

#[cfg(test)]
impl SuppressGuard {
    /// Suppress config reads and writes until the returned guard drops.
    /// Only valid while holding [`crate::test_env::env_lock`].
    pub(crate) fn new() -> Self {
        CONFIG_DIR_IN_USE.store(false, Ordering::Relaxed);
        Self
    }
}

#[cfg(test)]
impl Drop for SuppressGuard {
    fn drop(&mut self) {
        CONFIG_DIR_IN_USE.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Takes the crate-wide env lock for the same reason every other
    /// gate test does: it reads the global that those tests flip.
    #[test]
    fn reads_and_writes_are_allowed_by_default() {
        let _lock = crate::test_env::env_lock();
        assert!(config_writes_allowed());
        assert!(config_reads_allowed());
        assert_eq!(unpersisted_suffix(), "");
    }

    /// The guard both suppresses and restores — the restore matters as
    /// much as the suppression, since every other test in the binary
    /// runs against the same global.
    #[test]
    fn the_guard_suppresses_and_restores_both_halves() {
        let _lock = crate::test_env::env_lock();
        {
            let _g = SuppressGuard::new();
            assert!(!config_writes_allowed());
            assert!(!config_reads_allowed());
            assert_eq!(unpersisted_suffix(), NOT_PERSISTED_NOTE);
        }
        assert!(config_writes_allowed());
        assert!(config_reads_allowed());
    }
}
