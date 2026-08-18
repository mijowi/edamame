//! GitHub latest-release check: is a newer edamame out, and what
//! changed in it?
//!
//! Runs on two triggers that share one session cache
//! (`App::latest_release`):
//!
//! * **automatically at startup**, at most once per
//!   [`policy::CHECK_INTERVAL_SECS`], and only when
//!   `editor.check_for_updates` is on.  It is silent unless it finds a
//!   release newer than this build that the user has not already been
//!   told about, in which case `app::update_notice` raises a modal —
//!   but not until the modal stack is clear, so it can never land on
//!   top of the first-run welcome or a config warning.
//! * **on request**, from the About page's `[ Check for updates ]`
//!   button or the `CheckForUpdates` command-palette action.  An
//!   explicit request always re-fetches; it reports "up to date" and
//!   network failures too, which the startup path deliberately does
//!   not.
//!
//! The split across submodules follows what each part needs to be
//! trusted about: [`fetch`] is the only part that touches the network,
//! [`parse`] is the only part that touches remote bytes (and bounds
//! them before anything else sees them), [`policy`] is pure decisions
//! over primitives, and [`status`] is the domain vocabulary the `ui`
//! and `app` layers speak.  `ui` never sees any of these types — the
//! modal adapter hands `ui::update_check` plain values, the same rule
//! `ui::about` documents.

pub(crate) mod fetch;
pub(crate) mod parse;
pub(crate) mod policy;
pub(crate) mod status;

pub(crate) use fetch::{release_url, spawn_release_check, GITHUB_URL};
pub(crate) use policy::{network_check_due, notice_due, now_unix};
pub(crate) use status::{ReleaseInfo, ReleaseStatus, INSTALLED_VERSION};
