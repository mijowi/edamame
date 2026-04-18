mod app;
mod config;
mod document;
mod editor;
mod input;
mod markdown;
mod terminal;
mod ui;

use anyhow::Result;
use app::App;
use config::Config;
use terminal::{Capabilities, TerminalSetup};

fn main() -> Result<()> {
    // ── Parse CLI arguments ────────────────────────────────────────
    let file_path: Option<std::path::PathBuf> = std::env::args().nth(1).map(Into::into);

    // ── Load configuration ─────────────────────────────────────────
    let config = Config::load().unwrap_or_else(|e| {
        // Config errors are non-fatal; use defaults and note the problem.
        // Can't use tracing here since subscriber isn't set up yet.
        eprintln!("Warning: failed to load config: {e}. Using defaults.");
        Config::default()
    });

    // ── Set up logging (disabled by default) ──────────────────────
    let _log_guard = if config.editor.dev_mode {
        setup_logging()
    } else {
        None
    };

    // ── Initialise terminal ────────────────────────────────────────
    let TerminalSetup {
        terminal,
        keyboard_enhancement,
    } = terminal::setup()?;

    // Install a panic hook so the terminal is always restored, even on panic.
    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::restore();
        orig_hook(info);
    }));

    // ── Detect capabilities ───────────────────────────────────────
    // Must run AFTER EnterAlternateScreen so Picker::from_query_stdio is
    // talking to the live terminal, and BEFORE app.run() spawns its event
    // reader thread (competing reads would eat the escape-sequence replies).
    let capabilities = Capabilities::detect(keyboard_enhancement);
    log_capabilities(&capabilities);

    // ── Run the app ───────────────────────────────────────────────
    let mut app = App::new(config, file_path, capabilities)?;
    let run_result = app.run(terminal);

    // ── Restore terminal ──────────────────────────────────────────
    terminal::restore()?;

    run_result
}

// ── Logging setup ─────────────────────────────────────────────────────────────

/// Initialise the file-based tracing subscriber.
///
/// Returns the non-blocking writer guard; dropping it flushes and closes the
/// log file. The guard must be kept alive for the duration of the program.
fn setup_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let log_dir = Config::log_dir()?;
    if std::fs::create_dir_all(&log_dir).is_err() {
        return None;
    }

    let file_appender = tracing_appender::rolling::daily(&log_dir, "debug.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();

    tracing::info!("edamame starting");
    Some(guard)
}

/// Write a one-line summary of the detected terminal capabilities to the log.
fn log_capabilities(caps: &Capabilities) {
    tracing::info!(
        colour_depth = ?caps.colour_depth,
        mouse = caps.mouse,
        image_protocol = ?caps.image_protocol,
        unicode_full = caps.unicode_full,
        keyboard_enhancement = caps.keyboard_enhancement,
        "terminal capabilities detected"
    );
}
