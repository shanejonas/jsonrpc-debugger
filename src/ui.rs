use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table, TableState, Wrap,
    },
    Frame,
};

use crate::app::{App, AppMode, InputMode, TransportType};

// Helper function to format JSON with syntax highlighting and 2-space indentation
fn format_json_with_highlighting(json_value: &serde_json::Value) -> Vec<Line<'static>> {
    // Use the standard pretty formatter
    let json_str = match serde_json::to_string_pretty(json_value) {
        Ok(s) => s,
        Err(_) => return vec![Line::from("Failed to format JSON")],
    };

    let mut lines = Vec::new();

    for (line_num, line) in json_str.lines().enumerate() {
        // Limit total lines to prevent UI issues
        if line_num > 1000 {
            lines.push(Line::from(Span::styled(
                "... (content truncated)",
                Style::default().fg(Color::Gray),
            )));
            break;
        }

        // Don't trim the line - work with it as-is to preserve indentation
        let mut spans = Vec::new();
        let mut chars = line.chars().peekable();
        let mut current_token = String::new();

        while let Some(ch) = chars.next() {
            match ch {
                '"' => {
                    // Flush any accumulated token (including spaces)
                    if !current_token.is_empty() {
                        spans.push(Span::raw(current_token.clone()));
                        current_token.clear();
                    }

                    // Collect the entire string
                    let mut string_content = String::from("\"");
                    for string_ch in chars.by_ref() {
                        string_content.push(string_ch);
                        if string_ch == '"' && !string_content.ends_with("\\\"") {
                            break;
                        }
                    }

                    // Check if this is a key (followed by colon)
                    let peek_chars = chars.clone();
                    let mut found_colon = false;
                    for peek_ch in peek_chars {
                        if peek_ch == ':' {
                            found_colon = true;
                            break;
                        } else if !peek_ch.is_whitespace() {
                            break;
                        }
                    }

                    if found_colon {
                        // This is a key
                        spans.push(Span::styled(
                            string_content,
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ));
                    } else {
                        // This is a string value
                        spans.push(Span::styled(
                            string_content,
                            Style::default().fg(Color::Green),
                        ));
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
                    spans.push(Span::styled(
                        ch.to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                _ => {
                    // Accumulate all other characters including spaces
                    current_token.push(ch);
                }
            }
        }

        // Handle any remaining token (including trailing spaces)
        if !current_token.is_empty() {
            let trimmed_token = current_token.trim();
            if trimmed_token == "true" || trimmed_token == "false" {
                spans.push(Span::styled(
                    current_token,
                    Style::default().fg(Color::Magenta),
                ));
            } else if trimmed_token == "null" {
                spans.push(Span::styled(current_token, Style::default().fg(Color::Red)));
            } else if trimmed_token.parse::<f64>().is_ok() {
                spans.push(Span::styled(
                    current_token,
                    Style::default().fg(Color::Blue),
                ));
            } else {
                // This includes spaces and other whitespace - preserve as-is
                spans.push(Span::raw(current_token));
            }
        }

        lines.push(Line::from(spans));
    }

    lines
}

pub fn draw(f: &mut Frame, app: &App) {
    // Calculate footer height dynamically
    let keybinds = get_keybinds_for_mode(app);
    let available_width = f.size().width as usize;
    let line_spans = arrange_keybinds_responsive(keybinds, available_width);
    let footer_height = (line_spans.len() + 2).max(3); // +2 for borders, minimum 3

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),                    // Header
            Constraint::Min(10),                      // Main content
            Constraint::Length(footer_height as u16), // Dynamic footer height
            Constraint::Length(1),                    // Input dialog
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
    if app.input_mode == InputMode::EditingTarget {
        draw_input_dialog(f, app, "Edit Target URL", "Target URL");
    } else if app.input_mode == InputMode::FilteringRequests {
        draw_input_dialog(f, app, "Filter Requests", "Filter");
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let status = if app.is_running { "RUNNING" } else { "STOPPED" };
    let status_color = if app.is_running {
        Color::Green
    } else {
        Color::Red
    };

    let mode_text = match app.app_mode {
        AppMode::Normal => String::new(),
        AppMode::Paused => " | Mode: PAUSED".to_string(),
        AppMode::Intercepting => format!(
            " | Mode: INTERCEPTING ({} pending)",
            app.pending_requests.len()
        ),
    };
    let mode_color = match app.app_mode {
        AppMode::Normal => Color::White,
        AppMode::Paused => Color::Yellow,
        AppMode::Intercepting => Color::Red,
    };

    let header_text = vec![Line::from(vec![
        Span::raw("JSON-RPC Debugger | Status: "),
        Span::styled(
            status,
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            " | Port: {} | Target: {} | Filter: {}",
            app.proxy_config.listen_port, app.proxy_config.target_url, app.filter_text
        )),
        Span::styled(
            mode_text,
            Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
        ),
    ])];

    let header =
        Paragraph::new(header_text).block(Block::default().borders(Borders::ALL).title("Status"));

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
            format!(
                "Proxy is running on port {}. Waiting for JSON-RPC requests...",
                app.proxy_config.listen_port
            )
        } else {
            "Press 's' to start the proxy and begin capturing messages".to_string()
        };

        let paragraph = Paragraph::new(empty_message.as_str())
            .block(Block::default().borders(Borders::ALL).title("JSON-RPC"))
            .style(Style::default().fg(Color::Gray))
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
        return;
    }

    // Create table headers
    let header = Row::new(vec![
        Cell::from("Status"),
        Cell::from("Transport"),
        Cell::from("Method"),
        Cell::from("ID"),
        Cell::from("Duration"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD))
    .height(1);

    // Create table rows
    let rows: Vec<Row> = app
        .exchanges
        .iter()
        .enumerate()
        .filter(|(_, exchange)| {
            if app.filter_text.is_empty() {
                true
            } else {
                // TODO: Filter by id, params, result, error, etc.
                exchange
                    .method
                    .as_deref()
                    .unwrap_or("")
                    .contains(&app.filter_text)
            }
        })
        .map(|(i, exchange)| {
            let transport_symbol = match exchange.transport {
                TransportType::Http => "HTTP",
                TransportType::WebSocket => "WS",
            };

            let method = exchange.method.as_deref().unwrap_or("unknown");
            let id = exchange
                .id
                .as_ref()
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => v.to_string(),
                })
                .unwrap_or_else(|| "null".to_string());

            // Determine status
            let (status_symbol, status_color) = if exchange.response.is_none() {
                ("⏳ Pending", Color::Yellow)
            } else if let Some(response) = &exchange.response {
                if response.error.is_some() {
                    ("✗ Error", Color::Red)
                } else {
                    ("✓ Success", Color::Green)
                }
            } else {
                ("? Unknown", Color::Gray)
            };

            // Calculate duration if we have both request and response
            let duration_text =
                if let (Some(request), Some(response)) = (&exchange.request, &exchange.response) {
                    match response.timestamp.duration_since(request.timestamp) {
                        Ok(duration) => {
                            let millis = duration.as_millis();
                            if millis < 1000 {
                                format!("{}ms", millis)
                            } else {
                                format!("{:.2}s", duration.as_secs_f64())
                            }
                        }
                        Err(_) => "-".to_string(),
                    }
                } else {
                    "-".to_string()
                };

            let style = if i == app.selected_exchange {
                Style::default()
                    .bg(Color::Cyan)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(status_symbol).style(Style::default().fg(status_color)),
                Cell::from(transport_symbol).style(Style::default().fg(Color::Blue)),
                Cell::from(method).style(Style::default().fg(Color::Red)),
                Cell::from(id).style(Style::default().fg(Color::Gray)),
                Cell::from(duration_text).style(Style::default().fg(Color::Magenta)),
            ])
            .style(style)
            .height(1)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(12), // Status
            Constraint::Length(9),  // Transport
            Constraint::Min(15),    // Method (flexible)
            Constraint::Length(12), // ID
            Constraint::Length(10), // Duration
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("JSON-RPC"))
    .highlight_style(
        Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("→ ");

    let mut table_state = TableState::default();
    table_state.select(Some(app.selected_exchange));
    f.render_stateful_widget(table, area, &mut table_state);

    let filtered_count = app
        .exchanges
        .iter()
        .filter(|exchange| {
            if app.filter_text.is_empty() {
                true
            } else {
                exchange
                    .method
                    .as_deref()
                    .unwrap_or("")
                    .contains(&app.filter_text)
            }
        })
        .count();

    if filtered_count > 0 {
        let mut scrollbar_state =
            ScrollbarState::new(filtered_count).position(app.selected_exchange);

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("▐");

        f.render_stateful_widget(
            scrollbar,
            area.inner(&Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }
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
            lines.push(Line::from(Span::styled(
                "REQUEST:",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Green),
            )));

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
            lines.push(Line::from(Span::styled(
                "JSON-RPC Request:",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            let mut request_json = serde_json::Map::new();
            request_json.insert(
                "jsonrpc".to_string(),
                serde_json::Value::String("2.0".to_string()),
            );

            if let Some(id) = &request.id {
                request_json.insert("id".to_string(), id.clone());
            }
            if let Some(method) = &request.method {
                request_json.insert(
                    "method".to_string(),
                    serde_json::Value::String(method.clone()),
                );
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
            lines.push(Line::from(Span::styled(
                "RESPONSE:",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Blue),
            )));

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
            lines.push(Line::from(Span::styled(
                "JSON-RPC Response:",
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            let mut response_json = serde_json::Map::new();
            response_json.insert(
                "jsonrpc".to_string(),
                serde_json::Value::String("2.0".to_string()),
            );

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
            lines.push(Line::from(Span::styled(
                "RESPONSE: Pending...",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Yellow),
            )));
        }

        lines
    } else {
        vec![Line::from("No request selected")]
    };

    // Calculate visible area for scrolling
    let inner_area = area.inner(&Margin {
        vertical: 1,
        horizontal: 1,
    });
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
        let progress =
            ((app.details_scroll as f32 / (total_lines - visible_lines) as f32) * 100.0) as u8;
        format!("Details ({}% - vim: j/k/d/u/G/g)", progress)
    } else {
        "Details".to_string()
    };

    let details = Paragraph::new(visible_content)
        .block(Block::default().borders(Borders::ALL).title(scroll_info))
        .wrap(Wrap { trim: false });

    f.render_widget(details, area);

    if total_lines > visible_lines {
        let mut scrollbar_state = ScrollbarState::new(total_lines).position(app.details_scroll);

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("▐");

        f.render_stateful_widget(
            scrollbar,
            area.inner(&Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }
}

// Helper struct to represent a keybind with its display information
#[derive(Clone)]
struct KeybindInfo {
    key: String,
    description: String,
    priority: u8, // Lower number = higher priority
}

impl KeybindInfo {
    fn new(key: &str, description: &str, priority: u8) -> Self {
        Self {
            key: key.to_string(),
            description: description.to_string(),
            priority,
        }
    }

    // Calculate the display width of this keybind (key + description + separators)
    fn display_width(&self) -> usize {
        self.key.len() + 1 + self.description.len() + 3 // " | " separator
    }

    // Convert to spans for rendering
    fn to_spans(&self) -> Vec<Span<'static>> {
        vec![
            Span::styled(
                self.key.clone(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" {} | ", self.description)),
        ]
    }
}

fn get_keybinds_for_mode(app: &App) -> Vec<KeybindInfo> {
    let mut keybinds = vec![
        // Essential keybinds (priority 1)
        KeybindInfo::new("q", "quit", 1),
        KeybindInfo::new("↑↓", "navigate", 1),
        KeybindInfo::new("s", "start/stop proxy", 1),
        // Navigation keybinds (priority 2)
        KeybindInfo::new("^n/^p", "navigate", 2),
        KeybindInfo::new("t", "edit target", 2),
        KeybindInfo::new("/", "filter", 2),
        KeybindInfo::new("p", "pause", 2),
        // Advanced keybinds (priority 3)
        KeybindInfo::new("j/k/d/u/G/g", "scroll details", 3),
    ];

    // Add context-specific keybinds (priority 4)
    match app.app_mode {
        AppMode::Paused | AppMode::Intercepting => {
            // Only show intercept controls if there are pending requests
            if !app.pending_requests.is_empty() {
                keybinds.extend(vec![
                    KeybindInfo::new("a", "allow", 4),
                    KeybindInfo::new("e", "edit", 4),
                    KeybindInfo::new("h", "headers", 4),
                    KeybindInfo::new("c", "complete", 4),
                    KeybindInfo::new("b", "block", 4),
                    KeybindInfo::new("r", "resume", 4),
                ]);
            }
        }
        AppMode::Normal => {
            keybinds.push(KeybindInfo::new("c", "create request", 4));
        }
    }

    keybinds
}

fn arrange_keybinds_responsive(
    keybinds: Vec<KeybindInfo>,
    available_width: usize,
) -> Vec<Vec<Span<'static>>> {
    let mut lines = Vec::new();
    let mut current_line_spans = Vec::new();
    let mut current_line_width = 0;

    // Account for border padding (2 chars for left/right borders)
    let usable_width = available_width.saturating_sub(4);

    // Sort keybinds by priority
    let mut sorted_keybinds = keybinds;
    sorted_keybinds.sort_by_key(|k| k.priority);

    for (i, keybind) in sorted_keybinds.iter().enumerate() {
        let keybind_width = keybind.display_width();
        let is_last = i == sorted_keybinds.len() - 1;

        // Check if this keybind fits on the current line
        let width_needed = if is_last {
            keybind_width - 3 // Remove " | " from last item
        } else {
            keybind_width
        };

        if current_line_width + width_needed <= usable_width || current_line_spans.is_empty() {
            // Add to current line
            let mut spans = keybind.to_spans();
            if is_last {
                // Remove the trailing " | " from the last keybind
                if let Some(last_span) = spans.last_mut() {
                    if let Some(content) = last_span.content.strip_suffix(" | ") {
                        *last_span = Span::raw(content.to_string());
                    }
                }
            }
            current_line_spans.extend(spans);
            current_line_width += width_needed;
        } else {
            // Start a new line
            // Remove trailing " | " from the last span of the current line
            if let Some(last_span) = current_line_spans.last_mut() {
                if let Some(content) = last_span.content.strip_suffix(" | ") {
                    *last_span = Span::raw(content.to_string());
                }
            }

            lines.push(current_line_spans);
            current_line_spans = keybind.to_spans();
            current_line_width = keybind_width;

            // If this is the last keybind, remove trailing separator
            if is_last {
                if let Some(last_span) = current_line_spans.last_mut() {
                    if let Some(content) = last_span.content.strip_suffix(" | ") {
                        *last_span = Span::raw(content.to_string());
                    }
                }
            }
        }
    }

    // Add the last line if it has content
    if !current_line_spans.is_empty() {
        lines.push(current_line_spans);
    }

    lines
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let keybinds = get_keybinds_for_mode(app);
    let available_width = area.width as usize;

    let line_spans = arrange_keybinds_responsive(keybinds, available_width);

    // Convert spans to Lines
    let footer_text: Vec<Line> = line_spans.into_iter().map(Line::from).collect();

    let footer =
        Paragraph::new(footer_text).block(Block::default().borders(Borders::ALL).title("Controls"));

    f.render_widget(footer, area);
}

fn draw_input_dialog(f: &mut Frame, app: &App, title: &str, label: &str) {
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
    let background = Block::default().style(Style::default().bg(Color::Black));
    f.render_widget(background, area);

    // Clear the popup area specifically
    f.render_widget(Clear, popup_area);

    let input_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw(format!("{}: ", label)),
            Span::styled(&app.input_buffer, Style::default().fg(Color::Green)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Press Enter to confirm, Esc to cancel",
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
    ];

    let input_dialog = Paragraph::new(input_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .style(Style::default().fg(Color::White).bg(Color::DarkGray)),
        )
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
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Pending Requests"),
            )
            .style(Style::default().fg(Color::Yellow))
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
        return;
    }

    let requests: Vec<ListItem> = app
        .pending_requests
        .iter()
        .enumerate()
        .filter(|(_, pending)| {
            if app.filter_text.is_empty() {
                true
            } else {
                // Filter pending requests by method name (same as main list)
                pending
                    .original_request
                    .method
                    .as_deref()
                    .unwrap_or("")
                    .contains(&app.filter_text)
            }
        })
        .map(|(i, pending)| {
            let method = pending
                .original_request
                .method
                .as_deref()
                .unwrap_or("unknown");
            let id = pending
                .original_request
                .id
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".to_string());

            let style = if i == app.selected_pending {
                Style::default()
                    .bg(Color::Cyan)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            // Show different icon if request has been modified
            let (icon, icon_color) =
                if pending.modified_request.is_some() || pending.modified_headers.is_some() {
                    ("✏ ", Color::Blue) // Modified
                } else {
                    ("⏸ ", Color::Red) // Paused/Intercepted
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
                    Span::styled(
                        modification_text,
                        Style::default()
                            .fg(Color::Blue)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::raw("")
                },
            ]))
            .style(style)
        })
        .collect();

    let requests_list = List::new(requests)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Pending Requests ({})", app.pending_requests.len())),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        );

    f.render_widget(requests_list, area);
}

fn draw_request_details(f: &mut Frame, area: Rect, app: &App) {
    let content = if let Some(pending) = app.get_selected_pending() {
        let mut lines = Vec::new();

        if pending.modified_request.is_some() || pending.modified_headers.is_some() {
            lines.push(Line::from(Span::styled(
                "MODIFIED REQUEST:",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Blue),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "INTERCEPTED REQUEST:",
                Style::default().add_modifier(Modifier::BOLD).fg(Color::Red),
            )));
        }
        lines.push(Line::from(""));

        // Show headers section
        lines.push(Line::from(Span::styled(
            "HTTP Headers:",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Green),
        )));
        let headers_to_show = pending
            .modified_headers
            .as_ref()
            .or(pending.original_request.headers.as_ref());

        if let Some(headers) = headers_to_show {
            for (key, value) in headers {
                lines.push(Line::from(format!("  {}: {}", key, value)));
            }
            if pending.modified_headers.is_some() {
                lines.push(Line::from(Span::styled(
                    "  [Headers have been modified]",
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
        } else {
            lines.push(Line::from("  No headers"));
        }
        lines.push(Line::from(""));

        // Show JSON-RPC body section
        lines.push(Line::from(Span::styled(
            "JSON-RPC Request:",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Green),
        )));

        // Show the modified request if available, otherwise show original
        let json_to_show = if let Some(ref modified_json) = pending.modified_request {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(modified_json) {
                parsed
            } else {
                // Fallback to original if modified JSON is invalid
                let mut request_json = serde_json::Map::new();
                request_json.insert(
                    "jsonrpc".to_string(),
                    serde_json::Value::String("2.0".to_string()),
                );

                if let Some(id) = &pending.original_request.id {
                    request_json.insert("id".to_string(), id.clone());
                }
                if let Some(method) = &pending.original_request.method {
                    request_json.insert(
                        "method".to_string(),
                        serde_json::Value::String(method.clone()),
                    );
                }
                if let Some(params) = &pending.original_request.params {
                    request_json.insert("params".to_string(), params.clone());
                }

                serde_json::Value::Object(request_json)
            }
        } else {
            // Show original request
            let mut request_json = serde_json::Map::new();
            request_json.insert(
                "jsonrpc".to_string(),
                serde_json::Value::String("2.0".to_string()),
            );

            if let Some(id) = &pending.original_request.id {
                request_json.insert("id".to_string(), id.clone());
            }
            if let Some(method) = &pending.original_request.method {
                request_json.insert(
                    "method".to_string(),
                    serde_json::Value::String(method.clone()),
                );
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
        lines.push(Line::from(Span::styled(
            "Actions:",
            Style::default().add_modifier(Modifier::BOLD),
        )));
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

    // Calculate visible area for scrolling
    let inner_area = area.inner(&Margin {
        vertical: 1,
        horizontal: 1,
    });
    let visible_lines = inner_area.height as usize;
    let total_lines = content.len();

    // Apply scrolling offset
    let start_line = app.intercept_details_scroll;
    let end_line = std::cmp::min(start_line + visible_lines, total_lines);
    let visible_content = if start_line < total_lines {
        content[start_line..end_line].to_vec()
    } else {
        vec![]
    };

    // Create title with scroll indicator
    let scroll_info = if total_lines > visible_lines {
        let progress = ((app.intercept_details_scroll as f32
            / (total_lines - visible_lines) as f32)
            * 100.0) as u8;
        format!("Request Details ({}% - vim: j/k/d/u/G/g)", progress)
    } else {
        "Request Details".to_string()
    };

    let details = Paragraph::new(visible_content)
        .block(Block::default().borders(Borders::ALL).title(scroll_info))
        .wrap(Wrap { trim: false });

    f.render_widget(details, area);

    if total_lines > visible_lines {
        let mut scrollbar_state =
            ScrollbarState::new(total_lines).position(app.intercept_details_scroll);

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("▐");

        f.render_stateful_widget(
            scrollbar,
            area.inner(&Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }
}
