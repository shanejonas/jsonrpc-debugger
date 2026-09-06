//! Run with `cargo run --release --example performance --offline`.
use jsonrpc_debugger::{app::*, ui};
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
}
