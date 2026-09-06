#[cfg(unix)]
mod unix {
    use jsonrpc_debugger::{
        app::{App, AppMode},
        attach::ControlClient,
    };
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

        let output = Command::new(env!("CARGO_BIN_EXE_jsonrpc-debugger"))
            .args([
                "call",
                "--transport=http",
                &control_url,
                r#"{"jsonrpc":"2.0","id":1,"method":"debugger.getState","params":{}}"#,
            ])
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["result"]["dataPlane"], "stdio");

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
        let mut attached = App::new();
        client
            .snapshot(&attached)
            .await
            .unwrap()
            .apply(&mut attached)
            .unwrap();
        assert!(attached.proxy_config.transparent);
        assert_eq!(attached.exchanges().len(), 1);
        assert_eq!(
            attached.exchanges()[0].method.as_deref(),
            Some("example/run")
        );

        verify_annotation_threads(&control_url, &client, &mut attached).await;

        stdin.shutdown().await.unwrap();
        drop(stdin);
        let status = tokio::time::timeout(Duration::from_secs(2), wrapper.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
        std::fs::remove_dir_all(config_dir).unwrap();
    }

    async fn verify_annotation_threads(url: &str, client: &ControlClient, attached: &mut App) {
        use jsonrpc_debugger::app::{AnnotationAuthor, Focus};
        let before = control(url, "debugger.getState").await;
        let created = control_with_params(
            url,
            "debugger.annotateLines",
            json!({
                "panel": "request", "startLine": 2, "message": "What if every task is queued?"
            }),
        )
        .await;
        assert!(created.get("error").is_none(), "{created}");
        let root = &created["result"]["annotation"];
        let root_id = root["id"].as_str().unwrap();
        assert_eq!(root["author"], "agent");
        assert_eq!(root["parentId"], Value::Null);
        assert!(root["createdAtMs"].as_u64().unwrap() > 0);
        client
            .snapshot(attached)
            .await
            .unwrap()
            .apply(attached)
            .unwrap();
        assert!(attached.start_replying_to_annotation(root_id));
        attached.input_buffer = "Added a regression test for that path.".to_string();
        let reply_id = client.save_annotation(attached).await.unwrap();
        attached.cancel_editing();
        client
            .snapshot(attached)
            .await
            .unwrap()
            .apply(attached)
            .unwrap();
        let reply = attached
            .annotations
            .iter()
            .find(|note| note.id == reply_id)
            .unwrap()
            .clone();
        assert_eq!(reply.parent_id.as_deref(), Some(root_id));
        assert_eq!(reply.author, AnnotationAuthor::User);
        assert_eq!(reply.text, attached.annotations[0].text);
        attached.start_editing_annotation(&reply_id);
        attached.input_buffer = "The regression test passes.".to_string();
        assert_eq!(client.save_annotation(attached).await.unwrap(), reply_id);
        attached.cancel_editing();
        let nested = control_with_params(
            url,
            "debugger.replyAnnotation",
            json!({
                "annotationId": reply_id, "message": "Confirmed."
            }),
        )
        .await;
        assert_eq!(nested["result"]["annotation"]["parentId"], reply_id);
        assert_eq!(nested["result"]["annotation"]["author"], "agent");
        let notes = control(url, "debugger.getAnnotations").await;
        assert_eq!(notes["result"].as_array().unwrap().len(), 3);
        assert_eq!(notes["result"][0]["id"], root_id);
        assert_eq!(notes["result"][1]["id"], reply_id);
        assert_eq!(notes["result"][1]["message"], "The regression test passes.");
        assert_eq!(notes["result"][1]["createdAtMs"], reply.created_at_ms);
        let after = control(url, "debugger.getState").await;
        for key in [
            "focus",
            "scroll",
            "selectedExchange",
            "tabs",
            "lineSelection",
        ] {
            assert_eq!(before["result"][key], after["result"][key]);
        }
        client.remove_annotation(root_id).await.unwrap();
        client
            .snapshot(attached)
            .await
            .unwrap()
            .apply(attached)
            .unwrap();
        assert_eq!(attached.annotations.len(), 2);
        assert_eq!(attached.annotations[0].parent_id, None);
        assert_eq!(
            attached.annotations[1].parent_id.as_deref(),
            Some(reply_id.as_str())
        );
        let missing = control_with_params(
            url,
            "debugger.replyAnnotation",
            json!({"annotationId": root_id, "message": "Late reply"}),
        )
        .await;
        assert_eq!(missing["error"]["code"], -32602);
        let invalid_author = control_with_params(
            url,
            "debugger.replyAnnotation",
            json!({"annotationId": reply_id, "message": "Invalid author", "author": "unknown"}),
        )
        .await;
        assert_eq!(invalid_author["error"]["code"], -32602);
        // New root notes written by the attached UI are also attributed to the user.
        attached.select_lines(Focus::RequestSection, 2, 2, reply.text);
        attached.start_visual_selection();
        attached.start_annotating_selection();
        attached.input_buffer = "Another observation.".to_string();
        let user_root = client.save_annotation(attached).await.unwrap();
        attached.cancel_editing();
        client
            .snapshot(attached)
            .await
            .unwrap()
            .apply(attached)
            .unwrap();
        let note = attached
            .annotations
            .iter()
            .find(|note| note.id == user_root)
            .unwrap();
        assert_eq!(note.author, AnnotationAuthor::User);
        assert_eq!(note.parent_id, None);
    }

    #[tokio::test]
    async fn wrap_pauses_and_resolves_client_requests() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let control_port = listener.local_addr().unwrap().port();
        drop(listener);
        let config_dir =
            std::env::temp_dir().join(format!("jsonrpc-debugger-wrap-pause-{}", Uuid::new_v4()));
        let script = concat!(
            "while IFS= read -r line; do ",
            "case \"$line\" in *\\\"pause-1\\\"*) id=pause-1 ;; *\\\"edit-1\\\"*) id=edit-1 ;; *) id=unknown ;; esac; ",
            "case \"$line\" in *\\\"edited/run\\\"*) result=edited ;; *) result=ok ;; esac; ",
            "printf '{\"jsonrpc\":\"2.0\",\"id\":\"%s\",\"result\":\"%s\"}\\n' \"$id\" \"$result\"; ",
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

        let paused =
            control_with_params(&control_url, "debugger.setPaused", json!({"paused": true})).await;
        assert_eq!(paused["result"]["mode"], "paused");

        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"pause-1\",\"method\":\"example/run\"}\n")
            .await
            .unwrap();
        let pending = wait_for_pending(&control_url).await;
        assert_eq!(pending["request"]["id"], "pause-1");
        let client = ControlClient::new(control_url.clone());
        let mut attached = App::new();
        client
            .snapshot(&attached)
            .await
            .unwrap()
            .apply(&mut attached)
            .unwrap();
        assert_eq!(attached.app_mode, AppMode::Paused);
        assert_eq!(attached.pending_requests.len(), 1);
        assert_eq!(attached.pending_requests[0].original_body["id"], "pause-1");

        let mut response = String::new();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), stdout.read_line(&mut response))
                .await
                .is_err()
        );

        client
            .allow(pending["id"].as_str().unwrap(), None)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), stdout.read_line(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            response,
            "{\"jsonrpc\":\"2.0\",\"id\":\"pause-1\",\"result\":\"ok\"}\n"
        );

        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"block-1\",\"method\":\"example/block\"}\n")
            .await
            .unwrap();
        let pending = wait_for_pending(&control_url).await;
        assert_eq!(pending["request"]["id"], "block-1");
        client.block(pending["id"].as_str().unwrap()).await.unwrap();

        response.clear();
        tokio::time::timeout(Duration::from_secs(2), stdout.read_line(&mut response))
            .await
            .unwrap()
            .unwrap();
        let blocked: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(blocked["id"], "block-1");
        assert_eq!(blocked["error"]["code"], -32603);
        assert_eq!(blocked["error"]["message"], "Request blocked by user");

        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"edit-1\",\"method\":\"original/run\"}\n")
            .await
            .unwrap();
        let pending = wait_for_pending(&control_url).await;
        client
            .allow(
                pending["id"].as_str().unwrap(),
                Some(json!({
                    "jsonrpc": "2.0",
                    "id": "edit-1",
                    "method": "edited/run"
                })),
            )
            .await
            .unwrap();
        response.clear();
        tokio::time::timeout(Duration::from_secs(2), stdout.read_line(&mut response))
            .await
            .unwrap()
            .unwrap();
        let edited: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(edited["id"], "edit-1");
        assert_eq!(edited["result"], "edited");

        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"done-1\",\"method\":\"example/complete\"}\n")
            .await
            .unwrap();
        let pending = wait_for_pending(&control_url).await;
        client
            .complete(
                pending["id"].as_str().unwrap(),
                json!({"jsonrpc": "2.0", "id": "done-1", "result": "custom"}),
            )
            .await
            .unwrap();
        response.clear();
        tokio::time::timeout(Duration::from_secs(2), stdout.read_line(&mut response))
            .await
            .unwrap()
            .unwrap();
        let completed: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(completed["id"], "done-1");
        assert_eq!(completed["result"], "custom");

        client.set_paused(false).await.unwrap();
        let resumed = control(&control_url, "debugger.getState").await;
        assert_eq!(resumed["result"]["mode"], "normal");
        assert_eq!(resumed["result"]["pendingCount"], 0);

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

    async fn wait_for_pending(url: &str) -> Value {
        for _ in 0..100 {
            let response = control(url, "debugger.getPending").await;
            if let Some(pending) = response["result"]
                .as_array()
                .and_then(|items| items.first())
            {
                return pending.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("wrapper did not intercept request");
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
