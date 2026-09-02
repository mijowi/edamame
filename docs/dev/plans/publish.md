# Publish checklist

## Releases via `dist` (formerly cargo-dist)

**This is already set up.** `dist-workspace.toml` holds the config (dist 0.32.0, GitHub CI, `shell` + `homebrew` installers, the five targets, tap `mijowi/homebrew-tap`) and `.github/workflows/release.yml` is the generated workflow. `dist generate --check` passes and `dist plan` produces the expected matrix.

`dist` owns that workflow and it is coupled to the exact `dist` version that wrote it. **Never hand-edit `.github/workflows/release.yml`** — run `dist init` / `dist generate` and commit the result. `dist plan` fails in CI when the file has drifted, which is what it is for.

### Releasing

Ensure you are on `main`, then:

```bash
VERSION=0.1.0    # Whatever the new version number is. 
# This is for the commands that follow.
```

1. Write the `## [$VERSION]` section in `CHANGELOG.md` — it ships as the GitHub release body and as the update-check modal's notes.
2. Bump `version` in `Cargo.toml`, then `cargo update -p edamame` to sync `Cargo.lock`.
3. Verify lint and tests

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo nextest run
# cargo test --no-fail-fast     # alternative to nextest
```

4. `dist plan` — confirm it announces `v$VERSION` and the five targets.
5. Commit all three files together:

```bash
git add CHANGELOG.md Cargo.toml Cargo.lock
git commit -m "chore(release): v$VERSION"
```

6. **Push `main` first, and wait for CI to go green.**  The tag push is what triggers the release; pushing it ahead of the branch publishes from a commit that is not yet on any branch.

```bash
git push origin main
gh run watch            # wait for green before continuing
```

7. Tag and push the tag — this triggers the release workflow. If the workflow needs to be rerun: `gh run rerun <id>`.
```bash
git tag -a "v$VERSION" -m "edamame v$VERSION"
git push origin "v$VERSION"
gh run watch
```

The workflow builds every target, creates the GitHub Release with the archives + checksums + `edamame-installer.sh`, and pushes the Homebrew formula to the tap. `release.yml` also runs its `plan` job on every pull request as a dry run, so drift and config errors surface before a tag exists.

8. **Publish to crates.io by hand** — `publish-jobs` covers Homebrew only:

```bash
cargo publish --dry-run
cargo publish --locked
```

9. Verify: `gh release view v$VERSION`, `brew upgrade edamame`, `cargo info edamame`, and the docs.rs build.

### Prerequisites, in order

1. **The tap repo must exist** — `github.com/mijowi/homebrew-tap`, empty is fine. The `publish-homebrew-formula` job pushes `edamame.rb` into it.
2. **`HOMEBREW_TAP_TOKEN` secret** — a PAT with write access to the tap repo, set in this repo's Actions secrets. The default `GITHUB_TOKEN` cannot push to another repository. Without it the release still publishes; only the Homebrew job fails.
3. **The version in `Cargo.toml` must match the tag** (`0.1.0` ↔ `v0.1.0`), and `Cargo.lock` must be committed in sync.

### Notes

- `aarch64-unknown-linux-gnu` is cross-compiled by `dist` (musl-cross container); confirmed present in `dist plan`.
- glibc floor: the generated workflow builds the Linux targets on `ubuntu-22.04`. The `-musl` target sidesteps the question entirely.
- `dist init` must be re-run after upgrading `dist`, to regenerate the workflow.
- **Dependabot must not touch `release.yml`** — handled, but with a standing cost. PR #18 bumped `actions/checkout` there and `dist plan` failed on the drift. There is no file-level exclusion in `dependabot.yml` (`directories` globs at the directory level; no `exclude-paths` key exists), so the three actions that file uses are ignored wholesale. `actions/upload-artifact` and `actions/download-artifact` appear nowhere else and cost nothing; **`actions/checkout` is also used by `ci.yml` and `audit.yml` and now has to be bumped there by hand.** All three are bumped in `release.yml` only by re-running `dist init`.
- **The MSRV pin is not an action version.** The same PR rewrote `dtolnay/rust-toolchain@1.90` → `@1.100`, turning the MSRV job into a latest-stable check that still called itself "MSRV (1.90)". Fixed at the source rather than by an ignore: `ci.yml` now uses `@stable` and passes `toolchain: "1.90"` as an input, which Dependabot has no reason to touch. Don't put a Rust version back in the ref.

---

- [ ] **Re-enable the Windows CI job.** Disabled to `workflow_dispatch` only (`.github/workflows/ci.yml`) because `continue-on-error` still paints a red X on every PR's check list. Windows is a best-effort platform; the plan for the local cross-check, the four test fixes, and re-arming the job is [`windows.md`](windows.md).
- [ ] **Automate the crates.io publish.** Step 8 of Releasing is manual because   `dist`'s `publish-jobs` has no crates.io publisher — only `homebrew`, `npm`,   and `./user-defined` custom jobs. It needs a custom job plus a   `CARGO_REGISTRY_TOKEN` secret, wired through `dist-workspace.toml` and   `dist init` — never by hand-editing `release.yml`.

- [x] Add user-facing documentation — `docs/{getting-started,editing,keybindings,configuration,themes}.md`, plus corrections to `vim-mode.md` / `security.md`
- [x] Add a license - Apache 2.0 (`LICENSE`, verbatim text + appendix)
- [x] Update cargo.toml with license and other fields needed for crates.io publish — `cargo publish --dry-run` is clean (291 files, 1.2 MiB compressed)
- [x] Set up CI — `.github/workflows/ci.yml`: fmt, clippy (both feature configurations), test on ubuntu + macos, a non-blocking windows job, a docs build with `RUSTDOCFLAGS=-D warnings`, and `cargo audit`. All commands verified locally before commit.
- [x] Issue templates — `.github/ISSUE_TEMPLATE/{bug_report,feature_request,config}.yml`. The bug form requires terminal emulator + version, multiplexer, `$TERM`/`$COLORTERM`/`$TERM_PROGRAM`, and points at the capability summary and the clean-config check.
- [x] **Move everything in `issues.md` to GitHub issues**, then delete the file. Suggested labels: `bug`, `enhancement`, `docs`, `terminal-compat`, `good first issue`, `help wanted` — and delete GitHub's unused defaults (`duplicate`, `invalid`, `wontfix`). `terminal-compat` earns its place because those issues triage differently and often close as "your terminal, not the app".
- [x] Prune docs, especially plans — `docs/` is now user-facing only, `docs/dev/` holds design specs + plans and is excluded from the published crate
- [x] Diff edit mode removed. `Action::DiffEnterEdit` / `DiffExitEdit`, the `i` / `Enter` bindings, and the "coming soon" flash are all gone; `i` and `Enter` now fall through to the global keymap. Pinned by `diff_keys::tests::edit_sub_mode_keys_are_unbound`. The `src/diff/` design notes still describe a future Edit sub-mode — that groundwork is untouched, only the user-visible dead end was removed.
- [x] Turn on Dependabot alerts + security updates, and the dependency graph (Dependabot needs it).
- [x] Make a nice README with screenshots and videos
- [x] Release/tag v0.1.0 — see [Releases via `dist`](#releases-via-dist-formerly-cargo-dist) below
- [x] Compile binaries and add to GitHub Releases — automated by `dist` (`.github/workflows/release.yml`), triggered by the tag push. Targets: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl, aarch64-unknown-linux-gnu; Linux built on `ubuntu-22.04` for glibc compat.
- [x] Create the `mijowi/homebrew-tap` repo and add the `HOMEBREW_TAP_TOKEN` secret — **before** tagging, or the Homebrew job fails.
- [x] Set GitHub repo to public

After flipping repo to public:
- [x] Turn on private vulnerability reporting *(Settings → Security)* — **required**: `SECURITY.md`, `docs/security.md` and the README all link to it, and those links are dead until it is enabled.
- [x] Turn on secret scanning + push protection.
- [x] Add social preview image — the hero screenshot, once it exists.
- [x] Publish to crates.io
- [x] **Make the tap repo public.** It has to be *created* before tagging (above); flipping it public is what makes `brew install mijowi/tap/edamame` — as documented in the README — actually resolve.

