use jsonrpc_proxy_tui::proxy::ProxyServer;
use jsonrpc_proxy_tui::app::*;
use tokio::sync::mpsc;
use std::collections::HashMap;

#[tokio::test]
async fn test_proxy_server_creation() {
    let (sender, _receiver) = mpsc::unbounded_channel();
    
    let proxy = ProxyServer::new(
        8080,
        "https://eth-mainnet.g.alchemy.com/v2/demo".to_string(),
        sender,
    );
    
    // Just test that we can create the proxy server
    // We can't easily test the actual server functionality without setting up a real target
    assert_eq!(proxy.listen_port(), 8080);
    assert_eq!(proxy.target_url(), "https://eth-mainnet.g.alchemy.com/v2/demo");
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
    
    // Should have the original sample messages plus our new one
    let last_message = app.messages.last().unwrap();
    assert_eq!(last_message.method, Some("test_method".to_string()));
    assert_eq!(last_message.id, Some(serde_json::Value::Number(serde_json::Number::from(123))));
}

#[test]
fn test_app_with_receiver() {
    let (_sender, receiver) = mpsc::unbounded_channel();
    
    let app = App::new_with_receiver(receiver);
    
    // Should start empty
    assert!(app.messages.is_empty());
    assert_eq!(app.selected_message, 0);
    assert!(!app.is_running);
    assert!(app.message_receiver.is_some());
}

#[test]
fn test_multiple_message_handling() {
    let (sender, receiver) = mpsc::unbounded_channel();
    let mut app = App::new_with_receiver(receiver);
    
    let initial_count = app.messages.len();
    
    // Send multiple messages
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
    
    // Should have all the new messages
    assert_eq!(app.messages.len(), initial_count + 5);
    
    // Check the last message
    let last_message = app.messages.last().unwrap();
    assert_eq!(last_message.method, Some("method_5".to_string()));
    assert_eq!(last_message.id, Some(serde_json::Value::Number(serde_json::Number::from(5))));
} 