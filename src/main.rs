use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use clap::{Parser, Subcommand, ValueEnum};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::ffi::OsString;
use std::io;
use std::io::Write;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

mod app;
mod attach;
mod control;
mod exchange_store;
mod history;
mod proxy;
mod stdio;
mod ui;

use app::{
    AnnotationAuthor, App, AppMode, EditorMode, EditorMotion, EditorOperator, EditorTarget,
    LineAnnotation, Overlay, SearchHit, TextEditor,
};
use control::{ControlAction, ControlCommand, ControlError, PendingDecision};
use history::HistoryStore;
use proxy::{ProxyServer, ProxyState};
use uuid::Uuid;

const AGENT_SKILL: &str = include_str!("../skills/jsonrpc-debugger/SKILL.md");

#[derive(Parser)]
#[command(name = "jsonrpc-debugger", version)]
#[command(about = "A JSON-RPC debugger for intercepting and inspecting requests")]
struct Cli {
    /// HTTP proxy port in driver mode
    #[arg(short, long, default_value = "8080")]
    port: u16,

    /// Target URL to proxy requests to
    #[arg(short, long)]
    target: Option<String>,

    /// Local JSON-RPC control port (defaults to the proxy port plus one)
    #[arg(long)]
    control_port: Option<u16>,

    /// Print agent instructions and exit
    #[arg(long)]
    skill: bool,

    #[command(subcommand)]
    mode: Option<TargetMode>,
}

#[derive(Debug, Subcommand)]
enum TargetMode {
    /// Call a JSON-RPC endpoint
    Call {
        /// JSON-RPC transport
        #[arg(long, value_enum, default_value = "http")]
        transport: CallTransport,

        /// JSON-RPC endpoint
        url: String,

        /// Complete JSON-RPC request or batch
        request: String,
    },

    /// Drive a spawned JSON-RPC server through a local HTTP proxy
    Stdio {
        /// Message framing used on stdin and stdout
        #[arg(long, value_enum, default_value = "json-lines")]
        framing: CliFraming,

        /// Maximum seconds to wait for a server response
        #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..))]
        request_timeout: u64,

        /// Server command and arguments
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },

    /// Transparently wrap a JSON-RPC server over matching standard streams
    Wrap {
        /// Message framing used on stdin and stdout
        #[arg(long, value_enum, default_value = "json-lines")]
        framing: CliFraming,

        /// Server command and arguments
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },

    /// Attach an interactive TUI to a transparent wrapper
    Attach {
        /// Wrapper control-plane URL
        #[arg(default_value = "http://127.0.0.1:8081")]
        control_url: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliFraming {
    JsonLines,
    ContentLength,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CallTransport {
    Http,
}

impl From<CliFraming> for app::Framing {
    fn from(framing: CliFraming) -> Self {
        match framing {
            CliFraming::JsonLines => Self::JsonLines,
            CliFraming::ContentLength => Self::ContentLength,
        }
    }
}

fn copy_to_clipboard(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    content: &str,
) -> Result<()> {
    let content = BASE64.encode(content);
    if std::env::var_os("TMUX").is_some() {
        write!(
            terminal.backend_mut(),
            "\x1bPtmux;\x1b\x1b]52;c;{}\x07\x1b\\",
            content
        )?;
    } else {
        write!(terminal.backend_mut(), "\x1b]52;c;{}\x07", content)?;
    }
    terminal.backend_mut().flush()?;

    Ok(())
}

fn copy_focused_panel(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &App,
) -> Result<()> {
    let Some(markdown) = app.focused_markdown() else {
        return Ok(());
    };

    copy_to_clipboard(terminal, &markdown)
}

fn enter_request_list(app: &mut App) -> bool {
    if app.app_mode != AppMode::Normal
        || !app.is_message_list_focused()
        || app.visual_selection_active
    {
        return false;
    }

    let visible = app.filtered_exchange_indices();
    let selected = visible
        .iter()
        .copied()
        .find(|index| *index == app.selected_exchange)
        .or_else(|| visible.first().copied());
    if let Some(selected) = selected {
        app.select_exchange(selected);
        app.set_focus(app::Focus::ResponseSection);
    }

    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorAction {
    None,
    Save,
    Cancel,
}

struct ChangeWaiter {
    after_revision: u64,
    deadline: Instant,
    reply: oneshot::Sender<control::ControlResult>,
}

struct Runtime {
    history: HistoryStore,
    message_sender: mpsc::UnboundedSender<app::JsonRpcMessage>,
    shared_app_mode: Arc<Mutex<AppMode>>,
    pending_receiver: mpsc::UnboundedReceiver<app::PendingRequest>,
    control_receiver: mpsc::UnboundedReceiver<ControlCommand>,
    proxy_state: ProxyState,
    proxy_server: Option<JoinHandle<()>>,
    control_server: Option<JoinHandle<Result<(), String>>>,
    request_result_sender: mpsc::UnboundedSender<Result<(), String>>,
    request_result_receiver: mpsc::UnboundedReceiver<Result<(), String>>,
    change_waiters: Vec<ChangeWaiter>,
}

fn handle_editor_key(editor: &mut TextEditor, key: KeyEvent) -> EditorAction {
    editor.error = None;

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
        return EditorAction::Save;
    }

    match editor.mode {
        EditorMode::Insert => match key.code {
            KeyCode::Esc => editor.finish_insert(),
            KeyCode::Left => editor.move_left(),
            KeyCode::Right => editor.move_right(),
            KeyCode::Up => editor.move_up(),
            KeyCode::Down => editor.move_down(),
            KeyCode::Home => editor.move_to_start(),
            KeyCode::End => editor.move_to_end(),
            KeyCode::Enter => editor.newline(),
            KeyCode::Backspace => editor.backspace(),
            KeyCode::Delete => editor.delete(),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                editor.insert(character)
            }
            _ => {}
        },
        EditorMode::Normal => return handle_normal_editor_key(editor, key),
        EditorMode::Command => match key.code {
            KeyCode::Esc => {
                editor.mode = EditorMode::Normal;
                editor.command.clear();
            }
            KeyCode::Backspace => {
                if editor.command.pop().is_none() {
                    editor.mode = EditorMode::Normal;
                }
            }
            KeyCode::Enter => match editor.command.as_str() {
                "w" | "wq" | "x" => return EditorAction::Save,
                "q" | "q!" => return EditorAction::Cancel,
                _ => {
                    editor.error = Some(format!("Unknown command: :{}", editor.command));
                    editor.mode = EditorMode::Normal;
                    editor.command.clear();
                }
            },
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                editor.command.push(character)
            }
            _ => {}
        },
    }

    EditorAction::None
}

fn handle_normal_editor_key(editor: &mut TextEditor, key: KeyEvent) -> EditorAction {
    if key.code == KeyCode::Esc {
        editor.clear_pending();
        return EditorAction::None;
    }
    if let Some(operator) = editor.pending_operator {
        return handle_editor_operator(editor, operator, key.code);
    }
    if editor.pending_g {
        editor.pending_g = false;
        if key.code == KeyCode::Char('g') {
            editor.move_to_top();
        }
        return EditorAction::None;
    }
    if let Some(motion) = editor_motion(key.code) {
        editor.move_with(motion);
        return EditorAction::None;
    }

    match key.code {
        KeyCode::Char('q') => return EditorAction::Cancel,
        KeyCode::Char(':') => {
            editor.mode = EditorMode::Command;
            editor.command.clear();
        }
        KeyCode::Char('i') => editor.start_insert(),
        KeyCode::Char('a') => {
            editor.move_right();
            editor.start_insert();
        }
        KeyCode::Char('I') => {
            editor.move_to_first_non_blank();
            editor.start_insert();
        }
        KeyCode::Char('A') => {
            editor.move_to_end();
            editor.start_insert();
        }
        KeyCode::Char('o') => editor.open_line_below(),
        KeyCode::Char('O') => editor.open_line_above(),
        KeyCode::Char('k') | KeyCode::Up => editor.move_up(),
        KeyCode::Char('j') | KeyCode::Down => editor.move_down(),
        KeyCode::Char('g') => editor.pending_g = true,
        KeyCode::Char('G') => editor.move_to_bottom(),
        KeyCode::Char('d') => editor.pending_operator = Some(EditorOperator::Delete),
        KeyCode::Char('c') => editor.pending_operator = Some(EditorOperator::Change),
        KeyCode::Char('y') => editor.pending_operator = Some(EditorOperator::Yank),
        KeyCode::Char('D') => editor.apply_operator(EditorOperator::Delete, EditorMotion::LineEnd),
        KeyCode::Char('C') => editor.apply_operator(EditorOperator::Change, EditorMotion::LineEnd),
        KeyCode::Char('s') => editor.apply_operator(EditorOperator::Change, EditorMotion::Right),
        KeyCode::Char('S') => editor.apply_line_operator(EditorOperator::Change),
        KeyCode::Char('x') | KeyCode::Delete => editor.delete_character(),
        KeyCode::Char('X') => editor.delete_previous_character(),
        KeyCode::Char('p') => editor.paste(true),
        KeyCode::Char('P') => editor.paste(false),
        KeyCode::Char('u') => editor.undo(),
        _ => {}
    }

    EditorAction::None
}

fn handle_editor_operator(
    editor: &mut TextEditor,
    operator: EditorOperator,
    key: KeyCode,
) -> EditorAction {
    if key == KeyCode::Char(operator.key()) {
        editor.apply_line_operator(operator);
        return EditorAction::None;
    }
    let Some(motion) = editor_motion(key) else {
        editor.clear_pending();
        return EditorAction::None;
    };

    editor.apply_operator(operator, motion);
    EditorAction::None
}

fn editor_motion(key: KeyCode) -> Option<EditorMotion> {
    match key {
        KeyCode::Char('h') | KeyCode::Left => Some(EditorMotion::Left),
        KeyCode::Char('l') | KeyCode::Right => Some(EditorMotion::Right),
        KeyCode::Char('w') => Some(EditorMotion::WordForward),
        KeyCode::Char('b') => Some(EditorMotion::WordBackward),
        KeyCode::Char('e') => Some(EditorMotion::WordEnd),
        KeyCode::Char('0') | KeyCode::Home => Some(EditorMotion::LineStart),
        KeyCode::Char('$') | KeyCode::End => Some(EditorMotion::LineEnd),
        _ => None,
    }
}

fn save_editor(app: &mut App, request_result_sender: &mpsc::UnboundedSender<Result<(), String>>) {
    let Some(mut editor) = app.editor.take() else {
        return;
    };
    let content = editor.content();

    let result = match editor.target {
        EditorTarget::PendingRequest => app
            .apply_edited_json(content)
            .map(|_| "Request updated".to_string()),
        EditorTarget::PendingHeaders => app
            .apply_edited_headers(content)
            .map(|_| "Headers updated".to_string()),
        EditorTarget::PendingResponse => app
            .complete_selected_request(content)
            .map(|_| "Request completed".to_string()),
        EditorTarget::NewRequest => match app.prepare_new_request(content) {
            Ok(request) => {
                let sender = request_result_sender.clone();
                tokio::spawn(async move {
                    let _ = sender.send(app::send_new_request(request).await.map(|_| ()));
                });
                app.set_notice("Sending request…");
                return;
            }
            Err(error) => Err(error),
        },
    };

    match result {
        Ok(notice) => app.set_notice(notice),
        Err(error) => {
            editor.error = Some(error);
            app.editor = Some(editor);
        }
    }
}

struct ControlContext<'a> {
    terminal_area: ratatui::layout::Rect,
    proxy_server: &'a mut Option<JoinHandle<()>>,
    message_sender: &'a mpsc::UnboundedSender<app::JsonRpcMessage>,
    proxy_state: &'a ProxyState,
    request_result_sender: &'a mpsc::UnboundedSender<Result<(), String>>,
    history: &'a mut HistoryStore,
}

async fn handle_control_command(
    app: &mut App,
    command: ControlCommand,
    context: ControlContext<'_>,
) {
    let ControlContext {
        terminal_area,
        proxy_server,
        message_sender,
        proxy_state,
        request_result_sender,
        history,
    } = context;
    let ControlCommand { action, reply } = command;
    let result = match action {
        ControlAction::Discover => Ok(control::discovery(app.control_port)),
        ControlAction::GetState {
            include_annotations,
        } => Ok(control::state_with_annotations(app, include_annotations)),
        ControlAction::GetUpdates {
            session_id,
            next_index,
            pending_indices,
            annotation_revision,
        } => match annotation_revision {
            Some(revision) => control::updates_since(
                app,
                session_id.as_deref(),
                next_index,
                pending_indices,
                Some(revision),
            ),
            None => control::updates(app, session_id.as_deref(), next_index, pending_indices),
        },
        ControlAction::WaitForChange { .. } => {
            unreachable!("wait commands are registered by run_app")
        }
        ControlAction::GetPanel {
            focus,
            exchange_index,
            tab,
        } => {
            if app.app_mode != AppMode::Normal {
                Err(ControlError::invalid_params(
                    "line references require normal mode",
                ))
            } else {
                detail_lines_at(app, focus, exchange_index, tab)
                    .map(|lines| control::panel(focus, lines))
            }
        }
        ControlAction::GetHistory {
            limit,
            session_id,
            before,
        } => active_session_id(app)
            .map(|active| session_id.as_deref().unwrap_or(active))
            .ok_or_else(|| ControlError::runtime("No active session"))
            .and_then(|session_id| {
                history
                    .history(session_id, limit, before)
                    .map(control::stored_history)
                    .map_err(|error| ControlError::runtime(error.to_string()))
            }),
        ControlAction::Find {
            query,
            limit,
            session_id,
        } => history
            .find(&query, limit, session_id.as_deref())
            .map(control::find_results)
            .map_err(|error| ControlError::runtime(error.to_string())),
        ControlAction::ListSessions { limit } => history
            .list_sessions(limit)
            .map(control::sessions)
            .map_err(|error| ControlError::runtime(error.to_string())),
        ControlAction::CreateSession { name } => {
            create_session(app, history, name.as_deref()).map(|_| control::state(app))
        }
        ControlAction::SelectSession { id } => match select_session(app, history, &id) {
            Ok(target_changed) => {
                if target_changed && app.is_running {
                    restart_proxy(app, proxy_server, message_sender, proxy_state).await;
                }
                Ok(control::state(app))
            }
            Err(error) => Err(error),
        },
        ControlAction::RenameSession { id, name } => {
            rename_session(app, history, &id, &name).map(|_| control::state(app))
        }
        ControlAction::ExportSession => active_session_id(app)
            .ok_or_else(|| ControlError::runtime("No active session"))
            .and_then(|session_id| {
                history
                    .export_session(session_id)
                    .and_then(|session| serde_json::to_value(session).map_err(Into::into))
                    .map_err(|error| ControlError::runtime(error.to_string()))
            }),
        ControlAction::ReplaySession { session } => match control::replay_session(session) {
            Ok(exchanges) => {
                let imported = exchanges.len();
                active_session_id(app)
                    .ok_or_else(|| ControlError::runtime("No active session"))
                    .and_then(|session_id| {
                        history
                            .append_exchanges(session_id, &exchanges)
                            .map_err(|error| ControlError::runtime(error.to_string()))
                    })
                    .map(|_| {
                        app.append_exchanges(exchanges);
                        serde_json::json!({
                            "imported": imported,
                            "state": control::state(app),
                        })
                    })
            }
            Err(error) => Err(error),
        },
        ControlAction::GetPending => Ok(control::pending(app)),
        ControlAction::SendRequest { request } => {
            let request = app
                .prepare_new_request(request.to_string())
                .map(|mut request| {
                    request.url = format!("http://127.0.0.1:{}", app.proxy_config.listen_port);
                    request
                });
            match request {
                Ok(request) => {
                    let notice = request_result_sender.clone();
                    tokio::spawn(async move {
                        let result = app::send_new_request(request).await;
                        let _ = notice.send(result.as_ref().map(|_| ()).map_err(Clone::clone));
                        let _ = reply.send(result.map_err(ControlError::runtime));
                    });
                    return;
                }
                Err(error) => Err(ControlError::invalid_params(error)),
            }
        }
        ControlAction::SelectExchange { index } => {
            if index >= app.exchanges().len() {
                Err(ControlError::invalid_params(format!(
                    "Exchange index {index} does not exist"
                )))
            } else {
                app.select_exchange(index);
                Ok(control::state(app))
            }
        }
        ControlAction::SetFocus { focus } => {
            app.set_focus(focus);
            Ok(control::state(app))
        }
        ControlAction::SetFullscreen { fullscreen } => {
            app.set_panel_fullscreen(fullscreen);
            Ok(control::state(app))
        }
        ControlAction::RevealLines {
            focus,
            start_line,
            end_line,
            session_id,
            exchange_index,
            tab,
        } => {
            if app.app_mode != AppMode::Normal {
                Err(ControlError::invalid_params(
                    "line references require normal mode",
                ))
            } else {
                let session_changed = session_id
                    .as_deref()
                    .is_some_and(|id| active_session_id(app) != Some(id));
                let session_result = if session_changed {
                    match select_session(app, history, session_id.as_deref().unwrap()) {
                        Ok(target_changed) => {
                            if target_changed && app.is_running {
                                restart_proxy(app, proxy_server, message_sender, proxy_state).await;
                            }
                            Ok(())
                        }
                        Err(error) => Err(error),
                    }
                } else {
                    Ok(())
                };

                session_result.and_then(|_| {
                    reveal_lines_at(
                        app,
                        terminal_area,
                        focus,
                        exchange_index,
                        tab,
                        start_line,
                        end_line,
                    )
                    .map(|_| control::state(app))
                })
            }
        }
        ControlAction::AnnotateLines {
            author,
            focus,
            exchange_index,
            tab,
            start_line,
            end_line,
            message,
        } => match build_annotation(
            app,
            focus,
            exchange_index,
            tab,
            start_line,
            end_line,
            &message,
        ) {
            Ok(mut annotation) => {
                annotation.author = author;
                let value = control::annotation(&annotation);
                match persist_annotation(app, history, annotation) {
                    Ok(()) => Ok(serde_json::json!({
                        "annotation": value,
                        "state": control::state(app),
                    })),
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        },
        ControlAction::GetAnnotations => Ok(control::annotations(app)),
        ControlAction::ReplyAnnotation {
            id,
            message,
            author,
        } => build_reply(app, &id, &message, author).and_then(|annotation| {
            let value = control::annotation(&annotation);
            persist_annotation(app, history, annotation)?;
            Ok(serde_json::json!({"annotation": value, "state": control::state(app)}))
        }),
        ControlAction::UpdateAnnotation { id, message } => {
            update_annotation(app, history, &id, &message).map(|_| control::state(app))
        }
        ControlAction::ClearLineSelection => {
            app.clear_line_selection();
            Ok(control::state(app))
        }
        ControlAction::RemoveAnnotation { id } => {
            remove_annotation(app, history, &id).map(|_| control::state(app))
        }
        ControlAction::ScrollPanel { focus, lines } => {
            if focus == app::Focus::MessageList {
                let visible_rows = ui::panel_visible_lines(terminal_area, app, focus);
                scroll_history(app, lines, visible_rows);
                Ok(control::state(app))
            } else if app.app_mode != AppMode::Normal {
                Err(ControlError::invalid_params(
                    "request and response scrolling requires normal mode",
                ))
            } else {
                let total_lines = ui::detail_line_count(app, focus).unwrap_or(0)
                    + app.visible_annotations(focus).count() * ui::ANNOTATION_CARD_HEIGHT;
                app.scroll_panel_lines(focus, lines, total_lines);
                Ok(control::state(app))
            }
        }
        ControlAction::SetTarget { url } => {
            let url = url.trim();
            if app.proxy_config.stdio.is_some() {
                Err(ControlError::invalid_params(
                    "stdio commands are configured at startup",
                ))
            } else if url.is_empty() {
                Err(ControlError::invalid_params("url cannot be empty"))
            } else {
                let changed = app.proxy_config.target_url != url;
                if changed {
                    if let Some(session_id) = active_session_id(app) {
                        if let Err(error) = history.update_target(session_id, url) {
                            return send_control_reply(
                                reply,
                                Err(ControlError::runtime(error.to_string())),
                            );
                        }
                    }
                    app.proxy_config.target_url = url.to_string();
                    if let Some(session) = &mut app.session {
                        session.target = url.to_string();
                    }
                    app.mark_changed();
                }
                if changed && app.is_running {
                    restart_proxy(app, proxy_server, message_sender, proxy_state).await;
                }
                Ok(control::state(app))
            }
        }
        ControlAction::SetFilter { text } => {
            if app.filter_text != text {
                app.filter_text = text;
                app.mark_changed();
            }
            Ok(control::state(app))
        }
        ControlAction::SetPaused { paused } => {
            if paused {
                app.clear_line_selection();
            }
            let mode = if paused {
                AppMode::Paused
            } else if app.pending_requests.is_empty() {
                AppMode::Normal
            } else {
                AppMode::Intercepting
            };
            if app.app_mode != mode {
                app.app_mode = mode;
                app.mark_changed();
            }
            if let Ok(mut shared_mode) = proxy_state.app_mode.lock() {
                *shared_mode = app.app_mode;
            }
            Ok(control::state(app))
        }
        ControlAction::ResolvePending { id, decision } => resolve_pending(app, id, decision),
    };

    let _ = reply.send(result);
}

fn center_detail_range(
    app: &mut App,
    terminal_area: ratatui::layout::Rect,
    panel: app::Focus,
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    annotation_id: Option<&str>,
) {
    let visible_lines = ui::panel_visible_lines(terminal_area, app, panel).max(1);
    let annotations = app
        .visible_annotations(panel)
        .filter(|annotation| annotation.end_line <= total_lines)
        .collect::<Vec<_>>();
    let annotations_before = |line: usize| {
        annotations
            .iter()
            .filter(|annotation| annotation.end_line < line)
            .count()
            * ui::ANNOTATION_CARD_HEIGHT
    };
    let range_start = start_line.saturating_sub(1) + annotations_before(start_line);
    let source_range_end = end_line.saturating_sub(1) + annotations_before(end_line);
    let range_end = annotation_id
        .and_then(|id| {
            annotations
                .iter()
                .filter(|annotation| annotation.end_line == end_line)
                .position(|annotation| annotation.id == id)
                .map(|position| {
                    end_line
                        + annotations_before(end_line)
                        + position * ui::ANNOTATION_CARD_HEIGHT
                        + ui::ANNOTATION_CARD_HEIGHT / 2
                })
        })
        .unwrap_or(source_range_end);
    let display_total = total_lines + annotations.len() * ui::ANNOTATION_CARD_HEIGHT;
    let range_center = if annotation_id.is_some() {
        range_end
    } else {
        range_start + range_end.saturating_sub(range_start) / 2
    };
    let viewport_center = visible_lines.saturating_sub(1) / 2;
    let display_scroll = range_center
        .saturating_sub(viewport_center)
        .min(display_total.saturating_sub(visible_lines));
    let scroll = match panel {
        app::Focus::RequestSection => &mut app.request_details_scroll,
        app::Focus::ResponseSection => &mut app.response_details_scroll,
        app::Focus::MessageList | app::Focus::StatusHeader => return,
    };
    if *scroll == display_scroll {
        return;
    }
    *scroll = display_scroll;
    app.mark_changed();
}

fn send_control_reply(
    reply: oneshot::Sender<control::ControlResult>,
    result: control::ControlResult,
) {
    let _ = reply.send(result);
}

fn active_session_id(app: &App) -> Option<&str> {
    app.session.as_ref().map(|session| session.id.as_str())
}

fn build_annotation(
    app: &App,
    panel: app::Focus,
    exchange_index: Option<usize>,
    tab: Option<app::DetailTab>,
    start_line: usize,
    end_line: usize,
    message: &str,
) -> Result<LineAnnotation, ControlError> {
    if app.app_mode != AppMode::Normal {
        return Err(ControlError::invalid_params(
            "line annotations require normal mode",
        ));
    }
    let message = annotation_message(message)?;

    let exchange_index = exchange_index.unwrap_or(app.selected_exchange);
    let tab = tab
        .or_else(|| app.detail_tab(panel))
        .ok_or_else(|| ControlError::invalid_params("annotation panel must show details"))?;
    let lines = detail_lines_at(app, panel, Some(exchange_index), Some(tab))?;
    let total_lines = lines.len();
    let text = (start_line > 0 && end_line >= start_line && end_line <= total_lines)
        .then(|| lines[start_line - 1..end_line].to_vec())
        .ok_or_else(|| {
            ControlError::invalid_params(format!("line range must be within 1..={total_lines}"))
        })?;

    Ok(LineAnnotation {
        parent_id: None,
        author: AnnotationAuthor::Agent,
        created_at_ms: annotation_timestamp_ms(),
        id: Uuid::new_v4().to_string(),
        exchange_index,
        panel,
        tab,
        start_line,
        end_line,
        message: message.to_string(),
        text,
    })
}

fn detail_lines_at(
    app: &App,
    panel: app::Focus,
    exchange_index: Option<usize>,
    tab: Option<app::DetailTab>,
) -> Result<Vec<String>, ControlError> {
    let exchange_index = exchange_index.unwrap_or(app.selected_exchange);
    if exchange_index >= app.exchanges().len() {
        return Err(ControlError::invalid_params(format!(
            "Exchange index {exchange_index} does not exist"
        )));
    }
    let tab = tab
        .or_else(|| app.detail_tab(panel))
        .ok_or_else(|| ControlError::invalid_params("panel must be request or response"))?;
    ui::detail_lines_text_at(app, panel, exchange_index, tab)
        .map_err(|error| ControlError::runtime(format!("{error:#}")))?
        .ok_or_else(|| ControlError::invalid_params("panel must be request or response"))
}

fn reveal_lines_at(
    app: &mut App,
    terminal_area: ratatui::layout::Rect,
    panel: app::Focus,
    exchange_index: Option<usize>,
    tab: Option<app::DetailTab>,
    start_line: usize,
    end_line: usize,
) -> Result<(), ControlError> {
    let exchange_index = exchange_index.unwrap_or(app.selected_exchange);
    let tab = tab
        .or_else(|| app.detail_tab(panel))
        .ok_or_else(|| ControlError::invalid_params("panel must be request or response"))?;
    let lines = detail_lines_at(app, panel, Some(exchange_index), Some(tab))?;
    let total_lines = lines.len();
    let text = (start_line > 0 && end_line >= start_line && end_line <= total_lines)
        .then(|| lines[start_line - 1..end_line].to_vec())
        .ok_or_else(|| {
            ControlError::invalid_params(format!("line range must be within 1..={total_lines}"))
        })?;

    app.select_exchange(exchange_index);
    match panel {
        app::Focus::RequestSection => app.request_tab = usize::from(tab == app::DetailTab::Body),
        app::Focus::ResponseSection => app.response_tab = usize::from(tab == app::DetailTab::Body),
        app::Focus::MessageList | app::Focus::StatusHeader => {
            return Err(ControlError::invalid_params(
                "panel must be request or response",
            ));
        }
    }
    app.reveal_lines(panel, start_line, end_line, text);
    center_detail_range(
        app,
        terminal_area,
        panel,
        start_line,
        end_line,
        total_lines,
        None,
    );
    Ok(())
}

fn search_hits(app: &App, query: &str) -> Result<Vec<SearchHit>> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Ok(Vec::new());
    }

    let mut hits = Vec::new();
    for (exchange_index, exchange) in app.exchanges().iter().enumerate() {
        let exchange = exchange?;
        for panel in [app::Focus::RequestSection, app::Focus::ResponseSection] {
            for tab in [app::DetailTab::Headers, app::DetailTab::Body] {
                let Some(lines) = ui::detail_lines_text_for(&exchange, panel, tab) else {
                    continue;
                };
                hits.extend(lines.iter().enumerate().filter_map(|(index, line)| {
                    line.to_lowercase().contains(&query).then_some(SearchHit {
                        exchange_index,
                        panel,
                        tab,
                        start_line: index + 1,
                        end_line: index + 1,
                    })
                }));
            }
        }
    }
    hits.extend(app.annotations().iter().filter_map(|annotation| {
        annotation
            .message
            .to_lowercase()
            .contains(&query)
            .then_some(SearchHit {
                exchange_index: annotation.exchange_index,
                panel: annotation.panel,
                tab: annotation.tab,
                start_line: annotation.start_line,
                end_line: annotation.end_line,
            })
    }));
    hits.sort_by_key(search_hit_order);
    hits.dedup();
    Ok(hits)
}

fn search_hit_order(hit: &SearchHit) -> (usize, usize, usize, usize, usize) {
    let panel = usize::from(hit.panel == app::Focus::ResponseSection);
    let tab = usize::from(hit.tab == app::DetailTab::Body);
    (hit.exchange_index, panel, tab, hit.start_line, hit.end_line)
}

fn apply_search(app: &mut App, area: ratatui::layout::Rect) {
    let query = app.input_buffer.trim().to_string();
    let hits = match search_hits(app, &query) {
        Ok(hits) => hits,
        Err(error) => {
            app.set_notice(format!("Error: search history: {error:#}"));
            return;
        }
    };
    let first = app.set_search_results(query.clone(), hits);
    if let Some(hit) = first {
        reveal_search_hit(app, area, hit);
    } else if !query.is_empty() {
        app.set_notice(format!("No matches for {query:?}"));
    }
}

fn navigate_search(app: &mut App, area: ratatui::layout::Rect, next: bool) -> bool {
    if !app.search_active() {
        return false;
    }
    let hit = if next {
        app.next_search_hit()
    } else {
        app.previous_search_hit()
    };
    if let Some(hit) = hit {
        reveal_search_hit(app, area, hit);
    }
    true
}

fn reveal_search_hit(app: &mut App, area: ratatui::layout::Rect, hit: SearchHit) {
    if let Err(error) = reveal_lines_at(
        app,
        area,
        hit.panel,
        Some(hit.exchange_index),
        Some(hit.tab),
        hit.start_line,
        hit.end_line,
    ) {
        app.set_notice(format!("Error: {}", error.message));
    }
}

fn annotate_visual_selection(
    app: &mut App,
    history: &HistoryStore,
    _terminal_area: ratatui::layout::Rect,
    message: &str,
) -> Result<(), ControlError> {
    let selection = app
        .line_selection
        .as_ref()
        .filter(|_| app.visual_selection_active)
        .cloned()
        .ok_or_else(|| ControlError::invalid_params("No visual selection"))?;
    let mut annotation = build_annotation(
        app,
        selection.panel,
        Some(app.selected_exchange),
        app.detail_tab(selection.panel),
        selection.start_line,
        selection.end_line,
        message,
    )?;
    annotation.author = AnnotationAuthor::User;
    persist_annotation(app, history, annotation)?;
    app.visual_selection_active = false;
    app.mark_changed();
    Ok(())
}

fn save_annotation_draft(
    app: &mut App,
    history: &HistoryStore,
    area: ratatui::layout::Rect,
) -> Result<(), ControlError> {
    let message = app.input_buffer.clone();
    let target = if let Some(id) = app.annotation_edit_id.clone() {
        update_annotation(app, history, &id, &message)?;
        Some(id)
    } else if let Some(parent) = app.annotation_reply_id.clone() {
        let note = build_reply(app, &parent, &message, AnnotationAuthor::User)?;
        let id = note.id.clone();
        persist_annotation(app, history, note)?;
        Some(id)
    } else {
        annotate_visual_selection(app, history, area, &message)?;
        None
    };
    app.cancel_editing();
    if let Some(id) = target {
        app.focus_annotation(&id);
        center_active_annotation(app, area);
    }
    app.set_notice("Annotation saved");
    Ok(())
}

fn annotation_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn build_reply(
    app: &App,
    id: &str,
    message: &str,
    author: AnnotationAuthor,
) -> Result<LineAnnotation, ControlError> {
    let message = annotation_message(message)?;
    if app.app_mode != AppMode::Normal {
        return Err(ControlError::invalid_params(
            "line annotations require normal mode",
        ));
    }
    let parent = app
        .annotations()
        .iter()
        .find(|note| note.id == id)
        .ok_or_else(|| ControlError::invalid_params(format!("Annotation not found: {id}")))?;
    Ok(LineAnnotation {
        id: Uuid::new_v4().to_string(),
        parent_id: Some(parent.id.clone()),
        author,
        created_at_ms: annotation_timestamp_ms(),
        message: message.to_string(),
        ..parent.clone()
    })
}

fn persist_annotation(
    app: &mut App,
    history: &HistoryStore,
    annotation: LineAnnotation,
) -> Result<(), ControlError> {
    let session_id = active_session_id(app)
        .ok_or_else(|| ControlError::runtime("No active session"))?
        .to_string();
    history
        .add_annotation(&session_id, &annotation)
        .map_err(|error| ControlError::runtime(error.to_string()))?;
    app.add_annotation(annotation);
    Ok(())
}

fn update_annotation(
    app: &mut App,
    history: &HistoryStore,
    annotation_id: &str,
    message: &str,
) -> Result<(), ControlError> {
    let message = annotation_message(message)?;
    if !app
        .annotations()
        .iter()
        .any(|annotation| annotation.id == annotation_id)
    {
        return Err(ControlError::invalid_params(format!(
            "Annotation not found: {annotation_id}"
        )));
    }
    let session_id = active_session_id(app)
        .ok_or_else(|| ControlError::runtime("No active session"))?
        .to_string();
    let updated = history
        .update_annotation(&session_id, annotation_id, message)
        .map_err(|error| ControlError::runtime(error.to_string()))?;
    if !updated {
        return Err(ControlError::invalid_params(format!(
            "Annotation not found: {annotation_id}"
        )));
    }
    app.update_annotation(annotation_id, message.to_string());
    Ok(())
}

fn annotation_message(message: &str) -> Result<&str, ControlError> {
    let message = message.trim();
    if message.is_empty() || message.chars().count() > 160 || message.chars().any(char::is_control)
    {
        return Err(ControlError::invalid_params(
            "message must be one line containing 1 to 160 characters",
        ));
    }
    Ok(message)
}

fn remove_annotation(
    app: &mut App,
    history: &HistoryStore,
    annotation_id: &str,
) -> Result<(), ControlError> {
    let session_id = active_session_id(app)
        .ok_or_else(|| ControlError::runtime("No active session"))?
        .to_string();
    let removed = history
        .remove_annotation(&session_id, annotation_id)
        .map_err(|error| ControlError::runtime(error.to_string()))?;
    if !removed {
        return Err(ControlError::invalid_params(format!(
            "Annotation not found: {annotation_id}"
        )));
    }
    app.remove_annotation(annotation_id);
    Ok(())
}

fn rename_session(
    app: &mut App,
    history: &HistoryStore,
    session_id: &str,
    name: &str,
) -> Result<(), ControlError> {
    let name = name.trim();
    let renamed = history
        .rename_session(session_id, name)
        .map_err(|error| ControlError::invalid_params(error.to_string()))?;
    if !renamed {
        return Err(ControlError::invalid_params(format!(
            "Session not found: {session_id}"
        )));
    }
    app.rename_session(session_id, name.to_string());
    Ok(())
}

fn create_session(
    app: &mut App,
    history: &mut HistoryStore,
    name: Option<&str>,
) -> Result<(), ControlError> {
    if !app.pending_requests.is_empty() {
        return Err(ControlError::runtime(
            "Resolve pending requests before changing sessions",
        ));
    }
    let session = history
        .create_session(name, &app.proxy_config.target_url)
        .map_err(|error| ControlError::runtime(error.to_string()))?;
    app.activate_session(session, Vec::new(), Vec::new());
    Ok(())
}

fn select_session(app: &mut App, history: &HistoryStore, id: &str) -> Result<bool, ControlError> {
    if !app.pending_requests.is_empty() {
        return Err(ControlError::runtime(
            "Resolve pending requests before changing sessions",
        ));
    }
    let (session, exchanges, annotations) = history
        .load_cached_session(id)
        .map_err(|error| ControlError::runtime(error.to_string()))?;
    if app.proxy_config.stdio.is_some() && session.target != app.proxy_config.target_url {
        return Err(ControlError::invalid_params(
            "Cannot switch a stdio process to a session from another target",
        ));
    }
    let target_changed = app.proxy_config.target_url != session.target;
    app.proxy_config.target_url = session.target.clone();
    app.activate_cached_session(session, exchanges, annotations);
    Ok(target_changed)
}

fn scroll_history(app: &mut App, lines: i64, visible_rows: usize) {
    app.set_focus(app::Focus::MessageList);
    match app.app_mode {
        AppMode::Normal => app.scroll_history(lines, visible_rows),
        AppMode::Paused | AppMode::Intercepting => {
            if app.pending_requests.is_empty() {
                return;
            }
            app.selected_pending =
                offset_index(app.selected_pending, lines, app.pending_requests.len());
            app.reset_intercept_details_scroll();
        }
    }
}

fn offset_index(index: usize, offset: i64, length: usize) -> usize {
    let distance = usize::try_from(offset.unsigned_abs()).unwrap_or(usize::MAX);
    if offset >= 0 {
        index.saturating_add(distance).min(length.saturating_sub(1))
    } else {
        index.saturating_sub(distance)
    }
}

fn resolve_pending(app: &mut App, id: String, decision: PendingDecision) -> control::ControlResult {
    let Some(index) = app
        .pending_requests
        .iter()
        .position(|pending| pending.id == id)
    else {
        return Err(ControlError::invalid_params(format!(
            "Pending request not found: {id}"
        )));
    };

    app.selected_pending = index;
    let result = match decision {
        PendingDecision::Allow { request, headers } => {
            allow_pending_request(app, index, request, headers)
        }
        PendingDecision::Block => {
            app.block_selected_request();
            Ok(())
        }
        PendingDecision::Complete { response } => {
            app.complete_selected_request(response.to_string())
        }
    };
    result.map_err(ControlError::invalid_params)?;

    if app.pending_requests.is_empty() && app.app_mode == AppMode::Intercepting {
        app.app_mode = AppMode::Normal;
    }

    Ok(serde_json::json!({"resolved": id}))
}

fn allow_pending_request(
    app: &mut App,
    index: usize,
    request: Option<serde_json::Value>,
    headers: Option<std::collections::HashMap<String, String>>,
) -> Result<(), String> {
    if let Some(request) = request {
        app.apply_edited_json(request.to_string())?;
    }
    if let Some(headers) = headers {
        app.pending_requests[index].modified_headers = Some(headers);
    }

    app.allow_selected_request();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse command line arguments
    let cli = Cli::parse();
    if cli.skill {
        print!("{AGENT_SKILL}");
        return Ok(());
    }

    match &cli.mode {
        Some(TargetMode::Call {
            transport,
            url,
            request,
        }) => {
            let request: serde_json::Value = serde_json::from_str(request)
                .map_err(|error| anyhow::anyhow!("invalid request JSON: {error}"))?;
            let response: serde_json::Value = match transport {
                CallTransport::Http => {
                    reqwest::Client::new()
                        .post(url)
                        .json(&request)
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await?
                }
            };
            println!("{}", serde_json::to_string_pretty(&response)?);
            return Ok(());
        }
        Some(TargetMode::Wrap { framing, command }) => {
            if cli.target.is_some() {
                anyhow::bail!("--target cannot be used with the wrap subcommand");
            }
            let control_port = cli
                .control_port
                .or_else(|| cli.port.checked_add(1))
                .ok_or_else(|| {
                    anyhow::anyhow!("--control-port is required when --port is 65535")
                })?;
            return run_transparent_wrap(control_port, (*framing).into(), command).await;
        }
        Some(TargetMode::Attach { control_url }) => {
            if cli.target.is_some() {
                anyhow::bail!("--target cannot be used with the attach subcommand");
            }
            return run_attached_tui(control_url).await;
        }
        Some(TargetMode::Stdio { .. }) | None => {}
    }

    let control_port = cli
        .control_port
        .or_else(|| cli.port.checked_add(1))
        .ok_or_else(|| anyhow::anyhow!("--control-port is required when --port is 65535"))?;
    let (target, transport, stdio) = match cli.mode {
        Some(TargetMode::Stdio {
            framing,
            request_timeout,
            command,
        }) => {
            if cli.target.is_some() {
                anyhow::bail!("--target cannot be used with the stdio subcommand");
            }
            let framing = app::Framing::from(framing);
            (
                stdio::display_command(&command),
                app::TransportType::Stdio(framing),
                Some(app::StdioConfig {
                    command,
                    framing,
                    request_timeout: std::time::Duration::from_secs(request_timeout),
                }),
            )
        }
        None => (
            cli.target.unwrap_or_default(),
            app::TransportType::Http,
            None,
        ),
        Some(TargetMode::Call { .. } | TargetMode::Wrap { .. } | TargetMode::Attach { .. }) => {
            unreachable!()
        }
    };
    let proxy_config = app::ProxyConfig {
        listen_port: cli.port,
        target_url: target.clone(),
        transport,
        stdio,
        transparent: false,
    };

    // Create message channel for proxy communication
    let (message_sender, message_receiver) = mpsc::unbounded_channel();

    // Create pending request channel for pause/intercept functionality
    let (pending_sender, pending_receiver) = mpsc::unbounded_channel();

    // Create the local agent control plane.
    let (control_sender, control_receiver) = mpsc::unbounded_channel();

    // Create shared state for pause/intercept
    let shared_app_mode = Arc::new(Mutex::new(AppMode::Normal));
    let proxy_state = ProxyState {
        app_mode: shared_app_mode.clone(),
        pending_sender,
    };

    // Bind both ports before entering the TUI. A second debugger must not send through
    // or expose the control plane of an older process that owns the same ports.
    let initial_server = ProxyServer::from_config(&proxy_config, message_sender.clone())?
        .with_state(proxy_state.clone());
    let initial_proxy_server = initial_server.bind()?;
    let control_server = control::bind(control_port, control_sender).map_err(anyhow::Error::msg)?;

    let mut history = HistoryStore::open_default()?;
    let session = history.create_session(None, &target)?;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let initial_proxy_handle = tokio::spawn(initial_proxy_server);
    let control_handle = tokio::spawn(async move {
        control_server.await;
        Ok(())
    });

    // Create app with receiver, using CLI arguments
    let mut app = App::new_with_receiver(message_receiver);

    // Override default config with CLI arguments
    app.proxy_config = proxy_config;
    app.control_port = control_port;
    app.activate_session(session, Vec::new(), Vec::new());

    let (request_result_sender, request_result_receiver) = mpsc::unbounded_channel();
    let runtime = Runtime {
        history,
        message_sender,
        shared_app_mode,
        pending_receiver,
        control_receiver,
        proxy_state,
        proxy_server: Some(initial_proxy_handle),
        control_server: Some(control_handle),
        request_result_sender,
        request_result_receiver,
        change_waiters: Vec::new(),
    };
    let res = run_app(&mut terminal, app, runtime).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{err:?}");
    }

    Ok(())
}

async fn run_transparent_wrap(
    control_port: u16,
    framing: app::Framing,
    command: &[OsString],
) -> Result<()> {
    let target = stdio::display_command(command);
    let (message_sender, message_receiver) = mpsc::unbounded_channel();
    let (pending_sender, pending_receiver) = mpsc::unbounded_channel();
    let (control_sender, control_receiver) = mpsc::unbounded_channel();
    let shared_app_mode = Arc::new(Mutex::new(AppMode::Normal));
    let proxy_state = ProxyState {
        app_mode: shared_app_mode.clone(),
        pending_sender,
    };
    let control_server = control::bind(control_port, control_sender).map_err(anyhow::Error::msg)?;
    let control_server = tokio::spawn(async move {
        control_server.await;
        Ok(())
    });

    let mut history = HistoryStore::open_default()?;
    let session = history.create_session(None, &target)?;
    let mut app = App::new_with_receiver(message_receiver);
    app.proxy_config = app::ProxyConfig {
        listen_port: 0,
        target_url: target,
        transport: app::TransportType::Stdio(framing),
        stdio: Some(app::StdioConfig {
            command: command.to_vec(),
            framing,
            request_timeout: stdio::DEFAULT_REQUEST_TIMEOUT,
        }),
        transparent: true,
    };
    app.control_port = control_port;
    app.activate_session(session, Vec::new(), Vec::new());

    let (request_result_sender, request_result_receiver) = mpsc::unbounded_channel();
    let mut runtime = Runtime {
        history,
        message_sender: message_sender.clone(),
        shared_app_mode,
        pending_receiver,
        control_receiver,
        proxy_state,
        proxy_server: None,
        control_server: Some(control_server),
        request_result_sender,
        request_result_receiver,
        change_waiters: Vec::new(),
    };
    let relay_command = command.to_vec();
    let relay_state = runtime.proxy_state.clone();
    let mut relay = tokio::spawn(async move {
        stdio::wrap(&relay_command, framing, message_sender, relay_state).await
    });
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        record_new_messages(&mut app, &mut runtime.history);
        if let Ok(mut shared_mode) = runtime.shared_app_mode.try_lock() {
            *shared_mode = app.app_mode;
        }
        while let Ok(pending_request) = runtime.pending_receiver.try_recv() {
            app.pending_requests.push(pending_request);
            app.mark_changed();
        }
        while let Ok(command) = runtime.control_receiver.try_recv() {
            let Some(command) = register_change_waiter(&app, command, &mut runtime.change_waiters)
            else {
                continue;
            };
            handle_control_command(
                &mut app,
                command,
                ControlContext {
                    terminal_area: ratatui::layout::Rect::new(0, 0, 120, 40),
                    proxy_server: &mut runtime.proxy_server,
                    message_sender: &runtime.message_sender,
                    proxy_state: &runtime.proxy_state,
                    request_result_sender: &runtime.request_result_sender,
                    history: &mut runtime.history,
                },
            )
            .await;
        }
        resolve_change_waiters(&app, &mut runtime.change_waiters);

        if relay.is_finished() {
            let result = (&mut relay)
                .await
                .map_err(|error| anyhow::anyhow!("wrapper task failed: {error}"))?;
            record_new_messages(&mut app, &mut runtime.history);
            if let Some(control_server) = runtime.control_server.take() {
                control_server.abort();
            }
            return result.map_err(anyhow::Error::msg);
        }
        if runtime
            .control_server
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            relay.abort();
            anyhow::bail!("control plane stopped");
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
            result = &mut shutdown => {
                result?;
                relay.abort();
                let _ = relay.await;
                if let Some(control_server) = runtime.control_server.take() {
                    control_server.abort();
                }
                return Ok(());
            }
        }
    }
}

async fn run_attached_tui(control_url: &str) -> Result<()> {
    let client = attach::ControlClient::new(control_url.to_string());
    let mut app = App::new();
    let mut revision = client
        .snapshot(&app)
        .await
        .map_err(anyhow::Error::msg)?
        .apply(&mut app)
        .map_err(anyhow::Error::msg)?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_attached_app(&mut terminal, &client, &mut app, &mut revision).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

fn handle_attached_mouse(app: &mut App, mouse: MouseEvent, area: ratatui::layout::Rect) {
    if app.overlay != Overlay::None || app.input_mode != app::InputMode::Normal {
        return;
    }
    match mouse.kind {
        MouseEventKind::Moved => {
            app.set_annotation_hover(ui::annotation_hover(area, app, mouse.column, mouse.row));
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            if let Some(focus) = ui::panel_focus(area, app, mouse.column, mouse.row) {
                app.set_focus(focus);
                scroll_panel(
                    app,
                    focus,
                    mouse.kind == MouseEventKind::ScrollDown,
                    ui::panel_visible_lines(area, app, focus),
                );
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            match ui::mouse_action(area, app, mouse.column, mouse.row) {
                Some(ui::MouseAction::SelectAnnotation { id }) => {
                    app.focus_annotation(&id);
                    center_active_annotation(app, area);
                }
                Some(ui::MouseAction::AddAnnotation { panel, line }) => {
                    start_line_annotation(app, panel, line, area)
                }
                Some(ui::MouseAction::SelectLine { panel, line }) => {
                    app.finish_visual_selection();
                    let (anchor, start, end) = app.line_selection_range(
                        panel,
                        line,
                        mouse.modifiers.contains(KeyModifiers::SHIFT),
                    );
                    if let Some(text) = ui::detail_line_text(app, panel, start, end) {
                        app.select_lines_from_anchor(panel, anchor, start, end, text);
                    }
                }
                Some(ui::MouseAction::SelectExchange(index)) => app.select_exchange(index),
                Some(ui::MouseAction::Focus(focus)) => app.set_focus(focus),
                _ => {}
            }
        }
        _ => {}
    }
}

async fn run_attached_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    client: &attach::ControlClient,
    app: &mut App,
    revision: &mut u64,
) -> Result<()> {
    let mut last_refresh = Instant::now();
    let mut should_draw = true;
    let mut last_drawn_revision = app.revision();
    loop {
        if last_refresh.elapsed() >= std::time::Duration::from_millis(100) {
            match client.state().await {
                Ok(state)
                    if state.revision != *revision
                        || app.session.as_ref().map(|session| &session.id)
                            != Some(&state.session.id) =>
                {
                    match client.snapshot(app).await {
                        Ok(snapshot) => {
                            match snapshot.apply(app) {
                                Ok(next_revision) => *revision = next_revision,
                                Err(error) => app.set_notice(format!("Error: {error}")),
                            }
                            should_draw = true;
                        }
                        Err(error) => app.set_notice(format!("Error: {error}")),
                    }
                }
                Ok(_) => {}
                Err(error) => app.set_notice(format!("Error: {error}")),
            }
            last_refresh = Instant::now();
        }

        should_draw |= app.clear_expired_notice();
        should_draw |= app.revision() != last_drawn_revision;
        if should_draw {
            position_annotation_editor(app, terminal.size()?);
            terminal.draw(|frame| ui::draw(frame, app))?;
            last_drawn_revision = app.revision();
            should_draw = false;
        }
        if !event::poll(std::time::Duration::from_millis(50))? {
            continue;
        }
        let input = event::read()?;
        if matches!(input, Event::Resize(..)) {
            should_draw = true;
        }
        if let Event::Mouse(mouse) = input {
            handle_attached_mouse(app, mouse, terminal.size()?);
            should_draw = true;
            continue;
        }
        let Event::Key(key) = input else {
            continue;
        };
        should_draw = true;
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(());
        }
        if key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if app.overlay == Overlay::Prefix {
                app.close_overlay();
            } else {
                app.show_prefix();
            }
            continue;
        }
        if app.overlay == Overlay::Prefix {
            match key.code {
                KeyCode::Char('?') => app.show_help(),
                KeyCode::Char('a')
                    if app.visual_selection_active && app.line_selection.is_some() =>
                {
                    app.close_overlay();
                    app.start_annotating_selection();
                    position_annotation_editor(app, terminal.size()?);
                }
                KeyCode::Char('d') => {
                    app.close_overlay();
                    if let Some(id) = app.annotation_to_delete().map(|note| note.id.clone()) {
                        match client.remove_annotation(&id).await {
                            Ok(()) => {
                                app.remove_annotation(&id);
                                app.set_notice("Annotation deleted");
                            }
                            Err(error) => app.set_notice(format!("Error: {error}")),
                        }
                    } else {
                        app.set_notice("No annotation under cursor");
                    }
                }
                KeyCode::Char('z') => app.set_panel_fullscreen(!app.panel_fullscreen),
                KeyCode::Char('y') => copy_focused_panel(terminal, app)?,
                KeyCode::Char('p') => {
                    app.close_overlay();
                    let paused = app.app_mode != AppMode::Paused;
                    match client.set_paused(paused).await {
                        Ok(()) => {
                            app.app_mode = if paused {
                                AppMode::Paused
                            } else if app.pending_requests.is_empty() {
                                AppMode::Normal
                            } else {
                                AppMode::Intercepting
                            };
                            app.set_notice(if paused {
                                "Client requests paused"
                            } else {
                                "New client requests resumed"
                            });
                        }
                        Err(error) => app.set_notice(format!("Error: {error}")),
                    }
                }
                KeyCode::Char('r') => {
                    app.close_overlay();
                    app.toggle_request_list();
                }
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Esc => app.close_overlay(),
                _ => {}
            }
            continue;
        }
        if app.overlay == Overlay::Help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                app.close_overlay();
            }
            continue;
        }
        if app.editor.is_some() {
            let action = app
                .editor
                .as_mut()
                .map(|editor| handle_editor_key(editor, key))
                .unwrap_or(EditorAction::None);
            match action {
                EditorAction::None => {}
                EditorAction::Save => save_attached_editor(app, client).await,
                EditorAction::Cancel => app.editor = None,
            }
            continue;
        }
        if app.input_mode == app::InputMode::AnnotatingSelection {
            match key.code {
                KeyCode::Enter => match client.save_annotation(app).await {
                    Ok(id) => {
                        app.cancel_editing();
                        app.visual_selection_active = false;
                        match client
                            .snapshot(app)
                            .await
                            .and_then(|snapshot| snapshot.apply(app))
                        {
                            Ok(next_revision) => {
                                *revision = next_revision;
                                app.focus_annotation(&id);
                                center_active_annotation(app, terminal.size()?);
                            }
                            Err(error) => {
                                app.set_notice(format!("Saved, but refresh failed: {error}"))
                            }
                        }
                    }
                    Err(error) => app.set_notice(format!("Error: {error}")),
                },
                KeyCode::Esc => app.cancel_editing(),
                KeyCode::Backspace => app.handle_backspace(),
                KeyCode::Char(character) => app.handle_input_char(character),
                _ => {}
            }
            continue;
        }
        if app.input_mode == app::InputMode::Searching {
            match key.code {
                KeyCode::Enter => apply_search(app, terminal.size()?),
                KeyCode::Esc => app.cancel_search_input(),
                KeyCode::Backspace => app.handle_backspace(),
                KeyCode::Char(character) => app.handle_input_char(character),
                _ => {}
            }
            continue;
        }

        if navigate_request(app, key.code) {
            continue;
        }

        match key.code {
            KeyCode::Char('e') if app.app_mode == AppMode::Normal => {
                start_annotation_action(app, false, terminal.size()?);
            }
            KeyCode::Esc => {
                if app.search_active() {
                    app.clear_search();
                } else {
                    app.clear_line_selection();
                }
            }
            KeyCode::Enter => {
                if app.app_mode == AppMode::Normal && !enter_request_list(app) {
                    copy_focused_panel(terminal, app)?;
                }
            }
            KeyCode::Up => match app.app_mode {
                AppMode::Normal => {
                    if app.is_message_list_focused() {
                        app.select_previous();
                    } else {
                        move_focused_detail_cursor(app, -1, terminal.size()?);
                    }
                }
                AppMode::Paused | AppMode::Intercepting => app.select_previous_pending(),
            },
            KeyCode::Down => match app.app_mode {
                AppMode::Normal => {
                    if app.is_message_list_focused() {
                        app.select_next();
                    } else {
                        move_focused_detail_cursor(app, 1, terminal.size()?);
                    }
                }
                AppMode::Paused | AppMode::Intercepting => app.select_next_pending(),
            },
            KeyCode::Char('k') => match app.app_mode {
                AppMode::Normal => {
                    if app.is_message_list_focused() {
                        app.select_previous();
                    } else {
                        move_focused_detail_cursor(app, -1, terminal.size()?);
                    }
                }
                AppMode::Paused | AppMode::Intercepting => app.scroll_intercept_details_up(),
            },
            KeyCode::Char('j') => match app.app_mode {
                AppMode::Normal => {
                    if app.is_message_list_focused() {
                        app.select_next();
                    } else {
                        move_focused_detail_cursor(app, 1, terminal.size()?);
                    }
                }
                AppMode::Paused | AppMode::Intercepting => {
                    app.intercept_details_scroll += 1;
                }
            },
            KeyCode::Left | KeyCode::Char('h') if app.app_mode == AppMode::Normal => {
                if app.is_request_section_focused() {
                    app.previous_request_tab();
                } else if app.is_response_section_focused() {
                    app.previous_response_tab();
                } else if app.is_message_list_focused() {
                    app.select_previous();
                }
            }
            KeyCode::Right | KeyCode::Char('l') if app.app_mode == AppMode::Normal => {
                if app.is_request_section_focused() {
                    app.next_request_tab();
                } else if app.is_response_section_focused() {
                    app.next_response_tab();
                } else if app.is_message_list_focused() {
                    app.select_next();
                }
            }
            KeyCode::Tab if app.app_mode == AppMode::Normal => app.switch_focus(),
            KeyCode::BackTab if app.app_mode == AppMode::Normal => app.switch_focus_reverse(),
            KeyCode::Char('u') => match app.app_mode {
                AppMode::Normal => move_focused_detail_cursor(app, -10, terminal.size()?),
                AppMode::Paused | AppMode::Intercepting => app.page_up_intercept_details(),
            },
            KeyCode::Char('d') => match app.app_mode {
                AppMode::Normal => move_focused_detail_cursor(app, 10, terminal.size()?),
                AppMode::Paused | AppMode::Intercepting => app.page_down_intercept_details(),
            },
            KeyCode::Char('g') => match app.app_mode {
                AppMode::Normal => move_focused_detail_cursor(app, i64::MIN, terminal.size()?),
                AppMode::Paused | AppMode::Intercepting => app.goto_top_intercept_details(),
            },
            KeyCode::Char('G') => match app.app_mode {
                AppMode::Normal => move_focused_detail_cursor(app, i64::MAX, terminal.size()?),
                AppMode::Paused | AppMode::Intercepting => {
                    app.goto_bottom_intercept_details(1000, 20)
                }
            },
            KeyCode::Char('[') if app.app_mode == AppMode::Normal => {
                if !navigate_search(app, terminal.size()?, false) {
                    app.focus_previous_annotation();
                    center_active_annotation(app, terminal.size()?);
                }
            }
            KeyCode::Char(']') if app.app_mode == AppMode::Normal => {
                if !navigate_search(app, terminal.size()?, true) {
                    app.focus_next_annotation();
                    center_active_annotation(app, terminal.size()?);
                }
            }
            KeyCode::Char('/') => app.start_searching(),
            KeyCode::Char('v') if app.app_mode == AppMode::Normal => toggle_visual_selection(app),
            KeyCode::Char('a') if !app.pending_requests.is_empty() => {
                allow_attached_pending(app, client).await;
            }
            KeyCode::Char('e') if !app.pending_requests.is_empty() => {
                if let Some(content) = app.get_pending_request_json() {
                    app.open_editor(EditorTarget::PendingRequest, content);
                }
            }
            KeyCode::Char('c') if !app.pending_requests.is_empty() => {
                if let Some(content) = app.get_pending_response_template() {
                    app.open_editor(EditorTarget::PendingResponse, content);
                }
            }
            KeyCode::Char('b') if !app.pending_requests.is_empty() => {
                block_attached_pending(app, client).await;
            }
            KeyCode::Char('r') if !app.pending_requests.is_empty() => {
                resume_attached_pending(app, client).await;
            }
            _ => {}
        }
    }
}

async fn allow_attached_pending(app: &mut App, client: &attach::ControlClient) {
    let Some(pending) = app.get_selected_pending() else {
        return;
    };
    let id = pending.id.clone();
    let request = match pending.modified_request.as_deref() {
        Some(request) => match serde_json::from_str(request) {
            Ok(request) => Some(request),
            Err(error) => {
                app.set_notice(format!("Error: invalid request: {error}"));
                return;
            }
        },
        None => None,
    };
    match client.allow(&id, request).await {
        Ok(()) => {
            remove_attached_pending(app);
            app.set_notice("Request allowed");
        }
        Err(error) => app.set_notice(format!("Error: {error}")),
    }
}

async fn block_attached_pending(app: &mut App, client: &attach::ControlClient) {
    let Some(id) = app.get_selected_pending().map(|pending| pending.id.clone()) else {
        return;
    };
    match client.block(&id).await {
        Ok(()) => {
            remove_attached_pending(app);
            app.set_notice("Request blocked");
        }
        Err(error) => app.set_notice(format!("Error: {error}")),
    }
}

async fn resume_attached_pending(app: &mut App, client: &attach::ControlClient) {
    if let Err(error) = client.set_paused(false).await {
        app.set_notice(format!("Error: {error}"));
        return;
    }
    let pending = app
        .pending_requests
        .iter()
        .map(|pending| {
            let request = pending
                .modified_request
                .as_deref()
                .and_then(|request| serde_json::from_str(request).ok());
            (pending.id.clone(), request)
        })
        .collect::<Vec<_>>();
    for (id, request) in pending {
        if let Err(error) = client.allow(&id, request).await {
            app.set_notice(format!("Error: {error}"));
            return;
        }
    }
    app.pending_requests.clear();
    app.selected_pending = 0;
    app.app_mode = AppMode::Normal;
    app.set_notice("All client requests resumed");
}

async fn save_attached_editor(app: &mut App, client: &attach::ControlClient) {
    let Some(mut editor) = app.editor.take() else {
        return;
    };
    let content = editor.content();
    let result = match editor.target {
        EditorTarget::PendingRequest => app
            .apply_edited_json(content)
            .map(|_| "Request updated".to_string()),
        EditorTarget::PendingResponse => complete_attached_pending(app, client, &content).await,
        EditorTarget::PendingHeaders => Err("Stdio requests do not have headers".to_string()),
        EditorTarget::NewRequest => Err("Attached wrappers cannot create requests".to_string()),
    };
    match result {
        Ok(notice) => app.set_notice(notice),
        Err(error) => {
            editor.error = Some(error);
            app.editor = Some(editor);
        }
    }
}

async fn complete_attached_pending(
    app: &mut App,
    client: &attach::ControlClient,
    content: &str,
) -> Result<String, String> {
    let response =
        serde_json::from_str(content).map_err(|error| format!("Invalid JSON: {error}"))?;
    let id = app
        .get_selected_pending()
        .map(|pending| pending.id.clone())
        .ok_or_else(|| "No pending request selected".to_string())?;
    client.complete(&id, response).await?;
    remove_attached_pending(app);
    Ok("Request completed".to_string())
}

fn remove_attached_pending(app: &mut App) {
    app.pending_requests.remove(app.selected_pending);
    app.selected_pending = app
        .selected_pending
        .min(app.pending_requests.len().saturating_sub(1));
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mut app: App,
    mut runtime: Runtime,
) -> Result<()> {
    let mut should_draw = true;

    loop {
        let notice_expired = app.clear_expired_notice();
        // Check for new messages from proxy
        let received_messages = record_new_messages(&mut app, &mut runtime.history);

        // Sync app mode with shared state
        if let Ok(mut shared_mode) = runtime.shared_app_mode.try_lock() {
            *shared_mode = app.app_mode;
        }

        // Check for new pending requests
        let mut received_pending_request = false;
        while let Ok(pending_request) = runtime.pending_receiver.try_recv() {
            app.pending_requests.push(pending_request);
            app.mark_changed();
            received_pending_request = true;
        }

        let mut received_request_result = false;
        while let Ok(result) = runtime.request_result_receiver.try_recv() {
            app.set_notice(match result {
                Ok(()) if app.filter_text.is_empty() => "Request sent".to_string(),
                Ok(()) => "Request sent; filter is active".to_string(),
                Err(error) => format!("Error: {}", error),
            });
            received_request_result = true;
        }

        let mut received_control_command = false;
        while let Ok(command) = runtime.control_receiver.try_recv() {
            let Some(command) = register_change_waiter(&app, command, &mut runtime.change_waiters)
            else {
                continue;
            };
            let revision_before = app.revision();
            handle_control_command(
                &mut app,
                command,
                ControlContext {
                    terminal_area: terminal.size()?,
                    proxy_server: &mut runtime.proxy_server,
                    message_sender: &runtime.message_sender,
                    proxy_state: &runtime.proxy_state,
                    request_result_sender: &runtime.request_result_sender,
                    history: &mut runtime.history,
                },
            )
            .await;
            received_control_command |= app.revision() != revision_before;
        }
        resolve_change_waiters(&app, &mut runtime.change_waiters);

        if should_draw
            || notice_expired
            || received_messages
            || received_pending_request
            || received_request_result
            || received_control_command
        {
            position_annotation_editor(&mut app, terminal.size()?);
            terminal.draw(|f| ui::draw(f, &app))?;
            should_draw = false;
        }

        if event::poll(std::time::Duration::from_millis(50))? {
            should_draw = true;
            let input_event = event::read()?;

            if matches!(
                &input_event,
                Event::Key(key)
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
            ) {
                stop_proxy(&mut runtime.proxy_server).await;
                return Ok(());
            }

            if app.editor.is_none()
                && app.input_mode == app::InputMode::Normal
                && matches!(
                    &input_event,
                    Event::Key(key)
                        if key.code == KeyCode::Char('b')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                )
            {
                if app.overlay == Overlay::Prefix {
                    app.close_overlay();
                } else {
                    app.show_prefix();
                }
                continue;
            }

            if app.overlay != Overlay::None {
                match input_event {
                    Event::Key(key) => {
                        if handle_overlay_key(terminal, &mut app, &mut runtime, key).await? {
                            return Ok(());
                        }
                    }
                    Event::Mouse(mouse) => {
                        handle_mouse_event(
                            terminal,
                            &mut app,
                            mouse,
                            &mut runtime.proxy_server,
                            &runtime.message_sender,
                            &runtime.proxy_state,
                            &mut runtime.history,
                        )
                        .await?;
                    }
                    _ => {}
                }
                continue;
            }

            if app.editor.is_some() {
                let action = match input_event {
                    Event::Key(key) => app
                        .editor
                        .as_mut()
                        .map(|editor| handle_editor_key(editor, key))
                        .unwrap_or(EditorAction::None),
                    Event::Mouse(mouse) => {
                        if let Some(editor) = app.editor.as_mut() {
                            let move_cursor = match mouse.kind {
                                MouseEventKind::ScrollUp => {
                                    Some(TextEditor::move_up as fn(&mut TextEditor))
                                }
                                MouseEventKind::ScrollDown => {
                                    Some(TextEditor::move_down as fn(&mut TextEditor))
                                }
                                _ => None,
                            };
                            if let Some(move_cursor) = move_cursor {
                                for _ in 0..3 {
                                    move_cursor(editor);
                                }
                            }
                        }
                        EditorAction::None
                    }
                    _ => EditorAction::None,
                };
                match action {
                    EditorAction::None => {}
                    EditorAction::Save => save_editor(&mut app, &runtime.request_result_sender),
                    EditorAction::Cancel => app.editor = None,
                }
                continue;
            }

            if let Event::Mouse(mouse) = &input_event {
                handle_mouse_event(
                    terminal,
                    &mut app,
                    *mouse,
                    &mut runtime.proxy_server,
                    &runtime.message_sender,
                    &runtime.proxy_state,
                    &mut runtime.history,
                )
                .await?;
                continue;
            }

            if let Event::Key(key) = input_event {
                // Handle input modes first
                match app.input_mode {
                    app::InputMode::Searching => {
                        match key.code {
                            KeyCode::Enter => {
                                apply_search(&mut app, terminal.size()?);
                            }
                            KeyCode::Esc => {
                                app.cancel_search_input();
                            }
                            KeyCode::Backspace => {
                                app.handle_backspace();
                            }
                            KeyCode::Char(c) => {
                                app.handle_input_char(c);
                            }
                            _ => {}
                        }
                        continue;
                    }
                    app::InputMode::AnnotatingSelection => {
                        match key.code {
                            KeyCode::Enter => {
                                if let Err(error) = save_annotation_draft(
                                    &mut app,
                                    &runtime.history,
                                    terminal.size()?,
                                ) {
                                    app.set_notice(format!("Error: {}", error.message));
                                }
                            }
                            KeyCode::Esc => app.cancel_editing(),
                            KeyCode::Backspace => app.handle_backspace(),
                            KeyCode::Char(c) => app.handle_input_char(c),
                            _ => {}
                        }
                        continue;
                    }
                    app::InputMode::EditingTarget => {
                        match key.code {
                            KeyCode::Enter => {
                                let target = app.input_buffer.trim().to_string();
                                let session_id = active_session_id(&app).map(str::to_string);
                                if !target.is_empty() {
                                    if let Some(session_id) = session_id {
                                        if let Err(error) =
                                            runtime.history.update_target(&session_id, &target)
                                        {
                                            app.set_notice(format!("Error: save target: {error}"));
                                            continue;
                                        }
                                    }
                                }
                                app.confirm_target_edit();
                                if let Some(session) = &mut app.session {
                                    session.target = app.proxy_config.target_url.clone();
                                }
                                // If proxy is running, restart it with new target
                                if app.is_running {
                                    restart_proxy(
                                        &app,
                                        &mut runtime.proxy_server,
                                        &runtime.message_sender,
                                        &runtime.proxy_state,
                                    )
                                    .await;
                                }
                            }
                            KeyCode::Esc => {
                                app.cancel_editing();
                            }
                            KeyCode::Backspace => {
                                app.handle_backspace();
                            }
                            KeyCode::Char(c) => {
                                app.handle_input_char(c);
                            }
                            _ => {}
                        }
                        continue;
                    }
                    app::InputMode::NamingSession => {
                        match key.code {
                            KeyCode::Enter => {
                                let name = app.input_buffer.trim().to_string();
                                let name = (!name.is_empty()).then_some(name);
                                match create_session(
                                    &mut app,
                                    &mut runtime.history,
                                    name.as_deref(),
                                ) {
                                    Ok(()) => {
                                        app.cancel_editing();
                                        app.set_notice("Session created");
                                    }
                                    Err(error) => {
                                        app.set_notice(format!("Error: {}", error.message));
                                    }
                                }
                            }
                            KeyCode::Esc => app.cancel_editing(),
                            KeyCode::Backspace => app.handle_backspace(),
                            KeyCode::Char(c) => app.handle_input_char(c),
                            _ => {}
                        }
                        continue;
                    }
                    app::InputMode::RenamingSession => {
                        match key.code {
                            KeyCode::Enter => {
                                let session_id = active_session_id(&app).map(str::to_string);
                                let name = app.input_buffer.clone();
                                match session_id {
                                    Some(session_id) => match rename_session(
                                        &mut app,
                                        &runtime.history,
                                        &session_id,
                                        &name,
                                    ) {
                                        Ok(()) => {
                                            app.cancel_editing();
                                            app.set_notice("Session renamed");
                                        }
                                        Err(error) => {
                                            app.set_notice(format!("Error: {}", error.message));
                                        }
                                    },
                                    None => app.set_notice("Error: No active session"),
                                }
                            }
                            KeyCode::Esc => app.cancel_editing(),
                            KeyCode::Backspace => app.handle_backspace(),
                            KeyCode::Char(c) => app.handle_input_char(c),
                            _ => {}
                        }
                        continue;
                    }

                    app::InputMode::Normal => {
                        // Continue to normal key handling below
                    }
                }

                // Normal mode key handling
                if navigate_request(&mut app, key.code) {
                    continue;
                }

                match key.code {
                    KeyCode::Char('e') if app.app_mode == AppMode::Normal => {
                        start_annotation_action(&mut app, false, terminal.size()?);
                    }
                    KeyCode::Esc => {
                        if app.search_active() {
                            app.clear_search();
                        } else {
                            app.clear_line_selection();
                        }
                    }
                    KeyCode::Enter => {
                        if app.app_mode == AppMode::Normal && !enter_request_list(&mut app) {
                            copy_focused_panel(terminal, &app)?;
                        }
                    }
                    KeyCode::Up => match app.app_mode {
                        app::AppMode::Normal => {
                            if app.is_message_list_focused() {
                                app.select_previous();
                            } else {
                                move_focused_detail_cursor(&mut app, -1, terminal.size()?);
                            }
                        }
                        app::AppMode::Paused | app::AppMode::Intercepting => {
                            app.select_previous_pending()
                        }
                    },
                    KeyCode::Down => match app.app_mode {
                        app::AppMode::Normal => {
                            if app.is_message_list_focused() {
                                app.select_next();
                            } else {
                                move_focused_detail_cursor(&mut app, 1, terminal.size()?);
                            }
                        }
                        app::AppMode::Paused | app::AppMode::Intercepting => {
                            app.select_next_pending()
                        }
                    },
                    KeyCode::Left => {
                        if app.is_status_focused() {
                            let desired_running = !app.is_running;
                            set_proxy_running(
                                &mut app,
                                desired_running,
                                &mut runtime.proxy_server,
                                &runtime.message_sender,
                                &runtime.proxy_state,
                            )
                            .await;
                        } else if app.app_mode == app::AppMode::Normal {
                            if app.is_request_section_focused() {
                                app.previous_request_tab();
                            } else if app.is_response_section_focused() {
                                app.previous_response_tab();
                            } else if app.is_message_list_focused() {
                                app.select_previous();
                            }
                        }
                    }
                    KeyCode::Right => {
                        if app.is_status_focused() {
                            let desired_running = !app.is_running;
                            set_proxy_running(
                                &mut app,
                                desired_running,
                                &mut runtime.proxy_server,
                                &runtime.message_sender,
                                &runtime.proxy_state,
                            )
                            .await;
                        } else if app.app_mode == app::AppMode::Normal {
                            if app.is_request_section_focused() {
                                app.next_request_tab();
                            } else if app.is_response_section_focused() {
                                app.next_response_tab();
                            } else if app.is_message_list_focused() {
                                app.select_next();
                            }
                        }
                    }
                    KeyCode::Tab => {
                        if app.app_mode == app::AppMode::Normal {
                            app.switch_focus();
                        }
                        // Don't process any other key handling for Tab
                        continue;
                    }
                    KeyCode::BackTab => {
                        if app.app_mode == app::AppMode::Normal {
                            app.switch_focus_reverse();
                        }
                        // Don't process any other key handling for Shift+Tab
                        continue;
                    }
                    KeyCode::Char('k') => match app.app_mode {
                        app::AppMode::Normal => {
                            if app.is_message_list_focused() {
                                app.select_previous();
                            } else {
                                move_focused_detail_cursor(&mut app, -1, terminal.size()?);
                            }
                        }
                        app::AppMode::Paused | app::AppMode::Intercepting => {
                            app.scroll_intercept_details_up()
                        }
                    },
                    KeyCode::Char('j') => {
                        match app.app_mode {
                            app::AppMode::Normal => {
                                if app.is_message_list_focused() {
                                    app.select_next();
                                } else {
                                    move_focused_detail_cursor(&mut app, 1, terminal.size()?);
                                }
                            }
                            app::AppMode::Paused | app::AppMode::Intercepting => {
                                app.intercept_details_scroll += 1; // Allow unlimited scrolling, UI will clamp
                            }
                        }
                    }
                    KeyCode::Char('u') => match app.app_mode {
                        app::AppMode::Normal => {
                            move_focused_detail_cursor(&mut app, -10, terminal.size()?);
                        }
                        app::AppMode::Paused | app::AppMode::Intercepting => {
                            app.page_up_intercept_details()
                        }
                    },
                    KeyCode::Char('d') => match app.app_mode {
                        app::AppMode::Normal => {
                            move_focused_detail_cursor(&mut app, 10, terminal.size()?);
                        }
                        app::AppMode::Paused | app::AppMode::Intercepting => {
                            app.page_down_intercept_details();
                        }
                    },
                    KeyCode::Char('G') => {
                        match app.app_mode {
                            app::AppMode::Normal => {
                                move_focused_detail_cursor(&mut app, i64::MAX, terminal.size()?);
                            }
                            app::AppMode::Paused | app::AppMode::Intercepting => {
                                // For intercept mode, use a large number as max_lines
                                app.goto_bottom_intercept_details(1000, 20);
                            }
                        }
                    }
                    KeyCode::Char('g') => match app.app_mode {
                        app::AppMode::Normal => {
                            move_focused_detail_cursor(&mut app, i64::MIN, terminal.size()?);
                        }
                        app::AppMode::Paused | app::AppMode::Intercepting => {
                            app.goto_top_intercept_details()
                        }
                    },
                    KeyCode::Char('[') if app.app_mode == AppMode::Normal => {
                        if !navigate_search(&mut app, terminal.size()?, false) {
                            app.focus_previous_annotation();
                            center_active_annotation(&mut app, terminal.size()?);
                        }
                    }
                    KeyCode::Char(']') if app.app_mode == AppMode::Normal => {
                        if !navigate_search(&mut app, terminal.size()?, true) {
                            app.focus_next_annotation();
                            center_active_annotation(&mut app, terminal.size()?);
                        }
                    }
                    KeyCode::Char('/') => {
                        app.start_searching();
                    }
                    KeyCode::Char('v') if app.app_mode == AppMode::Normal => {
                        toggle_visual_selection(&mut app);
                    }
                    KeyCode::Char('n') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        match app.app_mode {
                            app::AppMode::Normal => {
                                if app.is_message_list_focused() {
                                    app.select_next();
                                } else {
                                    move_focused_detail_cursor(&mut app, 1, terminal.size()?);
                                }
                            }
                            app::AppMode::Paused | app::AppMode::Intercepting => {
                                app.select_next_pending()
                            }
                        }
                    }
                    KeyCode::Char('p') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        match app.app_mode {
                            app::AppMode::Normal => {
                                if app.is_message_list_focused() {
                                    app.select_previous();
                                } else {
                                    move_focused_detail_cursor(&mut app, -1, terminal.size()?);
                                }
                            }
                            app::AppMode::Paused | app::AppMode::Intercepting => {
                                app.select_previous_pending()
                            }
                        }
                    }
                    KeyCode::Char('a')
                        if app.app_mode != AppMode::Normal && !app.pending_requests.is_empty() =>
                    {
                        // Allow selected pending request
                        app.allow_selected_request();
                    }
                    KeyCode::Char('e')
                        if app.app_mode != AppMode::Normal && !app.pending_requests.is_empty() =>
                    {
                        if let Some(content) = app.get_pending_request_json() {
                            app.open_editor(EditorTarget::PendingRequest, content);
                        }
                    }
                    KeyCode::Char('h') => {
                        if app.is_status_focused() && app.app_mode == app::AppMode::Normal {
                            let desired_running = !app.is_running;
                            set_proxy_running(
                                &mut app,
                                desired_running,
                                &mut runtime.proxy_server,
                                &runtime.message_sender,
                                &runtime.proxy_state,
                            )
                            .await;
                            continue;
                        }

                        if app.app_mode == app::AppMode::Paused
                            || app.app_mode == app::AppMode::Intercepting
                        {
                            if let Some(content) = app.get_pending_request_headers() {
                                app.open_editor(EditorTarget::PendingHeaders, content);
                            }
                        }

                        if app.app_mode == app::AppMode::Normal
                            && (app.is_request_section_focused()
                                || app.is_response_section_focused())
                        {
                            if app.is_request_section_focused() {
                                app.previous_request_tab();
                            } else if app.is_response_section_focused() {
                                app.previous_response_tab();
                            }
                        }
                    }
                    KeyCode::Char('c')
                        if app.app_mode != AppMode::Normal && !app.pending_requests.is_empty() =>
                    {
                        if let Some(content) = app.get_pending_response_template() {
                            app.open_editor(EditorTarget::PendingResponse, content);
                        }
                    }
                    KeyCode::Char('b')
                        if app.app_mode != AppMode::Normal && !app.pending_requests.is_empty() =>
                    {
                        // Block selected pending request
                        app.block_selected_request();
                    }
                    KeyCode::Char('r')
                        if app.app_mode != AppMode::Normal && !app.pending_requests.is_empty() =>
                    {
                        // Resume all pending requests
                        app.resume_all_requests();
                    }
                    KeyCode::Char('l') => {
                        if app.is_status_focused() && app.app_mode == app::AppMode::Normal {
                            let desired_running = !app.is_running;
                            set_proxy_running(
                                &mut app,
                                desired_running,
                                &mut runtime.proxy_server,
                                &runtime.message_sender,
                                &runtime.proxy_state,
                            )
                            .await;
                        } else if app.app_mode == app::AppMode::Normal
                            && (app.is_request_section_focused()
                                || app.is_response_section_focused())
                        {
                            if app.is_request_section_focused() {
                                app.next_request_tab();
                            } else if app.is_response_section_focused() {
                                app.next_response_tab();
                            }
                        }
                    }

                    _ => {}
                }
            }
        }

        // Check if proxy server has died unexpectedly
        if let Some(handle) = &runtime.proxy_server {
            if handle.is_finished() {
                runtime.proxy_server = None;
                if app.is_running {
                    app.toggle_proxy(); // Mark as stopped
                    should_draw = true;
                }
            }
        }

        if runtime
            .control_server
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            let result = runtime.control_server.take().unwrap().await;
            let error = match result {
                Ok(Err(error)) => error,
                Err(error) => format!("Control plane stopped: {error}"),
                Ok(Ok(())) => "Control plane stopped".to_string(),
            };
            app.set_notice(format!("Error: {error}"));
            should_draw = true;
        }
    }
}

fn record_new_messages(app: &mut App, history: &mut HistoryStore) -> bool {
    let messages = app.take_new_messages();
    if messages.is_empty() {
        return false;
    }

    let active_session_id = app
        .session
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or_default();
    match history.record_messages(&active_session_id, &messages) {
        Ok(session_ids) => {
            for (message, session_id) in messages.into_iter().zip(session_ids) {
                if session_id == active_session_id {
                    app.add_message(message);
                }
            }
        }
        Err(error) => {
            app.set_notice(format!("Error: save history: {error}"));
            for message in messages {
                app.add_message(message);
            }
        }
    }
    true
}

async fn handle_overlay_key(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    runtime: &mut Runtime,
    key: KeyEvent,
) -> Result<bool> {
    match app.overlay {
        Overlay::Prefix => match key.code {
            _ if is_fullscreen_key(&key) => {
                app.close_overlay();
                app.set_panel_fullscreen(!app.panel_fullscreen);
            }
            KeyCode::Char('?') => app.show_help(),
            KeyCode::Char('a') if app.visual_selection_active && app.line_selection.is_some() => {
                app.close_overlay();
                app.start_annotating_selection();
                position_annotation_editor(app, terminal.size()?);
            }
            KeyCode::Char('d') => {
                app.close_overlay();
                let annotation_id = app
                    .annotation_to_delete()
                    .map(|annotation| annotation.id.clone());
                match annotation_id {
                    Some(id) => match remove_annotation(app, &runtime.history, &id) {
                        Ok(()) => app.set_notice("Annotation deleted"),
                        Err(error) => app.set_notice(format!("Error: {}", error.message)),
                    },
                    None => app.set_notice("No annotation under cursor"),
                }
            }
            KeyCode::Char('y') => {
                app.close_overlay();
                copy_focused_panel(terminal, app)?;
            }
            KeyCode::Char('s') => match runtime.history.list_sessions(1000) {
                Ok(sessions) => app.show_sessions(sessions),
                Err(error) => {
                    app.close_overlay();
                    app.set_notice(format!("Error: list sessions: {error}"));
                }
            },
            KeyCode::Char('n') => {
                app.close_overlay();
                app.start_naming_session();
            }
            KeyCode::Char('R') => {
                app.close_overlay();
                app.start_renaming_session();
            }
            KeyCode::Char('r') => {
                app.close_overlay();
                app.toggle_request_list();
            }
            KeyCode::Char('c') => {
                app.close_overlay();
                open_new_request(app);
            }
            KeyCode::Char('p') => {
                app.close_overlay();
                app.toggle_pause_mode();
            }
            KeyCode::Char('t') => {
                app.close_overlay();
                app.start_editing_target();
            }
            KeyCode::Char('x') => {
                app.close_overlay();
                let desired_running = !app.is_running;
                set_proxy_running(
                    app,
                    desired_running,
                    &mut runtime.proxy_server,
                    &runtime.message_sender,
                    &runtime.proxy_state,
                )
                .await;
            }
            KeyCode::Char('q') => {
                stop_proxy(&mut runtime.proxy_server).await;
                return Ok(true);
            }
            KeyCode::Esc => app.close_overlay(),
            _ => app.close_overlay(),
        },
        Overlay::Help => app.close_overlay(),
        Overlay::Sessions => match key.code {
            KeyCode::Up | KeyCode::Char('k') => app.select_previous_session(),
            KeyCode::Down | KeyCode::Char('j') => app.select_next_session(),
            KeyCode::Enter => {
                let session_id = app
                    .sessions
                    .get(app.selected_session)
                    .map(|session| session.id.clone());
                if let Some(session_id) = session_id {
                    match select_session(app, &runtime.history, &session_id) {
                        Ok(target_changed) if target_changed && app.is_running => {
                            restart_proxy(
                                app,
                                &mut runtime.proxy_server,
                                &runtime.message_sender,
                                &runtime.proxy_state,
                            )
                            .await;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            app.set_notice(format!("Error: {}", error.message));
                        }
                    }
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => app.close_overlay(),
            _ => {}
        },
        Overlay::None => {}
    }

    Ok(false)
}

fn navigate_request(app: &mut App, key: KeyCode) -> bool {
    if app.app_mode != AppMode::Normal {
        return false;
    }

    match key {
        KeyCode::Char(',') => app.select_previous(),
        KeyCode::Char('.') => app.select_next(),
        _ => return false,
    }
    true
}

fn is_fullscreen_key(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('z')
}

fn open_new_request(app: &mut App) {
    let content = r#"{
  "jsonrpc": "2.0",
  "method": "your_method",
  "params": [],
  "id": 1
}"#
    .to_string();
    app.open_editor(EditorTarget::NewRequest, content);
}

async fn stop_proxy(proxy_server: &mut Option<JoinHandle<()>>) {
    if let Some(handle) = proxy_server.take() {
        handle.abort();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn register_change_waiter(
    app: &App,
    command: ControlCommand,
    waiters: &mut Vec<ChangeWaiter>,
) -> Option<ControlCommand> {
    let ControlCommand { action, reply } = command;
    let ControlAction::WaitForChange {
        after_revision,
        timeout_ms,
    } = action
    else {
        return Some(ControlCommand { action, reply });
    };

    if app.revision() != after_revision || timeout_ms == 0 {
        let changed = app.revision() != after_revision;
        let _ = reply.send(Ok(control::change(app, changed)));
        return None;
    }

    waiters.push(ChangeWaiter {
        after_revision,
        deadline: Instant::now() + std::time::Duration::from_millis(timeout_ms),
        reply,
    });
    None
}

fn resolve_change_waiters(app: &App, waiters: &mut Vec<ChangeWaiter>) {
    let now = Instant::now();
    let mut index = 0;
    while index < waiters.len() {
        if waiters[index].reply.is_closed() {
            waiters.swap_remove(index);
            continue;
        }
        let changed = waiters[index].after_revision != app.revision();
        if !changed && waiters[index].deadline > now {
            index += 1;
            continue;
        }

        let waiter = waiters.swap_remove(index);
        let _ = waiter.reply.send(Ok(control::change(app, changed)));
    }
}

async fn handle_mouse_event(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    mouse: MouseEvent,
    proxy_server: &mut Option<JoinHandle<()>>,
    message_sender: &mpsc::UnboundedSender<app::JsonRpcMessage>,
    proxy_state: &ProxyState,
    history: &mut HistoryStore,
) -> Result<()> {
    let area = terminal.size()?;

    if app.overlay != Overlay::None {
        app.set_annotation_hover(None);
        match mouse.kind {
            MouseEventKind::ScrollUp if app.overlay == Overlay::Sessions => {
                app.select_previous_session();
            }
            MouseEventKind::ScrollDown if app.overlay == Overlay::Sessions => {
                app.select_next_session();
            }
            MouseEventKind::Down(MouseButton::Left) => {
                match ui::mouse_action(area, app, mouse.column, mouse.row) {
                    Some(ui::MouseAction::SelectSession(index)) => {
                        let session_id = app.sessions.get(index).map(|session| session.id.clone());
                        if let Some(session_id) = session_id {
                            match select_session(app, history, &session_id) {
                                Ok(target_changed) if target_changed && app.is_running => {
                                    restart_proxy(app, proxy_server, message_sender, proxy_state)
                                        .await;
                                }
                                Ok(_) => {}
                                Err(error) => {
                                    app.set_notice(format!("Error: {}", error.message));
                                }
                            }
                        }
                    }
                    Some(ui::MouseAction::CloseOverlay) => app.close_overlay(),
                    _ => {}
                }
            }
            _ => {}
        }
        return Ok(());
    }

    let hovered_focus = ui::panel_focus(area, app, mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Moved => {
            let hover = ui::annotation_hover(area, app, mouse.column, mouse.row);
            app.set_annotation_hover(hover);
            if let Some(focus) = hovered_focus {
                app.set_focus(focus);
            }
            return Ok(());
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            app.set_annotation_hover(None);
            if let Some(focus) = hovered_focus {
                app.set_focus(focus);
                let visible_lines = ui::panel_visible_lines(area, app, focus);
                scroll_panel(
                    app,
                    focus,
                    mouse.kind == MouseEventKind::ScrollDown,
                    visible_lines,
                );
            }
            return Ok(());
        }
        MouseEventKind::Down(MouseButton::Left) => {}
        _ => return Ok(()),
    }

    let Some(action) = ui::mouse_action(area, app, mouse.column, mouse.row) else {
        return Ok(());
    };
    app.set_annotation_hover(None);

    match action {
        ui::MouseAction::EditTarget => app.start_editing_target(),
        ui::MouseAction::EditSearch => app.start_searching(),
        ui::MouseAction::SetProxyRunning(should_run) => {
            app.set_focus(app::Focus::StatusHeader);
            set_proxy_running(app, should_run, proxy_server, message_sender, proxy_state).await;
        }
        ui::MouseAction::SelectExchange(index) => {
            app.set_focus(app::Focus::MessageList);
            let history_scroll = app.history_scroll_offset(ui::panel_visible_lines(
                area,
                app,
                app::Focus::MessageList,
            ));
            app.select_exchange(index);
            app.history_scroll = Some(history_scroll);
        }
        ui::MouseAction::SelectPending(index) => {
            app.set_focus(app::Focus::MessageList);
            app.selected_pending = index;
            app.reset_intercept_details_scroll();
            app.mark_changed();
        }
        ui::MouseAction::SelectRequestTab(tab) => {
            app.set_focus(app::Focus::RequestSection);
            app.request_tab = tab;
            app.request_details_scroll = 0;
            app.request_details_cursor_line = 1;
            app.clear_line_selection();
            app.mark_changed();
        }
        ui::MouseAction::SelectResponseTab(tab) => {
            app.set_focus(app::Focus::ResponseSection);
            app.response_tab = tab;
            app.response_details_scroll = 0;
            app.response_details_cursor_line = 1;
            app.clear_line_selection();
            app.mark_changed();
        }
        ui::MouseAction::AddAnnotation { panel, line } => {
            start_line_annotation(app, panel, line, area);
        }
        ui::MouseAction::SelectLine { panel, line } => {
            app.finish_visual_selection();
            let extend = mouse.modifiers.contains(KeyModifiers::SHIFT);
            let (anchor, start_line, end_line) = app.line_selection_range(panel, line, extend);
            let Some(text) = ui::detail_line_text(app, panel, start_line, end_line) else {
                return Ok(());
            };
            app.select_lines_from_anchor(panel, anchor, start_line, end_line, text);
        }
        ui::MouseAction::SelectAnnotation { id } => {
            app.focus_annotation(&id);
            center_active_annotation(app, area);
        }
        ui::MouseAction::SelectSession(_) | ui::MouseAction::CloseOverlay => {}
        ui::MouseAction::Focus(focus) => app.set_focus(focus),
    }

    Ok(())
}

fn center_active_annotation(app: &mut App, area: ratatui::layout::Rect) {
    let Some(note) = app
        .active_annotation_id
        .as_ref()
        .and_then(|id| app.annotation_by_id(id))
        .cloned()
    else {
        return;
    };
    let total = ui::detail_line_count(app, note.panel).unwrap_or(0);
    center_detail_range(
        app,
        area,
        note.panel,
        note.start_line,
        note.end_line,
        total,
        Some(&note.id),
    );
}

fn start_annotation_action(app: &mut App, reply: bool, area: ratatui::layout::Rect) {
    let Some(id) = app.annotation_to_delete().map(|note| note.id.clone()) else {
        return;
    };
    let started = if reply {
        app.start_replying_to_annotation(&id)
    } else {
        app.start_editing_annotation(&id)
    };
    if started {
        position_annotation_editor(app, area);
    }
}

fn start_line_annotation(
    app: &mut App,
    panel: app::Focus,
    line: usize,
    area: ratatui::layout::Rect,
) {
    let Some(text) = ui::detail_line_text(app, panel, line, line) else {
        return;
    };
    app.select_lines(panel, line, line, text);
    app.start_visual_selection();
    app.start_annotating_selection();
    position_annotation_editor(app, area);
}

fn position_annotation_editor(app: &mut App, area: ratatui::layout::Rect) {
    let Some((panel, scroll)) = ui::annotation_editor_scroll(area, app) else {
        return;
    };
    let current = match panel {
        app::Focus::RequestSection => &mut app.request_details_scroll,
        app::Focus::ResponseSection => &mut app.response_details_scroll,
        app::Focus::MessageList | app::Focus::StatusHeader => return,
    };
    if *current == scroll {
        return;
    }
    *current = scroll;
    app.mark_changed();
}

fn detail_source_offset(app: &App, panel: app::Focus, mut display_row: usize) -> usize {
    let mut notes = app.visible_annotations(panel).collect::<Vec<_>>();
    notes.sort_by_key(|note| note.end_line);
    for note in notes {
        if display_row < note.end_line {
            return display_row;
        }
        if display_row < note.end_line + ui::ANNOTATION_CARD_HEIGHT {
            return note.end_line.saturating_sub(1);
        }
        display_row -= ui::ANNOTATION_CARD_HEIGHT;
    }
    display_row
}

fn scroll_panel(app: &mut App, focus: app::Focus, down: bool, visible_lines: usize) {
    const LINES_PER_TICK: usize = 3;

    if focus == app::Focus::MessageList {
        match app.app_mode {
            AppMode::Normal => {
                let lines = if down {
                    LINES_PER_TICK as i64
                } else {
                    -(LINES_PER_TICK as i64)
                };
                app.scroll_history(lines, visible_lines);
            }
            AppMode::Paused | AppMode::Intercepting => {
                for _ in 0..LINES_PER_TICK {
                    if down {
                        app.select_next_pending();
                    } else {
                        app.select_previous_pending();
                    }
                }
            }
        }
        return;
    }

    let max_scroll = match (app.app_mode, focus) {
        (AppMode::Normal, app::Focus::RequestSection) => ui::detail_max_scroll(
            app,
            focus,
            app.get_request_details_content_lines(),
            visible_lines,
        ),
        (AppMode::Normal, app::Focus::ResponseSection) => ui::detail_max_scroll(
            app,
            focus,
            app.get_response_details_content_lines(),
            visible_lines,
        ),
        (AppMode::Paused | AppMode::Intercepting, app::Focus::RequestSection) => app
            .get_intercept_details_content_lines()
            .saturating_sub(visible_lines),
        _ => return,
    };

    let (previous_scroll, current_scroll) = {
        let scroll = match (app.app_mode, focus) {
            (AppMode::Normal, app::Focus::RequestSection) => &mut app.request_details_scroll,
            (AppMode::Normal, app::Focus::ResponseSection) => &mut app.response_details_scroll,
            (AppMode::Paused | AppMode::Intercepting, app::Focus::RequestSection) => {
                &mut app.intercept_details_scroll
            }
            _ => return,
        };
        let previous_scroll = *scroll;
        *scroll = if down {
            scroll.saturating_add(LINES_PER_TICK).min(max_scroll)
        } else {
            scroll.saturating_sub(LINES_PER_TICK)
        };
        (previous_scroll, *scroll)
    };
    if previous_scroll != current_scroll {
        let moved = detail_source_offset(app, focus, current_scroll)
            .abs_diff(detail_source_offset(app, focus, previous_scroll));
        match (app.app_mode, focus, down) {
            (AppMode::Normal, app::Focus::RequestSection, true) => {
                app.request_details_cursor_line = app
                    .request_details_cursor_line
                    .saturating_add(moved)
                    .min(app.get_request_details_content_lines().max(1));
            }
            (AppMode::Normal, app::Focus::RequestSection, false) => {
                app.request_details_cursor_line =
                    app.request_details_cursor_line.saturating_sub(moved).max(1);
            }
            (AppMode::Normal, app::Focus::ResponseSection, true) => {
                app.response_details_cursor_line = app
                    .response_details_cursor_line
                    .saturating_add(moved)
                    .min(app.get_response_details_content_lines().max(1));
            }
            (AppMode::Normal, app::Focus::ResponseSection, false) => {
                app.response_details_cursor_line = app
                    .response_details_cursor_line
                    .saturating_sub(moved)
                    .max(1);
            }
            _ => {}
        }
        extend_visual_selection(app, focus);
        app.mark_changed();
    }
}

fn move_focused_detail_cursor(app: &mut App, lines: i64, area: ratatui::layout::Rect) {
    let panel = app.focus;
    let Some(total_lines) = ui::detail_line_count(app, panel) else {
        return;
    };
    let visible_lines = ui::panel_visible_lines(area, app, panel);
    let previous_scroll = match panel {
        app::Focus::RequestSection => app.request_details_scroll,
        _ => app.response_details_scroll,
    };
    app.move_detail_cursor(panel, lines, total_lines, visible_lines);
    let annotations = app.visible_annotations(panel).collect::<Vec<_>>();
    if !annotations.is_empty() {
        let cursor = app.detail_cursor_line(panel).unwrap_or(1).saturating_sub(1);
        let display_row = cursor
            + annotations
                .iter()
                .filter(|note| note.end_line <= cursor)
                .count()
                * ui::ANNOTATION_CARD_HEIGHT;
        let scroll = if display_row < previous_scroll {
            display_row
        } else if display_row >= previous_scroll + visible_lines {
            display_row.saturating_sub(visible_lines.saturating_sub(1))
        } else {
            previous_scroll
        };
        match panel {
            app::Focus::RequestSection => app.request_details_scroll = scroll,
            _ => app.response_details_scroll = scroll,
        }
    }
    extend_visual_selection(app, panel);
}

fn extend_visual_selection(app: &mut App, panel: app::Focus) {
    if !app.visual_selection_active {
        return;
    }
    let Some(anchor_line) = app
        .line_selection
        .as_ref()
        .filter(|selection| selection.panel == panel)
        .map(|selection| selection.anchor_line)
    else {
        return;
    };
    let Some(cursor_line) = app.detail_cursor_line(panel) else {
        return;
    };
    let start_line = anchor_line.min(cursor_line);
    let end_line = anchor_line.max(cursor_line);
    let Some(text) = ui::detail_line_text(app, panel, start_line, end_line) else {
        return;
    };

    app.select_lines_from_anchor(panel, anchor_line, start_line, end_line, text);
}

fn toggle_visual_selection(app: &mut App) {
    let panel = app.focus;
    if !matches!(
        panel,
        app::Focus::RequestSection | app::Focus::ResponseSection
    ) {
        return;
    }
    if app.visual_selection_active {
        app.clear_line_selection();
        return;
    }

    let Some(cursor_line) = app.detail_cursor_line(panel) else {
        return;
    };
    let Some(text) = ui::detail_line_text(app, panel, cursor_line, cursor_line) else {
        return;
    };
    app.select_lines(panel, cursor_line, cursor_line, text);
    app.start_visual_selection();
}

async fn restart_proxy(
    app: &App,
    proxy_server: &mut Option<JoinHandle<()>>,
    message_sender: &mpsc::UnboundedSender<app::JsonRpcMessage>,
    proxy_state: &ProxyState,
) {
    if let Some(handle) = proxy_server.take() {
        handle.abort();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let config = app.proxy_config.clone();
    let sender = message_sender.clone();
    let state = proxy_state.clone();
    *proxy_server = Some(tokio::spawn(async move {
        match ProxyServer::from_config(&config, sender) {
            Ok(server) => {
                let _ = server.with_state(state).start().await;
            }
            Err(error) => eprintln!("Proxy server error: {error}"),
        }
    }));
}

async fn set_proxy_running(
    app: &mut App,
    should_run: bool,
    proxy_server: &mut Option<JoinHandle<()>>,
    message_sender: &mpsc::UnboundedSender<app::JsonRpcMessage>,
    proxy_state: &ProxyState,
) {
    if should_run == app.is_running {
        return;
    }

    if should_run {
        app.toggle_proxy();

        let config = app.proxy_config.clone();
        let sender_clone = message_sender.clone();
        let state_clone = proxy_state.clone();

        *proxy_server = Some(tokio::spawn(async move {
            match ProxyServer::from_config(&config, sender_clone) {
                Ok(server) => {
                    if let Err(error) = server.with_state(state_clone).start().await {
                        eprintln!("Proxy server error: {error}");
                    }
                }
                Err(error) => {
                    eprintln!("Proxy server error: {error}");
                }
            }
        }));
    } else {
        if let Some(handle) = proxy_server.take() {
            handle.abort();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        app.toggle_proxy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stdio_command_framing_and_timeout() {
        let cli = Cli::try_parse_from([
            "jsonrpc-debugger",
            "stdio",
            "--framing",
            "content-length",
            "--request-timeout",
            "45",
            "--",
            "rust-analyzer",
            "--stdio",
        ])
        .unwrap();

        assert!(matches!(
            cli.mode,
            Some(TargetMode::Stdio {
                framing: CliFraming::ContentLength,
                request_timeout: 45,
                command,
            }) if command == [OsString::from("rust-analyzer"), OsString::from("--stdio")]
        ));
    }

    #[test]
    fn stdio_request_timeout_defaults_to_120_seconds() {
        let cli = Cli::try_parse_from(["jsonrpc-debugger", "stdio", "--", "server"]).unwrap();

        assert!(matches!(
            cli.mode,
            Some(TargetMode::Stdio {
                request_timeout: 120,
                ..
            })
        ));
    }

    #[test]
    fn stdio_request_timeout_rejects_zero() {
        assert!(Cli::try_parse_from([
            "jsonrpc-debugger",
            "stdio",
            "--request-timeout",
            "0",
            "--",
            "server"
        ])
        .is_err());
    }

    #[test]
    fn parses_transparent_wrap_command() {
        let cli = Cli::try_parse_from([
            "jsonrpc-debugger",
            "wrap",
            "--framing",
            "content-length",
            "--",
            "gopls",
        ])
        .unwrap();

        assert!(matches!(
            cli.mode,
            Some(TargetMode::Wrap {
                framing: CliFraming::ContentLength,
                command,
            }) if command == [OsString::from("gopls")]
        ));
    }

    #[test]
    fn parses_attach_control_url() {
        let cli =
            Cli::try_parse_from(["jsonrpc-debugger", "attach", "http://127.0.0.1:8096"]).unwrap();

        assert!(matches!(
            cli.mode,
            Some(TargetMode::Attach { control_url })
                if control_url == "http://127.0.0.1:8096"
        ));
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn rpc_message(id: u64, direction: app::MessageDirection) -> app::JsonRpcMessage {
        let is_request = matches!(direction, app::MessageDirection::Request);
        app::JsonRpcMessage {
            id: Some(serde_json::json!(id)),
            method: is_request.then(|| format!("method_{id}")),
            params: is_request.then(|| serde_json::json!([])),
            result: (!is_request).then(|| serde_json::json!(id)),
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction,
            transport: app::TransportType::Http,
            headers: None,
        }
    }

    #[test]
    fn search_finds_request_response_headers_and_annotations() {
        let mut app = App::new();
        let mut request = rpc_message(1, app::MessageDirection::Request);
        request.method = Some("trace_needle".to_string());
        request.headers = Some(std::collections::HashMap::from([(
            "x-debug".to_string(),
            "needle-header".to_string(),
        )]));
        app.add_message(request);

        let mut response = rpc_message(1, app::MessageDirection::Response);
        response.result = Some(serde_json::json!({"receipt": "needle-result"}));
        app.add_message(response);
        app.add_annotation(LineAnnotation {
            parent_id: None,
            author: crate::app::AnnotationAuthor::Unknown,
            created_at_ms: 0,
            id: "note".to_string(),
            exchange_index: 0,
            panel: app::Focus::ResponseSection,
            tab: app::DetailTab::Body,
            start_line: 2,
            end_line: 2,
            message: "Payout recipient".to_string(),
            text: vec!["result".to_string()],
        });

        let hits = search_hits(&app, "NeEdLe").unwrap();
        assert!(hits.iter().any(|hit| {
            hit.panel == app::Focus::RequestSection && hit.tab == app::DetailTab::Headers
        }));
        assert!(hits.iter().any(|hit| {
            hit.panel == app::Focus::RequestSection && hit.tab == app::DetailTab::Body
        }));
        assert!(hits.iter().any(|hit| {
            hit.panel == app::Focus::ResponseSection && hit.tab == app::DetailTab::Body
        }));

        assert_eq!(
            search_hits(&app, "payout").unwrap(),
            vec![SearchHit {
                exchange_index: 0,
                panel: app::Focus::ResponseSection,
                tab: app::DetailTab::Body,
                start_line: 2,
                end_line: 2,
            }]
        );
    }

    #[test]
    fn search_reveals_matches_and_wraps_navigation() {
        let mut app = App::new();
        for id in 1..=2 {
            let mut request = rpc_message(id, app::MessageDirection::Request);
            request.params = Some(serde_json::json!({"value": format!("needle_{id}")}));
            app.add_message(request);
        }
        app.input_buffer = "needle".to_string();
        let area = ratatui::layout::Rect::new(0, 0, 120, 40);

        apply_search(&mut app, area);
        assert_eq!(app.search_progress(), (1, 2));
        assert_eq!(app.selected_exchange, 0);
        assert!(app
            .line_selection
            .as_ref()
            .unwrap()
            .text
            .join("\n")
            .contains("needle_1"));

        assert!(navigate_search(&mut app, area, true));
        assert_eq!(app.search_progress(), (2, 2));
        assert_eq!(app.selected_exchange, 1);

        assert!(navigate_search(&mut app, area, true));
        assert_eq!(app.search_progress(), (1, 2));
        assert_eq!(app.selected_exchange, 0);

        assert!(navigate_search(&mut app, area, false));
        assert_eq!(app.search_progress(), (2, 2));
        assert_eq!(app.selected_exchange, 1);
    }

    #[test]
    fn request_navigation_keys_work_from_details() {
        let mut app = App::new();
        app.add_message(rpc_message(1, app::MessageDirection::Request));
        app.add_message(rpc_message(2, app::MessageDirection::Request));
        app.select_exchange(0);
        app.set_focus(app::Focus::ResponseSection);

        assert!(navigate_request(&mut app, KeyCode::Char('.')));
        assert_eq!(app.selected_exchange, 1);
        assert_eq!(app.focus, app::Focus::ResponseSection);

        assert!(navigate_request(&mut app, KeyCode::Char(',')));
        assert_eq!(app.selected_exchange, 0);

        app.app_mode = AppMode::Paused;
        assert!(!navigate_request(&mut app, KeyCode::Char('.')));
        assert_eq!(app.selected_exchange, 0);
    }

    #[test]
    fn hover_annotation_starts_the_existing_single_line_prompt() {
        let mut app = App::new();
        app.add_message(rpc_message(1, app::MessageDirection::Request));

        start_line_annotation(
            &mut app,
            app::Focus::RequestSection,
            2,
            ratatui::layout::Rect::new(0, 0, 120, 24),
        );

        assert_eq!(app.input_mode, app::InputMode::AnnotatingSelection);
        assert!(app.visual_selection_active);
        assert_eq!(app.line_selection.unwrap().start_line, 2);
    }

    #[test]
    fn records_every_message_from_one_queue_drain_as_one_batch() {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut app = App::new_with_receiver(receiver);
        let mut history = HistoryStore::in_memory().unwrap();
        let session = history
            .create_session(Some("batch"), "http://node")
            .unwrap();
        app.activate_session(session.clone(), Vec::new(), Vec::new());

        for message in [
            rpc_message(1, app::MessageDirection::Request),
            rpc_message(2, app::MessageDirection::Request),
            rpc_message(2, app::MessageDirection::Response),
            rpc_message(1, app::MessageDirection::Response),
        ] {
            sender.send(message).unwrap();
        }

        assert!(record_new_messages(&mut app, &mut history));
        assert!(!record_new_messages(&mut app, &mut history));
        assert_eq!(app.exchanges().len(), 2);
        assert!(app
            .exchanges()
            .iter()
            .all(|exchange| exchange.unwrap().response.is_some()));
        let persisted = history.load_session(&session.id).unwrap().1;
        assert_eq!(persisted.len(), app.exchanges().len());
        for (persisted, displayed) in persisted.iter().zip(app.exchanges().iter()) {
            let displayed = displayed.unwrap();
            assert_eq!(persisted.id, displayed.id);
            assert_eq!(persisted.method, displayed.method);
            assert!(persisted.request.is_some());
            assert!(persisted.response.is_some());
        }
    }

    #[test]
    fn fullscreen_prefix_key_is_z() {
        assert!(is_fullscreen_key(&key(KeyCode::Char('z'))));
        assert!(!is_fullscreen_key(&key(KeyCode::Esc)));
    }

    #[test]
    fn inline_editor_supports_insert_and_vim_save() {
        let mut editor = TextEditor::new(EditorTarget::NewRequest, "{}".to_string());

        assert_eq!(
            handle_editor_key(&mut editor, key(KeyCode::Char('i'))),
            EditorAction::None
        );
        handle_editor_key(&mut editor, key(KeyCode::Char('x')));
        handle_editor_key(&mut editor, key(KeyCode::Esc));
        handle_editor_key(&mut editor, key(KeyCode::Char(':')));
        handle_editor_key(&mut editor, key(KeyCode::Char('w')));
        handle_editor_key(&mut editor, key(KeyCode::Char('q')));

        assert_eq!(
            handle_editor_key(&mut editor, key(KeyCode::Enter)),
            EditorAction::Save
        );
        assert_eq!(editor.content(), "x{}");
    }

    #[test]
    fn inline_editor_can_cancel_without_quitting_the_app() {
        let mut editor = TextEditor::new(EditorTarget::NewRequest, "{}".to_string());

        assert_eq!(
            handle_editor_key(&mut editor, key(KeyCode::Char('q'))),
            EditorAction::Cancel
        );
    }

    #[test]
    fn inline_editor_w_moves_to_the_next_word() {
        let mut editor = TextEditor::new(EditorTarget::NewRequest, "one two\nthree".to_string());

        handle_editor_key(&mut editor, key(KeyCode::Char('w')));
        assert_eq!((editor.row, editor.column), (0, 4));

        handle_editor_key(&mut editor, key(KeyCode::Char('w')));
        assert_eq!((editor.row, editor.column), (1, 0));
    }

    #[test]
    fn inline_editor_composes_change_with_word_motion_and_undo() {
        let mut editor = TextEditor::new(EditorTarget::NewRequest, "alpha beta".to_string());

        handle_editor_key(&mut editor, key(KeyCode::Char('c')));
        handle_editor_key(&mut editor, key(KeyCode::Char('w')));
        assert_eq!(editor.mode, EditorMode::Insert);
        assert_eq!(editor.content(), " beta");

        handle_editor_key(&mut editor, key(KeyCode::Char('x')));
        handle_editor_key(&mut editor, key(KeyCode::Esc));
        assert_eq!(editor.content(), "x beta");

        handle_editor_key(&mut editor, key(KeyCode::Char('u')));
        assert_eq!(editor.content(), "alpha beta");
    }

    #[test]
    fn inline_editor_supports_line_operators_and_linewise_paste() {
        let mut editor = TextEditor::new(EditorTarget::NewRequest, "one\ntwo".to_string());

        handle_editor_key(&mut editor, key(KeyCode::Char('d')));
        handle_editor_key(&mut editor, key(KeyCode::Char('d')));
        assert_eq!(editor.content(), "two");

        handle_editor_key(&mut editor, key(KeyCode::Char('p')));
        assert_eq!(editor.content(), "two\none");
    }

    #[test]
    fn inline_editor_word_operators_share_the_same_motions() {
        let mut editor = TextEditor::new(EditorTarget::NewRequest, "one two".to_string());
        handle_editor_key(&mut editor, key(KeyCode::Char('d')));
        handle_editor_key(&mut editor, key(KeyCode::Char('w')));
        assert_eq!(editor.content(), "two");

        let mut editor = TextEditor::new(EditorTarget::NewRequest, "one two".to_string());
        handle_editor_key(&mut editor, key(KeyCode::Char('d')));
        handle_editor_key(&mut editor, key(KeyCode::Char('e')));
        assert_eq!(editor.content(), " two");

        let mut editor = TextEditor::new(EditorTarget::NewRequest, "one two".to_string());
        handle_editor_key(&mut editor, key(KeyCode::Char('w')));
        handle_editor_key(&mut editor, key(KeyCode::Char('d')));
        handle_editor_key(&mut editor, key(KeyCode::Char('b')));
        assert_eq!(editor.content(), "two");
    }

    #[test]
    fn mouse_wheel_scrolls_the_focused_panel() {
        let mut app = App::new();
        app.add_message(app::JsonRpcMessage {
            id: Some(serde_json::json!(1)),
            method: Some("long_request".to_string()),
            params: Some(serde_json::json!([1, 2, 3, 4, 5, 6, 7, 8])),
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: app::MessageDirection::Request,
            transport: app::TransportType::Http,
            headers: None,
        });
        scroll_panel(&mut app, app::Focus::RequestSection, true, 2);
        assert_eq!(app.request_details_scroll, 3);

        scroll_panel(&mut app, app::Focus::RequestSection, false, 2);
        assert_eq!(app.request_details_scroll, 0);
    }

    #[test]
    fn mouse_wheel_scrolls_request_list_without_changing_selection() {
        let mut app = App::new();
        for id in 0..6 {
            app.add_message(app::JsonRpcMessage {
                id: Some(serde_json::json!(id)),
                method: Some(format!("request_{id}")),
                params: Some(serde_json::json!([])),
                result: None,
                error: None,
                timestamp: std::time::SystemTime::now(),
                direction: app::MessageDirection::Request,
                transport: app::TransportType::Http,
                headers: None,
            });
        }
        app.select_exchange(1);

        scroll_panel(&mut app, app::Focus::MessageList, true, 2);

        assert_eq!(app.selected_exchange, 1);
        assert_eq!(app.history_scroll, Some(3));
    }

    #[test]
    fn enter_on_request_list_focuses_the_visible_response() {
        let mut app = App::new();
        for (id, method) in [(1, "first"), (2, "second")] {
            app.add_message(app::JsonRpcMessage {
                id: Some(serde_json::json!(id)),
                method: Some(method.to_string()),
                params: Some(serde_json::json!([])),
                result: None,
                error: None,
                timestamp: std::time::SystemTime::now(),
                direction: app::MessageDirection::Request,
                transport: app::TransportType::Http,
                headers: None,
            });
        }
        app.selected_exchange = 0;
        app.filter_text = "second".to_string();

        assert!(enter_request_list(&mut app));
        assert_eq!(app.selected_exchange, 1);
        assert_eq!(app.focus, app::Focus::ResponseSection);
    }

    #[test]
    fn visual_selection_still_copies_from_request_list_focus() {
        let mut app = App::new();
        app.visual_selection_active = true;

        assert!(!enter_request_list(&mut app));
        assert_eq!(app.focus, app::Focus::MessageList);
    }

    #[test]
    fn keyboard_cursor_stops_at_the_bottom_and_keeps_it_visible() {
        let mut app = App::new();
        app.add_message(app::JsonRpcMessage {
            id: Some(serde_json::json!(1)),
            method: Some("long_request".to_string()),
            params: Some(serde_json::json!([1, 2, 3, 4, 5, 6, 7, 8])),
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: app::MessageDirection::Request,
            transport: app::TransportType::Http,
            headers: None,
        });
        app.focus = app::Focus::RequestSection;
        let area = ratatui::layout::Rect::new(0, 0, 120, 24);
        let visible_lines = ui::panel_visible_lines(area, &app, app.focus);
        let bottom = ui::detail_line_count(&app, app.focus)
            .unwrap()
            .saturating_sub(visible_lines);

        let total_lines = ui::detail_line_count(&app, app.focus).unwrap();
        move_focused_detail_cursor(&mut app, i64::MAX, area);
        assert_eq!(app.request_details_cursor_line, total_lines);
        assert_eq!(app.request_details_scroll, bottom);

        move_focused_detail_cursor(&mut app, -1, area);
        assert_eq!(app.request_details_cursor_line, total_lines - 1);
        assert_eq!(app.request_details_scroll, bottom);
    }

    #[test]
    fn visual_selection_extends_from_its_keyboard_anchor() {
        let mut app = App::new();
        app.add_message(app::JsonRpcMessage {
            id: Some(serde_json::json!(1)),
            method: Some("eth_call".to_string()),
            params: Some(serde_json::json!([])),
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: app::MessageDirection::Request,
            transport: app::TransportType::Http,
            headers: None,
        });
        app.focus = app::Focus::RequestSection;
        let area = ratatui::layout::Rect::new(0, 0, 120, 24);

        toggle_visual_selection(&mut app);
        move_focused_detail_cursor(&mut app, 2, area);

        let selection = app.line_selection.as_ref().unwrap();
        assert_eq!(app.request_details_cursor_line, 3);
        assert_eq!(selection.anchor_line, 1);
        assert_eq!((selection.start_line, selection.end_line), (1, 3));
        assert_eq!(selection.text.len(), 3);

        toggle_visual_selection(&mut app);
        assert!(app.line_selection.is_none());
    }

    #[test]
    fn visual_selection_annotation_uses_the_persistent_annotation_path() {
        let mut history = HistoryStore::in_memory().unwrap();
        let mut app = App::new();
        app.session = Some(
            history
                .create_session(Some("test"), "http://localhost:8090")
                .unwrap(),
        );
        app.add_message(app::JsonRpcMessage {
            id: Some(serde_json::json!(1)),
            method: Some("eth_call".to_string()),
            params: Some(serde_json::json!([])),
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: app::MessageDirection::Request,
            transport: app::TransportType::Http,
            headers: None,
        });
        app.focus = app::Focus::RequestSection;
        let text = ui::detail_line_text(&app, app.focus, 2, 2).unwrap();
        app.select_lines(app::Focus::RequestSection, 2, 2, text);
        app.start_visual_selection();

        annotate_visual_selection(
            &mut app,
            &history,
            ratatui::layout::Rect::new(0, 0, 120, 24),
            "Check this method",
        )
        .unwrap();

        assert_eq!(app.annotations()[0].message, "Check this method");
        assert!(!app.visual_selection_active);
        let session_id = app.session.as_ref().unwrap().id.clone();
        assert_eq!(history.annotations(&session_id).unwrap(), app.annotations());

        let annotation_id = app.annotations()[0].id.clone();
        update_annotation(&mut app, &history, &annotation_id, "Check the method name").unwrap();
        assert_eq!(app.annotations()[0].id, annotation_id);
        assert_eq!(app.annotations()[0].message, "Check the method name");
        assert_eq!(
            history.annotations(&session_id).unwrap()[0].message,
            "Check the method name"
        );
    }

    #[test]
    fn scrolling_does_not_change_a_fixed_line_reference() {
        let mut app = App::new();
        app.add_message(app::JsonRpcMessage {
            id: Some(serde_json::json!(1)),
            method: Some("eth_call".to_string()),
            params: Some(serde_json::json!([1, 2, 3, 4, 5, 6, 7, 8])),
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: app::MessageDirection::Request,
            transport: app::TransportType::Http,
            headers: None,
        });
        let text = ui::detail_line_text(&app, app::Focus::RequestSection, 2, 3).unwrap();
        app.reveal_lines(app::Focus::RequestSection, 2, 3, text.clone());
        app.add_annotation(LineAnnotation {
            parent_id: None,
            author: crate::app::AnnotationAuthor::Unknown,
            created_at_ms: 0,
            id: "annotation-1".to_string(),
            exchange_index: 0,
            panel: app::Focus::RequestSection,
            tab: app::DetailTab::Body,
            start_line: 2,
            end_line: 3,
            message: "Inspect this range".to_string(),
            text,
        });

        scroll_panel(&mut app, app::Focus::RequestSection, true, 2);

        let selection = app.line_selection.as_ref().unwrap();
        assert!(!app.visual_selection_active);
        assert_eq!((selection.start_line, selection.end_line), (2, 3));
        assert_eq!(
            app.annotations()
                .first()
                .map(|annotation| (annotation.start_line, annotation.end_line)),
            Some((2, 3))
        );
    }

    #[test]
    fn selected_annotation_is_centered_in_the_panel() {
        let mut app = App::new();
        let area = ratatui::layout::Rect::new(0, 0, 120, 50);
        let visible_lines = ui::panel_visible_lines(area, &app, app::Focus::ResponseSection);
        app.add_annotation(LineAnnotation {
            parent_id: None,
            author: crate::app::AnnotationAuthor::Unknown,
            created_at_ms: 0,
            id: "annotation-1".to_string(),
            exchange_index: 0,
            panel: app::Focus::ResponseSection,
            tab: app::DetailTab::Body,
            start_line: 17,
            end_line: 21,
            message: "Inspect this range".to_string(),
            text: Vec::new(),
        });

        center_detail_range(
            &mut app,
            area,
            app::Focus::ResponseSection,
            17,
            21,
            50,
            Some("annotation-1"),
        );

        let display_scroll = app.response_details_scroll;
        let viewport_center = display_scroll + visible_lines.saturating_sub(1) / 2;
        assert_eq!(viewport_center, 21 + ui::ANNOTATION_CARD_HEIGHT / 2);
    }

    #[tokio::test]
    async fn wait_for_change_returns_only_after_a_new_revision() {
        let mut app = App::new();
        let (reply, result) = tokio::sync::oneshot::channel();
        let command = ControlCommand {
            action: ControlAction::WaitForChange {
                after_revision: app.revision(),
                timeout_ms: 1_000,
            },
            reply,
        };
        let mut waiters = Vec::new();

        assert!(register_change_waiter(&app, command, &mut waiters).is_none());
        assert_eq!(waiters.len(), 1);
        app.set_focus(app::Focus::ResponseSection);
        resolve_change_waiters(&app, &mut waiters);

        let response = result.await.unwrap().unwrap();
        assert_eq!(response["changed"], true);
        assert_eq!(response["state"]["revision"], app.revision());
        assert!(waiters.is_empty());
    }

    #[tokio::test]
    async fn zero_timeout_checks_revision_without_waiting() {
        let app = App::new();
        let (reply, result) = tokio::sync::oneshot::channel();
        let command = ControlCommand {
            action: ControlAction::WaitForChange {
                after_revision: app.revision(),
                timeout_ms: 0,
            },
            reply,
        };
        let mut waiters = Vec::new();

        assert!(register_change_waiter(&app, command, &mut waiters).is_none());

        assert_eq!(result.await.unwrap().unwrap()["changed"], false);
        assert!(waiters.is_empty());
    }

    #[tokio::test]
    async fn control_commands_update_the_visible_app_state() {
        let mut app = App::new();
        app.add_message(app::JsonRpcMessage {
            id: Some(serde_json::json!(1)),
            method: Some("eth_call".to_string()),
            params: Some(serde_json::json!([])),
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: app::MessageDirection::Request,
            transport: app::TransportType::Http,
            headers: None,
        });
        app.add_message(app::JsonRpcMessage {
            id: Some(serde_json::json!(2)),
            method: Some("net_version".to_string()),
            params: Some(serde_json::json!([])),
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: app::MessageDirection::Request,
            transport: app::TransportType::Http,
            headers: None,
        });
        let (message_sender, _) = mpsc::unbounded_channel();
        let (pending_sender, _) = mpsc::unbounded_channel();
        let proxy_state = ProxyState {
            app_mode: Arc::new(Mutex::new(AppMode::Normal)),
            pending_sender,
        };
        let (notice_sender, _) = mpsc::unbounded_channel();
        let (reply, result) = tokio::sync::oneshot::channel();
        let mut proxy_server = None;
        let mut history = HistoryStore::in_memory().unwrap();
        app.session = Some(
            history
                .create_session(Some("test"), "http://localhost:8090")
                .unwrap(),
        );
        let terminal_area = ratatui::layout::Rect::new(0, 0, 120, 24);

        let revision_before_reads = app.revision();
        for action in [
            ControlAction::Discover,
            ControlAction::GetState {
                include_annotations: false,
            },
            ControlAction::GetUpdates {
                session_id: None,
                next_index: 0,
                pending_indices: vec![],
                annotation_revision: None,
            },
            ControlAction::GetAnnotations,
            ControlAction::GetPending,
            ControlAction::GetPanel {
                focus: app::Focus::RequestSection,
                exchange_index: Some(0),
                tab: Some(app::DetailTab::Body),
            },
        ] {
            let (reply, result) = tokio::sync::oneshot::channel();
            handle_control_command(
                &mut app,
                ControlCommand { action, reply },
                ControlContext {
                    terminal_area,
                    proxy_server: &mut proxy_server,
                    message_sender: &message_sender,
                    proxy_state: &proxy_state,
                    request_result_sender: &notice_sender,
                    history: &mut history,
                },
            )
            .await;
            assert!(result.await.unwrap().is_ok());
            assert_eq!(app.revision(), revision_before_reads);
        }

        handle_control_command(
            &mut app,
            ControlCommand {
                action: ControlAction::SetFocus {
                    focus: app::Focus::ResponseSection,
                },
                reply,
            },
            ControlContext {
                terminal_area,
                proxy_server: &mut proxy_server,
                message_sender: &message_sender,
                proxy_state: &proxy_state,
                request_result_sender: &notice_sender,
                history: &mut history,
            },
        )
        .await;

        assert!(app.revision() > revision_before_reads);
        assert_eq!(app.focus, app::Focus::ResponseSection);
        assert_eq!(result.await.unwrap().unwrap()["focus"], "response");

        let (reply, result) = tokio::sync::oneshot::channel();
        handle_control_command(
            &mut app,
            ControlCommand {
                action: ControlAction::RevealLines {
                    focus: app::Focus::RequestSection,
                    start_line: 2,
                    end_line: 2,
                    session_id: None,
                    exchange_index: None,
                    tab: None,
                },
                reply,
            },
            ControlContext {
                terminal_area,
                proxy_server: &mut proxy_server,
                message_sender: &message_sender,
                proxy_state: &proxy_state,
                request_result_sender: &notice_sender,
                history: &mut history,
            },
        )
        .await;

        let state = result.await.unwrap().unwrap();
        assert_eq!(state["lineSelection"]["text"], "Method: eth_call");
        assert_eq!(app.request_details_scroll, 0);
        let viewport = control::state(&app);

        let (reply, result) = tokio::sync::oneshot::channel();
        handle_control_command(
            &mut app,
            ControlCommand {
                action: ControlAction::AnnotateLines {
                    focus: app::Focus::RequestSection,
                    exchange_index: Some(1),
                    tab: Some(app::DetailTab::Body),
                    start_line: 2,
                    end_line: 2,
                    message: "Inspect this method".to_string(),
                    author: AnnotationAuthor::Agent,
                },
                reply,
            },
            ControlContext {
                terminal_area,
                proxy_server: &mut proxy_server,
                message_sender: &message_sender,
                proxy_state: &proxy_state,
                request_result_sender: &notice_sender,
                history: &mut history,
            },
        )
        .await;

        let result = result.await.unwrap().unwrap();
        let annotation = &result["annotation"];
        let state = &result["state"];
        assert_eq!(annotation["message"], "Inspect this method");
        assert_eq!(annotation["text"], "Method: net_version");
        assert_eq!(annotation["exchangeIndex"], 1);
        assert_eq!(state["selectedExchange"], viewport["selectedExchange"]);
        assert_eq!(state["focus"], viewport["focus"]);
        assert_eq!(state["scroll"], viewport["scroll"]);
        assert_eq!(state["tabs"], viewport["tabs"]);
        assert_eq!(state["lineSelection"], viewport["lineSelection"]);
        let annotation_id = annotation["id"].as_str().unwrap().to_string();

        let session_id = app.session.as_ref().unwrap().id.clone();
        let (reply, result) = tokio::sync::oneshot::channel();
        handle_control_command(
            &mut app,
            ControlCommand {
                action: ControlAction::RenameSession {
                    id: session_id,
                    name: "Renamed test".to_string(),
                },
                reply,
            },
            ControlContext {
                terminal_area,
                proxy_server: &mut proxy_server,
                message_sender: &message_sender,
                proxy_state: &proxy_state,
                request_result_sender: &notice_sender,
                history: &mut history,
            },
        )
        .await;
        assert_eq!(
            result.await.unwrap().unwrap()["session"]["name"],
            "Renamed test"
        );

        let (reply, result) = tokio::sync::oneshot::channel();
        handle_control_command(
            &mut app,
            ControlCommand {
                action: ControlAction::RemoveAnnotation { id: annotation_id },
                reply,
            },
            ControlContext {
                terminal_area,
                proxy_server: &mut proxy_server,
                message_sender: &message_sender,
                proxy_state: &proxy_state,
                request_result_sender: &notice_sender,
                history: &mut history,
            },
        )
        .await;
        assert!(result.await.unwrap().unwrap()["annotations"]
            .as_array()
            .unwrap()
            .is_empty());

        let saved = history
            .create_session(Some("Saved"), "http://localhost:8090")
            .unwrap();
        history
            .record_messages(&saved.id, &[rpc_message(9, app::MessageDirection::Request)])
            .unwrap();
        let (reply, result) = tokio::sync::oneshot::channel();
        handle_control_command(
            &mut app,
            ControlCommand {
                action: ControlAction::RevealLines {
                    focus: app::Focus::RequestSection,
                    start_line: 2,
                    end_line: 2,
                    session_id: Some(saved.id.clone()),
                    exchange_index: Some(0),
                    tab: Some(app::DetailTab::Headers),
                },
                reply,
            },
            ControlContext {
                terminal_area,
                proxy_server: &mut proxy_server,
                message_sender: &message_sender,
                proxy_state: &proxy_state,
                request_result_sender: &notice_sender,
                history: &mut history,
            },
        )
        .await;

        let state = result.await.unwrap().unwrap();
        assert_eq!(state["session"]["id"], saved.id);
        assert_eq!(state["selectedExchange"], 0);
        assert_eq!(state["tabs"]["request"], "headers");
        assert_eq!(state["lineSelection"]["text"], "Method: method_9");
    }

    #[test]
    fn reply_and_edit_drafts_persist_without_losing_thread_metadata() {
        let mut history = HistoryStore::in_memory().unwrap();
        let mut app = App::new();
        app.session = Some(
            history
                .create_session(Some("thread"), "http://node")
                .unwrap(),
        );
        app.add_message(rpc_message(1, app::MessageDirection::Request));
        let root = build_annotation(
            &app,
            app::Focus::RequestSection,
            None,
            None,
            2,
            2,
            "Check this method",
        )
        .unwrap();
        let root_id = root.id.clone();
        persist_annotation(&mut app, &history, root).unwrap();
        app.focus_annotation(&root_id);
        let area = ratatui::layout::Rect::new(0, 0, 120, 30);
        start_annotation_action(&mut app, true, area);
        assert_eq!(app.annotation_reply_id.as_ref(), Some(&root_id));
        app.input_buffer = "Added a regression test.".to_string();
        save_annotation_draft(&mut app, &history, area).unwrap();
        let reply = app.annotations()[1].clone();
        assert_eq!(reply.author, AnnotationAuthor::User);
        assert_eq!(reply.parent_id.as_ref(), Some(&root_id));
        assert_eq!((reply.start_line, reply.end_line), (2, 2));
        assert!(reply.created_at_ms > 0);
        assert_eq!(app.active_annotation_id.as_ref(), Some(&reply.id));
        assert_eq!(app.input_mode, app::InputMode::Normal);
        start_annotation_action(&mut app, false, area);
        app.input_buffer = "Updated regression test.".to_string();
        save_annotation_draft(&mut app, &history, area).unwrap();
        assert_eq!(
            app.annotations()[1],
            LineAnnotation {
                message: "Updated regression test.".to_string(),
                ..reply.clone()
            }
        );
        assert_eq!(
            history
                .annotations(&app.session.as_ref().unwrap().id)
                .unwrap()
                .len(),
            2
        );
        app.start_replying_to_annotation(&reply.id);
        app.input_buffer = "Keep this draft".to_string();
        remove_annotation(&mut app, &history, &reply.id).unwrap();
        assert!(save_annotation_draft(&mut app, &history, area).is_err());
        assert_eq!(app.input_buffer, "Keep this draft");
        assert_eq!(app.annotation_reply_id.as_ref(), Some(&reply.id));
    }

    #[test]
    fn agent_replies_inherit_references_without_moving_the_viewport() {
        let mut history = HistoryStore::in_memory().unwrap();
        let mut app = App::new();
        app.session = Some(
            history
                .create_session(Some("thread"), "http://node")
                .unwrap(),
        );
        app.add_message(rpc_message(1, app::MessageDirection::Request));
        let root = build_annotation(
            &app,
            app::Focus::RequestSection,
            None,
            None,
            2,
            3,
            "Check this range",
        )
        .unwrap();
        persist_annotation(&mut app, &history, root.clone()).unwrap();
        app.request_details_scroll = 4;
        let before = control::state(&app);
        let reply = build_reply(&app, &root.id, "I checked it.", AnnotationAuthor::Agent).unwrap();
        assert_eq!(reply.text, root.text);
        assert_eq!(reply.tab, root.tab);
        persist_annotation(&mut app, &history, reply).unwrap();
        let after = control::state(&app);
        for key in [
            "focus",
            "scroll",
            "selectedExchange",
            "tabs",
            "lineSelection",
            "activeAnnotationId",
        ] {
            assert_eq!(before[key], after[key]);
        }
        for invalid in ["", "  ", "line\nbreak"] {
            assert!(build_reply(&app, &root.id, invalid, AnnotationAuthor::Agent).is_err());
        }
        assert!(build_reply(&app, "missing", "Valid", AnnotationAuthor::Agent).is_err());
        app.app_mode = AppMode::Paused;
        assert!(build_reply(&app, &root.id, "Valid", AnnotationAuthor::Agent).is_err());
    }
}
