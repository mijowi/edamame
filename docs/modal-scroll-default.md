# Make `ModalView`/`ModalState` modals scrollable by default

**Status: planned — implement on its own branch, not on `diff`.**

## Context

Mouse-wheel scrolling of a modal body is currently *opt-in per modal*.
The run loop unconditionally forwards wheel events to the top modal
(`src/app/event_loop.rs:600`):

```rust
top.handle_wheel(modal_wheel_delta(me, wheel_step));
```

`Modal::handle_wheel` has a **no-op default** (`src/app/modal/types.rs:97`):

```rust
fn handle_wheel(&mut self, _delta: i32) {}
```

Every modal that wants its body to scroll must override it. For the 15
modals built on `ModalView` + `ModalState`, that override is *byte-for-byte
identical*:

```rust
fn handle_wheel(&mut self, delta: i32) {
    self.state.scroll_by(delta);
}
```

This is a footgun, not a feature. It already bit us once: the
`DiffIntroModal` refactor deleted its `handle_wheel` override as collateral
and silently lost wheel scrolling — no compile error, no test failure, just
a dead wheel on a body that can overflow a short terminal. A new
`ModalView` modal author who forgets the one-liner reintroduces the same
silent bug.

### Why this is safe to default-on

Verified facts (see "Findings" in the review thread that motivated this):

- **The wheel event is already consumed** by the top modal regardless of
  whether `handle_wheel` does anything — defaulting to "scroll" never steals
  an event some other layer wanted; today it is simply discarded.
- **`ModalState::scroll_by` clamps.** Test `scroll_by_is_a_noop_when_body_fits`
  (`src/ui/modal.rs:491`) proves scrolling a modal whose content fits is a
  true no-op, never a misrender.
- **No `ModalView` modal wants the wheel to do anything other than scroll.**
  All 15 overrides are identical.

So for `ModalView` modals, scroll-on-wheel is *always* correct, and the
current no-op default is pure risk.

### Why it isn't already a trait default

`handle_wheel`'s default lives on the `Modal` **trait**, which is the
polymorphic boundary — and not every `Modal` owns a `ModalState`. The
modals fall into two families by the type of their `state` field:

| Family | `state` type | Wheel behavior today |
|---|---|---|
| `ModalView`-based (15) | `ModalState` | `self.state.scroll_by(delta)` (identical) |
| Bespoke overlays (7) | `PaletteState`, `SettingsState`, `KeybindsState`, `ThemePickerState`, `ExportThemeState`, `SectionPickerState`, `WelcomeState` | custom — scroll a nested list / `scroll_state`, or forward to own state |
| Text-entry (3) | `InsertTableState`, `SaveCopyState` (×2: `save_copy`, `dirty_conflict_save_copy`) | none — bodies fit, scrolling N/A |

A trait default literally cannot reach `self.state` because the trait does
not know that field exists, and the bespoke family stores scroll state under
a different field path (`self.state.scroll_state`). So "scrollable by
default" needs a tiny structural hook, not just a default body.

## The change

Add one accessor to the `Modal` trait and key the default `handle_wheel`
off it. In `src/app/modal/types.rs`:

```rust
/// Modals built on `ModalView` return their `ModalState` here so the
/// shared default behaviors (today: wheel scroll) work without a
/// per-modal override. Modals with bespoke state leave this `None`
/// and handle the wheel themselves.
fn modal_state_mut(&mut self) -> Option<&mut ModalState> {
    None
}

fn handle_wheel(&mut self, delta: i32) {
    if let Some(state) = self.modal_state_mut() {
        state.scroll_by(delta);
    }
}
```

Each of the 15 `ModalState` modals then implements the accessor and
**deletes** its `handle_wheel`:

```rust
fn modal_state_mut(&mut self) -> Option<&mut ModalState> {
    Some(&mut self.state)
}
```

This is still one line per modal, but it is strictly better than the status
quo:

- **The default behavior is now correct.** A new `ModalView` modal scrolls
  out of the box. Forgetting the accessor is the failure mode, and that
  failure is louder than a silent wheel no-op (the same accessor is the
  natural hook for future shared `ModalState` plumbing — see "Optional
  follow-up").
- **It stays overridable.** A future modal that genuinely wants custom wheel
  behavior just implements `handle_wheel` directly; the default is ignored.

### Files to change (primary)

Trait + default: `src/app/modal/types.rs`.

Convert these 15 (`state: ModalState`) — add `modal_state_mut`, delete the
identical `handle_wheel`:

- `config_warning.rs`
- `diagrams_enabled.rs`
- `diff_intro.rs` *(keep its custom `handle_key`/`handle_click`; only swap the wheel override for the accessor — note its comment explaining why the wheel matters here can be dropped or moved to the trait default)*
- `diff_resolve_confirm.rs`
- `dirty_conflict.rs`
- `dirty_conflict_discard_confirm.rs`
- `dirty_guard.rs`
- `export_success.rs`
- `images_enabled.rs`
- `markdown_cheat_sheet.rs`
- `notice.rs`
- `quit_confirm.rs`
- `remote_image.rs`
- `terminal_capabilities.rs` *(keep its custom `handle_click`)*
- `width_injection.rs`

**Leave unchanged** (state is not `ModalState`; they keep their own
`handle_wheel`, which scrolls a nested list / `scroll_state` or forwards to
bespoke state): `command_palette.rs`, `export_theme.rs`, `keybinds.rs`,
`section_picker.rs`, `settings.rs`, `theme_picker.rs`, `welcome.rs`.

**Also unchanged** (text-entry, no scroll today, bodies fit):
`insert_table.rs`, `save_copy.rs`, `dirty_conflict_save_copy.rs`. They may
optionally implement `modal_state_mut` for free scroll-when-overflowing,
but it is not required and not the point of this change.

## Optional follow-up — consolidate the esc-button click default

The 15 modals also hand-roll a near-identical `handle_click`:

```rust
fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
    super::types::close_if_esc_clicked(self.state.esc_button_rect, col, row)
}
```

This is tempting to fold into a default too, but it is **messier than the
wheel case** and should be scoped separately (or deferred):

1. `esc_button_rect` is **broader than `ModalState`** — the bespoke states
   (`PaletteState`, `SettingsState`, `ExportThemeState`, `InsertTableState`,
   `SaveCopyState`, …) carry their own `esc_button_rect` and use the exact
   same `close_if_esc_clicked` call. Keying a default off `modal_state_mut()`
   would *not* cover them, so the boilerplate would only partially collapse.
   A cleaner hook would be a separate `fn esc_button_rect(&self) ->
   Option<Rect> { None }` accessor that any modal can implement, with a
   default `handle_click` calling `close_if_esc_clicked(self.esc_button_rect(),
   …)`.
2. Several `ModalState` modals have a **bespoke `handle_click` that must keep
   its override** and would shadow the default anyway:
   - `diff_intro.rs` — hit-tests footer buttons before the esc hint.
   - `terminal_capabilities.rs` — routes through `record_outcome()`.
   - (bespoke family) `section_picker.rs` — restores live-preview scroll via
     a `CloseAnd` callback; `theme_picker.rs` — pill hit-testing;
     `welcome.rs`, `keybinds.rs` — route through their own state.

Net: the esc-click consolidation saves fewer lines, needs a different
(looser) accessor than the wheel change, and must carefully exclude the
custom-click modals. Recommend landing the wheel change first as a clean,
self-contained win, and treating esc-click as a separate decision.

## Testing

- Add a unit test asserting the **default** path scrolls: construct any one
  of the converted modals (e.g. `NoticeModal`), drive `handle_wheel(+N)`
  through the trait object, and assert `state.scroll_state` advanced — i.e.
  the behavior survives *without* a per-modal override. This is the
  regression guard the `diff_intro` bug lacked.
- The existing `ModalState` scroll/clamp tests in `src/ui/modal.rs`
  (`scroll_by_clamps_at_top`, `…_at_bottom`, `…_noop_when_body_fits`) already
  cover the clamp semantics — no change needed there.
- After the refactor: `cargo build`, `cargo clippy --all-targets -- -D
  warnings`, `cargo test`. Pay attention to the bin target — `app::modal::*`
  unit tests live in the `edamame` **binary** unittests, not `--lib`
  (run `cargo test --bin edamame`).

## Rollout checklist

1. New branch off `main` (not `diff`).
2. Add `modal_state_mut` + default `handle_wheel` to the `Modal` trait
   (`types.rs`). Import/visibility: `ModalState` is already re-exported via
   `crate::ui`; confirm `types.rs` can name it.
3. Convert the 15 `ModalState` modals: add the accessor, delete the
   `handle_wheel` override.
4. Add the default-path regression test.
5. Build / clippy / test (including `--bin edamame`).
6. (Optional, separate commit or deferred) esc-click consolidation per the
   follow-up section.

## Out of scope

- Keyboard scroll routing — already handled per modal via `handle_key` →
  `ModalState::handle_key`; not a no-op-default footgun.
- The bespoke overlays' own scroll behavior.
- Any change to `modal_wheel_delta` or the event-loop dispatch.
