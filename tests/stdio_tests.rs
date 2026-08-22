use jsonrpc_debugger::{
    app::TransportType,
    stdio::{relay, Framer, Framing, StreamTransport},
};
use serde_json::json;
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
