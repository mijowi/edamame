# Optional Docked Outline Implementation Plan

> **REQUIRED SUB-SKILL:** Use the executing-plans skill to implement this plan task-by-task.

**Goal:** Add a persistent opt-in heading outline at the left of the existing Markdown editor, with mouse and Vim-compatible keyboard navigation.

**Architecture:** Reuse the section picker's ParsedDoc heading extraction. Compute one split viewport geometry shared by painting and event dispatch; keep App-only pane focus/list-scroll separate from editor and persisted preference. Use the existing in-document navigation history for committed jumps.

**Tech Stack:** Rust, ratatui, crossterm, ropey, existing config and theme; no new dependencies.

---

### Task 1: Persist the interaction contract

**Files:** `docs/dev/plans/2026-10-09-outline-sidebar-design.md` (created). Treat its decisions and acceptance criteria as binding before editing behavior. Check that `docs/dev/plans` is the repository's existing plan directory.

### Task 2: Shared geometry and heading widget

**Files:** `src/ui/editor_view.rs`, `src/app/event_loop.rs`, `src/ui/section_picker.rs`, `src/app/section_jump.rs`, new `src/ui/outline.rs` only if needed, `src/ui.rs`.

1. Write failing unit tests for the requested split under wide/narrow terminals and for a heading list after a document edit; run each focused test with `cargo +stable-x86_64-pc-windows-gnu test --lib --no-default-features <test-name> -- --exact` and observe the expected failure.
2. Extract one shared layout function that returns outline/separator/actual editor rect. Reuse it in both `EditorView::render` and `App::compute_doc_dims`; keep the bottom full-width. Add a small outline painter with source-heading levels, terminal-cell truncation, current/focused styling, and empty state. Reuse `collect_heading_entries`; never cache width-dependent `target_scroll` long-term.
3. Run the focused tests green; check current rendered/raw image snapshots and scroll layout rely on the new document rect, not the full terminal width.

### Task 3: Pane focus, input, navigation

**Files:** `src/app.rs`, `src/app/event_loop.rs`, `src/app/actions.rs`, `src/app/section_jump.rs`, `src/config/keymap.rs`, `src/ui/editor_view.rs`, `src/ui/outline.rs`.

1. Write failing behavior tests for F8/F6/Esc, selection via Up/Down and Vim j/k, Enter/click jumps, back/forward origin restore, wheel isolation, narrow resize restoration and disabled interactions in diff/modal; run focused tests red.
2. Add ephemeral focus and list-scroll state; dispatch outline keys and mouse events before editor/Vim only while focused or pointer lies in outline. Global F8/F6 must respect modal/command prompt capture and be intercepted before Vim pending-operator swallowing. Keep existing Ctrl-G picker behavior.
3. Commit jump through existing `record_in_doc_jump` and heading scroll helpers with live mode/width, then return editor focus. Run focused tests green; prevent click rows after editing/resize from using a stale target.

### Task 4: Persistent setting and public affordances

**Files:** `src/config/config.rs`, `src/config/keymap.rs`, `src/app/actions.rs`, `src/ui/settings_overlay.rs` and its rows, `src/app/modal/settings.rs`, `src/ui/command_palette/actions.rs`, `config/config.toml`, `config/keybindings.toml`, `docs/keybindings.md`, `docs/vim-mode.md`, `docs/editing.md`.

1. Add failing config/default/roundtrip and keymap tests for `[editor] show_outline = false`, F8/F6, and palette/settings toggle behavior; observe red.
2. Add `ToggleOutline` and a pane-focus action as required; use the current settings-save path and existing theme styles. Keep defaults off. Run focused tests green.
3. Update user-facing instructions and any pinned keymap snapshots if semantics changed; document narrow-screen fallback and vim focus.

### Task 5: Integration and smoke

**Files:** relevant in-module tests and user docs only.

1. Run focused Rust tests followed by `cargo +stable-x86_64-pc-windows-gnu test --no-fail-fast --no-default-features`, `cargo +stable-x86_64-pc-windows-gnu clippy --all-targets --no-default-features -- -D warnings`, and `cargo +stable-x86_64-pc-windows-gnu fmt -- --check`.
2. Launch the actual TUI on a temporary Markdown file in a terminal and observe outline, cursor, resizing, and an Enter-jump. Capture only observed behaviors; if interactive verification cannot run, state the missing surface precisely and exercise an equivalent headless widget render plus input dispatcher.
3. Remove only throwaway inputs and review edited files and doc-links for unintended changes. No unrelated commits or changes to user work.
