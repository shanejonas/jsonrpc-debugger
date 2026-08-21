use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, HighlightSpacing, List, ListItem, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table, TableState, Wrap,
    },
    Frame,
};

use crate::app::{
    request_matches_filter, App, AppMode, EditorMode, Focus, InputMode, JsonRpcExchange,
    LineAnnotation, Overlay, TransportType,
};

const ANNOTATION_AMBER: Color = Color::Rgb(245, 166, 35);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseAction {
    EditTarget,
    EditFilter,
    SetProxyRunning(bool),
    SelectExchange(usize),
    SelectPending(usize),
    SelectRequestTab(usize),
    SelectResponseTab(usize),
    SelectLine { panel: Focus, line: usize },
    SelectAnnotation { id: String },
    SelectSession(usize),
    CloseOverlay,
    Focus(Focus),
}

pub fn panel_focus(area: Rect, app: &App, column: u16, row: u16) -> Option<Focus> {
    if app.overlay != Overlay::None {
        return None;
    }
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
    if app.panel_fullscreen {
        return Some(app.focus);
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
    if app.panel_fullscreen {
        return match focus {
            Focus::RequestSection | Focus::ResponseSection => main_height.saturating_sub(2),
            Focus::MessageList | Focus::StatusHeader => main_height.saturating_sub(3),
        };
    }
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

pub fn detail_max_source_scroll(
    app: &App,
    panel: Focus,
    total_lines: usize,
    visible_lines: usize,
) -> usize {
    let annotations = detail_annotations(app, panel);
    let block_count = annotations
        .iter()
        .filter(|annotation| annotation.start_line != annotation.end_line)
        .count();
    let max_display_scroll = (total_lines + block_count).saturating_sub(visible_lines);
    (0..total_lines)
        .take_while(|source| detail_display_scroll(*source, &annotations) <= max_display_scroll)
        .last()
        .unwrap_or(0)
}

pub fn mouse_action(area: Rect, app: &App, column: u16, row: u16) -> Option<MouseAction> {
    match app.overlay {
        Overlay::Help => return Some(MouseAction::CloseOverlay),
        Overlay::Sessions => {
            return session_at_row(area, app, column, row)
                .map(MouseAction::SelectSession)
                .or(Some(MouseAction::CloseOverlay));
        }
        Overlay::Prefix => return Some(MouseAction::CloseOverlay),
        Overlay::None => {}
    }

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
    if app.panel_fullscreen {
        return fullscreen_mouse_action(chunks[1], app, column, row);
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

fn fullscreen_mouse_action(area: Rect, app: &App, column: u16, row: u16) -> Option<MouseAction> {
    match (app.app_mode, app.focus) {
        (AppMode::Normal, Focus::MessageList) => message_list_action(area, app, row),
        (AppMode::Normal, Focus::RequestSection) => request_details_action(area, app, column, row),
        (AppMode::Normal, Focus::ResponseSection) => {
            response_details_action(area, app, column, row)
        }
        (AppMode::Paused | AppMode::Intercepting, Focus::MessageList) => {
            pending_list_action(area, app, row)
        }
        (AppMode::Paused | AppMode::Intercepting, _) => {
            Some(MouseAction::Focus(Focus::RequestSection))
        }
        (_, Focus::StatusHeader) => status_header_action(area, column, row),
    }
}

pub fn session_at_row(area: Rect, app: &App, column: u16, row: u16) -> Option<usize> {
    let popup = session_popup(area);
    if !contains(popup, column, row) {
        return None;
    }
    let first_row = popup.y.saturating_add(1);
    let visible_rows = popup.height.saturating_sub(2) as usize;
    if row < first_row || row >= first_row.saturating_add(visible_rows as u16) {
        return None;
    }
    let offset = app
        .selected_session
        .saturating_sub(visible_rows.saturating_sub(1));
    let index = offset + row.saturating_sub(first_row) as usize;
    (index < app.sessions.len()).then_some(index)
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

    let offset = app.history_scroll_offset(visible_rows);
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
            request_matches_filter(
                pending.original_request.method.as_deref(),
                pending.original_request.id.as_ref(),
                &app.filter_text,
            )
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
    let annotations = detail_annotations(app, Focus::RequestSection);
    let clicked = clicked_detail_row(
        area,
        column,
        row,
        app.request_details_scroll,
        &content,
        &annotations,
    );
    let has_request = exchange.and_then(|value| value.request.as_ref()).is_some();
    if has_request && clicked == Some(ClickedDetail::Line(tab_line + 1)) {
        return tab_action(area, column, detail_gutter_width(content.len()))
            .map(MouseAction::SelectRequestTab);
    }

    clicked
        .map(|clicked| match clicked {
            ClickedDetail::Line(line) => MouseAction::SelectLine {
                panel: Focus::RequestSection,
                line,
            },
            ClickedDetail::Annotation(id) => MouseAction::SelectAnnotation { id },
        })
        .or(Some(MouseAction::Focus(Focus::RequestSection)))
}

fn response_details_action(area: Rect, app: &App, column: u16, row: u16) -> Option<MouseAction> {
    let content = response_detail_lines(app);
    let annotations = detail_annotations(app, Focus::ResponseSection);
    let clicked = clicked_detail_row(
        area,
        column,
        row,
        app.response_details_scroll,
        &content,
        &annotations,
    );
    let has_response = app
        .get_selected_exchange()
        .and_then(|exchange| exchange.response.as_ref())
        .is_some();
    if has_response && clicked == Some(ClickedDetail::Line(2)) {
        return tab_action(area, column, detail_gutter_width(content.len()))
            .map(MouseAction::SelectResponseTab);
    }

    clicked
        .map(|clicked| match clicked {
            ClickedDetail::Line(line) => MouseAction::SelectLine {
                panel: Focus::ResponseSection,
                line,
            },
            ClickedDetail::Annotation(id) => MouseAction::SelectAnnotation { id },
        })
        .or(Some(MouseAction::Focus(Focus::ResponseSection)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClickedDetail {
    Line(usize),
    Annotation(String),
}

#[derive(Debug, Clone, Copy)]
enum DetailRow<'a> {
    Line(usize),
    Annotation(&'a LineAnnotation),
}

struct InlineAnnotation<'a> {
    annotation: &'a LineAnnotation,
    message: String,
    width: usize,
}

fn inline_annotation_layout<'a>(
    line_width: usize,
    annotations: &[&'a LineAnnotation],
    max_width: usize,
) -> Vec<InlineAnnotation<'a>> {
    const PREFIX: &str = "  ◆ ";
    let prefix_width = Line::from(PREFIX).width();
    let available = max_width.saturating_sub(line_width);
    let visible = annotations.len().min(available / prefix_width);
    if visible == 0 {
        return Vec::new();
    }

    let share = available / visible;
    annotations
        .iter()
        .take(visible)
        .map(|annotation| {
            let message = truncate_to_width(&annotation.message, share - prefix_width);
            let width = prefix_width + Line::from(message.as_str()).width();
            InlineAnnotation {
                annotation,
                message,
                width,
            }
        })
        .collect()
}

fn truncate_to_width(value: &str, max_width: usize) -> String {
    if Line::from(value).width() <= max_width {
        return value.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let mut result = String::new();
    for character in value.chars() {
        result.push(character);
        if Line::from(result.as_str()).width() >= max_width {
            result.pop();
            break;
        }
    }
    result.push('…');
    result
}

fn detail_rows<'a>(
    content_len: usize,
    annotations: &'a [&'a LineAnnotation],
) -> Vec<DetailRow<'a>> {
    let mut rows = Vec::with_capacity(content_len + annotations.len());
    for source_index in 0..content_len {
        rows.push(DetailRow::Line(source_index));
        rows.extend(
            annotations
                .iter()
                .copied()
                .filter(|annotation| {
                    annotation.start_line != annotation.end_line
                        && annotation.end_line == source_index + 1
                })
                .map(DetailRow::Annotation),
        );
    }
    rows
}

fn clicked_detail_row(
    area: Rect,
    column: u16,
    row: u16,
    scroll: usize,
    content: &[Line<'_>],
    annotations: &[&LineAnnotation],
) -> Option<ClickedDetail> {
    if row <= area.y || row >= area.y.saturating_add(area.height).saturating_sub(1) {
        return None;
    }

    let width = usize::from(area.width.saturating_sub(2)).max(1);
    let gutter_width = detail_gutter_width(content.len());
    let clicked_column = usize::from(column.saturating_sub(area.x.saturating_add(1)));
    let mut visible_row = usize::from(row.saturating_sub(area.y + 1));
    let display_scroll = detail_display_scroll(scroll, annotations);
    for detail_row in detail_rows(content.len(), annotations)
        .into_iter()
        .skip(display_scroll)
    {
        let line_width = match detail_row {
            DetailRow::Line(index) => content[index].width(),
            DetailRow::Annotation(annotation) => {
                Line::from(format!("◆ {}", annotation.message)).width()
            }
        };
        let rendered_width = line_width + gutter_width;
        let height = rendered_width.max(1).div_ceil(width);
        if visible_row < height {
            if let DetailRow::Line(index) = detail_row {
                let inline = annotations
                    .iter()
                    .copied()
                    .filter(|annotation| {
                        annotation.start_line == annotation.end_line
                            && annotation.end_line == index + 1
                    })
                    .collect::<Vec<_>>();
                let mut start = rendered_width;
                for note in inline_annotation_layout(start, &inline, width) {
                    let end = start + note.width;
                    if visible_row == 0 && (start..end).contains(&clicked_column) {
                        return Some(ClickedDetail::Annotation(note.annotation.id.clone()));
                    }
                    start = end;
                }
            }
            return Some(match detail_row {
                DetailRow::Line(index) => ClickedDetail::Line(index + 1),
                DetailRow::Annotation(annotation) => {
                    ClickedDetail::Annotation(annotation.id.clone())
                }
            });
        }
        visible_row -= height;
    }

    None
}

fn detail_annotations(app: &App, panel: Focus) -> Vec<&LineAnnotation> {
    app.visible_annotations(panel).collect()
}

fn detail_display_scroll(source_scroll: usize, annotations: &[&LineAnnotation]) -> usize {
    source_scroll
        + annotations
            .iter()
            .filter(|annotation| {
                annotation.start_line != annotation.end_line && annotation.end_line <= source_scroll
            })
            .count()
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

    if app.panel_fullscreen {
        draw_fullscreen_panel(f, chunks[1], app);
    } else {
        match app.app_mode {
            AppMode::Normal => draw_main_content(f, chunks[1], app),
            AppMode::Paused | AppMode::Intercepting => draw_intercept_content(f, chunks[1], app),
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
    } else {
        match app.input_mode {
            InputMode::FilteringRequests => draw_input_dialog(f, app, "Filter Requests", "Filter"),
            InputMode::AnnotatingSelection => {
                let title = if app.selection_overlaps_annotation() {
                    "Add Another Annotation"
                } else {
                    "Add Annotation"
                };
                draw_input_dialog(f, app, title, "Message")
            }
            InputMode::NamingSession => draw_input_dialog(f, app, "New Session", "Name (optional)"),
            InputMode::RenamingSession => draw_input_dialog(f, app, "Rename Session", "Name"),
            InputMode::Normal | InputMode::EditingTarget => {}
        }
    }

    match app.overlay {
        Overlay::Help => draw_keybind_help(f),
        Overlay::Sessions => draw_sessions(f, app),
        Overlay::None | Overlay::Prefix => {}
    }
}

fn draw_keybind_help(f: &mut Frame) {
    let popup = centered_popup(f.size(), 90, 65);
    let lines = vec![
        Line::from(Span::styled(
            "Global commands",
            Style::default().fg(Color::Cyan),
        )),
        Line::from("^B s  sessions       ^B n  new session"),
        Line::from("^B R  rename session"),
        Line::from("^B a  annotate visual selection"),
        Line::from("^B c  create request ^B p  pause interception"),
        Line::from("^B t  target         ^B x  start/stop proxy"),
        Line::from("^B z  fullscreen panel"),
        Line::from("^B d  delete focused annotation"),
        Line::from("^B q  quit           ^B ?  this help"),
        Line::from(""),
        Line::from(Span::styled(
            "Focused pending request",
            Style::default().fg(Color::Cyan),
        )),
        Line::from("a allow   b block   e body   h headers"),
        Line::from("c complete   r resume all"),
        Line::from(""),
        Line::from(Span::styled("Navigation", Style::default().fg(Color::Cyan))),
        Line::from("↑/↓ or j/k navigate   Tab focus   h/l tabs   / filter"),
        Line::from("d/u page   g/G top/bottom   Enter copy Markdown"),
        Line::from("Details: v visual select   j/k extend   Esc clear"),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Keybinds")
        .border_style(Style::default().fg(Color::Yellow));
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(lines).block(block), popup);
}

fn draw_sessions(f: &mut Frame, app: &App) {
    let popup = session_popup(f.size());
    let active = app.session.as_ref().map(|session| session.id.as_str());
    let items = app
        .sessions
        .iter()
        .map(|session| {
            let marker = if active == Some(session.id.as_str()) {
                "●"
            } else {
                " "
            };
            let target = if session.target.is_empty() {
                "no target"
            } else {
                session.target.as_str()
            };
            ListItem::new(format!(
                "{marker} {}  {} exchanges  {target}",
                session.name, session.exchange_count
            ))
        })
        .collect::<Vec<_>>();
    let mut state = ratatui::widgets::ListState::default();
    state.select((!items.is_empty()).then_some(app.selected_session));
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Sessions — Enter/click open · Esc close"),
        )
        .highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black))
        .highlight_symbol("› ");
    f.render_widget(Clear, popup);
    f.render_stateful_widget(list, popup, &mut state);
}

fn session_popup(area: Rect) -> Rect {
    centered_popup(area, 82, 70)
}

fn centered_popup(area: Rect, width_percent: u16, height_percent: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100 - height_percent) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1])[1]
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

fn draw_fullscreen_panel(f: &mut Frame, area: Rect, app: &App) {
    match (app.app_mode, app.focus) {
        (AppMode::Normal, Focus::MessageList) => draw_message_list(f, area, app),
        (AppMode::Normal, Focus::RequestSection) => draw_request_details(f, area, app),
        (AppMode::Normal, Focus::ResponseSection) => draw_response_details(f, area, app),
        (AppMode::Paused | AppMode::Intercepting, Focus::MessageList) => {
            draw_pending_requests(f, area, app)
        }
        (AppMode::Paused | AppMode::Intercepting, _) => {
            draw_intercept_request_details(f, area, app)
        }
        (_, Focus::StatusHeader) => draw_status_header(f, area, app),
    }
}

fn draw_message_list(f: &mut Frame, area: Rect, app: &App) {
    let filtered: Vec<(usize, &JsonRpcExchange)> = app
        .filtered_exchange_indices()
        .into_iter()
        .map(|index| (index, &app.exchanges[index]))
        .collect();

    if filtered.is_empty() {
        let empty_message = if app.is_running {
            format!(
                "Proxy is running on port {}. Waiting for requests...",
                app.proxy_config.listen_port
            )
        } else {
            "Press Ctrl-B x to start the proxy and begin capturing messages".to_string()
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
    let visible_rows = area.height.saturating_sub(3) as usize;
    let offset = app.history_scroll_offset(visible_rows);

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
        .skip(offset)
        .take(visible_rows)
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
    .highlight_symbol("  ")
    .highlight_spacing(HighlightSpacing::Always);

    let mut table_state = TableState::default();
    table_state.select(
        selected_position
            .checked_sub(offset)
            .filter(|position| *position < visible_rows),
    );
    f.render_stateful_widget(table, area, &mut table_state);

    if filtered.len() > 1 {
        let mut scrollbar_state = ScrollbarState::new(filtered.len())
            .position(offset)
            .viewport_content_length(visible_rows);

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

pub fn detail_lines_text_at(
    app: &App,
    panel: Focus,
    exchange_index: usize,
    tab: crate::app::DetailTab,
) -> Option<Vec<String>> {
    let exchange = app.exchanges.get(exchange_index);
    let tab = usize::from(tab == crate::app::DetailTab::Body);
    let lines = match panel {
        Focus::RequestSection => request_detail_lines_for(exchange, tab, false),
        Focus::ResponseSection => response_detail_lines_for(exchange, tab, false),
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

fn annotate_detail_lines(lines: Vec<Line<'static>>, app: &App, panel: Focus) -> Vec<Line<'static>> {
    let annotations = detail_annotations(app, panel);
    if annotations.is_empty() {
        return lines;
    }

    let range_style = Style::default().bg(Color::Rgb(44, 34, 14));
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let line_number = index + 1;
            if !annotations.iter().any(|annotation| {
                (annotation.start_line..=annotation.end_line).contains(&line_number)
            }) {
                return line;
            }
            line.patch_style(range_style)
        })
        .collect()
}

fn insert_annotation_lines(
    lines: Vec<Line<'static>>,
    app: &App,
    panel: Focus,
    width: usize,
) -> Vec<Line<'static>> {
    let annotations = detail_annotations(app, panel);
    if annotations.is_empty() {
        return lines;
    }

    let number_width = lines.len().max(1).to_string().len();
    let mut displayed = Vec::with_capacity(lines.len() + annotations.len());
    for (source_index, mut line) in lines.into_iter().enumerate() {
        let inline = annotations
            .iter()
            .copied()
            .filter(|annotation| {
                annotation.start_line == annotation.end_line
                    && annotation.end_line == source_index + 1
            })
            .collect::<Vec<_>>();
        for note in inline_annotation_layout(line.width(), &inline, width) {
            line.spans.extend([
                Span::styled("  ◆ ", Style::default().fg(ANNOTATION_AMBER)),
                Span::styled(note.message, Style::default().fg(ANNOTATION_AMBER)),
            ]);
        }
        displayed.push(line);
        for annotation in annotations.iter().filter(|annotation| {
            annotation.start_line != annotation.end_line && annotation.end_line == source_index + 1
        }) {
            displayed.push(Line::from(vec![
                Span::styled(
                    format!(" {:>number_width$} ╰─", ""),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    "◆ ",
                    Style::default()
                        .fg(ANNOTATION_AMBER)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    annotation.message.clone(),
                    Style::default().fg(ANNOTATION_AMBER),
                ),
            ]));
        }
    }
    displayed
}

fn number_detail_lines(
    lines: Vec<Line<'static>>,
    cursor_line: Option<usize>,
) -> Vec<Line<'static>> {
    let number_width = lines.len().max(1).to_string().len();
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let line_number = index + 1;
            let cursor = if cursor_line == Some(line_number) {
                Span::styled(
                    "›",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw(" ")
            };
            let mut spans = vec![
                cursor,
                Span::styled(
                    format!("{line_number:>number_width$} │ "),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            spans.extend(line.spans);
            Line::from(spans).style(line.style)
        })
        .collect()
}

fn detail_gutter_width(line_count: usize) -> usize {
    line_count.max(1).to_string().len() + 4
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
    request_detail_lines_for(
        app.get_selected_exchange(),
        app.request_tab,
        matches!(app.focus, Focus::RequestSection),
    )
}

fn request_detail_lines_for(
    exchange: Option<&crate::app::JsonRpcExchange>,
    tab: usize,
    focused: bool,
) -> Vec<Line<'static>> {
    if let Some(exchange) = exchange {
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
            tab,
            focused,
            exchange.request.is_some(),
        ));

        if let Some(request) = &exchange.request {
            if tab == 0 {
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
    let inner_area = area.inner(&Margin {
        vertical: 1,
        horizontal: 1,
    });
    let cursor_line = app
        .detail_cursor_line(Focus::RequestSection)
        .filter(|_| app.focus == Focus::RequestSection);
    let content = annotate_detail_lines(request_detail_lines(app), app, Focus::RequestSection);
    let content = highlight_selected_lines(content, app, Focus::RequestSection);
    let content = number_detail_lines(content, cursor_line);
    let source_lines = content.len();
    let content = insert_annotation_lines(
        content,
        app,
        Focus::RequestSection,
        usize::from(inner_area.width),
    );

    // Calculate visible area for scrolling
    let visible_lines = inner_area.height as usize;
    let total_lines = content.len();

    // Apply scrolling offset
    let max_scroll = total_lines.saturating_sub(visible_lines);
    let annotations = detail_annotations(app, Focus::RequestSection);
    let start_line =
        detail_display_scroll(app.request_details_scroll, &annotations).min(max_scroll);
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
        draw_annotation_scrollbar_markers(f, area, &annotations, source_lines);
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
    response_detail_lines_for(
        app.get_selected_exchange(),
        app.response_tab,
        matches!(app.focus, Focus::ResponseSection),
    )
}

fn response_detail_lines_for(
    exchange: Option<&crate::app::JsonRpcExchange>,
    tab: usize,
    focused: bool,
) -> Vec<Line<'static>> {
    if let Some(exchange) = exchange {
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
            tab,
            focused,
            exchange.response.is_some(),
        ));

        if let Some(response) = &exchange.response {
            if tab == 0 {
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
    let inner_area = area.inner(&Margin {
        vertical: 1,
        horizontal: 1,
    });
    let cursor_line = app
        .detail_cursor_line(Focus::ResponseSection)
        .filter(|_| app.focus == Focus::ResponseSection);
    let content = annotate_detail_lines(response_detail_lines(app), app, Focus::ResponseSection);
    let content = highlight_selected_lines(content, app, Focus::ResponseSection);
    let content = number_detail_lines(content, cursor_line);
    let source_lines = content.len();
    let content = insert_annotation_lines(
        content,
        app,
        Focus::ResponseSection,
        usize::from(inner_area.width),
    );

    // Calculate visible area for scrolling
    let visible_lines = inner_area.height as usize;
    let total_lines = content.len();

    // Apply scrolling offset
    let max_scroll = total_lines.saturating_sub(visible_lines);
    let annotations = detail_annotations(app, Focus::ResponseSection);
    let start_line =
        detail_display_scroll(app.response_details_scroll, &annotations).min(max_scroll);
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
        draw_annotation_scrollbar_markers(f, area, &annotations, source_lines);
    }
}

fn draw_annotation_scrollbar_markers(
    f: &mut Frame,
    area: Rect,
    annotations: &[&LineAnnotation],
    total_lines: usize,
) {
    let rail = area.inner(&Margin {
        vertical: 1,
        horizontal: 0,
    });
    if rail.width == 0 || rail.height == 0 {
        return;
    }

    let x = rail.x + rail.width - 1;
    for row in annotation_scrollbar_rows(rail.height, annotations, total_lines) {
        f.render_widget(
            Paragraph::new(Span::styled(
                "▐",
                Style::default()
                    .fg(ANNOTATION_AMBER)
                    .add_modifier(Modifier::BOLD),
            )),
            Rect::new(x, rail.y + row, 1, 1),
        );
    }
}

fn annotation_scrollbar_rows(
    rail_height: u16,
    annotations: &[&LineAnnotation],
    total_lines: usize,
) -> Vec<u16> {
    if rail_height == 0 || total_lines == 0 {
        return Vec::new();
    }

    let rail_end = usize::from(rail_height.saturating_sub(1));
    let document_end = total_lines.saturating_sub(1).max(1);
    let mut rows = annotations
        .iter()
        .map(|annotation| {
            let line = annotation.end_line.clamp(1, total_lines) - 1;
            (line * rail_end / document_end) as u16
        })
        .collect::<Vec<_>>();
    rows.sort_unstable();
    rows.dedup();
    rows
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
    if app.overlay == Overlay::Prefix {
        let mut keybinds = vec![
            KeybindInfo::new("?", "keybinds", 1),
            KeybindInfo::new("s", "sessions", 1),
            KeybindInfo::new("n", "new session", 1),
            KeybindInfo::new("R", "rename session", 1),
            KeybindInfo::new("c", "create request", 1),
            KeybindInfo::new("p", "pause", 1),
            KeybindInfo::new("t", "target", 1),
            KeybindInfo::new("x", "start/stop", 1),
            KeybindInfo::new(
                "z",
                if app.panel_fullscreen {
                    "restore panels"
                } else {
                    "fullscreen panel"
                },
                1,
            ),
            KeybindInfo::new("q", "quit", 1),
            KeybindInfo::new("Esc", "cancel", 1),
        ];
        if app.annotation_to_delete().is_some() {
            keybinds.insert(1, KeybindInfo::new("d", "delete annotation", 1));
        }
        if app.visual_selection_active && app.line_selection.is_some() {
            let description = if app.selection_overlaps_annotation() {
                "add another annotation"
            } else {
                "add annotation"
            };
            keybinds.insert(1, KeybindInfo::new("a", description, 1));
        }
        return keybinds;
    }
    if matches!(app.overlay, Overlay::Help | Overlay::Sessions) {
        return vec![KeybindInfo::new("Esc", "close", 1)];
    }

    let mut keybinds = vec![
        KeybindInfo::new("^B", "commands", 1),
        KeybindInfo::new("↑↓/j/k", "navigate", 1),
        KeybindInfo::new("Tab", "focus", 1),
        KeybindInfo::new("Enter", "copy markdown", 1),
        KeybindInfo::new("/", "filter", 2),
        KeybindInfo::new("h/l", "tabs", 2),
        KeybindInfo::new("d/u/g/G", "scroll", 2),
    ];

    if app.app_mode == AppMode::Normal
        && matches!(app.focus, Focus::RequestSection | Focus::ResponseSection)
    {
        keybinds.extend([
            KeybindInfo::new("v", "visual select", 1),
            KeybindInfo::new("Esc", "clear selection", 2),
        ]);
    }

    // Add context-specific keybinds (priority 4)
    match app.app_mode {
        AppMode::Paused | AppMode::Intercepting => {
            if !app.pending_requests.is_empty() {
                keybinds.extend(vec![
                    KeybindInfo::new("a", "allow", 3),
                    KeybindInfo::new("e", "edit", 3),
                    KeybindInfo::new("h", "headers", 3),
                    KeybindInfo::new("c", "complete", 3),
                    KeybindInfo::new("b", "block", 3),
                    KeybindInfo::new("r", "resume", 3),
                ]);
            }
        }
        AppMode::Normal => {}
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
            request_matches_filter(
                pending.original_request.method.as_deref(),
                pending.original_request.id.as_ref(),
                &app.filter_text,
            )
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
    use crate::app::{DetailTab, JsonRpcMessage, MessageDirection, SessionSummary};
    use ratatui::{backend::TestBackend, Terminal};

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

    fn annotation(
        id: &str,
        panel: Focus,
        start_line: usize,
        end_line: usize,
        message: &str,
        text: Vec<String>,
    ) -> LineAnnotation {
        LineAnnotation {
            id: id.to_string(),
            exchange_index: 0,
            panel,
            tab: DetailTab::Body,
            start_line,
            end_line,
            message: message.to_string(),
            text,
        }
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
    fn clicking_a_session_row_selects_it() {
        let mut app = App::new();
        app.show_sessions(vec![SessionSummary {
            id: "saved".to_string(),
            name: "Saved".to_string(),
            target: "http://node".to_string(),
            created_at_ms: 1,
            updated_at_ms: 2,
            exchange_count: 3,
        }]);
        let area = Rect::new(0, 0, 120, 40);
        let popup = session_popup(area);

        assert_eq!(
            mouse_action(area, &app, popup.x + 2, popup.y + 1),
            Some(MouseAction::SelectSession(0))
        );
        assert_eq!(
            mouse_action(area, &app, 0, 0),
            Some(MouseAction::CloseOverlay)
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
    fn multiline_annotation_renders_on_its_own_amber_row_without_changing_panel_text() {
        let mut app = app_with_request();
        let text = detail_line_text(&app, Focus::RequestSection, 2, 3).unwrap();
        app.add_annotation(annotation(
            "annotation-1",
            Focus::RequestSection,
            2,
            3,
            "Method and id must agree",
            text,
        ));

        let annotated =
            annotate_detail_lines(request_detail_lines(&app), &app, Focus::RequestSection);
        let selected = highlight_selected_lines(annotated.clone(), &app, Focus::RequestSection);
        let numbered = number_detail_lines(selected.clone(), Some(3));
        let displayed = insert_annotation_lines(numbered, &app, Focus::RequestSection, 120);

        assert_eq!(
            detail_lines_text(&app, Focus::RequestSection).unwrap()[2],
            "ID: 1"
        );
        assert_eq!(line_text(&annotated[2]), "ID: 1");
        assert!(line_text(&displayed[3]).ends_with("◆ Method and id must agree"));
        assert_eq!(displayed.len(), annotated.len() + 1);
        assert_eq!(annotated[1].style.bg, Some(Color::Rgb(44, 34, 14)));
        assert_eq!(annotated[2].style.bg, Some(Color::Rgb(44, 34, 14)));
        assert_eq!(selected[1].style.bg, Some(Color::Rgb(44, 34, 14)));
        assert_eq!(selected[2].style.bg, Some(Color::Rgb(44, 34, 14)));
        assert_eq!(
            displayed[3].spans[1].style.fg,
            Some(Color::Rgb(245, 166, 35))
        );
    }

    #[test]
    fn annotations_remain_when_the_highlight_moves() {
        let mut app = app_with_request();
        app.add_annotation(annotation(
            "annotation-1",
            Focus::RequestSection,
            2,
            2,
            "Check the method",
            vec!["Method: eth_call".to_string()],
        ));
        app.add_annotation(annotation(
            "annotation-2",
            Focus::RequestSection,
            3,
            3,
            "Check the id",
            vec!["ID: 1".to_string()],
        ));
        let text = detail_line_text(&app, Focus::RequestSection, 4, 4).unwrap();
        app.select_lines(Focus::RequestSection, 4, 4, text);

        let annotated =
            annotate_detail_lines(request_detail_lines(&app), &app, Focus::RequestSection);
        let numbered = number_detail_lines(annotated.clone(), Some(4));
        let displayed = insert_annotation_lines(numbered, &app, Focus::RequestSection, 120);

        assert_eq!(app.annotations.len(), 2);
        assert_eq!(app.active_annotation_id, None);
        assert_eq!(displayed.len(), annotated.len());
        assert!(line_text(&displayed[1]).ends_with("◆ Check the method"));
        assert!(line_text(&displayed[2]).ends_with("◆ Check the id"));
    }

    #[test]
    fn single_line_annotations_stack_inline() {
        let mut app = app_with_request();
        app.add_annotation(annotation(
            "annotation-1",
            Focus::RequestSection,
            2,
            2,
            "Check the method",
            vec!["Method: eth_call".to_string()],
        ));
        app.add_annotation(annotation(
            "annotation-2",
            Focus::RequestSection,
            2,
            2,
            "Compare the name",
            vec!["Method: eth_call".to_string()],
        ));
        let numbered = number_detail_lines(request_detail_lines(&app), Some(2));
        let displayed = insert_annotation_lines(numbered, &app, Focus::RequestSection, 80);
        let line = line_text(&displayed[1]);

        assert_eq!(displayed.len(), request_detail_lines(&app).len());
        assert!(line.contains("◆ Check the method"));
        assert!(line.contains("◆ Compare the name"));
    }

    #[test]
    fn single_line_annotation_renders_inline_in_amber() {
        let mut app = app_with_request();
        app.add_annotation(annotation(
            "annotation-1",
            Focus::RequestSection,
            2,
            2,
            "Check the method",
            vec!["Method: eth_call".to_string()],
        ));
        app.request_details_scroll = 0;
        let area = Rect::new(0, 0, 80, 8);
        let numbered = number_detail_lines(request_detail_lines(&app), Some(2));
        let marker_x = 1 + numbered[1].width() as u16 + 2;
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();

        terminal
            .draw(|frame| draw_request_details(frame, area, &app))
            .unwrap();

        let marker = terminal.backend().buffer().get(marker_x, 2);
        assert_eq!(marker.symbol(), "◆");
        assert_eq!(marker.fg, ANNOTATION_AMBER);
    }

    #[test]
    fn narrow_inline_annotation_is_truncated_without_wrapping() {
        let note = annotation(
            "annotation-1",
            Focus::RequestSection,
            2,
            2,
            "Long annotation",
            vec!["Method: eth_call".to_string()],
        );
        let layout = inline_annotation_layout(10, &[&note], 20);

        assert_eq!(layout.len(), 1);
        assert_eq!(layout[0].message, "Long …");
        assert_eq!(layout[0].width, 10);
    }

    #[test]
    fn annotation_scrollbar_markers_map_document_positions_to_the_rail() {
        let top = annotation(
            "top",
            Focus::RequestSection,
            1,
            1,
            "top",
            vec!["top".to_string()],
        );
        let middle = annotation(
            "middle",
            Focus::RequestSection,
            50,
            51,
            "middle",
            vec!["middle".to_string()],
        );
        let same_row = annotation(
            "same-row",
            Focus::RequestSection,
            51,
            51,
            "same row",
            vec!["middle".to_string()],
        );
        let bottom = annotation(
            "bottom",
            Focus::RequestSection,
            101,
            101,
            "bottom",
            vec!["bottom".to_string()],
        );

        assert_eq!(
            annotation_scrollbar_rows(11, &[&top, &middle, &same_row, &bottom], 101),
            vec![0, 5, 10]
        );
    }

    #[test]
    fn annotation_scrollbar_marker_renders_in_amber() {
        let mut app = app_with_request();
        app.add_annotation(annotation(
            "marker",
            Focus::RequestSection,
            2,
            2,
            "marked",
            vec!["Method: eth_call".to_string()],
        ));
        let area = Rect::new(0, 0, 40, 6);
        let total_lines = request_detail_lines(&app).len();
        let row = annotation_scrollbar_rows(4, &[&app.annotations[0]], total_lines)[0];
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();

        terminal
            .draw(|frame| draw_request_details(frame, area, &app))
            .unwrap();

        let marker = terminal.backend().buffer().get(area.width - 1, 1 + row);
        assert_eq!(marker.symbol(), "▐");
        assert_eq!(marker.fg, ANNOTATION_AMBER);
    }

    #[test]
    fn prefix_offers_add_annotation_only_during_visual_selection() {
        let mut app = app_with_request();
        app.focus = Focus::RequestSection;
        app.show_prefix();
        assert!(!get_keybinds_for_mode(&app)
            .iter()
            .any(|keybind| keybind.key == "a"));

        app.close_overlay();
        let text = detail_line_text(&app, Focus::RequestSection, 2, 2).unwrap();
        app.select_lines(Focus::RequestSection, 2, 2, text);
        app.start_visual_selection();
        app.show_prefix();
        assert_eq!(
            get_keybinds_for_mode(&app)
                .iter()
                .find(|keybind| keybind.key == "a")
                .map(|keybind| keybind.description.as_str()),
            Some("add annotation")
        );

        app.close_overlay();
        app.add_annotation(annotation(
            "existing",
            Focus::RequestSection,
            2,
            2,
            "Existing note",
            vec!["Method: eth_call".to_string()],
        ));
        let text = detail_line_text(&app, Focus::RequestSection, 2, 2).unwrap();
        app.select_lines(Focus::RequestSection, 2, 2, text);
        app.start_visual_selection();
        app.show_prefix();
        assert_eq!(
            get_keybinds_for_mode(&app)
                .iter()
                .find(|keybind| keybind.key == "a")
                .map(|keybind| keybind.description.as_str()),
            Some("add another annotation")
        );

        app.finish_visual_selection();
        assert!(!get_keybinds_for_mode(&app)
            .iter()
            .any(|keybind| keybind.key == "a"));
    }

    #[test]
    fn annotation_row_does_not_change_mouse_line_numbers() {
        let area = Rect::new(0, 0, 40, 10);
        let content = vec![
            Line::from("one"),
            Line::from("two"),
            Line::from("three"),
            Line::from("four"),
        ];
        let annotation = annotation(
            "annotation-1",
            Focus::RequestSection,
            1,
            2,
            "note",
            vec!["one".to_string(), "two".to_string()],
        );
        let annotations = vec![&annotation];

        assert_eq!(
            clicked_detail_row(area, 1, 1, 0, &content, &annotations),
            Some(ClickedDetail::Line(1))
        );
        assert_eq!(
            clicked_detail_row(area, 1, 2, 0, &content, &annotations),
            Some(ClickedDetail::Line(2))
        );
        assert_eq!(
            clicked_detail_row(area, 1, 3, 0, &content, &annotations),
            Some(ClickedDetail::Annotation("annotation-1".to_string()))
        );
        assert_eq!(
            clicked_detail_row(area, 1, 4, 0, &content, &annotations),
            Some(ClickedDetail::Line(3))
        );
        assert_eq!(
            clicked_detail_row(area, 1, 1, 2, &content, &annotations),
            Some(ClickedDetail::Line(3))
        );
    }

    #[test]
    fn clicking_inline_annotation_text_focuses_the_annotation() {
        let area = Rect::new(0, 0, 40, 10);
        let content = vec![Line::from("one"), Line::from("two")];
        let annotation = annotation(
            "annotation-1",
            Focus::RequestSection,
            2,
            2,
            "note",
            vec!["two".to_string()],
        );
        let annotations = vec![&annotation];

        assert_eq!(
            clicked_detail_row(area, 9, 2, 0, &content, &annotations),
            Some(ClickedDetail::Annotation("annotation-1".to_string()))
        );
        assert_eq!(
            clicked_detail_row(area, 2, 2, 0, &content, &annotations),
            Some(ClickedDetail::Line(2))
        );
    }

    #[test]
    fn annotation_at_the_bottom_remains_scrollable() {
        let mut app = app_with_request();
        app.add_annotation(annotation(
            "annotation-1",
            Focus::RequestSection,
            3,
            4,
            "bottom note",
            vec!["three".to_string(), "four".to_string()],
        ));

        assert_eq!(
            detail_max_source_scroll(&app, Focus::RequestSection, 4, 2),
            3
        );
    }

    #[test]
    fn visible_line_numbers_match_get_panel_without_changing_its_text() {
        let app = app_with_request();
        let raw = detail_lines_text(&app, Focus::RequestSection).unwrap();
        let numbered = number_detail_lines(request_detail_lines(&app), Some(2));

        assert_eq!(raw[1], "Method: eth_call");
        assert_eq!(line_text(&numbered[1]), "› 2 │ Method: eth_call");
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

    #[test]
    fn fullscreen_draws_only_the_focused_panel_in_the_main_area() {
        let mut app = app_with_request();
        app.set_focus(Focus::ResponseSection);
        let area = Rect::new(0, 0, 120, 40);
        let split_visible_lines = panel_visible_lines(area, &app, app.focus);
        app.set_panel_fullscreen(true);

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let main = screen_chunks(area, &app)[1];
        let title = (0..area.width).fold(String::new(), |mut title, column| {
            title.push_str(terminal.backend().buffer().get(column, main.y).symbol());
            title
        });
        assert!(title.contains("Response Details"));
        assert!(!title.contains("Requests"));
        assert!(panel_visible_lines(area, &app, app.focus) > split_visible_lines);
        assert_eq!(
            panel_focus(area, &app, main.x + 2, main.y + 2),
            Some(Focus::ResponseSection)
        );
    }

    #[test]
    fn clicking_a_scrolled_request_uses_the_viewport_offset() {
        let mut app = App::new();
        for id in 0..6 {
            app.add_message(JsonRpcMessage {
                id: Some(serde_json::json!(id)),
                method: Some(format!("request_{id}")),
                params: Some(serde_json::json!([])),
                result: None,
                error: None,
                timestamp: std::time::SystemTime::now(),
                direction: MessageDirection::Request,
                transport: TransportType::Http,
                headers: None,
            });
        }
        app.history_scroll = Some(3);
        let area = Rect::new(0, 0, 80, 5);

        assert_eq!(
            message_list_action(area, &app, 2),
            Some(MouseAction::SelectExchange(3))
        );
    }

    #[test]
    fn request_columns_do_not_shift_when_selection_scrolls_offscreen() {
        let mut app = App::new();
        for id in 0..6 {
            app.add_message(JsonRpcMessage {
                id: Some(serde_json::json!(id)),
                method: Some(format!("request_{id}")),
                params: Some(serde_json::json!([])),
                result: None,
                error: None,
                timestamp: std::time::SystemTime::now(),
                direction: MessageDirection::Request,
                transport: TransportType::Http,
                headers: None,
            });
        }
        let area = Rect::new(0, 0, 80, 5);
        let header_x = |app: &App| {
            let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            terminal
                .draw(|frame| draw_message_list(frame, area, app))
                .unwrap();
            (0..area.width)
                .find(|column| terminal.backend().buffer().get(*column, 1).symbol() == "S")
                .unwrap()
        };

        app.history_scroll = Some(0);
        let selected_header_x = header_x(&app);
        app.history_scroll = Some(3);
        let scrolled_header_x = header_x(&app);

        assert_eq!(selected_header_x, scrolled_header_x);
    }
}
