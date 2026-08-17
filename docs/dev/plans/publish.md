# Publish checklist

- [x] Make a nice README with screenshots and videos
- [ ] Release/tag v0.1.0 — see [Releases via `dist`](#releases-via-dist-formerly-cargo-dist) below
- [x] Compile binaries and add to GitHub Releases — automated by `dist` (`.github/workflows/release.yml`), triggered by the tag push. Targets: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl, aarch64-unknown-linux-gnu; Linux built on `ubuntu-22.04` for glibc compat.
- [ ] Create the `mijowi/homebrew-tap` repo and add the `HOMEBREW_TAP_TOKEN` secret — **before** tagging, or the Homebrew job fails.
- [ ] Set GitHub repo to public

After flipping repo to public:
- [ ] Turn on private vulnerability reporting *(Settings → Security)* — **required**: `SECURITY.md`, `docs/security.md` and the README all link to it, and those links are dead until it is enabled.
- [ ] Turn on secret scanning + push protection.
- [ ] Add social preview image — the hero screenshot, once it exists.
- [ ] Publish to crates.io
- [ ] **Make the tap repo public.** It has to be *created* before tagging (above); flipping it public is what makes `brew install mijowi/tap/edamame` — as documented in the README — actually resolve.

- [x] Add user-facing documentation — `docs/{getting-started,editing,keybindings,configuration,themes}.md`, plus corrections to `vim-mode.md` / `security.md`
- [x] Add a license - Apache 2.0 (`LICENSE`, verbatim text + appendix)
- [x] Update cargo.toml with license and other fields needed for crates.io publish — `cargo publish --dry-run` is clean (291 files, 1.2 MiB compressed)
- [x] Set up CI — `.github/workflows/ci.yml`: fmt, clippy (both feature configurations), test on ubuntu + macos, a non-blocking windows job, a docs build with `RUSTDOCFLAGS=-D warnings`, and `cargo audit`. All commands verified locally before commit.
- [x] Issue templates — `.github/ISSUE_TEMPLATE/{bug_report,feature_request,config}.yml`. The bug form requires terminal emulator + version, multiplexer, `$TERM`/`$COLORTERM`/`$TERM_PROGRAM`, and points at the capability summary and the clean-config check.
- [x] **Move everything in `issues.md` to GitHub issues**, then delete the file. Suggested labels: `bug`, `enhancement`, `docs`, `terminal-compat`, `good first issue`, `help wanted` — and delete GitHub's unused defaults (`duplicate`, `invalid`, `wontfix`). `terminal-compat` earns its place because those issues triage differently and often close as "your terminal, not the app".
- [x] Prune docs, especially plans — `docs/` is now user-facing only, `docs/dev/` holds design specs + plans and is excluded from the published crate
- [x] Diff edit mode removed. `Action::DiffEnterEdit` / `DiffExitEdit`, the `i` / `Enter` bindings, and the "coming soon" flash are all gone; `i` and `Enter` now fall through to the global keymap. Pinned by `diff_keys::tests::edit_sub_mode_keys_are_unbound`. The `src/diff/` design notes still describe a future Edit sub-mode — that groundwork is untouched, only the user-visible dead end was removed.
- [x] Turn on Dependabot alerts + security updates, and the dependency graph (Dependabot needs it).
- [ ] **Re-enable the Windows CI job.** Disabled to `workflow_dispatch` only (`.github/workflows/ci.yml`) because `continue-on-error` still paints a red X on every PR's check list. Four failures, all POSIX path-separator assumptions in the tests rather than product bugs: `app::modal::dirty_conflict::tests::local_copy_path_appends_dot_local{,_with_extension}`, `config::config::tests::config_dir_prefers_absolute_xdg_config_home`, `ui::save_copy_modal::tests::save_as_default_keeps_name_and_shows_absolute_directory`.

---

## Releases via `dist` (formerly cargo-dist)

**This is already set up.** `dist-workspace.toml` holds the config (dist 0.32.0, GitHub CI, `shell` + `homebrew` installers, the five targets, tap `mijowi/homebrew-tap`) and `.github/workflows/release.yml` is the generated workflow. `dist generate --check` passes and `dist plan` produces the expected matrix.

`dist` owns that workflow and it is coupled to the exact `dist` version that wrote it. **Never hand-edit `.github/workflows/release.yml`** — run `dist init` / `dist generate` and commit the result. `dist plan` fails in CI when the file has drifted, which is what it is for.

### Releasing

```bash
dist plan                  # dry run: shows the artifact matrix
```

Then, on the commit that should be v0.1.0:

```bash
git tag -a v0.1.0 -m "edamame v0.1.0"
git push origin v0.1.0     # the tag push is what triggers the workflow
```

The workflow builds every target, creates the GitHub Release with the archives + checksums + `edamame-installer.sh`, and pushes the Homebrew formula to the tap. `release.yml` also runs its `plan` job on every pull request as a dry run, so drift and config errors surface before a tag exists.

### Prerequisites, in order

1. **The tap repo must exist** — `github.com/mijowi/homebrew-tap`, empty is fine. The `publish-homebrew-formula` job pushes `edamame.rb` into it.
2. **`HOMEBREW_TAP_TOKEN` secret** — a PAT with write access to the tap repo, set in this repo's Actions secrets. The default `GITHUB_TOKEN` cannot push to another repository. Without it the release still publishes; only the Homebrew job fails.
3. **The version in `Cargo.toml` must match the tag** (`0.1.0` ↔ `v0.1.0`), and `Cargo.lock` must be committed in sync.
4. **Merge `pre-publish` to `main` first**, and tag the merge commit — a tag on a side branch produces a release whose source doesn't match the default branch.

### Notes

- `aarch64-unknown-linux-gnu` is cross-compiled by `dist` (musl-cross container); confirmed present in `dist plan`.
- glibc floor: the generated workflow builds the Linux targets on `ubuntu-22.04`. The `-musl` target sidesteps the question entirely.
- `dist init` must be re-run after upgrading `dist`, to regenerate the workflow.
- **Dependabot must not touch `release.yml`** — handled, but with a standing cost. PR #18 bumped `actions/checkout` there and `dist plan` failed on the drift. There is no file-level exclusion in `dependabot.yml` (`directories` globs at the directory level; no `exclude-paths` key exists), so the three actions that file uses are ignored wholesale. `actions/upload-artifact` and `actions/download-artifact` appear nowhere else and cost nothing; **`actions/checkout` is also used by `ci.yml` and `audit.yml` and now has to be bumped there by hand.** All three are bumped in `release.yml` only by re-running `dist init`.
- **The MSRV pin is not an action version.** The same PR rewrote `dtolnay/rust-toolchain@1.90` → `@1.100`, turning the MSRV job into a latest-stable check that still called itself "MSRV (1.90)". Fixed at the source rather than by an ignore: `ci.yml` now uses `@stable` and passes `toolchain: "1.90"` as an input, which Dependabot has no reason to touch. Don't put a Rust version back in the ref.
