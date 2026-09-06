//! Run with `cargo run --release --example memory --offline` on Linux to inspect RSS.
use jsonrpc_debugger::app::*;
use std::time::SystemTime;

fn main() {
    let mut app = App::new();
    for id in 0..2048 {
        app.add_message(JsonRpcMessage {
            id: Some(id.into()),
            method: Some("example/large".into()),
            params: Some(serde_json::json!({"blob": "x".repeat(64 * 1024)})),
            result: None,
            error: None,
            timestamp: SystemTime::now(),
            direction: MessageDirection::Request,
            transport: TransportType::Http,
            headers: None,
        });
        if [511, 1023, 2047].contains(&id) {
            let status =
                std::fs::read_to_string("/proc/self/status").expect("Linux process status");
            println!(
                "{} exchanges, {}",
                id + 1,
                status
                    .lines()
                    .find(|line| line.starts_with("VmRSS:"))
                    .unwrap()
            );
        }
    }
    std::hint::black_box(app);
}
