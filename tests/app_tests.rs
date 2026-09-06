use jsonrpc_debugger::app::*;
use std::collections::HashMap;

#[test]
fn test_app_new_creates_empty() {
    let app = App::new();

    // Should start empty
    assert!(app.exchanges().is_empty());
    assert_eq!(app.selected_exchange, 0);
    assert!(app.is_running);
    assert_eq!(app.proxy_config.listen_port, 8080);
    assert_eq!(app.proxy_config.target_url, "");
}

#[test]
fn test_add_message() {
    let mut app = App::new();
    let initial_count = app.exchanges().len();

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

    assert_eq!(app.exchanges().len(), initial_count + 1);
    let last_exchange = app.exchanges().last().unwrap();
    assert_eq!(last_exchange.method, Some("test_method".to_string()));
    assert_eq!(
        last_exchange.id,
        Some(serde_json::Value::Number(serde_json::Number::from(999)))
    );
    assert!(last_exchange.request.is_some());
    assert!(last_exchange.response.is_none());
}

#[test]
fn idless_requests_are_notifications() {
    let mut app = App::new();
    app.add_message(JsonRpcMessage {
        id: None,
        method: Some("example_changed".to_string()),
        params: Some(serde_json::json!({})),
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::Http,
        headers: None,
    });

    assert!(app.exchanges()[0].is_notification());
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

    let exchange_count = app.exchanges().len();

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
    assert_eq!(app.exchanges().len(), 2);

    // Check first exchange is HTTP request-response pair
    let first_exchange = &app.exchanges()[0];
    assert!(first_exchange.request.is_some());
    assert!(first_exchange.response.is_some());
    assert_eq!(first_exchange.method, Some("eth_getBalance".to_string()));
    assert!(matches!(first_exchange.transport, TransportType::Http));

    // Check second exchange is WebSocket request-response pair
    let second_exchange = &app.exchanges()[1];
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
fn search_mode_cycles_and_wraps_matches() {
    let mut app = App::new();
    app.start_searching();
    app.handle_input_char('e');
    app.handle_input_char('t');
    app.handle_input_char('h');
    assert_eq!(app.input_mode, InputMode::Searching);
    assert_eq!(app.input_buffer, "eth");

    let first = SearchHit {
        exchange_index: 0,
        panel: Focus::RequestSection,
        tab: DetailTab::Body,
        start_line: 3,
        end_line: 3,
    };
    let second = SearchHit {
        exchange_index: 2,
        panel: Focus::ResponseSection,
        tab: DetailTab::Body,
        start_line: 5,
        end_line: 5,
    };
    assert_eq!(
        app.set_search_results("eth".to_string(), vec![first, second]),
        Some(first)
    );
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(app.search_active());
    assert_eq!(app.search_progress(), (1, 2));
    assert_eq!(app.next_search_hit(), Some(second));
    assert_eq!(app.next_search_hit(), Some(first));
    assert_eq!(app.previous_search_hit(), Some(second));

    app.start_searching();
    app.handle_input_char('x');
    app.cancel_search_input();
    assert_eq!(app.search_query, "eth");

    app.clear_search();
    assert!(!app.search_active());
    assert_eq!(app.search_progress(), (0, 0));
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
fn hidden_request_list_is_not_focusable() {
    let mut app = App::new();
    app.focus = Focus::MessageList;

    app.toggle_request_list();

    assert!(!app.request_list_visible);
    assert_eq!(app.focus, Focus::RequestSection);

    app.switch_focus_reverse();
    assert_eq!(app.focus, Focus::StatusHeader);
    app.switch_focus();
    assert_eq!(app.focus, Focus::RequestSection);

    app.toggle_request_list();
    app.set_focus(Focus::StatusHeader);
    app.switch_focus();
    assert_eq!(app.focus, Focus::MessageList);
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
fn batch_new_requests_are_validated_before_background_send() {
    let mut app = App::new();
    app.proxy_config.target_url = "http://localhost:8090".to_string();
    let body = r#"[
        {"jsonrpc":"2.0","method":"example_first","params":[],"id":1},
        {"jsonrpc":"2.0","method":"example_second","params":[],"id":2}
    ]"#
    .to_string();

    let request = app.prepare_new_request(body.clone()).unwrap();

    assert_eq!(request.url, "http://localhost:8080");
    assert_eq!(request.body, body);
    assert_eq!(
        app.prepare_new_request("[]".to_string()).unwrap_err(),
        "Batch request cannot be empty"
    );
    assert_eq!(
        app.prepare_new_request(
            r#"[{"jsonrpc":"2.0","method":"example_first"},{"jsonrpc":"2.0"}]"#.to_string()
        )
        .unwrap_err(),
        "Batch item 2: Missing 'method' field"
    );
}

#[test]
fn stdio_accepts_responses_to_server_requests() {
    let mut app = App::new();
    app.proxy_config.target_url = "example-server".to_string();
    app.proxy_config.transport = TransportType::Stdio(Framing::JsonLines);
    app.proxy_config.stdio = Some(StdioConfig {
        command: vec!["example-server".into()],
        framing: Framing::JsonLines,
        request_timeout: std::time::Duration::from_secs(120),
    });

    assert!(app
        .prepare_new_request(r#"{"jsonrpc":"2.0","id":9,"result":{}}"#.to_string())
        .is_ok());
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
fn activating_a_session_clears_the_previous_filter() {
    let mut app = App::new();
    app.filter_text = "old-filter".to_string();

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

    assert!(app.filter_text.is_empty());
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
fn editing_an_annotation_prefills_the_shared_prompt() {
    let mut app = App::new();
    app.focus = Focus::RequestSection;
    app.add_annotation(LineAnnotation {
        parent_id: None,
        author: jsonrpc_debugger::app::AnnotationAuthor::Unknown,
        created_at_ms: 0,
        id: "note".to_string(),
        exchange_index: 0,
        panel: Focus::RequestSection,
        tab: DetailTab::Body,
        start_line: 2,
        end_line: 3,
        message: "Original note".to_string(),
        text: vec!["two".to_string(), "three".to_string()],
    });

    assert!(app.start_editing_annotation("note"));
    assert_eq!(app.input_mode, InputMode::AnnotatingSelection);
    assert_eq!(app.input_buffer, "Original note");
    assert_eq!(app.annotation_edit_id.as_deref(), Some("note"));
    assert_eq!(app.line_selection.as_ref().unwrap().end_line, 3);
    assert!(!app.visual_selection_active);

    app.cancel_editing();
    assert!(app.annotation_edit_id.is_none());
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
        parent_id: None,
        author: jsonrpc_debugger::app::AnnotationAuthor::Unknown,
        created_at_ms: 0,
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
fn annotation_navigation_is_global_and_selects_the_target_tab() {
    let annotation = |id: &str, panel: Focus, tab: DetailTab, exchange, line| LineAnnotation {
        parent_id: None,
        author: jsonrpc_debugger::app::AnnotationAuthor::Unknown,
        created_at_ms: 0,
        id: id.to_string(),
        exchange_index: exchange,
        panel,
        tab,
        start_line: line,
        end_line: line,
        message: id.to_string(),
        text: vec![id.to_string()],
    };
    let mut app = App::new();
    for id in 0..2 {
        app.add_message(JsonRpcMessage {
            id: Some(serde_json::json!(id)),
            method: Some(format!("method_{id}")),
            params: None,
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: MessageDirection::Request,
            transport: TransportType::Http,
            headers: None,
        });
    }
    app.select_exchange(0);
    app.focus = Focus::RequestSection;
    app.annotations = vec![
        annotation("request-5", Focus::RequestSection, DetailTab::Body, 0, 5),
        annotation("response-1", Focus::ResponseSection, DetailTab::Body, 0, 1),
        annotation("request-2a", Focus::RequestSection, DetailTab::Body, 0, 2),
        annotation("request-2b", Focus::RequestSection, DetailTab::Body, 0, 2),
        annotation("headers-1", Focus::RequestSection, DetailTab::Headers, 0, 1),
        annotation("exchange-1", Focus::RequestSection, DetailTab::Body, 1, 1),
    ];

    for id in ["request-2a", "request-2b", "request-5", "response-1"] {
        assert!(app.focus_next_annotation());
        assert_eq!(app.active_annotation_id.as_deref(), Some(id));
        assert!(app.line_selection.is_none());
    }
    assert_eq!(app.focus, Focus::ResponseSection);
    assert_eq!(app.response_tab, 1);

    assert!(app.focus_next_annotation());
    assert_eq!(app.active_annotation_id.as_deref(), Some("exchange-1"));
    assert_eq!(app.selected_exchange, 1);
    assert_eq!(app.focus, Focus::RequestSection);

    assert!(app.focus_next_annotation());
    assert_eq!(app.active_annotation_id.as_deref(), Some("headers-1"));
    assert_eq!(app.selected_exchange, 0);
    assert_eq!(app.request_tab, 0);

    assert!(app.focus_next_annotation());
    assert_eq!(app.active_annotation_id.as_deref(), Some("request-2a"));
    assert_eq!(app.request_tab, 1);

    assert!(app.focus_previous_annotation());
    assert_eq!(app.active_annotation_id.as_deref(), Some("headers-1"));
    assert_eq!(app.request_tab, 0);

    assert!(app.focus_previous_annotation());
    assert_eq!(app.active_annotation_id.as_deref(), Some("exchange-1"));
    assert_eq!(app.selected_exchange, 1);

    assert!(app.focus_previous_annotation());
    assert_eq!(app.active_annotation_id.as_deref(), Some("response-1"));
    assert_eq!(app.selected_exchange, 0);
    assert_eq!(app.focus, Focus::ResponseSection);

    assert!(app.focus_previous_annotation());
    assert_eq!(app.active_annotation_id.as_deref(), Some("request-5"));
    assert_eq!(app.focus, Focus::RequestSection);
}

#[test]
fn annotation_navigation_starts_from_the_selected_exchange_outside_details() {
    let mut app = App::new();
    for id in 0..2 {
        app.add_message(JsonRpcMessage {
            id: Some(serde_json::json!(id)),
            method: Some(format!("method_{id}")),
            params: None,
            result: None,
            error: None,
            timestamp: std::time::SystemTime::now(),
            direction: MessageDirection::Request,
            transport: TransportType::Http,
            headers: None,
        });
    }
    app.annotations = vec![
        LineAnnotation {
            parent_id: None,
            author: jsonrpc_debugger::app::AnnotationAuthor::Unknown,
            created_at_ms: 0,
            id: "request".to_string(),
            exchange_index: 0,
            panel: Focus::RequestSection,
            tab: DetailTab::Body,
            start_line: 2,
            end_line: 2,
            message: "request".to_string(),
            text: vec!["request".to_string()],
        },
        LineAnnotation {
            parent_id: None,
            author: jsonrpc_debugger::app::AnnotationAuthor::Unknown,
            created_at_ms: 0,
            id: "response".to_string(),
            exchange_index: 0,
            panel: Focus::ResponseSection,
            tab: DetailTab::Body,
            start_line: 2,
            end_line: 2,
            message: "response".to_string(),
            text: vec!["response".to_string()],
        },
        LineAnnotation {
            parent_id: None,
            author: jsonrpc_debugger::app::AnnotationAuthor::Unknown,
            created_at_ms: 0,
            id: "next-exchange".to_string(),
            exchange_index: 1,
            panel: Focus::RequestSection,
            tab: DetailTab::Headers,
            start_line: 1,
            end_line: 1,
            message: "next".to_string(),
            text: vec!["next".to_string()],
        },
    ];

    app.select_exchange(0);
    app.set_focus(Focus::MessageList);
    assert!(app.focus_next_annotation());
    assert_eq!(app.active_annotation_id.as_deref(), Some("request"));
    assert_eq!(app.focus, Focus::RequestSection);

    app.select_exchange(0);
    app.set_focus(Focus::MessageList);
    assert!(app.focus_previous_annotation());
    assert_eq!(app.active_annotation_id.as_deref(), Some("response"));
    assert_eq!(app.focus, Focus::ResponseSection);

    app.select_exchange(1);
    app.set_focus(Focus::StatusHeader);
    assert!(app.focus_next_annotation());
    assert_eq!(app.active_annotation_id.as_deref(), Some("next-exchange"));
    assert_eq!(app.selected_exchange, 1);
    assert_eq!(app.focus, Focus::RequestSection);
    assert_eq!(app.request_tab, 0);

    assert!(app.focus_next_annotation());
    assert_eq!(app.active_annotation_id.as_deref(), Some("request"));
    assert_eq!(app.selected_exchange, 0);
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
        original_body: serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_chainId",
            "params": []
        }),
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

fn pairing_message(id: Option<serde_json::Value>, direction: MessageDirection) -> JsonRpcMessage {
    JsonRpcMessage {
        id,
        method: (direction == MessageDirection::Request).then(|| "example/run".to_string()),
        params: None,
        result: (direction == MessageDirection::Response).then(|| serde_json::json!("ok")),
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction,
        transport: TransportType::Http,
        headers: None,
    }
}

#[test]
fn pairs_duplicate_ids_newest_first_and_distinguishes_id_types() {
    let mut app = App::new();
    for id in [
        serde_json::json!(7),
        serde_json::json!("7"),
        serde_json::json!(7),
    ] {
        app.add_message(pairing_message(Some(id), MessageDirection::Request));
    }
    app.add_message(pairing_message(
        Some(serde_json::json!(7)),
        MessageDirection::Response,
    ));
    assert!(app.exchanges()[2].response.is_some());
    assert!(app.exchanges()[0].response.is_none());
    app.add_message(pairing_message(
        Some(serde_json::json!(7)),
        MessageDirection::Response,
    ));
    assert!(app.exchanges()[0].response.is_some());
    assert!(app.exchanges()[1].response.is_none());
    app.add_message(pairing_message(
        Some(serde_json::json!("7")),
        MessageDirection::Response,
    ));
    assert!(app.exchanges()[1].response.is_some());
    assert_eq!(app.pending_exchange_indices().count(), 0);
}

#[test]
fn response_index_survives_import_and_session_changes() {
    let mut source = App::new();
    source.add_message(pairing_message(
        Some(serde_json::json!(1)),
        MessageDirection::Request,
    ));
    let mut app = App::new();
    app.append_exchanges(source.exchanges().to_vec());
    app.add_message(pairing_message(
        Some(serde_json::json!(1)),
        MessageDirection::Response,
    ));
    assert_eq!(app.exchanges().len(), 1);
    assert!(app.exchanges()[0].response.is_some());

    source.add_message(pairing_message(
        Some(serde_json::json!(2)),
        MessageDirection::Request,
    ));
    app.activate_session(
        SessionSummary {
            id: "new".into(),
            name: "New".into(),
            target: "test".into(),
            created_at_ms: 0,
            updated_at_ms: 0,
            exchange_count: 2,
        },
        source.exchanges().to_vec(),
        Vec::new(),
    );
    app.add_message(pairing_message(
        Some(serde_json::json!(2)),
        MessageDirection::Response,
    ));
    assert!(app.exchanges()[1].response.is_some());
    app.add_message(pairing_message(None, MessageDirection::Request));
    app.add_message(pairing_message(None, MessageDirection::Response));
    assert!(app.exchanges()[2].is_notification());
    assert!(app.exchanges()[2].response.is_none());
    assert!(app.exchanges()[3].request.is_none());
}

#[test]
fn persisted_threads_remain_intact_while_navigation_follows_the_flat_list() {
    let mut app = App::new();
    app.add_message(JsonRpcMessage {
        id: Some(1.into()),
        method: Some("example/run".to_string()),
        params: None,
        result: None,
        error: None,
        timestamp: std::time::SystemTime::now(),
        direction: MessageDirection::Request,
        transport: TransportType::Http,
        headers: None,
    });
    let root = LineAnnotation {
        id: "root".to_string(),
        parent_id: None,
        author: AnnotationAuthor::Agent,
        created_at_ms: 1,
        exchange_index: 0,
        panel: Focus::RequestSection,
        tab: DetailTab::Body,
        start_line: 2,
        end_line: 2,
        message: "Check this".to_string(),
        text: vec![],
    };
    // Replies arrive after a second root but remain beneath their own parent.
    for (id, parent, time) in [
        ("other", None, 2),
        ("sibling", Some("root"), 5),
        ("root", None, 1),
        ("nested", Some("reply"), 4),
        ("reply", Some("root"), 3),
    ] {
        app.add_annotation(LineAnnotation {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            created_at_ms: time,
            ..root.clone()
        });
    }
    let order = app
        .annotation_threads()
        .into_iter()
        .map(|(note, depth)| (note.id.as_str(), depth))
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        [
            ("root", 0),
            ("reply", 1),
            ("nested", 2),
            ("sibling", 1),
            ("other", 0)
        ]
    );
    app.focus_annotation("root");
    for id in ["other", "reply", "nested", "sibling", "root"] {
        assert!(app.focus_next_annotation());
        assert_eq!(app.active_annotation_id.as_deref(), Some(id));
    }
    app.remove_annotation("reply");
    assert_eq!(
        app.annotations
            .iter()
            .find(|note| note.id == "nested")
            .unwrap()
            .parent_id
            .as_deref(),
        Some("root")
    );
    app.remove_annotation("root");
    assert!(app.annotations.iter().all(|note| note.parent_id.is_none()));
}
