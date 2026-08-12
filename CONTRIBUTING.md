# Contributing to edamame

Thanks for your interest. edamame is a small, opinionated project maintained by one person — the notes below are mostly about saving you wasted effort.

## Before you write code

**For anything non-trivial, open an issue first.** edamame deliberately keeps a narrow scope (a Markdown editor, not a general one), so a feature can be well-built and still not be a fit.

Small fixes, such as an obvious bug, a typo, a wrong default, or a doc correction need no preamble. Just send them.

## Getting set up

```bash
git clone https://github.com/mijowi/edamame
cd edamame
cargo run -- tests/fixtures/sample.md
```

edamame's MSRV is **Rust 1.90**, declared as `rust-version` in `Cargo.toml` and pinned by the `msrv` CI job. Note that the floor comes from a transitive dependency (`ratatui-image → icy_sixel → quantette`), not from edamame's own code — so please don't raise it casually to use a new language feature. If a dependency bump raises it, that's a decision to make explicitly, and the README and CI job need updating with it.

`tests/fixtures/sample.md` and `sample_diagrams.md` exercise most of the rendering surface and are the quickest way to see a change in action.

For the best experience while developing, use a terminal with 24-bit color, an image protocol and the kitty keyboard protocol — kitty, Ghostty, WezTerm or foot. Some features are invisible without them.

## Before you open a PR

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

CI runs these plus a docs build and a dependency audit. Two notes on running them locally:

- **Tests use `--no-default-features` in CI.** That drops the `arboard` dependency so copy/paste resolves against the in-process kill-ring, which is what makes the suite deterministic. Prefer the same flag locally. Unit tests are insulated either way by `edit_ops::OS_CLIPBOARD` (false under `cfg(test)`), but integration tests in `tests/` link the library compiled *without* `cfg(test)`, so a plain `cargo test` runs the live clipboard paths: `Paste` reads whatever is on your clipboard, `Copy` overwrites it, and OSC 52 escapes land in the test output. A new integration test touching copy or paste should assert against `kill_ring`, never the OS clipboard.
- **Don't run `cargo test --all-targets`.** It pulls in the Criterion benchmarks and runs them, turning a 30-second suite into several minutes.

If you changed a snapshot's output, review it with `cargo insta review` and commit the updated `.snap` alongside the code.

## Architecture

[`AGENTS.md`](AGENTS.md) is the real guide: the module layout, the layering rules, and — most valuably — a long list of decisions that are easy to break if you don't know they exist. If you're touching the renderer, the hybrid raw-reveal, tables, or the input layer, read the relevant section first. It will save you rediscovering a constraint the hard way.

Design rationale lives in [`docs/dev/`](docs/dev/).

## Code style

Idiomatic modern Rust; `AGENTS.md` has the specifics (import grouping, naming, the module facade pattern, the two-tier error strategy). The short version: match the surrounding code.

## Documentation is part of the change

If you change user-visible behavior, update the affected page in `docs/`.

These pages are written **from the code**, not from `AGENTS.md` — `AGENTS.md` records intent, which drifts from the shipped surface faster than the code does. Verify each claim against the implementation.

Some docs are pinned by tests, so you may see a failure telling you a doc needs updating:

- `keymap::tests::default_bindings_are_pinned_for_the_docs` — the default   keybinding table. Accepting the new snapshot is your reminder to update `docs/keybindings.md` and `config/keybindings.toml`.
- `theme::tests::builtin_themes_are_pinned_for_the_docs` — the built-in theme list in `docs/themes.md`.
- `readers::tests::shipped_reference_config_loads_without_warnings` and `…_keybindings_are_all_uncommentable` — the two reference files in `config/` are copied into every new user's config directory, so a wrong key or an unparseable example chord reaches users directly.

## Security-sensitive changes

edamame opens untrusted documents. If you're touching image or SVG decoding, remote fetches, Mermaid rendering, link opening, HTML export, or subprocess spawning, read [`docs/dev/security-invariants.md`](docs/dev/security-invariants.md) first — those paths have hardening that is easy to remove by accident.

Please report vulnerabilities privately rather than in a PR or issue; see [`SECURITY.md`](SECURITY.md).

## Commits and PRs

Keep commits focused, and write messages that explain *why*. Existing commits use Conventional Commits: `type(scope): summary` (`fix(vim): …`, `feat(diff): …`, `docs: …`) — following it is appreciated but not enforced.

By contributing, you agree your work is licensed under [Apache-2.0](LICENSE).
