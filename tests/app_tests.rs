use jsonrpc_proxy_tui::app::*;
use std::collections::HashMap;

#[test]
fn test_app_new_creates_empty() {
    let app = App::new();
    
    // Should start empty
    assert!(app.messages.is_empty());
    assert_eq!(app.selected_message, 0);
    assert!(!app.is_running);
    assert_eq!(app.proxy_config.listen_port, 8080);
    assert_eq!(app.proxy_config.target_url, "https://eth-mainnet.g.alchemy.com/v2/demo");
}

#[test]
fn test_add_message() {
    let mut app = App::new();
    let initial_count = app.messages.len();
    
    let test_message = JsonRpcMessage {
        id: Some(serde_json::Value::Number(serde_json::Number::from(999))),
        method: Some("test_method".to_string()),
        params: Some(serde_json::json!({"test": "value"})),
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::Http,
        headers: None,
    };
    
    app.add_message(test_message);
    
    assert_eq!(app.messages.len(), initial_count + 1);
    let last_message = app.messages.last().unwrap();
    assert_eq!(last_message.method, Some("test_method".to_string()));
    assert_eq!(last_message.id, Some(serde_json::Value::Number(serde_json::Number::from(999))));
}

#[test]
fn test_navigation() {
    let mut app = App::new();
    
    // Add some test messages first
    for i in 0..3 {
        let test_message = JsonRpcMessage {
            id: Some(serde_json::Value::Number(serde_json::Number::from(i))),
            method: Some(format!("test_method_{}", i)),
            params: None,
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: MessageDirection::Request,
            transport: TransportType::Http,
            headers: None,
        };
        app.add_message(test_message);
    }
    
    let message_count = app.messages.len();
    
    // Test selecting next
    app.select_next();
    assert_eq!(app.selected_message, 1);
    
    // Test wrapping around at end
    app.selected_message = message_count - 1;
    app.select_next();
    assert_eq!(app.selected_message, 0);
    
    // Test selecting previous
    app.selected_message = 1;
    app.select_previous();
    assert_eq!(app.selected_message, 0);
    
    // Test wrapping around at beginning
    app.select_previous();
    assert_eq!(app.selected_message, message_count - 1);
}

#[test]
fn test_get_selected_message() {
    let mut app = App::new();
    
    // Test with empty app
    assert!(app.get_selected_message().is_none());
    
    // Add a message and test selection
    let test_message = JsonRpcMessage {
        id: Some(serde_json::Value::Number(serde_json::Number::from(1))),
        method: Some("test_method".to_string()),
        params: None,
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::Http,
        headers: None,
    };
    app.add_message(test_message);
    
    let selected = app.get_selected_message();
    assert!(selected.is_some());
    assert_eq!(selected.unwrap().method, Some("test_method".to_string()));
}

#[test]
fn test_toggle_proxy() {
    let mut app = App::new();
    
    assert!(!app.is_running);
    app.toggle_proxy();
    assert!(app.is_running);
    app.toggle_proxy();
    assert!(!app.is_running);
}

#[test]
fn test_message_types() {
    let mut app = App::new();
    
    // Test HTTP request message
    let http_request = JsonRpcMessage {
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
            h.insert("Content-Type".to_string(), "application/json".to_string());
            h
        }),
    };
    app.add_message(http_request);
    
    // Test HTTP response message
    let http_response = JsonRpcMessage {
        id: Some(serde_json::Value::Number(serde_json::Number::from(1))),
        method: None,
        params: None,
        result: Some(serde_json::json!("0x1b1ae4d6e2ef500000")),
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Response,
        transport: TransportType::Http,
        headers: Some({
            let mut h = HashMap::new();
            h.insert("Content-Type".to_string(), "application/json".to_string());
            h
        }),
    };
    app.add_message(http_response);
    
    // Test WebSocket request message
    let ws_request = JsonRpcMessage {
        id: Some(serde_json::Value::String("ws-123".to_string())),
        method: Some("eth_subscribe".to_string()),
        params: Some(serde_json::json!(["newHeads"])),
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::WebSocket,
        headers: None, // WebSocket shouldn't have headers
    };
    app.add_message(ws_request);
    
    // Test error response message
    let error_response = JsonRpcMessage {
        id: Some(serde_json::Value::Number(serde_json::Number::from(2))),
        method: None,
        params: None,
        result: None,
        error: Some(serde_json::json!({
            "code": -32602,
            "message": "Invalid params"
        })),
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Response,
        transport: TransportType::Http,
        headers: None,
    };
    app.add_message(error_response);
    
    // Verify we have all 4 messages
    assert_eq!(app.messages.len(), 4);
    
    // Check first message is HTTP request
    let first_msg = &app.messages[0];
    assert!(matches!(first_msg.direction, MessageDirection::Request));
    assert!(matches!(first_msg.transport, TransportType::Http));
    assert_eq!(first_msg.method, Some("eth_getBalance".to_string()));
    assert!(first_msg.headers.is_some());
    
    // Check second message is HTTP response
    let second_msg = &app.messages[1];
    assert!(matches!(second_msg.direction, MessageDirection::Response));
    assert!(matches!(second_msg.transport, TransportType::Http));
    assert!(second_msg.result.is_some());
    
    // Check third message is WebSocket request
    let third_msg = &app.messages[2];
    assert!(matches!(third_msg.direction, MessageDirection::Request));
    assert!(matches!(third_msg.transport, TransportType::WebSocket));
    assert_eq!(third_msg.method, Some("eth_subscribe".to_string()));
    assert!(third_msg.headers.is_none()); // WebSocket shouldn't have headers
    
    // Check fourth message is error response
    let fourth_msg = &app.messages[3];
    assert!(matches!(fourth_msg.direction, MessageDirection::Response));
    assert!(fourth_msg.error.is_some());
    assert!(fourth_msg.result.is_none());
}

#[test]
fn test_json_rpc_message_creation() {
    let headers = {
        let mut h = HashMap::new();
        h.insert("Content-Type".to_string(), "application/json".to_string());
        h
    };
    
    let message = JsonRpcMessage {
        id: Some(serde_json::Value::String("test-id".to_string())),
        method: Some("test_method".to_string()),
        params: Some(serde_json::json!({"param1": "value1"})),
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::Http,
        headers: Some(headers),
    };
    
    assert_eq!(message.method, Some("test_method".to_string()));
    assert!(message.headers.is_some());
    assert!(matches!(message.direction, MessageDirection::Request));
    assert!(matches!(message.transport, TransportType::Http));
}

#[test]
fn test_proxy_config() {
    let config = ProxyConfig {
        listen_port: 9090,
        target_url: "ws://localhost:8545".to_string(),
        transport: TransportType::WebSocket,
    };
    
    assert_eq!(config.listen_port, 9090);
    assert_eq!(config.target_url, "ws://localhost:8545");
    assert!(matches!(config.transport, TransportType::WebSocket));
} 