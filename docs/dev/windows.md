# Windows: support stance and status

Windows is a **best-effort** platform, and will be until a Windows contributor shows up. This file is the single source of truth for what that means, what is verified, what is not, and how the verified half is enforced. The user-facing statement is the README's *Platforms* section and the *Windows and WSL* section of `docs/terminal-compatibility.md`; keep those consistent with this file when the stance changes.

## The stance

- edamame **must compile, lint, and pass its test suite** on `x86_64-pc-windows-msvc`. CI enforces that on every push and pull request, and a red Windows run blocks a merge the same way a red macOS run does. "Best-effort" describes what we *verify*, not what we tolerate in CI: a compile or deterministic-test failure on Windows is cheap to fix or `cfg`-gate. If a Linux-only contributor is stalled by a Windows-only failure, the escape hatch is a `#[cfg(not(windows))]` on the test with a comment saying why — never `continue-on-error`, which reports a red check on every PR even though the workflow passes.
- **Nobody runs the program on Windows.** Terminal behavior — ConPTY setup and restore, Windows Terminal vs. conhost, image protocols, the OS clipboard, `start`-based link opening, the `$EDITOR`-less external-editor path, `git difftool` walks — is unverified, and the user docs say so. Bug reports from Windows users are wanted and are triaged on the same footing as any other; nothing is dismissed for being Windows.
- **No Windows binary is shipped.** `dist-workspace.toml` has no Windows target; users build from source. Shipping one is a separate decision from supporting the platform, revisited when someone can actually run what comes out.
- **WSL is Linux.** It runs the Linux build and is covered by the Linux job. Its own limitations — no inotify on `/mnt/c`, no `xdg-open` by default, clipboard only with WSLg — come from WSL itself and are documented for users in `terminal-compatibility.md`; they are not tracked as edamame bugs.

Interactive testing without a Windows machine — a local KVM VM with a Microsoft eval image, or an Actions runner with an RDP tunnel — was considered and deliberately deferred. It is the route to promoting Windows to a supported platform if that ever becomes the goal, and this machine class can do it (KVM, enough RAM and disk); it is simply not worth the recurring cost for a platform nobody on the project uses.

## What is verified, and how

### In CI: the `windows` job

`.github/workflows/ci.yml` has a separate `windows` job (not a third `os:` entry in the Unix matrix — its steps differ). It runs on `windows-latest`:

1. `cargo clippy --all-targets --all-features -- -D warnings`. **This is the only place `arboard`'s Windows clipboard backend is compiled at all.** The `clippy` job is ubuntu-only and every test job runs `--no-default-features`, so without this step that code is linted by nothing. The `--no-default-features` lint is skipped here; the ubuntu job already covers the platform-neutral code that configuration reaches.
2. `cargo test --no-fail-fast --no-default-features`. Same feature choice as the Unix jobs and for the same reason: the OS clipboard is process-global, and tests would race on it.
3. The two `--ignored` watcher invocations, exercising `notify`'s `ReadDirectoryChangesW` backend — the only coverage that backend has. If it proves flaky under the runner's filesystem, gate *that step*, not the job.

Warm-cache wall clock is about 2 minutes against ubuntu's 40 seconds, which does not dominate the workflow. If it grows, the first thing to move is the watcher step, to a nightly `schedule:`.

### Locally: the msvc cross-check from Linux

The msvc target can be type-checked and linted from Linux, though not run:

```bash
rustup target add x86_64-pc-windows-msvc && cargo install cargo-xwin   # once
cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets --all-features -- -D warnings
cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets --no-default-features -- -D warnings
```

`cargo-xwin` rather than a plain `--target` because exactly one crate in the tree compiles C: `ring` (via `ureq` → rustls), which needs the MSVC CRT headers. `xwin` downloads the CRT and SDK into `~/.cache/cargo-xwin` on first use (a few hundred MB; it accepts Microsoft's SDK license on your behalf) and drives `clang-cl` / `llvm-lib` / `lld-link`, all of which Debian's `clang` package provides. `cargo xwin build` produces a real `.exe` too, but there is nothing to run it with, so the documented surface is check and clippy only. The `x86_64-pc-windows-gnu` target would build without the SDK but is not what anyone on Windows uses, and its `cfg` surface is nearly identical — a second target that finds nothing new.

Run both invocations before touching a `cfg(unix)` gate or a test module whose imports serve only `cfg(unix)` tests. `AGENTS.md` carries the same commands.

### Checkout: `.gitattributes`

`* text=auto eol=lf`. GitHub's Windows runner images set `core.autocrlf=true`, which without this line converts every LF file to CRLF at checkout — the `insta` snapshots, `tests/fixtures/*.md` (byte offsets feed the source map), and the `include_str!`d `config/config.toml` and `CHANGELOG.md`. This was observed, not guessed: the one test that reads a fixture from disk and depends on byte offsets failed on Windows and nowhere else. It also protects the CRLF-preservation feature's own tests, which construct their `\r\n` input in code and must not have the surrounding source rewritten.

## What is not verified

Every layer `cargo test` reaches through `TestBackend` — parser, renderer, editing model, diff, source map, modals — is platform-neutral and is proven on Windows by the job above. What remains unproven:

- Terminal setup and restore through ConPTY; `Picker::from_query_stdio` and the image protocols. Windows Terminal ≥ 1.22 has Sixel and no Kitty graphics; conhost has neither an alternate screen nor mouse reporting worth relying on.
- `open::that` (`cmd /C start`) for links and non-Markdown files.
- The external editor: `Command::new($EDITOR)` with the suspend/resume dance, and the `open::that` fallback when `$VISUAL` / `$EDITOR` are unset — which on Windows is the common case.
- `arboard` at runtime.
- `difftool::stop_walk`: there is no SIGINT, so it falls through to `process::exit(EXIT_INTERRUPTED)`, which stops a walk run with `--trust-exit-code` and nothing else. Whether Git for Windows hands `edamame --diff` a `/dev/null` or a `nul` for an absent side is also unknown; `diff_label` matches the former literally.
- **The custom-export runner has zero Windows test coverage.** All seven tests in `src/export/custom.rs` are `#[cfg(unix)]` because they shell out to `cp` / `cat` / `false`. Porting them (`cmd /C copy`, `cmd /C type`, `cmd /C exit 1`) is the first thing to hand a Windows contributor.

## Rules for tests, learned the hard way

The first full Windows run (2026-09-01, before the job was armed) failed seven lib tests — and, without `--no-fail-fast`, never built `tests/`, so the integration suite had never run on Windows at all. Every CI test step now passes `--no-fail-fast`. The seven had three causes, and each yields a rule:

**A `/`-rooted literal is not an absolute path on Windows.** `Path::new("/tmp/notes.md").is_absolute()` is `false` — there is no drive letter — so code that calls `std::path::absolute` prepends the cwd and code that checks `is_absolute()` takes its fallback branch. Five tests asserted correct behavior against a wrong fixture (`dirty_conflict::local_copy_path_*`, `save_copy_modal::save_as_default_*`, `export::custom::absolutize_*`, `config::config_dir_prefers_absolute_xdg_config_home`). The rule: **build paths from `tempfile::tempdir()` wherever a path reaches the filesystem or `absolute()`, and assert against `dir.path().join(..)`**; use a `cfg!(windows)`-selected literal (`C:\name` / `/name`, see `abs_root()` in `config::config::tests`) only for a pure function that never touches the disk. Never fix one of these with a `#[cfg(unix)]` gate — the point is that the behavior holds on Windows.

**A test of a Unix feature is Unix-only, and says so.** `difftool::read_side_reads_dev_null_as_an_empty_side` asserts on the OS's null device. It is `#[cfg(unix)]` with a comment, and an unconditional sibling (`read_side_reads_an_empty_file_as_an_empty_side`) covers the property everywhere. The rule: gate the *feature*, keep the *property* unconditional.

**An import that only gated tests use is an error on the other platform.** `export::custom`'s test module imported `std::sync::mpsc` for its seven `cfg(unix)` tests; on Windows that was an unused import and a hard error under `-D warnings`. Gate the import alongside the tests, with a comment. The local cross-check above catches this before CI does.

## Open items

- **Issue #2** ("Windows testing") — close with a pointer to this file once the stance is published.
- Port the seven `export::custom` tests — needs a Windows contributor, or the deferred VM.
- The `/dev/null` vs. `nul` question for `--diff` under Git for Windows — same.
- A Windows entry in `dist-workspace.toml` — only after someone has run a binary.
