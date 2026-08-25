# Publish checklist


## Prerequisites, in order

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
- [x] **Automate the crates.io publish.** Step 8 of Releasing is manual because   `dist`'s `publish-jobs` has no crates.io publisher — only `homebrew`, `npm`,   and `./user-defined` custom jobs. It needs a custom job plus a   `CARGO_REGISTRY_TOKEN` secret, wired through `dist-workspace.toml` and   `dist init` — never by hand-editing `release.yml`.

## crates.io Trusted Publishing (one-time setup)

`publish-crate.yml` authenticates with [Trusted Publishing]: GitHub mints a short-lived OIDC token, `rust-lang/crates-io-auth-action` exchanges it for a temporary crates.io token scoped to this crate, and that token is revoked when the job ends. No API token is ever stored in the repo or in Actions secrets.

Two one-time setups are needed — the GitHub environment and the crates.io trust config — and their **Environment name** fields must match.

**1. GitHub environment (the approval gate).** The `publish` job declares `environment: crates-io`. Create it under the repo's **Settings → Environments → New environment**, name it `crates-io`, and add yourself under **Required reviewers**. Each tag push then pauses the publish job until you approve it.

**2. crates.io trust config.** The crate must already exist on crates.io and you must be an owner (both true for `edamame`). Then, once per crate:

1. Sign in to <https://crates.io> and open the crate's **Settings** → **Trusted Publishing** → **Add**.
2. Fill in the form:
   - **Repository owner:** `mijowi`
   - **Repository name:** `edamame`
   - **Workflow filename:** `publish-crate.yml` (just the filename, not a path)
   - **Environment name:** `crates-io` (must match the GitHub environment above)
3. Save.

That's the entire server side. The matching GitHub half — `permissions: id-token: write`, the `crates-io` environment, and the auth action — is already in `publish-crate.yml`. 

[Trusted Publishing]: https://crates.io/docs/trusted-publishing
