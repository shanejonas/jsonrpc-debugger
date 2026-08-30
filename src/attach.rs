use crate::{
    app::{
        App, AppMode, Framing, JsonRpcMessage, MessageDirection, PendingRequest, ProxyConfig,
        SessionSummary, TransportType,
    },
    control::{self, Session},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, time::SystemTime};
use tokio::sync::oneshot;

#[derive(Clone)]
pub struct ControlClient {
    url: String,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteState {
    pub revision: u64,
    pub running: bool,
    pub mode: String,
    pub data_plane: String,
    pub proxy_port: Option<u16>,
    pub control_port: u16,
    pub target: String,
    pub transport: String,
    pub session: SessionSummary,
}

pub struct Snapshot {
    pub state: RemoteState,
    pub session: Session,
    pending: Vec<RemotePending>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemotePending {
    id: String,
    request: Value,
    headers: Option<HashMap<String, String>>,
    modified: bool,
}

impl ControlClient {
    pub fn new(url: String) -> Self {
        Self {
            url,
            client: reqwest::Client::new(),
        }
    }

    pub async fn state(&self) -> Result<RemoteState, String> {
        serde_json::from_value(self.call("debugger.getState", json!({})).await?)
            .map_err(|error| error.to_string())
    }

    pub async fn snapshot(&self, state: RemoteState) -> Result<Snapshot, String> {
        if state.data_plane != "stdio" {
            return Err("attach requires a transparent stdio wrapper".to_string());
        }
        let session = serde_json::from_value(self.call("debugger.exportSession", json!({})).await?)
            .map_err(|error| error.to_string())?;
        let pending = serde_json::from_value(self.call("debugger.getPending", json!({})).await?)
            .map_err(|error| error.to_string())?;
        Ok(Snapshot {
            state,
            session,
            pending,
        })
    }

    pub async fn set_paused(&self, paused: bool) -> Result<(), String> {
        self.call("debugger.setPaused", json!({"paused": paused}))
            .await?;
        Ok(())
    }

    pub async fn allow(&self, id: &str, request: Option<Value>) -> Result<(), String> {
        let mut params = json!({"id": id, "action": "allow"});
        if let Some(request) = request {
            params["request"] = request;
        }
        self.call("debugger.resolvePending", params).await?;
        Ok(())
    }

    pub async fn block(&self, id: &str) -> Result<(), String> {
        self.call(
            "debugger.resolvePending",
            json!({"id": id, "action": "block"}),
        )
        .await?;
        Ok(())
    }

    pub async fn complete(&self, id: &str, response: Value) -> Result<(), String> {
        self.call(
            "debugger.resolvePending",
            json!({"id": id, "action": "complete", "response": response}),
        )
        .await?;
        Ok(())
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let response = self
            .client
            .post(&self.url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": "attach",
                "method": method,
                "params": params,
            }))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let body: Value = response.json().await.map_err(|error| error.to_string())?;
        if let Some(error) = body.get("error") {
            return Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("control request failed")
                .to_string());
        }
        body.get("result")
            .cloned()
            .ok_or_else(|| "control response is missing result".to_string())
    }
}

impl Snapshot {
    pub fn apply(self, app: &mut App) -> Result<(), String> {
        let transport = parse_transport(&self.state.transport)?;
        let mode = parse_mode(&self.state.mode)?;
        let exchanges = control::replay_session(self.session).map_err(|error| error.message)?;
        let first_snapshot = app.session.is_none();

        app.proxy_config = ProxyConfig {
            listen_port: self.state.proxy_port.unwrap_or_default(),
            target_url: self.state.target,
            transport,
            stdio: None,
            transparent: true,
        };
        app.control_port = self.state.control_port;
        app.is_running = self.state.running;
        if first_snapshot {
            app.activate_session(self.state.session, exchanges, Vec::new());
        } else {
            let selected = app.selected_exchange.min(exchanges.len().saturating_sub(1));
            app.exchanges = exchanges;
            app.session = Some(self.state.session);
            app.selected_exchange = selected;
            app.mark_changed();
        }
        app.app_mode = mode;
        app.pending_requests = self
            .pending
            .into_iter()
            .map(|pending| attached_pending(pending, transport))
            .collect();
        app.selected_pending = app
            .selected_pending
            .min(app.pending_requests.len().saturating_sub(1));
        if first_snapshot {
            app.set_notice("Attached; Ctrl-B p pauses client requests");
        }
        Ok(())
    }
}

fn attached_pending(pending: RemotePending, transport: TransportType) -> PendingRequest {
    let request = match &pending.request {
        Value::Array(requests) => requests
            .iter()
            .find(|request| request.get("method").is_some())
            .unwrap_or(&pending.request),
        request => request,
    };
    let (decision_sender, _decision_receiver) = oneshot::channel();
    PendingRequest {
        id: pending.id,
        original_request: JsonRpcMessage {
            id: request.get("id").cloned(),
            method: request
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_string),
            params: request.get("params").cloned(),
            result: None,
            error: None,
            timestamp: SystemTime::now(),
            direction: MessageDirection::Request,
            transport,
            headers: pending.headers.clone(),
        },
        modified_request: pending
            .modified
            .then(|| serde_json::to_string_pretty(&pending.request).unwrap_or_default()),
        original_body: pending.request,
        modified_headers: None,
        decision_sender,
    }
}

fn parse_transport(name: &str) -> Result<TransportType, String> {
    match name {
        "http" => Ok(TransportType::Http),
        "http-batch" => Ok(TransportType::HttpBatch),
        "stdio-json-lines" => Ok(TransportType::Stdio(Framing::JsonLines)),
        "stdio-content-length" => Ok(TransportType::Stdio(Framing::ContentLength)),
        "websocket" => Ok(TransportType::WebSocket),
        name => Err(format!("unsupported transport: {name}")),
    }
}

fn parse_mode(name: &str) -> Result<AppMode, String> {
    match name {
        "normal" => Ok(AppMode::Normal),
        "paused" => Ok(AppMode::Paused),
        "intercepting" => Ok(AppMode::Intercepting),
        name => Err(format!("unsupported mode: {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_remote_transports() {
        assert_eq!(
            parse_transport("carrier-pigeon").unwrap_err(),
            "unsupported transport: carrier-pigeon"
        );
    }
}
