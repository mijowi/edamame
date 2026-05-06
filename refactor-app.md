# `src/app.rs` Refactor Plan

## Table of Contents

1. [Motivation](#motivation)
2. [Goals & Non-Goals](#goals--non-goals)
3. [Target Layout](#target-layout)
4. [Strategy & Sequencing](#strategy--sequencing)
5. [Step 1 — Modal Stack Abstraction](#step-1--modal-stack-abstraction)
6. [Step 2 — Subdomain Extraction](#step-2--subdomain-extraction)
7. [Step 3 — Decompose `run()`](#step-3--decompose-run)
8. [Step 4 — Test Relocation](#step-4--test-relocation)
9. [Risks & Mitigations](#risks--mitigations)
10. [Success Metrics](#success-metrics)

---

## Motivation

`src/app.rs` is currently **4591 lines** and contains:

- One `App` struct with **~40 fields**.
- One `impl App` block with **~50 methods**.
- One `run()` method spanning **~1025 lines** (lines 863–1888).
- **14 modal types** (`ConfigWarningModal`, `StartupNotice`, `ImagesEnabledPrompt`, `RemoteImagePrompt`, `DirtyGuardPrompt`, `QuitConfirm`, `CheatSheetModal`, `WidthInjectionWarning`, plus the 5 `ui` overlay states + `command_palette`), each replicating the same five-part scaffolding (struct, builder fn, `open_X`, `handle_X_key`, render slot, absorb-input arm in `run`).
- A **render-priority cascade** (lines 1010–1154) that nests `is_none()` chains 14 deep — adding a new modal currently means touching every modal's `if … { … } else { None }` slot.
- An **input-absorption ladder** in `run` (lines 1410–1687, ~280 lines) that is 14 near-identical match arms.

The duplication is mechanical, the file violates the project's facade-pattern conventions (no `app/` submodule split), and modifying any single modal's behaviour requires editing the whole cascade. Adding modal #15 will be painful; adding modal #20 will be intolerable.

---

## Goals & Non-Goals

### Goals

- **Reduce file sizes** — every file under `src/app/` should be **< 1000 LOC**, ideally < 500.
- **Eliminate modal duplication** — adding a new modal should require defining one struct + one trait impl, with **zero changes** to a central cascade or absorption ladder.
- **Split `App` into subdomains** — group the ~40 fields into named sub-state structs so smaller methods can take only the borrow they need.
- **Make `run()` legible** — the main loop body should fit on one screen and read as a sequence of named steps, not an inline state machine.
- **Improve test placement** — pure-helper tests stay co-located; App-behaviour tests move to `tests/`.
- **Increase Rust idiomaticity** — replace ad-hoc `Option<X>` field juggling with trait-based polymorphism; replace boolean priority chains with explicit ordering.

### Non-Goals

- **No user-visible behavioural changes.** Modal semantics, key handling, render order, frame timing — all preserved bit-for-bit. This is pure refactor.
- **No new features.** Nothing on the roadmap (Phase 11 file-change detection, Phase 16 HTML export, etc.) lands as part of this work.
- **No dependency changes.** No new crates introduced; no version bumps.
- **No performance regressions.** Hot-path costs (per-frame draw, per-event dispatch) must stay equivalent or improve. The modal trait will use `&mut dyn Modal` rather than enum-dispatch only if profiling shows the trait-object call cost is irrelevant on the modal path (it is — modals aren't on the per-frame hot path).

---

## Target Layout

```
src/
  app.rs                       # facade: re-exports + struct App definition + ::new + ::run
  app/
    event_loop.rs              # run() decomposed: tick_timers, drain_events, dispatch_event
    draw.rs                    # the per-frame draw closure + render priority resolution
    modal/
      mod.rs                   # facade: re-exports
      stack.rs                 # ModalStack: Vec<Box<dyn Modal>>, push/pop, dispatch
      r#trait.rs               # Modal trait (named "trait.rs" via raw ident, or "modal.rs")
      simple.rs                # SimpleModal: shared body+buttons+ModalState wrapper
      config_warning.rs        # build_config_warning_modal + ConfigWarningModal impl
      startup_notice.rs        # build_startup_notice + impl
      images_enabled.rs        # build_images_enabled_prompt + impl
      remote_image.rs          # build_remote_image_prompt + impl
      dirty_guard.rs           # DirtyGuardPrompt + impl
      quit_confirm.rs          # QuitConfirm + impl
      cheat_sheet.rs           # CheatSheetModal + impl
      width_injection.rs       # WidthInjectionWarning + impl
      hint_prompt.rs           # HintPrompt (Phase 11 scaffold)
    nav.rs                     # NavStack, NavEntry, navigate_back/forward/to_file/to_entry
    external_editor.rs         # open_config_in_editor, open_current_file, run_external_editor
    image_dispatch.rs          # dispatch_image_decodes(_for|_visible), infos_in_viewport_window,
                               #   effective_images_enabled, images_layout_enabled
    flash.rs                   # MessageKind, TransientMessage, flash, expire_transient_if_due,
                               #   transient_deadline, hint_content, dismiss_sticky_transient
    frame_timer.rs             # FrameTimer: needs_draw, last_draw_at, resize_quiesce_at,
                               #   last_scroll_at, mark_scrolling, is_scrolling, next_deadline
    pointer.rs                 # update_pointer_shape (+ last_pointer_shape state)
    actions.rs                 # handle_app_action + flash_for_action +
                               #   dispatch_palette_action (the action-router layer)
```

Rough projected sizes (every file < 1000 LOC, most < 400):

| File | Approx LOC |
|---|---|
| `app.rs` (facade + `App` struct + `new` + `run`) | 250–350 |
| `app/event_loop.rs` | 250–400 |
| `app/draw.rs` | 100–200 |
| `app/modal/stack.rs` + `trait.rs` + `simple.rs` | 150–250 |
| Each `app/modal/<modal>.rs` | 30–120 |
| `app/nav.rs` | 200–300 |
| `app/external_editor.rs` | 250–350 |
| `app/image_dispatch.rs` | 250–350 |
| `app/flash.rs` | 100–200 |
| `app/frame_timer.rs` | 100–200 |
| `app/actions.rs` | 200–350 |

---

## Strategy & Sequencing

The four steps are ordered so each one is independently shippable, reviewable, and testable. **Each step ends with a clean `cargo test` and `cargo clippy -- -D warnings` pass** before the next begins.

1. **Modal stack abstraction** — ✅ **COMPLETE** (2026-05-06). Biggest leverage; deletes the most code; unblocks step 2 and step 3.
2. **Subdomain extraction** — ✅ **COMPLETE** (2026-05-06). Mostly mechanical relocation once step 1 had shrunk the file.
3. **Decompose `run()`** — natural after step 2, since `run` will now mostly call into the new subdomain modules.
4. **Test relocation** — purely organisational; can land in the same PR as step 3 or separately.

Each step is a single PR. Total estimated LOC delta: **~–1500 net** from `src/app.rs` (currently 4591 → ~300 facade), redistributed across `src/app/`.

### Step 1 actuals

- `src/app.rs`: 4591 → 3195 LOC (**−1396**).
- 14 modal types extracted to per-file modules in `src/app/modal/`:
  - `cheat_sheet.rs` (64), `command_palette.rs` (65), `config_warning.rs` (205),
    `dirty_guard.rs` (103), `images_enabled.rs` (121), `insert_table.rs` (87),
    `keybinds.rs` (105), `quit_confirm.rs` (88), `remote_image.rs` (124),
    `save_copy.rs` (83), `settings.rs` (96), `startup_notice.rs` (91),
    `width_injection.rs` (102).
- Modal infrastructure: `types.rs` (76, `Modal` trait + `ModalRenderCtx` +
  `ModalOutcome`), `stack.rs` (188 incl. tests, `ModalStack`), `modal.rs`
  (41, facade).
- All 736 binary unit tests + integration tests pass.
- `cargo clippy --bins --tests` produces no new warnings.
- Migration done bottom-up (lowest legacy priority first) so no migrated
  modal ever lost render priority to a still-legacy modal.
- Render cascade (~145 LOC of nested `is_none()` chains) collapsed to a
  single `top.render(...)` call.
- Absorb-input ladder (~280 LOC across 13 arms) collapsed to a single
  `dispatch_modal_key` call.
- 11 `handle_X_key` methods on `App` deleted (replaced by `Modal::handle_key`
  trait dispatch via the pop-and-replace pattern in `App::dispatch_modal_key`).

### Step 2 actuals

- `src/app.rs`: 3195 → 1624 LOC (**−1571**); non-test code shrank to
  ~1044 LOC (~530 of which is the `run()` body that step 3 will tackle).
- Seven new submodules under `src/app/`:
  - `flash.rs` (178) — `MessageKind`, `TransientMessage`, `flash`,
    `flash_for_action`, `expire_transient_if_due`, `transient_deadline`,
    `dismiss_sticky_transient`, `hint_content`, `save_config_with_flash`.
  - `frame_timer.rs` (134) — `SCROLL_QUIESCE` / `MIN_FRAME_INTERVAL` /
    `RESIZE_QUIESCE`, `is_scrolling_within`, `mark_scrolling`,
    `is_scrolling`, `next_deadline` (the aggregator), and the
    `scroll_quiesce_tests` block.
  - `pointer.rs` (18) — `update_pointer_shape`.
  - `image_dispatch.rs` (351) — `infos_in_viewport_window`,
    `VIEWPORT_DISPATCH_MARGIN`, `effective_images_enabled`,
    `images_layout_enabled`, `dispatch_image_decodes`,
    `dispatch_visible_image_decodes`, `dispatch_image_decodes_for`,
    plus the `viewport_window_tests` block.
  - `external_editor.rs` (312) — `ExternalEditorOutcome`,
    `open_config_in_editor`, `open_current_file_in_editor`,
    `run_external_editor`, `spawn_open_worker`.
  - `nav.rs` (270) — `NavEntry`, `is_markdown_path`,
    `resolve_link_at_cursor`, `follow_link`, `scroll_to_heading`,
    `navigate_to_file`, `load_file_into_editor`, `current_nav_entry`,
    `navigate_back`, `navigate_forward`, `navigate_to_entry`,
    `open_dirty_guard`.
  - `actions.rs` (413) — `cursor_in_table`, `modal_wheel_delta`,
    `HandleEvent` trait + impl, `any_modal_open`, `handle_app_action`,
    `dispatch_modal_key`, `open_quit_confirm`, the `open_X` overlay
    helpers, `ensure_keymap_clone`, `dispatch_palette_action`,
    `handle_pending_column_widths`, `apply_active_theme`.
- App struct kept its ~30 fields; sub-state structs (NavStack,
  FrameTimer, ImageDispatch, …) deferred per the plan's note that
  step 2's goal is file-size reduction, not full encapsulation.
  Each method's `&mut App` borrow stays whole, which means borrow
  splits can come later without revisiting these moves.
- All `pub` / `pub(crate)` surface preserved.  `MessageKind` re-exported
  from `app.rs` (`pub use flash::MessageKind`) so existing
  `crate::app::MessageKind` paths in `src/app/modal/` still resolve.
- All 736 unit tests + 13 integration test binaries pass.
- `cargo clippy --bins --tests` produces no new warnings (the 4
  remaining warnings under `src/app/` are all pre-existing).

---

## Step 1 — Modal Stack Abstraction

### Problem

Today, every modal is duplicated five ways:

1. **Struct** — `{ body, buttons, state, [extras] }`.
2. **Builder fn** — `build_X(&EditorState, &Config) -> Option<X>` or `open_X(&mut self)`.
3. **`Option<X>` field on `App`**.
4. **`handle_X_key(&mut self, key) → calls X::state.handle_key → ModalResponse`**, then matches on which button index ran.
5. **Two slots in `run()`**: a render-priority arm (~10 lines × 14) and an absorb-input arm (~15 lines × 14).

The render-priority cascade compounds the problem: each new modal must `&& other.is_none()` against every higher-priority modal, so the cascade is O(N²) lines for N modals.

### Design

Introduce a `Modal` trait and a `ModalStack`:

```rust
// src/app/modal/trait.rs
pub trait Modal {
    /// Title shown in the modal frame.
    fn title(&self) -> &str;

    /// Render the modal. Receives the full frame area.
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme,
              cursor_visible: bool);

    /// Apply a key event. Returns whether the event was consumed
    /// and whether the modal should close.
    fn handle_key(&mut self, key: KeyEvent, ctx: &mut ModalCtx<'_>) -> ModalOutcome;

    /// Apply a wheel event for scrollable bodies. Default: no-op.
    fn handle_wheel(&mut self, _delta: i32) {}

    /// Whether arrow keys / Enter on a button should fall through
    /// to the editor when the body has no focusable controls.
    /// Defaults to false (fully absorbing).
    fn passes_through_input(&self) -> bool { false }
}

pub enum ModalOutcome {
    /// Modal stays open, no further action.
    Continue,
    /// Modal closes; optionally dispatch a follow-up Action through
    /// the regular pipeline.
    Close(Option<Action>),
    /// Modal closes; flash a transient message.
    CloseWith { flash: (String, MessageKind) },
    /// Modal closes; run a callback against `App` (used for the
    /// "save_config + persist" pattern shared by several modals).
    CloseAnd(Box<dyn FnOnce(&mut App)>),
}

/// Side-channel into App state that modals legitimately need to
/// touch — config mutation, editor cursor, etc. Passed by &mut
/// to handle_key so the modal doesn't have to own &mut App.
pub struct ModalCtx<'a> {
    pub config: &'a mut Config,
    pub editor: &'a mut EditorState,
    pub keybindings: &'a mut KeyBindingOverrides,
    pub keymap: Option<&'a mut KeyMap>,
    pub doc_height: usize,
    pub doc_width: usize,
}
```

```rust
// src/app/modal/stack.rs
pub struct ModalStack {
    inner: Vec<Box<dyn Modal>>,
}

impl ModalStack {
    pub fn push(&mut self, m: Box<dyn Modal>) { self.inner.push(m); }
    pub fn pop(&mut self) -> Option<Box<dyn Modal>> { self.inner.pop() }
    pub fn top_mut(&mut self) -> Option<&mut dyn Modal> {
        self.inner.last_mut().map(|b| b.as_mut())
    }
    pub fn is_empty(&self) -> bool { self.inner.is_empty() }
    pub fn len(&self) -> usize { self.inner.len() }

    /// Insert at a specific priority position. Used for the few cases
    /// where a high-priority modal (e.g. ConfigWarning) opens while a
    /// lower-priority modal is already showing. Most opens are pushes.
    pub fn insert_above_priority(&mut self, m: Box<dyn Modal>, priority: ModalPriority) { … }
}
```

Priority is no longer encoded in a giant `is_none()` cascade. Instead:

- The stack itself enforces "topmost wins" — input goes to the top, it renders last.
- A small `ModalPriority` enum exists only for the 2–3 cases where a high-priority modal must displace a lower one (e.g. a config-warning that pops up after the user closes the external editor while another modal is visible). These cases are rare and explicit.

### Shared simple-modal wrapper

For modals that are just `{ body, buttons, ModalState }` with a "click-to-close-and-dispatch" pattern (most of them), a `SimpleModal` wrapper covers the entire lifecycle:

```rust
// src/app/modal/simple.rs
pub struct SimpleModal {
    pub title: &'static str,
    pub body: Vec<Line<'static>>,
    pub buttons: Vec<ModalButton>,
    pub state: ModalState,
    pub on_button: fn(&mut App, button_idx: usize),
    pub on_cancel: fn(&mut App),
}
impl Modal for SimpleModal { … }
```

Modals with extra state (`DirtyGuardPrompt`'s `pending: PathBuf`, `WidthInjectionWarning`'s `pending_table_start`) become explicit structs implementing `Modal` directly.

### Migration order within Step 1

To avoid a 1500-line PR:

1. Land `Modal` trait, `ModalStack`, `SimpleModal`, and `ModalCtx` with **no callers**. Pure addition.
2. Migrate one modal end-to-end (suggest **`CheatSheetModal`** — simplest, single button, no state). Validates the design.
3. Migrate the rest in batches of 3–4: simple modals first (`StartupNotice`, `ConfigWarningModal`, `ImagesEnabledPrompt`, `RemoteImagePrompt`), then stateful (`DirtyGuardPrompt`, `QuitConfirm`, `WidthInjectionWarning`).
4. Migrate the **overlays** (`PaletteState`, `SettingsState`, `KeybindsState`, `InsertTableState`, `SaveCopyState`). These already have their own state types in `ui/`; the migration here is to wrap each in a `Modal`-implementing adapter that lives in `app/modal/`.
5. Delete the old render cascade and absorb-input ladder. Replace both with **one render line** and **one input dispatch line** in `run()`:

```rust
// In draw closure:
if let Some(modal) = modal_stack.top_mut() {
    modal.render(frame, frame.area(), theme, cursor_visible);
}

// In run loop:
if let Some(modal) = self.modal_stack.top_mut() {
    match event {
        Event::Key(k) if k.kind == KeyEventKind::Press =>
            self.handle_modal_key(*k),  // → calls top.handle_key, applies outcome
        Event::Mouse(me) =>
            modal.handle_wheel(modal_wheel_delta(me, wheel_step)),
        _ => {}
    }
    continue;
}
```

### Expected impact

| Metric | Before | After |
|---|---|---|
| Modal-related fields on `App` | 13 `Option<X>` | 1 `ModalStack` |
| Modal types defined in `app.rs` | 8 inline + 5 from `ui` | 0 (each in `app/modal/<name>.rs`) |
| Render priority cascade LOC | ~145 | ~5 |
| Absorb-input ladder LOC | ~280 | ~20 |
| `handle_X_key` methods on `App` | 11 | 0 (folded into `Modal::handle_key`) |
| `open_X` methods on `App` | 7 (kept as thin push helpers) | 7 |
| New trait/stack scaffolding | 0 | ~250 |
| **Net `app.rs` reduction** | — | **~–650 LOC** |

---

## Step 2 — Subdomain Extraction

With modals out of the way, the remaining ~3900 lines split into clean subdomains. Each becomes a submodule under `src/app/`, following the project's facade pattern.

### `app/nav.rs` (~200–300 LOC)

Owns `NavStack { back: Vec<NavEntry>, forward: Vec<NavEntry> }`. Methods migrated:

- `current_nav_entry`, `navigate_back`, `navigate_forward`, `navigate_to_entry`, `navigate_to_file`, `load_file_into_editor`, `open_dirty_guard`, `is_markdown_path`.

`load_file_into_editor` is borderline — it touches `file_path`, `editor`, `view_state`, `images_dirty`. Keep as a method on `App` that `nav` calls back into, OR pass a `NavCtx` mutable borrow group. Prefer the callback approach to avoid leaking implementation details.

### `app/external_editor.rs` (~250–350 LOC)

Owns the external-editor flow. Methods migrated:

- `open_config_in_editor`, `open_current_file_in_editor`, `run_external_editor`, `spawn_open_worker`.
- `ExternalEditorOutcome` enum.

This is the most cohesive subdomain — almost no entanglement with the rest of `App` beyond reading `capabilities` and getting a `&mut Terminal`.

### `app/image_dispatch.rs` (~250–350 LOC)

Owns image-decoding orchestration. Pure logic with worker threads. Methods migrated:

- `dispatch_image_decodes`, `dispatch_visible_image_decodes`, `dispatch_image_decodes_for`.
- `infos_in_viewport_window` (already a free fn — moves alongside).
- `effective_images_enabled`, `images_layout_enabled`.
- `drain_pending_image_ready` → split: cache mutations into `ImageDispatch::apply_event`; the channel-draining loop stays in `event_loop.rs`.

### `app/flash.rs` (~100–200 LOC)

Owns the transient-message system. Methods migrated:

- `flash`, `expire_transient_if_due`, `transient_deadline`, `dismiss_sticky_transient`, `flash_for_action`, `hint_content`.
- `MessageKind` (currently `pub` in `app.rs`) and `TransientMessage`.

The hint-content fn currently bundles in `HintPrompt` lookup — the prompt owner can stay on `App` but the rendering branch moves here.

### `app/frame_timer.rs` (~100–200 LOC)

Owns rendering timing concerns. Fields migrated into a `FrameTimer` struct:

- `needs_draw: bool`, `last_draw_at: Option<Instant>`, `resize_quiesce_at: Option<Instant>`, `last_scroll_at: Option<Instant>`.
- Methods: `mark_scrolling`, `is_scrolling`, `next_deadline` (for the timer-related deadlines; the cursor-blink and transient deadlines stay in their own owners' `next_deadline()` and the `App::next_deadline` fn aggregates).
- Constants: `SCROLL_QUIESCE`, `MIN_FRAME_INTERVAL`, `RESIZE_QUIESCE`.

### `app/pointer.rs` (~50 LOC)

Owns the OSC 22 pointer-shape state. Trivial.

- `last_pointer_shape: PointerShape` field, `update_pointer_shape` method.

### `app/actions.rs` (~200–350 LOC)

Owns the action-routing layer. Methods migrated:

- `handle_app_action`, `dispatch_palette_action`, `flash_for_action` (latter may live in `flash.rs` instead — pick one).
- The `cursor_in_table` helper.
- The `HandleEvent` extension trait + impl.

### Migration approach

Each subdomain extraction is a near-mechanical move:

1. Create the new file with the methods relocated.
2. If methods need `&mut App`, keep them as methods on `App` for now and re-export — splitting the borrow can come later if needed. The goal at this step is **file-size reduction**, not full encapsulation.
3. Verify with `cargo test` after each subdomain.

For genuinely independent state (FrameTimer, NavStack, ImageDispatch), introduce the sub-state struct on `App` and migrate fields:

```rust
pub struct App {
    config: Config,
    keybindings: KeyBindingOverrides,
    theme: &'static Theme,
    capabilities: Capabilities,
    file_path: Option<PathBuf>,
    editor: EditorState,
    view_state: EditorViewState,
    keymap: Option<KeyMap>,

    nav: NavStack,
    modals: ModalStack,
    flash: FlashState,
    timer: FrameTimer,
    images: ImageDispatch,
    external: ExternalEditorState,
    pointer: PointerState,
    mouse: MouseState,            // mouse + drag_target + last_pointer_shape adjacent

    should_quit: bool,
    app_tx: Option<mpsc::Sender<AppEvent>>,
    pending_term_event: Option<Event>,
    read_paused: Option<Arc<AtomicBool>>,
    hovered_link: Option<LinkTarget>,
    hint_prompt: Option<HintPrompt>,
}
```

From ~40 fields → ~17. Each remaining field has a clear single owner.

---

## Step 3 — Decompose `run()`

After steps 1 & 2, `run()` will already be substantially smaller — most of its body delegates to the new modules. This step finishes the job by extracting the inline state machine into named methods.

### Target shape for `run()`

```rust
pub fn run(&mut self, mut terminal: Terminal<...>) -> Result<()> {
    self.startup_pointer_hint();
    let (tx, rx) = self.spawn_event_threads()?;
    self.build_keymap_if_needed()?;

    loop {
        self.tick_timers();
        self.coalesce_image_updates();
        let term_size = terminal.size()?;
        let dims = self.compute_doc_dims(term_size);
        self.images.dispatch_visible(&self.editor, dims.scroll, dims.height);

        if self.should_draw() {
            self.draw_frame(&mut terminal, &dims)?;
        }

        let event = match self.next_event(&rx)? {
            Some(e) => e,
            None => continue,  // background event, loop again
        };

        if matches!(event, Event::Resize(_, _)) {
            self.on_resize();
            continue;
        }

        if self.modals.top_mut().is_some() {
            self.dispatch_modal_event(event, &dims, &mut terminal, &rx);
            if self.should_quit { break; }
            continue;
        }

        if let Event::Mouse(m) = event {
            self.dispatch_mouse_event(m, &dims);
            continue;
        }

        if let Event::Paste(text) = event {
            self.dispatch_paste(text, &dims);
            continue;
        }

        self.dispatch_key_event(event, &dims, &mut terminal, &rx);
        if self.should_quit { break; }
    }

    Ok(())
}
```

Each extracted method is < 100 LOC. The whole `run()` fits on a screen.

### Extracted methods

| Method | Source lines today | Estimated LOC after extraction |
|---|---|---|
| `startup_pointer_hint` | 866–868 | 5 |
| `spawn_event_threads` | 869–918 | 50 |
| `build_keymap_if_needed` | 924–926 | 5 |
| `tick_timers` | 932–947 | 20 |
| `coalesce_image_updates` | 954–958 | 10 |
| `compute_doc_dims` | 969–981 + 1689–1700 | 25 |
| `should_draw` | 991–994 | 10 |
| `draw_frame` | 996–1273 | ~80 (most logic moves to `app/draw.rs` and modal `render`) |
| `next_event` | 1285–1369 | 80 |
| `on_resize` | 1382–1391 | 15 |
| `dispatch_modal_event` | 1410–1687 | 30 (one branch on top modal) |
| `dispatch_mouse_event` | 1705–1795 | 90 |
| `dispatch_paste` | 1804–1808 | 10 |
| `dispatch_key_event` | 1810–1880 | 80 |

The 14 separate modal absorption blocks collapse to one `dispatch_modal_event` because the `Modal` trait already encapsulates per-modal behaviour. The 14 separate render arms collapse to one render call because `ModalStack::top_mut().render(…)` does the dispatch.

---

## Step 4 — Test Relocation

### Keep co-located (pure helper unit tests)

These tests verify pure functions and belong next to their implementations. After step 2, that means:

- `is_scrolling_within` test → `app/frame_timer.rs` `#[cfg(test)] mod tests`
- `infos_in_viewport_window` tests → `app/image_dispatch.rs`
- `modal_wheel_delta_translates_scroll_direction` → `app/modal/stack.rs` (or wherever the helper lands)

### Move to integration tests (`tests/`)

These tests construct a full `App` and exercise multi-step behaviour. They should live alongside `tests/editing.rs`, `tests/ui.rs`, etc.:

- `phase9_flash_tests` (~14 tests) → `tests/flash.rs`
- `phase15_insert_table_tests` (~7 tests) → `tests/insert_table.rs`
- `config_warning_modal_tests` (~6 tests) → either `tests/config_warning.rs` OR convert to unit tests on `build_config_warning_modal` in `app/modal/config_warning.rs` (preferred — the tests are pure-function tests against the builder, dressed up as `App` tests because the builder was private).

### New test opportunity

The `Modal` trait + `ModalStack` are testable in isolation. Add `app/modal/stack.rs` `#[cfg(test)] mod tests` covering:

- `push` / `pop` / `top_mut` / `is_empty` invariants
- Priority insertion correctly re-orders the stack
- A fake `MockModal` records its lifecycle calls; assert the ordering matches expectations through a few input sequences

These tests will outlive any specific modal and prevent regressions in the trait contract.

### Test file fixtures

Today's tests use `app_with_buffer(text, cursor_byte)` (lines 4284–4296) which requires substantial setup. After step 2, this fixture moves to `tests/common/mod.rs` (already used elsewhere if present, otherwise newly created) so all integration tests share it.

---

## Risks & Mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| Modal trait dispatch hides subtle behaviour differences (e.g. settings overlay's "Open external editor" intent flag) | Medium | Keep `ModalOutcome::CloseAnd(Box<dyn FnOnce(&mut App)>)` as an escape hatch; first-class variants only for the patterns shared by 3+ modals. |
| Rebuilding `ModalCtx<&mut Config, &mut EditorState, …>` per dispatch causes borrow-checker pain | Medium | Encapsulate the borrow group in a single `ModalCtx::new(&mut self) -> ModalCtx<'_>` constructor; if borrowck fights back, fall back to passing `&mut App` into `Modal::handle_key` (less pure but works). |
| Render priority semantics drift during migration (e.g. config-warning was supposed to render on top of settings overlay; gets buried under it) | Medium-high | Document the current priority order in a table before starting; bake it into `ModalPriority` enum; add an integration test that verifies "opening modal X while modal Y is showing" produces the expected render order. |
| `run()` decomposition introduces subtle deadlock / event-ordering bugs (e.g. `pending_term_event` handling) | Medium-high | Decompose only after step 1 + 2 land cleanly with `cargo test` + manual smoke testing. Each extracted method preserves the exact control flow of its source range; verify by running the full integration suite + a manual session per step. |
| Frame-timer extraction breaks the `next_deadline` aggregation | Low | Keep `App::next_deadline` as the aggregator; it calls into `self.timer.next_deadline()`, `self.flash.next_deadline()`, etc. |
| Subdomain extraction creates new public API surface that's hard to evolve later | Low | Mark all sub-state structs and their methods `pub(crate)` (or `pub(super)` where possible). Nothing in this refactor needs to be `pub`. |
| Modal struct removal breaks tests that name them directly | Medium | Update test assertions to use the trait API (`modals.top_mut().title() == "Quit"`) rather than concrete types. Apply the change in the same PR as the modal migration. |
| Net LOC actually grows due to trait scaffolding | Low | Each step has a target LOC delta. Track in PR description; if step 1 doesn't net-reduce by ≥ 400 LOC, reconsider design before proceeding. |

---

## Success Metrics

A successful refactor satisfies all of:

1. **`src/app.rs` is < 400 LOC** (down from 4591), serving only as the facade + `App` struct + `new` + thin `run`.
2. **No file in `src/app/` exceeds 1000 LOC**, and at most one exceeds 500.
3. **Adding a hypothetical modal #15** requires creating exactly **one new file** in `src/app/modal/` and **zero edits** to a central cascade or absorption ladder.
4. **`App` has ≤ 20 fields**, each with a clear owner (sub-state struct, terminal lifecycle, or top-level coordination).
5. **`run()` body fits in 60 lines** of dispatch logic.
6. **`cargo test` passes with the same number of tests** (modulo the move/split — net count may grow as new `ModalStack` tests land).
7. **`cargo clippy -- -D warnings` passes.**
8. **Manual smoke test** of every modal's open / close / button-press / Escape flow + every overlay's full UX works identically to `main` at the start of the refactor.
9. **No new dependencies in `Cargo.toml`.**

---

## Out of Scope (Explicitly Deferred)

- Splitting `EditorState` (~similar size and god-object characteristics, but a separate concern).
- Reworking the keybinding / action-dispatch architecture.
- Migrating from `mpsc` to `crossbeam` or `tokio` channels.
- Changing the modal *visual* design or the `ui::ModalView` widget itself.
- Adding new modal features (resizable, draggable, animated transitions).
- Replacing `Box::leak` for the `Theme`.
- Any work on Phase 11 (file-change detection) or Phase 16 (HTML export).
