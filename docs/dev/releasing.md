# Release checklist

> Part of the edamame contributor deep-dives. Index and project-wide conventions: [`AGENTS.md`](../../AGENTS.md). Sibling docs live in [`docs/dev/`](.).

## Releases via `dist` (formerly cargo-dist)

`dist-workspace.toml` holds the config (dist version, GitHub CI, `shell` + `homebrew` installers, the five targets, tap `mijowi/homebrew-tap`) and `.github/workflows/release.yml` is the generated workflow.

**Never hand-edit `.github/workflows/release.yml`.** `dist` owns it and it is coupled to the exact `dist` version that wrote it — run `dist init` / `dist generate` and commit the result (re-run `dist init` after upgrading `dist`). `dist plan` fails in CI when the file has drifted, and `release.yml` runs its `plan` job on every pull request as a dry run, so drift and config errors surface before a tag exists.

### Releasing

On `main`:

```bash
VERSION=0.1.0    # the new version, used by the commands below
PREV=0.0.9       # the previous release: the benchmark baseline to compare against
```

1. Write the `## [$VERSION]` section in `CHANGELOG.md` — it ships as the GitHub release body, the update-check modal's notes ([update-check.md](update-check.md)), and the post-upgrade notice ([post-upgrade.md](post-upgrade.md)).
2. Bump `version` in `Cargo.toml`, then `cargo update -p edamame` to sync `Cargo.lock`.
3. Verify lint and tests:

   ```bash
   cargo fmt -- --check
   cargo clippy --all-targets -- -D warnings
   cargo nextest run               # or: cargo test --no-fail-fast
   ```

4. Check for performance regressions against the previous release: one bench run, saved as this release's baseline for the next one, then compared against `v$PREV` without re-running. Use the same machine as last time, plugged in; full procedure and caveats in [performance.md](performance.md#checking-a-release-for-regressions):

   ```bash
   F='^(full_pipeline|visual_cache)'
   cargo bench --bench pipeline -- --noplot "$F" --save-baseline "v$VERSION"
   cargo bench --bench pipeline -- --noplot "$F" --load-baseline "v$VERSION" --baseline "v$PREV"
   ```

   Treat a "regressed" line as something to confirm, not a release blocker by itself. With no `v$PREV` baseline on this machine, run only the first command; this release's baseline starts the chain.

5. `dist plan` — confirm it announces `v$VERSION` and the five targets.
6. Commit all three files together:

   ```bash
   git add CHANGELOG.md Cargo.toml Cargo.lock
   git commit -m "chore(release): v$VERSION"
   ```

7. **Push `main` first, and wait for CI to go green.** The tag push is what triggers the release; pushing it ahead of the branch publishes from a commit that is not yet on any branch.

   ```bash
   git push origin main
   gh run watch
   ```

8. Tag and push the tag to trigger the release workflow (rerun with `gh run rerun <id>` if needed). It builds every target, creates the GitHub Release with the archives, checksums and `edamame-installer.sh`, and pushes the Homebrew formula to the tap.

   ```bash
   git tag -a "v$VERSION" -m "edamame v$VERSION"
   git push origin "v$VERSION"
   gh run watch
   ```

9. **Publish to crates.io by hand** — `publish-jobs` covers Homebrew only:

   ```bash
   cargo publish --dry-run
   cargo publish --locked
   ```

10. Verify: `gh release view v$VERSION`, `brew upgrade edamame`, `cargo info edamame`, and the docs.rs build.

### Prerequisites

1. **The tap repo must exist** — `github.com/mijowi/homebrew-tap`, empty is fine. The `publish-homebrew-formula` job pushes `edamame.rb` into it.
2. **`HOMEBREW_TAP_TOKEN` secret** — a PAT with write access to the tap repo, set in this repo's Actions secrets. The default `GITHUB_TOKEN` cannot push to another repository. Without it the release still publishes; only the Homebrew job fails.
3. **The version in `Cargo.toml` must match the tag** (`0.1.0` ↔ `v0.1.0`), and `Cargo.lock` must be committed in sync.

### Notes

- `aarch64-unknown-linux-gnu` is cross-compiled by `dist` (musl-cross container). The Linux targets build on `ubuntu-22.04`, which sets the glibc floor; the `-musl` target has none.
- **Dependabot must not touch `release.yml`**, or `dist plan` fails on the drift (PR #18). `dependabot.yml` has no file-level exclusion, so the three actions that file uses are ignored wholesale; `release.yml` gets them only by re-running `dist init`. **`actions/checkout` is also used by `ci.yml` and `audit.yml`, so it must be bumped there by hand.**
- **The MSRV pin is not an action version.** `ci.yml` uses `dtolnay/rust-toolchain@stable` with `toolchain: "1.90"` as an input, because Dependabot rewrites a version in the ref (`@1.90` → `@1.100`), silently turning the MSRV job into a latest-stable check. Don't put a Rust version back in the ref.
