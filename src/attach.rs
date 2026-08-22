use crate::{
    app::SessionSummary,
    app::{App, Framing, ProxyConfig, TransportType},
    control::{self, Session},
};
use serde::Deserialize;
use serde_json::{json, Value};

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
        Ok(Snapshot { state, session })
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
        app.notice =
            Some("Attached read-only; the external client owns the data plane".to_string());
        Ok(())
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
