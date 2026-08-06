//! Indexed-color theme substitution — the one place that decides a
//! session's theme must be swapped because the terminal can't render
//! 24-bit color, shared by the two paths that resolve a theme from a
//! freshly-read `Config`: [`crate::app::App::new`] at startup and the
//! external-editor reload in [`crate::app::external_editor`].
//!
//! Keeping it here (rather than inline in `App::new`) is what stops the
//! reload path from quietly undoing the swap: `Config::load` returns the
//! user's on-disk theme every time, so without re-applying, exiting
//! `$EDITOR` would repaint the session in the unreadable palette.
//!
//! See [`crate::app::modal::ThemeDowngradeModal`] for what the user is
//! told, and `Config::theme_downgraded_from` for how the substitution is
//! kept out of `config.toml`.

use crate::config::theme::indexed_fallback_theme;
use crate::config::{Config, ThemeFile};
use crate::terminal::{Capabilities, ColorDepth};

/// A substitution that fired.  `configured` is the user's own theme
/// (already stashed in `Config::theme_downgraded_from`); `substituted`
/// is the indexed-color built-in now in `Config::theme`.
pub(super) struct Downgrade {
    pub theme_file: ThemeFile,
    pub configured: String,
    pub substituted: &'static str,
}

/// Swap `config.theme` for an indexed-color built-in when `caps` lacks
/// 24-bit color, stashing the user's choice.  Returns the `ThemeFile`
/// the caller should render with, or `None` when no swap is needed —
/// on a truecolor terminal, on a colorless one, or when the configured
/// theme is already in `theme::INDEXED_SAFE_THEMES` (which is also what
/// keeps the swap idempotent across reloads).
///
/// Because `App::new` derives both the substitution and the modal from
/// this one `Option`, returning `None` suppresses the warning and the
/// swap together — there is no second predicate to keep in sync.
pub(super) fn apply(config: &mut Config, caps: &Capabilities) -> Option<Downgrade> {
    // A `NoColor` terminal is already handled further down: `App::new`
    // passes `monochrome` to `Theme::from_file`, which strips every
    // color regardless of which theme is active.  Swapping would change
    // nothing the user can see, so the modal explaining it would be pure
    // noise on the weakest terminals we support.
    if caps.full_color() || caps.color_depth == ColorDepth::NoColor {
        return None;
    }
    let substituted = indexed_fallback_theme(&config.theme, config.appearance)?;
    // Built-in names short-circuit ahead of any disk read, so the
    // `truecolor` argument can't matter here.
    let (theme_file, _) = Config::load_theme(substituted, false);
    let configured = config.theme.clone();
    config.theme_downgraded_from = Some(configured.clone());
    config.theme = substituted.to_owned();
    Some(Downgrade {
        theme_file,
        configured,
        substituted,
    })
}
