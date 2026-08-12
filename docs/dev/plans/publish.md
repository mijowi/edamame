# Publish checklist

- [x] Add a license - Apache 2.0 (`LICENSE`, verbatim text + appendix)
- [x] Update cargo.toml with license and other fields needed for crates.io publish — `cargo publish --dry-run` is clean (291 files, 1.2 MiB compressed)
- [x] Set up CI — `.github/workflows/ci.yml`: fmt, clippy (both feature
      configurations), test on ubuntu + macos, a non-blocking windows job, a
      docs build with `RUSTDOCFLAGS=-D warnings`, and `cargo audit`.
      All commands verified locally before commit.
- [x] Issue templates — `.github/ISSUE_TEMPLATE/{bug_report,feature_request,config}.yml`.
      The bug form requires terminal emulator + version, multiplexer,
      `$TERM`/`$COLORTERM`/`$TERM_PROGRAM`, and points at the capability
      summary and the clean-config check.
- [ ] **Move everything in `issues.md` to GitHub issues**, then delete the
      file. Suggested labels: `bug`, `enhancement`, `docs`, `terminal-compat`,
      `good first issue`, `help wanted` — and delete GitHub's unused defaults
      (`duplicate`, `invalid`, `wontfix`). `terminal-compat` earns its place
      because those issues triage differently and often close as "your
      terminal, not the app".
- [x] Prune docs, especially plans — `docs/` is now user-facing only, `docs/dev/` holds design specs + plans and is excluded from the published crate
- [ ] Tag v0.1.0
- [x] Add user-facing documentation — `docs/{getting-started,editing,keybindings,configuration,themes}.md`, plus corrections to `vim-mode.md` / `security.md`
- [ ] Make a nice README with screenshots and videos — README written; **screenshots screencast still needed**, marked with `TODO(screenshot)` / `TODO(screencast comments in the file
    1. The hero shot. A rendered document showing off range: an H1, a drawn table, a task list with some items checked, inline code, maybe an image. Crucially, put the cursor on a heading so that one line shows its raw ## Heading source while everything around it stays styled. If a reader only looks at one picture, this has to land the concept.
    2. A short screencast (asciinema or GIF, ~15s), which is where the reveal actually becomes obvious — motion sells it in a way a still can't. Arrow down through a few lines and let each one flip to raw and back, then Tab through table cells, then Ctrl-P and run a command.
    3. Diff review, mid-review with one hunk accepted (green), one rejected, one focused. This is a strong differentiator and completely invisible from a feature list.
    4. A theme strip — the same short document in three or four themes side by side. Cheap to produce, and disproportionately effective at making a project look finished.
- [ ] Add a documentation page for the terminal upgrade notice and a link to the page from within edamame — the prose now exists in `docs/getting-started.md` ("The terminal capabilities notice") and `docs/keybindings.md` ("Terminal compatibility"); still needs a link from inside the capabilities modal once the docs have a public URL
- [ ] Compile binaries

## Found during the docs pass — decide before publishing

- [x] **MSRV declared: 1.90.** `rust-version = "1.90"` in Cargo.toml, stated
      in the README, and pinned by the `msrv` CI job
      (`cargo check --locked --all-targets --all-features` on 1.90).

      Worth recording *why*, because it is not our code: edamame itself still
      compiles on 1.88. The floor comes from a transitive dependency —
      `ratatui-image → icy_sixel → quantette@0.5.1`, which declares
      `rust-version = 1.90`. Cargo enforces that gate for every consumer, so
      1.90 is the honest number even though `--ignore-rust-version` builds
      fine on 1.89.

      To lower it, pin an older `quantette` / `icy_sixel` / `ratatui-image`
      rather than touching our source. Probably not worth it: 1.90 is
      recent-ish but edamame ships primarily as a binary, and the people most
      likely to be on an old toolchain will install a release binary or a
      Homebrew bottle instead of building.
- [ ] **Create the Homebrew tap.** The README documents `brew install mijowi/tap/edamame`; that tap does not exist yet.
- [ ] `Action::Open` (`Ctrl-O`) is still a stub. Its default binding and palette     entry were **removed** so users don't hit a dead key — restore both when
      real file-opening lands. Pinned by `keymap::tests::open_stays_unbound_while_it_is_a_stub`.
- [ ] `[[export.custom]]` is parsed and `spawn_custom_export` exists, but nothing    ever calls it. The examples were **removed from the shipped config.toml** so   users can't configure a no-op. Wire it up or drop the code.
- [x] Diff edit mode removed. `Action::DiffEnterEdit` / `DiffExitEdit`, the `i` / `Enter` bindings, and the "coming soon" flash are all gone; `i` and `Enter` now fall through to the global keymap. Pinned by `diff_keys::tests::edit_sub_mode_keys_are_unbound`. The `src/diff/` design notes still describe a future Edit sub-mode — that groundwork is untouched, only the user-visible dead end was removed.
- [ ] Compile binaries and add to GitHub Releases. Targets: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu (build on the oldest Ubuntu runner you can, for glibc compat), x86_64-unknown-linux-musl, aarch64-unknown-linux-gnu
- [ ] Set GitHub repo to public
- [ ] Publish to crates.io
- [ ] Make a homebrew tap

Suggested order: README + LICENSE + Cargo metadata → CI green → repo public → tag v0.1.0 → release workflow produces binaries → cargo publish.

---

## Releases via `dist` (formerly cargo-dist)

`dist` generates its own release workflow, and that workflow is coupled to
the exact `dist` version that wrote it. **Do not hand-write or hand-edit
`.github/workflows/release.yml`** — regenerate it instead. That is why this
repo does not ship one yet: it has to come out of the tool.

Create the tap repo first (empty is fine), because the Homebrew installer
writes its formula there:

    github.com/mijowi/homebrew-tap

Then:

```bash
cargo install cargo-dist          # installs the `dist` binary
dist init                         # interactive; writes [workspace.metadata.dist]
                                  # into Cargo.toml AND .github/workflows/release.yml
```

Answers to give at the prompts:

| Prompt | Answer |
|---|---|
| CI backend | GitHub |
| Installers | `shell`, `homebrew` (add `powershell` only once Windows is supported) |
| Homebrew tap | `mijowi/homebrew-tap` |
| Targets | `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu` |

Then commit both generated files, and release with:

```bash
dist plan                         # dry run: shows the artifact matrix
git tag v0.1.0 && git push --tags # the workflow builds and publishes
```

Notes:

- `aarch64-unknown-linux-gnu` needs cross-compilation; `dist` handles this,
  but confirm it in `dist plan` before relying on it.
- For glibc compatibility, check which Ubuntu image the generated workflow
  uses — older is better for the `-gnu` target. The `-musl` target sidesteps
  the question entirely.
- `dist init` must be re-run after upgrading `dist`, to regenerate the
  workflow.
- The tap must exist and the workflow needs a token with permission to push
  to it (`dist` prompts about this).

## GitHub repo settings — web UI only

These cannot be set from the repo, so they need a pass in Settings:

**Turn on**
- Private vulnerability reporting *(Settings → Security)* — **required**:
  `SECURITY.md`, `docs/security.md` and the README all link to it, and those
  links are dead until it is enabled.
- Dependabot alerts + security updates, and the dependency graph
  (Dependabot needs it).
- Secret scanning + push protection.

**Turn off**
- **Wiki.** Docs are in-repo, versioned with the code, reviewed in PRs and
  shipped in the crate tarball. A wiki is none of those and would recreate
  exactly the doc-drift problem this repo just finished fixing.
- **Projects.** Milestones (`v0.1.0`, `v0.2.0`) are enough for one
  maintainer and ~10 issues. Revisit when there are contributors or parallel
  workstreams.

**Skip**
- Code scanning / CodeQL — no meaningful Rust support. `cargo audit` in CI is
  the equivalent.
- Branch protection — requiring PR review of yourself is pure friction while
  solo. Add "require status checks to pass" the day someone else's PR lands.

**Also worth two minutes**
- About + topics: `markdown`, `tui`, `terminal`, `editor`, `rust`, `ratatui`.
  This is most of the organic discoverability on GitHub.
- Social preview image — the hero screenshot, once it exists.
- Merge settings: squash-only, auto-delete head branches.

Nothing to configure for **Insights** — it is always-on reporting. Post-launch,
Traffic and Clones are the useful pages for seeing whether the README works.
