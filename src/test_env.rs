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
