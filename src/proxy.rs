use crate::app::{
    AppMode, JsonRpcMessage, MessageDirection, PendingRequest, ProxyDecision, TransportType,
};
use anyhow::Result;
use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use tokio::sync::{mpsc, oneshot};
use warp::Filter;

// Shared state between app and proxy
#[derive(Clone)]
pub struct ProxyState {
    pub app_mode: Arc<Mutex<AppMode>>,
    pub pending_sender: mpsc::UnboundedSender<PendingRequest>,
}

pub struct ProxyServer {
    listen_port: u16,
    target_url: String,
    message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
    client: Client,
    proxy_state: Option<ProxyState>,
}

impl ProxyServer {
    pub fn new(
        listen_port: u16,
        target_url: String,
        message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
    ) -> Self {
        Self {
            listen_port,
            target_url,
            message_sender,
            client: Client::new(),
            proxy_state: None,
        }
    }

    pub fn with_state(mut self, proxy_state: ProxyState) -> Self {
        self.proxy_state = Some(proxy_state);
        self
    }

    pub async fn start(&self) -> Result<()> {
        let target_url = self.target_url.clone();
        let client = self.client.clone();
        let message_sender = self.message_sender.clone();
        let proxy_state = self.proxy_state.clone();

        let proxy_route = warp::path::end()
            .and(warp::post())
            .and(warp::header::headers_cloned())
            .and(warp::body::json())
            .and_then(move |headers: warp::http::HeaderMap, body: Value| {
                let target_url = target_url.clone();
                let client = client.clone();
                let message_sender = message_sender.clone();
                let proxy_state = proxy_state.clone();

                async move {
                    handle_proxy_request(
                        headers,
                        body,
                        target_url,
                        client,
                        message_sender,
                        proxy_state,
                    )
                    .await
                }
            });

        let cors = warp::cors()
            .allow_any_origin()
            .allow_headers(vec!["content-type", "authorization"])
            .allow_methods(vec!["POST", "OPTIONS"]);

        let routes = proxy_route.with(cors);

        // Use a simpler approach - just run the server
        // The task abort from main.rs will handle shutdown
        warp::serve(routes)
            .run(([127, 0, 0, 1], self.listen_port))
            .await;

        Ok(())
    }
}

async fn handle_proxy_request(
    headers: warp::http::HeaderMap,
    body: Value,
    target_url: String,
    client: Client,
    message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
    proxy_state: Option<ProxyState>,
) -> Result<Box<dyn warp::Reply>, warp::Rejection> {
    // Convert headers to HashMap
    let mut header_map = HashMap::new();
    for (name, value) in headers.iter() {
        if let Ok(value_str) = value.to_str() {
            header_map.insert(name.to_string(), value_str.to_string());
        }
    }

    // Log the incoming request
    let request_message = JsonRpcMessage {
        id: body.get("id").cloned(),
        method: body
            .get("method")
            .and_then(|m| m.as_str())
            .map(String::from),
        params: body.get("params").cloned(),
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::Http,
        headers: Some(header_map.clone()),
    };

    let _ = message_sender.send(request_message.clone());

    // Check if we're in pause mode and should intercept the request
    if let Some(ref state) = proxy_state {
        let should_intercept = if let Ok(app_mode) = state.app_mode.lock() {
            matches!(*app_mode, AppMode::Paused)
        } else {
            false
        };

        if should_intercept {
            // Create oneshot channel for decision
            let (decision_sender, decision_receiver) = oneshot::channel();

            // Create a pending request
            let pending_request = PendingRequest {
                id: Uuid::new_v4().to_string(),
                original_request: request_message,
                modified_request: None,
                modified_headers: None,
                decision_sender,
            };

            // Send to app for interception
            let _ = state.pending_sender.send(pending_request);

            // Wait for user decision with timeout
            let decision = tokio::time::timeout(
                std::time::Duration::from_secs(300), // 5 minute timeout
                decision_receiver,
            )
            .await;

            return match decision {
                Ok(Ok(ProxyDecision::Allow(modified_json, modified_headers))) => {
                    // Use modified JSON if provided, otherwise use original body
                    let request_body = modified_json.unwrap_or(body);

                    // Use modified headers if provided, otherwise use original headers
                    let final_headers = if let Some(mod_headers) = modified_headers {
                        // Convert HashMap to HeaderMap
                        let mut header_map = warp::http::HeaderMap::new();
                        for (key, value) in mod_headers {
                            if let (Ok(header_name), Ok(header_value)) = (
                                warp::http::header::HeaderName::from_bytes(key.as_bytes()),
                                warp::http::header::HeaderValue::from_str(&value),
                            ) {
                                header_map.insert(header_name, header_value);
                            }
                        }
                        header_map
                    } else {
                        headers
                    };

                    forward_request(
                        final_headers,
                        request_body,
                        target_url,
                        client,
                        message_sender,
                    )
                    .await
                }
                Ok(Ok(ProxyDecision::Block)) => {
                    // Return blocked response
                    Ok(Box::new(warp::reply::with_status(
                        warp::reply::json(&serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": body.get("id"),
                            "error": {
                                "code": -32603,
                                "message": "Request blocked by user"
                            }
                        })),
                        warp::http::StatusCode::OK,
                    )))
                }
                Ok(Ok(ProxyDecision::Complete(response_json))) => {
                    // Log the custom response
                    let response_message = JsonRpcMessage {
                        id: response_json.get("id").cloned(),
                        method: None,
                        params: None,
                        result: response_json.get("result").cloned(),
                        error: response_json.get("error").cloned(),
                        timestamp: std::time::SystemTime::now(),
                        direction: MessageDirection::Response,
                        transport: TransportType::Http,
                        headers: Some(HashMap::from([
                            ("content-type".to_string(), "application/json".to_string()),
                            ("x-proxy-completed".to_string(), "true".to_string()),
                        ])),
                    };

                    let _ = message_sender.send(response_message);

                    // Return the custom response
                    Ok(Box::new(warp::reply::with_status(
                        warp::reply::json(&response_json),
                        warp::http::StatusCode::OK,
                    )))
                }
                Ok(Err(_)) | Err(_) => {
                    // Timeout or channel error - return timeout response
                    Ok(Box::new(warp::reply::with_status(
                        warp::reply::json(&serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": body.get("id"),
                            "error": {
                                "code": -32603,
                                "message": "Request timed out waiting for user decision"
                            }
                        })),
                        warp::http::StatusCode::REQUEST_TIMEOUT,
                    )))
                }
            };
        }
    }

    // Normal forwarding (not intercepted)
    forward_request(headers, body, target_url, client, message_sender).await
}

async fn forward_request(
    headers: warp::http::HeaderMap,
    body: Value,
    target_url: String,
    client: Client,
    message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
) -> Result<Box<dyn warp::Reply>, warp::Rejection> {
    // Forward the request to the target
    let mut request_builder = client.post(&target_url).json(&body);

    // Forward relevant headers
    for (name, value) in headers.iter() {
        if should_forward_header(name.as_str()) {
            request_builder = request_builder.header(name, value);
        }
    }

    match request_builder.send().await {
        Ok(response) => {
            let status = response.status();
            let response_headers = response.headers().clone();

            // Convert response headers
            let mut response_header_map = HashMap::new();
            for (name, value) in response_headers.iter() {
                if let Ok(value_str) = value.to_str() {
                    response_header_map.insert(name.to_string(), value_str.to_string());
                }
            }

            // Get the response text - reqwest should handle decompression automatically
            match response.text().await {
                Ok(response_text) => {
                    // Try to parse as JSON
                    match serde_json::from_str::<Value>(&response_text) {
                        Ok(response_body) => {
                            // Valid JSON response
                            let response_message = JsonRpcMessage {
                                id: response_body.get("id").cloned(),
                                method: None,
                                params: None,
                                result: response_body.get("result").cloned(),
                                error: response_body.get("error").cloned(),
                                timestamp: std::time::SystemTime::now(),
                                direction: MessageDirection::Response,
                                transport: TransportType::Http,
                                headers: Some(response_header_map.clone()),
                            };

                            let _ = message_sender.send(response_message);

                            // Return the original response as-is
                            Ok(Box::new(warp::reply::with_status(
                                warp::reply::json(&response_body),
                                status,
                            )))
                        }
                        Err(parse_error) => {
                            // Not valid JSON - analyze the response to provide better error info
                            let content_type = response_header_map
                                .get("content-type")
                                .unwrap_or(&"unknown".to_string())
                                .clone();

                            // Check if response contains null bytes (binary data)
                            let has_null_bytes = response_text.contains('\0');
                            let is_empty = response_text.trim().is_empty();

                            // Get a safe preview of the response content
                            let content_preview = if has_null_bytes {
                                // Show hex representation for binary data
                                let bytes: Vec<u8> = response_text.bytes().take(50).collect();
                                format!("Binary data: {:02x?}...", bytes)
                            } else if response_text.trim().starts_with('{')
                                || response_text.trim().starts_with('[')
                            {
                                // For JSON-like content, show more text
                                if response_text.len() > 500 {
                                    format!("{}...", &response_text[..500])
                                } else {
                                    response_text.clone()
                                }
                            } else if response_text.len() > 200 {
                                format!("{}...", &response_text[..200])
                            } else {
                                response_text.clone()
                            };

                            // Determine the likely issue
                            let issue_type = if is_empty {
                                "empty_response"
                            } else if has_null_bytes {
                                "binary_data"
                            } else if content_type.contains("text/html") {
                                "html_response"
                            } else if content_type.contains("application/json") {
                                "malformed_json"
                            } else {
                                "unknown_format"
                            };

                            let error_message = JsonRpcMessage {
                                id: body.get("id").cloned(),
                                method: None,
                                params: None,
                                result: None,
                                error: Some(serde_json::json!({
                                    "code": -32700,
                                    "message": format!("Invalid JSON response from server (HTTP {})", status),
                                    "data": {
                                        "issue_type": issue_type,
                                        "content_type": content_type,
                                        "response_preview": content_preview,
                                        "response_length": response_text.len(),
                                        "has_null_bytes": has_null_bytes,
                                        "parse_error": parse_error.to_string(),
                                        "target_url": target_url
                                    }
                                })),
                                timestamp: std::time::SystemTime::now(),
                                direction: MessageDirection::Response,
                                transport: TransportType::Http,
                                headers: Some(response_header_map.clone()),
                            };

                            let _ = message_sender.send(error_message);

                            // Return a proper JSON-RPC error response
                            Ok(Box::new(warp::reply::with_status(
                                warp::reply::json(&serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": body.get("id"),
                                    "error": {
                                        "code": -32700,
                                        "message": format!("Invalid JSON response from server (HTTP {})", status),
                                        "data": {
                                            "issue_type": issue_type,
                                            "content_type": content_type,
                                            "has_null_bytes": has_null_bytes
                                        }
                                    }
                                })),
                                warp::http::StatusCode::OK, // Return 200 with JSON-RPC error
                            )))
                        }
                    }
                }
                Err(_e) => {
                    // Log error response
                    let error_message = JsonRpcMessage {
                        id: body.get("id").cloned(),
                        method: None,
                        params: None,
                        result: None,
                        error: Some(serde_json::json!({
                            "code": -32603,
                            "message": "Internal error - failed to read response"
                        })),
                        timestamp: std::time::SystemTime::now(),
                        direction: MessageDirection::Response,
                        transport: TransportType::Http,
                        headers: Some(response_header_map),
                    };

                    let _ = message_sender.send(error_message);

                    Ok(Box::new(warp::reply::with_status(
                        warp::reply::json(&serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": body.get("id"),
                            "error": {
                                "code": -32603,
                                "message": "Internal error - failed to read response"
                            }
                        })),
                        warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    )))
                }
            }
        }
        Err(_e) => {
            // Log connection error
            let error_message = JsonRpcMessage {
                id: body.get("id").cloned(),
                method: None,
                params: None,
                result: None,
                error: Some(serde_json::json!({
                    "code": -32603,
                    "message": "Failed to connect to target server"
                })),
                timestamp: std::time::SystemTime::now(),
                direction: MessageDirection::Response,
                transport: TransportType::Http,
                headers: None,
            };

            let _ = message_sender.send(error_message);

            Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": body.get("id"),
                    "error": {
                        "code": -32603,
                        "message": "Failed to connect to target server"
                    }
                })),
                warp::http::StatusCode::BAD_GATEWAY,
            )))
        }
    }
}

fn should_forward_header(header_name: &str) -> bool {
    !matches!(
        header_name.to_lowercase().as_str(),
        "host" | "content-length" | "transfer-encoding" | "connection"
    )
}
