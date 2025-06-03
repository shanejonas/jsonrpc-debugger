use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, TransportType, InputMode, AppMode};

// Helper function to format JSON with syntax highlighting
fn format_json_with_highlighting(json_value: &serde_json::Value) -> Vec<Line<'static>> {
    let json_str = serde_json::to_string_pretty(json_value)
        .unwrap_or_else(|_| "Failed to format JSON".to_string());
    
    // Sanitize the JSON string to prevent UI corruption
    let sanitized_json = json_str
        .chars()
        .filter(|c| c.is_ascii() && (!c.is_control() || *c == '\n' || *c == '\t' || *c == '\r'))
        .collect::<String>();
    
    let mut lines = Vec::new();
    
    for (line_num, line) in sanitized_json.lines().enumerate() {
        // Limit total lines to prevent UI issues
        if line_num > 1000 {
            lines.push(Line::from(Span::styled("... (content truncated)", Style::default().fg(Color::Gray))));
            break;
        }
        
        let mut spans = Vec::new();
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        
        // Add indentation
        if indent > 0 {
            spans.push(Span::raw(" ".repeat(indent)));
        }
        
        // Parse the line for syntax highlighting
        let mut chars = trimmed.chars().peekable();
        let mut current_token = String::new();

        
        while let Some(ch) = chars.next() {
            match ch {
                '"' => {
                    if !current_token.is_empty() {
                        spans.push(Span::raw(current_token.clone()));
                        current_token.clear();
                    }
                    
                    // Collect the entire string
                    let mut string_content = String::from("\"");
                    while let Some(string_ch) = chars.next() {
                        string_content.push(string_ch);
                        if string_ch == '"' && !string_content.ends_with("\\\"") {
                            break;
                        }
                    }
                    
                    // Check if this is a key (followed by colon)
                    let mut peek_chars = chars.clone();
                    let mut found_colon = false;
                    while let Some(peek_ch) = peek_chars.next() {
                        if peek_ch == ':' {
                            found_colon = true;
                            break;
                        } else if !peek_ch.is_whitespace() {
                            break;
                        }
                    }
                    
                    if found_colon {
                        // This is a key
                        spans.push(Span::styled(string_content, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)));
                    } else {
                        // This is a string value
                        spans.push(Span::styled(string_content, Style::default().fg(Color::Green)));
                    }
                }
                ':' => {
                    if !current_token.is_empty() {
                        spans.push(Span::raw(current_token.clone()));
                        current_token.clear();
                    }
                    spans.push(Span::styled(":", Style::default().fg(Color::White)));
                }
                ',' => {
                    if !current_token.is_empty() {
                        spans.push(Span::raw(current_token.clone()));
                        current_token.clear();
                    }
                    spans.push(Span::styled(",", Style::default().fg(Color::White)));
                }
                '{' | '}' | '[' | ']' => {
                    if !current_token.is_empty() {
                        spans.push(Span::raw(current_token.clone()));
                        current_token.clear();
                    }
                    spans.push(Span::styled(ch.to_string(), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
                }
                _ => {
                    current_token.push(ch);
                }
            }
        }
        
        // Handle any remaining token
        if !current_token.is_empty() {
            let trimmed_token = current_token.trim();
            if trimmed_token == "true" || trimmed_token == "false" {
                spans.push(Span::styled(current_token, Style::default().fg(Color::Magenta)));
            } else if trimmed_token == "null" {
                spans.push(Span::styled(current_token, Style::default().fg(Color::Red)));
            } else if trimmed_token.parse::<f64>().is_ok() {
                spans.push(Span::styled(current_token, Style::default().fg(Color::Blue)));
            } else {
                spans.push(Span::raw(current_token));
            }
        }
        
        lines.push(Line::from(spans));
    }
    
    lines
}

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Header
            Constraint::Min(10),    // Main content
            Constraint::Length(3),  // Footer
        ])
        .split(f.size());

    draw_header(f, chunks[0], app);
    
    // Choose layout based on app mode
    match app.app_mode {
        AppMode::Normal => {
            draw_main_content(f, chunks[1], app);
        }
        AppMode::Paused | AppMode::Intercepting => {
            draw_intercept_content(f, chunks[1], app);
        }
    }
    
    draw_footer(f, chunks[2], app);

    // Draw input dialogs
    match app.input_mode {
        InputMode::EditingTarget => draw_input_dialog(f, app),

        _ => {}
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let status = if app.is_running { "RUNNING" } else { "STOPPED" };
    let status_color = if app.is_running { Color::Green } else { Color::Red };
    
    let mode_text = match app.app_mode {
        AppMode::Normal => String::new(),
        AppMode::Paused => " | Mode: PAUSED".to_string(),
        AppMode::Intercepting => format!(" | Mode: INTERCEPTING ({} pending)", app.pending_requests.len()),
    };
    let mode_color = match app.app_mode {
        AppMode::Normal => Color::White,
        AppMode::Paused => Color::Yellow,
        AppMode::Intercepting => Color::Red,
    };
    
    let header_text = vec![
        Line::from(vec![
            Span::raw("JSON-RPC Proxy TUI | Status: "),
            Span::styled(status, Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
            Span::raw(format!(" | Port: {} | Target: {}", 
                app.proxy_config.listen_port, 
                app.proxy_config.target_url
            )),
            Span::styled(mode_text, Style::default().fg(mode_color).add_modifier(Modifier::BOLD)),
        ])
    ];

    let header = Paragraph::new(header_text)
        .block(Block::default().borders(Borders::ALL).title("Status"));
    
    f.render_widget(header, area);
}

fn draw_main_content(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(50), // Message list
            Constraint::Percentage(50), // Message details
        ])
        .split(area);

    draw_message_list(f, chunks[0], app);
    draw_message_details(f, chunks[1], app);
}

fn draw_message_list(f: &mut Frame, area: Rect, app: &App) {
    if app.exchanges.is_empty() {
        let empty_message = if app.is_running {
            format!("Proxy is running on port {}. Waiting for JSON-RPC requests...", app.proxy_config.listen_port)
        } else {
            "Press 's' to start the proxy and begin capturing messages".to_string()
        };
        
        let paragraph = Paragraph::new(empty_message.as_str())
            .block(Block::default().borders(Borders::ALL).title("JSON-RPC Exchanges"))
            .style(Style::default().fg(Color::Gray))
            .wrap(Wrap { trim: true });
        
        f.render_widget(paragraph, area);
        return;
    }

    let exchanges: Vec<ListItem> = app
        .exchanges
        .iter()
        .enumerate()
        .map(|(i, exchange)| {
            let transport_symbol = match exchange.transport {
                TransportType::Http => "HTTP",
                TransportType::WebSocket => "WS",
            };

            let method = exchange.method.as_deref().unwrap_or("unknown");
            let id = exchange.id.as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".to_string());

            // Determine status
            let (status_symbol, status_color) = if exchange.response.is_none() {
                ("⏳", Color::Yellow) // Pending
            } else if let Some(response) = &exchange.response {
                if response.error.is_some() {
                    ("✗", Color::Red) // Error
                } else {
                    ("✓", Color::Green) // Success
                }
            } else {
                ("?", Color::Gray) // Unknown
            };

            let style = if i == app.selected_exchange {
                Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            ListItem::new(Line::from(vec![
                Span::styled("→ ", Style::default().fg(Color::Yellow)),
                Span::styled(format!("[{}] ", transport_symbol), Style::default().fg(Color::Blue)),
                Span::styled(format!("{} ", method), Style::default().fg(Color::Red)),
                Span::styled(format!("(id: {}) ", id), Style::default().fg(Color::Gray)),
                Span::styled(status_symbol, Style::default().fg(status_color)),
            ])).style(style)
        })
        .collect();

    let exchanges_list = List::new(exchanges)
        .block(Block::default().borders(Borders::ALL).title("JSON-RPC Exchanges"))
        .highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD));

    f.render_widget(exchanges_list, area);
}

fn draw_message_details(f: &mut Frame, area: Rect, app: &App) {
    let content = if let Some(exchange) = app.get_selected_exchange() {
        let mut lines = Vec::new();
        
        // Basic exchange info
        lines.push(Line::from(vec![
            Span::styled("Transport: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{:?}", exchange.transport)),
        ]));

        if let Some(method) = &exchange.method {
            lines.push(Line::from(vec![
                Span::styled("Method: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(method.clone()),
            ]));
        }

        if let Some(id) = &exchange.id {
            lines.push(Line::from(vec![
                Span::styled("ID: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(id.to_string()),
            ]));
        }

        // Request details
        if let Some(request) = &exchange.request {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("REQUEST:", Style::default().add_modifier(Modifier::BOLD).fg(Color::Green))));
            
            // Show HTTP headers if available
            if let Some(headers) = &request.headers {
                lines.push(Line::from(""));
                lines.push(Line::from("HTTP Headers:"));
                for (key, value) in headers {
                    lines.push(Line::from(format!("  {}: {}", key, value)));
                }
            }
            
            // Build and show the complete JSON-RPC request object
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("JSON-RPC Request:", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))));
            lines.push(Line::from(""));
            let mut request_json = serde_json::Map::new();
            request_json.insert("jsonrpc".to_string(), serde_json::Value::String("2.0".to_string()));
            
            if let Some(id) = &request.id {
                request_json.insert("id".to_string(), id.clone());
            }
            if let Some(method) = &request.method {
                request_json.insert("method".to_string(), serde_json::Value::String(method.clone()));
            }
            if let Some(params) = &request.params {
                request_json.insert("params".to_string(), params.clone());
            }
            
            let request_json_value = serde_json::Value::Object(request_json);
            let request_json_lines = format_json_with_highlighting(&request_json_value);
            for line in request_json_lines {
                lines.push(line);
            }
        }

        // Response details
        if let Some(response) = &exchange.response {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("RESPONSE:", Style::default().add_modifier(Modifier::BOLD).fg(Color::Blue))));
            
            // Show HTTP headers if available
            if let Some(headers) = &response.headers {
                lines.push(Line::from(""));
                lines.push(Line::from("HTTP Headers:"));
                for (key, value) in headers {
                    lines.push(Line::from(format!("  {}: {}", key, value)));
                }
            }
            
            // Build and show the complete JSON-RPC response object
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("JSON-RPC Response:", Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD))));
            lines.push(Line::from(""));
            let mut response_json = serde_json::Map::new();
            response_json.insert("jsonrpc".to_string(), serde_json::Value::String("2.0".to_string()));
            
            if let Some(id) = &response.id {
                response_json.insert("id".to_string(), id.clone());
            }
            if let Some(result) = &response.result {
                response_json.insert("result".to_string(), result.clone());
            }
            if let Some(error) = &response.error {
                response_json.insert("error".to_string(), error.clone());
            }
            
            let response_json_value = serde_json::Value::Object(response_json);
            let response_json_lines = format_json_with_highlighting(&response_json_value);
            for line in response_json_lines {
                lines.push(line);
            }
        } else {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("RESPONSE: Pending...", Style::default().add_modifier(Modifier::BOLD).fg(Color::Yellow))));
        }

        lines
    } else {
        vec![Line::from("No exchange selected")]
    };

    // Calculate visible area for scrolling
    let inner_area = area.inner(&Margin { vertical: 1, horizontal: 1 });
    let visible_lines = inner_area.height as usize;
    let total_lines = content.len();
    
    // Apply scrolling offset
    let start_line = app.details_scroll;
    let end_line = std::cmp::min(start_line + visible_lines, total_lines);
    let visible_content = if start_line < total_lines {
        content[start_line..end_line].to_vec()
    } else {
        vec![]
    };

    // Create title with scroll indicator
    let scroll_info = if total_lines > visible_lines {
        let progress = ((app.details_scroll as f32 / (total_lines - visible_lines) as f32) * 100.0) as u8;
        format!("Exchange Details ({}% - vim: j/k/d/u/G/g)", progress)
    } else {
        "Exchange Details".to_string()
    };

    let details = Paragraph::new(visible_content)
        .block(Block::default().borders(Borders::ALL).title(scroll_info))
        .wrap(Wrap { trim: true });

    f.render_widget(details, area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let mut footer_spans = vec![
        Span::styled("q", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(" quit | "),
        Span::styled("↑↓", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw("/"),
        Span::styled("^n/^p", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(" navigate | "),
        Span::styled("j/k/d/u/G/g", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(" scroll details | "),
        Span::styled("s", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(" start/stop proxy | "),
        Span::styled("t", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(" edit target | "),
        Span::styled("p", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(" pause"),
    ];

    // Show context-specific controls
    match app.app_mode {
        AppMode::Paused | AppMode::Intercepting => {
            footer_spans.extend(vec![
                Span::raw(" | "),
                Span::styled("a/e/h/c/b/r", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::raw(" allow/edit/headers/complete/block/resume"),
            ]);
        }
        AppMode::Normal => {
            footer_spans.extend(vec![
                Span::raw(" | "),
                Span::styled("c", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::raw(" create request"),
            ]);
        }
    }

    let footer_text = vec![Line::from(footer_spans)];

    let footer = Paragraph::new(footer_text)
        .block(Block::default().borders(Borders::ALL).title("Controls"));
    
    f.render_widget(footer, area);
}

fn draw_input_dialog(f: &mut Frame, app: &App) {
    let area = f.size();
    
    // Create a centered popup
    let popup_area = Rect {
        x: area.width / 4,
        y: area.height / 2 - 3,
        width: area.width / 2,
        height: 7,
    };

    // Clear the entire screen first
    f.render_widget(Clear, area);
    
    // Render a black background
    let background = Block::default()
        .style(Style::default().bg(Color::Black));
    f.render_widget(background, area);

    // Clear the popup area specifically
    f.render_widget(Clear, popup_area);

    let input_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("Target URL: "),
            Span::styled(&app.input_buffer, Style::default().fg(Color::Green)),
        ]),
        Line::from(""),
        Line::from(Span::styled("Press Enter to confirm, Esc to cancel", Style::default().fg(Color::Gray))),
        Line::from(""),
    ];

    let input_dialog = Paragraph::new(input_text)
        .block(Block::default()
            .borders(Borders::ALL)
            .title("Edit Target URL")
            .style(Style::default().fg(Color::White).bg(Color::DarkGray)))
        .wrap(Wrap { trim: true });

    f.render_widget(input_dialog, popup_area);
}

fn draw_intercept_content(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(50), // Pending requests list
            Constraint::Percentage(50), // Request details/editor
        ])
        .split(area);

    draw_pending_requests(f, chunks[0], app);
    draw_request_details(f, chunks[1], app);
}

fn draw_pending_requests(f: &mut Frame, area: Rect, app: &App) {
    if app.pending_requests.is_empty() {
        let mode_text = match app.app_mode {
            AppMode::Paused => "Pause mode active. New requests will be intercepted.",
            _ => "No pending requests.",
        };
        
        let paragraph = Paragraph::new(mode_text)
            .block(Block::default().borders(Borders::ALL).title("Pending Requests"))
            .style(Style::default().fg(Color::Yellow))
            .wrap(Wrap { trim: true });
        
        f.render_widget(paragraph, area);
        return;
    }

    let requests: Vec<ListItem> = app
        .pending_requests
        .iter()
        .enumerate()
        .map(|(i, pending)| {
            let method = pending.original_request.method.as_deref().unwrap_or("unknown");
            let id = pending.original_request.id.as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".to_string());

            let style = if i == app.selected_pending {
                Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            // Show different icon if request has been modified
            let (icon, icon_color) = if pending.modified_request.is_some() || pending.modified_headers.is_some() {
                ("✏ ", Color::Blue) // Modified
            } else {
                ("⏸ ", Color::Red)  // Paused/Intercepted
            };

            let mut modification_labels = Vec::new();
            if pending.modified_request.is_some() {
                modification_labels.push("BODY");
            }
            if pending.modified_headers.is_some() {
                modification_labels.push("HEADERS");
            }
            let modification_text = if !modification_labels.is_empty() {
                format!(" [{}]", modification_labels.join("+"))
            } else {
                String::new()
            };

            ListItem::new(Line::from(vec![
                Span::styled(icon, Style::default().fg(icon_color)),
                Span::styled(format!("{} ", method), Style::default().fg(Color::Red)),
                Span::styled(format!("(id: {})", id), Style::default().fg(Color::Gray)),
                if !modification_text.is_empty() {
                    Span::styled(modification_text, Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD))
                } else {
                    Span::raw("")
                },
            ])).style(style)
        })
        .collect();

    let requests_list = List::new(requests)
        .block(Block::default().borders(Borders::ALL).title(format!("Pending Requests ({})", app.pending_requests.len())))
        .highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD));

    f.render_widget(requests_list, area);
}

fn draw_request_details(f: &mut Frame, area: Rect, app: &App) {
    let content = if let Some(pending) = app.get_selected_pending() {
        let mut lines = Vec::new();
        
        if pending.modified_request.is_some() || pending.modified_headers.is_some() {
            lines.push(Line::from(Span::styled("MODIFIED REQUEST:", Style::default().add_modifier(Modifier::BOLD).fg(Color::Blue))));
        } else {
            lines.push(Line::from(Span::styled("INTERCEPTED REQUEST:", Style::default().add_modifier(Modifier::BOLD).fg(Color::Red))));
        }
        lines.push(Line::from(""));
        
        // Show headers section
        lines.push(Line::from(Span::styled("HTTP Headers:", Style::default().add_modifier(Modifier::BOLD).fg(Color::Green))));
        let headers_to_show = pending.modified_headers.as_ref()
            .or(pending.original_request.headers.as_ref());
        
        if let Some(headers) = headers_to_show {
            for (key, value) in headers {
                lines.push(Line::from(format!("  {}: {}", key, value)));
            }
            if pending.modified_headers.is_some() {
                lines.push(Line::from(Span::styled("  [Headers have been modified]", Style::default().fg(Color::Blue).add_modifier(Modifier::ITALIC))));
            }
        } else {
            lines.push(Line::from("  No headers"));
        }
        lines.push(Line::from(""));
        
        // Show JSON-RPC body section
        lines.push(Line::from(Span::styled("JSON-RPC Request:", Style::default().add_modifier(Modifier::BOLD).fg(Color::Green))));
        
        // Show the modified request if available, otherwise show original
        let json_to_show = if let Some(ref modified_json) = pending.modified_request {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(modified_json) {
                parsed
            } else {
                // Fallback to original if modified JSON is invalid
                let mut request_json = serde_json::Map::new();
                request_json.insert("jsonrpc".to_string(), serde_json::Value::String("2.0".to_string()));
                
                if let Some(id) = &pending.original_request.id {
                    request_json.insert("id".to_string(), id.clone());
                }
                if let Some(method) = &pending.original_request.method {
                    request_json.insert("method".to_string(), serde_json::Value::String(method.clone()));
                }
                if let Some(params) = &pending.original_request.params {
                    request_json.insert("params".to_string(), params.clone());
                }
                
                serde_json::Value::Object(request_json)
            }
        } else {
            // Show original request
            let mut request_json = serde_json::Map::new();
            request_json.insert("jsonrpc".to_string(), serde_json::Value::String("2.0".to_string()));
            
            if let Some(id) = &pending.original_request.id {
                request_json.insert("id".to_string(), id.clone());
            }
            if let Some(method) = &pending.original_request.method {
                request_json.insert("method".to_string(), serde_json::Value::String(method.clone()));
            }
            if let Some(params) = &pending.original_request.params {
                request_json.insert("params".to_string(), params.clone());
            }
            
            serde_json::Value::Object(request_json)
        };
        
        let request_json_lines = format_json_with_highlighting(&json_to_show);
        for line in request_json_lines {
            lines.push(line);
        }
        
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Actions:", Style::default().add_modifier(Modifier::BOLD))));
        lines.push(Line::from("• Press 'a' to Allow request"));
        lines.push(Line::from("• Press 'e' to Edit request body"));
        lines.push(Line::from("• Press 'h' to Edit headers"));
        lines.push(Line::from("• Press 'c' to Complete with custom response"));
        lines.push(Line::from("• Press 'b' to Block request"));
        lines.push(Line::from("• Press 'r' to Resume all requests"));

        lines
    } else {
        vec![Line::from("No request selected")]
    };

    let details = Paragraph::new(content)
        .block(Block::default().borders(Borders::ALL).title("Request Details"))
        .wrap(Wrap { trim: true });

    f.render_widget(details, area);
}

 