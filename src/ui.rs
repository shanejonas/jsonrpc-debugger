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

use crate::app::{App, AppMode, EditorMode, Focus, InputMode, JsonRpcExchange, TransportType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    EditTarget,
    EditFilter,
    SetProxyRunning(bool),
    SelectExchange(usize),
    SelectPending(usize),
    SelectRequestTab(usize),
    SelectResponseTab(usize),
    SelectLine { panel: Focus, line: usize },
    Focus(Focus),
}

pub fn panel_focus(area: Rect, app: &App, column: u16, row: u16) -> Option<Focus> {
    let chunks = screen_chunks(area, app);
    let header = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(chunks[0]);
    if contains(header[1], column, row) {
        return Some(Focus::StatusHeader);
    }
    if !contains(chunks[1], column, row) {
        return None;
    }

    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);
    if contains(main[0], column, row) {
        return Some(Focus::MessageList);
    }
    if !contains(main[1], column, row) || app.app_mode != AppMode::Normal {
        return contains(main[1], column, row).then_some(Focus::RequestSection);
    }

    let details = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(main[1]);
    if contains(details[0], column, row) {
        return Some(Focus::RequestSection);
    }
    if contains(details[1], column, row) {
        return Some(Focus::ResponseSection);
    }

    None
}

pub fn panel_visible_lines(area: Rect, app: &App, focus: Focus) -> usize {
    let chunks = screen_chunks(area, app);
    let main_height = chunks[1].height as usize;
    match (app.app_mode, focus) {
        (AppMode::Normal, Focus::RequestSection | Focus::ResponseSection) => {
            (main_height / 2).saturating_sub(2)
        }
        (AppMode::Paused | AppMode::Intercepting, Focus::RequestSection) => {
            main_height.saturating_sub(2)
        }
        _ => main_height.saturating_sub(3),
    }
}

pub fn mouse_action(area: Rect, app: &App, column: u16, row: u16) -> Option<MouseAction> {
    let chunks = screen_chunks(area, app);
    let header_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(chunks[0]);

    if contains(header_chunks[0], column, row) {
        return request_header_action(header_chunks[0], app, column, row);
    }
    if contains(header_chunks[1], column, row) {
        return status_header_action(header_chunks[1], column, row);
    }
    if !contains(chunks[1], column, row) {
        return None;
    }

    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);

    if contains(main_chunks[0], column, row) {
        return match app.app_mode {
            AppMode::Normal => message_list_action(main_chunks[0], app, row),
            AppMode::Paused | AppMode::Intercepting => {
                pending_list_action(main_chunks[0], app, row)
            }
        };
    }

    if !contains(main_chunks[1], column, row) {
        return None;
    }
    if app.app_mode != AppMode::Normal {
        return Some(MouseAction::Focus(Focus::RequestSection));
    }

    let details = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(main_chunks[1]);

    if contains(details[0], column, row) {
        return request_details_action(details[0], app, column, row);
    }
    if contains(details[1], column, row) {
        return response_details_action(details[1], app, column, row);
    }

    None
}

fn screen_chunks(area: Rect, app: &App) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(10),
            Constraint::Length(footer_height(app, area.width)),
            Constraint::Length(1),
        ])
        .split(area)
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

fn request_header_action(area: Rect, app: &App, column: u16, row: u16) -> Option<MouseAction> {
    if row != area.y.saturating_add(1) {
        return None;
    }

    let transport_width = match app.proxy_config.transport {
        TransportType::Http => 6,
        TransportType::WebSocket => 11,
    };
    let target = if app.input_mode == InputMode::EditingTarget {
        if app.input_buffer.is_empty() {
            "Enter target URL"
        } else {
            &app.input_buffer
        }
    } else if app.proxy_config.target_url.is_empty() {
        "Press t to set target"
    } else {
        &app.proxy_config.target_url
    };
    let target_start = area.x.saturating_add(1 + transport_width + 4);
    let target_end = target_start.saturating_add(target.chars().count() as u16 + 2);
    if column >= target_start && column < target_end {
        return Some(MouseAction::EditTarget);
    }

    let cursor_width = u16::from(app.input_mode == InputMode::EditingTarget);
    let filter_start = target_end.saturating_add(cursor_width + 2);
    let filter_width = if app.filter_text.is_empty() {
        "Filter (press /)".chars().count() as u16 + 2
    } else {
        "Filter: ".chars().count() as u16 + app.filter_text.chars().count() as u16 + 2
    };
    if column >= filter_start && column < filter_start.saturating_add(filter_width) {
        return Some(MouseAction::EditFilter);
    }

    None
}

fn status_header_action(area: Rect, column: u16, row: u16) -> Option<MouseAction> {
    if row == area.y.saturating_add(1) {
        let running_start = area.x.saturating_add(1);
        let stopped_start = running_start.saturating_add(9);
        if column >= running_start && column < stopped_start {
            return Some(MouseAction::SetProxyRunning(true));
        }
        if column >= stopped_start && column < stopped_start.saturating_add(9) {
            return Some(MouseAction::SetProxyRunning(false));
        }
    }

    Some(MouseAction::Focus(Focus::StatusHeader))
}

fn message_list_action(area: Rect, app: &App, row: u16) -> Option<MouseAction> {
    let indices = app.filtered_exchange_indices();
    let first_row = area.y.saturating_add(2);
    let visible_rows = area.height.saturating_sub(3) as usize;
    if row < first_row || visible_rows == 0 || indices.is_empty() {
        return Some(MouseAction::Focus(Focus::MessageList));
    }

    let selected = indices
        .iter()
        .position(|index| *index == app.selected_exchange)
        .unwrap_or(0);
    let offset = selected.saturating_sub(visible_rows.saturating_sub(1));
    let clicked = offset + row.saturating_sub(first_row) as usize;
    let Some(index) = indices.get(clicked) else {
        return Some(MouseAction::Focus(Focus::MessageList));
    };

    Some(MouseAction::SelectExchange(*index))
}

fn pending_list_action(area: Rect, app: &App, row: u16) -> Option<MouseAction> {
    let first_row = area.y.saturating_add(1);
    if row < first_row {
        return Some(MouseAction::Focus(Focus::MessageList));
    }

    let index = app
        .pending_requests
        .iter()
        .enumerate()
        .filter(|(_, pending)| {
            app.filter_text.is_empty()
                || pending
                    .original_request
                    .method
                    .as_deref()
                    .unwrap_or("")
                    .contains(&app.filter_text)
        })
        .nth(row.saturating_sub(first_row) as usize)
        .map(|(index, _)| index);

    let Some(index) = index else {
        return Some(MouseAction::Focus(Focus::MessageList));
    };

    Some(MouseAction::SelectPending(index))
}

fn request_details_action(area: Rect, app: &App, column: u16, row: u16) -> Option<MouseAction> {
    let exchange = app.get_selected_exchange();
    let tab_line = 3
        + usize::from(exchange.and_then(|value| value.method.as_ref()).is_some())
        + usize::from(exchange.and_then(|value| value.id.as_ref()).is_some());

    let content = request_detail_lines(app);
    let clicked = clicked_detail_line(area, row, app.request_details_scroll, &content);
    let has_request = exchange.and_then(|value| value.request.as_ref()).is_some();
    if has_request && clicked == Some(tab_line + 1) {
        return tab_action(area, column, detail_gutter_width(content.len()))
            .map(MouseAction::SelectRequestTab);
    }

    clicked
        .map(|line| MouseAction::SelectLine {
            panel: Focus::RequestSection,
            line,
        })
        .or(Some(MouseAction::Focus(Focus::RequestSection)))
}

fn response_details_action(area: Rect, app: &App, column: u16, row: u16) -> Option<MouseAction> {
    let content = response_detail_lines(app);
    let clicked = clicked_detail_line(area, row, app.response_details_scroll, &content);
    let has_response = app
        .get_selected_exchange()
        .and_then(|exchange| exchange.response.as_ref())
        .is_some();
    if has_response && clicked == Some(2) {
        return tab_action(area, column, detail_gutter_width(content.len()))
            .map(MouseAction::SelectResponseTab);
    }

    clicked
        .map(|line| MouseAction::SelectLine {
            panel: Focus::ResponseSection,
            line,
        })
        .or(Some(MouseAction::Focus(Focus::ResponseSection)))
}

fn clicked_detail_line(area: Rect, row: u16, scroll: usize, content: &[Line<'_>]) -> Option<usize> {
    if row <= area.y || row >= area.y.saturating_add(area.height).saturating_sub(1) {
        return None;
    }

    let width = usize::from(area.width.saturating_sub(2)).max(1);
    let gutter_width = detail_gutter_width(content.len());
    let mut visible_row = usize::from(row.saturating_sub(area.y + 1));
    for (index, line) in content.iter().enumerate().skip(scroll) {
        let height = (line.width() + gutter_width).max(1).div_ceil(width);
        if visible_row < height {
            return Some(index + 1);
        }
        visible_row -= height;
    }

    None
}

fn tab_action(area: Rect, column: u16, gutter_width: usize) -> Option<usize> {
    let gutter_width = u16::try_from(gutter_width).unwrap_or(u16::MAX);
    let column = column.checked_sub(area.x.saturating_add(1 + gutter_width))?;
    match column {
        0..=8 => Some(0),
        9..=14 => Some(1),
        _ => None,
    }
}

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

fn build_tab_line(
    labels: &'static [&'static str],
    selected: usize,
    is_active: bool,
    is_enabled: bool,
) -> Line<'static> {
    let mut spans = Vec::new();

    for (index, label) in labels.iter().enumerate() {
        let is_selected = index == selected;

        if is_selected {
            // Active tab - use a more prominent style like modern tab designs
            let mut style = Style::default();
            if is_enabled {
                style = style
                    .fg(Color::Black)
                    .bg(if is_active { Color::Cyan } else { Color::White })
                    .add_modifier(Modifier::BOLD);
            } else {
                style = style.fg(Color::DarkGray).bg(Color::DarkGray);
            }

            spans.push(Span::styled(format!(" {} ", *label), style));
        } else if is_enabled {
            // Inactive tab - subtle background
            let style = Style::default()
                .fg(if is_active { Color::White } else { Color::Gray })
                .bg(Color::DarkGray);
            spans.push(Span::styled(format!(" {} ", *label), style));
        } else {
            // Disabled tab
            let style = Style::default().fg(Color::DarkGray);
            spans.push(Span::styled(format!(" {} ", *label), style));
        }

        // Add separator between tabs
        if index < labels.len() - 1 {
            spans.push(Span::raw(""));
        }
    }

    Line::from(spans)
}

pub fn draw(f: &mut Frame, app: &App) {
    let footer_height = footer_height(app, f.size().width);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),             // Header
            Constraint::Min(10),               // Main content
            Constraint::Length(footer_height), // Dynamic footer height
            Constraint::Length(1),             // Input dialog
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

    if let Some(notice) = &app.notice {
        let color = if notice.starts_with("Error:") {
            Color::Red
        } else {
            Color::Green
        };
        f.render_widget(
            Paragraph::new(notice.as_str()).style(Style::default().fg(color)),
            chunks[3],
        );
    }

    if app.editor.is_some() {
        draw_text_editor(f, app);
    } else if app.input_mode == InputMode::FilteringRequests {
        draw_input_dialog(f, app, "Filter Requests", "Filter");
    }
}

fn draw_text_editor(f: &mut Frame, app: &App) {
    let Some(editor) = &app.editor else {
        return;
    };

    let area = f.size();
    let popup = Rect {
        x: area.x + 2.min(area.width / 2),
        y: area.y + 2.min(area.height / 2),
        width: area.width.saturating_sub(4).max(1),
        height: area.height.saturating_sub(4).max(1),
    };
    let mode = match editor.mode {
        EditorMode::Normal => "NORMAL",
        EditorMode::Insert => "INSERT",
        EditorMode::Command => "COMMAND",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("{} — {}", editor.target.title(), mode))
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(popup);

    f.render_widget(Clear, popup);
    f.render_widget(block, popup);
    if inner.height == 0 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let visible_lines = chunks[0].height as usize;
    let scroll = editor.row.saturating_sub(visible_lines.saturating_sub(1));
    let lines = editor
        .lines
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_lines)
        .map(|(index, line)| {
            let style = if index == editor.row {
                Style::default().bg(Color::Rgb(35, 35, 35))
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(
                    format!("{:>4} │ ", index + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(line.clone(), style),
            ])
        })
        .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(lines), chunks[0]);

    let status = if let Some(error) = &editor.error {
        Span::styled(error.clone(), Style::default().fg(Color::Red))
    } else if editor.mode == EditorMode::Command {
        Span::styled(
            format!(":{}", editor.command),
            Style::default().fg(Color::Yellow),
        )
    } else if let Some(operator) = editor.pending_operator {
        Span::styled(
            format!("{}…  motion or {} for line", operator.key(), operator.key()),
            Style::default().fg(Color::Yellow),
        )
    } else if editor.pending_g {
        Span::styled("g…  g for first line", Style::default().fg(Color::Yellow))
    } else {
        Span::styled(
            "i/a/I/A/o/O · w/b/e · d/c/y+motion · dd/cc/yy · u · p/P · :wq",
            Style::default().fg(Color::Gray),
        )
    };
    f.render_widget(Paragraph::new(Line::from(status)), chunks[1]);

    let cursor_x = chunks[0]
        .x
        .saturating_add(7 + editor.column as u16)
        .min(chunks[0].right().saturating_sub(1));
    let cursor_y = chunks[0]
        .y
        .saturating_add(editor.row.saturating_sub(scroll) as u16)
        .min(chunks[0].bottom().saturating_sub(1));
    f.set_cursor(cursor_x, cursor_y);
}

fn footer_height(app: &App, width: u16) -> u16 {
    let keybinds = get_keybinds_for_mode(app);
    let lines = arrange_keybinds_responsive(keybinds, width as usize);
    (lines.len() + 2).max(3) as u16
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let header_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(area);

    draw_request_header(f, header_chunks[0], app);
    draw_status_header(f, header_chunks[1], app);
}

fn draw_request_header(f: &mut Frame, area: Rect, app: &App) {
    let transport_label = match app.proxy_config.transport {
        TransportType::Http => "HTTP",
        TransportType::WebSocket => "WebSocket",
    };

    let transport_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Rgb(210, 160, 255))
        .add_modifier(Modifier::BOLD);

    let dropdown_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Rgb(170, 120, 235))
        .add_modifier(Modifier::BOLD);

    let target_bg = if app.input_mode == InputMode::EditingTarget {
        Color::Rgb(80, 56, 140)
    } else {
        Color::Rgb(48, 36, 96)
    };

    let target_style = Style::default()
        .fg(Color::White)
        .bg(target_bg)
        .add_modifier(Modifier::BOLD);

    let target_text = if app.input_mode == InputMode::EditingTarget {
        if app.input_buffer.is_empty() {
            "Enter target URL".to_string()
        } else {
            app.input_buffer.clone()
        }
    } else if app.proxy_config.target_url.is_empty() {
        "Press t to set target".to_string()
    } else {
        app.proxy_config.target_url.clone()
    };

    let mut spans = vec![
        Span::styled(format!(" {} ", transport_label), transport_style),
        Span::styled(" ▾ ", dropdown_style),
        Span::raw(" "),
        Span::styled(format!(" {} ", target_text), target_style),
    ];

    if app.input_mode == InputMode::EditingTarget {
        spans.push(Span::styled("█", target_style));
    }

    spans.push(Span::raw("  "));

    let filter_bg = if app.input_mode == InputMode::FilteringRequests {
        Color::Rgb(80, 56, 140)
    } else {
        Color::Rgb(48, 36, 96)
    };

    let filter_style = Style::default()
        .fg(if app.filter_text.is_empty() {
            Color::Rgb(180, 170, 210)
        } else {
            Color::White
        })
        .bg(filter_bg)
        .add_modifier(Modifier::BOLD);

    let filter_text = if app.filter_text.is_empty() {
        "Filter (press /)".to_string()
    } else {
        format!("Filter: {}", app.filter_text)
    };

    spans.push(Span::styled(format!(" {} ", filter_text), filter_style));

    if app.input_mode == InputMode::FilteringRequests {
        spans.push(Span::styled("█", filter_style));
    }

    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        "Request",
        Style::default().fg(Color::LightMagenta),
    ));

    let paragraph = Paragraph::new(Line::from(spans))
        .block(block)
        .wrap(Wrap { trim: true });

    f.render_widget(paragraph, area);
}

fn draw_status_header(f: &mut Frame, area: Rect, app: &App) {
    let status_focus = matches!(app.focus, Focus::StatusHeader);

    let inactive_fg = Color::Rgb(180, 170, 210);

    let mut running_style = if app.is_running {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(inactive_fg).bg(Color::Rgb(60, 60, 60))
    };

    let mut stopped_style = if app.is_running {
        Style::default().fg(inactive_fg).bg(Color::Rgb(60, 60, 60))
    } else {
        Style::default()
            .fg(Color::White)
            .bg(Color::Rgb(120, 35, 52))
            .add_modifier(Modifier::BOLD)
    };

    if status_focus {
        if app.is_running {
            running_style = running_style.add_modifier(Modifier::UNDERLINED);
        } else {
            stopped_style = stopped_style.add_modifier(Modifier::UNDERLINED);
        }
    }

    let mode_text = match app.app_mode {
        AppMode::Normal => "Normal".to_string(),
        AppMode::Paused => "Paused".to_string(),
        AppMode::Intercepting => format!("Intercepting ({})", app.pending_requests.len()),
    };

    let mode_color = match app.app_mode {
        AppMode::Normal => Color::Gray,
        AppMode::Paused => Color::Yellow,
        AppMode::Intercepting => Color::Red,
    };

    let mut lines = Vec::new();

    let tab_spans = vec![
        Span::styled(" RUNNING ", running_style),
        Span::styled(" STOPPED ", stopped_style),
    ];
    lines.push(Line::from(tab_spans));

    let label_style = Style::default()
        .fg(Color::Gray)
        .add_modifier(Modifier::BOLD);

    let info_line = Line::from(vec![
        Span::styled("Port:", label_style),
        Span::raw(format!(" {}", app.proxy_config.listen_port)),
        Span::raw(format!("  RPC: {}  ", app.control_port)),
        Span::styled("Mode:", label_style),
        Span::styled(format!(" {}", mode_text), Style::default().fg(mode_color)),
    ]);
    lines.push(info_line);

    if app.input_mode == InputMode::EditingTarget {
        lines.push(Line::from(Span::styled(
            "Editing target (Enter to save, Esc to cancel)",
            Style::default().fg(Color::Yellow),
        )));
    }

    let mut block = Block::default().borders(Borders::ALL).title(Span::styled(
        "Status",
        Style::default().fg(Color::LightMagenta),
    ));

    if status_focus {
        block = block.border_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    } else {
        block = block.border_style(Style::default().fg(Color::DarkGray));
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });

    f.render_widget(paragraph, area);
}

fn draw_main_content(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(50), // Message list
            Constraint::Percentage(50), // Details area
        ])
        .split(area);

    draw_message_list(f, chunks[0], app);
    draw_details_split(f, chunks[1], app);
}

fn draw_message_list(f: &mut Frame, area: Rect, app: &App) {
    let filtered: Vec<(usize, &JsonRpcExchange)> = app
        .exchanges
        .iter()
        .enumerate()
        .filter(|(_, exchange)| {
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
        .collect();

    if filtered.is_empty() {
        let empty_message = if app.is_running {
            format!(
                "Proxy is running on port {}. Waiting for requests...",
                app.proxy_config.listen_port
            )
        } else {
            "Press 's' to start the proxy and begin capturing messages".to_string()
        };

        let mut block = Block::default().borders(Borders::ALL).title("Requests");
        if matches!(app.focus, Focus::MessageList) {
            block = block.border_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            );
        } else {
            block = block.border_style(Style::default().fg(Color::DarkGray));
        }

        let paragraph = Paragraph::new(empty_message.as_str())
            .block(block)
            .style(Style::default().fg(Color::Gray))
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
        return;
    }

    let selected_position = filtered
        .iter()
        .position(|(index, _)| *index == app.selected_exchange)
        .unwrap_or(0);

    let highlight_style = if matches!(app.focus, Focus::MessageList) {
        Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };

    let header = Row::new(vec![
        Cell::from("Status"),
        Cell::from("Transport"),
        Cell::from("Method"),
        Cell::from("ID"),
        Cell::from("Duration"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD))
    .height(1);

    let rows: Vec<Row> = filtered
        .iter()
        .map(|(_, exchange)| {
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

            Row::new(vec![
                Cell::from(status_symbol).style(Style::default().fg(status_color)),
                Cell::from(transport_symbol).style(Style::default().fg(Color::Blue)),
                Cell::from(method).style(Style::default().fg(Color::Red)),
                Cell::from(id).style(Style::default().fg(Color::Gray)),
                Cell::from(duration_text).style(Style::default().fg(Color::Magenta)),
            ])
            .height(1)
        })
        .collect();

    let mut table_block = Block::default().borders(Borders::ALL).title("Requests");
    if matches!(app.focus, Focus::MessageList) {
        table_block = table_block.border_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    } else {
        table_block = table_block.border_style(Style::default().fg(Color::DarkGray));
    }

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
    .block(table_block)
    .highlight_style(highlight_style)
    .highlight_symbol("  ");

    let mut table_state = TableState::default();
    table_state.select(Some(selected_position));
    f.render_stateful_widget(table, area, &mut table_state);

    if filtered.len() > 1 {
        let mut scrollbar_state = ScrollbarState::new(filtered.len()).position(selected_position);

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

pub fn detail_line_count(app: &App, panel: Focus) -> Option<usize> {
    detail_lines_text(app, panel).map(|lines| lines.len())
}

pub fn detail_lines_text(app: &App, panel: Focus) -> Option<Vec<String>> {
    let lines = match panel {
        Focus::RequestSection => request_detail_lines(app),
        Focus::ResponseSection => response_detail_lines(app),
        Focus::MessageList | Focus::StatusHeader => return None,
    };

    Some(lines.iter().map(line_text).collect())
}

pub fn detail_line_text(
    app: &App,
    panel: Focus,
    start_line: usize,
    end_line: usize,
) -> Option<Vec<String>> {
    let lines = detail_lines_text(app, panel)?;
    if start_line == 0 || end_line < start_line || end_line > lines.len() {
        return None;
    }

    Some(lines[start_line - 1..end_line].to_vec())
}

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn highlight_selected_lines(
    lines: Vec<Line<'static>>,
    app: &App,
    panel: Focus,
) -> Vec<Line<'static>> {
    let Some(selection) = &app.line_selection else {
        return lines;
    };
    if selection.panel != panel {
        return lines;
    }

    let style = Style::default()
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let line_number = index + 1;
            if (selection.start_line..=selection.end_line).contains(&line_number) {
                line.style(style)
            } else {
                line
            }
        })
        .collect()
}

fn number_detail_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let number_width = lines.len().max(1).to_string().len();
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let mut spans = vec![Span::styled(
                format!("{:>number_width$} │ ", index + 1),
                Style::default().fg(Color::DarkGray),
            )];
            spans.extend(line.spans);
            Line::from(spans).style(line.style)
        })
        .collect()
}

fn detail_gutter_width(line_count: usize) -> usize {
    line_count.max(1).to_string().len() + 3
}

fn detail_title(title: &str, app: &App, panel: Focus) -> String {
    let Some(selection) = &app.line_selection else {
        return title.to_string();
    };
    if selection.panel != panel {
        return title.to_string();
    }
    if selection.start_line == selection.end_line {
        return format!("{title} • line {}", selection.start_line);
    }

    format!(
        "{title} • lines {}-{}",
        selection.start_line, selection.end_line
    )
}

pub fn request_detail_lines(app: &App) -> Vec<Line<'static>> {
    if let Some(exchange) = app.get_selected_exchange() {
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

        // Request section with tabs
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "REQUEST:",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Green),
        )));
        lines.push(build_tab_line(
            &["Headers", "Body"],
            app.request_tab,
            matches!(app.focus, Focus::RequestSection),
            exchange.request.is_some(),
        ));

        if let Some(request) = &exchange.request {
            if app.request_tab == 0 {
                // Show headers regardless of focus state
                lines.push(Line::from(""));
                match &request.headers {
                    Some(headers) if !headers.is_empty() => {
                        for (key, value) in headers {
                            lines.push(Line::from(format!("  {}: {}", key, value)));
                        }
                    }
                    Some(_) => {
                        lines.push(Line::from("  No headers"));
                    }
                    None => {
                        lines.push(Line::from("  No headers captured"));
                    }
                }
            } else {
                // Show body regardless of focus state
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
        } else {
            lines.push(Line::from(""));
            lines.push(Line::from("Request not captured yet"));
        }

        lines
    } else {
        vec![Line::from("No request selected")]
    }
}

fn draw_request_details(f: &mut Frame, area: Rect, app: &App) {
    let content = number_detail_lines(request_detail_lines(app));
    let content = highlight_selected_lines(content, app, Focus::RequestSection);

    // Calculate visible area for scrolling
    let inner_area = area.inner(&Margin {
        vertical: 1,
        horizontal: 1,
    });
    let visible_lines = inner_area.height as usize;
    let total_lines = content.len();

    // Apply scrolling offset
    let max_scroll = total_lines.saturating_sub(visible_lines);
    let start_line = app.request_details_scroll.min(max_scroll);
    let end_line = std::cmp::min(start_line + visible_lines, total_lines);
    let visible_content = if start_line < total_lines {
        content[start_line..end_line].to_vec()
    } else {
        vec![]
    };

    // Create title with scroll indicator
    let base_title = detail_title("Request Details", app, Focus::RequestSection);

    let scroll_info = if total_lines > visible_lines {
        let progress = ((start_line as f32 / max_scroll as f32) * 100.0) as u8;
        format!("{} ({}% - vim: j/k/d/u/G/g)", base_title, progress)
    } else {
        base_title
    };

    let details_block = if matches!(app.focus, Focus::RequestSection) {
        Block::default()
            .borders(Borders::ALL)
            .title(scroll_info)
            .border_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
    } else {
        Block::default().borders(Borders::ALL).title(scroll_info)
    };

    let details = Paragraph::new(visible_content)
        .block(details_block)
        .wrap(Wrap { trim: false });

    f.render_widget(details, area);

    if total_lines > visible_lines {
        let mut scrollbar_state = ScrollbarState::new(total_lines).position(start_line);

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

fn draw_details_split(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50), // Request details
            Constraint::Percentage(50), // Response details
        ])
        .split(area);

    draw_request_details(f, chunks[0], app);
    draw_response_details(f, chunks[1], app);
}

pub fn response_detail_lines(app: &App) -> Vec<Line<'static>> {
    if let Some(exchange) = app.get_selected_exchange() {
        let mut lines = Vec::new();

        // Response section with tabs
        lines.push(Line::from(Span::styled(
            "RESPONSE:",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Blue),
        )));
        lines.push(build_tab_line(
            &["Headers", "Body"],
            app.response_tab,
            matches!(app.focus, Focus::ResponseSection),
            exchange.response.is_some(),
        ));

        if let Some(response) = &exchange.response {
            if app.response_tab == 0 {
                // Show headers regardless of focus state
                lines.push(Line::from(""));
                match &response.headers {
                    Some(headers) if !headers.is_empty() => {
                        for (key, value) in headers {
                            lines.push(Line::from(format!("  {}: {}", key, value)));
                        }
                    }
                    Some(_) => {
                        lines.push(Line::from("  No headers"));
                    }
                    None => {
                        lines.push(Line::from("  No headers captured"));
                    }
                }
            } else {
                // Show body regardless of focus state
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
            }
        } else {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Response pending...",
                Style::default().fg(Color::Yellow),
            )));
        }

        lines
    } else {
        vec![Line::from("No request selected")]
    }
}

fn draw_response_details(f: &mut Frame, area: Rect, app: &App) {
    let content = number_detail_lines(response_detail_lines(app));
    let content = highlight_selected_lines(content, app, Focus::ResponseSection);

    // Calculate visible area for scrolling
    let inner_area = area.inner(&Margin {
        vertical: 1,
        horizontal: 1,
    });
    let visible_lines = inner_area.height as usize;
    let total_lines = content.len();

    // Apply scrolling offset
    let max_scroll = total_lines.saturating_sub(visible_lines);
    let start_line = app.response_details_scroll.min(max_scroll);
    let end_line = std::cmp::min(start_line + visible_lines, total_lines);
    let visible_content = if start_line < total_lines {
        content[start_line..end_line].to_vec()
    } else {
        vec![]
    };

    // Create title with scroll indicator
    let base_title = detail_title("Response Details", app, Focus::ResponseSection);

    let scroll_info = if total_lines > visible_lines {
        let progress = ((start_line as f32 / max_scroll as f32) * 100.0) as u8;
        format!("{} ({}% - vim: j/k/d/u/G/g)", base_title, progress)
    } else {
        base_title
    };

    let details_block = if matches!(app.focus, Focus::ResponseSection) {
        Block::default()
            .borders(Borders::ALL)
            .title(scroll_info)
            .border_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
    } else {
        Block::default().borders(Borders::ALL).title(scroll_info)
    };

    let details = Paragraph::new(visible_content)
        .block(details_block)
        .wrap(Wrap { trim: false });

    f.render_widget(details, area);

    if total_lines > visible_lines {
        let mut scrollbar_state = ScrollbarState::new(total_lines).position(start_line);

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
        KeybindInfo::new("Tab/Shift+Tab", "navigate", 2),
        KeybindInfo::new("Enter", "copy markdown", 2),
        KeybindInfo::new("^n/^p", "navigate", 2),
        KeybindInfo::new("t", "edit target", 2),
        KeybindInfo::new("/", "filter", 2),
        KeybindInfo::new("p", "pause", 2),
        // Advanced keybinds (priority 3)
        KeybindInfo::new("j/k/d/u/G/g", "scroll details", 3),
        KeybindInfo::new("h/l", "navigate tabs", 3),
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
    draw_intercept_request_details(f, chunks[1], app);
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

    let pending_block = if matches!(app.focus, Focus::MessageList) {
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Pending Requests ({})", app.pending_requests.len()))
            .border_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
    } else {
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Pending Requests ({})", app.pending_requests.len()))
    };

    let requests_list = List::new(requests).block(pending_block).highlight_style(
        Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    );

    f.render_widget(requests_list, area);
}

fn draw_intercept_request_details(f: &mut Frame, area: Rect, app: &App) {
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

    let details_block = if matches!(app.focus, Focus::RequestSection) {
        Block::default()
            .borders(Borders::ALL)
            .title(scroll_info)
            .border_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
    } else {
        Block::default().borders(Borders::ALL).title(scroll_info)
    };

    let details = Paragraph::new(visible_content)
        .block(details_block)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{JsonRpcMessage, MessageDirection};

    fn app_with_request() -> App {
        let mut app = App::new();
        app.add_message(JsonRpcMessage {
            id: Some(serde_json::json!(1)),
            method: Some("eth_call".to_string()),
            params: Some(serde_json::json!([])),
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: MessageDirection::Request,
            transport: TransportType::Http,
            headers: None,
        });
        app.add_message(JsonRpcMessage {
            id: Some(serde_json::json!(1)),
            method: None,
            params: None,
            result: Some(serde_json::json!("0x1")),
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: MessageDirection::Response,
            transport: TransportType::Http,
            headers: None,
        });
        app
    }

    fn normal_panels(area: Rect, app: &App) -> (Rect, Rect, Rect) {
        let screen = screen_chunks(area, app);
        let main = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(screen[1]);
        let details = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(main[1]);

        (main[0], details[0], details[1])
    }

    #[test]
    fn clicking_a_request_row_selects_it() {
        let app = app_with_request();
        let area = Rect::new(0, 0, 120, 40);
        let (requests, _, _) = normal_panels(area, &app);

        assert_eq!(
            mouse_action(area, &app, requests.x + 2, requests.y + 2),
            Some(MouseAction::SelectExchange(0))
        );
    }

    #[test]
    fn clicking_detail_tabs_selects_them() {
        let app = app_with_request();
        let area = Rect::new(0, 0, 120, 40);
        let (_, request, response) = normal_panels(area, &app);
        let request_gutter = detail_gutter_width(request_detail_lines(&app).len()) as u16;
        let response_gutter = detail_gutter_width(response_detail_lines(&app).len()) as u16;

        assert_eq!(
            mouse_action(area, &app, request.x + 1 + request_gutter, request.y + 6,),
            Some(MouseAction::SelectRequestTab(0))
        );
        assert_eq!(
            mouse_action(
                area,
                &app,
                response.x + 11 + response_gutter,
                response.y + 2,
            ),
            Some(MouseAction::SelectResponseTab(1))
        );
    }

    #[test]
    fn clicking_detail_content_selects_a_line() {
        let app = app_with_request();
        let area = Rect::new(0, 0, 120, 40);
        let (_, request, response) = normal_panels(area, &app);

        assert_eq!(
            mouse_action(area, &app, request.x + 20, request.y + 3),
            Some(MouseAction::SelectLine {
                panel: Focus::RequestSection,
                line: 3,
            })
        );
        assert_eq!(
            mouse_action(area, &app, response.x + 20, response.y + 3),
            Some(MouseAction::SelectLine {
                panel: Focus::ResponseSection,
                line: 3,
            })
        );
    }

    #[test]
    fn selected_line_text_matches_the_highlighted_content() {
        let mut app = app_with_request();
        let text = detail_line_text(&app, Focus::RequestSection, 2, 2).unwrap();
        app.select_lines(Focus::RequestSection, 2, 2, text.clone());

        let lines =
            highlight_selected_lines(request_detail_lines(&app), &app, Focus::RequestSection);

        assert_eq!(text, vec!["Method: eth_call"]);
        assert_eq!(lines[1].style.bg, Some(Color::DarkGray));
        assert_eq!(app.request_details_scroll, 0);
    }

    #[test]
    fn visible_line_numbers_match_get_panel_without_changing_its_text() {
        let app = app_with_request();
        let raw = detail_lines_text(&app, Focus::RequestSection).unwrap();
        let numbered = number_detail_lines(request_detail_lines(&app));

        assert_eq!(raw[1], "Method: eth_call");
        assert_eq!(line_text(&numbered[1]), " 2 │ Method: eth_call");
    }

    #[test]
    fn extending_a_selection_keeps_the_original_anchor() {
        let mut app = app_with_request();
        let text = detail_line_text(&app, Focus::RequestSection, 3, 3).unwrap();
        app.select_lines(Focus::RequestSection, 3, 3, text);

        assert_eq!(
            app.line_selection_range(Focus::RequestSection, 1, true),
            (3, 1, 3)
        );
        let text = detail_line_text(&app, Focus::RequestSection, 1, 3).unwrap();
        app.select_lines_from_anchor(Focus::RequestSection, 3, 1, 3, text);
        assert_eq!(
            app.line_selection_range(Focus::RequestSection, 5, true),
            (3, 3, 5)
        );
    }

    #[test]
    fn clicking_header_controls_activates_them() {
        let app = app_with_request();
        let area = Rect::new(0, 0, 120, 40);
        let screen = screen_chunks(area, &app);
        let header = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(screen[0]);
        let target_start = header[0].x + 11;
        let filter_start = target_start + "Press t to set target".len() as u16 + 4;

        assert_eq!(
            mouse_action(area, &app, target_start, header[0].y + 1),
            Some(MouseAction::EditTarget)
        );
        assert_eq!(
            mouse_action(area, &app, filter_start, header[0].y + 1),
            Some(MouseAction::EditFilter)
        );
        assert_eq!(
            mouse_action(area, &app, header[1].x + 2, header[1].y + 1),
            Some(MouseAction::SetProxyRunning(true))
        );
        assert_eq!(
            mouse_action(area, &app, header[1].x + 11, header[1].y + 1),
            Some(MouseAction::SetProxyRunning(false))
        );
    }

    #[test]
    fn hovering_panels_reports_their_focus() {
        let app = app_with_request();
        let area = Rect::new(0, 0, 120, 40);
        let (requests, request, response) = normal_panels(area, &app);

        assert_eq!(
            panel_focus(area, &app, requests.x + 2, requests.y + 2),
            Some(Focus::MessageList)
        );
        assert_eq!(
            panel_focus(area, &app, request.x + 2, request.y + 2),
            Some(Focus::RequestSection)
        );
        assert_eq!(
            panel_focus(area, &app, response.x + 2, response.y + 2),
            Some(Focus::ResponseSection)
        );
    }
}
