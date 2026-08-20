use crate::app::{
    App, AppMode, Focus, JsonRpcExchange, JsonRpcMessage, MessageDirection, TransportType,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    convert::Infallible,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot};
use warp::{http::StatusCode, Filter, Reply};

#[derive(Debug)]
pub enum ControlAction {
    Discover,
    GetState,
    WaitForChange {
        after_revision: u64,
        timeout_ms: u64,
    },
    GetPanel {
        focus: Focus,
    },
    GetHistory {
        limit: usize,
    },
    ExportSession,
    ReplaySession {
        session: Session,
    },
    SendRequest {
        request: Value,
    },
    SelectExchange {
        index: usize,
    },
    SetFocus {
        focus: Focus,
    },
    RevealLines {
        focus: Focus,
        start_line: usize,
        end_line: usize,
    },
    ClearLineSelection,
    ScrollPanel {
        focus: Focus,
        lines: i64,
    },
    SetTarget {
        url: String,
    },
    SetFilter {
        text: String,
    },
    SetPaused {
        paused: bool,
    },
    GetPending,
    ResolvePending {
        id: String,
        decision: PendingDecision,
    },
}

#[derive(Debug)]
pub enum PendingDecision {
    Allow {
        request: Option<Value>,
        headers: Option<HashMap<String, String>>,
    },
    Block,
    Complete {
        response: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub schema_version: u32,
    pub exported_at_ms: u64,
    pub target: String,
    pub exchanges: Vec<SessionExchange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionExchange {
    pub transport: SessionTransport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<SessionMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<SessionMessage>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SessionTransport {
    Http,
    Websocket,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessage {
    pub body: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    pub timestamp_ms: u64,
}

#[derive(Debug)]
pub struct ControlCommand {
    pub action: ControlAction,
    pub reply: oneshot::Sender<ControlResult>,
}

pub type ControlResult = Result<Value, ControlError>;

#[derive(Debug)]
pub struct ControlError {
    pub code: i64,
    pub message: String,
}

impl ControlError {
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: message.into(),
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self {
            code: -32000,
            message: message.into(),
        }
    }
}

pub async fn serve(port: u16, sender: mpsc::UnboundedSender<ControlCommand>) -> Result<(), String> {
    let sender = warp::any().map(move || sender.clone());
    let route = warp::post()
        .and(warp::path::end())
        .and(warp::body::content_length_limit(2 * 1024 * 1024))
        .and(warp::body::json())
        .and(sender)
        .and_then(handle_request);

    warp::serve(route).try_bind(([127, 0, 0, 1], port)).await;

    Ok(())
}

async fn handle_request(
    request: Value,
    sender: mpsc::UnboundedSender<ControlCommand>,
) -> Result<warp::reply::Response, Infallible> {
    let id = request.get("id").cloned();
    let action = parse_request(&request);
    let action = match action {
        Ok(action) => action,
        Err(error) => return Ok(error_response(id.unwrap_or(Value::Null), error)),
    };

    let (reply, result) = oneshot::channel();
    if sender.send(ControlCommand { action, reply }).is_err() {
        return Ok(error_response(
            id.unwrap_or(Value::Null),
            ControlError::runtime("Debugger is shutting down"),
        ));
    }

    let Some(id) = id else {
        return Ok(warp::reply::with_status("", StatusCode::NO_CONTENT).into_response());
    };
    let result = result
        .await
        .unwrap_or_else(|_| Err(ControlError::runtime("Debugger did not answer")));

    Ok(match result {
        Ok(result) => warp::reply::json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
        .into_response(),
        Err(error) => error_response(id, error),
    })
}

fn error_response(id: Value, error: ControlError) -> warp::reply::Response {
    warp::reply::json(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": error.code,
            "message": error.message,
        },
    }))
    .into_response()
}

fn parse_request(request: &Value) -> Result<ControlAction, ControlError> {
    if request.get("jsonrpc") != Some(&Value::String("2.0".to_string())) {
        return Err(ControlError::invalid_request("jsonrpc must be \"2.0\""));
    }
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| ControlError::invalid_request("method must be a string"))?;
    let params = request.get("params").unwrap_or(&Value::Null);

    match method {
        "rpc.discover" => Ok(ControlAction::Discover),
        "debugger.getState" => Ok(ControlAction::GetState),
        "debugger.waitForChange" => Ok(ControlAction::WaitForChange {
            after_revision: required_u64(params, 0, "afterRevision")?,
            timeout_ms: optional_u64(params, 1, "timeoutMs")?
                .unwrap_or(30_000)
                .min(60_000),
        }),
        "debugger.getPanel" => Ok(ControlAction::GetPanel {
            focus: parse_detail_focus(required_string(params, 0, "panel")?)?,
        }),
        "debugger.getHistory" => Ok(ControlAction::GetHistory {
            limit: optional_usize(params, 0, "limit")?.unwrap_or(100).min(1000),
        }),
        "debugger.exportSession" => Ok(ControlAction::ExportSession),
        "debugger.replaySession" => Ok(ControlAction::ReplaySession {
            session: serde_json::from_value(required(params, 0, "session")?.clone()).map_err(
                |error| ControlError::invalid_params(format!("invalid session: {error}")),
            )?,
        }),
        "debugger.sendRequest" => Ok(ControlAction::SendRequest {
            request: required(params, 0, "request")?.clone(),
        }),
        "debugger.selectExchange" => Ok(ControlAction::SelectExchange {
            index: required_usize(params, 0, "index")?,
        }),
        "debugger.setFocus" => Ok(ControlAction::SetFocus {
            focus: parse_focus(required_string(params, 0, "panel")?)?,
        }),
        "debugger.revealLines" => {
            let start_line = required_usize(params, 1, "startLine")?;
            Ok(ControlAction::RevealLines {
                focus: parse_detail_focus(required_string(params, 0, "panel")?)?,
                start_line,
                end_line: optional_usize(params, 2, "endLine")?.unwrap_or(start_line),
            })
        }
        "debugger.clearLineSelection" => Ok(ControlAction::ClearLineSelection),
        "debugger.scrollPanel" => Ok(ControlAction::ScrollPanel {
            focus: parse_scroll_focus(required_string(params, 0, "panel")?)?,
            lines: required_i64(params, 1, "lines")?,
        }),
        "debugger.setTarget" => Ok(ControlAction::SetTarget {
            url: required_string(params, 0, "url")?.to_string(),
        }),
        "debugger.setFilter" => Ok(ControlAction::SetFilter {
            text: required_string(params, 0, "text")?.to_string(),
        }),
        "debugger.setPaused" => Ok(ControlAction::SetPaused {
            paused: required_bool(params, 0, "paused")?,
        }),
        "debugger.getPending" => Ok(ControlAction::GetPending),
        "debugger.resolvePending" => parse_pending_decision(params),
        _ => Err(ControlError {
            code: -32601,
            message: format!("Method not found: {method}"),
        }),
    }
}

fn parse_pending_decision(params: &Value) -> Result<ControlAction, ControlError> {
    let id = required_string(params, 0, "id")?.to_string();
    let decision = match required_string(params, 1, "action")? {
        "allow" => PendingDecision::Allow {
            request: optional(params, 2, "request").cloned(),
            headers: optional_headers(params, 3, "headers")?,
        },
        "block" => PendingDecision::Block,
        "complete" => PendingDecision::Complete {
            response: required(params, 4, "response")?.clone(),
        },
        action => {
            return Err(ControlError::invalid_params(format!(
                "action must be allow, block, or complete; got {action}"
            )))
        }
    };

    Ok(ControlAction::ResolvePending { id, decision })
}

fn parameter<'a>(params: &'a Value, index: usize, name: &str) -> Option<&'a Value> {
    match params {
        Value::Object(params) => params.get(name),
        Value::Array(params) => params.get(index),
        Value::Null => None,
        _ => None,
    }
}

fn required<'a>(params: &'a Value, index: usize, name: &str) -> Result<&'a Value, ControlError> {
    parameter(params, index, name)
        .ok_or_else(|| ControlError::invalid_params(format!("Missing parameter: {name}")))
}

fn optional<'a>(params: &'a Value, index: usize, name: &str) -> Option<&'a Value> {
    parameter(params, index, name)
}

fn required_string<'a>(
    params: &'a Value,
    index: usize,
    name: &str,
) -> Result<&'a str, ControlError> {
    required(params, index, name)?
        .as_str()
        .ok_or_else(|| ControlError::invalid_params(format!("{name} must be a string")))
}

fn required_bool(params: &Value, index: usize, name: &str) -> Result<bool, ControlError> {
    required(params, index, name)?
        .as_bool()
        .ok_or_else(|| ControlError::invalid_params(format!("{name} must be a boolean")))
}

fn required_usize(params: &Value, index: usize, name: &str) -> Result<usize, ControlError> {
    required(params, index, name)?
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| ControlError::invalid_params(format!("{name} must be an integer")))
}

fn required_u64(params: &Value, index: usize, name: &str) -> Result<u64, ControlError> {
    required(params, index, name)?
        .as_u64()
        .ok_or_else(|| ControlError::invalid_params(format!("{name} must be an integer")))
}

fn required_i64(params: &Value, index: usize, name: &str) -> Result<i64, ControlError> {
    required(params, index, name)?
        .as_i64()
        .ok_or_else(|| ControlError::invalid_params(format!("{name} must be an integer")))
}

fn optional_usize(params: &Value, index: usize, name: &str) -> Result<Option<usize>, ControlError> {
    let Some(value) = optional(params, index, name) else {
        return Ok(None);
    };
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .map(Some)
        .ok_or_else(|| ControlError::invalid_params(format!("{name} must be an integer")))
}

fn optional_u64(params: &Value, index: usize, name: &str) -> Result<Option<u64>, ControlError> {
    let Some(value) = optional(params, index, name) else {
        return Ok(None);
    };
    value
        .as_u64()
        .map(Some)
        .ok_or_else(|| ControlError::invalid_params(format!("{name} must be an integer")))
}

fn optional_headers(
    params: &Value,
    index: usize,
    name: &str,
) -> Result<Option<HashMap<String, String>>, ControlError> {
    let Some(value) = optional(params, index, name) else {
        return Ok(None);
    };
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|_| ControlError::invalid_params(format!("{name} must contain string values")))
}

fn parse_focus(panel: &str) -> Result<Focus, ControlError> {
    match panel {
        "history" => Ok(Focus::MessageList),
        "request" => Ok(Focus::RequestSection),
        "response" => Ok(Focus::ResponseSection),
        "status" => Ok(Focus::StatusHeader),
        _ => Err(ControlError::invalid_params(
            "panel must be history, request, response, or status",
        )),
    }
}

fn parse_detail_focus(panel: &str) -> Result<Focus, ControlError> {
    match panel {
        "request" => Ok(Focus::RequestSection),
        "response" => Ok(Focus::ResponseSection),
        _ => Err(ControlError::invalid_params(
            "panel must be request or response",
        )),
    }
}

fn parse_scroll_focus(panel: &str) -> Result<Focus, ControlError> {
    match panel {
        "history" => Ok(Focus::MessageList),
        "request" => Ok(Focus::RequestSection),
        "response" => Ok(Focus::ResponseSection),
        _ => Err(ControlError::invalid_params(
            "panel must be history, request, or response",
        )),
    }
}

pub fn discovery(port: u16) -> Value {
    let mut document: Value =
        serde_json::from_str(include_str!("../openrpc.json")).expect("openrpc.json must be valid");
    document["info"]["version"] = json!(env!("CARGO_PKG_VERSION"));
    document["servers"][0]["url"] = json!(format!("http://127.0.0.1:{port}"));
    document
}

pub fn state(app: &App) -> Value {
    let line_selection = app.line_selection.as_ref().map(|selection| {
        json!({
            "panel": focus_name(selection.panel),
            "startLine": selection.start_line,
            "endLine": selection.end_line,
            "text": selection.text.join("\n"),
        })
    });
    json!({
        "revision": app.revision(),
        "running": app.is_running,
        "mode": mode_name(&app.app_mode),
        "proxyPort": app.proxy_config.listen_port,
        "controlPort": app.control_port,
        "target": app.proxy_config.target_url,
        "filter": app.filter_text,
        "focus": focus_name(app.focus),
        "lineSelection": line_selection,
        "scroll": {
            "request": app.request_details_scroll,
            "response": app.response_details_scroll,
        },
        "tabs": {
            "request": if app.request_tab == 0 { "headers" } else { "body" },
            "response": if app.response_tab == 0 { "headers" } else { "body" },
        },
        "selectedExchange": app.selected_exchange,
        "exchangeCount": app.exchanges.len(),
        "pendingCount": app.pending_requests.len(),
    })
}

pub fn change(app: &App, changed: bool) -> Value {
    json!({
        "changed": changed,
        "state": state(app),
    })
}

pub fn panel(focus: Focus, lines: Vec<String>) -> Value {
    let lines = lines
        .into_iter()
        .enumerate()
        .map(|(index, text)| json!({"line": index + 1, "text": text}))
        .collect::<Vec<_>>();
    json!({
        "panel": focus_name(focus),
        "lines": lines,
    })
}

pub fn history(app: &App, limit: usize) -> Value {
    let start = app.exchanges.len().saturating_sub(limit);
    Value::Array(
        app.exchanges
            .iter()
            .enumerate()
            .skip(start)
            .map(|(index, exchange)| exchange_value(index, exchange))
            .collect(),
    )
}

pub fn export_session(app: &App) -> Session {
    Session {
        schema_version: 1,
        exported_at_ms: timestamp_ms(SystemTime::now()),
        target: app.proxy_config.target_url.clone(),
        exchanges: app.exchanges.iter().map(SessionExchange::from).collect(),
    }
}

pub fn replay_session(session: Session) -> Result<Vec<JsonRpcExchange>, ControlError> {
    if session.schema_version != 1 {
        return Err(ControlError::invalid_params(format!(
            "unsupported session schema version: {}",
            session.schema_version
        )));
    }

    session
        .exchanges
        .into_iter()
        .enumerate()
        .map(|(index, exchange)| {
            exchange
                .try_into()
                .map_err(|error| ControlError::invalid_params(format!("exchange {index}: {error}")))
        })
        .collect()
}

impl From<&JsonRpcExchange> for SessionExchange {
    fn from(exchange: &JsonRpcExchange) -> Self {
        Self {
            transport: match exchange.transport {
                TransportType::Http => SessionTransport::Http,
                TransportType::WebSocket => SessionTransport::Websocket,
            },
            request: exchange.request.as_ref().map(SessionMessage::from),
            response: exchange.response.as_ref().map(SessionMessage::from),
        }
    }
}

impl From<&JsonRpcMessage> for SessionMessage {
    fn from(message: &JsonRpcMessage) -> Self {
        Self {
            body: message_body(message),
            headers: message.headers.clone(),
            timestamp_ms: timestamp_ms(message.timestamp),
        }
    }
}

impl TryFrom<SessionExchange> for JsonRpcExchange {
    type Error = String;

    fn try_from(exchange: SessionExchange) -> Result<Self, Self::Error> {
        if exchange.request.is_none() && exchange.response.is_none() {
            return Err("request or response is required".to_string());
        }

        let transport = match exchange.transport {
            SessionTransport::Http => TransportType::Http,
            SessionTransport::Websocket => TransportType::WebSocket,
        };
        let request = exchange
            .request
            .map(|message| session_message(message, MessageDirection::Request, transport.clone()))
            .transpose()?;
        let response = exchange
            .response
            .map(|message| session_message(message, MessageDirection::Response, transport.clone()))
            .transpose()?;
        let id = request
            .as_ref()
            .and_then(|message| message.id.clone())
            .or_else(|| response.as_ref().and_then(|message| message.id.clone()));
        let method = request.as_ref().and_then(|message| message.method.clone());
        let timestamp = request
            .as_ref()
            .map(|message| message.timestamp)
            .or_else(|| response.as_ref().map(|message| message.timestamp))
            .unwrap_or(UNIX_EPOCH);

        Ok(Self {
            id,
            method,
            request,
            response,
            timestamp,
            transport,
        })
    }
}

fn session_message(
    message: SessionMessage,
    direction: MessageDirection,
    transport: TransportType,
) -> Result<JsonRpcMessage, String> {
    let body = message
        .body
        .as_object()
        .ok_or_else(|| "message body must be an object".to_string())?;
    if body.get("jsonrpc") != Some(&json!("2.0")) {
        return Err("message jsonrpc must be \"2.0\"".to_string());
    }

    let method = body
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string);
    if matches!(direction, MessageDirection::Request) && method.is_none() {
        return Err("request method must be a string".to_string());
    }

    Ok(JsonRpcMessage {
        id: body.get("id").cloned(),
        method,
        params: body.get("params").cloned(),
        result: body.get("result").cloned(),
        error: body.get("error").cloned(),
        timestamp: UNIX_EPOCH + Duration::from_millis(message.timestamp_ms),
        direction,
        transport,
        headers: message.headers,
    })
}

fn timestamp_ms(timestamp: SystemTime) -> u64 {
    timestamp
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub fn pending(app: &App) -> Value {
    Value::Array(
        app.pending_requests
            .iter()
            .map(|pending| {
                json!({
                    "id": pending.id,
                    "request": pending.modified_request.as_ref()
                        .and_then(|request| serde_json::from_str::<Value>(request).ok())
                        .unwrap_or_else(|| message_body(&pending.original_request)),
                    "headers": pending.modified_headers.as_ref()
                        .or(pending.original_request.headers.as_ref()),
                    "modified": pending.modified_request.is_some()
                        || pending.modified_headers.is_some(),
                })
            })
            .collect(),
    )
}

fn exchange_value(index: usize, exchange: &JsonRpcExchange) -> Value {
    let duration = exchange
        .request
        .as_ref()
        .zip(exchange.response.as_ref())
        .and_then(|(request, response)| response.timestamp.duration_since(request.timestamp).ok())
        .map(|duration| duration.as_millis());
    json!({
        "index": index,
        "id": exchange.id,
        "method": exchange.method,
        "transport": transport_name(&exchange.transport),
        "status": if exchange.response.as_ref().is_some_and(|response| response.error.is_some()) {
            "error"
        } else if exchange.response.is_some() {
            "success"
        } else {
            "pending"
        },
        "durationMs": duration,
        "request": exchange.request.as_ref().map(message_value),
        "response": exchange.response.as_ref().map(message_value),
    })
}

fn message_value(message: &JsonRpcMessage) -> Value {
    json!({
        "body": message_body(message),
        "headers": message.headers,
        "timestampMs": message.timestamp.duration_since(UNIX_EPOCH)
            .unwrap_or_default().as_millis(),
    })
}

fn message_body(message: &JsonRpcMessage) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("jsonrpc".to_string(), json!("2.0"));
    if let Some(id) = &message.id {
        body.insert("id".to_string(), id.clone());
    }
    match message.direction {
        MessageDirection::Request => {
            if let Some(method) = &message.method {
                body.insert("method".to_string(), json!(method));
            }
            if let Some(params) = &message.params {
                body.insert("params".to_string(), params.clone());
            }
        }
        MessageDirection::Response => {
            if let Some(result) = &message.result {
                body.insert("result".to_string(), result.clone());
            }
            if let Some(error) = &message.error {
                body.insert("error".to_string(), error.clone());
            }
        }
    }
    Value::Object(body)
}

fn mode_name(mode: &AppMode) -> &'static str {
    match mode {
        AppMode::Normal => "normal",
        AppMode::Paused => "paused",
        AppMode::Intercepting => "intercepting",
    }
}

fn focus_name(focus: Focus) -> &'static str {
    match focus {
        Focus::MessageList => "history",
        Focus::RequestSection => "request",
        Focus::ResponseSection => "response",
        Focus::StatusHeader => "status",
    }
}

fn transport_name(transport: &TransportType) -> &'static str {
    match transport {
        TransportType::Http => "http",
        TransportType::WebSocket => "websocket",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typed_control_methods() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "debugger.setFocus",
            "params": {"panel": "response"},
        });

        assert!(matches!(
            parse_request(&request),
            Ok(ControlAction::SetFocus {
                focus: Focus::ResponseSection
            })
        ));

        let positional = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "debugger.setFocus",
            "params": ["request"],
        });
        assert!(matches!(
            parse_request(&positional),
            Ok(ControlAction::SetFocus {
                focus: Focus::RequestSection
            })
        ));
    }

    #[test]
    fn discovery_is_an_openrpc_document() {
        let document = discovery(8081);

        assert_eq!(document["openrpc"], "1.3.2");
        assert_eq!(document["servers"][0]["url"], "http://127.0.0.1:8081");
        assert_eq!(document["info"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(document["methods"].as_array().unwrap().len(), 18);
        assert!(document["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|method| method["name"] == "debugger.waitForChange"));
        assert!(document["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|method| method["name"] == "debugger.replaySession"));
    }

    #[test]
    fn parses_line_selection_and_scrolling() {
        let reveal = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "debugger.revealLines",
            "params": {"panel": "request", "startLine": 7, "endLine": 9},
        });
        let scroll = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "debugger.scrollPanel",
            "params": {"panel": "response", "lines": -4},
        });

        assert!(matches!(
            parse_request(&reveal),
            Ok(ControlAction::RevealLines {
                focus: Focus::RequestSection,
                start_line: 7,
                end_line: 9,
            })
        ));
        assert!(matches!(
            parse_request(&scroll),
            Ok(ControlAction::ScrollPanel {
                focus: Focus::ResponseSection,
                lines: -4,
            })
        ));
    }

    #[test]
    fn parses_wait_and_session_methods() {
        let wait = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "debugger.waitForChange",
            "params": {"afterRevision": 7, "timeoutMs": 250},
        });
        let replay = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "debugger.replaySession",
            "params": {"session": {
                "schemaVersion": 1,
                "exportedAtMs": 0,
                "target": "http://localhost:8545",
                "exchanges": [],
            }},
        });

        assert!(matches!(
            parse_request(&wait),
            Ok(ControlAction::WaitForChange {
                after_revision: 7,
                timeout_ms: 250,
            })
        ));
        assert!(matches!(
            parse_request(&replay),
            Ok(ControlAction::ReplaySession { .. })
        ));
    }

    #[test]
    fn state_exposes_the_shared_line_reference() {
        let mut app = App::new();
        app.reveal_lines(
            Focus::ResponseSection,
            3,
            4,
            vec!["first".to_string(), "second".to_string()],
        );

        let state = state(&app);

        assert_eq!(state["focus"], "response");
        assert_eq!(state["scroll"]["response"], 2);
        assert_eq!(state["lineSelection"]["panel"], "response");
        assert_eq!(state["lineSelection"]["startLine"], 3);
        assert_eq!(state["lineSelection"]["endLine"], 4);
        assert_eq!(state["lineSelection"]["text"], "first\nsecond");

        let panel = panel(
            Focus::ResponseSection,
            vec!["first".to_string(), "second".to_string()],
        );
        assert_eq!(panel["panel"], "response");
        assert_eq!(panel["lines"][0]["line"], 1);
        assert_eq!(panel["lines"][1]["text"], "second");
    }

    #[test]
    fn sessions_round_trip_history() {
        let mut app = App::new();
        app.add_message(JsonRpcMessage {
            id: Some(json!(7)),
            method: Some("eth_chainId".to_string()),
            params: Some(json!([])),
            result: None,
            error: None,
            timestamp: UNIX_EPOCH + Duration::from_millis(10),
            direction: MessageDirection::Request,
            transport: TransportType::Http,
            headers: Some(HashMap::from([("x-test".to_string(), "yes".to_string())])),
        });
        app.add_message(JsonRpcMessage {
            id: Some(json!(7)),
            method: None,
            params: None,
            result: Some(json!("0x1")),
            error: None,
            timestamp: UNIX_EPOCH + Duration::from_millis(20),
            direction: MessageDirection::Response,
            transport: TransportType::Http,
            headers: None,
        });

        let session = export_session(&app);
        let exchanges = replay_session(session.clone()).unwrap();

        assert_eq!(session.schema_version, 1);
        assert_eq!(exchanges.len(), 1);
        assert_eq!(exchanges[0].method.as_deref(), Some("eth_chainId"));
        assert_eq!(
            exchanges[0].response.as_ref().unwrap().result,
            Some(json!("0x1"))
        );
        assert_eq!(
            exchanges[0]
                .request
                .as_ref()
                .unwrap()
                .headers
                .as_ref()
                .unwrap()["x-test"],
            "yes"
        );
        assert!(replay_session(Session {
            schema_version: 2,
            ..session
        })
        .is_err());
    }
}
