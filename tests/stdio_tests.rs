use jsonrpc_debugger::{
    app::{AppMode, ProxyDecision, TransportType},
    proxy::ProxyState,
    stdio::{
        relay, relay_with_interception, Framer, Framing, StreamTransport, DEFAULT_REQUEST_TIMEOUT,
    },
};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{split, AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

#[test]
fn json_lines_handles_split_and_multiple_frames() {
    let mut framer = Framer::new(Framing::JsonLines);
    let first = framer.encode(&json!({"jsonrpc": "2.0", "id": 1})).unwrap();
    let second = framer.encode(&json!({"jsonrpc": "2.0", "id": 2})).unwrap();
    let split = first.len() - 2;

    assert!(framer.decode(&first[..split]).unwrap().is_empty());

    let decoded = framer
        .decode(&[&first[split..], second.as_slice()].concat())
        .unwrap();
    assert_eq!(
        decoded,
        vec![
            json!({"jsonrpc": "2.0", "id": 1}),
            json!({"jsonrpc": "2.0", "id": 2})
        ]
    );
}

#[test]
fn content_length_uses_utf8_bytes_and_handles_split_frames() {
    let mut framer = Framer::new(Framing::ContentLength);
    let message = json!({"jsonrpc": "2.0", "id": 1, "result": "hello 世界"});
    let encoded = framer.encode(&message).unwrap();
    let body = serde_json::to_vec(&message).unwrap();

    assert!(encoded.starts_with(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes()));
    for chunk in encoded.chunks(3).take(encoded.chunks(3).len() - 1) {
        assert!(framer.decode(chunk).unwrap().is_empty());
    }
    let consumed = encoded
        .chunks(3)
        .take(encoded.chunks(3).len() - 1)
        .map(<[u8]>::len)
        .sum::<usize>();
    assert_eq!(framer.decode(&encoded[consumed..]).unwrap(), vec![message]);
}

#[tokio::test]
async fn stream_transport_correlates_responses_and_records_notifications() {
    let (client_stream, server_stream) = tokio::io::duplex(4096);
    let (client_reader, client_writer) = split(client_stream);
    let (mut server_reader, mut server_writer) = split(server_stream);
    let (message_sender, mut message_receiver) = mpsc::unbounded_channel();
    let transport = StreamTransport::new(
        client_reader,
        client_writer,
        Framing::JsonLines,
        TransportType::Stdio(Framing::JsonLines),
        message_sender,
        DEFAULT_REQUEST_TIMEOUT,
    );

    tokio::spawn(async move {
        let mut decoder = Framer::new(Framing::JsonLines);
        let encoder = Framer::new(Framing::JsonLines);
        let mut bytes = [0; 1024];
        let request = loop {
            let count = server_reader.read(&mut bytes).await.unwrap();
            let mut messages = decoder.decode(&bytes[..count]).unwrap();
            if let Some(message) = messages.pop() {
                break message;
            }
        };
        let notification = encoder
            .encode(&json!({"jsonrpc": "2.0", "method": "example/changed"}))
            .unwrap();
        let response = encoder
            .encode(&json!({"jsonrpc": "2.0", "id": request["id"], "result": "ok"}))
            .unwrap();
        server_writer.write_all(&notification).await.unwrap();
        server_writer.write_all(&response).await.unwrap();
    });

    let response = transport
        .send(json!({"jsonrpc": "2.0", "id": 7, "method": "example/run"}))
        .await
        .unwrap();
    assert_eq!(response["result"], "ok");

    let notification = message_receiver.recv().await.unwrap();
    assert_eq!(notification.method.as_deref(), Some("example/changed"));
    assert!(notification.id.is_none());
    let response = message_receiver.recv().await.unwrap();
    assert_eq!(response.id, Some(json!(7)));
}

#[tokio::test]
async fn stream_transport_times_out_unanswered_requests_and_releases_ids() {
    let (client_stream, server_stream) = tokio::io::duplex(4096);
    let (client_reader, client_writer) = split(client_stream);
    let (mut server_reader, mut server_writer) = split(server_stream);
    let (message_sender, _message_receiver) = mpsc::unbounded_channel();
    let transport = StreamTransport::new(
        client_reader,
        client_writer,
        Framing::JsonLines,
        TransportType::Stdio(Framing::JsonLines),
        message_sender,
        Duration::from_millis(10),
    );
    let request = json!({"jsonrpc": "2.0", "id": 7, "method": "example/run"});

    assert_eq!(
        transport.send(request.clone()).await.unwrap_err(),
        "stdio request timed out after 10ms"
    );

    let mut bytes = [0; 1024];
    let count = server_reader.read(&mut bytes).await.unwrap();
    assert_eq!(
        Framer::new(Framing::JsonLines)
            .decode(&bytes[..count])
            .unwrap(),
        vec![request.clone()]
    );

    let server = async move {
        let count = server_reader.read(&mut bytes).await.unwrap();
        let second_request = Framer::new(Framing::JsonLines)
            .decode(&bytes[..count])
            .unwrap()
            .pop()
            .unwrap();
        let response = Framer::new(Framing::JsonLines)
            .encode(&json!({"jsonrpc": "2.0", "id": second_request["id"], "result": "ok"}))
            .unwrap();
        server_writer.write_all(&response).await.unwrap();
    };
    let (response, ()) = tokio::join!(transport.send(request), server);

    assert_eq!(response.unwrap()["result"], "ok");
}

#[tokio::test]
async fn stream_transport_writes_client_responses_without_waiting() {
    let (client_stream, server_stream) = tokio::io::duplex(4096);
    let (client_reader, client_writer) = split(client_stream);
    let (mut server_reader, _server_writer) = split(server_stream);
    let (message_sender, _message_receiver) = mpsc::unbounded_channel();
    let transport = StreamTransport::new(
        client_reader,
        client_writer,
        Framing::JsonLines,
        TransportType::Stdio(Framing::JsonLines),
        message_sender,
        DEFAULT_REQUEST_TIMEOUT,
    );

    let response = json!({"jsonrpc": "2.0", "id": 9, "result": {}});
    assert_eq!(transport.send(response.clone()).await.unwrap(), json!(null));

    let mut bytes = [0; 1024];
    let count = server_reader.read(&mut bytes).await.unwrap();
    assert_eq!(
        Framer::new(Framing::JsonLines)
            .decode(&bytes[..count])
            .unwrap(),
        vec![response]
    );
}

#[tokio::test]
async fn content_length_stream_supports_batch_responses() {
    let (client_stream, server_stream) = tokio::io::duplex(4096);
    let (client_reader, client_writer) = split(client_stream);
    let (mut server_reader, mut server_writer) = split(server_stream);
    let (message_sender, _message_receiver) = mpsc::unbounded_channel();
    let transport = StreamTransport::new(
        client_reader,
        client_writer,
        Framing::ContentLength,
        TransportType::Stdio(Framing::ContentLength),
        message_sender,
        DEFAULT_REQUEST_TIMEOUT,
    );

    tokio::spawn(async move {
        let mut decoder = Framer::new(Framing::ContentLength);
        let encoder = Framer::new(Framing::ContentLength);
        let mut bytes = [0; 1024];
        loop {
            let count = server_reader.read(&mut bytes).await.unwrap();
            if !decoder.decode(&bytes[..count]).unwrap().is_empty() {
                break;
            }
        }
        server_writer
            .write_all(
                &encoder
                    .encode(&json!([
                        {"jsonrpc": "2.0", "id": 2, "result": "second"},
                        {"jsonrpc": "2.0", "id": 1, "result": "first"}
                    ]))
                    .unwrap(),
            )
            .await
            .unwrap();
    });

    let response = transport
        .send(json!([
            {"jsonrpc": "2.0", "id": 1, "method": "example/first"},
            {"jsonrpc": "2.0", "id": 2, "method": "example/second"}
        ]))
        .await
        .unwrap();
    assert_eq!(response[0]["id"], 2);
    assert_eq!(response[1]["id"], 1);
}

#[tokio::test]
async fn transparent_json_lines_relay_preserves_bytes_both_ways() {
    let request = b"  {\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"example/run\"}  \r\n";
    let response = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":\"ok\"}\n";
    let messages = transparent_round_trip(Framing::JsonLines, request, response).await;

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].method.as_deref(), Some("example/run"));
    assert_eq!(messages[1].result, Some(json!("ok")));
}

#[tokio::test]
async fn transparent_content_length_relay_preserves_batches() {
    let encoder = Framer::new(Framing::ContentLength);
    let request = encoder
        .encode(&json!([
            {"jsonrpc": "2.0", "id": 1, "method": "example/first"},
            {"jsonrpc": "2.0", "id": 2, "method": "example/second"}
        ]))
        .unwrap();
    let response = encoder
        .encode(&json!([
            {"jsonrpc": "2.0", "id": 2, "result": "second"},
            {"jsonrpc": "2.0", "id": 1, "result": "first"}
        ]))
        .unwrap();
    let messages = transparent_round_trip(Framing::ContentLength, &request, &response).await;

    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].method.as_deref(), Some("example/first"));
    assert_eq!(messages[1].method.as_deref(), Some("example/second"));
    assert_eq!(messages[2].id, Some(json!(2)));
    assert_eq!(messages[3].id, Some(json!(1)));
}

#[tokio::test]
async fn transparent_relay_forwards_bytes_it_cannot_decode() {
    let messages = transparent_round_trip(
        Framing::JsonLines,
        b"this is not JSON\n",
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":\"still alive\"}\n",
    )
    .await;

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].result, Some(json!("still alive")));
}

#[tokio::test]
async fn transparent_content_length_relay_pauses_and_blocks_requests() {
    let framing = Framing::ContentLength;
    let (client, relay_client) = tokio::io::duplex(4096);
    let (server, relay_server) = tokio::io::duplex(4096);
    let (mut client_reader, mut client_writer) = split(client);
    let (relay_client_reader, relay_client_writer) = split(relay_client);
    let (mut server_reader, mut server_writer) = split(server);
    let (relay_server_reader, relay_server_writer) = split(relay_server);
    let (message_sender, _message_receiver) = mpsc::unbounded_channel();
    let (pending_sender, mut pending_receiver) = mpsc::unbounded_channel();
    let proxy_state = ProxyState {
        app_mode: Arc::new(Mutex::new(AppMode::Paused)),
        pending_sender,
    };
    let relay = tokio::spawn(relay_with_interception(
        relay_client_reader,
        relay_client_writer,
        relay_server_reader,
        relay_server_writer,
        framing,
        message_sender,
        proxy_state,
    ));
    let encoder = Framer::new(framing);

    let request = json!({"jsonrpc": "2.0", "id": "allow-1", "method": "example/run"});
    let request_frame = encoder.encode(&request).unwrap();
    client_writer.write_all(&request_frame).await.unwrap();
    let pending = tokio::time::timeout(Duration::from_secs(1), pending_receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.original_body, request);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), server_reader.read_u8())
            .await
            .is_err()
    );
    pending
        .decision_sender
        .send(ProxyDecision::Allow(None, None))
        .unwrap();
    let mut forwarded = vec![0; request_frame.len()];
    server_reader.read_exact(&mut forwarded).await.unwrap();
    assert_eq!(forwarded, request_frame);

    let request = json!({"jsonrpc": "2.0", "id": "block-1", "method": "example/block"});
    client_writer
        .write_all(&encoder.encode(&request).unwrap())
        .await
        .unwrap();
    let pending = tokio::time::timeout(Duration::from_secs(1), pending_receiver.recv())
        .await
        .unwrap()
        .unwrap();
    pending.decision_sender.send(ProxyDecision::Block).unwrap();
    let mut response = [0; 4096];
    let count = tokio::time::timeout(Duration::from_secs(1), client_reader.read(&mut response))
        .await
        .unwrap()
        .unwrap();
    let mut decoder = Framer::new(framing);
    let blocked = decoder.decode(&response[..count]).unwrap().remove(0);
    assert_eq!(blocked["id"], "block-1");
    assert_eq!(blocked["error"]["code"], -32603);

    client_writer.shutdown().await.unwrap();
    server_writer.shutdown().await.unwrap();
    relay.await.unwrap().unwrap();
}

async fn transparent_round_trip(
    framing: Framing,
    request: &[u8],
    response: &[u8],
) -> Vec<jsonrpc_debugger::app::JsonRpcMessage> {
    let (client, relay_client) = tokio::io::duplex(4096);
    let (server, relay_server) = tokio::io::duplex(4096);
    let (mut client_reader, mut client_writer) = split(client);
    let (relay_client_reader, relay_client_writer) = split(relay_client);
    let (mut server_reader, mut server_writer) = split(server);
    let (relay_server_reader, relay_server_writer) = split(relay_server);
    let (message_sender, mut message_receiver) = mpsc::unbounded_channel();
    let relay = tokio::spawn(relay(
        relay_client_reader,
        relay_client_writer,
        relay_server_reader,
        relay_server_writer,
        framing,
        message_sender,
    ));

    client_writer.write_all(request).await.unwrap();
    let mut forwarded_request = vec![0; request.len()];
    server_reader
        .read_exact(&mut forwarded_request)
        .await
        .unwrap();
    assert_eq!(forwarded_request, request);

    server_writer.write_all(response).await.unwrap();
    let mut forwarded_response = vec![0; response.len()];
    client_reader
        .read_exact(&mut forwarded_response)
        .await
        .unwrap();
    assert_eq!(forwarded_response, response);

    server_writer.shutdown().await.unwrap();
    client_writer.shutdown().await.unwrap();
    relay.await.unwrap().unwrap();

    let mut messages = Vec::new();
    while let Ok(message) = message_receiver.try_recv() {
        messages.push(message);
    }
    messages
}
