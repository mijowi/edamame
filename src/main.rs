// The binary is a thin shell over the library crate — it deliberately
// declares no modules of its own.  Re-declaring them here would compile
// a second, private copy of the entire tree, which is how `app`'s unit
// tests previously ended up reachable only via `cargo test --bin
// edamame`.  See `src/lib.rs`.
use std::path::PathBuf;

use anyhow::Result;

use edamame::app::App;
use edamame::cli::{self, Invocation, RunOpts};
use edamame::config::{self, Config, LoadedConfig};
use edamame::terminal::{self, Capabilities, ColorDepth, TerminalSetup};

/// Exit status for a command line we couldn't parse.  2 is the
/// long-standing convention for a usage error (as distinct from 1, a
/// program that ran and failed).
const EXIT_USAGE: i32 = 2;

fn main() -> Result<()> {
    // ── Parse CLI arguments ────────────────────────────────────────
    // `args_os`, not `args`: the latter panics on a non-UTF-8 argument,
    // which is a legal file name on Linux.  See `cli::args`.
    let invocation = Invocation::parse(std::env::args_os().skip(1)).unwrap_or_else(|e| {
        eprintln!("edamame: {e}\n\n{}", cli::USAGE);
        std::process::exit(EXIT_USAGE);
    });

    match invocation {
        // The informational flags answer and exit without ever touching
        // the config directory or the alternate screen — `--doctor`
        // enters and leaves it to run the capability probe, but draws
        // no frame and prints only after restoring.
        Invocation::Help => {
            print!("{}", cli::help_text());
            Ok(())
        }
        Invocation::Version => {
            println!("{}", cli::version_line());
            Ok(())
        }
        Invocation::Doctor => cli::run_doctor(),
        Invocation::Run { file, opts } => run(file, opts),
    }
}

/// Start the editor: load config, set up the terminal, probe
/// capabilities, run the app, restore.
fn run(file_path: Option<PathBuf>, opts: RunOpts) -> Result<()> {
    // ── Load configuration ─────────────────────────────────────────
    // Three files: config.toml (editor/modal/table/image + active theme
    // name), keybindings.toml (overrides), themes/<active>.toml (style
    // table).
    //
    // Order matters: scaffold the default files FIRST so a first-run user
    // gets a themed editor on their first launch (otherwise `load` reads
    // the theme file before it has been written).  `ensure_default_files`
    // never overwrites existing user files, so this is safe on every
    // subsequent run.  The `load` fallback also covers the missing-file
    // case — if scaffolding fails (e.g. unwritable XDG dir) the compiled
    // `Theme::default()` is used.
    //
    // We need to know the terminal's color depth *before* both of those
    // steps: the scaffolder seeds `theme = "256 Dark"` instead of the
    // truecolor default on an indexed-color terminal, and `Config::load`
    // picks the same capability-appropriate built-in when the active theme
    // file is missing on disk.  The full capability probe
    // (`Capabilities::detect`) writes escape sequences to the terminal and
    // must therefore run *after* `terminal::setup` — too late for this
    // decision.  `detect_color_depth_from_env` inspects `$COLORTERM`,
    // `$TERM`, and a handful of terminal-specific env vars only (no I/O),
    // so it's safe to call at this point and gives us the same answer the
    // full probe will compute later.  The full probe remains the source of
    // truth for everything else (mouse, keyboard enhancements, image
    // support, etc.); this is a one-bit early read, not a parallel
    // implementation.
    //
    // `--no-config` short-circuits all three files: no scaffolding and
    // no reads here, and `suppress_config_writes` below closes the write
    // half for every site in the process (see `config::persistence`).
    // `LoadedConfig::default()` is the same in-memory fallback `load`
    // failures already use, so this is an existing tested path rather
    // than a second definition of "the built-in defaults".
    let truecolor_at_load = Capabilities::detect_color_depth_from_env() == ColorDepth::TrueColor;
    let loaded = if opts.no_config {
        LoadedConfig::default()
    } else {
        Config::ensure_default_files(truecolor_at_load);
        Config::load(truecolor_at_load, true).unwrap_or_else(|e| {
            // Config errors are non-fatal; use defaults and note the problem.
            // Can't use tracing here since subscriber isn't set up yet.
            eprintln!("Warning: failed to load config: {e}. Using defaults.");
            LoadedConfig::default()
        })
    };
    let LoadedConfig {
        mut config,
        keybindings,
        theme,
        warnings: config_warnings,
    } = loaded;

    // ── Apply run flags on top of the loaded config ────────────────
    if opts.no_config {
        // Take the config directory out of play for the rest of the
        // process — reads as well as writes — before `App` exists and so
        // before anything can save or enumerate it.  Skipping the load
        // above is only the startup half: the theme picker and the
        // export-stylesheet list read the directory again mid-session.
        config::disable_config_dir();

        // The welcome modal exists to capture first-run choices *to
        // disk*, and a first run opens it non-dismissable (there is no
        // prior choice to protect, so it has no Cancel).  With saving
        // suppressed it has nothing to capture — leaving it on would put
        // an unskippable prompt at the head of every triage run for no
        // outcome.  The capabilities notice is left alone: it is
        // dismissable, and what it reports is exactly what someone
        // running `--no-config` is usually trying to find out.
        config.editor.show_welcome = false;
    }
    config.dev.logging |= opts.log;

    // ── Set up logging (disabled by default) ──────────────────────
    let log_guard = if config.dev.logging {
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

    // ── Enable mouse reporting ────────────────────────────────────
    // Only on terminals that advertise mouse support — sending the enable
    // sequence to a terminal that doesn't understand it (TERM=linux, dumb)
    // leaves the bytes echoed as literal output.  Any failure is non-fatal:
    // the app still runs, just without mouse input.
    if capabilities.mouse {
        if let Err(e) = terminal::enable_mouse() {
            tracing::warn!(error = %e, "failed to enable mouse capture");
        }
    }

    // ── Run the app ───────────────────────────────────────────────
    let mut app = App::new(
        config,
        keybindings,
        theme,
        file_path,
        capabilities,
        config_warnings,
    )?;
    let run_result = app.run(terminal);

    // ── Restore terminal ──────────────────────────────────────────
    terminal::restore()?;

    // Point `--log` at what it produced.  After `restore`, so the line
    // lands on the user's normal screen rather than being swallowed with
    // the alternate one — and after dropping the appender guard, so the
    // file is flushed and closed by the time we name it.
    //
    // The guard, not `log_dir()`, is what says a log exists: `log_dir`
    // only resolves a path, while `setup_logging` also returns `None`
    // when creating that directory failed — in which case no subscriber
    // was ever installed and naming the file would send the user after
    // something that isn't there.
    if opts.log {
        let logging_started = log_guard.is_some();
        drop(log_guard);
        match Config::log_dir().filter(|_| logging_started) {
            // The appender rolls daily, so the file name carries a date
            // we'd need a date library to render.  Naming the directory
            // and the pattern is exact without that dependency — and
            // "written under" doesn't read as a file path the way the
            // bare directory did (`cat`ting it was the obvious next
            // move, and it is a directory).
            Some(dir) => eprintln!(
                "edamame: debug log written under {} (debug.log.<date>)",
                dir.display()
            ),
            None => eprintln!("edamame: --log could not open a log file; no log was written"),
        }
    }

    run_result
}

// ── Logging setup ─────────────────────────────────────────────────────────────

/// Initialise the file-based tracing subscriber.
///
/// Returns the non-blocking writer guard; dropping it flushes and closes the
/// log file. The guard must be kept alive for the duration of the program.
///
/// **Level.** The default filter is a bare `debug`, and both halves of that
/// are deliberate.  `tracing_subscriber::fmt()`'s own default is `info`,
/// which silently discarded every `debug!` in the crate into a file named
/// `debug.log` — the whole point of `[dev] logging` / `--log` is the
/// diagnostic trail (image decode dispatch and results, watcher events,
/// link handling), and essentially all of it is logged at `debug`.  It is
/// *unscoped* because the obvious `edamame=debug` would miss most of that
/// trail: `EnvFilter` matches on target, and the diagnostic call sites set
/// their own — `image`, `watcher`, `link`, `mouse`, `app` — none of which
/// live under the crate's target path.  Nothing in the dependency graph
/// pulls `tracing` (`cargo tree -i tracing` lists only this crate and the
/// subscriber), so an unscoped filter can't be flooded by a chatty
/// dependency.  `RUST_LOG` overrides it when a contributor wants something
/// narrower, or a `trace`.
fn setup_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let log_dir = Config::log_dir()?;
    if std::fs::create_dir_all(&log_dir).is_err() {
        return None;
    }

    let file_appender = tracing_appender::rolling::daily(&log_dir, "debug.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();

    tracing::info!("edamame starting");
    Some(guard)
}

/// Write a one-line summary of the detected terminal capabilities to the log.
fn log_capabilities(caps: &Capabilities) {
    tracing::info!(
        color_depth = ?caps.color_depth,
        mouse = caps.mouse,
        image_protocol = ?caps.image_protocol,
        unicode_full = caps.unicode_full,
        keyboard_enhancement = caps.keyboard_enhancement,
        "terminal capabilities detected"
    );
}
