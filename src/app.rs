use std::{collections::HashMap, ffi::OsString};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone)]
pub struct JsonRpcMessage {
    pub id: Option<serde_json::Value>,
    pub method: Option<String>,
    pub params: Option<serde_json::Value>,
    pub result: Option<serde_json::Value>,
    pub error: Option<serde_json::Value>,
    pub timestamp: std::time::SystemTime,
    pub direction: MessageDirection,
    pub transport: TransportType,
    pub headers: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone)]
pub struct JsonRpcExchange {
    pub id: Option<serde_json::Value>,
    pub method: Option<String>,
    pub request: Option<JsonRpcMessage>,
    pub response: Option<JsonRpcMessage>,
    #[allow(dead_code)] // Used in UI for duration calculation
    pub timestamp: std::time::SystemTime,
    pub transport: TransportType,
}

impl JsonRpcExchange {
    pub fn is_notification(&self) -> bool {
        self.request
            .as_ref()
            .is_some_and(|request| request.id.is_none() && request.method.is_some())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageDirection {
    Request,
    Response,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    JsonLines,
    ContentLength,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportType {
    Http,
    HttpBatch,
    Stdio(Framing),
    #[allow(dead_code)] // Used in tests and UI display
    WebSocket,
}

impl TransportType {
    pub fn label(self) -> &'static str {
        match self {
            Self::Http => "HTTP",
            Self::HttpBatch => "HTTP-BATCH",
            Self::Stdio(Framing::JsonLines) => "STDIO/JSONL",
            Self::Stdio(Framing::ContentLength) => "STDIO/LSP",
            Self::WebSocket => "WebSocket",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::HttpBatch => "http-batch",
            Self::Stdio(Framing::JsonLines) => "stdio-json-lines",
            Self::Stdio(Framing::ContentLength) => "stdio-content-length",
            Self::WebSocket => "websocket",
        }
    }
}

pub fn json_rpc_messages(
    body: &serde_json::Value,
    direction: MessageDirection,
    transport: TransportType,
    headers: Option<&HashMap<String, String>>,
) -> Vec<JsonRpcMessage> {
    body.as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(body))
        .iter()
        .map(|body| json_rpc_message(body, direction, transport, headers))
        .collect()
}

pub fn incoming_json_rpc_messages(
    body: &serde_json::Value,
    transport: TransportType,
) -> Vec<JsonRpcMessage> {
    json_rpc_messages_by_shape(body, transport, None)
}

pub fn json_rpc_messages_by_shape(
    body: &serde_json::Value,
    transport: TransportType,
    headers: Option<&HashMap<String, String>>,
) -> Vec<JsonRpcMessage> {
    body.as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(body))
        .iter()
        .map(|body| {
            let direction = if body.get("method").is_some() {
                MessageDirection::Request
            } else {
                MessageDirection::Response
            };
            json_rpc_message(body, direction, transport, headers)
        })
        .collect()
}

fn json_rpc_message(
    body: &serde_json::Value,
    direction: MessageDirection,
    transport: TransportType,
    headers: Option<&HashMap<String, String>>,
) -> JsonRpcMessage {
    let (method, params, result, error) = match direction {
        MessageDirection::Request => (
            body.get("method")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            body.get("params").cloned(),
            None,
            None,
        ),
        MessageDirection::Response => (
            None,
            None,
            body.get("result").cloned(),
            body.get("error").cloned(),
        ),
    };
    JsonRpcMessage {
        id: body.get("id").cloned(),
        method,
        params,
        result,
        error,
        timestamp: std::time::SystemTime::now(),
        direction,
        transport,
        headers: headers.cloned(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputMode {
    Normal,
    EditingTarget,
    FilteringRequests,
    AnnotatingSelection,
    NamingSession,
    RenamingSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    MessageList,
    RequestSection,
    ResponseSection,
    StatusHeader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Headers,
    Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Prefix,
    Help,
    Sessions,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub name: String,
    pub target: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub exchange_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineSelection {
    pub panel: Focus,
    pub anchor_line: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub text: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineAnnotation {
    pub id: String,
    pub exchange_index: usize,
    pub panel: Focus,
    pub tab: DetailTab,
    pub start_line: usize,
    pub end_line: usize,
    pub message: String,
    pub text: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorTarget {
    PendingRequest,
    PendingHeaders,
    PendingResponse,
    NewRequest,
}

impl EditorTarget {
    pub fn title(self) -> &'static str {
        match self {
            Self::PendingRequest => "Edit Request",
            Self::PendingHeaders => "Edit Headers",
            Self::PendingResponse => "Complete Request",
            Self::NewRequest => "New Request",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorMode {
    Normal,
    Insert,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorOperator {
    Change,
    Delete,
    Yank,
}

impl EditorOperator {
    pub fn key(self) -> char {
        match self {
            Self::Change => 'c',
            Self::Delete => 'd',
            Self::Yank => 'y',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorMotion {
    Left,
    Right,
    WordForward,
    WordBackward,
    WordEnd,
    LineStart,
    LineEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorSnapshot {
    content: String,
    row: usize,
    column: usize,
}

#[derive(Debug, Clone)]
enum EditorRegister {
    Empty,
    Characters(String),
    Lines(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordClass {
    Keyword,
    Punctuation,
    Whitespace,
}

#[derive(Debug, Clone)]
pub struct TextEditor {
    pub target: EditorTarget,
    pub lines: Vec<String>,
    pub row: usize,
    pub column: usize,
    pub mode: EditorMode,
    pub command: String,
    pub error: Option<String>,
    pub pending_operator: Option<EditorOperator>,
    pub pending_g: bool,
    register: EditorRegister,
    undo: Vec<EditorSnapshot>,
    insert_snapshot: Option<EditorSnapshot>,
}

impl TextEditor {
    pub fn new(target: EditorTarget, content: String) -> Self {
        let lines = content.split('\n').map(str::to_string).collect();

        Self {
            target,
            lines,
            row: 0,
            column: 0,
            mode: EditorMode::Normal,
            command: String::new(),
            error: None,
            pending_operator: None,
            pending_g: false,
            register: EditorRegister::Empty,
            undo: Vec::new(),
            insert_snapshot: None,
        }
    }

    pub fn content(&self) -> String {
        self.lines.join("\n")
    }

    pub fn move_left(&mut self) {
        self.column = self.column.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.column = (self.column + 1).min(self.line_length());
    }

    pub fn move_up(&mut self) {
        self.row = self.row.saturating_sub(1);
        self.clamp_column();
    }

    pub fn move_down(&mut self) {
        self.row = (self.row + 1).min(self.lines.len() - 1);
        self.clamp_column();
    }

    pub fn move_to_start(&mut self) {
        self.column = 0;
    }

    pub fn move_to_end(&mut self) {
        self.column = self.line_length();
    }

    pub fn move_to_first_non_blank(&mut self) {
        self.column = self.lines[self.row]
            .chars()
            .position(|character| !character.is_whitespace())
            .unwrap_or(0);
    }

    pub fn move_to_top(&mut self) {
        self.row = 0;
        self.clamp_column();
    }

    pub fn move_to_bottom(&mut self) {
        self.row = self.lines.len() - 1;
        self.clamp_column();
    }

    pub fn move_word_forward(&mut self) {
        let characters = self.content().chars().collect::<Vec<_>>();
        let offset = next_word_start(&characters, self.cursor_offset());
        self.set_cursor_offset(offset);
    }

    pub fn move_word_backward(&mut self) {
        let characters = self.content().chars().collect::<Vec<_>>();
        let offset = previous_word_start(&characters, self.cursor_offset());
        self.set_cursor_offset(offset);
    }

    pub fn move_word_end(&mut self) {
        let characters = self.content().chars().collect::<Vec<_>>();
        let offset = next_word_end(&characters, self.cursor_offset());
        self.set_cursor_offset(offset);
    }

    pub fn move_with(&mut self, motion: EditorMotion) {
        match motion {
            EditorMotion::Left => self.move_left(),
            EditorMotion::Right => {
                self.column = (self.column + 1).min(self.line_length().saturating_sub(1));
            }
            EditorMotion::WordForward => self.move_word_forward(),
            EditorMotion::WordBackward => self.move_word_backward(),
            EditorMotion::WordEnd => self.move_word_end(),
            EditorMotion::LineStart => self.move_to_start(),
            EditorMotion::LineEnd => {
                self.column = self.line_length().saturating_sub(1);
            }
        }
    }

    pub fn start_insert(&mut self) {
        self.insert_snapshot = Some(self.snapshot());
        self.mode = EditorMode::Insert;
        self.clear_pending();
    }

    pub fn finish_insert(&mut self) {
        self.mode = EditorMode::Normal;
        let Some(snapshot) = self.insert_snapshot.take() else {
            return;
        };
        if snapshot.content != self.content() {
            self.column = self.column.saturating_sub(1);
            self.undo.push(snapshot);
        }
    }

    pub fn open_line_below(&mut self) {
        let snapshot = self.snapshot();
        self.row += 1;
        self.column = 0;
        self.lines.insert(self.row, String::new());
        self.insert_snapshot = Some(snapshot);
        self.mode = EditorMode::Insert;
        self.clear_pending();
    }

    pub fn open_line_above(&mut self) {
        let snapshot = self.snapshot();
        self.column = 0;
        self.lines.insert(self.row, String::new());
        self.insert_snapshot = Some(snapshot);
        self.mode = EditorMode::Insert;
        self.clear_pending();
    }

    pub fn insert(&mut self, character: char) {
        let byte = char_to_byte(&self.lines[self.row], self.column);
        self.lines[self.row].insert(byte, character);
        self.column += 1;
    }

    pub fn newline(&mut self) {
        let byte = char_to_byte(&self.lines[self.row], self.column);
        let next_line = self.lines[self.row].split_off(byte);
        self.row += 1;
        self.column = 0;
        self.lines.insert(self.row, next_line);
    }

    pub fn backspace(&mut self) {
        if self.column > 0 {
            let end = char_to_byte(&self.lines[self.row], self.column);
            let start = char_to_byte(&self.lines[self.row], self.column - 1);
            self.lines[self.row].replace_range(start..end, "");
            self.column -= 1;
            return;
        }
        if self.row == 0 {
            return;
        }

        let current = self.lines.remove(self.row);
        self.row -= 1;
        self.column = self.line_length();
        self.lines[self.row].push_str(&current);
    }

    pub fn delete(&mut self) {
        if self.column < self.line_length() {
            let start = char_to_byte(&self.lines[self.row], self.column);
            let end = char_to_byte(&self.lines[self.row], self.column + 1);
            self.lines[self.row].replace_range(start..end, "");
            return;
        }
        if self.row + 1 >= self.lines.len() {
            return;
        }

        let next = self.lines.remove(self.row + 1);
        self.lines[self.row].push_str(&next);
    }

    pub fn delete_character(&mut self) {
        if self.column >= self.line_length() {
            return;
        }

        let start = self.cursor_offset();
        self.register =
            EditorRegister::Characters(self.content().chars().skip(start).take(1).collect());
        self.delete_range(start, start + 1, false);
    }

    pub fn delete_previous_character(&mut self) {
        if self.column == 0 {
            return;
        }

        let end = self.cursor_offset();
        self.register =
            EditorRegister::Characters(self.content().chars().skip(end - 1).take(1).collect());
        self.delete_range(end - 1, end, false);
    }

    pub fn apply_operator(&mut self, operator: EditorOperator, motion: EditorMotion) {
        let characters = self.content().chars().collect::<Vec<_>>();
        let cursor = self.cursor_offset();
        let (start, end) =
            operator_range(&characters, cursor, operator, motion, self.line_bounds());
        self.clear_pending();

        if start == end {
            if operator == EditorOperator::Change {
                self.start_insert();
            }
            return;
        }

        let selected = characters[start..end].iter().collect::<String>();
        self.register = EditorRegister::Characters(selected);
        if operator == EditorOperator::Yank {
            return;
        }

        self.delete_range(start, end, operator == EditorOperator::Change);
    }

    pub fn apply_line_operator(&mut self, operator: EditorOperator) {
        self.clear_pending();
        let line = self.lines[self.row].clone();
        self.register = EditorRegister::Lines(vec![line]);
        if operator == EditorOperator::Yank {
            return;
        }

        let snapshot = self.snapshot();
        if operator == EditorOperator::Change {
            self.lines[self.row].clear();
            self.column = 0;
            self.insert_snapshot = Some(snapshot);
            self.mode = EditorMode::Insert;
            return;
        }

        if self.lines.len() == 1 {
            self.lines[0].clear();
        } else {
            self.lines.remove(self.row);
            self.row = self.row.min(self.lines.len() - 1);
        }
        self.column = 0;
        self.undo.push(snapshot);
    }

    pub fn paste(&mut self, after: bool) {
        let register = self.register.clone();
        match register {
            EditorRegister::Empty => {}
            EditorRegister::Characters(value) => self.paste_characters(&value, after),
            EditorRegister::Lines(lines) => self.paste_lines(lines, after),
        }
    }

    pub fn undo(&mut self) {
        let Some(snapshot) = self.undo.pop() else {
            return;
        };

        self.restore(snapshot);
        self.clear_pending();
    }

    pub fn clear_pending(&mut self) {
        self.pending_operator = None;
        self.pending_g = false;
    }

    fn paste_characters(&mut self, value: &str, after: bool) {
        if value.is_empty() {
            return;
        }

        let snapshot = self.snapshot();
        let mut characters = self.content().chars().collect::<Vec<_>>();
        let cursor = self.cursor_offset();
        let insert_at = if after && self.column < self.line_length() {
            cursor + 1
        } else {
            cursor
        };
        let inserted = value.chars().collect::<Vec<_>>();
        characters.splice(insert_at..insert_at, inserted.iter().copied());

        self.replace_content(characters.iter().collect(), insert_at + inserted.len() - 1);
        self.undo.push(snapshot);
    }

    fn paste_lines(&mut self, lines: Vec<String>, after: bool) {
        if lines.is_empty() {
            return;
        }

        let snapshot = self.snapshot();
        if self.lines.len() == 1 && self.lines[0].is_empty() {
            self.lines = lines;
            self.row = self.lines.len() - 1;
            self.column = 0;
            self.undo.push(snapshot);
            return;
        }

        let insert_at = self.row + usize::from(after);
        let inserted = lines.len();
        self.lines.splice(insert_at..insert_at, lines);
        self.row = insert_at + inserted - 1;
        self.column = 0;
        self.undo.push(snapshot);
    }

    fn delete_range(&mut self, start: usize, end: usize, change: bool) {
        let snapshot = self.snapshot();
        let mut characters = self.content().chars().collect::<Vec<_>>();
        characters.drain(start..end);
        self.replace_content(characters.iter().collect(), start);

        if change {
            self.insert_snapshot = Some(snapshot);
            self.mode = EditorMode::Insert;
        } else {
            self.undo.push(snapshot);
        }
    }

    fn cursor_offset(&self) -> usize {
        self.lines[..self.row]
            .iter()
            .map(|line| line.chars().count() + 1)
            .sum::<usize>()
            + self.column
    }

    fn set_cursor_offset(&mut self, mut offset: usize) {
        for (row, line) in self.lines.iter().enumerate() {
            let line_length = line.chars().count();
            if offset <= line_length {
                self.row = row;
                self.column = offset;
                return;
            }
            offset = offset.saturating_sub(line_length + 1);
        }

        self.row = self.lines.len() - 1;
        self.column = self.line_length();
    }

    fn line_bounds(&self) -> (usize, usize) {
        let cursor = self.cursor_offset();
        (
            cursor.saturating_sub(self.column),
            cursor + self.line_length().saturating_sub(self.column),
        )
    }

    fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            content: self.content(),
            row: self.row,
            column: self.column,
        }
    }

    fn restore(&mut self, snapshot: EditorSnapshot) {
        self.lines = snapshot.content.split('\n').map(str::to_string).collect();
        self.row = snapshot.row.min(self.lines.len() - 1);
        self.column = snapshot.column.min(self.line_length());
    }

    fn replace_content(&mut self, content: String, cursor: usize) {
        self.lines = content.split('\n').map(str::to_string).collect();
        self.set_cursor_offset(cursor);
    }

    fn line_length(&self) -> usize {
        self.lines[self.row].chars().count()
    }

    fn clamp_column(&mut self) {
        self.column = self.column.min(self.line_length());
    }
}

fn operator_range(
    characters: &[char],
    cursor: usize,
    operator: EditorOperator,
    motion: EditorMotion,
    line_bounds: (usize, usize),
) -> (usize, usize) {
    match motion {
        EditorMotion::Left => (cursor.saturating_sub(1), cursor),
        EditorMotion::Right => (cursor, (cursor + 1).min(characters.len())),
        EditorMotion::WordBackward => (previous_word_start(characters, cursor), cursor),
        EditorMotion::WordForward
            if operator == EditorOperator::Change
                && cursor < characters.len()
                && word_class(characters[cursor]) != WordClass::Whitespace =>
        {
            let end = next_word_end(characters, cursor);
            (cursor, (end + 1).min(characters.len()))
        }
        EditorMotion::WordForward => (cursor, next_word_start(characters, cursor)),
        EditorMotion::WordEnd => {
            let end = next_word_end(characters, cursor);
            (cursor, (end + 1).min(characters.len()))
        }
        EditorMotion::LineStart => (line_bounds.0, cursor),
        EditorMotion::LineEnd => (cursor, line_bounds.1),
    }
}

fn next_word_start(characters: &[char], mut offset: usize) -> usize {
    if offset >= characters.len() {
        return characters.len();
    }

    let class = word_class(characters[offset]);
    if class != WordClass::Whitespace {
        while offset < characters.len() && word_class(characters[offset]) == class {
            offset += 1;
        }
    }
    while offset < characters.len() && word_class(characters[offset]) == WordClass::Whitespace {
        offset += 1;
    }

    offset
}

fn previous_word_start(characters: &[char], offset: usize) -> usize {
    if offset == 0 || characters.is_empty() {
        return 0;
    }

    let mut offset = offset.min(characters.len()) - 1;
    while offset > 0 && word_class(characters[offset]) == WordClass::Whitespace {
        offset -= 1;
    }
    let class = word_class(characters[offset]);
    while offset > 0 && word_class(characters[offset - 1]) == class {
        offset -= 1;
    }

    offset
}

fn next_word_end(characters: &[char], offset: usize) -> usize {
    if offset >= characters.len() {
        return characters.len();
    }

    let mut offset = (offset + 1).min(characters.len() - 1);
    while offset < characters.len() - 1 && word_class(characters[offset]) == WordClass::Whitespace {
        offset += 1;
    }
    let class = word_class(characters[offset]);
    while offset < characters.len() - 1 && word_class(characters[offset + 1]) == class {
        offset += 1;
    }

    offset
}

fn word_class(character: char) -> WordClass {
    if character.is_whitespace() {
        return WordClass::Whitespace;
    }
    if character.is_alphanumeric() || character == '_' {
        return WordClass::Keyword;
    }

    WordClass::Punctuation
}

fn char_to_byte(value: &str, index: usize) -> usize {
    value
        .char_indices()
        .nth(index)
        .map(|(byte, _)| byte)
        .unwrap_or(value.len())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Normal,       // Regular proxy mode
    Paused,       // All requests paused
    Intercepting, // Inspecting a specific request
}

#[derive(Debug)]
pub enum ProxyDecision {
    Allow(Option<serde_json::Value>, Option<HashMap<String, String>>), // Allow with optional modified JSON and headers
    Block,                                                             // Block the request
    Complete(serde_json::Value), // Complete with custom response
}

#[allow(dead_code)]
pub struct PendingRequest {
    pub id: String,
    pub original_request: JsonRpcMessage,
    pub modified_request: Option<String>, // JSON string for editing
    pub modified_headers: Option<HashMap<String, String>>, // Modified headers
    pub decision_sender: oneshot::Sender<ProxyDecision>,
}

#[allow(dead_code)]
pub struct App {
    pub exchanges: Vec<JsonRpcExchange>,
    pub selected_exchange: usize,
    pub filter_text: String,
    pub history_scroll: Option<usize>,
    pub details_scroll: usize,
    pub request_details_scroll: usize,
    pub response_details_scroll: usize,
    pub request_details_cursor_line: usize,
    pub response_details_cursor_line: usize,
    pub details_tab: usize,
    pub request_details_tab: usize,
    pub response_details_tab: usize,
    pub intercept_details_scroll: usize, // New field for intercept details scrolling
    pub proxy_config: ProxyConfig,
    pub is_running: bool,
    pub message_receiver: Option<mpsc::UnboundedReceiver<JsonRpcMessage>>,
    pub input_mode: InputMode,
    pub input_buffer: String,
    pub app_mode: AppMode,                     // New field
    pub pending_requests: Vec<PendingRequest>, // New field
    pub selected_pending: usize,               // New field
    pub request_editor_buffer: String,         // New field
    pub focus: Focus,                          // New field for tracking which element is active
    pub request_tab: usize,                    // 0 = Headers, 1 = Body
    pub response_tab: usize,                   // 0 = Headers, 1 = Body
    pub line_selection: Option<LineSelection>,
    pub visual_selection_active: bool,
    pub annotations: Vec<LineAnnotation>,
    pub active_annotation_id: Option<String>,
    pub editor: Option<TextEditor>,
    pub notice: Option<String>,
    pub control_port: u16,
    pub overlay: Overlay,
    pub panel_fullscreen: bool,
    pub session: Option<SessionSummary>,
    pub sessions: Vec<SessionSummary>,
    pub selected_session: usize,
    revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRequest {
    pub url: String,
    pub body: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ProxyConfig {
    pub listen_port: u16,
    pub target_url: String,
    pub transport: TransportType,
    pub stdio: Option<StdioConfig>,
    pub transparent: bool,
}

#[derive(Debug, Clone)]
pub struct StdioConfig {
    pub command: Vec<OsString>,
    pub framing: Framing,
}

fn exchange_status(exchange: &JsonRpcExchange) -> &'static str {
    if exchange.is_notification() {
        return "Notification";
    }
    match &exchange.response {
        None => "Pending",
        Some(response) if response.error.is_some() => "Error",
        Some(_) => "Success",
    }
}

fn transport_name(transport: &TransportType) -> &'static str {
    transport.label()
}

fn display_id(id: Option<&serde_json::Value>) -> String {
    match id {
        Some(serde_json::Value::String(value)) => value.clone(),
        Some(value) => value.to_string(),
        None => "null".to_string(),
    }
}

pub fn request_matches_filter(
    method: Option<&str>,
    id: Option<&serde_json::Value>,
    filter: &str,
) -> bool {
    filter.is_empty() || method.unwrap_or("").contains(filter) || display_id(id).contains(filter)
}

fn exchange_duration(exchange: &JsonRpcExchange) -> String {
    let (Some(request), Some(response)) = (&exchange.request, &exchange.response) else {
        return "-".to_string();
    };
    let Ok(duration) = response.timestamp.duration_since(request.timestamp) else {
        return "-".to_string();
    };

    if duration.as_millis() < 1000 {
        return format!("{}ms", duration.as_millis());
    }

    format!("{:.2}s", duration.as_secs_f64())
}

fn markdown_cell(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('\n', "<br>")
}

fn exchange_heading(title: &str, exchange: &JsonRpcExchange) -> String {
    format!(
        "# {title}\n\n- Transport: {}\n- Method: {}\n- ID: {}\n",
        transport_name(&exchange.transport),
        exchange.method.as_deref().unwrap_or("unknown"),
        display_id(exchange.id.as_ref()),
    )
}

fn headers_markdown(headers: Option<&HashMap<String, String>>) -> String {
    let Some(headers) = headers else {
        return "\n## Headers\n\n_No headers captured._".to_string();
    };
    if headers.is_empty() {
        return "\n## Headers\n\n_No headers._".to_string();
    }

    let mut headers = headers.iter().collect::<Vec<_>>();
    headers.sort_by_key(|(name, _)| *name);

    let rows = headers
        .into_iter()
        .map(|(name, value)| format!("| {} | {} |", markdown_cell(name), markdown_cell(value)))
        .collect::<Vec<_>>()
        .join("\n");

    format!("\n## Headers\n\n| Header | Value |\n| --- | --- |\n{rows}")
}

fn json_markdown(value: &serde_json::Value) -> String {
    let json = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    format!("\n## Body\n\n```json\n{json}\n```")
}

fn request_json(request: &JsonRpcMessage) -> serde_json::Value {
    let mut json = serde_json::Map::new();
    json.insert("jsonrpc".to_string(), serde_json::json!("2.0"));
    if let Some(id) = &request.id {
        json.insert("id".to_string(), id.clone());
    }
    if let Some(method) = &request.method {
        json.insert("method".to_string(), serde_json::json!(method));
    }
    if let Some(params) = &request.params {
        json.insert("params".to_string(), params.clone());
    }

    serde_json::Value::Object(json)
}

fn response_json(response: &JsonRpcMessage) -> serde_json::Value {
    let mut json = serde_json::Map::new();
    json.insert("jsonrpc".to_string(), serde_json::json!("2.0"));
    if let Some(id) = &response.id {
        json.insert("id".to_string(), id.clone());
    }
    if let Some(result) = &response.result {
        json.insert("result".to_string(), result.clone());
    }
    if let Some(error) = &response.error {
        json.insert("error".to_string(), error.clone());
    }

    serde_json::Value::Object(json)
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl App {
    pub fn new() -> Self {
        Self {
            exchanges: Vec::new(),
            selected_exchange: 0,
            filter_text: String::new(),
            history_scroll: None,
            details_scroll: 0,
            request_details_scroll: 0,
            response_details_scroll: 0,
            request_details_cursor_line: 1,
            response_details_cursor_line: 1,
            details_tab: 0,
            request_details_tab: 0,
            response_details_tab: 0,
            intercept_details_scroll: 0,
            proxy_config: ProxyConfig {
                listen_port: 8080,
                target_url: "".to_string(),
                transport: TransportType::Http,
                stdio: None,
                transparent: false,
            },
            is_running: true,
            message_receiver: None,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            app_mode: AppMode::Normal,
            pending_requests: Vec::new(),
            selected_pending: 0,
            request_editor_buffer: String::new(),
            focus: Focus::MessageList,
            request_tab: 1,  // Body selected by default
            response_tab: 1, // Body selected by default
            line_selection: None,
            visual_selection_active: false,
            annotations: Vec::new(),
            active_annotation_id: None,
            editor: None,
            notice: None,
            control_port: 8081,
            overlay: Overlay::None,
            panel_fullscreen: false,
            session: None,
            sessions: Vec::new(),
            selected_session: 0,
            revision: 0,
        }
    }

    pub fn new_with_receiver(receiver: mpsc::UnboundedReceiver<JsonRpcMessage>) -> Self {
        let mut app = Self::new();
        app.message_receiver = Some(receiver);
        app
    }

    pub fn check_for_new_messages(&mut self) -> bool {
        let new_messages = self.take_new_messages();
        let received_messages = !new_messages.is_empty();
        for message in new_messages {
            self.add_message(message);
        }

        received_messages
    }

    pub fn take_new_messages(&mut self) -> Vec<JsonRpcMessage> {
        let Some(receiver) = &mut self.message_receiver else {
            return Vec::new();
        };

        let mut messages = Vec::new();
        while let Ok(message) = receiver.try_recv() {
            messages.push(message);
        }

        messages
    }

    pub fn activate_session(
        &mut self,
        session: SessionSummary,
        exchanges: Vec<JsonRpcExchange>,
        annotations: Vec<LineAnnotation>,
    ) {
        self.exchanges = exchanges;
        self.selected_exchange = self.exchanges.len().saturating_sub(1);
        self.history_scroll = None;
        self.filter_text.clear();
        self.session = Some(session);
        self.overlay = Overlay::None;
        self.line_selection = None;
        self.visual_selection_active = false;
        self.annotations = annotations;
        self.active_annotation_id = None;
        self.reset_details_scroll();
        self.request_details_scroll = 0;
        self.response_details_scroll = 0;
        self.reset_detail_cursors();
        self.mark_changed();
    }

    pub fn show_prefix(&mut self) {
        self.overlay = Overlay::Prefix;
        self.mark_changed();
    }

    pub fn show_help(&mut self) {
        self.overlay = Overlay::Help;
        self.mark_changed();
    }

    pub fn show_sessions(&mut self, sessions: Vec<SessionSummary>) {
        self.selected_session = self
            .session
            .as_ref()
            .and_then(|active| sessions.iter().position(|session| session.id == active.id))
            .unwrap_or(0);
        self.sessions = sessions;
        self.overlay = Overlay::Sessions;
        self.mark_changed();
    }

    pub fn close_overlay(&mut self) {
        if self.overlay == Overlay::None {
            return;
        }
        self.overlay = Overlay::None;
        self.mark_changed();
    }

    pub fn select_next_session(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        self.selected_session = (self.selected_session + 1).min(self.sessions.len() - 1);
        self.mark_changed();
    }

    pub fn select_previous_session(&mut self) {
        let selected = self.selected_session.saturating_sub(1);
        if selected == self.selected_session {
            return;
        }
        self.selected_session = selected;
        self.mark_changed();
    }

    pub fn add_message(&mut self, mut message: JsonRpcMessage) {
        // Sanitize message content to prevent UI corruption
        if let Some(ref mut error) = message.error {
            if let Some(data) = error.get_mut("data") {
                if let Some(data_str) = data.as_str() {
                    let sanitized = data_str
                        .chars()
                        .filter(|c| c.is_ascii() && (!c.is_control() || *c == '\n' || *c == '\t'))
                        .take(500)
                        .collect::<String>();
                    *data = serde_json::Value::String(sanitized);
                }
            }
        }

        match message.direction {
            MessageDirection::Request => {
                // Create a new exchange for the request
                let exchange = JsonRpcExchange {
                    id: message.id.clone(),
                    method: message.method.clone(),
                    request: Some(message.clone()),
                    response: None,
                    timestamp: message.timestamp,
                    transport: message.transport,
                };
                self.exchanges.push(exchange);
            }
            MessageDirection::Response => {
                // Find matching request by ID and add response
                if let Some(exchange) =
                    self.exchanges.iter_mut().rev().find(|e| {
                        !e.is_notification() && e.id == message.id && e.response.is_none()
                    })
                {
                    exchange.response = Some(message);
                } else {
                    // No matching request found, create exchange with just response
                    let exchange = JsonRpcExchange {
                        id: message.id.clone(),
                        method: None,
                        request: None,
                        response: Some(message.clone()),
                        timestamp: message.timestamp,
                        transport: message.transport,
                    };
                    self.exchanges.push(exchange);
                }
            }
        }
        if let Some(session) = &mut self.session {
            session.exchange_count = self.exchanges.len();
        }
        self.mark_changed();
    }

    pub fn get_selected_exchange(&self) -> Option<&JsonRpcExchange> {
        self.exchanges.get(self.selected_exchange)
    }

    pub fn filtered_exchange_indices(&self) -> Vec<usize> {
        self.exchanges
            .iter()
            .enumerate()
            .filter(|(_, exchange)| {
                request_matches_filter(
                    exchange.method.as_deref(),
                    exchange.id.as_ref(),
                    &self.filter_text,
                )
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub fn history_scroll_offset(&self, visible_rows: usize) -> usize {
        let indices = self.filtered_exchange_indices();
        let selected = indices
            .iter()
            .position(|index| *index == self.selected_exchange)
            .unwrap_or(0);
        let visible_rows = visible_rows.max(1);
        let followed = selected.saturating_sub(visible_rows.saturating_sub(1));
        let max_scroll = indices.len().saturating_sub(visible_rows);
        self.history_scroll.unwrap_or(followed).min(max_scroll)
    }

    pub fn scroll_history(&mut self, lines: i64, visible_rows: usize) {
        let previous = self.history_scroll_offset(visible_rows);
        let distance = usize::try_from(lines.unsigned_abs()).unwrap_or(usize::MAX);
        let max_scroll = self
            .filtered_exchange_indices()
            .len()
            .saturating_sub(visible_rows.max(1));
        let current = if lines >= 0 {
            previous.saturating_add(distance).min(max_scroll)
        } else {
            previous.saturating_sub(distance)
        };
        self.history_scroll = Some(current);
        if current != previous {
            self.mark_changed();
        }
    }

    pub fn select_exchange(&mut self, index: usize) {
        if index >= self.exchanges.len() {
            return;
        }

        self.selected_exchange = index;
        self.history_scroll = None;
        self.request_details_scroll = 0;
        self.response_details_scroll = 0;
        self.reset_detail_cursors();
        self.line_selection = None;
        self.visual_selection_active = false;
        self.active_annotation_id = None;
        self.mark_changed();
    }

    pub fn select_lines(
        &mut self,
        panel: Focus,
        start_line: usize,
        end_line: usize,
        text: Vec<String>,
    ) {
        self.visual_selection_active = false;
        self.active_annotation_id = None;
        self.select_lines_from_anchor(panel, start_line, start_line, end_line, text);
    }

    pub fn start_visual_selection(&mut self) {
        if self.visual_selection_active {
            return;
        }
        self.visual_selection_active = true;
        self.mark_changed();
    }

    pub fn finish_visual_selection(&mut self) {
        if !self.visual_selection_active {
            return;
        }
        self.visual_selection_active = false;
        self.mark_changed();
    }

    pub fn select_lines_from_anchor(
        &mut self,
        panel: Focus,
        anchor_line: usize,
        start_line: usize,
        end_line: usize,
        text: Vec<String>,
    ) {
        if matches!(panel, Focus::MessageList | Focus::StatusHeader) {
            return;
        }

        self.focus = panel;
        self.active_annotation_id = None;
        let cursor_line = if anchor_line == start_line {
            end_line
        } else {
            start_line
        };
        match panel {
            Focus::RequestSection => self.request_details_cursor_line = cursor_line,
            Focus::ResponseSection => self.response_details_cursor_line = cursor_line,
            Focus::MessageList | Focus::StatusHeader => {}
        }
        self.line_selection = Some(LineSelection {
            panel,
            anchor_line,
            start_line,
            end_line,
            text,
        });
        self.mark_changed();
    }

    pub fn line_selection_range(
        &self,
        panel: Focus,
        line: usize,
        extend: bool,
    ) -> (usize, usize, usize) {
        let anchor = if extend {
            self.line_selection
                .as_ref()
                .filter(|selection| selection.panel == panel)
                .map(|selection| selection.anchor_line)
                .unwrap_or(line)
        } else {
            line
        };

        (anchor, anchor.min(line), anchor.max(line))
    }

    pub fn reveal_lines(
        &mut self,
        panel: Focus,
        start_line: usize,
        end_line: usize,
        text: Vec<String>,
    ) {
        self.select_lines(panel, start_line, end_line, text);
        match panel {
            Focus::RequestSection => self.request_details_scroll = start_line.saturating_sub(1),
            Focus::ResponseSection => self.response_details_scroll = start_line.saturating_sub(1),
            Focus::MessageList | Focus::StatusHeader => {}
        }
    }

    pub fn clear_line_selection(&mut self) {
        if self.line_selection.is_none() && !self.visual_selection_active {
            return;
        }
        self.line_selection = None;
        self.visual_selection_active = false;
        self.mark_changed();
    }

    pub fn add_annotation(&mut self, annotation: LineAnnotation) {
        self.annotations.push(annotation);
        self.mark_changed();
    }

    pub fn focus_annotation(&mut self, id: &str) {
        let Some(annotation) = self
            .annotations
            .iter()
            .find(|annotation| annotation.id == id)
            .cloned()
        else {
            return;
        };
        self.reveal_lines(
            annotation.panel,
            annotation.start_line,
            annotation.end_line,
            annotation.text,
        );
        self.active_annotation_id = Some(annotation.id);
        self.mark_changed();
    }

    pub fn remove_annotation(&mut self, id: &str) -> bool {
        let before = self.annotations.len();
        self.annotations.retain(|annotation| annotation.id != id);
        if self.annotations.len() == before {
            return false;
        }
        if self.active_annotation_id.as_deref() == Some(id) {
            self.active_annotation_id = None;
        }
        self.mark_changed();
        true
    }

    pub fn detail_tab(&self, panel: Focus) -> Option<DetailTab> {
        match panel {
            Focus::RequestSection => Some(if self.request_tab == 0 {
                DetailTab::Headers
            } else {
                DetailTab::Body
            }),
            Focus::ResponseSection => Some(if self.response_tab == 0 {
                DetailTab::Headers
            } else {
                DetailTab::Body
            }),
            Focus::MessageList | Focus::StatusHeader => None,
        }
    }

    pub fn visible_annotations(
        &self,
        panel: Focus,
    ) -> impl DoubleEndedIterator<Item = &LineAnnotation> {
        let tab = self.detail_tab(panel);
        self.annotations.iter().filter(move |annotation| {
            annotation.exchange_index == self.selected_exchange
                && annotation.panel == panel
                && Some(annotation.tab) == tab
        })
    }

    pub fn selection_overlaps_annotation(&self) -> bool {
        let Some(selection) = &self.line_selection else {
            return false;
        };
        self.visible_annotations(selection.panel).any(|annotation| {
            selection.start_line <= annotation.end_line
                && annotation.start_line <= selection.end_line
        })
    }

    pub fn annotation_at_cursor(&self) -> Option<&LineAnnotation> {
        let cursor = self.detail_cursor_line(self.focus)?;
        self.visible_annotations(self.focus)
            .rev()
            .find(|annotation| (annotation.start_line..=annotation.end_line).contains(&cursor))
    }

    pub fn annotation_to_delete(&self) -> Option<&LineAnnotation> {
        self.active_annotation_id
            .as_deref()
            .and_then(|id| {
                self.visible_annotations(self.focus)
                    .find(|annotation| annotation.id == id)
            })
            .or_else(|| self.annotation_at_cursor())
    }

    pub fn detail_cursor_line(&self, panel: Focus) -> Option<usize> {
        match panel {
            Focus::RequestSection => Some(self.request_details_cursor_line),
            Focus::ResponseSection => Some(self.response_details_cursor_line),
            Focus::MessageList | Focus::StatusHeader => None,
        }
    }

    pub fn move_detail_cursor(
        &mut self,
        panel: Focus,
        lines: i64,
        total_lines: usize,
        visible_lines: usize,
    ) {
        let total_lines = total_lines.max(1);
        let visible_lines = visible_lines.max(1);
        let distance = usize::try_from(lines.unsigned_abs()).unwrap_or(usize::MAX);
        let previous_focus = self.focus;
        self.focus = panel;

        let (cursor, scroll) = match panel {
            Focus::RequestSection => (
                &mut self.request_details_cursor_line,
                &mut self.request_details_scroll,
            ),
            Focus::ResponseSection => (
                &mut self.response_details_cursor_line,
                &mut self.response_details_scroll,
            ),
            Focus::MessageList | Focus::StatusHeader => return,
        };
        let previous_cursor = *cursor;
        let previous_scroll = *scroll;
        *cursor = (*cursor).clamp(1, total_lines);
        *cursor = if lines >= 0 {
            cursor.saturating_add(distance).min(total_lines)
        } else {
            cursor.saturating_sub(distance).max(1)
        };

        let cursor_index = cursor.saturating_sub(1);
        let max_scroll = total_lines.saturating_sub(visible_lines);
        *scroll = (*scroll).min(max_scroll);
        if cursor_index < *scroll {
            *scroll = cursor_index;
        } else if cursor_index >= scroll.saturating_add(visible_lines) {
            *scroll = cursor_index.saturating_add(1).saturating_sub(visible_lines);
        }

        if previous_focus != self.focus || previous_cursor != *cursor || previous_scroll != *scroll
        {
            self.mark_changed();
        }
    }

    fn reset_detail_cursors(&mut self) {
        self.request_details_cursor_line = 1;
        self.response_details_cursor_line = 1;
    }

    pub fn set_focus(&mut self, focus: Focus) {
        if self.focus == focus {
            return;
        }
        self.focus = focus;
        self.mark_changed();
    }

    pub fn set_panel_fullscreen(&mut self, fullscreen: bool) {
        if self.panel_fullscreen == fullscreen {
            return;
        }
        self.panel_fullscreen = fullscreen;
        self.mark_changed();
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn mark_changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn append_exchanges(&mut self, exchanges: Vec<JsonRpcExchange>) {
        if exchanges.is_empty() {
            return;
        }

        self.exchanges.extend(exchanges);
        if let Some(session) = &mut self.session {
            session.exchange_count = self.exchanges.len();
        }
        self.select_exchange(self.exchanges.len() - 1);
    }

    pub fn scroll_panel_lines(&mut self, panel: Focus, lines: i64, total_lines: usize) {
        let previous_focus = self.focus;
        self.focus = panel;
        let scroll_changed = {
            let scroll = match panel {
                Focus::RequestSection => &mut self.request_details_scroll,
                Focus::ResponseSection => &mut self.response_details_scroll,
                Focus::MessageList | Focus::StatusHeader => return,
            };
            let previous_scroll = *scroll;
            let distance = usize::try_from(lines.unsigned_abs()).unwrap_or(usize::MAX);
            let max_scroll = total_lines.saturating_sub(1);
            *scroll = (*scroll).min(max_scroll);
            *scroll = if lines >= 0 {
                scroll.saturating_add(distance).min(max_scroll)
            } else {
                scroll.saturating_sub(distance)
            };
            previous_scroll != *scroll
        };
        if previous_focus != self.focus || scroll_changed {
            self.mark_changed();
        }
    }

    pub fn focused_markdown(&self) -> Option<String> {
        if let Some(selection) = self
            .line_selection
            .as_ref()
            .filter(|selection| self.visual_selection_active || selection.panel == self.focus)
        {
            return Some(format!("```text\n{}\n```", selection.text.join("\n")));
        }

        match self.focus {
            Focus::MessageList => Some(self.requests_markdown()),
            Focus::RequestSection => self.request_markdown(),
            Focus::ResponseSection => self.response_markdown(),
            Focus::StatusHeader => Some(self.status_markdown()),
        }
    }

    fn requests_markdown(&self) -> String {
        let mut lines = vec![
            "| Status | Transport | Method | ID | Duration |".to_string(),
            "| --- | --- | --- | --- | --- |".to_string(),
        ];

        for index in self.filtered_exchange_indices() {
            let exchange = &self.exchanges[index];
            lines.push(format!(
                "| {} | {} | {} | {} | {} |",
                exchange_status(exchange),
                transport_name(&exchange.transport),
                markdown_cell(exchange.method.as_deref().unwrap_or("unknown")),
                markdown_cell(&display_id(exchange.id.as_ref())),
                exchange_duration(exchange),
            ));
        }

        lines.join("\n")
    }

    fn request_markdown(&self) -> Option<String> {
        let exchange = self.get_selected_exchange()?;
        let request = exchange.request.as_ref()?;
        let mut markdown = exchange_heading("Request", exchange);

        if self.request_tab == 0 {
            markdown.push_str(&headers_markdown(request.headers.as_ref()));
        } else {
            markdown.push_str(&json_markdown(&request_json(request)));
        }

        Some(markdown)
    }

    fn response_markdown(&self) -> Option<String> {
        let exchange = self.get_selected_exchange()?;
        let response = exchange.response.as_ref()?;
        let mut markdown = "# Response\n".to_string();

        if self.response_tab == 0 {
            markdown.push_str(&headers_markdown(response.headers.as_ref()));
        } else {
            markdown.push_str(&json_markdown(&response_json(response)));
        }

        Some(markdown)
    }

    fn status_markdown(&self) -> String {
        let state = if self.is_running {
            "Running"
        } else {
            "Stopped"
        };
        let mode = match self.app_mode {
            AppMode::Normal => "Normal".to_string(),
            AppMode::Paused => "Paused".to_string(),
            AppMode::Intercepting => format!("Intercepting ({})", self.pending_requests.len()),
        };
        let data_plane = if self.proxy_config.transparent {
            "Stdio".to_string()
        } else {
            format!("HTTP port {}", self.proxy_config.listen_port)
        };
        format!(
            "# Status\n\n- State: {state}\n- Data plane: {data_plane}\n- Control port: {}\n- Mode: {mode}",
            self.control_port
        )
    }

    pub fn select_next(&mut self) {
        if !self.exchanges.is_empty() {
            self.selected_exchange = (self.selected_exchange + 1) % self.exchanges.len();
            self.history_scroll = None;
            self.reset_details_scroll();
            self.request_details_scroll = 0;
            self.response_details_scroll = 0;
            self.reset_detail_cursors();
            self.details_tab = 0;
            self.request_details_tab = 0;
            self.response_details_tab = 0;
            self.line_selection = None;
            self.visual_selection_active = false;
            self.active_annotation_id = None;
            self.mark_changed();
        }
    }

    pub fn select_previous(&mut self) {
        if !self.exchanges.is_empty() {
            self.selected_exchange = if self.selected_exchange == 0 {
                self.exchanges.len() - 1
            } else {
                self.selected_exchange - 1
            };
            self.history_scroll = None;
            self.reset_details_scroll();
            self.request_details_scroll = 0;
            self.response_details_scroll = 0;
            self.reset_detail_cursors();
            self.details_tab = 0;
            self.request_details_tab = 0;
            self.response_details_tab = 0;
            self.line_selection = None;
            self.visual_selection_active = false;
            self.active_annotation_id = None;
            self.mark_changed();
        }
    }

    pub fn toggle_proxy(&mut self) {
        self.is_running = !self.is_running;
        self.mark_changed();
    }

    pub fn scroll_details_up(&mut self) {
        if self.details_scroll > 0 {
            self.details_scroll -= 1;
        }
    }

    pub fn scroll_details_down(&mut self, max_lines: usize, visible_lines: usize) {
        if max_lines > visible_lines && self.details_scroll < max_lines - visible_lines {
            self.details_scroll += 1;
        }
    }

    pub fn reset_details_scroll(&mut self) {
        self.details_scroll = 0;
    }

    // Intercept details scrolling methods
    pub fn scroll_intercept_details_up(&mut self) {
        if self.intercept_details_scroll > 0 {
            self.intercept_details_scroll -= 1;
        }
    }

    pub fn scroll_intercept_details_down(&mut self, max_lines: usize, visible_lines: usize) {
        if max_lines > visible_lines && self.intercept_details_scroll < max_lines - visible_lines {
            self.intercept_details_scroll += 1;
        }
    }

    pub fn reset_intercept_details_scroll(&mut self) {
        self.intercept_details_scroll = 0;
    }

    pub fn page_down_intercept_details(&mut self) {
        let page_size = 10; // Half page
        self.intercept_details_scroll += page_size;
    }

    pub fn page_up_intercept_details(&mut self) {
        let page_size = 10; // Half page
        self.intercept_details_scroll = self.intercept_details_scroll.saturating_sub(page_size);
    }

    pub fn goto_top_intercept_details(&mut self) {
        self.intercept_details_scroll = 0;
    }

    pub fn goto_bottom_intercept_details(&mut self, max_lines: usize, visible_lines: usize) {
        if max_lines > visible_lines {
            self.intercept_details_scroll = max_lines - visible_lines;
        }
    }

    // Enhanced details scrolling with vim-style page jumps
    pub fn page_down_details(&mut self, visible_lines: usize) {
        let page_size = visible_lines / 2; // Half page
        self.details_scroll += page_size;
    }

    pub fn page_up_details(&mut self) {
        let page_size = 10; // Half page
        self.details_scroll = self.details_scroll.saturating_sub(page_size);
    }

    pub fn goto_top_details(&mut self) {
        self.details_scroll = 0;
    }

    pub fn goto_bottom_details(&mut self, max_lines: usize, visible_lines: usize) {
        if max_lines > visible_lines {
            self.details_scroll = max_lines - visible_lines;
        }
    }

    pub fn switch_focus(&mut self) {
        self.focus = match self.focus {
            Focus::MessageList => Focus::RequestSection,
            Focus::RequestSection => Focus::ResponseSection,
            Focus::ResponseSection => Focus::StatusHeader,
            Focus::StatusHeader => Focus::MessageList,
        };
        self.mark_changed();
    }

    pub fn switch_focus_reverse(&mut self) {
        self.focus = match self.focus {
            Focus::MessageList => Focus::StatusHeader,
            Focus::RequestSection => Focus::MessageList,
            Focus::ResponseSection => Focus::RequestSection,
            Focus::StatusHeader => Focus::ResponseSection,
        };
        self.mark_changed();
    }

    pub fn is_message_list_focused(&self) -> bool {
        matches!(self.focus, Focus::MessageList)
    }

    pub fn is_request_section_focused(&self) -> bool {
        matches!(self.focus, Focus::RequestSection)
    }

    pub fn is_response_section_focused(&self) -> bool {
        matches!(self.focus, Focus::ResponseSection)
    }

    pub fn is_status_focused(&self) -> bool {
        matches!(self.focus, Focus::StatusHeader)
    }

    pub fn next_request_tab(&mut self) {
        self.request_tab = 1 - self.request_tab; // Toggle between 0 and 1
        self.request_details_scroll = 0;
        self.request_details_cursor_line = 1;
        self.line_selection = None;
        self.visual_selection_active = false;
        self.active_annotation_id = None;
        self.mark_changed();
    }

    pub fn previous_request_tab(&mut self) {
        self.request_tab = 1 - self.request_tab; // Toggle between 0 and 1
        self.request_details_scroll = 0;
        self.request_details_cursor_line = 1;
        self.line_selection = None;
        self.visual_selection_active = false;
        self.active_annotation_id = None;
        self.mark_changed();
    }

    pub fn next_response_tab(&mut self) {
        self.response_tab = 1 - self.response_tab; // Toggle between 0 and 1
        self.response_details_scroll = 0;
        self.response_details_cursor_line = 1;
        self.line_selection = None;
        self.visual_selection_active = false;
        self.active_annotation_id = None;
        self.mark_changed();
    }

    pub fn previous_response_tab(&mut self) {
        self.response_tab = 1 - self.response_tab; // Toggle between 0 and 1
        self.response_details_scroll = 0;
        self.response_details_cursor_line = 1;
        self.line_selection = None;
        self.visual_selection_active = false;
        self.active_annotation_id = None;
        self.mark_changed();
    }

    // Filtering requests methods
    pub fn start_filtering_requests(&mut self) {
        self.input_mode = InputMode::FilteringRequests;
        self.input_buffer.clear();
    }

    pub fn cancel_filtering(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    pub fn apply_filter(&mut self) {
        self.filter_text = self.input_buffer.clone();
        self.history_scroll = None;
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
        self.mark_changed();
    }

    // Get content lines for proper scrolling calculations
    // Target editing methods
    pub fn start_editing_target(&mut self) {
        if self.proxy_config.stdio.is_some() {
            self.notice = Some("The stdio command is configured at startup".to_string());
            self.mark_changed();
            return;
        }
        self.input_mode = InputMode::EditingTarget;
        self.input_buffer = self.proxy_config.target_url.clone();
    }

    pub fn cancel_editing(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    pub fn start_naming_session(&mut self) {
        self.input_mode = InputMode::NamingSession;
        self.input_buffer.clear();
    }

    pub fn start_annotating_selection(&mut self) {
        if !self.visual_selection_active || self.line_selection.is_none() {
            return;
        }
        self.input_mode = InputMode::AnnotatingSelection;
        self.input_buffer.clear();
    }

    pub fn start_renaming_session(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        self.input_mode = InputMode::RenamingSession;
        self.input_buffer = session.name.clone();
    }

    pub fn rename_session(&mut self, id: &str, name: String) {
        if let Some(session) = &mut self.session {
            if session.id == id {
                session.name = name.clone();
            }
        }
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            session.name = name;
        }
        self.mark_changed();
    }

    pub fn confirm_target_edit(&mut self) {
        if !self.input_buffer.trim().is_empty() {
            self.proxy_config.target_url = self.input_buffer.trim().to_string();
        }
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
        self.mark_changed();
    }

    pub fn handle_input_char(&mut self, c: char) {
        if self.input_mode != InputMode::Normal {
            self.input_buffer.push(c);
        }
    }

    pub fn handle_backspace(&mut self) {
        if self.input_mode != InputMode::Normal {
            self.input_buffer.pop();
        }
    }

    pub fn get_details_content_lines(&self) -> usize {
        if let Some(exchange) = self.get_selected_exchange() {
            let mut line_count = 1; // Transport line

            if exchange.method.is_some() {
                line_count += 1;
            }
            if exchange.id.is_some() {
                line_count += 1;
            }

            // Request section
            line_count += 1; // Blank line before section
            line_count += 1; // Section header
            line_count += 1; // Tabs line

            if let Some(request) = &exchange.request {
                match self.request_tab {
                    0 => match &request.headers {
                        Some(headers) if !headers.is_empty() => {
                            line_count += headers.len();
                        }
                        Some(_) | None => {
                            line_count += 1;
                        }
                    },
                    _ => {
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

                        if let Ok(json_str) =
                            serde_json::to_string_pretty(&serde_json::Value::Object(request_json))
                        {
                            line_count += json_str.lines().count();
                        }
                    }
                }
            } else {
                line_count += 1;
            }

            // Response section
            line_count += 1; // Blank line before section
            line_count += 1; // Section header
            line_count += 1; // Tabs line

            if let Some(response) = &exchange.response {
                match self.response_tab {
                    0 => match &response.headers {
                        Some(headers) if !headers.is_empty() => {
                            line_count += headers.len();
                        }
                        Some(_) | None => {
                            line_count += 1;
                        }
                    },
                    _ => {
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

                        if let Ok(json_str) =
                            serde_json::to_string_pretty(&serde_json::Value::Object(response_json))
                        {
                            line_count += json_str.lines().count();
                        }
                    }
                }
            } else {
                line_count += 1;
            }

            line_count
        } else {
            1
        }
    }

    pub fn get_request_details_content_lines(&self) -> usize {
        if let Some(exchange) = self.get_selected_exchange() {
            let mut line_count = 0;

            // Basic exchange info
            line_count += 1; // Transport line

            if exchange.method.is_some() {
                line_count += 1;
            }
            if exchange.id.is_some() {
                line_count += 1;
            }

            // Request section
            line_count += 1; // Blank line before section
            line_count += 1; // Section header
            line_count += 1; // Tabs line

            if let Some(request) = &exchange.request {
                match self.request_tab {
                    0 => match &request.headers {
                        Some(headers) if !headers.is_empty() => {
                            line_count += headers.len();
                        }
                        Some(_) | None => {
                            line_count += 1;
                        }
                    },
                    _ => {
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

                        if let Ok(json_str) =
                            serde_json::to_string_pretty(&serde_json::Value::Object(request_json))
                        {
                            line_count += json_str.lines().count();
                        }
                    }
                }
            } else {
                line_count += 1;
            }

            line_count
        } else {
            1
        }
    }

    pub fn get_response_details_content_lines(&self) -> usize {
        if let Some(exchange) = self.get_selected_exchange() {
            let mut line_count = 0;

            // Response section
            line_count += 1; // Section header
            line_count += 1; // Tabs line

            if let Some(response) = &exchange.response {
                match self.response_tab {
                    0 => match &response.headers {
                        Some(headers) if !headers.is_empty() => {
                            line_count += headers.len();
                        }
                        Some(_) | None => {
                            line_count += 1;
                        }
                    },
                    _ => {
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

                        if let Ok(json_str) =
                            serde_json::to_string_pretty(&serde_json::Value::Object(response_json))
                        {
                            line_count += json_str.lines().count();
                        }
                    }
                }
            } else {
                line_count += 1;
            }

            line_count
        } else {
            1
        }
    }

    pub fn get_intercept_details_content_lines(&self) -> usize {
        let Some(pending) = self.get_selected_pending() else {
            return 1;
        };

        let headers = pending
            .modified_headers
            .as_ref()
            .or(pending.original_request.headers.as_ref());
        let header_lines = headers.map(HashMap::len).unwrap_or(1)
            + usize::from(pending.modified_headers.is_some());
        let request = pending
            .modified_request
            .clone()
            .or_else(|| self.get_pending_request_json())
            .unwrap_or_default();

        13 + header_lines + request.lines().count().max(1)
    }

    // Pause/Intercept functionality
    pub fn toggle_pause_mode(&mut self) {
        self.line_selection = None;
        self.visual_selection_active = false;
        self.active_annotation_id = None;
        self.app_mode = match self.app_mode {
            AppMode::Normal => AppMode::Paused,
            AppMode::Paused if self.pending_requests.is_empty() => AppMode::Normal,
            AppMode::Paused => AppMode::Intercepting,
            AppMode::Intercepting => AppMode::Paused,
        };
        self.mark_changed();
    }

    pub fn select_next_pending(&mut self) {
        if !self.pending_requests.is_empty() {
            self.selected_pending = (self.selected_pending + 1) % self.pending_requests.len();
            self.reset_intercept_details_scroll();
            self.mark_changed();
        }
    }

    pub fn select_previous_pending(&mut self) {
        if !self.pending_requests.is_empty() {
            self.selected_pending = if self.selected_pending == 0 {
                self.pending_requests.len() - 1
            } else {
                self.selected_pending - 1
            };
            self.reset_intercept_details_scroll();
            self.mark_changed();
        }
    }

    pub fn get_selected_pending(&self) -> Option<&PendingRequest> {
        self.pending_requests.get(self.selected_pending)
    }

    pub fn allow_selected_request(&mut self) {
        if self.selected_pending < self.pending_requests.len() {
            let pending = self.pending_requests.remove(self.selected_pending);
            if self.selected_pending > 0 && self.selected_pending >= self.pending_requests.len() {
                self.selected_pending -= 1;
            }

            // Send decision to proxy
            let decision = if let Some(ref modified_json) = pending.modified_request {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(modified_json) {
                    ProxyDecision::Allow(Some(parsed), pending.modified_headers.clone())
                } else {
                    ProxyDecision::Allow(None, pending.modified_headers.clone())
                    // Fallback to original if modified JSON is invalid
                }
            } else {
                ProxyDecision::Allow(None, pending.modified_headers.clone()) // Use original request
            };

            let _ = pending.decision_sender.send(decision);
            self.mark_changed();
        }
    }

    pub fn block_selected_request(&mut self) {
        if self.selected_pending < self.pending_requests.len() {
            let pending = self.pending_requests.remove(self.selected_pending);
            if self.selected_pending > 0 && self.selected_pending >= self.pending_requests.len() {
                self.selected_pending -= 1;
            }

            // Send block decision to proxy
            let _ = pending.decision_sender.send(ProxyDecision::Block);
            self.mark_changed();
        }
    }

    pub fn resume_all_requests(&mut self) {
        let changed = !self.pending_requests.is_empty() || self.app_mode != AppMode::Normal;
        for pending in self.pending_requests.drain(..) {
            let _ = pending
                .decision_sender
                .send(ProxyDecision::Allow(None, None));
        }
        self.selected_pending = 0;
        self.app_mode = AppMode::Normal;
        if changed {
            self.mark_changed();
        }
    }

    pub fn get_pending_request_json(&self) -> Option<String> {
        if let Some(pending) = self.get_selected_pending() {
            // Get the original request JSON and format it nicely
            let json_value = serde_json::json!({
                "jsonrpc": "2.0",
                "method": pending.original_request.method,
                "params": pending.original_request.params,
                "id": pending.original_request.id
            });

            // Pretty print the JSON for editing
            serde_json::to_string_pretty(&json_value).ok()
        } else {
            None
        }
    }

    pub fn open_editor(&mut self, target: EditorTarget, content: String) {
        self.editor = Some(TextEditor::new(target, content));
        self.notice = None;
    }

    pub fn apply_edited_json(&mut self, edited_json: String) -> Result<(), String> {
        if self.selected_pending >= self.pending_requests.len() {
            return Err("No pending request selected".to_string());
        }

        // Parse the edited JSON
        let parsed: serde_json::Value =
            serde_json::from_str(&edited_json).map_err(|e| format!("Invalid JSON: {}", e))?;

        // Validate it's a proper JSON-RPC request
        if parsed.get("jsonrpc") != Some(&serde_json::Value::String("2.0".to_string())) {
            return Err("Missing or invalid 'jsonrpc' field".to_string());
        }

        if parsed.get("method").is_none() {
            return Err("Missing 'method' field".to_string());
        }

        // Store the modified request
        self.pending_requests[self.selected_pending].modified_request = Some(edited_json);
        self.mark_changed();

        Ok(())
    }

    pub fn get_pending_request_headers(&self) -> Option<String> {
        if let Some(pending) = self.get_selected_pending() {
            // Get headers (modified if available, otherwise original)
            let headers = pending
                .modified_headers
                .as_ref()
                .or(pending.original_request.headers.as_ref());

            if let Some(headers) = headers {
                // Format headers as key: value pairs for editing
                let mut header_lines = Vec::new();
                for (key, value) in headers {
                    header_lines.push(format!("{}: {}", key, value));
                }
                Some(header_lines.join("\n"))
            } else {
                Some(
                    "# No headers\n# Add headers in the format:\n# header-name: header-value"
                        .to_string(),
                )
            }
        } else {
            None
        }
    }

    pub fn apply_edited_headers(&mut self, edited_headers: String) -> Result<(), String> {
        if self.selected_pending >= self.pending_requests.len() {
            return Err("No pending request selected".to_string());
        }

        let mut headers = HashMap::new();

        for line in edited_headers.lines() {
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Parse header line (key: value)
            if let Some(colon_pos) = line.find(':') {
                let key = line[..colon_pos].trim().to_string();
                let value = line[colon_pos + 1..].trim().to_string();

                if !key.is_empty() {
                    headers.insert(key, value);
                }
            } else {
                return Err(format!(
                    "Invalid header format: '{}'. Use 'key: value' format.",
                    line
                ));
            }
        }

        // Store the modified headers
        self.pending_requests[self.selected_pending].modified_headers = Some(headers);
        self.mark_changed();

        Ok(())
    }

    pub fn get_pending_response_template(&self) -> Option<String> {
        if let Some(pending) = self.get_selected_pending() {
            // Create a template JSON-RPC response with simple string result
            let response_template = serde_json::json!({
                "jsonrpc": "2.0",
                "id": pending.original_request.id,
                "result": "custom response"
            });

            // Pretty print the JSON for editing
            serde_json::to_string_pretty(&response_template).ok()
        } else {
            None
        }
    }

    pub fn complete_selected_request(&mut self, response_json: String) -> Result<(), String> {
        if self.selected_pending >= self.pending_requests.len() {
            return Err("No pending request selected".to_string());
        }

        // Parse the response JSON
        let parsed: serde_json::Value =
            serde_json::from_str(&response_json).map_err(|e| format!("Invalid JSON: {}", e))?;

        // Validate it's a proper JSON-RPC response
        if parsed.get("jsonrpc") != Some(&serde_json::Value::String("2.0".to_string())) {
            return Err("Missing or invalid 'jsonrpc' field".to_string());
        }

        if parsed.get("id").is_none() {
            return Err("Missing 'id' field".to_string());
        }

        // Must have either result or error, but not both
        let has_result = parsed.get("result").is_some();
        let has_error = parsed.get("error").is_some();

        if !has_result && !has_error {
            return Err("Response must have either 'result' or 'error' field".to_string());
        }

        if has_result && has_error {
            return Err("Response cannot have both 'result' and 'error' fields".to_string());
        }

        // Remove the pending request and send the completion decision
        let pending = self.pending_requests.remove(self.selected_pending);
        if self.selected_pending > 0 && self.selected_pending >= self.pending_requests.len() {
            self.selected_pending -= 1;
        }

        let _ = pending
            .decision_sender
            .send(ProxyDecision::Complete(parsed));
        self.mark_changed();

        Ok(())
    }

    pub fn prepare_new_request(&self, request_json: String) -> Result<OutboundRequest, String> {
        if !self.is_running {
            return Err("Proxy is stopped. Press Ctrl-B x to start it.".to_string());
        }
        if self.proxy_config.transparent {
            return Err("Transparent wrappers receive requests from stdin".to_string());
        }

        let parsed: serde_json::Value =
            serde_json::from_str(&request_json).map_err(|e| format!("Invalid JSON: {}", e))?;

        match &parsed {
            serde_json::Value::Array(requests) => {
                if requests.is_empty() {
                    return Err("Batch request cannot be empty".to_string());
                }
                for (index, request) in requests.iter().enumerate() {
                    validate_json_rpc_message(request, self.proxy_config.stdio.is_some())
                        .map_err(|error| format!("Batch item {}: {error}", index + 1))?;
                }
            }
            request => validate_json_rpc_message(request, self.proxy_config.stdio.is_some())?,
        }

        // Check if target URL is empty
        if self.proxy_config.target_url.trim().is_empty() {
            return Err(
                "Target URL is not set. Press Ctrl-B t to set a target URL first.".to_string(),
            );
        }

        let url = if matches!(self.app_mode, AppMode::Paused | AppMode::Intercepting) {
            self.proxy_config.target_url.clone()
        } else {
            format!("http://localhost:{}", self.proxy_config.listen_port)
        };

        Ok(OutboundRequest {
            url,
            body: request_json,
        })
    }
}

fn validate_json_rpc_message(
    request: &serde_json::Value,
    allow_response: bool,
) -> Result<(), String> {
    if request.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Err("Missing or invalid 'jsonrpc' field".to_string());
    }
    if request.get("method").is_some() {
        return Ok(());
    }
    if allow_response
        && request.get("id").is_some()
        && (request.get("result").is_some() || request.get("error").is_some())
    {
        return Ok(());
    }
    Err("Missing 'method' field".to_string())
}

pub async fn send_new_request(request: OutboundRequest) -> Result<serde_json::Value, String> {
    let response = reqwest::Client::new()
        .post(request.url)
        .header("Content-Type", "application/json")
        .body(request.body)
        .send()
        .await
        .map_err(|e| format!("Failed to send request: {}", e))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("Request failed with status: {status}"));
    }

    response
        .json()
        .await
        .map_err(|error| format!("Invalid JSON response: {error}"))
}
