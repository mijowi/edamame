# Issue/Feature Tracker

- ==Add user-facing documentation==
- Add a documentation page for the terminal upgrade notice and a link to the page from within edamame

- Add next cell/prev cell to keybindings modal?

- Refactor/clean up source and tests
- Add/update performance benchmark tests

- Two separate modal content-width systems exist. The 20 `ModalChrome`-backed modals now share an opt-in cap — `ModalView::with_max_content_width` / `ModalChrome::with_max_content_width`, defaulting to size-to-content, with `PROSE_CONTENT_WIDTH` (64 columns of text) as the house value for prose bodies. The 9 overlays that bypass `ModalView` and build `ContentSize` directly (`welcome`, `settings_overlay`, `keybinds_overlay`, `search_modal`, `save_copy_modal`, `insert_table_modal`, `export_html_modal`, `diff_intro_modal`, `searchable_list`) each hand-roll their own sizing instead — e.g. `welcome` pins `const CONTENT_WIDTH = 64`, `keybinds_overlay` raises `KEYBINDS_MAX_PAD_H` to 8. Unify by lifting the cap onto `ContentSize` (where `max_pad_h` already lives) so `centered_rect_for_content` clamps for every caller and `ModalView` merely forwards to it; then the 9 bespoke overlays can drop their private constants. Touches 9 call sites, so it wants its own change.

- Handle paragraphs containing line breaks

## Diff mode
- Do we want to implement editing while in diff mode? If yes, use Ctrl with diff mode keybinds so they don't conflict with editor keybinds.
- Implement rendered diff mode? Use edamame as git difftool for Markdown files?

