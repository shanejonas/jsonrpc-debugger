use jsonrpc_proxy_tui::app::*;
use std::collections::HashMap;

#[test]
fn test_full_exchange_flow() {
    let mut app = App::new();
    let initial_count = app.exchanges.len();
    
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
    
    // Should have 1 exchange (request-response pair)
    assert_eq!(app.exchanges.len(), initial_count + 1);
    
    // Navigate to the exchange
    app.selected_exchange = app.exchanges.len() - 1;
    let selected_exchange = app.get_selected_exchange().unwrap();
    
    // Verify the exchange has both request and response
    assert!(selected_exchange.request.is_some());
    assert!(selected_exchange.response.is_some());
    assert_eq!(selected_exchange.method, Some("eth_blockNumber".to_string()));
    
    // Verify request details
    let request_msg = selected_exchange.request.as_ref().unwrap();
    assert_eq!(request_msg.method, Some("eth_blockNumber".to_string()));
    assert!(matches!(request_msg.direction, MessageDirection::Request));
    
    // Verify response details
    let response_msg = selected_exchange.response.as_ref().unwrap();
    assert!(response_msg.result.is_some());
    assert!(matches!(response_msg.direction, MessageDirection::Response));
    
    // Both should have the same ID
    assert_eq!(request_msg.id, response_msg.id);
}

#[test]
fn test_websocket_vs_http_exchanges() {
    let mut app = App::new();
    
    // Add HTTP request
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
            h.insert("Authorization".to_string(), "Bearer token123".to_string());
            h
        }),
    };
    
    // Add WebSocket request
    let ws_request = JsonRpcMessage {
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
    
    app.add_message(http_request);
    app.add_message(ws_request);
    
    // Should have 2 exchanges
    assert_eq!(app.exchanges.len(), 2);
    
    let http_exchange = &app.exchanges[0];
    let ws_exchange = &app.exchanges[1];
    
    // HTTP exchange should have headers in request
    assert!(http_exchange.request.as_ref().unwrap().headers.is_some());
    assert!(matches!(http_exchange.transport, TransportType::Http));
    
    // WebSocket exchange should not have headers
    assert!(ws_exchange.request.as_ref().unwrap().headers.is_none());
    assert!(matches!(ws_exchange.transport, TransportType::WebSocket));
}

#[test]
fn test_error_handling() {
    let mut app = App::new();
    
    // Add a request first
    let request = JsonRpcMessage {
        id: Some(serde_json::Value::Number(serde_json::Number::from(999))),
        method: Some("invalid_method".to_string()),
        params: None,
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::Http,
        headers: None,
    };
    
    app.add_message(request);
    
    // Add an error response
    let error_response = JsonRpcMessage {
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
    
    app.add_message(error_response);
    
    // Should have 1 exchange with error response
    assert_eq!(app.exchanges.len(), 1);
    let exchange = app.exchanges.last().unwrap();
    
    assert!(exchange.request.is_some());
    assert!(exchange.response.is_some());
    
    let error_response = exchange.response.as_ref().unwrap();
    assert!(error_response.error.is_some());
    assert!(error_response.result.is_none());
    assert!(error_response.method.is_none());
    
    // Check error structure
    let error = error_response.error.as_ref().unwrap();
    assert_eq!(error["code"], -32601);
    assert_eq!(error["message"], "Method not found");
}

#[test]
fn test_proxy_state_management() {
    let mut app = App::new();
    
    // Initially running (changed from original)
    assert!(app.is_running);
    
    // Stop proxy
    app.toggle_proxy();
    assert!(!app.is_running);
    
    // Start proxy again
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
    
    // Exchanges should still be there
    assert!(!app.exchanges.is_empty());
} 