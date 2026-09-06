//! Run with `cargo run --release --example performance --offline`.
use jsonrpc_debugger::{app::*, control, ui};
use ratatui::{backend::TestBackend, Terminal};
use std::{
    hint::black_box,
    time::{Instant, SystemTime},
};

fn message(id: usize, direction: MessageDirection) -> JsonRpcMessage {
    JsonRpcMessage {
        id: Some(id.into()),
        method: Some("example/run".into()),
        params: None,
        result: None,
        error: None,
        timestamp: SystemTime::now(),
        direction,
        transport: TransportType::Http,
        headers: None,
    }
}

fn main() {
    for count in [2_000, 4_000, 8_000] {
        let mut app = App::new();
        for id in 0..count {
            app.add_message(message(id, MessageDirection::Request));
        }
        let responses: Vec<_> = (0..count)
            .map(|id| message(id, MessageDirection::Response))
            .collect();
        let start = Instant::now();
        for response in responses {
            app.add_message(response);
        }
        println!("pair {count} queued responses: {:?}", start.elapsed());
        black_box(app);
    }
    for count in [1_000, 10_000, 100_000] {
        let mut app = App::new();
        let mut request = message(1, MessageDirection::Request);
        request.params = Some(vec!["abcdefghij"; count].into());
        app.add_message(request);
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &app)).unwrap();
        let start = Instant::now();
        for _ in 0..10 {
            terminal.draw(|frame| ui::draw(frame, &app)).unwrap();
        }
        println!(
            "draw {count} array entries: {:?} per frame",
            start.elapsed() / 10
        );
    }
    for count in [0, 100, 1000, 10_000] {
        let mut app = App::new();
        app.activate_session(
            SessionSummary {
                id: "audit".into(),
                name: "audit".into(),
                target: "test".into(),
                created_at_ms: 0,
                updated_at_ms: 0,
                exchange_count: 0,
            },
            vec![],
            vec![],
        );
        for id in 0..2000 {
            app.add_message(message(id, MessageDirection::Request));
        }
        for index in 0..count {
            app.add_annotation(LineAnnotation {
                id: format!("note-{index}"),
                parent_id: None,
                author: AnnotationAuthor::Agent,
                created_at_ms: 1,
                exchange_index: index % 2000,
                panel: Focus::RequestSection,
                tab: DetailTab::Body,
                start_line: 2,
                end_line: 2,
                message: "Inspect this method".into(),
                text: vec!["Method: example/run".into()],
            });
        }
        app.focus_next_annotation();
        let navigation = Instant::now();
        for _ in 0..1000 {
            black_box(app.focus_next_annotation());
            black_box(app.focus_previous_annotation());
        }
        println!(
            "navigate {count} annotations: {:?} per key",
            navigation.elapsed() / 2000
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &app)).unwrap();
        let start = Instant::now();
        for _ in 0..30 {
            terminal.draw(|frame| ui::draw(frame, &app)).unwrap();
        }
        println!(
            "draw {count} annotations: {:?} per frame",
            start.elapsed() / 30
        );
        let start = Instant::now();
        let mut bytes = 0;
        for _ in 0..30 {
            let update = control::updates_since(
                &app,
                Some("audit"),
                2000,
                vec![],
                Some(app.annotation_revision()),
            )
            .unwrap();
            bytes = serde_json::to_vec(&update).unwrap().len();
            black_box(update);
        }
        println!(
            "unchanged {count} annotations: {:?} per update, {bytes} bytes",
            start.elapsed() / 30
        );
    }
    for count in [100, 1000, 10_000] {
        let mut app = App::new();
        let mut request = message(0, MessageDirection::Request);
        request.params = Some(vec!["abcdefghij"; 1000].into());
        app.add_message(request);
        for index in 0..count {
            app.add_annotation(LineAnnotation {
                id: format!("dense-{index}"),
                parent_id: None,
                author: AnnotationAuthor::Agent,
                created_at_ms: 1,
                exchange_index: 0,
                panel: Focus::RequestSection,
                tab: DetailTab::Body,
                start_line: 10 + index % 900,
                end_line: 10 + index % 900,
                message: "Inspect this field".into(),
                text: vec!["abcdefghij".into()],
            });
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &app)).unwrap();
        let start = Instant::now();
        for _ in 0..30 {
            app.focus_next_annotation();
            terminal.draw(|frame| ui::draw(frame, &app)).unwrap();
        }
        println!(
            "navigate and draw {count} notes in one panel: {:?}",
            start.elapsed() / 30
        );
    }
    for count in [10_000, 100_000] {
        let mut app = App::new();
        for id in 0..count {
            app.add_message(message(id, MessageDirection::Request));
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        for filter in ["", "999"] {
            app.filter_text = filter.into();
            terminal.draw(|frame| ui::draw(frame, &app)).unwrap();
            let start = Instant::now();
            for _ in 0..30 {
                terminal.draw(|frame| ui::draw(frame, &app)).unwrap();
            }
            println!(
                "draw {count} requests, filter {filter:?}: {:?}",
                start.elapsed() / 30
            );
        }
    }
}
