# Publish checklist

- [ ] Make a nice README with screenshots and videos — README written; **screenshots screencast still needed**, marked with `TODO(screenshot)` / `TODO(screencast comments in the file
    1. The hero shot. A rendered document showing off range: an H1, a drawn table, a task list with some items checked, inline code, maybe an image. Crucially, put the cursor on a heading so that one line shows its raw ## Heading source while everything around it stays styled. If a reader only looks at one picture, this has to land the concept.
    2. A short screencast (asciinema or GIF, ~15s), which is where the reveal actually becomes obvious — motion sells it in a way a still can't. Arrow down through a few lines and let each one flip to raw and back, then Tab through table cells, then Ctrl-P and run a command.
    3. Diff review, mid-review with one hunk accepted (green), one rejected, one focused. This is a strong differentiator and completely invisible from a feature list.
    4. A theme strip — the same short document in three or four themes side by side. Cheap to produce, and disproportionately effective at making a project look finished.
- [ ] Add a documentation page for the terminal upgrade notice and a link to the page from within edamame — the prose now exists in `docs/getting-started.md` ("The terminal capabilities notice") and `docs/keybindings.md` ("Terminal compatibility"); still needs a link from inside the capabilities modal once the docs have a public URL
- [ ] Release/tag v0.1.0
- [ ] Compile binaries and add to GitHub Releases. Targets: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu (build on the oldest Ubuntu runner you can, for glibc compat), x86_64-unknown-linux-musl, aarch64-unknown-linux-gnu
- [ ] Set GitHub repo to public

After flipping repo to public:
- [ ] Turn on private vulnerability reporting *(Settings → Security)* — **required**: `SECURITY.md`, `docs/security.md` and the README all link to it, and those links are dead until it is enabled.
- [ ] Turn on secret scanning + push protection.
- [ ] Add social preview image — the hero screenshot, once it exists.
- [ ] Publish to crates.io
- [ ] **Create the Homebrew tap.** The README documents `brew install mijowi/tap/edamame`; that tap does not exist yet.

- [x] Add user-facing documentation — `docs/{getting-started,editing,keybindings,configuration,themes}.md`, plus corrections to `vim-mode.md` / `security.md`
- [x] Add a license - Apache 2.0 (`LICENSE`, verbatim text + appendix)
- [x] Update cargo.toml with license and other fields needed for crates.io publish — `cargo publish --dry-run` is clean (291 files, 1.2 MiB compressed)
- [x] Set up CI — `.github/workflows/ci.yml`: fmt, clippy (both feature configurations), test on ubuntu + macos, a non-blocking windows job, a docs build with `RUSTDOCFLAGS=-D warnings`, and `cargo audit`. All commands verified locally before commit.
- [x] Issue templates — `.github/ISSUE_TEMPLATE/{bug_report,feature_request,config}.yml`. The bug form requires terminal emulator + version, multiplexer, `$TERM`/`$COLORTERM`/`$TERM_PROGRAM`, and points at the capability summary and the clean-config check.
- [x] **Move everything in `issues.md` to GitHub issues**, then delete the file. Suggested labels: `bug`, `enhancement`, `docs`, `terminal-compat`, `good first issue`, `help wanted` — and delete GitHub's unused defaults (`duplicate`, `invalid`, `wontfix`). `terminal-compat` earns its place because those issues triage differently and often close as "your terminal, not the app".
- [x] Prune docs, especially plans — `docs/` is now user-facing only, `docs/dev/` holds design specs + plans and is excluded from the published crate
- [x] Diff edit mode removed. `Action::DiffEnterEdit` / `DiffExitEdit`, the `i` / `Enter` bindings, and the "coming soon" flash are all gone; `i` and `Enter` now fall through to the global keymap. Pinned by `diff_keys::tests::edit_sub_mode_keys_are_unbound`. The `src/diff/` design notes still describe a future Edit sub-mode — that groundwork is untouched, only the user-visible dead end was removed.
- [x] Turn on Dependabot alerts + security updates, and the dependency graph (Dependabot needs it).

---

## Releases via `dist` (formerly cargo-dist)

`dist` generates its own release workflow, and that workflow is coupled to the exact `dist` version that wrote it. **Do not hand-write or hand-edit `.github/workflows/release.yml`** — regenerate it instead. That is why this repo does not ship one yet: it has to come out of the tool.

Create the tap repo first (empty is fine), because the Homebrew installer writes its formula there:

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

- `aarch64-unknown-linux-gnu` needs cross-compilation; `dist` handles this, but confirm it in `dist plan` before relying on it.
- For glibc compatibility, check which Ubuntu image the generated workflow uses — older is better for the `-gnu` target. The `-musl` target sidesteps the question entirely.
- `dist init` must be re-run after upgrading `dist`, to regenerate the workflow.
- The tap must exist and the workflow needs a token with permission to push to it (`dist` prompts about this).