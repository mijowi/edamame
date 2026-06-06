//! Terminal capability detection.
//!
//! Probes the terminal for features the editor uses: color depth, mouse
//! support, the image protocol supported
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

use ratatui_image::picker::Picker;

/// Color bit-depth supported by the terminal.
///
/// Values are ordered from poorest to richest so comparisons like
/// `depth >= ColorDepth::Ansi256` work as expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorDepth {
    /// Terminal advertises no color support (e.g. `TERM=dumb`).  Rendering
    /// falls back to plain text with no ANSI style escapes.
    NoColor,
    /// Classic 8/16-color palette.
    Ansi16,
    /// 256-indexed color palette (xterm-256color and friends).
    Ansi256,
    /// 24-bit / true-color palette (`COLORTERM=truecolor` or `24bit`).
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
    /// Unicode half-block fallback (works in any truecolor terminal).
    Halfblocks,
}

/// Detected terminal capabilities.
#[derive(Debug, Clone)]
pub struct Capabilities {
    /// Color bit-depth the terminal advertises.
    pub color_depth: ColorDepth,
    /// Whether the terminal appears to support mouse reporting.
    pub mouse: bool,
    /// Image protocol detected by `ratatui-image`, or `None` when image display
    /// is not supported.
    pub image_protocol: Option<ImageProtocol>,
    /// The `Picker` instance returned by `ratatui_image`'s startup probe.
    /// Retained so image rendering can reuse the already-probed
    /// configuration instead of re-running `Picker::from_query_stdio` on
    /// every cold image load.  `None` iff `image_protocol` is also `None`.
    pub image_picker: Option<Picker>,
    /// A second `Picker` forced to `ProtocolType::Halfblocks`, built from
    /// the same font-size reported by the native picker (no extra stdin
    /// probe).  Used by the halfblocks-fallback path: during
    /// partial visibility or active scrolling on non-Kitty terminals, we
    /// render via halfblocks (which is position-independent and cheap to
    /// cell-copy) and upgrade back to the native protocol once the image
    /// is fully visible and scroll has quiesced.  `None` iff image
    /// support is absent altogether.
    pub halfblocks_picker: Option<Picker>,
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
        let color_depth = detect_color_depth(&term);
        let mouse = detect_mouse(&term);
        let unicode_full = detect_unicode_full();
        let (image_protocol, image_picker) = detect_image_protocol();
        // Reuse the native picker's font_size so the halfblocks
        // encoding renders at the same pixel-to-cell aspect ratio as
        // the native one — crucial for images that cross the
        // native↔halfblocks boundary during scroll.  `from_fontsize`
        // is deprecated in ratatui-image 9 but `Picker::halfblocks()`
        // hardcodes font_size to (10, 20), which is wrong for any
        // non-default terminal; we need the probed value here.
        #[allow(deprecated)]
        let halfblocks_picker = image_picker
            .as_ref()
            .map(|p| Picker::from_fontsize(p.font_size()));

        Self {
            color_depth,
            mouse,
            image_protocol,
            image_picker,
            halfblocks_picker,
            unicode_full,
            keyboard_enhancement: kbd_enhancement,
        }
    }

    /// Probe the terminal's color depth from environment variables
    /// only — no escape-sequence I/O.  Safe to call before
    /// [`terminal::setup`] (and therefore before the full
    /// [`Self::detect`]), so the config loader can pick a
    /// capability-appropriate fallback theme when the active theme
    /// file is missing.  The full probe is the source of truth for
    /// every other consumer.
    pub fn detect_color_depth_from_env() -> ColorDepth {
        let term = env::var("TERM").unwrap_or_default();
        detect_color_depth(&term)
    }

    /// Conservative default used by tests and when probing is impossible.
    ///
    /// Assumes the minimum-common-denominator terminal: 16 colors, no mouse,
    /// no images, no kitty keyboard protocol.
    pub fn minimal() -> Self {
        Self {
            color_depth: ColorDepth::Ansi16,
            mouse: false,
            image_protocol: None,
            image_picker: None,
            halfblocks_picker: None,
            unicode_full: false,
            keyboard_enhancement: false,
        }
    }

    /// Build a stable, debuggable fingerprint that identifies this terminal
    /// "identity" for the new-terminal detection in the startup capabilities
    /// notice.  Combines `$TERM_PROGRAM` and `$TERM` (the env-level identity)
    /// with the detected capability tuple (so two environments that probe
    /// differently are treated as different terminals) and a `tmux` marker
    /// when running inside tmux.
    ///
    /// `$TERM_PROGRAM_VERSION` is deliberately excluded — every minor version
    /// update would otherwise re-trigger the notice.
    pub fn fingerprint(&self) -> String {
        let term_program = env::var("TERM_PROGRAM").unwrap_or_default();
        let term = env::var("TERM").unwrap_or_default();
        let tmux = if env::var("TMUX").is_ok() { "tmux" } else { "" };
        let color = match self.color_depth {
            ColorDepth::NoColor => "none",
            ColorDepth::Ansi16 => "16",
            ColorDepth::Ansi256 => "256",
            ColorDepth::TrueColor => "truecolor",
        };
        let image = match self.image_protocol {
            None => "none",
            Some(ImageProtocol::Sixel) => "sixel",
            Some(ImageProtocol::KittyGraphics) => "kitty",
            Some(ImageProtocol::ITerm2) => "iterm2",
            Some(ImageProtocol::Halfblocks) => "halfblocks",
        };
        format!(
            "{term_program}|{term}|{tmux}|{color}|{image}|mouse={}|kbd={}|unicode={}",
            self.mouse, self.keyboard_enhancement, self.unicode_full,
        )
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::minimal()
    }
}

// ── Probing helpers ──────────────────────────────────────────────────────────

/// Infer color depth from environment variables.
///
/// `$COLORTERM` takes precedence when it names a true-color terminal; failing
/// that we fall back to inspecting `$TERM` for the conventional `-256color`
/// suffix, then to the built-in 8/16-color palette.
fn detect_color_depth(term: &str) -> ColorDepth {
    if term == "dumb" || term.is_empty() {
        return ColorDepth::NoColor;
    }
    let colorterm = env::var("COLORTERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if colorterm == "truecolor" || colorterm == "24bit" {
        return ColorDepth::TrueColor;
    }
    if term.contains("direct") {
        // e.g. `xterm-direct`, `tmux-direct`.
        return ColorDepth::TrueColor;
    }
    // A handful of modern terminals are known to support truecolor even when
    // `$COLORTERM` is unset (e.g. a remote session that stripped it).
    if env::var("KITTY_WINDOW_ID").is_ok() || env::var("WEZTERM_PANE").is_ok() {
        return ColorDepth::TrueColor;
    }
    if let Ok(tp) = env::var("TERM_PROGRAM") {
        match tp.as_str() {
            "iTerm.app" | "Apple_Terminal" | "WezTerm" | "ghostty" | "Ghostty" => {
                return ColorDepth::TrueColor
            }
            _ => {}
        }
    }
    if term.contains("256color") {
        return ColorDepth::Ansi256;
    }
    ColorDepth::Ansi16
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

/// Ask `ratatui_image` to probe for an image protocol.  Returns the detected
/// protocol and the `Picker` instance; the Picker is retained and
/// passed through to the image-rendering layer so cold image loads reuse
/// the already-probed configuration.
///
/// Halfblocks are reported as `Some(ImageProtocol::Halfblocks)` — they are
/// still a usable protocol, just lower-fidelity than sixel/kitty/iterm2.
fn detect_image_protocol() -> (Option<ImageProtocol>, Option<Picker>) {
    use ratatui_image::picker::ProtocolType;

    // Picker::from_query_stdio may write escape sequences; a panic here would
    // be disastrous (corrupted terminal state), so we catch-and-swallow.
    let result = std::panic::catch_unwind(Picker::from_query_stdio);
    let picker = match result {
        Ok(Ok(p)) => p,
        _ => return (None, None),
    };
    let protocol = match picker.protocol_type() {
        ProtocolType::Sixel => ImageProtocol::Sixel,
        ProtocolType::Kitty => ImageProtocol::KittyGraphics,
        ProtocolType::Iterm2 => ImageProtocol::ITerm2,
        ProtocolType::Halfblocks => ImageProtocol::Halfblocks,
    };
    (Some(protocol), Some(picker))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Save the current value of an env var, set a new value (or clear it),
    /// and restore on drop.  Lets tests mutate env vars without races — we
    /// just need to be careful to not run color-depth tests in parallel,
    /// which cargo does by default per test binary.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = env::var(key).ok();
            // SAFETY: this test module is serialized by the `env_mutex` below.
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
    fn color_depth_no_color_for_dumb_terminal() {
        assert_eq!(detect_color_depth("dumb"), ColorDepth::NoColor);
        assert_eq!(detect_color_depth(""), ColorDepth::NoColor);
    }

    #[test]
    fn color_depth_truecolor_from_colorterm() {
        let _lock = env_lock();
        let _g1 = EnvGuard::set("COLORTERM", "truecolor");
        let _g2 = EnvGuard::unset("KITTY_WINDOW_ID");
        let _g3 = EnvGuard::unset("WEZTERM_PANE");
        let _g4 = EnvGuard::unset("TERM_PROGRAM");
        assert_eq!(detect_color_depth("xterm-256color"), ColorDepth::TrueColor);
    }

    #[test]
    fn color_depth_256_from_term_suffix() {
        let _lock = env_lock();
        let _g1 = EnvGuard::unset("COLORTERM");
        let _g2 = EnvGuard::unset("KITTY_WINDOW_ID");
        let _g3 = EnvGuard::unset("WEZTERM_PANE");
        let _g4 = EnvGuard::unset("TERM_PROGRAM");
        assert_eq!(detect_color_depth("xterm-256color"), ColorDepth::Ansi256);
    }

    #[test]
    fn color_depth_16_for_plain_xterm() {
        let _lock = env_lock();
        let _g1 = EnvGuard::unset("COLORTERM");
        let _g2 = EnvGuard::unset("KITTY_WINDOW_ID");
        let _g3 = EnvGuard::unset("WEZTERM_PANE");
        let _g4 = EnvGuard::unset("TERM_PROGRAM");
        assert_eq!(detect_color_depth("xterm"), ColorDepth::Ansi16);
    }

    #[test]
    fn color_depth_truecolor_for_kitty_envvar() {
        let _lock = env_lock();
        let _g1 = EnvGuard::unset("COLORTERM");
        let _g2 = EnvGuard::set("KITTY_WINDOW_ID", "1");
        let _g3 = EnvGuard::unset("WEZTERM_PANE");
        let _g4 = EnvGuard::unset("TERM_PROGRAM");
        assert_eq!(detect_color_depth("xterm"), ColorDepth::TrueColor);
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
        assert_eq!(caps.color_depth, ColorDepth::Ansi16);
        assert!(!caps.mouse);
        assert!(caps.image_protocol.is_none());
        assert!(!caps.unicode_full);
        assert!(!caps.keyboard_enhancement);
    }

    #[test]
    fn fingerprint_is_stable_for_identical_caps() {
        let _lock = env_lock();
        let _g1 = EnvGuard::set("TERM_PROGRAM", "WezTerm");
        let _g2 = EnvGuard::set("TERM", "xterm-256color");
        let _g3 = EnvGuard::unset("TMUX");
        let caps = Capabilities {
            color_depth: ColorDepth::TrueColor,
            mouse: true,
            image_protocol: Some(ImageProtocol::KittyGraphics),
            image_picker: None,
            halfblocks_picker: None,
            unicode_full: true,
            keyboard_enhancement: true,
        };
        assert_eq!(caps.fingerprint(), caps.fingerprint());
    }

    #[test]
    fn fingerprint_differs_when_a_capability_flips() {
        let _lock = env_lock();
        let _g1 = EnvGuard::set("TERM_PROGRAM", "WezTerm");
        let _g2 = EnvGuard::set("TERM", "xterm-256color");
        let _g3 = EnvGuard::unset("TMUX");
        let base = Capabilities {
            color_depth: ColorDepth::TrueColor,
            mouse: true,
            image_protocol: Some(ImageProtocol::KittyGraphics),
            image_picker: None,
            halfblocks_picker: None,
            unicode_full: true,
            keyboard_enhancement: true,
        };
        let mouseless = Capabilities {
            mouse: false,
            ..base.clone()
        };
        let no_kbd = Capabilities {
            keyboard_enhancement: false,
            ..base.clone()
        };
        let no_image = Capabilities {
            image_protocol: None,
            ..base.clone()
        };
        assert_ne!(base.fingerprint(), mouseless.fingerprint());
        assert_ne!(base.fingerprint(), no_kbd.fingerprint());
        assert_ne!(base.fingerprint(), no_image.fingerprint());
    }

    #[test]
    fn fingerprint_includes_tmux_marker() {
        let _lock = env_lock();
        let _g1 = EnvGuard::set("TERM_PROGRAM", "WezTerm");
        let _g2 = EnvGuard::set("TERM", "tmux-256color");
        let caps = Capabilities::minimal();
        let _no_tmux = EnvGuard::unset("TMUX");
        let outside = caps.fingerprint();
        let _in_tmux = EnvGuard::set("TMUX", "/tmp/tmux-1000/default,123,0");
        let inside = caps.fingerprint();
        assert_ne!(outside, inside);
        assert!(inside.contains("tmux"));
    }
}
