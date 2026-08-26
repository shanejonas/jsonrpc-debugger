#[cfg(unix)]
mod unix {
    use jsonrpc_debugger::{app::App, attach::ControlClient};
    use serde_json::{json, Value};
    use std::{process::Stdio, time::Duration};
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        process::Command,
    };
    use uuid::Uuid;

    #[tokio::test]
    async fn wrap_relays_a_real_child_and_records_history() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let control_port = listener.local_addr().unwrap().port();
        drop(listener);
        let config_dir =
            std::env::temp_dir().join(format!("jsonrpc-debugger-wrap-test-{}", Uuid::new_v4()));
        let script = concat!(
            "while IFS= read -r line; do ",
            "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"wrap-1\",\"result\":\"ok\"}'; ",
            "done"
        );
        let mut wrapper = Command::new(env!("CARGO_BIN_EXE_jsonrpc-debugger"))
            .args([
                "--control-port",
                &control_port.to_string(),
                "wrap",
                "--",
                "sh",
                "-c",
                script,
            ])
            .env("JSONRPC_DEBUGGER_CONFIG_DIR", &config_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdin = wrapper.stdin.take().unwrap();
        let mut stdout = BufReader::new(wrapper.stdout.take().unwrap());
        let control_url = format!("http://127.0.0.1:{control_port}");
        wait_for_control(&control_url).await;

        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"wrap-1\",\"method\":\"example/run\"}\n")
            .await
            .unwrap();
        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(2), stdout.read_line(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            response,
            "{\"jsonrpc\":\"2.0\",\"id\":\"wrap-1\",\"result\":\"ok\"}\n"
        );

        let state = control(&control_url, "debugger.getState").await;
        assert_eq!(state["result"]["dataPlane"], "stdio");
        assert_eq!(state["result"]["proxyPort"], Value::Null);
        assert_eq!(state["result"]["transport"], "stdio-json-lines");

        let history = wait_for_history(&control_url).await;
        assert_eq!(history["method"], "example/run");
        assert_eq!(history["status"], "success");
        assert_eq!(history["transport"], "stdio-json-lines");

        let found = control_with_params(
            &control_url,
            "debugger.find",
            json!({"query": "EXAMPLE/RUN"}),
        )
        .await;
        assert_eq!(found["result"]["exchanges"].as_array().unwrap().len(), 1);
        assert_eq!(
            found["result"]["exchanges"][0]["exchange"]["method"],
            "example/run"
        );
        let reference = found["result"]["exchanges"][0]["references"][0].clone();
        assert_eq!(reference["sessionId"], state["result"]["session"]["id"]);
        assert_eq!(reference["exchangeIndex"], 0);
        assert_eq!(reference["panel"], "request");
        assert!(reference["text"].as_str().unwrap().contains("example/run"));
        assert!(found["result"]["sessions"].as_array().unwrap().is_empty());
        assert!(found["result"]["annotations"]
            .as_array()
            .unwrap()
            .is_empty());

        let revealed = control_with_params(&control_url, "debugger.revealLines", reference).await;
        assert_eq!(revealed["result"]["selectedExchange"], 0);
        assert_eq!(revealed["result"]["focus"], "request");
        assert!(revealed["result"]["lineSelection"]["text"]
            .as_str()
            .unwrap()
            .contains("example/run"));

        let client = ControlClient::new(control_url.clone());
        let state = client.state().await.unwrap();
        let mut attached = App::new();
        client
            .snapshot(state)
            .await
            .unwrap()
            .apply(&mut attached)
            .unwrap();
        assert!(attached.proxy_config.transparent);
        assert_eq!(attached.exchanges.len(), 1);
        assert_eq!(attached.exchanges[0].method.as_deref(), Some("example/run"));

        stdin.shutdown().await.unwrap();
        drop(stdin);
        let status = tokio::time::timeout(Duration::from_secs(2), wrapper.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
        std::fs::remove_dir_all(config_dir).unwrap();
    }

    async fn wait_for_control(url: &str) {
        for _ in 0..100 {
            if reqwest::Client::new()
                .post(url)
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": "state",
                    "method": "debugger.getState",
                    "params": {}
                }))
                .send()
                .await
                .is_ok()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("control plane did not start");
    }

    async fn wait_for_history(url: &str) -> Value {
        for _ in 0..100 {
            let response = control(url, "debugger.getHistory").await;
            if let Some(exchange) = response["result"]
                .as_array()
                .and_then(|items| items.first())
            {
                return exchange.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("wrapper did not record history");
    }

    async fn control(url: &str, method: &str) -> Value {
        control_with_params(url, method, json!({})).await
    }

    async fn control_with_params(url: &str, method: &str, params: Value) -> Value {
        reqwest::Client::new()
            .post(url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": "test",
                "method": method,
                "params": params
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}
