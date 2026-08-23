//! Opening a page of the embedded manual (`crate::docs`) as the live
//! document.
//!
//! The pathless counterpart to [`super::nav`]'s file loading.  Every
//! page is compiled into the binary, so this never touches the disk:
//! there is no read to fail, no own-write hash to stamp, and nothing
//! for the watcher to watch.

use crate::docs::DocId;
use crate::document::Buffer;
use crate::ui::{EditorViewState, ModalLinkTarget};

use super::nav::NavPending;
use super::App;

impl App {
    /// Replace the live document with `id`'s page.
    ///
    /// The [`super::App::load_file_into_editor`] analogue, sharing its
    /// editor wiring through `editor_for_buffer` and deliberately
    /// skipping the two steps that only make sense for a real file:
    ///
    /// * **No own-write hash.**  `set_disk_hash` exists to suppress the
    ///   watcher event some backends synthesize on `open(2)`.  Nothing
    ///   was opened.
    /// * **No watcher repoint.**  There is no path to watch, and the
    ///   watcher must be left armed on the file the reader came from.
    ///   Setting `file_path = None` is what makes that safe:
    ///   `handle_file_changed` and `handle_file_removed` both return
    ///   early unless `file_path` names the changed path, so an
    ///   external write that lands while the manual is open is dropped
    ///   rather than being diffed against the manual's text.  That
    ///   guard is the only thing standing between a reader and a diff
    ///   review of two unrelated documents — do not add a second one
    ///   here that could drift from it.
    ///
    /// `on_document_contents_swapped` still runs, exactly as for any
    /// other document.  The shipped pages carry no images today, and
    /// the three media prompts already decline for a document with
    /// none — but skipping the call would mean a page that *gains* an
    /// image silently never decodes it, which is the failure mode that
    /// call exists to prevent.
    pub(super) fn load_doc_into_editor(&mut self, id: DocId) {
        let buffer = Buffer::from_str(&id.source());
        let mut new_editor = self.editor_for_buffer(buffer);
        // The flag the layers below `app` read to refuse a mutation
        // they would otherwise perform without ever producing an
        // `Action` — see `EditorState::readonly`.
        new_editor.readonly = true;
        // A read-only document rests in Preview, which *is* the
        // read-only view: no cursor is drawn, no block reveals its raw
        // source, and `mouse_ops::apply_preview_action` already refuses
        // the checkbox toggle and the table handles.
        //
        // **Load-bearing under vim, not a defensive restatement.**  An
        // `EditorState` is born in Preview, but `editor_for_buffer` has
        // already run `configure_new_editor` → `leave_preview_under_vim`
        // by the time we get here, and that moves a vim session's new
        // editor to Rendered.  Its `readonly` early-return cannot help:
        // the flag is set on the line above, one call too late.  So this
        // line is what puts a manual page back where reading mode lives
        // — deleting it drops a vim user into Rendered with a cursor and
        // a raw reveal on a page nobody can edit
        // (`a_read_only_document_parks_the_vim_session_and_gives_it_back`
        // pins it).  Setting the flag inside `editor_for_buffer` instead
        // would mean threading a `readonly` argument through the one
        // constructor both document kinds share, for a fact only one of
        // them has.
        new_editor.mode = crate::editor::Mode::Preview;
        self.editor = new_editor;
        self.file_path = None;
        self.open_doc = Some(id);
        self.view_state = EditorViewState::new();
        // Vim-Normal and Preview are alternative resting modes, so a
        // read-only document suspends vim for its duration.  Parked,
        // not destroyed — the session comes back with the next
        // editable buffer.
        self.sync_vim_suspension();
        self.on_document_contents_swapped();
    }

    /// Open `id`, recording the current position so Back returns to it,
    /// and jump to `fragment` when the link named a section.
    ///
    /// Returns whether the page was opened, mirroring
    /// [`super::App::navigate_to_file_at`]'s contract — the dirty
    /// guard's arms use it to decide whether to re-assert cursor
    /// visibility on the document they were covering.
    pub(super) fn open_doc_page(
        &mut self,
        id: DocId,
        fragment: Option<String>,
        doc_height: usize,
        doc_width: usize,
    ) -> bool {
        if let Some(entry) = self.current_origin_entry() {
            self.nav_back.push(entry);
        }
        // Browser semantics: a fresh navigation abandons the forward
        // stack, exactly as `navigate_to_file` does.
        self.nav_forward.clear();
        self.load_doc_into_editor(id);
        self.editor.set_viewport_width(doc_width);
        if let Some(frag) = fragment {
            match self.heading_line_for_fragment(&frag) {
                Some(line) => self.scroll_to_rendered_line(line, doc_height, doc_width),
                // The page opened; only the section is missing.  Say so
                // rather than silently landing at the top — the reader
                // asked for a specific place.
                None => self.flash(
                    format!("Section '{frag}' not found"),
                    super::MessageKind::Info,
                ),
            }
        }
        true
    }

    /// Resume a navigation the dirty guard interrupted, whichever kind
    /// of destination it was holding.
    ///
    /// One dispatcher so the guard's Save, Save-as and Discard arms
    /// each carry a single call rather than branching three times over
    /// the same two cases.
    pub(super) fn navigate_to_pending(
        &mut self,
        pending: NavPending,
        fragment: Option<String>,
        doc_height: usize,
        doc_width: usize,
    ) -> bool {
        match pending {
            NavPending::File(path) => {
                self.navigate_to_file_at(path, fragment, doc_height, doc_width)
            }
            NavPending::Doc(id) => self.open_doc_page(id, fragment, doc_height, doc_width),
        }
    }

    /// Follow a link activated from inside a modal.
    ///
    /// Two things separate this from [`super::App::follow_link`], both
    /// consequences of the caller being an overlay rather than a
    /// document.
    ///
    /// **The viewport dimensions come from the cache, not the caller.**
    /// `Modal::handle_click` carries no dimensions — only
    /// `handle_key` does — so a click-activated link would otherwise
    /// need them threaded through every `handle_click` override in
    /// `app::modal`, thirty-six of them, purely to reach this one
    /// call.  `last_doc_height` / `last_doc_width` are refreshed every
    /// frame in `prepare_viewport`, and a modal can only be clicked on
    /// a frame that was drawn, so the cached pair is the same one the
    /// keyboard path would have passed.
    ///
    /// **A manual page still goes through the dirty guard.**  Opening
    /// one replaces the document, exactly as following a cross-file
    /// link does, so unsaved work must not be dropped just because the
    /// navigation started from a modal.
    ///
    /// **And it is refused outright during a diff review.**  A modal
    /// callback is not an [`crate::config::Action`], so it never passes
    /// `actions::diff_safe_action` — the gate that otherwise makes it
    /// impossible to navigate away mid-review.  Reachable in a
    /// `git difftool` walk, where `App::new` pushes the config-warning
    /// and capabilities notices *before* `main` enters diff mode, so a
    /// link-bearing modal can sit over a review on the very first
    /// frame.  Left ungated, following one discards the review with no
    /// confirmation and records a nav entry stamped `Mode::Diff`, which
    /// `restore_nav_entry` then re-applies to a fresh editor that has no
    /// `DiffState` behind it — a blank document area under a diff hint
    /// row.  The refusal is a flash rather than the quit-confirm modal:
    /// a review is abandoned only by an explicit decision, and clicking
    /// a footnote is not one.  It is written against *replacing the
    /// live document*, which is what every [`ModalLinkTarget`] does
    /// today — a future target that merely hands a URL to the browser
    /// would be safe mid-review and should say so with an arm of its
    /// own.
    pub(super) fn follow_modal_link(&mut self, target: ModalLinkTarget) {
        if self.editor.mode == crate::editor::Mode::Diff {
            self.flash_action_unavailable("diff review");
            return;
        }
        let (h, w) = (self.last_doc_height, self.last_doc_width);
        let ModalLinkTarget { id, fragment } = target;
        let fragment = fragment.map(str::to_owned);
        if self.editor.dirty {
            self.open_dirty_guard(NavPending::Doc(id), fragment);
        } else {
            self.open_doc_page(id, fragment, h, w);
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::nav::{NavDest, NavPending};
    use crate::app::test_utils::app_with_buffer;
    use crate::config::Action;
    use crate::docs::DocId;
    use crate::editor::Mode;

    const H: usize = 20;
    const W: usize = 80;

    /// An `App` whose document is a real file on disk.
    ///
    /// `app_with_buffer` leaves `file_path` unset, and a nav entry can
    /// only name a document it can reload — so the back/forward tests
    /// need a document with a path, exactly as they would for a
    /// cross-file link.
    fn app_on_file(contents: &str) -> (tempfile::NamedTempFile, crate::app::App) {
        use std::io::Write;
        let mut f = tempfile::Builder::new()
            .suffix(".md")
            .tempfile()
            .expect("temp file");
        f.write_all(contents.as_bytes()).expect("write");
        f.flush().expect("flush");
        let mut app = app_with_buffer("", 0);
        app.load_file_into_editor(f.path().to_path_buf())
            .expect("load");
        (f, app)
    }

    #[test]
    fn opening_a_page_loads_it_pathless_and_read_only() {
        let mut app = app_with_buffer("hello\n", 0);
        assert!(app.open_doc_page(DocId::Keybindings, None, H, W));

        assert_eq!(app.open_doc, Some(DocId::Keybindings));
        assert!(app.editor.readonly);
        // Pathless is what keeps the watcher and every save path away
        // from a document that has no file.
        assert!(app.file_path.is_none());
        assert!(app.editor.buffer.path().is_none());
        assert!(app
            .editor
            .buffer
            .contents()
            .contains("Terminal compatibility"));
    }

    #[test]
    fn the_status_bar_names_the_page_rather_than_reading_no_file() {
        let mut app = app_with_buffer("hello\n", 0);
        app.open_doc_page(DocId::VimMode, None, H, W);
        assert_eq!(app.display_filename(), "Docs: Vim mode");
    }

    #[test]
    fn a_fragment_lands_on_that_section() {
        let mut app = app_with_buffer("hello\n", 0);
        app.open_doc_page(
            DocId::Keybindings,
            Some("terminal-compatibility".to_owned()),
            H,
            W,
        );
        assert!(
            app.editor.scroll > 0,
            "a deep link should move the viewport off the top"
        );
    }

    #[test]
    fn a_fragment_naming_no_section_still_opens_the_page() {
        let mut app = app_with_buffer("hello\n", 0);
        app.open_doc_page(DocId::Themes, Some("no-such-heading".to_owned()), H, W);
        // The page did load — only the section was missing, which is
        // reported rather than swallowed.
        assert_eq!(app.open_doc, Some(DocId::Themes));
        assert_eq!(app.editor.scroll, 0);
    }

    #[test]
    fn back_returns_from_a_page_to_the_users_own_document() {
        let (_f, mut app) = app_on_file("my own notes\n");
        app.open_doc_page(DocId::Editing, None, H, W);
        assert_eq!(app.nav_back.len(), 1);

        app.navigate_back(H, W);
        assert!(app.open_doc.is_none(), "back should leave the manual");
        assert!(
            !app.editor.readonly,
            "the user's document is editable again"
        );
        assert!(app.editor.buffer.contents().contains("my own notes"));
    }

    #[test]
    fn an_unsaved_document_records_no_way_back_just_as_a_link_would_not() {
        // A nav entry names a document by path so it can be reloaded,
        // and an unsaved buffer has none.  Following a cross-file link
        // out of one already behaves this way; the manual is not a new
        // exception, and nothing is at risk — a buffer with anything in
        // it is dirty, so the guard has already had its say.
        let mut app = app_with_buffer("scratch\n", 0);
        app.open_doc_page(DocId::Editing, None, H, W);
        assert!(app.nav_back.is_empty());
    }

    #[test]
    fn back_and_forward_walk_between_two_pages() {
        let mut app = app_with_buffer("notes\n", 0);
        app.open_doc_page(DocId::Editing, None, H, W);
        app.open_doc_page(DocId::Themes, None, H, W);
        assert_eq!(app.open_doc, Some(DocId::Themes));

        app.navigate_back(H, W);
        assert_eq!(app.open_doc, Some(DocId::Editing));
        assert!(app.editor.readonly, "still inside the manual");

        app.navigate_forward(H, W);
        assert_eq!(app.open_doc, Some(DocId::Themes));
    }

    #[test]
    fn a_cross_page_link_resolves_against_the_embedded_set() {
        let mut app = app_with_buffer("notes\n", 0);
        app.open_doc_page(DocId::Index, None, H, W);
        // The generated index links every page by file name; following
        // one must land on the embedded page, never on whatever file of
        // that name sits in the working directory.
        app.follow_link(
            crate::editor::link::LinkTarget::LocalFile {
                path: std::path::PathBuf::from("security.md"),
                fragment: None,
            },
            H,
            W,
        );
        assert_eq!(app.open_doc, Some(DocId::Security));
    }

    #[test]
    fn a_link_out_of_the_embedded_set_is_not_opened_as_a_page() {
        let mut app = app_with_buffer("notes\n", 0);
        app.open_doc_page(DocId::Themes, None, H, W);
        // `dev/theming.md` ships in the repository but not in the
        // binary — it must go to the browser, leaving the current page
        // in place rather than blanking the document.
        app.follow_link(
            crate::editor::link::LinkTarget::LocalFile {
                path: std::path::PathBuf::from("dev/theming.md"),
                fragment: None,
            },
            H,
            W,
        );
        assert_eq!(app.open_doc, Some(DocId::Themes));
    }

    /// A modal callback is not an `Action`, so it never passes
    /// `diff_safe_action` — the gate that makes navigating away
    /// mid-review impossible everywhere else.  Reachable in a
    /// `git difftool` walk, where the startup notices are pushed before
    /// `main` enters diff mode.
    #[test]
    fn a_modal_link_is_refused_during_a_diff_review() {
        use crate::ui::ModalLinkTarget;

        let mut app = app_with_buffer("alpha\n", 0);
        app.enter_diff_mode("bravo\n".to_owned());
        assert!(app.editor.diff.is_some(), "a review is under way");

        app.follow_modal_link(ModalLinkTarget {
            id: DocId::Security,
            fragment: None,
        });

        assert!(app.open_doc.is_none(), "the page must not have opened");
        assert_eq!(app.editor.mode, Mode::Diff, "the review is still showing");
        assert!(
            app.editor.diff.is_some(),
            "the review must not be discarded by a footnote click"
        );
        // And no nav entry stamped `Mode::Diff` was recorded, which
        // `restore_nav_entry` would later re-apply to an editor with no
        // `DiffState` behind it.
        assert!(app.nav_back.is_empty());
    }

    /// The other half, so the refusal above cannot be over-broad: with
    /// no review under way the same call opens the page, fragment and
    /// all.
    #[test]
    fn a_modal_link_outside_a_diff_review_opens_its_page() {
        use crate::ui::ModalLinkTarget;

        let mut app = app_with_buffer("alpha\n", 0);
        app.last_doc_height = H;
        app.last_doc_width = W;
        app.follow_modal_link(ModalLinkTarget {
            id: DocId::Keybindings,
            fragment: Some("terminal-compatibility"),
        });

        assert_eq!(app.open_doc, Some(DocId::Keybindings));
        assert!(app.editor.scroll > 0, "the fragment landed on its section");
    }

    #[test]
    fn an_ordinary_document_is_unaffected_by_the_doc_resolver() {
        // The interception is gated on a page being open, so a user
        // document linking to a file of its own named `security.md`
        // still resolves against the filesystem.
        let mut app = app_with_buffer("[x](security.md)\n", 0);
        assert!(app.open_doc.is_none());
        app.follow_link(
            crate::editor::link::LinkTarget::LocalFile {
                path: std::path::PathBuf::from("security.md"),
                fragment: None,
            },
            H,
            W,
        );
        assert!(
            app.open_doc.is_none(),
            "a real document must never fall into the manual"
        );
    }

    #[test]
    fn opening_a_page_from_a_dirty_buffer_raises_the_guard_first() {
        let mut app = app_with_buffer("draft\n", 0);
        app.editor.dirty = true;
        let before = app.modal_stack.len();
        app.dispatch_action(Action::OpenDoc(DocId::Security), H, W);

        assert!(app.open_doc.is_none(), "the guard must come first");
        assert_eq!(
            app.modal_stack.len(),
            before + 1,
            "the guard should be on top"
        );

        // Discard, and the pending page opens.
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), H, W);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), H, W);
        assert_eq!(app.open_doc, Some(DocId::Security));
    }

    #[test]
    fn the_guard_names_the_page_it_is_about_to_open() {
        assert_eq!(
            NavPending::Doc(DocId::Keybindings).display_name(),
            "the Keybindings documentation"
        );
    }

    #[test]
    fn leaving_a_page_records_it_so_the_way_back_exists() {
        let mut app = app_with_buffer("notes\n", 0);
        app.open_doc_page(DocId::Editing, None, H, W);
        app.open_doc_page(DocId::Security, None, H, W);
        assert!(matches!(
            app.nav_back.last().map(|e| &e.dest),
            Some(NavDest::EmbeddedDoc(DocId::Editing))
        ));
    }

    #[test]
    fn opening_a_page_abandons_the_forward_stack() {
        let (_f, mut app) = app_on_file("notes\n");
        app.open_doc_page(DocId::Editing, None, H, W);
        app.navigate_back(H, W);
        assert_eq!(app.nav_forward.len(), 1);
        // Browser semantics: a fresh navigation drops the forward path.
        app.open_doc_page(DocId::Themes, None, H, W);
        assert!(app.nav_forward.is_empty());
    }

    // ── The read-only gate ────────────────────────────────────────

    #[test]
    fn a_mutating_action_is_refused_while_a_page_is_open() {
        let mut app = app_with_buffer("notes\n", 0);
        app.open_doc_page(DocId::Security, None, H, W);
        let before = app.editor.buffer.contents();

        app.dispatch_action(Action::InsertChar('x'), H, W);
        app.dispatch_action(Action::Newline, H, W);
        app.dispatch_action(Action::DeleteCharBack, H, W);
        app.dispatch_action(Action::Paste, H, W);

        assert_eq!(app.editor.buffer.contents(), before);
        assert!(!app.editor.dirty, "a refused edit must not dirty the page");
    }

    #[test]
    fn saving_a_page_is_refused_rather_than_detouring_into_save_as() {
        let mut app = app_with_buffer("notes\n", 0);
        app.open_doc_page(DocId::Security, None, H, W);
        let before = app.modal_stack.len();
        app.dispatch_action(Action::Save, H, W);
        app.dispatch_action(Action::SaveAs, H, W);
        // A Save-as prompt for a document the reader cannot own is a
        // state worth never reaching.
        assert_eq!(app.modal_stack.len(), before, "no save prompt should open");
    }

    #[test]
    fn navigation_and_search_still_work_inside_a_page() {
        use super::super::actions::readonly_safe_action;
        // Cross-linking and section jumping are what the manual is for,
        // so these must survive the gate that denies editing — a
        // read-only document denies `mutates_buffer` and `needs_path`,
        // never `navigates_away`.
        for action in [
            Action::ScrollDown,
            Action::MoveDown,
            Action::SelectAll,
            Action::Copy,
            Action::NavigateBack,
            Action::NavigateForward,
            Action::FollowLinkUnderCursor,
            Action::GoToSection,
            Action::OpenSearch,
            Action::SearchNext,
        ] {
            assert!(
                readonly_safe_action(&action),
                "{action} should be allowed while reading the manual"
            );
        }
    }

    #[test]
    fn a_replace_flow_cannot_be_started_inside_a_page() {
        use super::super::actions::readonly_safe_action;
        // `search_safe_action` allows the replace actions, so the
        // read-only gate has to be the outer one or a replace flow
        // would rewrite the in-memory page through an allowlist that
        // never heard of it.
        for action in [Action::SearchReplace, Action::SearchReplaceAll] {
            assert!(!readonly_safe_action(&action));
        }
    }

    /// The search modal reaches `enter_search_flow` directly, not
    /// through an `Action`, so the gate above never sees it — and the
    /// flow's own Preview → Rendered transition is a bare
    /// `editor.mode = …` that neither `enter_edit_if_preview` nor
    /// `apply_delta` can refuse.  Dropping the replacement at the flow's
    /// one entry point is what closes that.
    #[test]
    fn a_replace_typed_into_the_search_modal_degrades_to_a_find() {
        let mut app = app_with_buffer("notes\n", 0);
        app.open_doc_page(DocId::Security, None, H, W);
        app.enter_search_flow("the".to_owned(), Some("THE".to_owned()));

        let search = app
            .editor
            .search
            .as_ref()
            .expect("the find half still runs");
        assert!(
            !search.is_replace_flow(),
            "a read-only page gets the find half only"
        );
        assert_eq!(
            app.editor.mode,
            Mode::Preview,
            "reading mode survives a search"
        );
    }

    /// The find-only flow must also stay *non-capturing*.  A capturing
    /// replace flow traps `Tab` / `r` / `a` at three choke points and
    /// default-denies everything off `search_safe_action` — which on a
    /// page nobody can edit would hold keys for commands the read-only
    /// gate then refuses.
    #[test]
    fn a_search_started_from_the_modal_does_not_capture_input() {
        let mut app = app_with_buffer("notes\n", 0);
        app.open_doc_page(DocId::Security, None, H, W);
        app.enter_search_flow("the".to_owned(), Some("THE".to_owned()));

        assert!(
            !app.search_flow_captures(),
            "a find-only flow never captures"
        );
        assert_eq!(app.editor.mode, Mode::Preview);
    }

    /// An ordinary document is untouched: a typed replacement still
    /// starts a real replace flow.
    #[test]
    fn an_editable_document_still_gets_its_replace_flow() {
        let mut app = app_with_buffer("the quick the\n", 0);
        app.enter_search_flow("the".to_owned(), Some("THE".to_owned()));
        assert!(app
            .editor
            .search
            .as_ref()
            .expect("flow entered")
            .is_replace_flow());
        assert_eq!(
            app.editor.mode,
            Mode::Rendered,
            "a replace flow leaves Preview"
        );
    }

    /// The five denials the read-only rule resolves to, named
    /// explicitly — the list the rule replaced.
    #[test]
    fn the_read_only_rule_denies_exactly_the_five_it_is_meant_to() {
        use super::super::actions::readonly_safe_action;
        for action in [
            Action::InsertTable,
            Action::Save,
            Action::SaveAs,
            Action::ExportHtml,
            Action::OpenInExternalEditor,
        ] {
            assert!(!readonly_safe_action(&action), "{action} should be denied");
        }
    }

    #[test]
    fn a_page_cannot_be_switched_out_of_preview() {
        // Reading mode *is* Preview: the mode transition is what the
        // whole design refuses, so the two actions that request one are
        // no-ops rather than doors into a cursor and a raw reveal.
        let mut app = app_with_buffer("notes\n", 0);
        app.open_doc_page(DocId::Editing, None, H, W);
        assert_eq!(app.editor.mode, Mode::Preview);
        app.dispatch_action(Action::ToggleRawMode, H, W);
        assert_eq!(app.editor.mode, Mode::Preview, "raw view is not reachable");
        app.dispatch_action(Action::EnterEditMode, H, W);
        assert_eq!(app.editor.mode, Mode::Preview, "edit mode is not reachable");
        assert!(app.editor.readonly);
    }

    #[test]
    fn alt_left_navigates_back_from_inside_one_of_the_manual_s_tables() {
        // `keybindings.md` is mostly tables, and the default `Alt+Left`
        // binding is `TableMoveColumnLeft` — redirected to Back only
        // outside a table.  A read-only document has no column to
        // reorder, so the redirect must always fire, and it must fire
        // *before* the gate that denies the pre-redirect action.
        let mut app = app_with_buffer("notes\n", 0);
        app.open_doc_page(DocId::Editing, None, H, W);
        app.open_doc_page(DocId::Keybindings, None, H, W);
        // Park the cursor inside the page's first table.
        let src = app.editor.buffer.contents();
        let table_byte = src.find("\n|").expect("keybindings.md has a table") + 1;
        app.editor.cursor.offset = app.editor.buffer.rope().byte_to_char(table_byte);
        assert_eq!(app.open_doc, Some(DocId::Keybindings));

        app.dispatch_action(Action::TableMoveColumnLeft, H, W);
        assert_eq!(
            app.open_doc,
            Some(DocId::Editing),
            "Alt+Left must navigate back, not be denied as a column move"
        );
    }
}
