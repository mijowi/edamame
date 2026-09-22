// A thin shell over the library crate, declaring no modules of its own: re-declaring them here
// would compile a second private copy of the tree, hiding its unit tests from `cargo test --lib`.
use std::path::PathBuf;

use anyhow::Result;

use edamame::app::{diff_label, difftool, is_markdown_pair, read_side, App};
use edamame::cli::{self, Invocation, RunOpts};
use edamame::config::{self, Config, LoadedConfig};
use edamame::terminal::{self, Capabilities, ColorDepth, TerminalSetup};

/// Usage-error exit status, by long-standing convention (1 means ran-and-failed).
const EXIT_USAGE: i32 = 2;

/// What [`run`] should put on screen.  A parameter rather than two copies of startup: the arms
/// share every step and differ only in what reaches the `App` afterwards.
enum Session {
    /// Normal editing session; `None` opens an empty, unnamed buffer.
    Open(Option<PathBuf>),
    /// Read-only `--diff` review, carrying the *contents* — read and compared by `main` before
    /// terminal setup, so an unreadable path or identical pair reports on the normal screen.
    Diff {
        old: String,
        new: String,
        label: String,
    },
}

fn main() -> Result<()> {
    // `args_os`, not `args`: the latter panics on a non-UTF-8 argument, a legal Linux file name.
    let invocation = Invocation::parse(std::env::args_os().skip(1)).unwrap_or_else(|e| {
        eprintln!("edamame: {e}\n\n{}", cli::USAGE);
        std::process::exit(EXIT_USAGE);
    });

    match invocation {
        // The informational flags never touch the config directory.  `--doctor` enters the
        // alternate screen for its probe but draws no frame and prints after restoring.
        Invocation::Help => {
            print!("{}", cli::help_text());
            Ok(())
        }
        Invocation::Version => {
            println!("{}", cli::version_line());
            Ok(())
        }
        Invocation::Doctor => cli::run_doctor(),
        Invocation::Run { file, opts } => run(Session::Open(file), opts),
        Invocation::Diff { old, new, opts } => {
            // Three ways a pair is declined, all of them exit 0: under `--trust-exit-code` a
            // non-zero status abandons every file behind it.  Ending the walk deliberately is a
            // signal, not a status — see `difftool::stop_walk`.
            //
            // Not Markdown.  git invokes a difftool on every changed path, and edamame has
            // nothing to offer a `.rs` or a `.png`.  Checked before the read, so a binary file is
            // declined by its name and never reaches a UTF-8 decode.  Named by `diff_label`
            // because git's side of the pair is a temp copy under its own scratch directory.
            let label = diff_label(&old, &new);
            if !is_markdown_pair(&old, &new) {
                eprintln!("edamame: {label} is not Markdown — skipped");
                return Ok(());
            }
            // Unreadable — not valid UTF-8, or gone since git wrote it.
            let sides = read_side(&old).and_then(|o| read_side(&new).map(|n| (o, n)));
            let (old_text, new_text) = match sides {
                Ok(pair) => pair,
                Err(e) => {
                    eprintln!("edamame: {e:#}");
                    return Ok(());
                }
            };
            // Identical.  git decides "differs" from the index, so a mode-only change or a
            // normalizing filter still reaches us with two identical files.
            if old_text == new_text {
                eprintln!("edamame: {label} has no differences to review");
                return Ok(());
            }
            run(
                Session::Diff {
                    old: old_text,
                    new: new_text,
                    label,
                },
                opts,
            )
        }
    }
}

/// Load config, set up the terminal, probe capabilities, run the app, restore.
fn run(session: Session, opts: RunOpts) -> Result<()> {
    // A `--diff` review opens no file: `file_path` drives the watcher and any save, and both
    // would point at temp files git deletes the moment we exit.  It gets a display label instead.
    let file_path = match &session {
        Session::Open(path) => path.clone(),
        Session::Diff { .. } => None,
    };

    // ── Split a `file.md#section` deep link and validate the file ──
    // Split here rather than in `Invocation::parse`: `#` is legal in a file name, so the rule asks
    // the disk before taking one away from a path, and the parser is pure.
    //
    // Both steps happen *before* `terminal::setup` on purpose.  A directory or binary file makes
    // `Buffer::load_file` fail from inside `App::new`, and that failure has no path back to
    // `terminal::restore` — it left the shell wrecked.  Refusing here reports on the normal screen.
    let (file_path, startup_anchor) = match file_path {
        Some(path) => {
            let (path, anchor) = cli::split_startup_anchor(&path);
            if let Err(e) = preflight_open(&path) {
                eprintln!("edamame: {e}");
                std::process::exit(1);
            }
            (Some(path), anchor)
        }
        None => (None, None),
    };

    // ── Load configuration ─────────────────────────────────────────
    // Scaffold FIRST, so a first-run user's `load` finds the theme file already written.
    // `ensure_default_files` never overwrites, and a scaffolding failure still falls back to the
    // compiled `Theme::default()`.
    //
    // Both steps need the color depth: the scaffolder seeds a 256-color theme on an indexed
    // terminal, and `Config::load` picks the same capability-appropriate built-in for a missing
    // theme file.  The full probe writes escape sequences and must run after `terminal::setup` —
    // too late — so this is a one-bit early read from the environment only, giving the same answer
    // the probe will compute later.  The probe stays the source of truth for everything else.
    //
    // `--no-config` short-circuits all three files; `disable_config_dir` below closes the write
    // half for the whole process.
    let truecolor_at_load = Capabilities::detect_color_depth_from_env() == ColorDepth::TrueColor;
    let loaded = if opts.no_config {
        LoadedConfig::default()
    } else {
        Config::ensure_default_files(truecolor_at_load);
        Config::load(truecolor_at_load, true).unwrap_or_else(|e| {
            // Non-fatal.  Not `tracing` — the subscriber isn't set up yet.
            eprintln!("Warning: failed to load config: {e}. Using defaults.");
            LoadedConfig::default()
        })
    };
    let LoadedConfig {
        mut config,
        keybindings,
        theme,
        state,
        warnings: config_warnings,
    } = loaded;

    // ── Apply run flags on top of the loaded config ────────────────
    if opts.no_config {
        // Takes the directory out of play for reads too, before `App` exists.  Skipping the load
        // is only the startup half — the theme picker and stylesheet list read it mid-session.
        config::disable_config_dir();

        // The welcome modal captures first-run choices *to disk* and opens non-dismissable, so
        // with saving suppressed it is an unskippable prompt with no outcome.  The capabilities
        // notice stays: it is dismissable, and it reports what `--no-config` users came for.
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

    // Restore the terminal on panic.  The hook runs *before* unwinding and cannot see whether
    // anyone will catch the panic, so guarded sections tell it via `terminal::ExpectedPanic`:
    // restoring for one of those left the app still running with no alt screen and no raw mode,
    // strictly worse than the clean crash this hook exists to give.  Chaining to the default hook
    // is suppressed for the same reason — it prints the payload straight through the TUI.
    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if terminal::panic_is_expected() {
            tracing::warn!(%info, "caught a panic in a guarded section");
            return;
        }
        let _ = terminal::restore();
        orig_hook(info);
    }));

    // ── Detect capabilities ───────────────────────────────────────
    // AFTER EnterAlternateScreen, so the probe talks to the live terminal; BEFORE `app.run`
    // spawns its event reader, whose competing reads would eat the escape-sequence replies.
    let capabilities = Capabilities::detect(keyboard_enhancement, config.images.sharp_scrolling);
    log_capabilities(&capabilities);

    // ── Enable mouse reporting ────────────────────────────────────
    // Only where advertised: a terminal that doesn't understand the enable sequence echoes it as
    // literal output.  Failure is non-fatal.
    if capabilities.mouse {
        if let Err(e) = terminal::enable_mouse() {
            tracing::warn!(error = %e, "failed to enable mouse capture");
        }
    }

    // ── Run the app ───────────────────────────────────────────────
    // `App::new` can still fail for reasons the pre-flight can't foresee, so restore before
    // propagating — the panic hook only fires on an actual panic.
    let mut app = match App::new(
        config,
        state,
        keybindings,
        theme,
        file_path,
        capabilities,
        config_warnings,
    ) {
        Ok(app) => app.with_startup_anchor(startup_anchor),
        Err(e) => {
            let _ = terminal::restore();
            return Err(e);
        }
    };
    if let Session::Diff { old, new, label } = session {
        app.set_diff_label(Some(label));
        // `main` established the sides differ, so this cannot decline — but restore before
        // erroring rather than trust that.
        if !app.enter_read_only_diff(old, new) {
            terminal::restore()?;
            anyhow::bail!("no differences to review");
        }
    }
    let run_result = app.run(terminal);
    let diff_stop_walk = app.diff_stop_walk();

    // ── Restore terminal ──────────────────────────────────────────
    terminal::restore()?;

    // Dropped here, unconditionally: the drop flushes and closes the log, and the difftool-abort
    // path below leaves via `std::process::exit`, which runs no destructors.  Logging is also
    // enabled by config, so deferring into the `--log` branch discarded those sessions' tails.
    //
    // The guard, not `log_dir()`, is what says a log exists — `setup_logging` returns `None` when
    // the directory couldn't be created, and naming a file then sends the user after nothing.
    let logging_started = log_guard.is_some();
    drop(log_guard);

    // After `restore`, so the line lands on the normal screen, and after the drop, so the file is
    // closed by the time it is named.
    if opts.log {
        match Config::log_dir().filter(|_| logging_started) {
            // The appender rolls daily, so the name carries a date we'd need a date library to
            // render; naming the directory plus the pattern is exact without that dependency.
            Some(dir) => eprintln!(
                "edamame: debug log written under {} (debug.log.<date>)",
                dir.display()
            ),
            None => eprintln!("edamame: --log could not open a log file; no log was written"),
        }
    }

    run_result?;
    // `Esc` returns normally and git moves on; `Quit` means "quit the whole walk", which an exit
    // code cannot express — git discards a difftool's status without `--trust-exit-code` — so the
    // process group is signalled instead.  Last, because nothing below this line runs.
    if diff_stop_walk && difftool::under_git_difftool() {
        difftool::stop_walk();
    }
    Ok(())
}

/// Refuse a file edamame cannot open — a directory or a non-text file — *before* the terminal is
/// set up, since the same failure from inside `App::new` has no path back to `terminal::restore`.
///
/// A *non-existent* path passes: `App::new` opens it as a new buffer, like `vim`.  There is
/// deliberately no extension gate — a file the user named explicitly is opened whatever it is
/// called, Markdown being a superset of plain text.  The `--diff` path is the opposite, and *is*
/// gated, because git invokes it unattended on every changed file.
///
/// The UTF-8 check re-reads the file `App::load_file` will read again; cheap at Markdown sizes,
/// and it buys a pre-terminal decision without threading bytes through `App::new`.
fn preflight_open(path: &std::path::Path) -> Result<(), String> {
    // A path that can't be stat'd is a new file; `App::new` handles it.
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    if meta.is_dir() {
        return Err(format!("{} is a directory", path.display()));
    }
    if let Err(e) = std::fs::read_to_string(path) {
        return Err(format!("cannot open {}: {e}", path.display()));
    }
    Ok(())
}

// ── Logging setup ─────────────────────────────────────────────────────────────

/// Initialize the file-based tracing subscriber, returning the writer guard that must stay alive
/// for the program's duration (dropping it flushes and closes the log).
///
/// **The default filter is a bare `debug`, and both halves of that are deliberate.** `fmt()`'s own
/// default is `info`, which discards every `debug!` — and essentially the whole diagnostic trail
/// is at `debug`.  It is *unscoped* because `EnvFilter` matches on target and the diagnostic call
/// sites set their own (`image`, `watcher`, `link`, `mouse`, `app`), none under the crate's target
/// path; nothing in the dependency graph pulls `tracing`, so it cannot be flooded.  `RUST_LOG`
/// overrides it.
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

/// One-line summary of the detected capabilities, for the log.
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
