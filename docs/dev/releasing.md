# Release checklist

> Part of the edamame contributor deep-dives. Index and project-wide conventions: [`AGENTS.md`](../../AGENTS.md). Sibling docs live in [`docs/dev/`](.).

## Releases via `dist` (formerly cargo-dist)

`dist-workspace.toml` holds the config (dist version, GitHub CI, `shell` + `homebrew` installers, the six targets, tap `mijowi/homebrew-tap`, the crates.io publish job) and `.github/workflows/release.yml` is the generated workflow.

**Never hand-edit `.github/workflows/release.yml`.** `dist` owns it and it is coupled to the exact `dist` version that wrote it — run `dist init` / `dist generate` and commit the result (re-run `dist init` after upgrading `dist`). `dist plan` fails in CI when the file has drifted, and `release.yml` runs its `plan` job on every pull request as a dry run, so drift and config errors surface before a tag exists.

A tag push runs, in order: the builds for every target, `host` (creates the public GitHub Release with the archives — a `.zip` for Windows — checksums and `edamame-installer.sh`), then two publish jobs in parallel: `publish-homebrew-formula` (pushes `Formula/edamame.rb` to the tap) and `custom-publish-crates` ([`publish-crates.yml`](../../.github/workflows/publish-crates.yml), `cargo publish` through crates.io Trusted Publishing, in the `crates-io` environment). A failed publish job fails the run but does not retract the release.

### Releasing

[`scripts/release.sh`](../../scripts/release.sh) runs the procedure in five subcommands. Each is one stage, run in order, and each can be re-run after a failure: `push` and `release` check what is already done (commit made, `main` pushed, tag created, tag pushed) and skip it. Every subcommand after `prepare` reads the version from `Cargo.toml`. Before each push, `push` and `release` ask for confirmation; `-y` skips the prompts. It runs on bash 3.2 (macOS's `/bin/bash`) and later.

`push` and `release` are separate so nothing irreversible happens until you choose to: `push` ends once `main` is green, leaving time to review it on GitHub, and only `release` creates the tag that publishes.

The release commit is made on `develop` and fast-forwarded onto `main`, so `main` never receives a commit `develop` lacks and no rebase is needed afterwards.

1. **Write the release notes** under `## [Unreleased]` in `CHANGELOG.md`, on `develop`. That section ships as the GitHub release body, the update-check modal's notes ([update-check.md](update-check.md)), and the post-upgrade notice ([post-upgrade.md](post-upgrade.md)). Commit them.

2. **`scripts/release.sh prepare X.Y.Z`** refuses unless it is on a clean `develop` that includes `origin/develop` and `origin/main`, the version is newer than `Cargo.toml`'s, the tag does not exist, and `[Unreleased]` has content. It then:
   - renames `[Unreleased]` to `## [X.Y.Z] - <today>` under a fresh empty `[Unreleased]`;
   - rewrites the footer link block the way `tests/changelog.rs` checks it;
   - bumps `version` in `Cargo.toml` and runs `cargo update -p edamame`.

   It commits nothing; review with `git diff`.

3. **`scripts/release.sh bench`** checks for performance regressions against the previous release tag. It benches that tag from a temporary worktree and the current tree back to back, both pinned with `taskset` to CPU 3. Bench plugged in. `PREV=X.Y.Z` overrides the tag and `CORE=N` the core. It refuses when the harness changed since the tag, because the comparison would be meaningless: anything under `benches/`, the `criterion` line or a `[[bench]]` table in `Cargo.toml`, or the `criterion` version `Cargo.lock` resolves. Treat a "regressed" line as something to confirm, not a release blocker by itself; procedure and caveats in [performance.md](performance.md#checking-a-release-for-regressions).

4. **`scripts/release.sh push`**, from `develop`. Before anything slow, it refuses if `origin/develop` has commits `develop` lacks, if `origin/main` has commits `develop` lacks, if the tag already exists anywhere but the release commit, if there is an uncommitted change outside the three release files, or if `CHANGELOG.md` has no `[X.Y.Z]` section. It then:
   1. runs the checks: `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run --no-fail-fast` plus `cargo test --doc` (nextest cannot run doctests; without nextest, `cargo test --no-fail-fast` covers both), `dist plan --tag vX.Y.Z --output-format=json` (fails unless it announces that tag and lists at least one archive), and `cargo publish --dry-run --locked`. CI on `main` runs neither `dist plan` nor the publish dry run, so these are the only place a packaging error is caught before the release is public. They are skipped only once `origin/main` is at the release commit;
   2. commits the three release files as `chore(release): vX.Y.Z`;
   3. fast-forwards `origin/main` to it (`git push origin HEAD:main`) and updates the local `main` (only a warning if that fails);
   4. waits for that commit's CI run to pass.

   If a check fails and the fix changes code the benches exercise, run `bench` again before re-running `push`, since the earlier results no longer describe what ships; a formatting or lint fix doesn't need it.

   When a run fails, the script prints the `gh run view` / `gh run rerun` commands; after a rerun passes, run `push` again. If the watch itself stops (Ctrl-C, a network drop) while the run is still going, the script says so instead, and re-running the subcommand resumes waiting. Then review `main` on GitHub: the rendered changelog section, the CI run, the diff since the previous tag.

5. **`scripts/release.sh release`**, from `develop`. It refuses unless `HEAD` is the release commit and `origin/main` is at it. It then:
   1. re-checks that commit's CI run, rather than trusting the earlier `push` — **CI must be green before the tag exists**, because the tag push publishes regardless;
   2. creates and pushes the annotated `vX.Y.Z` tag;
   3. waits for the release workflow (once the run has finished, a failure confined to the crates.io job is accepted if crates.io lists the version, so a hand publish lets `release` continue);
   4. pushes `develop`.

6. **`scripts/release.sh verify`** checks the published release:
   - the GitHub release has every archive `dist plan` lists for the tag;
   - the release workflow succeeded (as in `release`, a failure confined to the crates.io job passes once crates.io lists the version);
   - the tap's formula is at the new version;
   - crates.io lists the version;
   - docs.rs built it.

   docs.rs can lag by several minutes; re-run until it passes. Then `brew upgrade edamame` on a Mac, and skim the user docs for anything the release changed.

### Recovering by hand

The script only wraps commands; any stage can be done manually. `push` and `release` resume from whichever of these steps already happened:

```bash
VERSION=0.2.0
git commit -m "chore(release): v$VERSION" -- CHANGELOG.md Cargo.toml Cargo.lock
git push origin HEAD:main && gh run watch --exit-status   # pick the CI run on main
git tag -a "v$VERSION" -m "edamame v$VERSION"
git push origin "v$VERSION" && gh run watch --exit-status
git push origin develop
```

If only `custom-publish-crates` failed (for example, before Trusted Publishing was configured), rerun it with `gh run rerun <id> --failed`, or publish from a checkout of the tag with `cargo publish --locked` and a personal token. Either way, run `release` again afterwards to push `develop`.

If a build job fails for a transient reason (a runner or network error), rerun it with `gh run rerun <id> --failed` and run `release` again.

If a build fails because the code needs a fix, the script cannot resume: the release commit is already on `main`, so the fix has to land on top of it, and both `push` and `release` require `HEAD` to be the release commit. `host` runs only after every build succeeds, so nothing has been published yet — confirm that `gh release view vX.Y.Z` finds no release — and the tag can be moved to the fix:

```bash
VERSION=0.2.0
git push origin ":refs/tags/v$VERSION" && git tag -d "v$VERSION"
# commit the fix on develop, then:
git push origin HEAD:main && gh run watch --exit-status   # pick the CI run on main
git tag -a "v$VERSION" -m "edamame v$VERSION"
git push origin "v$VERSION" && gh run watch --exit-status
git push origin develop
scripts/release.sh verify
```

Run `dist plan --tag "v$VERSION"` and `cargo publish --dry-run --locked` before re-tagging if the fix touches packaging; `push` would have run them. If the GitHub Release or the crate *was* published, the tag must not move: release `X.Y.Z+1` instead.

### Prerequisites

1. **The tap repo must exist** — `github.com/mijowi/homebrew-tap`, empty is fine. The `publish-homebrew-formula` job pushes `Formula/edamame.rb` into it.
2. **`HOMEBREW_TAP_TOKEN` secret** — a PAT with write access to the tap repo, set in this repo's Actions secrets. The default `GITHUB_TOKEN` cannot push to another repository. Without it the release still publishes; only the Homebrew job fails.
3. **A `crates-io` environment restricted to release tags.** In this repo's *Settings → Environments*, `crates-io` must allow deployments only from tags matching `v*` (*Deployment branches and tags → Selected branches and tags*), with no required reviewers. `publish-crates.yml` runs its job in it, so a run on any other ref is refused before it can request a token.
4. **A crates.io trusted publisher for `release.yml`.** On crates.io, under the crate's *Settings → Trusted Publishing*, add a GitHub publisher: owner `mijowi`, repository `edamame`, workflow filename **`release.yml`**, environment **`crates-io`**. It must name `release.yml`, not `publish-crates.yml`: crates.io matches the OIDC token's `workflow_ref` claim, which names the *calling* workflow. The environment is matched too, so with the tag restriction above only a `v*` tag's release run can publish. Once a release has published this way, consider enabling the option that rejects token-based publishes. No registry token is stored in the repo.
5. **The version in `Cargo.toml` must match the tag** (`0.1.0` ↔ `v0.1.0`), and `Cargo.lock` must be committed in sync. `prepare` and `push` keep these aligned.
6. **Tools:** `gh` (authenticated), `dist` at the version pinned in `dist-workspace.toml`, `jq` (reads the `dist plan` manifest; preinstalled on macOS 15 and later), `curl`, and `taskset` for `bench` (Linux; elsewhere the benches run unpinned).

### Notes

- **Custom publish job permissions.** `dist` grants a custom publish job `id-token` + `packages: write` by default, and a called workflow cannot receive more than its caller passes. `github-custom-job-permissions` in `dist-workspace.toml` replaces that with `contents: read` (the checkout) + `id-token: write` (the OIDC exchange). Changing either side means running `dist generate`.
- `aarch64-unknown-linux-gnu` is cross-compiled by `dist` (musl-cross container). The Linux targets build on `ubuntu-22.04`, which sets the glibc floor; the `-musl` target has none.
- **Dependabot must not touch `release.yml`**, or `dist plan` fails on the drift (PR #18). `dependabot.yml` has no file-level exclusion, so the three actions that file uses are ignored wholesale; `release.yml` gets them only by re-running `dist init`. **`actions/checkout` is also used by `ci.yml`, `audit.yml` and `publish-crates.yml`, so it must be bumped there by hand.**
- **The MSRV pin is not an action version.** `ci.yml` uses `dtolnay/rust-toolchain@stable` with `toolchain: "1.90"` as an input, because Dependabot rewrites a version in the ref (`@1.90` → `@1.100`), silently turning the MSRV job into a latest-stable check. Don't put a Rust version back in the ref.
