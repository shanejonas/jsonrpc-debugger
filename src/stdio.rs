pub use crate::app::Framing;
use crate::{
    app::{
        incoming_json_rpc_messages, json_rpc_messages_by_shape, AppMode, JsonRpcMessage,
        MessageDirection, PendingRequest, ProxyDecision, TransportType,
    },
    proxy::ProxyState,
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::Command,
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{sleep_until, Instant},
};
use uuid::Uuid;

pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Framer {
    framing: Framing,
    buffer: Vec<u8>,
}

impl Framer {
    pub fn new(framing: Framing) -> Self {
        Self {
            framing,
            buffer: Vec::new(),
        }
    }

    pub fn encode(&self, message: &Value) -> Result<Vec<u8>, String> {
        let body = serde_json::to_vec(message).map_err(|error| error.to_string())?;
        Ok(match self.framing {
            Framing::JsonLines => [body.as_slice(), b"\n"].concat(),
            Framing::ContentLength => [
                format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes(),
                body.as_slice(),
            ]
            .concat(),
        })
    }

    pub fn decode(&mut self, chunk: &[u8]) -> Result<Vec<Value>, String> {
        self.buffer.extend_from_slice(chunk);
        match self.framing {
            Framing::JsonLines => self.decode_json_lines(),
            Framing::ContentLength => self.decode_content_length(),
        }
    }

    fn decode_json_lines(&mut self) -> Result<Vec<Value>, String> {
        let mut messages = Vec::new();
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line = self.buffer.drain(..=end).collect::<Vec<_>>();
            let line = trim_ascii(&line[..line.len() - 1]);
            if !line.is_empty() {
                messages.push(serde_json::from_slice(line).map_err(|error| error.to_string())?);
            }
        }
        Ok(messages)
    }

    fn decode_content_length(&mut self) -> Result<Vec<Value>, String> {
        let mut messages = Vec::new();
        while let Some(header_end) = find_bytes(&self.buffer, b"\r\n\r\n") {
            let headers = std::str::from_utf8(&self.buffer[..header_end])
                .map_err(|error| error.to_string())?;
            let content_length = headers
                .split("\r\n")
                .find_map(|header| {
                    let (name, value) = header.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim())
                })
                .ok_or_else(|| "Missing Content-Length header".to_string())?
                .parse::<usize>()
                .map_err(|error| format!("Invalid Content-Length header: {error}"))?;
            let body_start = header_end + 4;
            let frame_end = body_start + content_length;
            if self.buffer.len() < frame_end {
                break;
            }
            messages.push(
                serde_json::from_slice(&self.buffer[body_start..frame_end])
                    .map_err(|error| error.to_string())?,
            );
            self.buffer.drain(..frame_end);
        }
        Ok(messages)
    }
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |end| end + 1);
    &bytes[start..end]
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[allow(dead_code)]
pub async fn relay<ClientReader, ClientWriter, ServerReader, ServerWriter>(
    client_reader: ClientReader,
    client_writer: ClientWriter,
    server_reader: ServerReader,
    server_writer: ServerWriter,
    framing: Framing,
    message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
) -> Result<(), String>
where
    ClientReader: AsyncRead + Unpin,
    ClientWriter: AsyncWrite + Unpin,
    ServerReader: AsyncRead + Unpin,
    ServerWriter: AsyncWrite + Unpin,
{
    let transport = TransportType::Stdio(framing);
    let client_to_server = forward_frames(
        client_reader,
        server_writer,
        framing,
        transport,
        message_sender.clone(),
    );
    let server_to_client = forward_frames(
        server_reader,
        client_writer,
        framing,
        transport,
        message_sender,
    );
    tokio::pin!(client_to_server, server_to_client);

    tokio::select! {
        result = &mut server_to_client => result,
        result = &mut client_to_server => {
            result?;
            server_to_client.await
        }
    }
}

pub async fn relay_with_interception<ClientReader, ClientWriter, ServerReader, ServerWriter>(
    client_reader: ClientReader,
    client_writer: ClientWriter,
    server_reader: ServerReader,
    server_writer: ServerWriter,
    framing: Framing,
    message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
    proxy_state: ProxyState,
) -> Result<(), String>
where
    ClientReader: AsyncRead + Unpin,
    ClientWriter: AsyncWrite + Unpin,
    ServerReader: AsyncRead + Unpin,
    ServerWriter: AsyncWrite + Unpin,
{
    let transport = TransportType::Stdio(framing);
    let (response_sender, response_receiver) = mpsc::unbounded_channel();
    let client_to_server = forward_client_frames(
        client_reader,
        server_writer,
        framing,
        transport,
        message_sender.clone(),
        proxy_state,
        response_sender,
    );
    let server_to_client = forward_server_frames(
        server_reader,
        client_writer,
        framing,
        transport,
        message_sender,
        response_receiver,
    );
    tokio::pin!(client_to_server, server_to_client);

    tokio::select! {
        result = &mut server_to_client => result,
        result = &mut client_to_server => {
            result?;
            server_to_client.await
        }
    }
}

pub async fn wrap(
    command: &[OsString],
    framing: Framing,
    message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
    proxy_state: ProxyState,
) -> Result<(), String> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| "wrap command cannot be empty".to_string())?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("failed to start {}: {error}", program.to_string_lossy()))?;
    let child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| "failed to open child stdin".to_string())?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to open child stdout".to_string())?;
    let mut child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to open child stderr".to_string())?;
    let stderr = tokio::spawn(async move {
        let _ = io::copy(&mut child_stderr, &mut io::stderr()).await;
    });

    let result = relay_with_interception(
        io::stdin(),
        io::stdout(),
        child_stdout,
        child_stdin,
        framing,
        message_sender,
        proxy_state,
    )
    .await;
    if result.is_err() {
        let _ = child.kill().await;
    }
    let status = child.wait().await.map_err(|error| error.to_string())?;
    let _ = stderr.await;
    result?;
    if !status.success() {
        return Err(format!(
            "{} exited with {status}",
            program.to_string_lossy()
        ));
    }
    Ok(())
}

struct RawFramer {
    framing: Framing,
    buffer: Vec<u8>,
}

impl RawFramer {
    fn new(framing: Framing) -> Self {
        Self {
            framing,
            buffer: Vec::new(),
        }
    }

    fn decode(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        self.buffer.extend_from_slice(chunk);
        match self.framing {
            Framing::JsonLines => Ok(self.decode_json_lines()),
            Framing::ContentLength => self.decode_content_length(),
        }
    }

    fn decode_json_lines(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            frames.push(self.buffer.drain(..=end).collect());
        }
        frames
    }

    fn decode_content_length(&mut self) -> Result<Vec<Vec<u8>>, String> {
        let mut frames = Vec::new();
        while let Some(header_end) = find_bytes(&self.buffer, b"\r\n\r\n") {
            let headers = std::str::from_utf8(&self.buffer[..header_end])
                .map_err(|error| error.to_string())?;
            let content_length = headers
                .split("\r\n")
                .find_map(|header| {
                    let (name, value) = header.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim())
                })
                .ok_or_else(|| "Missing Content-Length header".to_string())?
                .parse::<usize>()
                .map_err(|error| format!("Invalid Content-Length header: {error}"))?;
            let frame_end = header_end + 4 + content_length;
            if self.buffer.len() < frame_end {
                break;
            }
            frames.push(self.buffer.drain(..frame_end).collect());
        }
        Ok(frames)
    }

    fn remaining(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buffer)
    }
}

async fn forward_client_frames<Reader, Writer>(
    mut reader: Reader,
    mut writer: Writer,
    framing: Framing,
    transport: TransportType,
    message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
    proxy_state: ProxyState,
    response_sender: mpsc::UnboundedSender<Value>,
) -> Result<(), String>
where
    Reader: AsyncRead + Unpin,
    Writer: AsyncWrite + Unpin,
{
    let encoder = Framer::new(framing);
    let mut decoder = Some(RawFramer::new(framing));
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader
            .read(&mut chunk)
            .await
            .map_err(|error| error.to_string())?;
        if count == 0 {
            if let Some(decoder) = &mut decoder {
                writer
                    .write_all(&decoder.remaining())
                    .await
                    .map_err(|error| error.to_string())?;
            }
            writer.shutdown().await.map_err(|error| error.to_string())?;
            return Ok(());
        }

        let Some(raw_decoder) = &mut decoder else {
            writer
                .write_all(&chunk[..count])
                .await
                .map_err(|error| error.to_string())?;
            writer.flush().await.map_err(|error| error.to_string())?;
            continue;
        };
        let frames = match raw_decoder.decode(&chunk[..count]) {
            Ok(frames) => frames,
            Err(error) => {
                eprintln!(
                    "jsonrpc-debugger: stopped decoding {}: {error}",
                    transport.label()
                );
                writer
                    .write_all(&raw_decoder.remaining())
                    .await
                    .map_err(|error| error.to_string())?;
                writer.flush().await.map_err(|error| error.to_string())?;
                decoder = None;
                continue;
            }
        };

        for frame in frames {
            let body = match frame_body(&frame, framing) {
                Ok(body) => body,
                Err(_) => {
                    writer
                        .write_all(&frame)
                        .await
                        .map_err(|error| error.to_string())?;
                    writer.flush().await.map_err(|error| error.to_string())?;
                    continue;
                }
            };
            let request = record_body(&body, transport, &message_sender);
            if request.is_none() || !is_paused(&proxy_state) {
                writer
                    .write_all(&frame)
                    .await
                    .map_err(|error| error.to_string())?;
                writer.flush().await.map_err(|error| error.to_string())?;
                continue;
            }

            let request = request.unwrap();
            let (decision_sender, decision_receiver) = oneshot::channel();
            proxy_state
                .pending_sender
                .send(PendingRequest {
                    id: Uuid::new_v4().to_string(),
                    original_request: request,
                    original_body: body.clone(),
                    modified_request: None,
                    modified_headers: None,
                    decision_sender,
                })
                .map_err(|_| "interception queue is closed".to_string())?;

            let decision = tokio::time::timeout(Duration::from_secs(300), decision_receiver).await;
            match decision {
                Ok(Ok(ProxyDecision::Allow(modified, _))) => {
                    let outgoing = match modified {
                        Some(body) => encoder.encode(&body)?,
                        None => frame,
                    };
                    writer
                        .write_all(&outgoing)
                        .await
                        .map_err(|error| error.to_string())?;
                    writer.flush().await.map_err(|error| error.to_string())?;
                }
                Ok(Ok(ProxyDecision::Block)) => {
                    if let Some(response) = blocked_response(&body, "Request blocked by user") {
                        response_sender
                            .send(response)
                            .map_err(|_| "client response stream is closed".to_string())?;
                    }
                }
                Ok(Ok(ProxyDecision::Complete(response))) => {
                    response_sender
                        .send(response)
                        .map_err(|_| "client response stream is closed".to_string())?;
                }
                Ok(Err(_)) | Err(_) => {
                    if let Some(response) =
                        blocked_response(&body, "Request timed out waiting for user decision")
                    {
                        response_sender
                            .send(response)
                            .map_err(|_| "client response stream is closed".to_string())?;
                    }
                }
            }
        }
    }
}

async fn forward_server_frames<Reader, Writer>(
    mut reader: Reader,
    mut writer: Writer,
    framing: Framing,
    transport: TransportType,
    message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
    mut responses: mpsc::UnboundedReceiver<Value>,
) -> Result<(), String>
where
    Reader: AsyncRead + Unpin,
    Writer: AsyncWrite + Unpin,
{
    let encoder = Framer::new(framing);
    let mut decoder = Some(Framer::new(framing));
    let mut chunk = [0_u8; 8192];
    loop {
        tokio::select! {
            read = reader.read(&mut chunk) => {
                let count = read.map_err(|error| error.to_string())?;
                if count == 0 {
                    writer.shutdown().await.map_err(|error| error.to_string())?;
                    return Ok(());
                }
                writer.write_all(&chunk[..count]).await.map_err(|error| error.to_string())?;
                writer.flush().await.map_err(|error| error.to_string())?;
                record_chunk(&chunk[..count], transport, &message_sender, &mut decoder);
            }
            Some(response) = responses.recv() => {
                let frame = encoder.encode(&response)?;
                writer.write_all(&frame).await.map_err(|error| error.to_string())?;
                writer.flush().await.map_err(|error| error.to_string())?;
                record_body(&response, transport, &message_sender);
            }
        }
    }
}

fn record_chunk(
    chunk: &[u8],
    transport: TransportType,
    message_sender: &mpsc::UnboundedSender<JsonRpcMessage>,
    decoder: &mut Option<Framer>,
) {
    let Some(framer) = decoder else {
        return;
    };
    match framer.decode(chunk) {
        Ok(bodies) => {
            for body in bodies {
                record_body(&body, transport, message_sender);
            }
        }
        Err(error) => {
            eprintln!(
                "jsonrpc-debugger: stopped decoding {}: {error}",
                transport.label()
            );
            *decoder = None;
        }
    }
}

fn record_body(
    body: &Value,
    transport: TransportType,
    message_sender: &mpsc::UnboundedSender<JsonRpcMessage>,
) -> Option<JsonRpcMessage> {
    let messages = json_rpc_messages_by_shape(body, transport, None);
    let request = messages
        .iter()
        .find(|message| matches!(message.direction, MessageDirection::Request))
        .cloned();
    for message in messages {
        let _ = message_sender.send(message);
    }
    request
}

fn is_paused(proxy_state: &ProxyState) -> bool {
    proxy_state
        .app_mode
        .lock()
        .is_ok_and(|mode| matches!(*mode, AppMode::Paused))
}

fn frame_body(frame: &[u8], framing: Framing) -> Result<Value, String> {
    let body = match framing {
        Framing::JsonLines => trim_ascii(frame),
        Framing::ContentLength => {
            let header_end = find_bytes(frame, b"\r\n\r\n")
                .ok_or_else(|| "Missing Content-Length header".to_string())?;
            &frame[header_end + 4..]
        }
    };
    serde_json::from_slice(body).map_err(|error| error.to_string())
}

fn blocked_response(body: &Value, message: &str) -> Option<Value> {
    fn response(request: &Value, message: &str) -> Option<Value> {
        let id = request.get("id")?;
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32603,
                "message": message,
            }
        }))
    }

    let Value::Array(requests) = body else {
        return response(body, message);
    };
    let responses = requests
        .iter()
        .filter_map(|request| response(request, message))
        .collect::<Vec<_>>();
    (!responses.is_empty()).then_some(Value::Array(responses))
}

#[allow(dead_code)]
async fn forward_frames<Reader, Writer>(
    mut reader: Reader,
    mut writer: Writer,
    framing: Framing,
    transport: TransportType,
    message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
) -> Result<(), String>
where
    Reader: AsyncRead + Unpin,
    Writer: AsyncWrite + Unpin,
{
    let mut framer = Some(Framer::new(framing));
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader
            .read(&mut chunk)
            .await
            .map_err(|error| error.to_string())?;
        if count == 0 {
            writer.shutdown().await.map_err(|error| error.to_string())?;
            return Ok(());
        }

        writer
            .write_all(&chunk[..count])
            .await
            .map_err(|error| error.to_string())?;
        writer.flush().await.map_err(|error| error.to_string())?;
        let Some(decoder) = &mut framer else {
            continue;
        };
        match decoder.decode(&chunk[..count]) {
            Ok(bodies) => {
                for body in bodies {
                    for message in json_rpc_messages_by_shape(&body, transport, None) {
                        let _ = message_sender.send(message);
                    }
                }
            }
            Err(error) => {
                eprintln!(
                    "jsonrpc-debugger: stopped decoding {}: {error}",
                    transport.label()
                );
                framer = None;
            }
        }
    }
}

#[derive(Clone)]
pub struct StreamTransport {
    inner: Arc<StreamTransportInner>,
}

struct StreamTransportInner {
    commands: mpsc::UnboundedSender<StreamCommand>,
    task: JoinHandle<()>,
}

impl Drop for StreamTransportInner {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum StreamCommand {
    Send {
        message: Value,
        reply: oneshot::Sender<Result<Value, String>>,
    },
}

enum Incoming {
    Message(Value),
    Closed,
    Error(String),
}

struct PendingCall {
    batch: bool,
    deadline: Instant,
    remaining: usize,
    responses: Vec<Value>,
    reply: Option<oneshot::Sender<Result<Value, String>>>,
}

impl StreamTransport {
    pub fn new<R, W>(
        reader: R,
        writer: W,
        framing: Framing,
        transport: TransportType,
        message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
        request_timeout: Duration,
    ) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (command_sender, command_receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_stream(
            reader,
            writer,
            framing,
            transport,
            message_sender,
            command_receiver,
            request_timeout,
        ));
        Self {
            inner: Arc::new(StreamTransportInner {
                commands: command_sender,
                task,
            }),
        }
    }

    pub async fn send(&self, message: Value) -> Result<Value, String> {
        let (reply, response) = oneshot::channel();
        self.inner
            .commands
            .send(StreamCommand::Send { message, reply })
            .map_err(|_| "stdio stream is closed".to_string())?;
        response
            .await
            .map_err(|_| "stdio stream is closed".to_string())?
    }
}

async fn run_stream<R, W>(
    reader: R,
    mut writer: W,
    framing: Framing,
    transport: TransportType,
    message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
    mut commands: mpsc::UnboundedReceiver<StreamCommand>,
    request_timeout: Duration,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (incoming_sender, mut incoming) = mpsc::unbounded_channel();
    let reader = tokio::spawn(read_stream(reader, framing, incoming_sender));
    let encoder = Framer::new(framing);
    let mut next_call_id = 0_u64;
    let mut call_by_rpc_id = HashMap::<String, u64>::new();
    let mut calls = HashMap::<u64, PendingCall>::new();

    loop {
        let deadline = calls.values().map(|call| call.deadline).min();
        let wait_for_timeout = async move {
            match deadline {
                Some(deadline) => sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            command = commands.recv() => {
                let Some(StreamCommand::Send { message, reply }) = command else {
                    break;
                };
                let frame = match encoder.encode(&message) {
                    Ok(frame) => frame,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        continue;
                    }
                };
                let ids = match request_ids(&message, &call_by_rpc_id) {
                    Ok(ids) => ids,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        continue;
                    }
                };
                if let Err(error) = writer.write_all(&frame).await {
                    let _ = reply.send(Err(error.to_string()));
                    continue;
                }
                if let Err(error) = writer.flush().await {
                    let _ = reply.send(Err(error.to_string()));
                    continue;
                }
                if ids.is_empty() {
                    let _ = reply.send(Ok(Value::Null));
                    continue;
                }

                let call_id = next_call_id;
                next_call_id = next_call_id.wrapping_add(1);
                for id in &ids {
                    call_by_rpc_id.insert(id.clone(), call_id);
                }
                calls.insert(call_id, PendingCall {
                    batch: message.is_array(),
                    deadline: Instant::now() + request_timeout,
                    remaining: ids.len(),
                    responses: Vec::with_capacity(ids.len()),
                    reply: Some(reply),
                });
            }
            event = incoming.recv() => match event {
                Some(Incoming::Message(message)) => {
                    for rpc_message in incoming_json_rpc_messages(&message, transport) {
                        let _ = message_sender.send(rpc_message);
                    }
                    resolve_responses(&message, &mut call_by_rpc_id, &mut calls);
                }
                Some(Incoming::Error(error)) => {
                    fail_pending(&mut calls, error);
                    break;
                }
                Some(Incoming::Closed) | None => {
                    fail_pending(&mut calls, "stdio stream closed".to_string());
                    break;
                }
            },
            _ = wait_for_timeout => {
                expire_pending(&mut call_by_rpc_id, &mut calls, request_timeout);
            }
        }
    }

    reader.abort();
}

fn expire_pending(
    call_by_rpc_id: &mut HashMap<String, u64>,
    calls: &mut HashMap<u64, PendingCall>,
    request_timeout: Duration,
) {
    let now = Instant::now();
    let expired = calls
        .iter()
        .filter_map(|(id, call)| (call.deadline <= now).then_some(*id))
        .collect::<Vec<_>>();
    for call_id in expired {
        call_by_rpc_id.retain(|_, pending_call_id| *pending_call_id != call_id);
        let Some(mut call) = calls.remove(&call_id) else {
            continue;
        };
        if let Some(reply) = call.reply.take() {
            let _ = reply.send(Err(format!(
                "stdio request timed out after {request_timeout:?}"
            )));
        }
    }
}

async fn read_stream<R>(mut reader: R, framing: Framing, incoming: mpsc::UnboundedSender<Incoming>)
where
    R: AsyncRead + Unpin,
{
    let mut framer = Framer::new(framing);
    let mut chunk = [0_u8; 8192];
    loop {
        let count = match reader.read(&mut chunk).await {
            Ok(0) => {
                let _ = incoming.send(Incoming::Closed);
                return;
            }
            Ok(count) => count,
            Err(error) => {
                let _ = incoming.send(Incoming::Error(error.to_string()));
                return;
            }
        };
        match framer.decode(&chunk[..count]) {
            Ok(messages) => {
                for message in messages {
                    if incoming.send(Incoming::Message(message)).is_err() {
                        return;
                    }
                }
            }
            Err(error) => {
                let _ = incoming.send(Incoming::Error(error));
                return;
            }
        }
    }
}

fn request_ids(message: &Value, pending: &HashMap<String, u64>) -> Result<Vec<String>, String> {
    let messages = message
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(message));
    let ids = messages
        .iter()
        .filter(|message| message.get("method").is_some())
        .filter_map(|message| message.get("id"))
        .map(Value::to_string)
        .collect::<Vec<_>>();
    let unique = ids.iter().collect::<HashSet<_>>();
    if unique.len() != ids.len() {
        return Err("request IDs must be unique within a batch".to_string());
    }
    if let Some(id) = ids.iter().find(|id| pending.contains_key(*id)) {
        return Err(format!("request ID is already pending: {id}"));
    }
    Ok(ids)
}

fn resolve_responses(
    message: &Value,
    call_by_rpc_id: &mut HashMap<String, u64>,
    calls: &mut HashMap<u64, PendingCall>,
) {
    let responses = message
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(message));
    for response in responses {
        if response.get("method").is_some() {
            continue;
        }
        let Some(id) = response.get("id").map(Value::to_string) else {
            continue;
        };
        let Some(call_id) = call_by_rpc_id.remove(&id) else {
            continue;
        };
        let Some(call) = calls.get_mut(&call_id) else {
            continue;
        };
        call.responses.push(response.clone());
        call.remaining -= 1;
        if call.remaining != 0 {
            continue;
        }

        let mut call = calls.remove(&call_id).expect("pending call exists");
        let response = if call.batch {
            Value::Array(call.responses)
        } else {
            call.responses.pop().expect("single response exists")
        };
        if let Some(reply) = call.reply.take() {
            let _ = reply.send(Ok(response));
        }
    }
}

fn fail_pending(calls: &mut HashMap<u64, PendingCall>, error: String) {
    for call in calls.values_mut() {
        if let Some(reply) = call.reply.take() {
            let _ = reply.send(Err(error.clone()));
        }
    }
    calls.clear();
}

#[derive(Clone)]
pub struct StdioTransport {
    stream: StreamTransport,
    _process: Arc<ChildProcess>,
}

struct ChildProcess {
    wait: JoinHandle<()>,
    stderr: JoinHandle<()>,
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        self.wait.abort();
        self.stderr.abort();
    }
}

impl StdioTransport {
    pub fn spawn(
        command: &[OsString],
        framing: Framing,
        message_sender: mpsc::UnboundedSender<JsonRpcMessage>,
        request_timeout: Duration,
    ) -> Result<Self, String> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| "stdio command cannot be empty".to_string())?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("failed to start {}: {error}", program.to_string_lossy()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open child stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "failed to open child stdout".to_string())?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| "failed to open child stderr".to_string())?;
        let stderr = tokio::spawn(async move {
            let _ = io::copy(&mut stderr, &mut io::sink()).await;
        });
        let wait = tokio::spawn(async move {
            let _ = child.wait().await;
        });
        let stream = StreamTransport::new(
            stdout,
            stdin,
            framing,
            TransportType::Stdio(framing),
            message_sender,
            request_timeout,
        );

        Ok(Self {
            stream,
            _process: Arc::new(ChildProcess { wait, stderr }),
        })
    }

    pub async fn send(&self, message: Value) -> Result<Value, String> {
        self.stream.send(message).await
    }
}

pub fn display_command(command: &[OsString]) -> String {
    command
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}
