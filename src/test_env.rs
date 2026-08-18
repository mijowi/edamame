//! Test-only helpers for mutating process environment variables.
//!
//! **Crate-wide, deliberately.**  `std::env::set_var` is `unsafe` because
//! it races any concurrent `env::var` in the same process — and cargo runs
//! every test in a binary on parallel threads.  A per-module lock is
//! therefore not enough: `config::config` mutates `XDG_CONFIG_HOME` while
//! `cli::doctor` reads it, and `terminal::capabilities` mutates
//! `TERM_PROGRAM` while `cli::doctor` reads that too.  One lock shared by
//! the whole crate is what actually excludes them.
//!
//! Two rules for any test that touches the environment:
//!
//! 1. Take [`env_lock`] first, and hold it for the whole test.
//! 2. Mutate only through [`EnvGuard`], so the variable is restored even
//!    if an assertion panics — a leaked `XDG_CONFIG_HOME` pointing at a
//!    deleted `tempdir` fails every later config test for no reason.
//!
//! A test that only *reads* the environment must also take the lock; it
//! is the read side of the same race.
//!
//! The same lock also serialises [`config::persistence::SuppressGuard`],
//! which flips a process-global `AtomicBool` rather than an environment
//! variable — and the read side of *that* race is easy to miss, because
//! such a test touches no environment variable of its own.  Any test
//! that observes `config_reads_allowed` / `config_writes_allowed` — which
//! means any test calling `list_export_stylesheets`, `list_theme_names`,
//! `read_theme_named`, or `Config::save` — must take the lock too, or it
//! reads the gate while a suppressing test on another thread holds it
//! closed and sees an empty list it can't explain.
//!
//! [`config::persistence::SuppressGuard`]: crate::config::persistence::SuppressGuard

use std::env;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Serialises every environment-touching test in the crate.
///
/// Poisoning is ignored: [`EnvGuard`]'s `Drop` has already restored the
/// variable by the time a panicking test releases the lock, so there is
/// no corrupt state for the next holder to inherit.
pub fn env_lock() -> MutexGuard<'static, ()> {
    static M: OnceLock<Mutex<()>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Sets (or clears) one environment variable and restores its previous
/// value on drop.  Create it only while holding [`env_lock`].
pub struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    /// Set `key` to `value` for the guard's lifetime.
    pub fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prev = env::var(key).ok();
        // SAFETY: every env-mutating and env-reading test in this crate
        // holds `env_lock`, so no other thread is in `env::var` here.
        unsafe {
            env::set_var(key, value);
        }
        Self { key, prev }
    }

    /// Remove `key` for the guard's lifetime.
    pub fn unset(key: &'static str) -> Self {
        let prev = env::var(key).ok();
        // SAFETY: as above.
        unsafe {
            env::remove_var(key);
        }
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: as above — the lock is still held by the test whose
        // scope this guard is ending.
        unsafe {
            match &self.prev {
                Some(v) => env::set_var(self.key, v),
                None => env::remove_var(self.key),
            }
        }
    }
}

/// The crate-wide env lock plus suppressed config reads and writes, as
/// one guard.
///
/// A test that drives a code path calling [`Config::save`] must hold
/// this.  Nothing in the test environment redirects
/// `~/.config/edamame` by default, so an unguarded `save()` rewrites the
/// *developer's own* config — and a value asserted in a test is exactly
/// the kind of value that does damage there (an update-check test
/// recording a `v999.0.0` tag suppresses the real update notice
/// forever).  Suppressing the gate is preferable to pointing
/// `XDG_CONFIG_HOME` at a tempdir: the write is what the test wants
/// gone, not merely relocated, and `Config::save` already returns
/// `Ok(())` under it, so no assertion has to know.
///
/// Reads are suppressed alongside writes because the gate is one flag —
/// see [`crate::config::persistence`].  A test that needs a real write
/// (`save_writes_nothing_while_config_writes_are_suppressed`) takes
/// [`env_lock`] and an [`EnvGuard`] on `XDG_CONFIG_HOME` instead.
pub struct ConfigIsolation {
    // Declaration order is drop order: the suppression must be lifted
    // while the lock is still held, or another test observes the gate
    // mid-restore.
    _suppress: crate::config::persistence::SuppressGuard,
    _lock: MutexGuard<'static, ()>,
}

/// Take [`ConfigIsolation`] for the rest of the current scope.  Hold it
/// for the whole test body, and take it only once — it is a mutex.
pub fn config_isolation() -> ConfigIsolation {
    let lock = env_lock();
    ConfigIsolation {
        _suppress: crate::config::persistence::SuppressGuard::new(),
        _lock: lock,
    }
}
