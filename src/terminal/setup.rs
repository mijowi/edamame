use std::io::Stdout;

use anyhow::Result;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

/// Result of terminal setup: the ratatui `Terminal` plus a flag indicating
/// whether the kitty keyboard enhancement protocol was successfully enabled.
///
/// Capability detection needs the latter because the `PushKeyboardEnhancementFlags`
/// command silently succeeds on terminals that ignore it, so we can only
/// tell it actually worked by whether `execute!` returned `Ok`.
pub struct TerminalSetup {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
    pub keyboard_enhancement: bool,
}

/// Set up the terminal for TUI rendering.
///
/// Enables raw mode, the alternate screen buffer, and — on terminals that
/// support it — the kitty keyboard protocol so key combinations like
/// `Shift+Enter` and `Alt+Shift+Backspace` can be disambiguated from their
/// legacy escape-code equivalents.
pub fn setup() -> Result<TerminalSetup> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // Bracketed paste turns a terminal-native paste (Ctrl-Shift-V,
    // middle-click, etc.) into a single `Event::Paste(String)` so we can
    // insert the pasted content atomically — the only path that works when
    // the host terminal can reach the system clipboard but this process
    // cannot (SSH, Wayland without data-control, WSL, etc.).
    let _ = execute!(stdout, EnableBracketedPaste);
    // Best-effort: terminals without the kitty protocol return an error here
    // and we remember that so the caller can report the degraded state.
    let keyboard_enhancement = execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
    .is_ok();
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(TerminalSetup {
        terminal,
        keyboard_enhancement,
    })
}

/// Enable xterm mouse reporting so the app receives `Event::Mouse` events.
///
/// Called from `main` after capability detection when `capabilities.mouse` is
/// true.  Terminals that don't actually support mouse reporting silently drop
/// the enable sequence; the capability check prevents trying at all on
/// `TERM=linux` / `TERM=dumb` where the escape bytes would end up echoed as
/// literal output.
pub fn enable_mouse() -> Result<()> {
    execute!(std::io::stdout(), EnableMouseCapture)?;
    Ok(())
}

/// Disable mouse capture.  Called by `restore()` (best-effort) so the terminal
/// is never left reporting mouse events after the app exits.
pub fn disable_mouse() {
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
}

/// Supported pointer shapes for [`set_pointer_shape`].
///
/// These map to CSS cursor names (the convention used by modern OSC 22
/// terminals: Ghostty, kitty recent versions, wezterm).  Older terminals like
/// xterm use X11 cursor-font names (`xterm`, `hand2`, `left_ptr`) — we emit
/// both: the X11 name first, then a follow-up OSC 22 with the CSS name, so
/// whichever the terminal understands wins.  Terminals that don't implement
/// OSC 22 at all silently drop the sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerShape {
    /// I-beam cursor: shown when the pointer is over editable text.
    Text,
    /// Pointing-hand cursor: shown over clickable elements (checkboxes, links).
    Hand,
    /// Default arrow cursor: used to restore the terminal's native cursor on
    /// shutdown.
    Default,
}

/// Emit the OSC 22 escape sequence that asks the terminal to change the
/// pointer (mouse) cursor shape.  Best-effort — terminals that don't
/// implement OSC 22 ignore the sequence.
pub fn set_pointer_shape(shape: PointerShape) {
    use std::io::Write;
    // Emit both the X11 cursor-font name and the CSS name.  Any terminal that
    // implements OSC 22 will pick whichever of the two it recognises; the
    // other is ignored.  Emitting both costs ~20 bytes of escape per update
    // and avoids having to probe which dialect the host terminal prefers.
    let (x11, css) = match shape {
        PointerShape::Text => ("xterm", "text"),
        PointerShape::Hand => ("hand2", "pointer"),
        PointerShape::Default => ("left_ptr", "default"),
    };
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "\x1b]22;{x11}\x07\x1b]22;{css}\x07");
    let _ = stdout.flush();
}

/// Restore the terminal to its normal state.
///
/// Must be called before the process exits, even on error, to avoid leaving
/// the terminal in raw mode.
pub fn restore() -> Result<()> {
    set_pointer_shape(PointerShape::Default);
    disable_mouse();
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    disable_raw_mode()?;
    execute!(std::io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

/// Re-enter the TUI after a temporary suspension (e.g. shelling out to
/// `$EDITOR`).  Mirrors [`setup`] minus the `Terminal` construction —
/// the caller already owns a `Terminal` and just needs the underlying
/// terminal state restored to alt-screen / raw-mode.  `mouse` and
/// `keyboard_enhancement` should be the same flags that were passed to
/// the original setup so transient terminal features stay consistent.
///
/// Best-effort: errors during alt-screen re-enter propagate, but the
/// optional features (bracketed paste, kitty keyboard, mouse) are
/// silently ignored on failure to match the original setup's
/// permissiveness.
pub fn re_enter(mouse: bool, keyboard_enhancement: bool) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let _ = execute!(stdout, EnableBracketedPaste);
    if keyboard_enhancement {
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    if mouse {
        let _ = execute!(stdout, EnableMouseCapture);
    }
    Ok(())
}
