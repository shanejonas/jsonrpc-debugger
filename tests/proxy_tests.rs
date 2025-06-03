use jsonrpc_proxy_tui::proxy::ProxyServer;
use jsonrpc_proxy_tui::app::*;
use tokio::sync::mpsc;
use std::collections::HashMap;

#[tokio::test]
async fn test_proxy_server_creation() {
    let (sender, _receiver) = mpsc::unbounded_channel();
    
    let proxy = ProxyServer::new(
        8080,
        "https://mock.open-rpc.org".to_string(),
        sender,
    );
    
    // Just test that we can create the proxy server
    // We can't easily test the actual server functionality without setting up a real target
    assert_eq!(proxy.listen_port(), 8080);
    assert_eq!(proxy.target_url(), "https://mock.open-rpc.org");
}

#[test]
fn test_message_channel_integration() {
    let (sender, receiver) = mpsc::unbounded_channel();
    
    // Test that we can send messages through the channel
    let test_message = JsonRpcMessage {
        id: Some(serde_json::Value::Number(serde_json::Number::from(123))),
        method: Some("test_method".to_string()),
        params: Some(serde_json::json!({"test": "value"})),
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
    
    sender.send(test_message.clone()).unwrap();
    
    // Create app with receiver
    let mut app = App::new_with_receiver(receiver);
    
    // Check for new messages
    app.check_for_new_messages();
    
    // Should have one exchange with our request
    assert_eq!(app.exchanges.len(), 1);
    let last_exchange = app.exchanges.last().unwrap();
    assert_eq!(last_exchange.method, Some("test_method".to_string()));
    assert_eq!(last_exchange.id, Some(serde_json::Value::Number(serde_json::Number::from(123))));
    assert!(last_exchange.request.is_some());
    assert!(last_exchange.response.is_none()); // No response yet
}

#[test]
fn test_app_with_receiver() {
    let (_sender, receiver) = mpsc::unbounded_channel();
    
    let app = App::new_with_receiver(receiver);
    
    // Should start empty
    assert!(app.exchanges.is_empty());
    assert_eq!(app.selected_exchange, 0);
    assert!(app.is_running);
    assert!(app.message_receiver.is_some());
}

#[test]
fn test_multiple_message_handling() {
    let (sender, receiver) = mpsc::unbounded_channel();
    let mut app = App::new_with_receiver(receiver);
    
    let initial_count = app.exchanges.len();
    
    // Send multiple request messages
    for i in 1..=5 {
        let message = JsonRpcMessage {
            id: Some(serde_json::Value::Number(serde_json::Number::from(i))),
            method: Some(format!("method_{}", i)),
            params: None,
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: MessageDirection::Request,
            transport: TransportType::Http,
            headers: None,
        };
        sender.send(message).unwrap();
    }
    
    // Check for new messages
    app.check_for_new_messages();
    
    // Should have all the new exchanges (one per request)
    assert_eq!(app.exchanges.len(), initial_count + 5);
    
    // Check the last exchange
    let last_exchange = app.exchanges.last().unwrap();
    assert_eq!(last_exchange.method, Some("method_5".to_string()));
    assert_eq!(last_exchange.id, Some(serde_json::Value::Number(serde_json::Number::from(5))));
    assert!(last_exchange.request.is_some());
    assert!(last_exchange.response.is_none()); // No responses yet
} 