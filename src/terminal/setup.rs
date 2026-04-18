use std::io::Stdout;

use anyhow::Result;
use crossterm::{
    event::{KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags},
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

/// Restore the terminal to its normal state.
///
/// Must be called before the process exits, even on error, to avoid leaving
/// the terminal in raw mode.
pub fn restore() -> Result<()> {
    let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    disable_raw_mode()?;
    execute!(std::io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}
