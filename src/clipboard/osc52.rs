//! Copy through the *terminal* rather than the OS: OSC 52.
//!
//! An escape sequence written to stdout, which the terminal may turn into a clipboard write on
//! its own machine — which is why it works where OS access cannot: over SSH, on Wayland without
//! `wayland-data-control`, in WSL, and in a build without the `clipboard` feature.  Terminals
//! that don't understand it ignore it.  Only the real sources in [`super::source`] emit it, so a
//! test's clipboard never writes escapes to test stdout.

use std::io::Write;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

/// Write `text` to the terminal's clipboard (`ESC ] 52 ; c ; base64 BEL`).
pub fn osc52_copy(text: &str) {
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "\x1b]52;c;{}\x07", BASE64.encode(text));
    let _ = stdout.flush();
}
