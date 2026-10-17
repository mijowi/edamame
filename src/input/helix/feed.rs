use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::editor::helix_ops::actions::Transform;
use crate::editor::vim_ops::Operator;
use crate::editor::{helix_ops, EditorState};

use super::{HelixPending, HelixState, HelixSubMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelixOutcome {
    Consumed,
    Passthrough,
    Flash(String),
    Save { then_quit: bool },
    Quit,
}

/// Global shortcut ownership outside a prompt; ordinary Insert input delegates separately.
pub fn is_app_shortcut(key: KeyEvent, mode: HelixSubMode) -> bool {
    key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
        || (key.modifiers.contains(KeyModifiers::ALT)
            && (mode == HelixSubMode::Insert || key.code != KeyCode::Char(';')))
        || matches!(
            key.code,
            KeyCode::F(_) | KeyCode::PageUp | KeyCode::PageDown
        )
}

/// Feed one document key; Insert delegates to the ordinary Markdown editor.
pub fn helix_feed(
    state: &mut HelixState,
    editor: &mut EditorState,
    key: KeyEvent,
    viewport_height: usize,
    viewport_width: usize,
) -> HelixOutcome {
    if editor.readonly {
        return HelixOutcome::Passthrough;
    }
    editor.set_viewport_width(viewport_width);
    if let Some(prompt) = state.prompt.as_mut() {
        use crate::input::vim::cmdline::{feed_key, CmdLineStep};
        let line = &mut prompt.line;
        if let KeyCode::Char(c) = key.code {
            if c.is_control() || line.input.chars().count() >= super::state::PROMPT_CHAR_CAP {
                return HelixOutcome::Consumed;
            }
        }
        let kind = line.kind;
        let step = feed_key(line, key);
        match step {
            CmdLineStep::Editing => {}
            CmdLineStep::Cancel => state.cancel_prompt(editor, true),
            CmdLineStep::Submit(query) => {
                state.prompt = None;
                if kind == crate::input::vim::state::CmdLineKind::Ex {
                    return match query.trim() {
                        "w" | "write" => HelixOutcome::Save { then_quit: false },
                        "wq" | "write-quit" => HelixOutcome::Save { then_quit: true },
                        "q" | "quit" => HelixOutcome::Quit,
                        _ => HelixOutcome::Flash(
                            "Unsupported Helix command (no arguments or !)".into(),
                        ),
                    };
                }
                state.selection_linewise = false;
                let result = helix_ops::search::start(
                    editor,
                    query,
                    kind == crate::input::vim::state::CmdLineKind::SearchForward,
                    state.sub_mode == HelixSubMode::Select,
                );
                if let Err(message) = result {
                    return HelixOutcome::Flash(message.to_string());
                }
                editor.ensure_cursor_visible(viewport_height, viewport_width);
            }
        }
        return HelixOutcome::Consumed;
    }
    if key.code == KeyCode::Esc
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        let was_insert = state.sub_mode == HelixSubMode::Insert;
        state.sub_mode = HelixSubMode::Normal;
        state.reset_pending();
        if was_insert {
            helix_ops::collapse_selection(editor);
        }
        return HelixOutcome::Consumed;
    }
    if state.sub_mode == HelixSubMode::Insert {
        return HelixOutcome::Passthrough;
    }
    if is_app_shortcut(key, state.sub_mode) {
        state.reset_pending();
        return HelixOutcome::Passthrough;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        state.reset_pending();
    }
    if !matches!(
        key.code,
        KeyCode::Char('x' | 'X' | 'y' | 'd' | 'c' | 'v' | '/' | '?' | ':' | '0'..='9')
    ) {
        state.selection_linewise = false;
    }
    match state.pending {
        HelixPending::None => {}
        HelixPending::Replace => {
            let result = match key.code {
                KeyCode::Char(c) => helix_ops::actions::transform(editor, Transform::Replace(c), 1),
                _ => Ok(()),
            };
            state.reset_pending();
            if let Err(message) = result {
                return HelixOutcome::Flash(message.to_string());
            }
            state.sub_mode = HelixSubMode::Normal;
            return HelixOutcome::Consumed;
        }
        HelixPending::Find(command) => {
            let target = match key.code {
                KeyCode::Char(c) => Some(c),
                KeyCode::Enter => Some('\n'),
                _ => None,
            };
            if let Some(target) = target {
                helix_ops::find::find(
                    editor,
                    command,
                    target,
                    state.count.unwrap_or(1),
                    state.sub_mode == HelixSubMode::Select,
                );
            }
            state.reset_pending();
            editor.ensure_cursor_visible(viewport_height, viewport_width);
            return HelixOutcome::Consumed;
        }
        HelixPending::G => {
            let motion = match key.code {
                KeyCode::Char('g') => Some(match state.count {
                    Some(line) => helix_ops::navigation::Navigation::GotoLine(line),
                    None => helix_ops::navigation::Navigation::DocStart,
                }),
                KeyCode::Char('e') => Some(helix_ops::navigation::Navigation::DocEnd),
                KeyCode::Char('h') => Some(helix_ops::navigation::Navigation::LineStart),
                KeyCode::Char('l') => Some(helix_ops::navigation::Navigation::LineEnd),
                KeyCode::Char('s') => Some(helix_ops::navigation::Navigation::FirstNonBlank),
                _ => None,
            };
            if let Some(motion) = motion {
                let count = state.count.unwrap_or(1).clamp(1, 9999);
                helix_ops::navigation::navigate(
                    editor,
                    motion,
                    count,
                    state.sub_mode == HelixSubMode::Select,
                    viewport_height,
                    viewport_width,
                );
            }
            state.reset_pending();
            return HelixOutcome::Consumed;
        }
    }
    if !key.modifiers.intersects(KeyModifiers::ALT) {
        if let KeyCode::Char(digit @ '0'..='9') = key.code {
            if digit != '0' || state.count.is_some() {
                let next = state
                    .count
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit as u32 - '0' as u32);
                state.count = Some(next);
                return HelixOutcome::Consumed;
            }
        }
    }
    let count = state.count.unwrap_or(1).clamp(1, 9999);
    helix_ops::normalize_selection(editor);
    let transform = match (key.code, key.modifiers.contains(KeyModifiers::ALT)) {
        (KeyCode::Char('>'), false) => Some(Transform::Indent),
        (KeyCode::Char('<'), false) => Some(Transform::Outdent),
        (KeyCode::Char('~'), false) => Some(Transform::SwapCase),
        (KeyCode::Char('J'), false) => Some(Transform::Join),
        _ => None,
    };
    if let Some(transform) = transform {
        state.reset_pending();
        if let Err(message) = helix_ops::actions::transform(editor, transform, count) {
            return HelixOutcome::Flash(message.to_string());
        }
        state.sub_mode = HelixSubMode::Normal;
        editor.ensure_cursor_visible(viewport_height, viewport_width);
        return HelixOutcome::Consumed;
    }
    if key.code == KeyCode::Char('g') && !key.modifiers.contains(KeyModifiers::ALT) {
        state.pending = HelixPending::G;
        return HelixOutcome::Consumed;
    }
    if let Some(motion) = helix_ops::navigation::from_key(key.code) {
        helix_ops::navigation::navigate(
            editor,
            motion,
            count,
            state.sub_mode == HelixSubMode::Select,
            viewport_height,
            viewport_width,
        );
        state.reset_pending();
        return HelixOutcome::Consumed;
    }
    match (key.code, key.modifiers.contains(KeyModifiers::ALT)) {
        (KeyCode::Char(';'), true) => helix_ops::flip_selection(editor),
        (_, true) => state.reset_pending(),
        (KeyCode::Char(command @ ('f' | 'F' | 't' | 'T')), false) => {
            state.pending = HelixPending::Find(command);
            return HelixOutcome::Consumed;
        }
        (KeyCode::Char(command @ ('u' | 'U')), false) => {
            editor.selection = None;
            for _ in 0..count {
                let version = editor.buffer.version();
                crate::editor::edit_ops::apply(
                    editor,
                    if command == 'u' {
                        crate::config::Action::Undo
                    } else {
                        crate::config::Action::Redo
                    },
                    viewport_height,
                    viewport_width,
                );
                if editor.buffer.version() == version {
                    break;
                }
            }
            editor.history.break_group();
            helix_ops::collapse_selection(editor);
            state.sub_mode = HelixSubMode::Normal;
        }
        (KeyCode::Char(command @ ('/' | '?' | ':')), false) => {
            state.reset_pending();
            state.prompt = Some(super::state::HelixPrompt {
                line: crate::input::vim::state::CmdLineState::new(match command {
                    '/' => crate::input::vim::state::CmdLineKind::SearchForward,
                    '?' => crate::input::vim::state::CmdLineKind::SearchBackward,
                    _ => crate::input::vim::state::CmdLineKind::Ex,
                }),
                origin: super::state::PromptOrigin {
                    selection: editor.selection,
                    cursor: editor.cursor,
                    scroll: editor.scroll,
                    version: editor.buffer.version(),
                },
            });
        }
        (KeyCode::Char(command @ ('n' | 'N')), false) => {
            state.reset_pending();
            if let Err(message) = helix_ops::search::next(
                editor,
                command == 'n',
                count,
                state.sub_mode == HelixSubMode::Select,
            ) {
                return HelixOutcome::Flash(message.to_string());
            }
        }
        (KeyCode::Char('r'), false) => {
            state.pending = HelixPending::Replace;
            return HelixOutcome::Consumed;
        }
        (KeyCode::Char(command @ ('p' | 'P')), false) => {
            state.reset_pending();
            if let Err(message) = helix_ops::paste::paste(
                editor,
                &state.register.text,
                state.register.linewise,
                command == 'p',
                count,
            ) {
                return HelixOutcome::Flash(message.to_string());
            }
            state.sub_mode = HelixSubMode::Normal;
        }
        (KeyCode::Char('R'), false) => {
            state.reset_pending();
            if !state.register.text.is_empty() {
                if let Err(message) =
                    helix_ops::actions::replace_selection(editor, &state.register.text)
                {
                    return HelixOutcome::Flash(message.to_string());
                }
            }
            state.sub_mode = HelixSubMode::Normal;
        }
        (KeyCode::Char('x' | 'X'), false) => {
            helix_ops::line::select_lines(editor, count, key.code == KeyCode::Char('x'));
            state.selection_linewise = true;
        }
        (KeyCode::Char('%'), false) => {
            helix_ops::set_selection(
                editor,
                crate::document::Selection {
                    anchor: 0,
                    active: editor.buffer.len_chars(),
                },
            );
        }
        (KeyCode::Char(';'), false) => helix_ops::collapse_selection(editor),
        (KeyCode::Char('v'), false) => {
            state.sub_mode = if state.sub_mode == HelixSubMode::Select {
                HelixSubMode::Normal
            } else {
                HelixSubMode::Select
            };
        }
        (KeyCode::Char(command @ ('i' | 'a' | 'I' | 'A' | 'o' | 'O')), false) => {
            state.reset_pending();
            if let Err(message) =
                helix_ops::insert::enter(editor, command, viewport_height, viewport_width)
            {
                return HelixOutcome::Flash(message.to_string());
            }
            state.sub_mode = HelixSubMode::Insert;
        }
        (KeyCode::Char(key @ ('w' | 'e' | 'b' | 'W' | 'E' | 'B')), false) => {
            let motion = match key.to_ascii_lowercase() {
                'w' => helix_ops::WordMotion::Forward,
                'e' => helix_ops::WordMotion::End,
                _ => helix_ops::WordMotion::Backward,
            };
            helix_ops::select_word(
                editor,
                motion,
                key.is_ascii_uppercase(),
                count,
                state.sub_mode == HelixSubMode::Select,
            );
        }
        (KeyCode::Char('d' | 'c' | 'y'), false) => {
            state.reset_pending();
            let op = match key.code {
                KeyCode::Char('d') => Operator::Delete,
                KeyCode::Char('c') => Operator::Change,
                _ => Operator::Yank,
            };
            match helix_ops::apply_operator(editor, op, state.selection_linewise) {
                Ok(result) => {
                    if !result.register_text.is_empty() {
                        state.register.text = result.register_text;
                        state.register.linewise = result.linewise;
                    }
                    state.sub_mode = if result.enter_insert {
                        HelixSubMode::Insert
                    } else {
                        HelixSubMode::Normal
                    };
                }
                Err(message) => return HelixOutcome::Flash(message.to_string()),
            }
        }
        _ => state.reset_pending(),
    }
    state.reset_pending();
    editor.ensure_cursor_visible(viewport_height, viewport_width);
    HelixOutcome::Consumed
}
