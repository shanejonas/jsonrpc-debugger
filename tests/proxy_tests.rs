use jsonrpc_debugger::app::*;
use jsonrpc_debugger::proxy::ProxyServer;
use std::collections::HashMap;
use tokio::sync::mpsc;

#[tokio::test]
async fn test_proxy_server_creation() {
    let (sender, _receiver) = mpsc::unbounded_channel();

    let _proxy = ProxyServer::new(8080, "https://mock.open-rpc.org".to_string(), sender);

    // Just test that we can create the proxy server
    // We can't easily test the actual server functionality without setting up a real target
    // The constructor parameters are validated during creation
}

#[tokio::test]
async fn test_proxy_handles_different_paths() {
    use warp::Filter;

    // Create a mock target server that echoes back the path
    let mock_target = warp::path::full()
        .and(warp::post())
        .and(warp::body::json())
        .map(|path: warp::path::FullPath, body: serde_json::Value| {
            warp::reply::json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": body.get("id"),
                "result": {
                    "received_path": path.as_str(),
                    "original_request": body
                }
            }))
        });

    let cors = warp::cors()
        .allow_any_origin()
        .allow_headers(vec!["content-type"])
        .allow_methods(vec!["POST"]);

    let mock_routes = mock_target.with(cors);

    // Start mock target server on port 8061
    let mock_server = tokio::spawn(async move {
        warp::serve(mock_routes).run(([127, 0, 0, 1], 8061)).await;
    });

    // Give the mock server time to start
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Create message channel for proxy
    let (message_sender, mut message_receiver) = mpsc::unbounded_channel();

    // Create proxy pointing to mock target
    let proxy = ProxyServer::new(8071, "http://localhost:8061".to_string(), message_sender);

    // Start proxy server
    let proxy_server = tokio::spawn(async move {
        let _ = proxy.start().await;
    });

    // Give the proxy time to start
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Test different paths
    let client = reqwest::Client::new();
    let test_cases = vec!["/", "/rpc/v1", "/api/v2", "/some/nested/path"];

    for path in test_cases {
        let request_body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "test_method",
            "params": {"test": "value"},
            "id": 1
        });

        let response = client
            .post(&format!("http://localhost:8071{}", path))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await;

        assert!(response.is_ok(), "Request to {} should succeed", path);

        let response = response.unwrap();
        assert!(
            response.status().is_success(),
            "Response should be successful for path {}",
            path
        );

        let response_json: serde_json::Value = response.json().await.unwrap();

        // Verify the mock target received the correct path
        assert_eq!(
            response_json["result"]["received_path"].as_str().unwrap(),
            path,
            "Path should be forwarded correctly"
        );

        // Verify the request was logged by checking the message channel
        // We should receive both request and response messages
        let mut found_request = false;
        let mut attempts = 0;

        while !found_request && attempts < 3 {
            let received_message = tokio::time::timeout(
                std::time::Duration::from_millis(200),
                message_receiver.recv(),
            )
            .await;

            if let Ok(Some(message)) = received_message {
                if matches!(message.direction, MessageDirection::Request)
                    && message.method == Some("test_method".to_string())
                {
                    found_request = true;
                }
            }
            attempts += 1;
        }

        assert!(
            found_request,
            "Should receive a request message for path {}",
            path
        );
    }

    // Clean up
    proxy_server.abort();
    mock_server.abort();
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
    assert_eq!(
        last_exchange.id,
        Some(serde_json::Value::Number(serde_json::Number::from(123)))
    );
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
    assert_eq!(
        last_exchange.id,
        Some(serde_json::Value::Number(serde_json::Number::from(5)))
    );
    assert!(last_exchange.request.is_some());
    assert!(last_exchange.response.is_none()); // No responses yet
}
