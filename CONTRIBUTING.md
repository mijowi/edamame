# Contributing to edamame

Thanks for your interest! Please read below for how to contribute to edamame.

## Before you write code

**For anything non-trivial, open an issue first.** edamame's focus is on Markdown editing, so a feature can be well-built and still not be a fit. Small fixes, such as an obvious bug or a typo, can just be a PR. If you have a question, [Discussions](https://github.com/mijowi/edamame/discussions) is the place to ask.

## AI Policy

**Code:** AI-written code is welcome if the quality is high, repo conventions and architecture are followed, and a human has reviewed it before the PR is opened. AI can write a lot of code quickly, but PRs are ideally smaller and well-scoped instead of large and unwieldy.

**Communication:** Issues, PRs, comments, and discussions must be written, or at least thoroughly edited, by a human. Fully AI-written text is not acceptable. I read every issue and comment myself, and AI writing tends to be verbose, dense, and hard to read. Issues and comments that appear fully AI-written may be closed without reply.

## Getting set up

```bash
git clone https://github.com/mijowi/edamame
cd edamame
git switch develop
cargo run -- tests/fixtures/general.md
```

**Base every PR on `develop`.** `main` only moves at releases.

edamame's MSRV is **Rust 1.90**, declared as `rust-version` in `Cargo.toml` and pinned by the `msrv` CI job. Note that the floor comes from a transitive dependency (`ratatui-image → icy_sixel → quantette`), not from edamame's own code, so please don't raise it casually to use a new language feature. If a dependency bump raises it, that's a decision to make explicitly, and the README and CI job need updating with it.

The files in `tests/fixtures` exercise most of the rendering surface and are the quickest way to see a change in action.

For the best experience while developing, use a terminal with 24-bit color, an image protocol and the kitty keyboard protocol (see [Terminal compatibility](docs/terminal-compatibility.md#tested-terminals)).

Keep commits focused, and write messages that explain *why*. Please use Conventional Commits: `type(scope): summary` (`fix(vim): …`, `feat(diff): …`, `docs: …`).

## Before you open a PR

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test --no-fail-fast
```

CI runs these plus a docs build. A RustSec dependency audit runs weekly and on any change to `Cargo.lock`, `Cargo.toml` or `.cargo/audit.toml`, so a PR that bumps a dependency will pick up an extra check. Two notes on running the commands above locally:

**Don't run `cargo test --all-targets`.** It pulls in the Criterion benchmarks and runs them, turning a 30-second suite into several minutes.

If you changed a snapshot's output, review it with `cargo insta review` and commit the updated `.snap` alongside the code.

## Architecture

[`AGENTS.md`](AGENTS.md) is a guide: the module layout, the layering rules, and a long list of decisions that are easy to break if you don't know they exist. If you're touching the renderer, the hybrid raw-reveal, tables, or the input layer, read the relevant section first. It will save you rediscovering a constraint the hard way.

Architecture details and design rationale live in [`docs/dev/`](docs/dev/).

## Code style

Idiomatic modern Rust; `AGENTS.md` has the specifics (import grouping, naming, the module facade pattern, the two-tier error strategy). The short version: match the surrounding code.

## Documentation is part of the change

If you change user-visible behavior, update the affected page in `docs/`.

These pages are written **from the code**, not from `AGENTS.md` — `AGENTS.md` records intent, which drifts from the shipped surface faster than the code does. Verify each claim against the implementation.

Some docs are pinned by tests, so you may see a failure telling you a doc needs updating.

## Security-sensitive changes

edamame opens untrusted documents. If you're touching image or SVG decoding, remote fetches, Mermaid rendering, link opening, HTML export, or subprocess spawning, read [`docs/dev/security-invariants.md`](docs/dev/security-invariants.md) first. Those paths have hardening that is easy to remove by accident.

Please report vulnerabilities privately rather than in a PR or issue; see [`SECURITY.md`](SECURITY.md).

## Performance

If your PR touches parsing, rendering, editing or layout (`src/markdown`, `src/document`, `src/editor`, `src/ui`), or may significantly impact performance another way, bench it against `develop` and paste the results into the PR. Rebase onto current `develop` first, then run the script. It compares against your local `develop`; in a fork, pass the upstream branch instead (`scripts/bench-compare.sh upstream/develop`).

```bash
scripts/bench-compare.sh                        # benches develop, then your tree, and prints each case's change
EDAMAME_BENCH_CORE=N scripts/bench-compare.sh   # Linux, hybrid CPU: pin both runs to performance core N
```

Both builds run back to back in one session, so they share the machine's state; keep it idle while they run. A full run takes several minutes.

Analyze any significant regression, then fix it or justify the cost in the PR. A single "regressed" line is often noise, so re-run that case to confirm it first. [`docs/dev/performance.md`](docs/dev/performance.md#checking-for-regressions) has more on reading the results.

## Licensing

By contributing, you agree your work is licensed under [Apache-2.0](LICENSE).
