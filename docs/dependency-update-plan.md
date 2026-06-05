# Dependency update plan

An audit of every `edamame` dependency against the latest stable release,
with the compatibility impact of each upgrade and a recommended order of
work. Captured 2026-06-01 against the toolchain `rustc 1.94.1`.

## Context

- The local toolchain is `rustc 1.94.1`, comfortably above every
  candidate crate's MSRV (the most demanding, `ratatui-image 11`, needs
  only 1.86). **MSRV is not a blocker for any upgrade in this plan.**
- "Locked" below is the resolved version in `Cargo.lock`; "Req" is the
  declaration in `Cargo.toml`; "Latest" is the newest version on
  crates.io at audit time.
- Network-restricted audits used the docs.rs source view for manifests
  and the GitHub API for changelogs.

## Summary

| Crate | Locked | Req | Latest | Action |
|---|---|---|---|---|
| ratatui | 0.30.0 | 0.30 | 0.30.0 | none — current |
| serde | 1.0.228 | 1 | 1.0.228 | none — current |
| ropey | 1.6.1 | 1.6 | 1.6.1 | none — current |
| thiserror | 2.0.18 | 2 | 2.0.18 | none — current |
| anyhow | 1.0.102 | 1 | 1.0.102 | none — current |
| tracing | 0.1.44 | 0.1 | 0.1.44 | none — current |
| tracing-subscriber | 0.3.23 | 0.3 | 0.3.23 | none — current |
| arboard | 3.6.1 | 3 | 3.6.1 | none — current |
| image | 0.25.10 | 0.25 | 0.25.10 | none — current |
| nucleo-matcher | 0.3.1 | 0.3 | 0.3.1 | none — current |
| base64 | 0.22.1 | 0.22 | 0.22.1 | none — current |
| tempfile | 3.27.0 | 3 | 3.27.0 | none — current |
| tui-big-text | 0.8.4 | 0.8 | 0.8.4 | none — current |
| serde_ignored | 0.1.14 | 0.1 | 0.1.14 | none — current |
| insta (dev) | 1.47.2 | 1 | 1.47.2 | none — current |
| proptest (dev) | 1.11.0 | 1 | 1.11.0 | none — current |
| pulldown-cmark | 0.13.3 | 0.13 | 0.13.4 | `cargo update` |
| tracing-appender | 0.2.4 | 0.2 | 0.2.5 | `cargo update` |
| unicode-width | 0.2.0 | 0.2 | 0.2.2 | `cargo update` |
| unicode-segmentation | 1.13.2 | 1 | 1.13.3 | `cargo update` |
| open | 5.3.4 | 5 | 5.3.5 | `cargo update` |
| crossterm | 0.28.1 | 0.28 | 0.29.0 | manifest bump — Phase 2 |
| dirs | 5.0.1 | 5 | 6.0.0 | manifest bump — Phase 2 |
| toml | 0.8.23 | 0.8 | 1.1.2 | manifest bump (with toml_edit) — Phase 3 |
| toml_edit | 0.22.27 | 0.22 | 0.25.12 | manifest bump (with toml) — Phase 3 |
| ratatui-image | 10.0.8 | 10 | 11.0.2 | code work — Phase 4, optional |
| ureq | 2.12.1 | 2 | 3.3.0 | code work — Phase 4, optional |
| mermaid-rs-renderer | 0.2.1 | =0.2.1 | 0.2.2 | optional patch bump |
| sha2 | 0.10.9 | 0.10 | 0.11.0 | leave — no benefit |
| resvg | 0.46.0 | 0.46 | 0.47.0 | **do not bump** — pinned to mermaid |
| usvg | 0.46.0 | 0.46 | 0.47.0 | **do not bump** — pinned to mermaid |

> **Status (implemented 2026-06-05):** Phases 1–3 done and verified
> (`cargo build` + `cargo clippy --all-targets -- -D warnings` +
> `cargo test`, all green). Phase 4 and the optional patches were left
> for a future session per scope decision. See per-phase ✅ notes below.

## Phase 1 — lockfile drift (zero risk) ✅ done

Five patch releases already satisfy the existing version requirements;
only `Cargo.lock` is stale. No `Cargo.toml` edit, no API change.

```bash
cargo update -p pulldown-cmark -p tracing-appender \
  -p unicode-width -p unicode-segmentation -p open
```

(Equivalently a plain `cargo update`, which also refreshes transitive
crates.) Verify with `cargo build && cargo test`.

> ✅ Applied. Bumped open 5.3.4→5.3.5, pulldown-cmark 0.13.3→0.13.4,
> tracing-appender 0.2.4→0.2.5, unicode-segmentation 1.13.2→1.13.3,
> unicode-width 0.2.0→0.2.2. Also pulled in a new transitive `symlink
> 0.1.0` (dependency of `open`). Build + full test suite green.

## Phase 2 — cheap manifest bumps (low risk) ✅ done

### crossterm 0.28 → 0.29 (recommended)

`ratatui = { version = "0.30", features = [..., "crossterm_0_28"] }`
plus a direct `crossterm = "0.28"`. ratatui 0.30 also ships a
`crossterm_0_29` feature, and `ratatui-crossterm` already compiles
**both** crossterm 0.28 and 0.29 into the build today (a duplicate copy).

- Change the ratatui feature `crossterm_0_28` → `crossterm_0_29`.
- Change the direct dep `crossterm = "0.28"` → `"0.29"`.
- 0.29 is a small release; our usage is plain event/key handling and
  should compile unchanged.
- **Payoff:** removes the duplicate crossterm 0.28 from the build.

### dirs 5 → 6 (recommended)

Only `dirs::config_dir()` and `dirs::data_dir()` are used
(`src/config/config.rs:170`, `:236`), both unchanged in 6.0. The bump is
internal (`dirs-sys` 0.4 → 0.5). Change `dirs = "5"` → `"6"` and build.

Verify the whole phase with `cargo build && cargo clippy --all-targets
-- -D warnings && cargo test`.

> ✅ Applied. ratatui feature `crossterm_0_28`→`crossterm_0_29`,
> `crossterm "0.28"`→`"0.29"`, `dirs "5"`→`"6"`. Build + clippy + full
> tests green; no source edits needed.
>
> **Note on the payoff:** the *build* now compiles only crossterm 0.29
> (confirmed via `cargo tree`), so the duplicate-copy waste is gone as
> intended. However, an inactive `crossterm 0.28.1` entry *remains in
> `Cargo.lock`*: `ratatui-crossterm` declares both crossterm 0.28 and
> 0.29 as optional deps, and cargo records optional deps in the lock
> regardless of feature activation. This entry is not compiled and
> cannot be pruned by `cargo update`. (A stray `windows-sys 0.59.0`
> orphan *was* pruned as a side effect.)

## Phase 3 — toml / toml_edit (low–moderate risk, move together) ✅ done

`toml` 1.x requires `toml_edit` ≥ 0.23, so both bump in one step:

- `toml = "0.8"` → `"1"`
- `toml_edit = "0.22"` → `"0.25"`

Breaking changes across 0.23 → 0.25 (toml_edit): removed deprecated
APIs; `InternalString` → `String`; `Table::position` `usize` → `isize`;
`InlineTable::preamble`/`set_preamble` → `trailing`/`set_trailing`;
`ArrayOfTables::remove` now returns the `Table`; datetime `Time` fields
became `Option`.

**Exposure is shallow.** Our `toml_edit` surface is only
`toml_edit::{Value, Table, DocumentMut}` and a match on
`Item::ArrayOfTables` (`src/config/config.rs:398`, `src/config/sections.rs`);
none of the changed APIs are called. The `toml` serde surface
(`toml::to_string_pretty`, `toml::from_str`) is unchanged. Expected to
compile with no source edits.

- **Risk:** the surgical comment-preserving edits in
  `config/sections.rs` are the only place behavior could subtly shift.
- **Verify:** `cargo test` over the config/sections round-trip tests in
  `src/config/config.rs` (`#[cfg(test)]`) and any `sections` coverage;
  manually confirm `Config::save` still preserves comments in
  `~/.config/edamame/config.toml`.
- **Payoff:** TOML 1.1 parse support and several parser panic fixes.

> ✅ Applied. `toml "0.8"`→`"1"`, `toml_edit "0.22"`→`"0.25"`.
>
> **Deviation from plan:** the plan predicted "no source edits". One was
> required. In toml 1.x `toml::Deserializer::new` parses eagerly and now
> returns `Result<Self, Error>` (and is itself deprecated in favor of
> `Deserializer::parse`). `src/config/readers.rs:32` constructed it
> directly and fed it to `serde_ignored::deserialize`, which no longer
> type-checks. Fixed by switching to `toml::Deserializer::parse(raw)?`
> — parse errors now surface at construction rather than during the
> serde walk; behavior is otherwise unchanged. This is the only
> `toml`/`toml_edit` call site that broke; the `toml_edit`
> `Value`/`Table`/`DocumentMut`/`ArrayOfTables` surface in
> `config.rs`/`sections.rs` compiled unchanged, as the plan predicted.
>
> Comment-preservation is covered by `config.rs::save_merge_*` tests
> (incl. `save_merge_unchanged_config_preserves_file_verbatim`), all
> green under toml_edit 0.25 — no separate manual check needed.

## Phase 4 — larger, isolated upgrades (do when the changes are wanted)

These are real work, not urgent. Each is self-contained.

### ratatui-image 10 → 11 (moderate code work)

Still targets `ratatui ^0.30` (identical to v10), so **no ratatui
change is needed**. v11.0.0 has breaking API changes that touch our code:

- `FontSize` tuple `(u16, u16)` → struct `{ width: u16, height: u16 }`.
- `Rect` replaced with `Size` for size-without-position across
  `Picker::new_protocol`, `Resize::render_area` → `size_for`,
  `Protocol::area`/`ProtocolTrait::area` → `size`,
  `ResizeEncodeRender::{resize_encode, needs_resize}`.

Files to touch: `src/terminal/capabilities.rs`, `src/ui/image_view.rs`,
`src/image/cache.rs`, `src/image/render.rs`. v11 also adds an opt-in
`sliced`/viewport-scrolling module we don't need. Verify with the
`tests/ui.rs` `TestBackend` image renders and a manual smoke test of
image display under each protocol.

### ureq 2 → 3 (moderate, fully contained)

v3 is an API rewrite. `fetch_remote` (`src/image/loader.rs:188`) uses
`AgentBuilder::new().timeout_connect().timeout_read().build()` and
`response.into_reader()`, all removed in v3 (now `Agent::config_builder()`
with a `Timeouts` config, and `response.into_body().read_to_vec()` /
`.into_reader()`).

- **Feature flag also changed:** `features = ["tls"]` is invalid in v3.
  TLS is now `rustls` (default, aws-lc-rs backend) or `native-tls`. To
  keep the no-system-OpenSSL property, use
  `default-features = false, features = ["rustls"]` (consider
  `rustls` with `ring` provider if aws-lc-rs's build deps are a
  concern).
- Entirely contained in one function plus the `ureq::Error` type in
  `ImageLoadError::Http` (`src/image/loader.rs:54`).
- Verify with the loader's `#[cfg(test)]` tests and a manual remote-image
  fetch.

## Do not touch

- **resvg / usvg 0.46 → 0.47:** `mermaid-rs-renderer 0.2.2` (the latest)
  still pins resvg/usvg **0.46**. Bumping to 0.47 would compile two
  copies and break the in-memory SVG→PNG path — exactly the constraint
  documented in `Cargo.toml` and `CLAUDE.md`. Stay on 0.46.
- **sha2 0.10 → 0.11:** only `Sha256::digest` for the diagram cache key
  (`src/diagram/mermaid.rs:101`); the API is identical across the bump
  and it is a leaf dep. No benefit; skip unless another crate forces
  `digest` 0.11.

## Optional patch

- **mermaid-rs-renderer `=0.2.1` → `=0.2.2`:** 0.2.2 keeps the same
  resvg/usvg 0.46 pin, so it does not disturb the version-lock
  invariant. The `=` pin is intentional (pre-1.0, known panic bugs); a
  manual bump to `=0.2.2` is safe if its fixes are wanted.

## Recommended order

1. **Phase 1** — `cargo update` for patch drift (zero risk).
2. **Phase 2** — crossterm → 0.29 and dirs → 6 (cheap; removes the
   duplicate crossterm copy).
3. **Phase 3** — toml + toml_edit together (cheap given shallow usage;
   gives a clean compile/test signal).
4. **Phase 4** — ratatui-image 11 and ureq 3 only when their changes are
   wanted; both are isolated.
5. Leave resvg / usvg / sha2 / mermaid pins as documented.

After each phase: `cargo build && cargo clippy --all-targets -- -D
warnings && cargo test`.
