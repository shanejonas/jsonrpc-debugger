use crate::app::{
    json_rpc_messages, json_rpc_messages_by_shape, AppMode, Framing, JsonRpcMessage,
    MessageDirection, PendingRequest, ProxyConfig, ProxyDecision, TransportType,
};
use crate::stdio::StdioTransport;
use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::Value;
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
};
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
    target: ProxyTarget,
    message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
    proxy_state: Option<ProxyState>,
}

#[derive(Clone)]
enum ProxyTarget {
    Http {
        url: String,
        client: Client,
    },
    Stdio {
        transport: StdioTransport,
        framing: Framing,
    },
}

impl ProxyTarget {
    fn transport(&self, body: &Value) -> TransportType {
        match self {
            Self::Http { .. } => http_transport(body),
            Self::Stdio { framing, .. } => TransportType::Stdio(*framing),
        }
    }
}

impl ProxyServer {
    pub fn new(
        listen_port: u16,
        target_url: String,
        message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
    ) -> Self {
        // Configure client for higher concurrency
        let client = Client::builder()
            .pool_max_idle_per_host(50) // More idle connections
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .http2_max_frame_size(Some(16384)) // Larger frame size
            .http2_keep_alive_interval(Some(std::time::Duration::from_secs(10)))
            .build()
            .unwrap_or_else(|_| Client::new()); // Fallback to default if config fails

        Self {
            listen_port,
            target: ProxyTarget::Http {
                url: target_url,
                client,
            },
            message_sender,
            proxy_state: None,
        }
    }

    pub fn from_config(
        config: &ProxyConfig,
        message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
    ) -> Result<Self> {
        let Some(stdio) = &config.stdio else {
            return Ok(Self::new(
                config.listen_port,
                config.target_url.clone(),
                message_sender,
            ));
        };
        let transport =
            StdioTransport::spawn(&stdio.command, stdio.framing, message_sender.clone())
                .map_err(anyhow::Error::msg)?;
        Ok(Self {
            listen_port: config.listen_port,
            target: ProxyTarget::Stdio {
                transport,
                framing: stdio.framing,
            },
            message_sender,
            proxy_state: None,
        })
    }

    pub fn with_state(mut self, proxy_state: ProxyState) -> Self {
        self.proxy_state = Some(proxy_state);
        self
    }

    pub async fn start(&self) -> Result<()> {
        self.bind()?.await;
        Ok(())
    }

    pub fn bind(&self) -> Result<impl Future<Output = ()> + 'static> {
        let target = self.target.clone();
        let message_sender = self.message_sender.clone();
        let proxy_state = self.proxy_state.clone();

        let proxy_route = warp::path::full()
            .and(warp::post())
            .and(warp::header::headers_cloned())
            .and(warp::body::json())
            .and_then(
                move |path: warp::path::FullPath, headers: warp::http::HeaderMap, body: Value| {
                    let target = target.clone();
                    let message_sender = message_sender.clone();
                    let proxy_state = proxy_state.clone();

                    async move {
                        handle_proxy_request(
                            path,
                            headers,
                            body,
                            target,
                            message_sender,
                            proxy_state,
                        )
                        .await
                    }
                },
            );

        let cors = warp::cors()
            .allow_any_origin()
            .allow_headers(vec!["content-type", "authorization"])
            .allow_methods(vec!["POST", "OPTIONS"]);

        let routes = proxy_route.with(cors);

        let address = ([127, 0, 0, 1], self.listen_port);
        let (_, server) = warp::serve(routes)
            .try_bind_ephemeral(address)
            .with_context(|| format!("bind proxy port {}", self.listen_port))?;
        Ok(server)
    }
}

async fn handle_proxy_request(
    path: warp::path::FullPath,
    headers: warp::http::HeaderMap,
    body: Value,
    target: ProxyTarget,
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

    // Log each JSON-RPC request in the HTTP body.
    let transport = target.transport(&body);
    let request_messages = if matches!(transport, TransportType::Stdio(_)) {
        json_rpc_messages_by_shape(&body, transport, Some(&header_map))
    } else {
        json_rpc_messages(
            &body,
            MessageDirection::Request,
            transport,
            Some(&header_map),
        )
    };
    let request_message = request_messages.first().cloned().unwrap_or(JsonRpcMessage {
        id: None,
        method: None,
        params: None,
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: if matches!(transport, TransportType::Stdio(_)) && body.get("method").is_none() {
            MessageDirection::Response
        } else {
            MessageDirection::Request
        },
        transport,
        headers: Some(header_map.clone()),
    });
    for message in request_messages {
        let _ = message_sender.send(message);
    }

    // Check if we're in pause mode and should intercept the request
    if let Some(ref state) = proxy_state {
        let should_intercept = if let Ok(app_mode) = state.app_mode.lock() {
            matches!(*app_mode, AppMode::Paused)
                && matches!(request_message.direction, MessageDirection::Request)
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
                        path.as_str(),
                        target,
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
                        transport,
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
    forward_request(headers, body, path.as_str(), target, message_sender).await
}

async fn forward_request(
    headers: warp::http::HeaderMap,
    body: Value,
    path: &str,
    target: ProxyTarget,
    message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
) -> Result<Box<dyn warp::Reply>, warp::Rejection> {
    match target {
        ProxyTarget::Http { url, client } => {
            forward_http_request(
                headers,
                body,
                format!("{url}{path}"),
                client,
                message_sender,
            )
            .await
        }
        ProxyTarget::Stdio { transport, framing } => {
            forward_stdio_request(body, transport, framing, message_sender).await
        }
    }
}

async fn forward_http_request(
    headers: warp::http::HeaderMap,
    body: Value,
    target_url: String,
    client: Client,
    message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
) -> Result<Box<dyn warp::Reply>, warp::Rejection> {
    let transport = http_transport(&body);
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
                            for message in json_rpc_messages(
                                &response_body,
                                MessageDirection::Response,
                                transport,
                                Some(&response_header_map),
                            ) {
                                let _ = message_sender.send(message);
                            }

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
                                transport,
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
                        transport,
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
                transport,
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

async fn forward_stdio_request(
    body: Value,
    target: StdioTransport,
    framing: Framing,
    message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
) -> Result<Box<dyn warp::Reply>, warp::Rejection> {
    match target.send(body.clone()).await {
        Ok(response) => Ok(Box::new(warp::reply::with_status(
            warp::reply::json(&response),
            warp::http::StatusCode::OK,
        ))),
        Err(message) => {
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": body.get("id").cloned().unwrap_or(Value::Null),
                "error": {
                    "code": -32603,
                    "message": message,
                }
            });
            for message in json_rpc_messages(
                &response,
                MessageDirection::Response,
                TransportType::Stdio(framing),
                None,
            ) {
                let _ = message_sender.send(message);
            }
            Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&response),
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

fn http_transport(body: &Value) -> TransportType {
    if body.is_array() {
        TransportType::HttpBatch
    } else {
        TransportType::Http
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn batch_bodies_become_individual_messages() {
        let request = json!([
            {"jsonrpc": "2.0", "method": "example_first", "params": [], "id": 1},
            {"jsonrpc": "2.0", "method": "example_second", "params": [], "id": 2}
        ]);
        let requests = json_rpc_messages(
            &request,
            MessageDirection::Request,
            TransportType::HttpBatch,
            None,
        );

        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .all(|message| matches!(message.transport, TransportType::HttpBatch)));
        assert_eq!(requests[0].method.as_deref(), Some("example_first"));
        assert_eq!(requests[1].id, Some(json!(2)));

        let response = json!([
            {"jsonrpc": "2.0", "id": 2, "result": "second"},
            {"jsonrpc": "2.0", "id": 1, "result": "first"}
        ]);
        let responses = json_rpc_messages(
            &response,
            MessageDirection::Response,
            TransportType::HttpBatch,
            None,
        );

        assert_eq!(responses.len(), 2);
        assert!(responses
            .iter()
            .all(|message| matches!(message.transport, TransportType::HttpBatch)));
        assert_eq!(responses[0].id, Some(json!(2)));
        assert_eq!(responses[1].result, Some(json!("first")));

        let rejected = json_rpc_messages(
            &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700}}),
            MessageDirection::Response,
            TransportType::HttpBatch,
            None,
        );
        assert!(matches!(rejected[0].transport, TransportType::HttpBatch));

        let mut app = crate::app::App::new();
        for message in requests.into_iter().chain(responses) {
            app.add_message(message);
        }
        assert_eq!(app.exchanges.len(), 2);
        assert!(app
            .exchanges
            .iter()
            .all(|exchange| exchange.response.is_some()));
    }
}
