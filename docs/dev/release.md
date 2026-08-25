# Releasing

## Releases via `dist` (formerly cargo-dist)

 `dist-workspace.toml` holds the config (dist 0.32.0, GitHub CI, `shell` + `homebrew` installers, the five targets, tap `mijowi/homebrew-tap`) and `.github/workflows/release.yml` is the generated workflow. `dist generate --check` passes and `dist plan` produces the expected matrix.

`dist` owns that workflow and it is coupled to the exact `dist` version that wrote it. **Never hand-edit `.github/workflows/release.yml`** — run `dist init` / `dist generate` and commit the result. `dist plan` fails in CI when the file has drifted, which is what it is for.

## Releasing

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
# cargo test --no-fail-fast     # if nextest is not installed
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

That single tag push drives two independent workflows:

- **`release.yml`** (cargo-dist) — builds every target, creates the GitHub Release with the archives + checksums + `edamame-installer.sh`, and pushes the Homebrew formula to the tap.
- **`publish-crate.yml`** — publishes the crate to crates.io, behind a manual approval gate.

`cargo publish` reads the version from `Cargo.toml`, not from the tag, so the manifest must be bumped to match the tag before pushing. A tag whose version is already on crates.io makes the publish job fail (harmlessly — nothing is uploaded twice). 

8. Verify: `gh release view v$VERSION`, `brew upgrade edamame`, `cargo info edamame`, and the docs.rs build.
