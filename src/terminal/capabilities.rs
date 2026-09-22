//! Terminal capability detection: color depth, mouse, image protocol, UTF-8 locale, and
//! the kitty keyboard enhancement protocol.
//!
//! Three signals: environment variables, crossterm's `supports_keyboard_enhancement()`,
//! and `Picker::from_query_stdio`.  [`Capabilities::detect`] must never panic and never
//! block noticeably — every probe either finishes in milliseconds or falls back to a
//! conservative default.

use std::env;

use ratatui_image::picker::{Picker, ProtocolType};

/// Color bit-depth supported by the terminal.  Ordered poorest to richest, so
/// `depth >= ColorDepth::Ansi256` works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorDepth {
    /// No color at all (`TERM=dumb`); rendering emits no ANSI style escapes.
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
    /// DEC Sixel graphics (Windows Terminal 1.22+, xterm with `--enable-sixel-graphics`, foot).
    ///
    /// The one protocol here that keeps no image on the terminal side: every sequence is drawn
    /// where it is sent, so a partly visible image is a re-slice of the payload rather than a
    /// placement.  See `docs/dev/plans/image-partial-rendering.md` (M2).
    Sixel,
    /// Kitty graphics protocol through `ratatui_image`'s **unicode-placeholder** backend (kitty,
    /// Ghostty).  A partly visible image is rendered sharply from the placeholder grid, but the
    /// image re-composites wherever the placeholders move, so it drops to halfblocks *while
    /// scrolling*.  When `images.sharp_scrolling` is on (the default) and outside tmux, kitty and
    /// Ghostty are served as [`ImageProtocol::KittyDirect`] instead; this is the path they take with
    /// that setting off, and always under tmux, where direct placement's passthrough is unavailable.
    KittyGraphics,
    /// iTerm2 inline-images protocol.
    ITerm2,
    /// Kitty graphics through **direct placement**: the image is transmitted once, and every
    /// frame places the visible rows of it with a source rectangle (`a=p`).  Unlike
    /// [`ImageProtocol::KittyGraphics`] the placement is one short escape, so a partly visible image
    /// stays sharp even while scrolling.
    ///
    /// Two kinds of terminal land here, both gated on `images.sharp_scrolling` (and never under
    /// tmux):
    /// - WezTerm, which implements the transmit, placement and delete but *not* the `U=1`
    ///   unicode-placeholder extension `ratatui_image`'s Kitty backend renders through, so without
    ///   this route it is served as iTerm2 and every partly visible image falls back to halfblocks.
    /// - genuine kitty and Ghostty, which *do* render placeholders ([`ImageProtocol::KittyGraphics`]) but are
    ///   cheaper to scroll through `a=p` than to re-composite; direct placement is the opt-out knob's
    ///   whole subject for them.
    KittyDirect,
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
    /// `None` when image display is not supported.
    pub image_protocol: Option<ImageProtocol>,
    /// The startup probe's `Picker`, retained so cold image loads reuse the probed
    /// configuration instead of re-running `Picker::from_query_stdio`.  `None` iff
    /// `image_protocol` is.
    pub image_picker: Option<Picker>,
    /// A second `Picker` pinned to halfblocks at the native picker's font size (no extra
    /// probe).  The fallback path renders through it while an image is partly visible or
    /// scrolling — halfblocks are position-independent and cheap to cell-copy — and
    /// upgrades back once the view quiesces.  `None` iff image support is absent.
    pub halfblocks_picker: Option<Picker>,
    /// Whether the locale env vars advertise UTF-8; a proxy for "full Unicode support".
    pub unicode_full: bool,
    /// Whether `supports_keyboard_enhancement()` answered affirmatively.  Without it the
    /// terminal is limited to the legacy control-byte encoding, which can represent
    /// neither shifted modifier combinations nor `Ctrl` with a non-alphabetic key, so
    /// features relying on those (e.g. `Ctrl-Shift-Z` redo) must degrade.
    pub keyboard_enhancement: bool,
}

impl Capabilities {
    /// Detect all capabilities.  Must be called **after** the terminal enters the
    /// alternate screen and raw mode, since the Picker probes stdout/stdin with escape
    /// sequences.  `kbd_enhancement` is passed in rather than re-queried: both it and the
    /// Picker probe read replies off the tty, and one could consume the other's.
    pub fn detect(kbd_enhancement: bool, sharp_scrolling: bool) -> Self {
        let term = env::var("TERM").unwrap_or_default();
        let color_depth = detect_color_depth(&term);
        let mouse = detect_mouse(&term);
        let unicode_full = detect_unicode_full();
        let (image_protocol, image_picker) = detect_image_protocol(sharp_scrolling);
        let halfblocks_picker = image_picker
            .as_ref()
            .map(|p| halfblocks_from(p.font_size()));

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

    /// Color depth from environment variables only, with no escape-sequence I/O, so it is
    /// safe before [`terminal::setup()`](fn@crate::terminal::setup) — the config loader
    /// needs it to pick a fallback theme.  [`Self::detect`] is the source of truth
    /// everywhere else.
    pub fn detect_color_depth_from_env() -> ColorDepth {
        let term = env::var("TERM").unwrap_or_default();
        detect_color_depth(&term)
    }

    /// Everything knowable from the environment alone, leaving the two probe-derived
    /// facts at their [`Self::minimal`] values.
    ///
    /// For `--doctor` when stdout or stdin is not a terminal: the image and keyboard
    /// probes would write escape sequences into the user's redirected file and then report
    /// "no support" for a terminal that has it.  [`crate::cli::doctor`] prints the two as
    /// *unknown*.
    pub fn env_only() -> Self {
        let term = env::var("TERM").unwrap_or_default();
        Self {
            color_depth: detect_color_depth(&term),
            mouse: detect_mouse(&term),
            unicode_full: detect_unicode_full(),
            ..Self::minimal()
        }
    }

    /// True iff the terminal advertises 24-bit color.  Anything less quantizes the RGB
    /// every theme and image is authored in, so this gates the theme picker, the welcome
    /// modal's image / diagram options, and the first-run default theme.
    pub fn full_color(&self) -> bool {
        self.color_depth == ColorDepth::TrueColor
    }

    /// Minimum-common-denominator terminal, for tests and when probing is impossible.
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

    /// Stable terminal identity for the startup capabilities notice's new-terminal
    /// detection: the env-level identity plus the detected capability tuple (so two
    /// environments that probe differently count as different terminals) plus a tmux
    /// marker.  `$TERM_PROGRAM_VERSION` is excluded, or every minor update would re-notify.
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
            Some(ImageProtocol::KittyDirect) => "kitty-direct",
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

/// Infer color depth from environment variables: `$COLORTERM` first, then `$TERM`'s
/// conventional `-256color` suffix, then 8/16-color.
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
        return ColorDepth::TrueColor;
    }
    // Some modern terminals support truecolor with `$COLORTERM` unset (e.g. a remote
    // session that stripped it).
    if env::var("KITTY_WINDOW_ID").is_ok() || env::var("WEZTERM_PANE").is_ok() {
        return ColorDepth::TrueColor;
    }
    if let Ok(tp) = env::var("TERM_PROGRAM") {
        match tp.as_str() {
            // Deliberately NOT `Apple_Terminal`: it silently quantizes 24-bit SGR, so
            // claiming truecolor would hand it themes and images it renders wrong.
            "iTerm.app" | "WezTerm" | "ghostty" | "Ghostty" => return ColorDepth::TrueColor,
            _ => {}
        }
    }
    if term.contains("256color") {
        return ColorDepth::Ansi256;
    }
    ColorDepth::Ansi16
}

/// Infer mouse support from `$TERM`.  Essentially every xterm-compatible terminal has it;
/// the exceptions are `dumb`, empty, and `linux` (the framebuffer console).
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

/// A picker **guaranteed** to encode halfblocks, at the terminal's real `font_size`.
/// Neither ratatui-image constructor manages both, so take the font size from one and
/// stamp the protocol onto it.
///
/// `Picker::halfblocks()` hardcodes a (10, 20) font size, which changes the image's aspect
/// ratio every time it crosses the native↔halfblocks boundary.  `Picker::from_fontsize()`
/// keeps the size but *infers* the protocol from `$TERM_PROGRAM`, yielding an iTerm2
/// picker on many terminals — whose scratch buffer is one base64-PNG escape cell rather
/// than position-independent halfblock cells, which the row-clipping partial painter
/// cannot slice.
fn halfblocks_from(font_size: ratatui_image::FontSize) -> Picker {
    // Deprecated in ratatui-image 9+, but the only constructor taking a probed font size.
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(font_size);
    picker.set_protocol_type(ProtocolType::Halfblocks);
    picker
}

/// True when we are talking to iTerm2.app, directly or over ssh — `$LC_TERMINAL` is
/// iTerm2's own forwarded marker, and the far end of the pipe is what matters here.
fn is_iterm2_app() -> bool {
    env::var("TERM_PROGRAM").is_ok_and(|v| v.contains("iTerm"))
        || env::var("LC_TERMINAL").is_ok_and(|v| v.contains("iTerm"))
}

/// Whether the iTerm2 env hint may override an affirmative Kitty probe (see
/// [`resolve_protocol`]).  Under tmux it may not: `update-environment` covers neither
/// `TERM_PROGRAM` nor `LC_TERMINAL`, so a pane created from iTerm2 and reattached from
/// Ghostty still advertises iTerm2 — and pinning `Iterm2` would break images on a terminal
/// that had them working.  The stdio probe asks the live terminal, so inside tmux we defer
/// to it.  The case given up is tmux *inside* iTerm2, which costs blank rows that the
/// halfblocks fallback can recover; a wrong-protocol pin on Ghostty cannot be.
fn iterm2_hint_is_trustworthy() -> bool {
    is_iterm2_app() && env::var_os("TMUX").is_none()
}

/// True for the terminals this build serves through direct placement rather than through the
/// protocol the stdio probe answers with.
///
/// WezTerm implements the Kitty protocol's transmit, its placement (with a source rectangle) and
/// its delete, but **not** the `U=1` unicode-placeholder extension — and that extension is the
/// only way `ratatui-image`'s Kitty backend renders.  It answers the iTerm2 query as well, which
/// is what the probe concludes, so no capability query can route this: the missing feature is a
/// sub-feature of a protocol the terminal does support.  Hence an identity hint, on the same
/// footing as [`is_iterm2_app`].
fn is_wezterm() -> bool {
    env::var("TERM_PROGRAM").is_ok_and(|v| v.contains("WezTerm"))
        || env::var("WEZTERM_PANE").is_ok()
}

/// Whether [`is_wezterm`] may route the protocol — the reasoning is [`iterm2_hint_is_trustworthy`]'s
/// exactly.  `update-environment` carries neither `TERM_PROGRAM` nor `WEZTERM_PANE`, so a pane
/// created in WezTerm and reattached from elsewhere still advertises it, and routing it to
/// direct placement would then ask that terminal to place graphics it may not support at all.
fn direct_placement_hint_is_trustworthy() -> bool {
    is_wezterm() && env::var_os("TMUX").is_none()
}

/// Whether a genuine kitty/Ghostty probe may be upgraded to direct placement.  The genuineness is
/// the probe's own `Kitty` result (unlike WezTerm, which the probe reports as iTerm2); this adds
/// only the tmux gate.  Inside tmux the upgrade is refused: `a=p` needs `allow-passthrough`, which
/// upstream enables by spawning `tmux` — a subprocess edamame will not run — whereas
/// `ratatui_image`'s placeholder backend keeps working through tmux's graphics passthrough, so the
/// placeholder path ([`ImageProtocol::KittyGraphics`]) is the right one to leave in place there.
fn kitty_direct_hint_is_trustworthy() -> bool {
    env::var_os("TMUX").is_none()
}

/// Upgrade a probed protocol to [`ImageProtocol::KittyDirect`] when the user has opted in to
/// `images.sharp_scrolling` (the default) and a hint says the terminal can place images.
///
/// `wezterm` is [`direct_placement_hint_is_trustworthy`] — a terminal that places images but lacks
/// unicode placeholders, which the probe served as iTerm2 or Kitty.  `kitty` is
/// [`kitty_direct_hint_is_trustworthy`] — a genuine `Kitty` probe outside tmux, which renders
/// placeholders but is cheaper to scroll through `a=p`.  Halfblocks and Sixel are never touched:
/// halfblocks means the probe saw no graphics support, and Sixel has no placement to make.
fn resolve_direct_placement(
    protocol: ImageProtocol,
    sharp_scrolling: bool,
    wezterm: bool,
    kitty: bool,
) -> ImageProtocol {
    if !sharp_scrolling {
        return protocol;
    }
    match protocol {
        // The two protocols that mean "this terminal displayed the graphics it was asked about".
        ImageProtocol::ITerm2 | ImageProtocol::KittyGraphics if wezterm => {
            ImageProtocol::KittyDirect
        }
        // A genuine kitty/Ghostty probe (never iTerm2's, which cannot place).
        ImageProtocol::KittyGraphics if kitty => ImageProtocol::KittyDirect,
        other => other,
    }
}

/// Resolve the protocol to encode with from the one `Picker::from_query_stdio` probed.
///
/// iTerm2 3.5+ answers the Kitty capability query affirmatively, so the picker comes back
/// as `Kitty` — but ratatui-image's Kitty backend renders only through the
/// unicode-placeholder extension, which iTerm2 does not implement, so the image is
/// transmitted and never placed and the reserved rows stay blank.  Pin it to `Iterm2`.
///
/// Only `Kitty` is overridden.  `iterm2` must come from [`iterm2_hint_is_trustworthy`],
/// not `is_iterm2_app` directly.
fn resolve_protocol(probed: ProtocolType, iterm2: bool) -> ProtocolType {
    match probed {
        ProtocolType::Kitty if iterm2 => ProtocolType::Iterm2,
        other => other,
    }
}

/// Probe for an image protocol, returning it alongside the `Picker` the rendering layer
/// reuses.  Halfblocks count as support — lower fidelity, still usable.
fn detect_image_protocol(sharp_scrolling: bool) -> (Option<ImageProtocol>, Option<Picker>) {
    // A panic here would corrupt terminal state, so catch and swallow.  Scoping the guard
    // to the `catch_unwind` alone is load-bearing: this runs on the main thread after the
    // hook is installed, so a guard live over the code below would let a real panic unwind
    // out of `main` with the alternate screen up and nothing printed.
    let result = {
        let _expected = super::ExpectedPanic::new();
        std::panic::catch_unwind(Picker::from_query_stdio)
    };
    let mut picker = match result {
        Ok(Ok(p)) => p,
        _ => return (None, None),
    };

    picker.set_protocol_type(resolve_protocol(
        picker.protocol_type(),
        iterm2_hint_is_trustworthy(),
    ));

    let protocol = match picker.protocol_type() {
        ProtocolType::Sixel => ImageProtocol::Sixel,
        ProtocolType::Kitty => ImageProtocol::KittyGraphics,
        ProtocolType::Iterm2 => ImageProtocol::ITerm2,
        ProtocolType::Halfblocks => ImageProtocol::Halfblocks,
    };
    // Upgrade to direct placement where the user opted in and a hint applies (WezTerm, or a genuine
    // kitty/Ghostty probe outside tmux).  Halfblocks and Sixel are left alone; see
    // [`resolve_direct_placement`].
    let protocol = resolve_direct_placement(
        protocol,
        sharp_scrolling,
        direct_placement_hint_is_trustworthy(),
        kitty_direct_hint_is_trustworthy(),
    );
    (Some(protocol), Some(picker))
}

#[cfg(test)]
mod tests {
    use super::*;

    // `env_lock` is crate-wide because the race is process-wide: a lock private to this
    // module could not exclude `cli::doctor`'s reads of the very variables set below.
    use crate::test_env::{env_lock, EnvGuard};

    // ── Image protocol ────────────────────────────────────────────────

    /// Regression: `Picker::from_fontsize` infers its protocol from `$TERM_PROGRAM`, so
    /// the "halfblocks" picker came back as an iTerm2 one.  See [`halfblocks_from`].
    #[test]
    fn halfblocks_picker_is_halfblocks_even_under_iterm2() {
        let _lock = env_lock();
        let _g1 = EnvGuard::set("TERM_PROGRAM", "iTerm.app");
        let _g2 = EnvGuard::unset("TMUX");
        let picker = halfblocks_from(ratatui_image::FontSize::new(7, 15));
        assert_eq!(picker.protocol_type(), ProtocolType::Halfblocks);
    }

    /// The probed font size must survive; `Picker::halfblocks()` would hardcode (10, 20).
    #[test]
    fn halfblocks_picker_keeps_the_probed_font_size() {
        let _lock = env_lock();
        let _g = EnvGuard::unset("TERM_PROGRAM");
        let picker = halfblocks_from(ratatui_image::FontSize::new(7, 15));
        assert_eq!(picker.font_size().width, 7);
        assert_eq!(picker.font_size().height, 15);
    }

    #[test]
    fn iterm2_app_detected_from_term_program_and_lc_terminal() {
        let _lock = env_lock();
        {
            let _g1 = EnvGuard::set("TERM_PROGRAM", "iTerm.app");
            let _g2 = EnvGuard::unset("LC_TERMINAL");
            assert!(is_iterm2_app());
        }
        {
            let _g1 = EnvGuard::unset("TERM_PROGRAM");
            let _g2 = EnvGuard::set("LC_TERMINAL", "iTerm2");
            assert!(is_iterm2_app());
        }
        {
            let _g1 = EnvGuard::set("TERM_PROGRAM", "ghostty");
            let _g2 = EnvGuard::unset("LC_TERMINAL");
            assert!(!is_iterm2_app());
        }
    }

    /// The headline override — see [`resolve_protocol`].
    #[test]
    fn kitty_probe_under_iterm2_is_pinned_to_iterm2() {
        assert_eq!(
            resolve_protocol(ProtocolType::Kitty, true),
            ProtocolType::Iterm2
        );
    }

    /// Only `Kitty` is overridden, and only under the iTerm2 hint.
    #[test]
    fn resolve_protocol_leaves_every_other_probe_alone() {
        for probed in [
            ProtocolType::Kitty,
            ProtocolType::Sixel,
            ProtocolType::Iterm2,
            ProtocolType::Halfblocks,
        ] {
            assert_eq!(
                resolve_protocol(probed, false),
                probed,
                "{probed:?} must survive without an iTerm2 hint"
            );
        }
        for probed in [
            ProtocolType::Sixel,
            ProtocolType::Iterm2,
            ProtocolType::Halfblocks,
        ] {
            assert_eq!(
                resolve_protocol(probed, true),
                probed,
                "{probed:?} must survive even under iTerm2"
            );
        }
    }

    /// See [`iterm2_hint_is_trustworthy`]: a stale forwarded marker inside tmux must not
    /// pin a protocol the live terminal cannot render.
    #[test]
    fn iterm2_hint_is_distrusted_inside_tmux() {
        let _lock = env_lock();
        let _g1 = EnvGuard::set("LC_TERMINAL", "iTerm2");
        let _g2 = EnvGuard::unset("TERM_PROGRAM");
        {
            let _g3 = EnvGuard::unset("TMUX");
            assert!(iterm2_hint_is_trustworthy(), "bare iTerm2 is trusted");
        }
        {
            let _g3 = EnvGuard::set("TMUX", "/tmp/tmux-501/default,1234,0");
            assert!(
                !iterm2_hint_is_trustworthy(),
                "a stale forwarded marker inside tmux must not pin Iterm2"
            );
            assert_eq!(
                resolve_protocol(ProtocolType::Kitty, iterm2_hint_is_trustworthy()),
                ProtocolType::Kitty,
                "the live capability probe wins inside tmux"
            );
        }
    }

    /// The guard must gate on the hint, not merely on tmux.
    #[test]
    fn iterm2_hint_is_absent_without_an_iterm2_marker() {
        let _lock = env_lock();
        let _g1 = EnvGuard::set("TERM_PROGRAM", "ghostty");
        let _g2 = EnvGuard::unset("LC_TERMINAL");
        let _g3 = EnvGuard::unset("TMUX");
        assert!(!iterm2_hint_is_trustworthy());
    }

    /// WezTerm is recognised either way it announces itself — and by nothing else, since a
    /// terminal that is not WezTerm would be asked to place graphics it may not support.
    #[test]
    fn the_direct_placement_hint_recognises_only_wezterm() {
        let _lock = env_lock();
        let _g3 = EnvGuard::unset("TMUX");

        let _g1 = EnvGuard::set("TERM_PROGRAM", "WezTerm");
        let _g2 = EnvGuard::unset("WEZTERM_PANE");
        assert!(is_wezterm() && direct_placement_hint_is_trustworthy());

        // The pane variable alone is enough: it is what survives a shell that rewrites
        // `TERM_PROGRAM`.
        let _g1 = EnvGuard::set("TERM_PROGRAM", "xterm-256color");
        let _g2 = EnvGuard::set("WEZTERM_PANE", "0");
        assert!(is_wezterm());

        let _g1 = EnvGuard::set("TERM_PROGRAM", "iTerm.app");
        let _g2 = EnvGuard::unset("WEZTERM_PANE");
        assert!(
            !is_wezterm(),
            "iTerm2 has its own path and no a=p worth using"
        );
    }

    /// Same distrust as [`iterm2_hint_is_trustworthy`], for the same reason: a stale forwarded
    /// marker inside tmux must not pin a protocol the live pane cannot speak.  M4 additionally
    /// needs `allow-passthrough`, which upstream turns on by spawning `tmux` — a subprocess
    /// edamame will not spawn for this.
    #[test]
    fn the_direct_placement_hint_is_distrusted_inside_tmux() {
        let _lock = env_lock();
        let _g1 = EnvGuard::set("TERM_PROGRAM", "WezTerm");
        let _g2 = EnvGuard::unset("WEZTERM_PANE");
        {
            let _g3 = EnvGuard::unset("TMUX");
            assert!(
                direct_placement_hint_is_trustworthy(),
                "bare WezTerm is trusted"
            );
        }
        {
            let _g3 = EnvGuard::set("TMUX", "/tmp/tmux-501/default,1234,0");
            assert!(!direct_placement_hint_is_trustworthy());
        }
    }

    /// The opt-in gate: with `sharp_scrolling` off, no probe is ever upgraded to direct placement,
    /// whichever hint applies.
    #[test]
    fn sharp_scrolling_off_never_upgrades_to_direct_placement() {
        for protocol in [
            ImageProtocol::KittyGraphics,
            ImageProtocol::ITerm2,
            ImageProtocol::Sixel,
            ImageProtocol::Halfblocks,
        ] {
            assert_eq!(
                resolve_direct_placement(protocol, false, true, true),
                protocol,
                "{protocol:?} must survive with sharp_scrolling off"
            );
        }
    }

    /// With the setting on: WezTerm's hint upgrades either protocol the probe reports it as, a
    /// genuine Kitty probe upgrades on the kitty hint, and Sixel/Halfblocks never move.
    #[test]
    fn sharp_scrolling_on_upgrades_only_placeable_protocols() {
        // WezTerm is served as iTerm2 or Kitty by the probe; its hint upgrades both.
        assert_eq!(
            resolve_direct_placement(ImageProtocol::ITerm2, true, true, false),
            ImageProtocol::KittyDirect
        );
        assert_eq!(
            resolve_direct_placement(ImageProtocol::KittyGraphics, true, true, false),
            ImageProtocol::KittyDirect
        );
        // Genuine kitty/Ghostty: the Kitty probe upgrades on the kitty hint alone.
        assert_eq!(
            resolve_direct_placement(ImageProtocol::KittyGraphics, true, false, true),
            ImageProtocol::KittyDirect
        );
        // iTerm2 without the WezTerm hint stays iTerm2 — real iTerm2 cannot place.
        assert_eq!(
            resolve_direct_placement(ImageProtocol::ITerm2, true, false, true),
            ImageProtocol::ITerm2
        );
        // Sixel and Halfblocks are never touched, whatever the hints say.
        for protocol in [ImageProtocol::Sixel, ImageProtocol::Halfblocks] {
            assert_eq!(
                resolve_direct_placement(protocol, true, true, true),
                protocol,
                "{protocol:?} has no placement to make"
            );
        }
    }

    /// A genuine kitty/Ghostty probe keeps the placeholder path under tmux, where direct
    /// placement's passthrough is unavailable — the kitty hint is false there.
    #[test]
    fn the_kitty_direct_hint_is_distrusted_inside_tmux() {
        let _lock = env_lock();
        {
            let _g = EnvGuard::unset("TMUX");
            assert!(kitty_direct_hint_is_trustworthy(), "bare kitty is trusted");
            assert_eq!(
                resolve_direct_placement(ImageProtocol::KittyGraphics, true, false, true),
                ImageProtocol::KittyDirect
            );
        }
        {
            let _g = EnvGuard::set("TMUX", "/tmp/tmux-501/default,1234,0");
            assert!(!kitty_direct_hint_is_trustworthy());
            assert_eq!(
                resolve_direct_placement(
                    ImageProtocol::KittyGraphics,
                    true,
                    false,
                    kitty_direct_hint_is_trustworthy(),
                ),
                ImageProtocol::KittyGraphics,
                "the placeholder path stays under tmux"
            );
        }
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
    fn color_depth_256_for_apple_terminal() {
        // Terminal.app quantizes 24-bit SGR, so it must resolve to Ansi256 despite
        // `TERM=xterm-256color`.
        let _lock = env_lock();
        let _g1 = EnvGuard::unset("COLORTERM");
        let _g2 = EnvGuard::unset("KITTY_WINDOW_ID");
        let _g3 = EnvGuard::unset("WEZTERM_PANE");
        let _g4 = EnvGuard::set("TERM_PROGRAM", "Apple_Terminal");
        assert_eq!(detect_color_depth("xterm-256color"), ColorDepth::Ansi256);
    }

    #[test]
    fn full_color_only_for_truecolor() {
        let caps = Capabilities {
            color_depth: ColorDepth::TrueColor,
            ..Capabilities::minimal()
        };
        assert!(caps.full_color());
        for depth in [ColorDepth::Ansi256, ColorDepth::Ansi16, ColorDepth::NoColor] {
            let caps = Capabilities {
                color_depth: depth,
                ..Capabilities::minimal()
            };
            assert!(!caps.full_color(), "{depth:?} is not full color");
        }
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
