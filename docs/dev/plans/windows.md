# Windows: best-effort support

## Decision

Windows is a **best-effort** platform until a Windows contributor shows up. Concretely:

- edamame must **compile, lint, and pass its test suite** on `x86_64-pc-windows-msvc`, and CI enforces that on every push and pull request, the same as Linux and macOS.
- Nobody smoke-tests the running TUI on Windows. Terminal behavior (ConPTY, Windows Terminal vs. conhost, image protocols, the OS clipboard, `start`-based link opening, the `$EDITOR`-less external-editor path, `git difftool` walks) is **unverified**, and the docs say so.
- No Windows target is added to `dist-workspace.toml`. Users build from source (`cargo install edamame`). Shipping a binary is a separate decision, revisited when someone can actually run one.
- WSL is out of scope here. It runs the Linux binary and is covered by the Linux job; its own bug class (no inotify on `/mnt/c`, no `xdg-open`, clipboard without WSLg) is an environment matter, tracked separately if at all.

Nothing in this plan requires a Windows machine. Interactive testing (a local KVM VM, or an Actions runner with a tunnel) was considered and deliberately deferred.

Findings this rests on, established 2026-09-01:

- `cargo check --target x86_64-pc-windows-msvc` on Linux gets through the whole dependency tree and fails on exactly one crate: **`ring`** (via `ureq` → rustls), and only because the MSVC CRT headers (`assert.h`) are absent. `clang-cl` and `llvm-lib` already work as the compiler and archiver. The missing piece is the SDK, which is exactly what `cargo-xwin` provides.
- The Windows CI job has run once (`workflow_dispatch`) and reported **four failures, all in tests**, all POSIX path-separator assumptions. They are listed under Tier 2 with their fixes.
- The clippy job is `ubuntu-latest` only and the test jobs run `--no-default-features`, so **`arboard`'s Windows clipboard backend is compiled and linted by nothing.** Tier 1 closes this locally; Tier 2 closes it in CI.

## Tier 1 — local cross-check from Linux

Goal: a contributor on Linux can type-check and lint the Windows build in minutes, on every change, before CI sees it. `cargo-xwin` produces a real `.exe` as well, but it cannot run one, so this tier is compile + clippy only.

### Steps

- [x] **Install the toolchain.** Once per machine:

  ```bash
  rustup target add x86_64-pc-windows-msvc
  cargo install cargo-xwin        # needs clang + llvm-lib + lld-link on PATH
  ```

  `cargo-xwin` downloads the MSVC CRT and Windows SDK into `~/.cache/cargo-xwin` on first use (a few hundred MB; it accepts Microsoft's SDK license on your behalf — read `cargo xwin --help` if that matters to you). Debian's `clang` package supplies all three LLVM tools.

- [x] **Verify both configurations are clean.** These mirror the ubuntu clippy job plus the Windows-specific feature set:

  ```bash
  cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets --all-features -- -D warnings
  cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets --no-default-features -- -D warnings
  ```

  Done 2026-09-01. The whole tree compiles for msvc, `arboard`'s Windows backend included. The single finding was in `export/custom.rs`: the test module's `use std::sync::mpsc` served only the seven `cfg(unix)` tests, so on Windows it was an unused import and a hard error under `-D warnings`. Now `#[cfg(unix)]`-gated with a comment. Fix anything further it reports. A warning in a *dependency* is not ours to fix — `-D warnings` is passed to cargo, not through `RUSTFLAGS`, for the same reason `ci.yml` does it that way.

- [x] **Add a `cfg`-audit pass while the build is fresh.** `grep -rn 'cfg(unix)' src/` currently lists `app/difftool.rs::stop_walk` (SIGINT delivery; the non-Unix fallthrough to `process::exit(EXIT_INTERRUPTED)` is intentional and already commented), `cli/args.rs`, and the seven `#[cfg(unix)]` tests in `export/custom.rs`. Confirmed by the clean `--all-targets` run above, which compiles every non-Unix branch.

- [x] **Document it.** Add a short "Cross-checking the Windows build" block to the Lint section of `AGENTS.md` with the two commands above and the one-line reason (`ring` needs the SDK; `cargo-xwin` supplies it). Keep it to the commands — the *why* lives in this file.

### Not in scope

- `cargo xwin build` for an actual `.exe` works but has no consumer without a way to run it. Don't add it to docs until Tier 3 exists.
- `x86_64-pc-windows-gnu` (mingw). It would build without the SDK, but it is not the target anyone on Windows uses, and its `cfg` surface is nearly identical to msvc — a second target that finds nothing new.

## Tier 2 — a standing Windows signal in CI

Goal: the `windows` job in `.github/workflows/ci.yml` runs on every push and PR, and is green. Today it is gated on `workflow_dispatch` with `continue-on-error: true`, because that combination still paints a red X on every PR (the job's own check fails even though the workflow passes).

### Steps

- [x] **Re-baseline first.** Run 33588933260 (2026-09-01) against `main` at `cfd3d8d`: **7 failures in the lib target, 2210 passed** — and because the job had no `--no-fail-fast`, `tests/` was never built, so the integration suite had never run on Windows at all. The re-armed job passes `--no-fail-fast` for that reason. The suspected latent candidates (`config_warning`, `tests/mouse.rs`, `tests/vim.rs`) were *not* among the failures — their literals are display-only or never reach an `is_absolute` check.

- [x] **Fix the seven test failures.** Three causes:

  *Five `/`-rooted literals.* A `/`-rooted path is not absolute on Windows (`Path::new("/tmp/notes.md").is_absolute()` is `false` — there is no drive letter), so code that calls `std::path::absolute` prepends the cwd, and code that checks `is_absolute()` takes its fallback branch. The tests were asserting the intended behavior; only the fixtures were wrong.

| Test | Fix |
|---|---|
| `app::modal::dirty_conflict::tests::local_copy_path_appends_dot_local_{with,without}_extension` | Input from `tempfile::tempdir()`, expectation from `dir.path().join(..).display()` |
| `ui::save_copy_modal::tests::save_as_default_keeps_name_and_shows_absolute_directory` | Same; the relative and unnamed halves already compared against `cwd.join(..)` |
| `export::custom::tests::absolutize_makes_a_relative_path_absolute` (new since the plan) | Same, for the already-absolute half |
| `config::config::tests::config_dir_prefers_absolute_xdg_config_home` | `resolve_config_dir` is pure, so no tempdir: an `abs_root()` helper picks `C:\name` / `/name` by `cfg!(windows)`. `config_dir_falls_back_to_dot_config_on_every_platform` got the same treatment, since its `/home/u` home was only absolute on Unix |

  Rule applied: real paths from `tempdir()` wherever a path reaches the filesystem or `absolute()`; a `cfg!`-selected literal only for a pure function. None of these got a `#[cfg(unix)]` gate — the point is that the behavior holds on Windows.

  *One Unix feature.* `app::difftool::tests::read_side_reads_dev_null_as_an_empty_side` (new since the plan) asserts on the OS's null device, which is legitimately Unix-only; it is now `#[cfg(unix)]` with a comment, and a new unconditional `read_side_reads_an_empty_file_as_an_empty_side` covers the property on every platform. Whether Git for Windows hands `edamame --diff` a `/dev/null` or a `nul` for an absent side is unverified — a question for a Windows contributor.

  *One CRLF checkout.* `editor::state_source_lines::tests::fixture_labels_are_unique_ascending_and_agree_with_the_cursor` (new since the plan) reads `tests/fixtures/sample.md` from disk and was off by one row. That is the `core.autocrlf=true` conversion described in the next step, observed rather than suspected. No test change; `.gitattributes` is the fix.

- [x] **Pin line endings at checkout.** GitHub's Windows runner images configure `core.autocrlf=true`, and the repo has no `.gitattributes`, so every LF file may arrive as CRLF: the 13 `insta` snapshots, `tests/fixtures/*.md` (byte offsets feed `source_map`), and the `include_str!`d `config/config.toml` and `CHANGELOG.md`. The baseline run confirmed it: the one test that reads a fixture from disk and depends on byte offsets failed on Windows and nowhere else. Add:

  ```gitattributes
  * text=auto eol=lf
  ```

  This also protects the CRLF-preservation feature's own tests, which construct their `\r\n` input in code and must not have the *surrounding* source rewritten.

- [x] **Widen what the job checks.** The current job runs `cargo test --no-default-features` only. Bring it to parity with the Unix jobs and add the one thing they can't cover:

```yaml
- run: cargo clippy --all-targets --all-features -- -D warnings   # the only place arboard's Windows backend is linted
- run: cargo test --no-default-features
- name: Test (watcher — needs live filesystem notifications)
shell: bash
run: |
  cargo test --no-default-features --test watcher -- --ignored
  cargo test --no-default-features --lib -- --ignored watcher::
```

  `shell: bash` because the runner's default shell on Windows is `pwsh`; the multi-line `run:` works either way but bash keeps it identical to the Unix step. The watcher step exercises `notify`'s `ReadDirectoryChangesW` backend, currently exercised by nothing — the `#[ignore]` filter reasoning in `AGENTS.md` applies unchanged. If the Windows notifications prove flaky under the runner's filesystem, gate *that step* rather than dropping the job.

  Keep `--no-default-features` on the test invocations for the reason the Unix jobs give: the OS clipboard is process-global and the tests would race on it. Skip the `--no-default-features` clippy pass here — the ubuntu job already lints the platform-neutral code that configuration reaches, and Windows minutes cost more.

- [x] **Re-arm the job.** Delete the `if: github.event_name == 'workflow_dispatch'` and `continue-on-error: true` lines. Kept as a separate named job rather than a third matrix entry: its step list differs (the extra `--all-features` clippy), and a conditional step inside the matrix is exactly the awkwardness that argued against folding. Rewrite the comment block above the job: it currently explains why the job is disabled and lists the four failures; it should now say Windows is best-effort, that the job is the *only* Windows verification, and where the cfg-gated Unix-only tests live so a future Windows contributor knows what to port.

- [ ] **Job time.** Windows runners build Rust roughly 2–3× slower than ubuntu. `Swatinem/rust-cache` is already in the job and works on Windows; check the first few runs' wall clock and, if it dominates the workflow, consider trimming to `cargo test --no-default-features` plus the clippy step and dropping the watcher step to a nightly `schedule:` instead.

- [x] **Say it in the docs.** User-facing — a `### Platforms` section in the README's install block, since `getting-started.md` has no install section and the README owns that: Windows builds from source and passes CI, is not smoke-tested, and Windows Terminal is the only console likely to work — legacy conhost has no alternate screen or mouse support worth relying on. `docs/terminal-compatibility.md:117` keeps its `?` row: those cells are observations, and there are none. Contributor-facing: tick the "Re-enable the Windows CI job" box in `publish.md` and point it here.

### What this tier does not prove

Every layer that `cargo test` exercises through `TestBackend` — parser, renderer, editing model, diff, source map, modals — is platform-neutral and is proven. What remains unproven is exactly the list in the decision above: terminal setup and restore through ConPTY, `Picker::from_query_stdio`, `open::that` (`cmd /C start`), `Command::new($EDITOR)` and the `open::that` fallback when it is unset, `arboard` at runtime, and `difftool::stop_walk`'s non-signal exit. The custom-export runner (`export/custom.rs`) has **zero** test coverage on Windows because all seven of its tests shell out to `cp`/`cat`/`false`; porting them (`cmd /C copy`, `cmd /C type`, `cmd /C exit 1`) is the first thing to hand a Windows contributor, and the `cfg(unix)` gates should carry a comment saying so.

## Definition of done

- `cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets --all-features -- -D warnings` is clean on a Linux checkout, and `AGENTS.md` says how to run it.
- The `windows` job runs on push and PR with no `if:` and no `continue-on-error`, and has been green on `main` for at least two consecutive pushes.
- `.gitattributes` pins LF.
- The README's install section states the best-effort status; `publish.md` and issue #2 point here.
