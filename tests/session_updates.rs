use jsonrpc_debugger::{
    app::{App, Framing, JsonRpcMessage, MessageDirection, SessionSummary, TransportType},
    attach::Snapshot,
    control,
};
use serde_json::{json, Value};
use std::time::SystemTime;

fn session(id: &str) -> SessionSummary {
    SessionSummary {
        id: id.into(),
        name: id.into(),
        target: "test".into(),
        created_at_ms: 0,
        updated_at_ms: 0,
        exchange_count: 0,
    }
}

fn message(id: usize, direction: MessageDirection) -> JsonRpcMessage {
    JsonRpcMessage {
        id: Some(id.into()),
        method: (direction == MessageDirection::Request).then(|| "example/run".into()),
        params: None,
        result: (direction == MessageDirection::Response).then(|| json!("ok")),
        error: None,
        timestamp: SystemTime::now(),
        direction,
        transport: TransportType::Stdio(Framing::JsonLines),
        headers: None,
    }
}

fn source() -> App {
    let mut app = App::new();
    app.proxy_config.transparent = true;
    app.proxy_config.transport = TransportType::Stdio(Framing::JsonLines);
    app.activate_session(session("one"), Vec::new(), Vec::new());
    app
}

fn updates(source: &App, attached: &App) -> Value {
    control::updates(
        source,
        attached.session.as_ref().map(|s| s.id.as_str()),
        attached.exchanges().len(),
        attached.pending_exchange_indices().collect(),
    )
    .unwrap()
}

fn apply(value: Value, app: &mut App) {
    serde_json::from_value::<Snapshot>(value)
        .unwrap()
        .apply(app)
        .unwrap();
}

#[test]
fn incremental_attach_completes_old_requests_without_resending_history_or_moving_view() {
    let mut source = source();
    for id in 0..100 {
        source.add_message(message(id, MessageDirection::Request));
        if id != 0 {
            source.add_message(message(id, MessageDirection::Response));
        }
    }
    let mut attached = App::new();
    apply(updates(&source, &attached), &mut attached);
    attached.select_exchange(50);
    attached.history_scroll = Some(20);
    attached.response_details_scroll = 3;
    assert_eq!(attached.pending_exchange_indices().collect::<Vec<_>>(), [0]);
    assert!(updates(&source, &attached)["exchanges"]
        .as_array()
        .unwrap()
        .is_empty());

    source.add_message(message(0, MessageDirection::Response));
    source.add_message(message(100, MessageDirection::Request));
    let delta = updates(&source, &attached);
    assert_eq!(delta["reset"], false);
    assert_eq!(delta["exchanges"].as_array().unwrap().len(), 2);
    assert_eq!(delta["exchanges"][0]["index"], 0);
    assert_eq!(delta["exchanges"][1]["index"], 100);
    apply(delta, &mut attached);
    assert!(attached
        .exchanges()
        .get(0)
        .unwrap()
        .unwrap()
        .response
        .is_some());
    assert_eq!(attached.exchanges().len(), 101);
    assert_eq!(
        attached.pending_exchange_indices().collect::<Vec<_>>(),
        [100]
    );
    assert_eq!(attached.selected_exchange, 50);
    assert_eq!(attached.history_scroll, Some(20));
    assert_eq!(attached.response_details_scroll, 3);
    assert!(updates(&source, &attached)["exchanges"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn attach_resets_when_the_session_changes_or_the_cursor_is_stale() {
    let mut source = source();
    source.add_message(message(1, MessageDirection::Request));
    let mut attached = App::new();
    apply(updates(&source, &attached), &mut attached);
    source.activate_session(session("two"), Vec::new(), Vec::new());
    source.add_message(message(2, MessageDirection::Request));
    let delta = updates(&source, &attached);
    assert_eq!(delta["reset"], true);
    apply(delta, &mut attached);
    assert_eq!(attached.session.as_ref().unwrap().id, "two");
    assert_eq!(
        attached.exchanges().get(0).unwrap().unwrap().id,
        Some(json!(2))
    );
    let reset = control::updates(&source, Some("two"), 999, vec![998]).unwrap();
    assert_eq!(reset["reset"], true);
    apply(reset, &mut attached);
    assert_eq!(attached.exchanges().len(), 1);
}

#[test]
fn malformed_updates_leave_the_attached_state_intact() {
    let mut source = source();
    source.add_message(message(1, MessageDirection::Request));
    let mut attached = App::new();
    apply(updates(&source, &attached), &mut attached);
    source.add_message(message(2, MessageDirection::Request));
    let mut delta = updates(&source, &attached);
    delta["exchanges"][0]["index"] = json!(3);
    assert!(serde_json::from_value::<Snapshot>(delta)
        .unwrap()
        .apply(&mut attached)
        .is_err());
    assert_eq!(attached.exchanges().len(), 1);
    assert_eq!(attached.session.as_ref().unwrap().exchange_count, 1);
    assert!(control::updates(&source, Some("one"), 1, vec![1]).is_err());
}

#[test]
fn attached_annotations_sync_replies_edits_deletions_and_preserve_drafts() {
    use jsonrpc_debugger::app::{AnnotationAuthor, DetailTab, Focus, LineAnnotation};
    let mut source = source();
    source.add_message(message(1, MessageDirection::Request));
    let root = LineAnnotation {
        id: "root".to_string(),
        parent_id: None,
        author: AnnotationAuthor::Agent,
        created_at_ms: 123,
        exchange_index: 0,
        panel: Focus::RequestSection,
        tab: DetailTab::Body,
        start_line: 2,
        end_line: 2,
        message: "Check this".to_string(),
        text: vec!["Method: example/run".to_string()],
    };
    source.add_annotation(root.clone());
    let mut attached = App::new();
    apply(updates(&source, &attached), &mut attached);
    assert_eq!(attached.annotations(), vec![root.clone()]);
    source.add_annotation(LineAnnotation {
        id: "reply".to_string(),
        parent_id: Some(root.id.clone()),
        author: AnnotationAuthor::User,
        created_at_ms: 124,
        ..root
    });
    attached.request_details_scroll = 4;
    apply(updates(&source, &attached), &mut attached);
    assert_eq!(attached.annotations(), source.annotations());
    assert_eq!(attached.request_details_scroll, 4);
    attached.start_editing_annotation("reply");
    attached.input_buffer = "Unsaved local draft".to_string();
    source.update_annotation("reply", "Remote edit".to_string());
    apply(updates(&source, &attached), &mut attached);
    assert_eq!(attached.annotations()[1].message, "Remote edit");
    assert_eq!(attached.input_buffer, "Unsaved local draft");
    assert_eq!(attached.annotation_edit_id.as_deref(), Some("reply"));
    source.remove_annotation("root");
    apply(updates(&source, &attached), &mut attached);
    assert_eq!(attached.annotations().len(), 1);
    assert_eq!(attached.annotations()[0].parent_id, None);
    assert_eq!(attached.annotations()[0].author, AnnotationAuthor::User);
    source.remove_annotation("reply");
    apply(updates(&source, &attached), &mut attached);
    assert!(attached.annotations().is_empty());
    assert_eq!(attached.active_annotation_id, None);
    assert_eq!(attached.annotation_edit_id.as_deref(), Some("reply"));
    assert_eq!(attached.input_buffer, "Unsaved local draft");
}

#[test]
fn unchanged_annotations_are_omitted_and_attach_preserves_them() {
    use jsonrpc_debugger::app::{AnnotationAuthor, DetailTab, Focus, LineAnnotation};
    let mut source = source();
    source.add_message(message(1, MessageDirection::Request));
    source.add_annotation(LineAnnotation {
        id: "note".into(),
        parent_id: None,
        author: AnnotationAuthor::User,
        created_at_ms: 1,
        exchange_index: 0,
        panel: Focus::RequestSection,
        tab: DetailTab::Body,
        start_line: 2,
        end_line: 2,
        message: "Keep this note".into(),
        text: vec!["id".into()],
    });
    let mut attached = App::new();
    apply(updates(&source, &attached), &mut attached);
    let revision = source.annotation_revision();
    source.add_message(message(1, MessageDirection::Response));
    let delta = control::updates_since(&source, Some("one"), 1, vec![0], Some(revision)).unwrap();
    assert!(delta.get("annotations").is_none());
    assert!(delta["state"].get("annotations").is_none());
    apply(delta, &mut attached);
    assert_eq!(attached.annotations(), source.annotations());
    assert!(attached
        .exchanges()
        .get(0)
        .unwrap()
        .unwrap()
        .response
        .is_some());

    source.update_annotation("note", "Updated".into());
    let edit = control::updates_since(&source, Some("one"), 1, vec![], Some(revision)).unwrap();
    assert_eq!(edit["annotations"][0]["message"], "Updated");
    apply(edit, &mut attached);
    assert_eq!(attached.annotations(), source.annotations());
    let revision = source.annotation_revision();
    source.remove_annotation("note");
    let delete = control::updates_since(&source, Some("one"), 1, vec![], Some(revision)).unwrap();
    assert_eq!(delete["annotations"], json!([]));
    apply(delete, &mut attached);
    assert!(attached.annotations().is_empty());
    // An explicit session reset always supplies a full annotation list, even with an equal revision.
    let reset =
        control::updates_since(&source, None, 0, vec![], Some(source.annotation_revision()))
            .unwrap();
    assert_eq!(reset["annotations"], json!([]));
    assert!(control::state_with_annotations(&source, false)
        .get("annotations")
        .is_none());
    assert!(control::state(&source).get("annotations").is_some());
}
