use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use clap::Parser;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::io::Write;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

mod app;
mod control;
mod proxy;
mod ui;

use app::{App, AppMode, EditorMode, EditorMotion, EditorOperator, EditorTarget, TextEditor};
use control::{ControlAction, ControlCommand, ControlError, PendingDecision};
use proxy::{ProxyServer, ProxyState};

#[derive(Parser)]
#[command(name = "jsonrpc-debugger", version)]
#[command(about = "A JSON-RPC debugger TUI for intercepting and inspecting requests")]
struct Cli {
    /// Port to listen on for incoming requests
    #[arg(short, long, default_value = "8080")]
    port: u16,

    /// Target URL to proxy requests to
    #[arg(short, long)]
    target: Option<String>,

    /// Port for the local JSON-RPC control plane (defaults to proxy port + 1)
    #[arg(long)]
    control_port: Option<u16>,
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
                app.notice = Some("Sending request…".to_string());
                return;
            }
            Err(error) => Err(error),
        },
    };

    match result {
        Ok(notice) => app.notice = Some(notice),
        Err(error) => {
            editor.error = Some(error);
            app.editor = Some(editor);
        }
    }
}

async fn handle_control_command(
    app: &mut App,
    command: ControlCommand,
    proxy_server: &mut Option<JoinHandle<()>>,
    message_sender: &mpsc::UnboundedSender<app::JsonRpcMessage>,
    proxy_state: &ProxyState,
    request_result_sender: &mpsc::UnboundedSender<Result<(), String>>,
) {
    let ControlCommand { action, reply } = command;
    let result = match action {
        ControlAction::Discover => Ok(control::discovery(app.control_port)),
        ControlAction::GetState => Ok(control::state(app)),
        ControlAction::WaitForChange { .. } => {
            unreachable!("wait commands are registered by run_app")
        }
        ControlAction::GetPanel { focus } => {
            if app.app_mode != AppMode::Normal {
                Err(ControlError::invalid_params(
                    "line references require normal mode",
                ))
            } else {
                match ui::detail_lines_text(app, focus) {
                    Some(lines) => Ok(control::panel(focus, lines)),
                    None => Err(ControlError::invalid_params(
                        "panel must be request or response",
                    )),
                }
            }
        }
        ControlAction::GetHistory { limit } => Ok(control::history(app, limit)),
        ControlAction::ExportSession => Ok(serde_json::to_value(control::export_session(app))
            .expect("session values are serializable")),
        ControlAction::ReplaySession { session } => match control::replay_session(session) {
            Ok(exchanges) => {
                let imported = exchanges.len();
                app.append_exchanges(exchanges);
                Ok(serde_json::json!({
                    "imported": imported,
                    "state": control::state(app),
                }))
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
            if index >= app.exchanges.len() {
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
        ControlAction::RevealLines {
            focus,
            start_line,
            end_line,
        } => {
            if app.app_mode != AppMode::Normal {
                Err(ControlError::invalid_params(
                    "line references require normal mode",
                ))
            } else {
                let total_lines = ui::detail_line_count(app, focus).unwrap_or(0);
                match ui::detail_line_text(app, focus, start_line, end_line) {
                    Some(text) => {
                        app.reveal_lines(focus, start_line, end_line, text);
                        Ok(control::state(app))
                    }
                    None => Err(ControlError::invalid_params(format!(
                        "line range must be within 1..={total_lines}"
                    ))),
                }
            }
        }
        ControlAction::ClearLineSelection => {
            app.clear_line_selection();
            Ok(control::state(app))
        }
        ControlAction::ScrollPanel { focus, lines } => {
            if focus == app::Focus::MessageList {
                scroll_history(app, lines);
                Ok(control::state(app))
            } else if app.app_mode != AppMode::Normal {
                Err(ControlError::invalid_params(
                    "request and response scrolling requires normal mode",
                ))
            } else {
                let total_lines = ui::detail_line_count(app, focus).unwrap_or(0);
                app.scroll_panel_lines(focus, lines, total_lines);
                Ok(control::state(app))
            }
        }
        ControlAction::SetTarget { url } => {
            let url = url.trim();
            if url.is_empty() {
                Err(ControlError::invalid_params("url cannot be empty"))
            } else {
                let changed = app.proxy_config.target_url != url;
                if changed {
                    app.proxy_config.target_url = url.to_string();
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
            Ok(control::state(app))
        }
        ControlAction::ResolvePending { id, decision } => resolve_pending(app, id, decision),
    };

    let _ = reply.send(result);
}

fn scroll_history(app: &mut App, lines: i64) {
    app.focus = app::Focus::MessageList;
    match app.app_mode {
        AppMode::Normal => {
            let indices = app.filtered_exchange_indices();
            let Some(current) = indices
                .iter()
                .position(|index| *index == app.selected_exchange)
            else {
                return;
            };
            let selected = offset_index(current, lines, indices.len());
            app.select_exchange(indices[selected]);
        }
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
    let control_port = cli
        .control_port
        .or_else(|| cli.port.checked_add(1))
        .ok_or_else(|| anyhow::anyhow!("--control-port is required when --port is 65535"))?;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create message channel for proxy communication
    let (message_sender, message_receiver) = mpsc::unbounded_channel();

    // Create pending request channel for pause/intercept functionality
    let (pending_sender, pending_receiver) = mpsc::unbounded_channel();

    // Create the local agent control plane.
    let (control_sender, control_receiver) = mpsc::unbounded_channel();
    let control_handle = tokio::spawn(control::serve(control_port, control_sender));

    // Create shared state for pause/intercept
    let shared_app_mode = Arc::new(Mutex::new(AppMode::Normal));
    let proxy_state = ProxyState {
        app_mode: shared_app_mode.clone(),
        pending_sender,
    };

    // Create app with receiver, using CLI arguments
    let mut app = App::new_with_receiver(message_receiver);

    // Override default config with CLI arguments
    app.proxy_config.listen_port = cli.port;
    app.control_port = control_port;
    if let Some(target) = cli.target {
        app.proxy_config.target_url = target;
    }

    // Start the proxy server immediately since app.is_running is true by default
    let initial_server = ProxyServer::new(
        app.proxy_config.listen_port,
        app.proxy_config.target_url.clone(),
        message_sender.clone(),
    )
    .with_state(proxy_state.clone());
    let initial_proxy_handle = tokio::spawn(async move {
        if let Err(_e) = initial_server.start().await {
            // Silent error handling
        }
    });

    let (request_result_sender, request_result_receiver) = mpsc::unbounded_channel();
    let runtime = Runtime {
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

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mut app: App,
    mut runtime: Runtime,
) -> Result<()> {
    let mut should_draw = true;

    loop {
        // Check for new messages from proxy
        let received_messages = app.check_for_new_messages();

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
            app.notice = Some(match result {
                Ok(()) => "Request sent".to_string(),
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
            handle_control_command(
                &mut app,
                command,
                &mut runtime.proxy_server,
                &runtime.message_sender,
                &runtime.proxy_state,
                &runtime.request_result_sender,
            )
            .await;
            received_control_command = true;
        }
        resolve_change_waiters(&app, &mut runtime.change_waiters);

        if should_draw
            || received_messages
            || received_pending_request
            || received_request_result
            || received_control_command
        {
            terminal.draw(|f| ui::draw(f, &app))?;
            should_draw = false;
        }

        if event::poll(std::time::Duration::from_millis(50))? {
            should_draw = true;
            let input_event = event::read()?;
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
                )
                .await?;
                continue;
            }

            if let Event::Key(key) = input_event {
                // Handle input modes first
                match app.input_mode {
                    app::InputMode::FilteringRequests => {
                        match key.code {
                            KeyCode::Enter => {
                                app.apply_filter();
                            }
                            KeyCode::Esc => {
                                app.cancel_filtering();
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
                    app::InputMode::EditingTarget => {
                        match key.code {
                            KeyCode::Enter => {
                                app.confirm_target_edit();
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
                                terminal.clear()?;
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

                    app::InputMode::Normal => {
                        // Continue to normal key handling below
                    }
                }

                // Normal mode key handling
                match key.code {
                    KeyCode::Enter => {
                        if app.app_mode == AppMode::Normal {
                            copy_focused_panel(terminal, &app)?;
                        }
                    }
                    KeyCode::Char('q') => {
                        // Clean shutdown
                        if let Some(handle) = runtime.proxy_server.take() {
                            handle.abort();
                            // Give it a moment to clean up
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                        return Ok(());
                    }
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        // Clean shutdown
                        if let Some(handle) = runtime.proxy_server.take() {
                            handle.abort();
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                        return Ok(());
                    }
                    KeyCode::Up => match app.app_mode {
                        app::AppMode::Normal => {
                            if app.is_message_list_focused() {
                                app.select_previous();
                            } else {
                                scroll_focused_details(&mut app, -1, terminal.size()?);
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
                            } else if app.is_request_section_focused() {
                                if app.get_selected_exchange().is_some() {
                                    app.request_details_scroll += 1; // Allow unlimited scrolling, UI will clamp
                                }
                            } else if app.is_response_section_focused()
                                && app.get_selected_exchange().is_some()
                            {
                                app.response_details_scroll += 1; // Allow unlimited scrolling, UI will clamp
                            }
                        }
                        app::AppMode::Paused | app::AppMode::Intercepting => {
                            app.select_next_pending()
                        }
                    },
                    KeyCode::Left => {
                        if app.is_status_focused() {
                            let desired_running = !app.is_running;
                            if set_proxy_running(
                                &mut app,
                                desired_running,
                                &mut runtime.proxy_server,
                                &runtime.message_sender,
                                &runtime.proxy_state,
                            )
                            .await
                            {
                                terminal.clear()?;
                                terminal.draw(|f| ui::draw(f, &app))?;
                            }
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
                            if set_proxy_running(
                                &mut app,
                                desired_running,
                                &mut runtime.proxy_server,
                                &runtime.message_sender,
                                &runtime.proxy_state,
                            )
                            .await
                            {
                                terminal.clear()?;
                                terminal.draw(|f| ui::draw(f, &app))?;
                            }
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
                            } else if app.is_request_section_focused() {
                                if app.request_details_scroll > 0 {
                                    app.request_details_scroll -= 1;
                                }
                            } else if app.is_response_section_focused()
                                && app.response_details_scroll > 0
                            {
                                app.response_details_scroll -= 1;
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
                                    scroll_focused_details(&mut app, 1, terminal.size()?);
                                }
                            }
                            app::AppMode::Paused | app::AppMode::Intercepting => {
                                app.intercept_details_scroll += 1; // Allow unlimited scrolling, UI will clamp
                            }
                        }
                    }
                    KeyCode::Char('u') => match app.app_mode {
                        app::AppMode::Normal => {
                            scroll_focused_details(&mut app, -10, terminal.size()?);
                        }
                        app::AppMode::Paused | app::AppMode::Intercepting => {
                            app.page_up_intercept_details()
                        }
                    },
                    KeyCode::Char('d') => match app.app_mode {
                        app::AppMode::Normal => {
                            scroll_focused_details(&mut app, 10, terminal.size()?);
                        }
                        app::AppMode::Paused | app::AppMode::Intercepting => {
                            app.page_down_intercept_details();
                        }
                    },
                    KeyCode::Char('G') => {
                        match app.app_mode {
                            app::AppMode::Normal => {
                                scroll_focused_details(&mut app, i64::MAX, terminal.size()?);
                            }
                            app::AppMode::Paused | app::AppMode::Intercepting => {
                                // For intercept mode, use a large number as max_lines
                                app.goto_bottom_intercept_details(1000, 20);
                            }
                        }
                    }
                    KeyCode::Char('g') => match app.app_mode {
                        app::AppMode::Normal => {
                            scroll_focused_details(&mut app, i64::MIN, terminal.size()?);
                        }
                        app::AppMode::Paused | app::AppMode::Intercepting => {
                            app.goto_top_intercept_details()
                        }
                    },
                    KeyCode::Char('t') => {
                        app.start_editing_target();
                    }
                    KeyCode::Char('/') => {
                        app.start_filtering_requests();
                    }
                    KeyCode::Char('n') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        match app.app_mode {
                            app::AppMode::Normal => {
                                if app.is_message_list_focused() {
                                    app.select_next();
                                } else {
                                    scroll_focused_details(&mut app, 1, terminal.size()?);
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
                                    scroll_focused_details(&mut app, -1, terminal.size()?);
                                }
                            }
                            app::AppMode::Paused | app::AppMode::Intercepting => {
                                app.select_previous_pending()
                            }
                        }
                    }
                    KeyCode::Char('s') => {
                        let desired_running = !app.is_running;
                        if set_proxy_running(
                            &mut app,
                            desired_running,
                            &mut runtime.proxy_server,
                            &runtime.message_sender,
                            &runtime.proxy_state,
                        )
                        .await
                        {
                            terminal.clear()?;
                            terminal.draw(|f| ui::draw(f, &app))?;
                        }
                    }
                    // Pause/Intercept key bindings
                    KeyCode::Char('p') => {
                        app.toggle_pause_mode();
                        terminal.clear()?;
                    }
                    KeyCode::Char('a') => {
                        // Allow selected pending request
                        app.allow_selected_request();
                    }
                    KeyCode::Char('e') => {
                        if let Some(content) = app.get_pending_request_json() {
                            app.open_editor(EditorTarget::PendingRequest, content);
                        }
                    }
                    KeyCode::Char('h') => {
                        if app.is_status_focused() && app.app_mode == app::AppMode::Normal {
                            let desired_running = !app.is_running;
                            if set_proxy_running(
                                &mut app,
                                desired_running,
                                &mut runtime.proxy_server,
                                &runtime.message_sender,
                                &runtime.proxy_state,
                            )
                            .await
                            {
                                terminal.clear()?;
                                terminal.draw(|f| ui::draw(f, &app))?;
                            }
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
                    KeyCode::Char('c') => {
                        if (app.app_mode == AppMode::Paused
                            || app.app_mode == AppMode::Intercepting)
                            && !app.pending_requests.is_empty()
                        {
                            if let Some(content) = app.get_pending_response_template() {
                                app.open_editor(EditorTarget::PendingResponse, content);
                            }
                        } else {
                            let content = r#"{
  "jsonrpc": "2.0",
  "method": "your_method",
  "params": [],
  "id": 1
}"#
                            .to_string();
                            app.open_editor(EditorTarget::NewRequest, content);
                        }
                    }
                    KeyCode::Char('b') => {
                        // Block selected pending request
                        app.block_selected_request();
                    }
                    KeyCode::Char('r') => {
                        // Resume all pending requests
                        app.resume_all_requests();
                        terminal.clear()?;
                    }
                    KeyCode::Char('l') => {
                        if app.is_status_focused() && app.app_mode == app::AppMode::Normal {
                            let desired_running = !app.is_running;
                            if set_proxy_running(
                                &mut app,
                                desired_running,
                                &mut runtime.proxy_server,
                                &runtime.message_sender,
                                &runtime.proxy_state,
                            )
                            .await
                            {
                                terminal.clear()?;
                                terminal.draw(|f| ui::draw(f, &app))?;
                            }
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
            app.notice = Some(format!("Error: {error}"));
            should_draw = true;
        }
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
) -> Result<()> {
    let area = terminal.size()?;
    let hovered_focus = ui::panel_focus(area, app, mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Moved => {
            if let Some(focus) = hovered_focus {
                app.set_focus(focus);
            }
            return Ok(());
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
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

    match action {
        ui::MouseAction::EditTarget => app.start_editing_target(),
        ui::MouseAction::EditFilter => app.start_filtering_requests(),
        ui::MouseAction::SetProxyRunning(should_run) => {
            app.set_focus(app::Focus::StatusHeader);
            if set_proxy_running(app, should_run, proxy_server, message_sender, proxy_state).await {
                terminal.clear()?;
            }
        }
        ui::MouseAction::SelectExchange(index) => {
            app.set_focus(app::Focus::MessageList);
            app.select_exchange(index);
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
            app.clear_line_selection();
            app.mark_changed();
        }
        ui::MouseAction::SelectResponseTab(tab) => {
            app.set_focus(app::Focus::ResponseSection);
            app.response_tab = tab;
            app.response_details_scroll = 0;
            app.clear_line_selection();
            app.mark_changed();
        }
        ui::MouseAction::SelectLine { panel, line } => {
            let extend = mouse.modifiers.contains(KeyModifiers::SHIFT);
            let (anchor, start_line, end_line) = app.line_selection_range(panel, line, extend);
            let Some(text) = ui::detail_line_text(app, panel, start_line, end_line) else {
                return Ok(());
            };
            app.select_lines_from_anchor(panel, anchor, start_line, end_line, text);
        }
        ui::MouseAction::Focus(focus) => app.set_focus(focus),
    }

    Ok(())
}

fn scroll_panel(app: &mut App, focus: app::Focus, down: bool, visible_lines: usize) {
    const LINES_PER_TICK: usize = 3;

    if focus == app::Focus::MessageList {
        for _ in 0..LINES_PER_TICK {
            match (app.app_mode, down) {
                (AppMode::Normal, true) => app.select_next(),
                (AppMode::Normal, false) => app.select_previous(),
                (AppMode::Paused | AppMode::Intercepting, true) => app.select_next_pending(),
                (AppMode::Paused | AppMode::Intercepting, false) => app.select_previous_pending(),
            }
        }
        return;
    }

    let max_scroll = match (app.app_mode, focus) {
        (AppMode::Normal, app::Focus::RequestSection) => app
            .get_request_details_content_lines()
            .saturating_sub(visible_lines),
        (AppMode::Normal, app::Focus::ResponseSection) => app
            .get_response_details_content_lines()
            .saturating_sub(visible_lines),
        (AppMode::Paused | AppMode::Intercepting, app::Focus::RequestSection) => app
            .get_intercept_details_content_lines()
            .saturating_sub(visible_lines),
        _ => return,
    };

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
    if previous_scroll != *scroll {
        app.mark_changed();
    }
}

fn scroll_focused_details(app: &mut App, lines: i64, area: ratatui::layout::Rect) {
    let focus = app.focus;
    let Some(total_lines) = ui::detail_line_count(app, focus) else {
        return;
    };
    let visible_lines = ui::panel_visible_lines(area, app, focus);
    let scroll_positions = total_lines.saturating_sub(visible_lines).saturating_add(1);

    app.scroll_panel_lines(focus, lines, scroll_positions);
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

    let server = ProxyServer::new(
        app.proxy_config.listen_port,
        app.proxy_config.target_url.clone(),
        message_sender.clone(),
    )
    .with_state(proxy_state.clone());
    *proxy_server = Some(tokio::spawn(async move {
        let _ = server.start().await;
    }));
}

async fn set_proxy_running(
    app: &mut App,
    should_run: bool,
    proxy_server: &mut Option<JoinHandle<()>>,
    message_sender: &mpsc::UnboundedSender<app::JsonRpcMessage>,
    proxy_state: &ProxyState,
) -> bool {
    if should_run == app.is_running {
        return false;
    }

    if should_run {
        app.toggle_proxy();

        let listen_port = app.proxy_config.listen_port;
        let target_url = app.proxy_config.target_url.clone();
        let sender_clone = message_sender.clone();
        let state_clone = proxy_state.clone();

        *proxy_server = Some(tokio::spawn(async move {
            let server =
                ProxyServer::new(listen_port, target_url, sender_clone).with_state(state_clone);
            if let Err(e) = server.start().await {
                eprintln!("Proxy server error: {}", e);
            }
        }));
    } else {
        if let Some(handle) = proxy_server.take() {
            handle.abort();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        app.toggle_proxy();
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
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
    fn keyboard_scroll_stops_at_the_visible_bottom() {
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

        scroll_focused_details(&mut app, i64::MAX, area);
        assert_eq!(app.request_details_scroll, bottom);

        scroll_focused_details(&mut app, -1, area);
        assert_eq!(app.request_details_scroll, bottom.saturating_sub(1));
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
        let (message_sender, _) = mpsc::unbounded_channel();
        let (pending_sender, _) = mpsc::unbounded_channel();
        let proxy_state = ProxyState {
            app_mode: Arc::new(Mutex::new(AppMode::Normal)),
            pending_sender,
        };
        let (notice_sender, _) = mpsc::unbounded_channel();
        let (reply, result) = tokio::sync::oneshot::channel();
        let mut proxy_server = None;

        handle_control_command(
            &mut app,
            ControlCommand {
                action: ControlAction::SetFocus {
                    focus: app::Focus::ResponseSection,
                },
                reply,
            },
            &mut proxy_server,
            &message_sender,
            &proxy_state,
            &notice_sender,
        )
        .await;

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
                },
                reply,
            },
            &mut proxy_server,
            &message_sender,
            &proxy_state,
            &notice_sender,
        )
        .await;

        let state = result.await.unwrap().unwrap();
        assert_eq!(state["lineSelection"]["text"], "Method: eth_call");
        assert_eq!(app.request_details_scroll, 1);
    }
}
