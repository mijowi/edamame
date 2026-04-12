/// Terminal capabilities detected at startup.
///
/// Phase 0 uses a minimal stub. Phase 4 will implement full probing via
/// crossterm queries and environment variable heuristics.
#[derive(Debug, Clone)]
pub struct Capabilities {
    /// Whether the terminal supports mouse events.
    pub mouse: bool,
    /// Whether the kitty keyboard enhancement protocol is available
    /// (enables Ctrl-Shift-Z as a secondary redo binding).
    pub keyboard_enhancement: bool,
}

impl Capabilities {
    /// Probe the terminal for capabilities.
    ///
    /// Phase 0: returns conservative defaults (no mouse, no enhancement).
    /// Phase 4 will replace this with real probing.
    pub fn detect() -> Self {
        Self {
            mouse: false,
            keyboard_enhancement: false,
        }
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::detect()
    }
}
