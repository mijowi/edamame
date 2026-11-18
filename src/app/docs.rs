//! Opening a page of the embedded manual (`crate::docs`) as the live document: the pathless
//! counterpart to [`super::nav`]'s file loading. See `docs/dev/in-app-docs.md`.

use crate::docs::DocId;
use crate::document::Buffer;
use crate::ui::{EditorViewState, ModalLinkTarget};

use super::nav::NavPending;
use super::App;

impl App {
    /// Replace the live document with `id`'s page.
    ///
    /// The [`super::App::load_file_into_editor`] analogue minus the two file-only steps: no
    /// own-write hash (nothing was opened) and no watcher repoint. The watcher stays armed on
    /// the file the reader came from, and `file_path = None` is the one guard that keeps an
    /// external write to it from being diffed against the manual's text (`handle_file_changed`
    /// / `handle_file_removed` return early unless `file_path` matches). Do not add a second
    /// guard here that could drift from it.
    pub(super) fn load_doc_into_editor(&mut self, id: DocId) {
        // The file being left keeps its position, as on a file switch; after this its path is gone.
        self.record_cursor_position();
        let buffer = Buffer::from_str(&id.source());
        let mut new_editor = self.editor_for_buffer(buffer);
        new_editor.readonly = true;
        // Load-bearing under vim: `editor_for_buffer` already ran `leave_preview_under_vim`,
        // which moved a vim session's editor to Rendered before `readonly` was set. Without
        // this a vim user gets a cursor and raw reveal on a page nobody can edit.
        new_editor.mode = crate::editor::Mode::Preview;
        self.editor = new_editor;
        self.outline_cache_key = None;
        self.outline_selected = 0;
        self.outline_scroll = 0;
        self.outline_focused = false;
        self.outline_browsing = false;
        self.file_path = None;
        self.open_doc = Some(id);
        self.view_state = EditorViewState::new();
        // Vim-Normal and Preview are alternative resting modes; vim is parked, not destroyed.
        self.sync_vim_suspension();
        self.on_document_contents_swapped();
    }

    /// Open `id`, recording the current position so Back returns to it, and jump to `fragment`
    /// if given. Returns whether the page opened, matching [`super::App::navigate_to_file_at`].
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
        self.nav_forward.clear();
        self.load_doc_into_editor(id);
        self.editor.set_viewport_width(doc_width);
        if let Some(frag) = fragment {
            match self.heading_line_for_fragment(&frag) {
                Some(line) => self.scroll_to_rendered_line(line, doc_height, doc_width),
                None => self.flash(
                    format!("Section '{frag}' not found"),
                    super::MessageKind::Info,
                ),
            }
        }
        true
    }

    /// Resume a navigation the dirty guard interrupted, whichever kind of destination it held.
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
    /// Viewport dimensions come from `last_doc_height` / `last_doc_width` because
    /// `Modal::handle_click` carries none; they are refreshed every drawn frame, so they match
    /// what the keyboard path would pass. The dirty guard still applies.
    ///
    /// Refused during a diff review: a modal callback is not an [`crate::config::Action`], so it
    /// bypasses `actions::diff_safe_action`, and startup notices can sit over a review in a
    /// `git difftool` walk. Ungated it would discard the review and record a `Mode::Diff` nav
    /// entry that `restore_nav_entry` re-applies to an editor with no `DiffState`. The refusal
    /// assumes every [`ModalLinkTarget`] replaces the live document; a future browser-only
    /// target would be safe mid-review and should get its own arm.
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

    /// An `App` on a real file: a nav entry can only name a document it can reload.
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
        // Must land on the embedded page, not a same-named file in the working directory.
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
        // `dev/theming.md` is in the repository but not the binary.
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

    /// See `follow_modal_link`: a modal callback bypasses `diff_safe_action`.
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
        assert!(app.nav_back.is_empty());
    }

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

        // Discard.
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
        assert_eq!(app.modal_stack.len(), before, "no save prompt should open");
    }

    #[test]
    fn navigation_and_search_still_work_inside_a_page() {
        use super::super::actions::readonly_safe_action;
        // The read-only gate denies `mutates_buffer` and `needs_path`, never `navigates_away`.
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
        // `search_safe_action` allows these, so the read-only gate must be the outer one.
        for action in [Action::SearchReplace, Action::SearchReplaceAll] {
            assert!(!readonly_safe_action(&action));
        }
    }

    /// The search modal reaches `enter_search_flow` directly, not through an `Action`, so the
    /// read-only gate never sees it; the flow's entry point drops the replacement instead.
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

    /// A capturing flow would hold keys for commands the read-only gate then refuses.
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
        // `Alt+Left` is `TableMoveColumnLeft`, redirected to Back only outside a table. A
        // read-only page has no column to move, so the redirect must fire inside one too, and
        // before the gate that denies the pre-redirect action.
        let mut app = app_with_buffer("notes\n", 0);
        app.open_doc_page(DocId::Editing, None, H, W);
        app.open_doc_page(DocId::Keybindings, None, H, W);
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
