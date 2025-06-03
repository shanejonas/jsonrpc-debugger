use jsonrpc_proxy_tui::app::*;
use std::collections::HashMap;

#[test]
fn test_full_message_flow() {
    let mut app = App::new();
    let initial_count = app.messages.len();
    
    // Add a request message
    let request = JsonRpcMessage {
        id: Some(serde_json::Value::Number(serde_json::Number::from(42))),
        method: Some("eth_blockNumber".to_string()),
        params: Some(serde_json::json!([])),
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::Http,
        headers: Some({
            let mut h = HashMap::new();
            h.insert("Content-Type".to_string(), "application/json".to_string());
            h
        }),
    };
    
    app.add_message(request);
    
    // Add corresponding response
    let response = JsonRpcMessage {
        id: Some(serde_json::Value::Number(serde_json::Number::from(42))),
        method: None,
        params: None,
        result: Some(serde_json::json!("0x1234567")),
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Response,
        transport: TransportType::Http,
        headers: Some({
            let mut h = HashMap::new();
            h.insert("Content-Type".to_string(), "application/json".to_string());
            h.insert("Content-Length".to_string(), "25".to_string());
            h
        }),
    };
    
    app.add_message(response);
    
    assert_eq!(app.messages.len(), initial_count + 2);
    
    // Navigate to the new messages
    let request_idx = app.messages.len() - 2;
    let response_idx = app.messages.len() - 1;
    
    app.selected_message = request_idx;
    let selected_request = app.get_selected_message().unwrap();
    assert_eq!(selected_request.method, Some("eth_blockNumber".to_string()));
    assert!(matches!(selected_request.direction, MessageDirection::Request));
    let request_id = selected_request.id.clone();
    
    app.selected_message = response_idx;
    let selected_response = app.get_selected_message().unwrap();
    assert!(selected_response.result.is_some());
    assert!(matches!(selected_response.direction, MessageDirection::Response));
    
    // Both should have the same ID
    assert_eq!(request_id, selected_response.id);
}

#[test]
fn test_websocket_vs_http_messages() {
    let mut app = App::new();
    
    // Add HTTP message
    let http_msg = JsonRpcMessage {
        id: Some(serde_json::Value::Number(serde_json::Number::from(1))),
        method: Some("eth_getBalance".to_string()),
        params: Some(serde_json::json!(["0x123", "latest"])),
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::Http,
        headers: Some({
            let mut h = HashMap::new();
            h.insert("Authorization".to_string(), "Bearer token123".to_string());
            h
        }),
    };
    
    // Add WebSocket message
    let ws_msg = JsonRpcMessage {
        id: Some(serde_json::Value::String("ws-456".to_string())),
        method: Some("eth_subscribe".to_string()),
        params: Some(serde_json::json!(["newHeads"])),
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::WebSocket,
        headers: None, // WebSocket messages shouldn't have HTTP headers
    };
    
    app.add_message(http_msg);
    app.add_message(ws_msg);
    
    let http_message = &app.messages[app.messages.len() - 2];
    let ws_message = &app.messages[app.messages.len() - 1];
    
    // HTTP message should have headers
    assert!(http_message.headers.is_some());
    assert!(matches!(http_message.transport, TransportType::Http));
    
    // WebSocket message should not have headers
    assert!(ws_message.headers.is_none());
    assert!(matches!(ws_message.transport, TransportType::WebSocket));
}

#[test]
fn test_error_handling() {
    let mut app = App::new();
    
    // Add an error response
    let error_msg = JsonRpcMessage {
        id: Some(serde_json::Value::Number(serde_json::Number::from(999))),
        method: None,
        params: None,
        result: None,
        error: Some(serde_json::json!({
            "code": -32601,
            "message": "Method not found",
            "data": "The method 'invalid_method' does not exist"
        })),
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Response,
        transport: TransportType::Http,
        headers: None,
    };
    
    app.add_message(error_msg);
    
    let error_message = app.messages.last().unwrap();
    assert!(error_message.error.is_some());
    assert!(error_message.result.is_none());
    assert!(error_message.method.is_none());
    
    // Check error structure
    let error = error_message.error.as_ref().unwrap();
    assert_eq!(error["code"], -32601);
    assert_eq!(error["message"], "Method not found");
}

#[test]
fn test_proxy_state_management() {
    let mut app = App::new();
    
    // Initially stopped
    assert!(!app.is_running);
    
    // Start proxy
    app.toggle_proxy();
    assert!(app.is_running);
    
    // Add a message while running
    let msg = JsonRpcMessage {
        id: Some(serde_json::Value::Number(serde_json::Number::from(100))),
        method: Some("test_method".to_string()),
        params: None,
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::Http,
        headers: None,
    };
    
    app.add_message(msg);
    
    // Should still be running
    assert!(app.is_running);
    
    // Stop proxy
    app.toggle_proxy();
    assert!(!app.is_running);
    
    // Messages should still be there
    assert!(!app.messages.is_empty());
} 