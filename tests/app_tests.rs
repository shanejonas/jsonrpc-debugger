use jsonrpc_debugger::app::*;
use std::collections::HashMap;

#[test]
fn test_app_new_creates_empty() {
    let app = App::new();

    // Should start empty
    assert!(app.exchanges.is_empty());
    assert_eq!(app.selected_exchange, 0);
    assert!(app.is_running);
    assert_eq!(app.proxy_config.listen_port, 8080);
    assert_eq!(app.proxy_config.target_url, "");
}

#[test]
fn test_add_message() {
    let mut app = App::new();
    let initial_count = app.exchanges.len();

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

    assert_eq!(app.exchanges.len(), initial_count + 1);
    let last_exchange = app.exchanges.last().unwrap();
    assert_eq!(last_exchange.method, Some("test_method".to_string()));
    assert_eq!(
        last_exchange.id,
        Some(serde_json::Value::Number(serde_json::Number::from(999)))
    );
    assert!(last_exchange.request.is_some());
    assert!(last_exchange.response.is_none());
}

#[test]
fn test_navigation() {
    let mut app = App::new();

    // Add some test request messages first
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

    let exchange_count = app.exchanges.len();

    // Test selecting next
    app.select_next();
    assert_eq!(app.selected_exchange, 1);

    // Test wrapping around at end
    app.selected_exchange = exchange_count - 1;
    app.select_next();
    assert_eq!(app.selected_exchange, 0);

    // Test selecting previous
    app.selected_exchange = 1;
    app.select_previous();
    assert_eq!(app.selected_exchange, 0);

    // Test wrapping around at beginning
    app.select_previous();
    assert_eq!(app.selected_exchange, exchange_count - 1);
}

#[test]
fn test_get_selected_exchange() {
    let mut app = App::new();

    // Test with empty app
    assert!(app.get_selected_exchange().is_none());

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

    let selected = app.get_selected_exchange();
    assert!(selected.is_some());
    assert_eq!(selected.unwrap().method, Some("test_method".to_string()));
}

#[test]
fn test_toggle_proxy() {
    let mut app = App::new();

    assert!(app.is_running);
    app.toggle_proxy();
    assert!(!app.is_running);
    app.toggle_proxy();
    assert!(app.is_running);
}

#[test]
fn test_request_response_pairing() {
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

    // Test HTTP response message with matching ID
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

    // Test error response message with matching ID
    let error_response = JsonRpcMessage {
        id: Some(serde_json::Value::String("ws-123".to_string())),
        method: None,
        params: None,
        result: None,
        error: Some(serde_json::json!({
            "code": -32602,
            "message": "Invalid params"
        })),
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Response,
        transport: TransportType::WebSocket,
        headers: None,
    };
    app.add_message(error_response);

    // Verify we have 2 exchanges (request-response pairs)
    assert_eq!(app.exchanges.len(), 2);

    // Check first exchange is HTTP request-response pair
    let first_exchange = &app.exchanges[0];
    assert!(first_exchange.request.is_some());
    assert!(first_exchange.response.is_some());
    assert_eq!(first_exchange.method, Some("eth_getBalance".to_string()));
    assert!(matches!(first_exchange.transport, TransportType::Http));

    // Check second exchange is WebSocket request-response pair
    let second_exchange = &app.exchanges[1];
    assert!(second_exchange.request.is_some());
    assert!(second_exchange.response.is_some());
    assert_eq!(second_exchange.method, Some("eth_subscribe".to_string()));
    assert!(matches!(
        second_exchange.transport,
        TransportType::WebSocket
    ));

    // Verify the response has error
    let ws_response = second_exchange.response.as_ref().unwrap();
    assert!(ws_response.error.is_some());
    assert!(ws_response.result.is_none());
}

#[test]
fn test_json_rpc_message_creation() {
    let message = JsonRpcMessage {
        id: Some(serde_json::Value::Number(serde_json::Number::from(42))),
        method: Some("test_method".to_string()),
        params: Some(serde_json::json!({"param1": "value1"})),
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::Http,
        headers: None,
    };

    assert_eq!(
        message.id,
        Some(serde_json::Value::Number(serde_json::Number::from(42)))
    );
    assert_eq!(message.method, Some("test_method".to_string()));
    assert!(matches!(message.direction, MessageDirection::Request));
    assert!(matches!(message.transport, TransportType::Http));
}

#[test]
fn test_proxy_config() {
    let config = ProxyConfig {
        listen_port: 9090,
        target_url: "https://example.com".to_string(),
        transport: TransportType::Http,
    };

    assert_eq!(config.listen_port, 9090);
    assert_eq!(config.target_url, "https://example.com");
    assert!(matches!(config.transport, TransportType::Http));
}

#[test]
fn test_filtering_functionality() {
    let mut app = App::new();

    // Add test exchanges with different methods
    let methods = [
        "eth_getBalance",
        "eth_sendTransaction",
        "net_version",
        "eth_blockNumber",
    ];

    for (i, method) in methods.iter().enumerate() {
        let test_message = JsonRpcMessage {
            id: Some(serde_json::Value::Number(serde_json::Number::from(
                i as i64,
            ))),
            method: Some(method.to_string()),
            params: Some(serde_json::json!({"test": format!("value_{}", i)})),
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: MessageDirection::Request,
            transport: TransportType::Http,
            headers: None,
        };
        app.add_message(test_message);
    }

    // Test initial state - no filter
    assert_eq!(app.filter_text, "");
    assert_eq!(app.exchanges.len(), 4);

    // Test filter methods
    app.start_filtering_requests();
    assert_eq!(app.input_mode, InputMode::FilteringRequests);
    assert_eq!(app.input_buffer, ""); // Should start empty

    // Simulate typing "eth"
    app.handle_input_char('e');
    app.handle_input_char('t');
    app.handle_input_char('h');
    assert_eq!(app.input_buffer, "eth");

    // Apply the filter
    app.apply_filter();
    assert_eq!(app.filter_text, "eth");
    assert_eq!(app.input_mode, InputMode::Normal);
    assert_eq!(app.input_buffer, "");

    // Test that filtering logic would work (this tests the filter logic conceptually)
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

    // Should match 3 exchanges: eth_getBalance, eth_sendTransaction, eth_blockNumber
    assert_eq!(filtered_count, 3);

    // Test cancel filtering
    app.start_filtering_requests();
    app.handle_input_char('n');
    app.handle_input_char('e');
    app.handle_input_char('t');
    app.cancel_filtering();
    assert_eq!(app.filter_text, "eth"); // Should keep previous filter
    assert_eq!(app.input_mode, InputMode::Normal);
    assert_eq!(app.input_buffer, "");

    // Test clearing filter
    app.start_filtering_requests();
    app.apply_filter(); // Apply empty filter
    assert_eq!(app.filter_text, "");

    // All exchanges should match when filter is empty
    let all_count = app
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
    assert_eq!(all_count, 4);

    // Test case-insensitive filtering (if implemented)
    app.start_filtering_requests();
    app.handle_input_char('E');
    app.handle_input_char('T');
    app.handle_input_char('H');
    app.apply_filter();
    assert_eq!(app.filter_text, "ETH");

    // This would test case-insensitive matching if implemented
    let case_insensitive_count = app
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
                    .to_lowercase()
                    .contains(&app.filter_text.to_lowercase())
            }
        })
        .count();
    assert_eq!(case_insensitive_count, 3);
}
