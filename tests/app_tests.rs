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
fn panel_fullscreen_is_an_idempotent_view_state() {
    let mut app = App::new();
    let revision = app.revision();

    app.set_panel_fullscreen(true);
    assert!(app.panel_fullscreen);
    assert_eq!(app.revision(), revision + 1);

    app.set_panel_fullscreen(true);
    assert_eq!(app.revision(), revision + 1);

    app.set_panel_fullscreen(false);
    assert!(!app.panel_fullscreen);
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

#[test]
fn filters_requests_by_their_visible_id() {
    let mut app = App::new();
    for (id, method) in [
        (serde_json::json!("audit-ab12"), "first_method"),
        (serde_json::json!(73), "second_method"),
    ] {
        app.add_message(JsonRpcMessage {
            id: Some(id),
            method: Some(method.to_string()),
            params: Some(serde_json::json!([])),
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: MessageDirection::Request,
            transport: TransportType::Http,
            headers: None,
        });
    }

    app.filter_text = "ab12".to_string();
    assert_eq!(app.filtered_exchange_indices(), vec![0]);

    app.filter_text = "73".to_string();
    assert_eq!(app.filtered_exchange_indices(), vec![1]);
}

#[test]
fn focused_request_list_copies_as_markdown_table() {
    let mut app = App::new();
    app.add_message(JsonRpcMessage {
        id: Some(serde_json::json!(1)),
        method: Some("eth|call".to_string()),
        params: Some(serde_json::json!([])),
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::Http,
        headers: None,
    });

    assert_eq!(
        app.focused_markdown().unwrap(),
        "| Status | Transport | Method | ID | Duration |\n\
         | --- | --- | --- | --- | --- |\n\
         | Pending | HTTP | eth\\|call | 1 | - |"
    );
}

#[test]
fn focused_details_copy_the_selected_tab_as_markdown() {
    let mut app = App::new();
    app.add_message(JsonRpcMessage {
        id: Some(serde_json::json!(1)),
        method: Some("eth_call".to_string()),
        params: Some(serde_json::json!([{"to": "0x123"}])),
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::Http,
        headers: Some(HashMap::from([(
            "Content-Type".to_string(),
            "application/json".to_string(),
        )])),
    });

    app.focus = Focus::RequestSection;
    let body = app.focused_markdown().unwrap();
    assert!(body.starts_with("# Request\n"));
    assert!(body.contains("## Body\n\n```json"));
    assert!(body.contains("\"method\": \"eth_call\""));

    app.request_tab = 0;
    let headers = app.focused_markdown().unwrap();
    assert!(headers.contains("| Header | Value |"));
    assert!(headers.contains("| Content-Type | application/json |"));

    app.focus = Focus::ResponseSection;
    assert!(app.focused_markdown().is_none());
}

#[test]
fn focused_detail_selection_copies_only_the_selected_lines() {
    let mut app = App::new();
    app.focus = Focus::RequestSection;
    app.select_lines(
        Focus::RequestSection,
        2,
        3,
        vec!["one".to_string(), "two".to_string()],
    );

    assert_eq!(app.focused_markdown().unwrap(), "```text\none\ntwo\n```");
    assert_eq!(app.request_details_cursor_line, 3);
}

#[test]
fn visual_selection_copy_takes_priority_after_hover_changes_focus() {
    let mut app = App::new();
    app.focus = Focus::RequestSection;
    app.select_lines(
        Focus::RequestSection,
        2,
        3,
        vec!["one".to_string(), "two".to_string()],
    );
    app.start_visual_selection();
    app.focus = Focus::MessageList;

    assert_eq!(app.focused_markdown().unwrap(), "```text\none\ntwo\n```");
}

#[test]
fn detail_cursor_and_scroll_survive_focus_changes() {
    let mut app = App::new();
    app.focus = Focus::RequestSection;
    app.request_details_cursor_line = 7;
    app.request_details_scroll = 4;

    app.switch_focus();
    app.switch_focus_reverse();

    assert_eq!(app.focus, Focus::RequestSection);
    assert_eq!(app.request_details_cursor_line, 7);
    assert_eq!(app.request_details_scroll, 4);
}

#[test]
fn inline_editor_edits_multiline_unicode_text() {
    let mut editor = TextEditor::new(EditorTarget::NewRequest, "aé\ncd".to_string());

    editor.move_right();
    editor.insert('X');
    assert_eq!(editor.content(), "aXé\ncd");

    editor.newline();
    assert_eq!(editor.content(), "aX\né\ncd");

    editor.backspace();
    assert_eq!(editor.content(), "aXé\ncd");

    editor.move_to_end();
    editor.delete();
    assert_eq!(editor.content(), "aXécd");
}

#[test]
fn inline_editor_word_motions_are_unicode_and_punctuation_aware() {
    let mut editor = TextEditor::new(EditorTarget::NewRequest, "éclair 東京".to_string());

    editor.move_word_forward();
    assert_eq!((editor.row, editor.column), (0, 7));

    editor.move_word_backward();
    assert_eq!((editor.row, editor.column), (0, 0));

    let mut editor = TextEditor::new(EditorTarget::NewRequest, r#"{"key": 1}"#.to_string());
    editor.move_word_forward();
    assert_eq!((editor.row, editor.column), (0, 2));

    let editor = TextEditor::new(EditorTarget::NewRequest, "value\n".to_string());
    assert_eq!(editor.content(), "value\n");
}

#[test]
fn new_requests_are_validated_before_background_send() {
    let mut app = App::new();
    app.proxy_config.target_url = "http://localhost:8090".to_string();
    let body = r#"{"jsonrpc":"2.0","method":"eth_chainId","id":1}"#.to_string();

    let request = app.prepare_new_request(body.clone()).unwrap();
    assert_eq!(request.url, "http://localhost:8080");
    assert_eq!(request.body, body);

    app.app_mode = AppMode::Paused;
    let request = app
        .prepare_new_request(r#"{"jsonrpc":"2.0","method":"eth_chainId","id":1}"#.to_string())
        .unwrap();
    assert_eq!(request.url, "http://localhost:8090");

    assert!(app.prepare_new_request("{}".to_string()).is_err());
}

#[test]
fn stopped_proxy_refuses_to_send_through_its_port() {
    let mut app = App::new();
    app.is_running = false;

    let error = app
        .prepare_new_request(r#"{"jsonrpc":"2.0","method":"eth_chainId","id":1}"#.to_string())
        .unwrap_err();

    assert_eq!(error, "Proxy is stopped. Press Ctrl-B x to start it.");
}

#[test]
fn active_session_tracks_new_exchanges() {
    let mut app = App::new();
    app.activate_session(
        SessionSummary {
            id: "session".to_string(),
            name: "Session".to_string(),
            target: "http://node".to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
            exchange_count: 0,
        },
        Vec::new(),
        Vec::new(),
    );

    app.add_message(JsonRpcMessage {
        id: Some(serde_json::json!(1)),
        method: Some("eth_chainId".to_string()),
        params: Some(serde_json::json!([])),
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::Http,
        headers: None,
    });

    assert_eq!(app.session.unwrap().exchange_count, 1);
}

#[test]
fn session_name_prompts_use_the_shared_input_buffer() {
    let mut app = App::new();
    app.activate_session(
        SessionSummary {
            id: "session".to_string(),
            name: "Original".to_string(),
            target: "http://node".to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
            exchange_count: 0,
        },
        Vec::new(),
        Vec::new(),
    );

    app.start_naming_session();
    app.handle_input_char('N');
    assert_eq!(app.input_mode, InputMode::NamingSession);
    assert_eq!(app.input_buffer, "N");

    app.start_renaming_session();
    assert_eq!(app.input_mode, InputMode::RenamingSession);
    assert_eq!(app.input_buffer, "Original");

    app.rename_session("session", "Refunds".to_string());
    assert_eq!(app.session.unwrap().name, "Refunds");
}

#[test]
fn annotation_prompt_requires_an_active_visual_selection() {
    let mut app = App::new();
    app.select_lines(
        Focus::RequestSection,
        2,
        3,
        vec!["two".to_string(), "three".to_string()],
    );

    app.start_annotating_selection();
    assert_eq!(app.input_mode, InputMode::Normal);

    app.start_visual_selection();
    app.start_annotating_selection();
    app.handle_input_char('N');

    assert_eq!(app.input_mode, InputMode::AnnotatingSelection);
    assert_eq!(app.input_buffer, "N");
}

#[test]
fn adding_an_annotation_preserves_the_viewport() {
    let mut app = App::new();
    app.selected_exchange = 7;
    app.focus = Focus::MessageList;
    app.request_tab = 0;
    app.response_tab = 1;
    app.request_details_scroll = 3;
    app.response_details_scroll = 9;
    app.line_selection = Some(LineSelection {
        panel: Focus::RequestSection,
        anchor_line: 2,
        start_line: 2,
        end_line: 4,
        text: vec!["selected".to_string()],
    });
    app.active_annotation_id = Some("existing".to_string());

    let selection = app.line_selection.clone();
    app.add_annotation(LineAnnotation {
        id: "new".to_string(),
        exchange_index: 12,
        panel: Focus::ResponseSection,
        tab: DetailTab::Body,
        start_line: 20,
        end_line: 22,
        message: "Background finding".to_string(),
        text: vec!["evidence".to_string()],
    });

    assert_eq!(app.annotations.len(), 1);
    assert_eq!(app.selected_exchange, 7);
    assert_eq!(app.focus, Focus::MessageList);
    assert_eq!((app.request_tab, app.response_tab), (0, 1));
    assert_eq!(
        (app.request_details_scroll, app.response_details_scroll),
        (3, 9)
    );
    assert_eq!(app.line_selection, selection);
    assert_eq!(app.active_annotation_id.as_deref(), Some("existing"));
}

#[test]
fn unpausing_with_pending_requests_keeps_them_visible() {
    let mut app = App::new();
    let (decision_sender, _decision_receiver) = tokio::sync::oneshot::channel();
    app.app_mode = AppMode::Paused;
    app.pending_requests.push(PendingRequest {
        id: "pending".to_string(),
        original_request: JsonRpcMessage {
            id: Some(serde_json::json!(1)),
            method: Some("eth_chainId".to_string()),
            params: Some(serde_json::json!([])),
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: MessageDirection::Request,
            transport: TransportType::Http,
            headers: None,
        },
        modified_request: None,
        modified_headers: None,
        decision_sender,
    });

    app.toggle_pause_mode();
    assert_eq!(app.app_mode, AppMode::Intercepting);
    assert_eq!(app.pending_requests.len(), 1);

    app.toggle_pause_mode();
    assert_eq!(app.app_mode, AppMode::Paused);
}
