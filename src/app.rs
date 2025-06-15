use ratatui::widgets::TableState;
use std::collections::HashMap;
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

#[derive(Debug, Clone)]
pub enum MessageDirection {
    Request,
    Response,
}

#[derive(Debug, Clone)]
pub enum TransportType {
    Http,
    #[allow(dead_code)] // Used in tests and UI display
    WebSocket,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputMode {
    Normal,
    EditingTarget,
    FilteringRequests,
}

#[derive(Debug, Clone, PartialEq)]
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
    pub table_state: TableState,
    pub details_scroll: usize,
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
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct ProxyConfig {
    pub listen_port: u16,
    pub target_url: String,
    pub transport: TransportType,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));

        Self {
            exchanges: Vec::new(),
            selected_exchange: 0,
            filter_text: String::new(),
            table_state,
            details_scroll: 0,
            intercept_details_scroll: 0,
            proxy_config: ProxyConfig {
                listen_port: 8080,
                target_url: "".to_string(),
                transport: TransportType::Http,
            },
            is_running: true,
            message_receiver: None,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            app_mode: AppMode::Normal,
            pending_requests: Vec::new(),
            selected_pending: 0,
            request_editor_buffer: String::new(),
        }
    }

    pub fn new_with_receiver(receiver: mpsc::UnboundedReceiver<JsonRpcMessage>) -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));

        Self {
            exchanges: Vec::new(),
            selected_exchange: 0,
            filter_text: String::new(),
            table_state,
            details_scroll: 0,
            intercept_details_scroll: 0,
            proxy_config: ProxyConfig {
                listen_port: 8080,
                target_url: "".to_string(),
                transport: TransportType::Http,
            },
            is_running: true,
            message_receiver: Some(receiver),
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            app_mode: AppMode::Normal,
            pending_requests: Vec::new(),
            selected_pending: 0,
            request_editor_buffer: String::new(),
        }
    }

    pub fn check_for_new_messages(&mut self) {
        if let Some(receiver) = &mut self.message_receiver {
            let mut new_messages = Vec::new();
            while let Ok(message) = receiver.try_recv() {
                new_messages.push(message);
            }
            for message in new_messages {
                self.add_message(message);
            }
        }
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
                    transport: message.transport.clone(),
                };
                self.exchanges.push(exchange);
            }
            MessageDirection::Response => {
                // Find matching request by ID and add response
                if let Some(exchange) = self
                    .exchanges
                    .iter_mut()
                    .rev()
                    .find(|e| e.id == message.id && e.response.is_none())
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
                        transport: message.transport.clone(),
                    };
                    self.exchanges.push(exchange);
                }
            }
        }
    }

    pub fn get_selected_exchange(&self) -> Option<&JsonRpcExchange> {
        self.exchanges.get(self.selected_exchange)
    }

    pub fn select_next(&mut self) {
        if !self.exchanges.is_empty() {
            self.selected_exchange = (self.selected_exchange + 1) % self.exchanges.len();
            self.table_state.select(Some(self.selected_exchange));
            self.reset_details_scroll();
        }
    }

    pub fn select_previous(&mut self) {
        if !self.exchanges.is_empty() {
            self.selected_exchange = if self.selected_exchange == 0 {
                self.exchanges.len() - 1
            } else {
                self.selected_exchange - 1
            };
            self.table_state.select(Some(self.selected_exchange));
            self.reset_details_scroll();
        }
    }

    pub fn toggle_proxy(&mut self) {
        self.is_running = !self.is_running;
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

    pub fn page_down_intercept_details(&mut self, visible_lines: usize) {
        let page_size = visible_lines / 2; // Half page
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
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    // Get content lines for proper scrolling calculations
    // Target editing methods
    pub fn start_editing_target(&mut self) {
        self.input_mode = InputMode::EditingTarget;
        self.input_buffer.clear();
    }

    pub fn cancel_editing(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    pub fn confirm_target_edit(&mut self) {
        if !self.input_buffer.trim().is_empty() {
            self.proxy_config.target_url = self.input_buffer.trim().to_string();
        }
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    pub fn handle_input_char(&mut self, c: char) {
        if self.input_mode == InputMode::EditingTarget
            || self.input_mode == InputMode::FilteringRequests
        {
            self.input_buffer.push(c);
        }
    }

    pub fn handle_backspace(&mut self) {
        if self.input_mode == InputMode::EditingTarget
            || self.input_mode == InputMode::FilteringRequests
        {
            self.input_buffer.pop();
        }
    }

    pub fn get_details_content_lines(&self) -> usize {
        if let Some(exchange) = self.get_selected_exchange() {
            let mut line_count = 0;

            // Basic info lines
            line_count += 3; // Transport, Method, ID

            // Request section
            if let Some(request) = &exchange.request {
                line_count += 2; // Empty line + "REQUEST:" header

                if let Some(headers) = &request.headers {
                    line_count += 2; // Empty line + "HTTP Headers:"
                    line_count += headers.len();
                }

                line_count += 2; // Empty line + "JSON-RPC Request:"

                // Estimate JSON lines (rough calculation)
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

            // Response section
            if let Some(response) = &exchange.response {
                line_count += 2; // Empty line + "RESPONSE:" header

                if let Some(headers) = &response.headers {
                    line_count += 2; // Empty line + "HTTP Headers:"
                    line_count += headers.len();
                }

                line_count += 2; // Empty line + "JSON-RPC Response:"

                // Estimate JSON lines
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
            } else {
                line_count += 2; // Empty line + "RESPONSE: Pending..."
            }

            line_count
        } else {
            1 // "No exchange selected"
        }
    }

    // Pause/Intercept functionality
    pub fn toggle_pause_mode(&mut self) {
        self.app_mode = match self.app_mode {
            AppMode::Normal => AppMode::Paused,
            AppMode::Paused => AppMode::Normal,
            AppMode::Intercepting => AppMode::Normal,
        };
    }

    pub fn select_next_pending(&mut self) {
        if !self.pending_requests.is_empty() {
            self.selected_pending = (self.selected_pending + 1) % self.pending_requests.len();
            self.reset_intercept_details_scroll();
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
        }
    }

    pub fn resume_all_requests(&mut self) {
        for pending in self.pending_requests.drain(..) {
            let _ = pending
                .decision_sender
                .send(ProxyDecision::Allow(None, None));
        }
        self.selected_pending = 0;
        self.app_mode = AppMode::Normal;
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

        Ok(())
    }

    pub async fn send_new_request(&self, request_json: String) -> Result<(), String> {
        // Parse the request JSON
        let parsed: serde_json::Value =
            serde_json::from_str(&request_json).map_err(|e| format!("Invalid JSON: {}", e))?;

        // Validate it's a proper JSON-RPC request
        if parsed.get("jsonrpc") != Some(&serde_json::Value::String("2.0".to_string())) {
            return Err("Missing or invalid 'jsonrpc' field".to_string());
        }

        if parsed.get("method").is_none() {
            return Err("Missing 'method' field".to_string());
        }

        // Check if target URL is empty
        if self.proxy_config.target_url.trim().is_empty() {
            return Err("Target URL is not set. Press 't' to set a target URL first.".to_string());
        }

        let client = reqwest::Client::new();

        // If we're in paused mode, send directly to target to avoid interception
        // Otherwise, send through proxy for normal logging
        let url = if matches!(self.app_mode, AppMode::Paused | AppMode::Intercepting) {
            &self.proxy_config.target_url
        } else {
            // Send through proxy for normal logging
            &format!("http://localhost:{}", self.proxy_config.listen_port)
        };

        let response = client
            .post(url)
            .header("Content-Type", "application/json")
            .body(request_json)
            .send()
            .await
            .map_err(|e| format!("Failed to send request: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Request failed with status: {}", response.status()));
        }

        Ok(())
    }
}
