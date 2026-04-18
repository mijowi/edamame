//! Terminal capability detection.
//!
//! Phase 4 probes the terminal for features the editor uses or will use in
//! later phases: colour depth, mouse support, the image protocol supported
//! by the emulator (sixel / kitty / iterm2 / halfblocks), whether the locale
//! advertises full Unicode support, and whether the kitty keyboard
//! enhancement protocol is available.
//!
//! Detection uses three signals:
//!
//! * Environment variables (`$TERM`, `$COLORTERM`, `$TERM_PROGRAM`,
//!   `$KITTY_WINDOW_ID`, `$LC_ALL`, `$LANG`, etc.).
//! * Crossterm's `supports_keyboard_enhancement()` for keyboard protocol.
//! * `ratatui_image::picker::Picker::from_query_stdio` for image protocol.
//!
//! The `detect` function is designed to never panic and never block the UI
//! for noticeable time: all probes either complete in a few milliseconds or
//! gracefully fall back to a conservative default.

use std::env;

/// Colour bit-depth supported by the terminal.
///
/// Values are ordered from poorest to richest so comparisons like
/// `depth >= ColourDepth::Ansi256` work as expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColourDepth {
    /// Terminal advertises no colour support (e.g. `TERM=dumb`).  Rendering
    /// falls back to plain text with no ANSI style escapes.
    NoColour,
    /// Classic 8/16-colour palette.
    Ansi16,
    /// 256-indexed colour palette (xterm-256color and friends).
    Ansi256,
    /// 24-bit / true-colour palette (`COLORTERM=truecolor` or `24bit`).
    TrueColor,
}

/// Image protocol supported by the terminal emulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    /// DEC Sixel graphics (xterm with `--enable-sixel-graphics`, foot, wezterm).
    Sixel,
    /// Kitty graphics protocol (kitty, ghostty, wezterm).
    KittyGraphics,
    /// iTerm2 inline-images protocol.
    ITerm2,
    /// Unicode half-block fallback (works in any truecolour terminal).
    Halfblocks,
}

/// Detected terminal capabilities.
#[derive(Debug, Clone)]
pub struct Capabilities {
    /// Colour bit-depth the terminal advertises.
    pub colour_depth: ColourDepth,
    /// Whether the terminal appears to support mouse reporting.
    pub mouse: bool,
    /// Image protocol detected by `ratatui-image`, or `None` when image display
    /// is not supported.
    pub image_protocol: Option<ImageProtocol>,
    /// Whether `$LC_ALL` / `$LC_CTYPE` / `$LANG` advertise a UTF-8 locale.
    /// Used as a proxy for "full Unicode support".
    pub unicode_full: bool,
    /// Whether `PushKeyboardEnhancementFlags` succeeded.  When `true`,
    /// `Ctrl-Shift-Z` is usable as a secondary redo binding; when `false`
    /// the terminal cannot disambiguate shifted modifier combinations and
    /// features that rely on them must gracefully degrade.
    pub keyboard_enhancement: bool,
}

impl Capabilities {
    /// Detect all capabilities.
    ///
    /// Must be called **after** the terminal has entered the alternate screen
    /// and raw mode, because `ratatui_image`'s Picker probes stdout/stdin
    /// with escape sequences.  The `kbd_enhancement` flag should be the
    /// result of the `PushKeyboardEnhancementFlags` call in `setup()` — we
    /// pass it in rather than re-querying here so the source of truth is the
    /// actual push operation, not a separate capability probe (which can
    /// disagree with reality on some terminals).
    pub fn detect(kbd_enhancement: bool) -> Self {
        let term = env::var("TERM").unwrap_or_default();
        let colour_depth = detect_colour_depth(&term);
        let mouse = detect_mouse(&term);
        let unicode_full = detect_unicode_full();
        let image_protocol = detect_image_protocol();

        Self {
            colour_depth,
            mouse,
            image_protocol,
            unicode_full,
            keyboard_enhancement: kbd_enhancement,
        }
    }

    /// Conservative default used by tests and when probing is impossible.
    ///
    /// Assumes the minimum-common-denominator terminal: 16 colours, no mouse,
    /// no images, no kitty keyboard protocol.
    pub fn minimal() -> Self {
        Self {
            colour_depth: ColourDepth::Ansi16,
            mouse: false,
            image_protocol: None,
            unicode_full: false,
            keyboard_enhancement: false,
        }
    }

    /// Returns true when the user's terminal is missing at least one feature
    /// the editor would otherwise light up (mouse, colour, image support,
    /// kitty keyboard protocol).  The UI uses this to decide whether to show
    /// a one-time notice at startup.
    ///
    /// `Ansi16` terminals are *not* considered missing a feature — the editor
    /// is fully usable in 16 colours, the theme just looks muted.  We only
    /// trigger the notice for `NoColour`, where style escapes are stripped
    /// entirely.
    pub fn has_missing_features(&self) -> bool {
        !self.mouse
            || self.colour_depth == ColourDepth::NoColour
            || self.image_protocol.is_none()
            || !self.keyboard_enhancement
    }

    /// Human-readable summary of the missing features (for display in the
    /// startup notice modal).
    pub fn missing_features_summary(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.mouse {
            out.push("Mouse reporting not available.".to_owned());
        }
        match self.colour_depth {
            ColourDepth::NoColour => {
                out.push("No colour support — falling back to plain text.".to_owned())
            }
            ColourDepth::Ansi16 => {
                out.push("Only 16 colours available — themes will look muted.".to_owned())
            }
            _ => {}
        }
        if self.image_protocol.is_none() {
            out.push("No image protocol detected — images will render as placeholders.".to_owned());
        }
        if !self.keyboard_enhancement {
            out.push(
                "Kitty keyboard protocol unavailable — Ctrl-Shift-Z redo and \
                 Alt+Shift+Arrow table insert/delete bindings are disabled."
                    .to_owned(),
            );
        }
        out
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::minimal()
    }
}

// ── Probing helpers ──────────────────────────────────────────────────────────

/// Infer colour depth from environment variables.
///
/// `$COLORTERM` takes precedence when it names a true-colour terminal; failing
/// that we fall back to inspecting `$TERM` for the conventional `-256color`
/// suffix, then to the built-in 8/16-colour palette.
fn detect_colour_depth(term: &str) -> ColourDepth {
    if term == "dumb" || term.is_empty() {
        return ColourDepth::NoColour;
    }
    let colorterm = env::var("COLORTERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if colorterm == "truecolor" || colorterm == "24bit" {
        return ColourDepth::TrueColor;
    }
    if term.contains("direct") {
        // e.g. `xterm-direct`, `tmux-direct`.
        return ColourDepth::TrueColor;
    }
    // A handful of modern terminals are known to support truecolour even when
    // `$COLORTERM` is unset (e.g. a remote session that stripped it).
    if env::var("KITTY_WINDOW_ID").is_ok() || env::var("WEZTERM_PANE").is_ok() {
        return ColourDepth::TrueColor;
    }
    if let Ok(tp) = env::var("TERM_PROGRAM") {
        match tp.as_str() {
            "iTerm.app" | "Apple_Terminal" | "WezTerm" | "ghostty" | "Ghostty" => {
                return ColourDepth::TrueColor
            }
            _ => {}
        }
    }
    if term.contains("256color") {
        return ColourDepth::Ansi256;
    }
    ColourDepth::Ansi16
}

/// Infer mouse support from `$TERM`.
///
/// Essentially every post-1990s xterm-compatible terminal supports xterm
/// mouse reporting.  The short list of exceptions is: `dumb`, empty, and
/// `linux` (the Linux framebuffer console has no mouse by default).
fn detect_mouse(term: &str) -> bool {
    if term == "dumb" || term == "linux" || term.is_empty() {
        return false;
    }
    true
}

/// True when the active locale advertises UTF-8.
fn detect_unicode_full() -> bool {
    for var in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Ok(v) = env::var(var) {
            let upper = v.to_ascii_uppercase();
            if upper.contains("UTF-8") || upper.contains("UTF8") {
                return true;
            }
        }
    }
    false
}

/// Ask `ratatui_image` to probe for an image protocol.  Returns `None` when
/// probing fails or the terminal advertises only halfblock support.
///
/// Halfblocks are reported as `Some(ImageProtocol::Halfblocks)` — they are
/// still a usable protocol, just lower-fidelity than sixel/kitty/iterm2.
fn detect_image_protocol() -> Option<ImageProtocol> {
    use ratatui_image::picker::{Picker, ProtocolType};

    // Picker::from_query_stdio may write escape sequences; a panic here would
    // be disastrous (corrupted terminal state), so we catch-and-swallow.
    let result = std::panic::catch_unwind(Picker::from_query_stdio);
    let picker = match result {
        Ok(Ok(p)) => p,
        _ => return None,
    };
    Some(match picker.protocol_type() {
        ProtocolType::Sixel => ImageProtocol::Sixel,
        ProtocolType::Kitty => ImageProtocol::KittyGraphics,
        ProtocolType::Iterm2 => ImageProtocol::ITerm2,
        ProtocolType::Halfblocks => ImageProtocol::Halfblocks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Save the current value of an env var, set a new value (or clear it),
    /// and restore on drop.  Lets tests mutate env vars without races — we
    /// just need to be careful to not run colour-depth tests in parallel,
    /// which cargo does by default per test binary.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = env::var(key).ok();
            // SAFETY: this test module is serialised by the `env_mutex` below.
            unsafe {
                env::set_var(key, value);
            }
            Self { key, prev }
        }

        fn unset(key: &'static str) -> Self {
            let prev = env::var(key).ok();
            unsafe {
                env::remove_var(key);
            }
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prev {
                    Some(v) => env::set_var(self.key, v),
                    None => env::remove_var(self.key),
                }
            }
        }
    }

    /// Serialise env-var-mutating tests so they don't clobber each other.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::Mutex;
        use std::sync::OnceLock;
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        M.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn colour_depth_no_colour_for_dumb_terminal() {
        assert_eq!(detect_colour_depth("dumb"), ColourDepth::NoColour);
        assert_eq!(detect_colour_depth(""), ColourDepth::NoColour);
    }

    #[test]
    fn colour_depth_truecolor_from_colorterm() {
        let _lock = env_lock();
        let _g1 = EnvGuard::set("COLORTERM", "truecolor");
        let _g2 = EnvGuard::unset("KITTY_WINDOW_ID");
        let _g3 = EnvGuard::unset("WEZTERM_PANE");
        let _g4 = EnvGuard::unset("TERM_PROGRAM");
        assert_eq!(
            detect_colour_depth("xterm-256color"),
            ColourDepth::TrueColor
        );
    }

    #[test]
    fn colour_depth_256_from_term_suffix() {
        let _lock = env_lock();
        let _g1 = EnvGuard::unset("COLORTERM");
        let _g2 = EnvGuard::unset("KITTY_WINDOW_ID");
        let _g3 = EnvGuard::unset("WEZTERM_PANE");
        let _g4 = EnvGuard::unset("TERM_PROGRAM");
        assert_eq!(detect_colour_depth("xterm-256color"), ColourDepth::Ansi256);
    }

    #[test]
    fn colour_depth_16_for_plain_xterm() {
        let _lock = env_lock();
        let _g1 = EnvGuard::unset("COLORTERM");
        let _g2 = EnvGuard::unset("KITTY_WINDOW_ID");
        let _g3 = EnvGuard::unset("WEZTERM_PANE");
        let _g4 = EnvGuard::unset("TERM_PROGRAM");
        assert_eq!(detect_colour_depth("xterm"), ColourDepth::Ansi16);
    }

    #[test]
    fn colour_depth_truecolor_for_kitty_envvar() {
        let _lock = env_lock();
        let _g1 = EnvGuard::unset("COLORTERM");
        let _g2 = EnvGuard::set("KITTY_WINDOW_ID", "1");
        let _g3 = EnvGuard::unset("WEZTERM_PANE");
        let _g4 = EnvGuard::unset("TERM_PROGRAM");
        assert_eq!(detect_colour_depth("xterm"), ColourDepth::TrueColor);
    }

    #[test]
    fn mouse_false_for_dumb_and_linux() {
        assert!(!detect_mouse("dumb"));
        assert!(!detect_mouse("linux"));
        assert!(!detect_mouse(""));
    }

    #[test]
    fn mouse_true_for_modern_terminals() {
        assert!(detect_mouse("xterm-256color"));
        assert!(detect_mouse("alacritty"));
        assert!(detect_mouse("tmux-256color"));
    }

    #[test]
    fn unicode_full_from_lang() {
        let _lock = env_lock();
        let _g1 = EnvGuard::set("LANG", "en_US.UTF-8");
        let _g2 = EnvGuard::unset("LC_ALL");
        let _g3 = EnvGuard::unset("LC_CTYPE");
        assert!(detect_unicode_full());
    }

    #[test]
    fn unicode_false_for_c_locale() {
        let _lock = env_lock();
        let _g1 = EnvGuard::set("LANG", "C");
        let _g2 = EnvGuard::unset("LC_ALL");
        let _g3 = EnvGuard::unset("LC_CTYPE");
        assert!(!detect_unicode_full());
    }

    #[test]
    fn minimal_capabilities_are_conservative() {
        let caps = Capabilities::minimal();
        assert_eq!(caps.colour_depth, ColourDepth::Ansi16);
        assert!(!caps.mouse);
        assert!(caps.image_protocol.is_none());
        assert!(!caps.unicode_full);
        assert!(!caps.keyboard_enhancement);
        // Minimal configuration is "missing features" by definition.
        assert!(caps.has_missing_features());
    }

    #[test]
    fn ansi16_alone_does_not_trigger_missing_features_notice() {
        let caps = Capabilities {
            colour_depth: ColourDepth::Ansi16,
            mouse: true,
            image_protocol: Some(ImageProtocol::KittyGraphics),
            unicode_full: true,
            keyboard_enhancement: true,
        };
        assert!(!caps.has_missing_features());
    }

    #[test]
    fn no_colour_triggers_missing_features_notice() {
        let caps = Capabilities {
            colour_depth: ColourDepth::NoColour,
            mouse: true,
            image_protocol: Some(ImageProtocol::KittyGraphics),
            unicode_full: true,
            keyboard_enhancement: true,
        };
        assert!(caps.has_missing_features());
    }

    #[test]
    fn missing_features_summary_is_empty_when_everything_is_supported() {
        let caps = Capabilities {
            colour_depth: ColourDepth::TrueColor,
            mouse: true,
            image_protocol: Some(ImageProtocol::KittyGraphics),
            unicode_full: true,
            keyboard_enhancement: true,
        };
        assert!(!caps.has_missing_features());
        assert!(caps.missing_features_summary().is_empty());
    }

    #[test]
    fn missing_features_summary_names_each_missing_capability() {
        let caps = Capabilities::minimal();
        let summary = caps.missing_features_summary();
        assert!(!summary.is_empty());
        // All four "missing" messages should appear.
        assert!(summary.iter().any(|s| s.contains("Mouse")));
        assert!(summary
            .iter()
            .any(|s| s.contains("colours") || s.contains("colour")));
        assert!(summary.iter().any(|s| s.contains("image")));
        assert!(summary.iter().any(|s| s.contains("Kitty")));
    }
}
