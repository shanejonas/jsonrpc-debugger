use crate::{
    app::{
        App, AppMode, Framing, JsonRpcMessage, LineAnnotation, MessageDirection, PendingRequest,
        ProxyConfig, SessionSummary, TransportType,
    },
    control::SessionExchange,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub state: RemoteState,
    reset: bool,
    next_index: usize,
    exchanges: Vec<RemoteExchange>,
    annotations: Vec<LineAnnotation>,
    pending: Vec<RemotePending>,
}

#[derive(Deserialize)]
struct RemoteExchange {
    index: usize,
    #[serde(flatten)]
    exchange: SessionExchange,
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

    pub async fn snapshot(&self, app: &App) -> Result<Snapshot, String> {
        let mut params = json!({
            "nextIndex": app.exchanges().len(),
            "pendingIndices": app.pending_exchange_indices().collect::<Vec<_>>(),
        });
        if let Some(session) = &app.session {
            params["sessionId"] = json!(session.id);
        }
        serde_json::from_value(self.call("debugger.getUpdates", params).await?)
            .map_err(|error| error.to_string())
    }

    pub async fn save_annotation(&self, app: &App) -> Result<String, String> {
        if let Some(id) = &app.annotation_edit_id {
            self.call(
                "debugger.updateAnnotation",
                json!({"annotationId": id, "message": app.input_buffer}),
            )
            .await?;
            return Ok(id.clone());
        }
        let result = if let Some(id) = &app.annotation_reply_id {
            self.call(
                "debugger.replyAnnotation",
                json!({"annotationId": id, "message": app.input_buffer, "author": "user"}),
            )
            .await?
        } else {
            let selection = app
                .line_selection
                .as_ref()
                .filter(|_| app.visual_selection_active)
                .ok_or_else(|| "No visual selection".to_string())?;
            let panel = match selection.panel {
                crate::app::Focus::RequestSection => "request",
                crate::app::Focus::ResponseSection => "response",
                _ => return Err("Select request or response lines".to_string()),
            };
            let params = json!({
                "panel": panel,
                "exchangeIndex": app.selected_exchange,
                "tab": app.detail_tab(selection.panel),
                "startLine": selection.start_line,
                "endLine": selection.end_line,
                "message": app.input_buffer,
                "author": "user",
            });
            self.call("debugger.annotateLines", params).await?
        };
        result["annotation"]["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "annotation response is missing id".to_string())
    }

    pub async fn remove_annotation(&self, id: &str) -> Result<(), String> {
        self.call("debugger.removeAnnotation", json!({"annotationId": id}))
            .await?;
        Ok(())
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
    pub fn apply(self, app: &mut App) -> Result<u64, String> {
        if self.state.data_plane != "stdio" {
            return Err("attach requires a transparent stdio wrapper".to_string());
        }
        let transport = parse_transport(&self.state.transport)?;
        let mode = parse_mode(&self.state.mode)?;
        let first_snapshot = app.session.is_none();
        if !self.reset
            && app.session.as_ref().map(|session| &session.id) != Some(&self.state.session.id)
        {
            return Err("incremental update belongs to a different session".to_string());
        }
        let mut next_index = if self.reset { 0 } else { app.exchanges().len() };
        let mut previous = None;
        let mut exchanges = Vec::with_capacity(self.exchanges.len());
        for update in self.exchanges {
            if update.index > next_index || previous.is_some_and(|index| update.index <= index) {
                return Err(
                    "exchange updates must be ordered and contiguous with local history"
                        .to_string(),
                );
            }
            if update.index == next_index {
                next_index += 1;
            }
            previous = Some(update.index);
            exchanges.push((update.index, update.exchange.try_into()?));
        }
        if next_index != self.next_index || next_index != self.state.session.exchange_count {
            return Err("exchange update count does not match session".to_string());
        }

        app.proxy_config = ProxyConfig {
            listen_port: self.state.proxy_port.unwrap_or_default(),
            target_url: self.state.target,
            transport,
            stdio: None,
            transparent: true,
        };
        app.control_port = self.state.control_port;
        app.is_running = self.state.running;
        if self.reset {
            app.activate_session(
                self.state.session,
                exchanges
                    .into_iter()
                    .map(|(_, exchange)| exchange)
                    .collect(),
                self.annotations,
            );
        } else {
            for (index, exchange) in exchanges {
                if index < app.exchanges().len() {
                    app.replace_exchange(index, exchange);
                } else {
                    app.push_exchange(exchange);
                }
            }
            app.annotations = self.annotations;
            if app
                .active_annotation_id
                .as_ref()
                .is_some_and(|id| !app.annotations.iter().any(|note| &note.id == id))
            {
                app.active_annotation_id = None;
            }
            app.session = Some(self.state.session);
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
        Ok(self.state.revision)
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
