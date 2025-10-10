use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::io::Write;
use std::process::Command;
use std::sync::{Arc, Mutex};
use tempfile::NamedTempFile;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

mod app;
mod proxy;
mod ui;

use app::{App, AppMode};
use proxy::{ProxyServer, ProxyState};

#[derive(Parser)]
#[command(name = "jsonrpc-debugger")]
#[command(about = "A JSON-RPC debugger TUI for intercepting and inspecting requests")]
struct Cli {
    /// Port to listen on for incoming requests
    #[arg(short, long, default_value = "8080")]
    port: u16,

    /// Target URL to proxy requests to
    #[arg(short, long)]
    target: Option<String>,
}

// Function to launch external editor
fn launch_external_editor(content: &str) -> Result<String> {
    // Create a temporary file
    let mut temp_file = NamedTempFile::new()?;
    temp_file.write_all(content.as_bytes())?;
    let temp_path = temp_file.path().to_string_lossy().to_string();

    // Get the editor from environment, fallback to vim, then nano
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| {
            if Command::new("vim").arg("--version").output().is_ok() {
                "vim".to_string()
            } else if Command::new("nano").arg("--version").output().is_ok() {
                "nano".to_string()
            } else {
                "vi".to_string() // Last resort
            }
        });

    // Launch the editor
    let status = Command::new(&editor).arg(&temp_path).status()?;

    if !status.success() {
        return Err(anyhow::anyhow!("Editor exited with non-zero status"));
    }

    // Read the modified content
    let modified_content = std::fs::read_to_string(&temp_path)?;

    Ok(modified_content)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse command line arguments
    let cli = Cli::parse();

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

    let res = run_app(
        &mut terminal,
        app,
        message_sender,
        shared_app_mode,
        pending_receiver,
        proxy_state,
        Some(initial_proxy_handle),
    )
    .await;

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
    message_sender: mpsc::UnboundedSender<app::JsonRpcMessage>,
    shared_app_mode: Arc<Mutex<AppMode>>,
    mut pending_receiver: mpsc::UnboundedReceiver<app::PendingRequest>,
    proxy_state: ProxyState,
    initial_proxy_handle: Option<JoinHandle<()>>,
) -> Result<()> {
    let mut proxy_server: Option<JoinHandle<()>> = initial_proxy_handle;

    loop {
        // Check for new messages from proxy
        app.check_for_new_messages();

        // Sync app mode with shared state
        if let Ok(mut shared_mode) = shared_app_mode.try_lock() {
            *shared_mode = app.app_mode.clone();
        }

        // Check for new pending requests
        while let Ok(pending_request) = pending_receiver.try_recv() {
            app.pending_requests.push(pending_request);
        }

        // Force a redraw to ensure clean rendering
        terminal.draw(|f| ui::draw(f, &app))?;

        // Use timeout to avoid blocking indefinitely
        if let Ok(has_event) = tokio::time::timeout(std::time::Duration::from_millis(50), async {
            event::poll(std::time::Duration::from_millis(0))
        })
        .await
        {
            if has_event? {
                if let Event::Key(key) = event::read()? {
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
                                        if let Some(handle) = proxy_server.take() {
                                            handle.abort();
                                            tokio::time::sleep(std::time::Duration::from_millis(
                                                100,
                                            ))
                                            .await;
                                        }
                                        let server = ProxyServer::new(
                                            app.proxy_config.listen_port,
                                            app.proxy_config.target_url.clone(),
                                            message_sender.clone(),
                                        )
                                        .with_state(proxy_state.clone());
                                        proxy_server = Some(tokio::spawn(async move {
                                            if let Err(_e) = server.start().await {
                                                // Silent error handling
                                            }
                                        }));
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
                        KeyCode::Char('q') => {
                            // Clean shutdown
                            if let Some(handle) = proxy_server.take() {
                                handle.abort();
                                // Give it a moment to clean up
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            }
                            return Ok(());
                        }
                        KeyCode::Char('c')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            // Clean shutdown
                            if let Some(handle) = proxy_server.take() {
                                handle.abort();
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            }
                            return Ok(());
                        }
                        KeyCode::Up => match app.app_mode {
                            app::AppMode::Normal => {
                                if app.is_message_list_focused() {
                                    app.select_previous();
                                } else if app.is_request_section_focused() {
                                    if app.request_details_scroll > 0 {
                                        app.request_details_scroll -= 1;
                                    }
                                } else if app.is_response_section_focused() {
                                    if app.response_details_scroll > 0 {
                                        app.response_details_scroll -= 1;
                                    }
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
                                  } else if app.is_response_section_focused() {
                                    if app.get_selected_exchange().is_some() {
                                        app.response_details_scroll += 1; // Allow unlimited scrolling, UI will clamp
                                    }
                                }
                            }
                            app::AppMode::Paused | app::AppMode::Intercepting => {
                                app.select_next_pending()
                            }
                        },
                        KeyCode::Left => {
                            if app.app_mode == app::AppMode::Normal {
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
                            if app.app_mode == app::AppMode::Normal {
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
                                } else if app.is_response_section_focused() {
                                    if app.response_details_scroll > 0 {
                                        app.response_details_scroll -= 1;
                                    }
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
                                    } else if app.is_request_section_focused() {
                                        if app.get_selected_exchange().is_some() {
                                            app.request_details_scroll += 1; // Allow unlimited scrolling, UI will clamp
                                        }
                                    } else if app.is_response_section_focused() {
                                        if app.get_selected_exchange().is_some() {
                                            app.response_details_scroll += 1; // Allow unlimited scrolling, UI will clamp
                                        }
                                    }
                                }
                                app::AppMode::Paused | app::AppMode::Intercepting => {
                                    app.intercept_details_scroll += 1; // Allow unlimited scrolling, UI will clamp
                                }
                            }
                        },
                        KeyCode::Char('u') => match app.app_mode {
                            app::AppMode::Normal => {
                                if app.is_request_section_focused() {
                                    let page_size = 10;
                                    app.request_details_scroll = app.request_details_scroll.saturating_sub(page_size);
                                } else if app.is_response_section_focused() {
                                    let page_size = 10;
                                    app.response_details_scroll = app.response_details_scroll.saturating_sub(page_size);
                                }
                                // u does nothing when message list is focused
                            }
                            app::AppMode::Paused | app::AppMode::Intercepting => {
                                app.page_up_intercept_details()
                            }
                        },
                        KeyCode::Char('d') => match app.app_mode {
                            app::AppMode::Normal => {
                                if app.is_request_section_focused() {
                                    let page_size = 10;
                                    app.request_details_scroll += page_size;
                                } else if app.is_response_section_focused() {
                                    let page_size = 10;
                                    app.response_details_scroll += page_size;
                                }
                                // d does nothing when message list is focused
                            }
                            app::AppMode::Paused | app::AppMode::Intercepting => {
                                app.page_down_intercept_details();
                            }
                        },
                        KeyCode::Char('G') => {
                            match app.app_mode {
                                app::AppMode::Normal => {
                                    if app.is_request_section_focused() {
                                        if app.get_selected_exchange().is_some() {
                                             app.request_details_scroll = 10000; // Large number, UI will clamp to actual bottom
                                        }
                                    } else if app.is_response_section_focused() {
                                        if app.get_selected_exchange().is_some() {
                                             app.response_details_scroll = 10000; // Large number, UI will clamp to actual bottom
                                        }
                                    }
                                    // G does nothing when message list is focused
                                }
                                app::AppMode::Paused | app::AppMode::Intercepting => {
                                    // For intercept mode, use a large number as max_lines
                                    app.goto_bottom_intercept_details(1000, 20);
                                }
                            }
                        }
                        KeyCode::Char('g') => match app.app_mode {
                            app::AppMode::Normal => {
                                if app.is_request_section_focused() {
                                    app.request_details_scroll = 0;
                                } else if app.is_response_section_focused() {
                                    app.response_details_scroll = 0;
                                }
                                // g does nothing when message list is focused
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
                        KeyCode::Char('n')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            match app.app_mode {
                                app::AppMode::Normal => {
                                    if app.is_message_list_focused() {
                                        app.select_next();
                                    } else if app.is_request_section_focused() {
                                        // Request details - scroll down
                                        if app.get_selected_exchange().is_some() {
                                             app.request_details_scroll += 1; // Allow unlimited scrolling, UI will clamp
                                        }
                                    } else if app.is_response_section_focused() {
                                        // Response details - scroll down
                                        if app.get_selected_exchange().is_some() {
                                             app.response_details_scroll += 1; // Allow unlimited scrolling, UI will clamp
                                        }
                                    }
                                }
                                app::AppMode::Paused | app::AppMode::Intercepting => {
                                    app.select_next_pending()
                                }
                            }
                        }
                        KeyCode::Char('p')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            match app.app_mode {
                                app::AppMode::Normal => {
                                    if app.is_message_list_focused() {
                                        app.select_previous();
                                    } else if app.is_request_section_focused() {
                                        // Request details - scroll up
                                        if app.request_details_scroll > 0 {
                                            app.request_details_scroll -= 1;
                                        }
                                    } else if app.is_response_section_focused() {
                                        // Response details - scroll up
                                        if app.response_details_scroll > 0 {
                                            app.response_details_scroll -= 1;
                                        }
                                    }
                                }
                                app::AppMode::Paused | app::AppMode::Intercepting => {
                                    app.select_previous_pending()
                                }
                            }
                        }
                        KeyCode::Char('s') => {
                            if app.is_running {
                                // Stop proxy server first
                                if let Some(handle) = proxy_server.take() {
                                    handle.abort();
                                    // Wait a bit for cleanup
                                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                                }
                                app.toggle_proxy();
                            } else {
                                // Start proxy server
                                app.toggle_proxy();
                                let server = ProxyServer::new(
                                    app.proxy_config.listen_port,
                                    app.proxy_config.target_url.clone(),
                                    message_sender.clone(),
                                )
                                .with_state(proxy_state.clone());
                                proxy_server = Some(tokio::spawn(async move {
                                    if let Err(e) = server.start().await {
                                        eprintln!("Proxy server error: {}", e);
                                    }
                                }));
                            }

                            // Clear and force a redraw after state change
                            terminal.clear()?;
                            terminal.draw(|f| ui::draw(f, &app))?;
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
                            // Edit selected pending request with external editor
                            if let Some(json_content) = app.get_pending_request_json() {
                                // Temporarily exit TUI mode
                                disable_raw_mode()?;
                                execute!(
                                    terminal.backend_mut(),
                                    LeaveAlternateScreen,
                                    DisableMouseCapture
                                )?;

                                // Launch external editor
                                match launch_external_editor(&json_content) {
                                    Ok(edited_content) => {
                                        // Apply the edited JSON
                                        if let Err(e) = app.apply_edited_json(edited_content) {
                                            println!("Error applying edited JSON: {}", e);
                                            println!("Press Enter to continue...");
                                            let _ = std::io::stdin().read_line(&mut String::new());
                                        }
                                    }
                                    Err(e) => {
                                        println!("Error launching editor: {}", e);
                                        println!("Press Enter to continue...");
                                        let _ = std::io::stdin().read_line(&mut String::new());
                                    }
                                }

                                // Re-enter TUI mode
                                enable_raw_mode()?;
                                execute!(
                                    terminal.backend_mut(),
                                    EnterAlternateScreen,
                                    EnableMouseCapture
                                )?;
                                terminal.clear()?;
                            }
                        }
                        KeyCode::Char('h') => {
                            // Edit selected pending request headers with external editor (intercept mode)
                            if (app.app_mode == app::AppMode::Paused || app.app_mode == app::AppMode::Intercepting)
                                && app.get_pending_request_headers().is_some() {
                                let headers_content = app.get_pending_request_headers().unwrap();
                                // Temporarily exit TUI mode
                                disable_raw_mode()?;
                                execute!(
                                    terminal.backend_mut(),
                                    LeaveAlternateScreen,
                                    DisableMouseCapture
                                )?;

                                // Launch external editor for headers
                                match launch_external_editor(&headers_content) {
                                    Ok(edited_content) => {
                                        // Apply the edited headers
                                        if let Err(e) = app.apply_edited_headers(edited_content) {
                                            println!("Error applying edited headers: {}", e);
                                            println!("Press Enter to continue...");
                                            let _ = std::io::stdin().read_line(&mut String::new());
                                        }
                                    }
                                    Err(e) => {
                                        println!("Error launching editor: {}", e);
                                        println!("Press Enter to continue...");
                                        let _ = std::io::stdin().read_line(&mut String::new());
                                    }
                                }

                                // Re-enter TUI mode
                                enable_raw_mode()?;
                                execute!(
                                    terminal.backend_mut(),
                                    EnterAlternateScreen,
                                    EnableMouseCapture
                                )?;
                                terminal.clear()?;
                            }
                            // Navigate tabs left in normal mode
                            if app.app_mode == app::AppMode::Normal
                                && (app.is_request_section_focused() || app.is_response_section_focused()) {
                                if app.is_request_section_focused() {
                                    app.previous_request_tab();
                                } else if app.is_response_section_focused() {
                                    app.previous_response_tab();
                                }
                            }
                        }
                        KeyCode::Char('c') => {
                            // Check if we have a selected pending request (in either Paused or Intercepting mode)
                            if (app.app_mode == AppMode::Paused
                                || app.app_mode == AppMode::Intercepting)
                                && !app.pending_requests.is_empty()
                            {
                                // Complete selected pending request with custom response
                                if let Some(response_template) = app.get_pending_response_template()
                                {
                                    // Temporarily exit TUI mode
                                    disable_raw_mode()?;
                                    execute!(
                                        terminal.backend_mut(),
                                        LeaveAlternateScreen,
                                        DisableMouseCapture
                                    )?;

                                    // Launch external editor for response
                                    match launch_external_editor(&response_template) {
                                        Ok(edited_content) => {
                                            // Complete the request with the custom response
                                            if let Err(e) =
                                                app.complete_selected_request(edited_content)
                                            {
                                                println!("Error completing request: {}", e);
                                                println!("Press Enter to continue...");
                                                let _ =
                                                    std::io::stdin().read_line(&mut String::new());
                                            }
                                        }
                                        Err(e) => {
                                            println!("Error launching editor: {}", e);
                                            println!("Press Enter to continue...");
                                            let _ = std::io::stdin().read_line(&mut String::new());
                                        }
                                    }

                                    // Re-enter TUI mode
                                    enable_raw_mode()?;
                                    execute!(
                                        terminal.backend_mut(),
                                        EnterAlternateScreen,
                                        EnableMouseCapture
                                    )?;
                                    terminal.clear()?;
                                }
                            } else {
                                // Create new request
                                let new_request_template = r#"{
  "jsonrpc": "2.0",
  "method": "your_method",
  "params": [],
  "id": 1
}"#;

                                // Temporarily exit TUI mode
                                disable_raw_mode()?;
                                execute!(
                                    terminal.backend_mut(),
                                    LeaveAlternateScreen,
                                    DisableMouseCapture
                                )?;

                                // Launch external editor for new request
                                match launch_external_editor(new_request_template) {
                                    Ok(edited_content) => {
                                        // Send the new request
                                        if let Err(e) = app.send_new_request(edited_content).await {
                                            println!("Error sending request: {}", e);
                                            println!("Press Enter to continue...");
                                            let _ = std::io::stdin().read_line(&mut String::new());
                                        }
                                    }
                                    Err(e) => {
                                        println!("Error launching editor: {}", e);
                                        println!("Press Enter to continue...");
                                        let _ = std::io::stdin().read_line(&mut String::new());
                                    }
                                }

                                // Re-enter TUI mode
                                enable_raw_mode()?;
                                execute!(
                                    terminal.backend_mut(),
                                    EnterAlternateScreen,
                                    EnableMouseCapture
                                )?;
                                terminal.clear()?;
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
                            if app.app_mode == app::AppMode::Normal
                                && (app.is_request_section_focused() || app.is_response_section_focused()) {
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
        }

        // Check if proxy server has died unexpectedly
        if let Some(handle) = &proxy_server {
            if handle.is_finished() {
                proxy_server = None;
                if app.is_running {
                    app.toggle_proxy(); // Mark as stopped
                }
            }
        }
    }
}
