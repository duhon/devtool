use axum::Router;
use serde_json::json;
use services_native::adapters::{
    framing::{FrameReader, encode},
    native,
};
use services_testkit::{
    DataDir,
    http::{self, Server},
    native::read_frame,
};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

#[test]
fn frames_accept_fragments_and_unicode_and_drop_malformed_json() {
    let a = json!({"id":"α","params":{"body":"🦀"}});
    let mut bytes = encode(&a).unwrap();
    bytes.extend([1, 0, 0, 0, b'!']);
    bytes.extend(encode(&json!({"id":2})).unwrap());
    let mut reader = FrameReader::default();
    let mut messages = vec![];
    for byte in bytes {
        messages.extend(reader.feed(&[byte]).unwrap());
    }
    assert_eq!(messages, vec![a, json!({"id":2})]);
    assert!(
        FrameReader::default()
            .feed(&u32::MAX.to_le_bytes())
            .is_err()
    );
}

#[tokio::test]
async fn clipboard_is_reachable_only_through_the_native_adapter() {
    let data = DataDir::new();
    let reply = data
        .runtime()
        .dispatch(&json!({"id":1,"type":"clipboardRead"}))
        .await;
    assert_eq!(reply["ok"], false);
    assert!(reply["error"].as_str().unwrap().contains("Unknown service"));
    assert!(reply.get("result").is_none());
}

#[tokio::test]
async fn large_responses_are_chunked_without_stopping_terminal_and_old_clients_are_told_to_update()
{
    use services_contracts::native_messaging::ResponseAssembler;
    let data = DataDir::new();
    let body = format!("head{}tail", "🦀\"\\\n".repeat(180_000));
    let response = body.clone();
    let server = Server::new(Router::new().route(
        "/large",
        axum::routing::get(move || {
            let response = response.clone();
            async move { response }
        }),
    ))
    .await;
    let (mut input, input_host) = tokio::io::duplex(65536);
    let (output_host, mut output) = tokio::io::duplex(65536);
    let host = tokio::spawn(native::run(data.runtime(), input_host, output_host));
    let roundtrip = async {
        input.write_all(&encode(&json!({"id":1,"service":"terminal","params":{"endpoint":"sh -c","command":"printf ready; sleep 30"}})).unwrap()).await.unwrap();
        assert_eq!(read_frame(&mut output).await["event"]["data"], "ready");
        let request = |id| json!({"id":id,"service":"rest","params":{"method":"GET","url":format!("{}/large",server.url)}});
        input
            .write_all(&encode(&request(2)).unwrap())
            .await
            .unwrap();
        let error = read_frame(&mut output).await;
        assert_eq!(error["id"], 2);
        assert_eq!(error["ok"], false);
        assert!(
            error["error"]
                .as_str()
                .unwrap()
                .contains("Update the extension")
        );
        input
            .write_all(
                &encode(&json!({"id":3,"type":"health","capabilities":{"chunkedResponses":true}}))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(read_frame(&mut output).await["id"], 3);
        input
            .write_all(&encode(&request(4)).unwrap())
            .await
            .unwrap();
        input
            .write_all(&encode(&json!({"id":5,"type":"health"})).unwrap())
            .await
            .unwrap();
        let mut assembler = ResponseAssembler::default();
        let mut result = None;
        let mut healthy = false;
        let mut chunks = 0;
        while result.is_none() || !healthy {
            let message = read_frame(&mut output).await;
            if message["id"] == 4 {
                assert_eq!(message["type"], "nativeChunk");
                chunks += 1;
                if let Some(value) = assembler.push(&message).unwrap() {
                    result = Some(value);
                }
            } else {
                assert_eq!(message["id"], 5, "Unrelated Terminal must not exit");
                assert_eq!(message["result"]["protocolVersion"], 1);
                healthy = true;
            }
        }
        assert!(chunks > 1);
        assert_eq!(result.unwrap()["result"]["body"], body);
        input
            .write_all(&encode(&json!({"id":1,"type":"stop"})).unwrap())
            .await
            .unwrap();
        let exit = read_frame(&mut output).await;
        assert_eq!(exit["id"], 1);
        assert_eq!(exit["type"], "exit");
        assert_eq!(exit["info"]["cancelled"], true);
        input.shutdown().await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(10), roundtrip)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(4), host)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn writer_failure_ends_the_host_without_waiting_for_input() {
    let data = DataDir::new();
    let (mut input, input_host) = tokio::io::duplex(65536);
    let (output_host, output) = tokio::io::duplex(65536);
    drop(output);
    let host = tokio::spawn(native::run(data.runtime(), input_host, output_host));
    input
        .write_all(&encode(&json!({"id":1,"type":"health"})).unwrap())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(4), host)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
async fn concurrent_requests_stop_and_health_are_routed_by_id() {
    let data = DataDir::new();
    let (server, _) = http::services().await;
    let (mut input, input_host) = tokio::io::duplex(65536);
    let (output_host, mut output) = tokio::io::duplex(65536);
    let host = tokio::spawn(native::run(data.runtime(), input_host, output_host));
    let slow = json!({"id":"slow","service":"rest","params":{"method":"POST","url":format!("{}/slow",server.url),"body":"body"}});
    input.write_all(&encode(&slow).unwrap()).await.unwrap();
    let health = encode(&json!({"id":"health","type":"health"})).unwrap();
    for bytes in health.chunks(2) {
        input.write_all(bytes).await.unwrap();
    }
    let first = read_frame(&mut output).await;
    assert_eq!(first["id"], "health");
    assert_eq!(first["result"]["protocolVersion"], 1);
    let slow = read_frame(&mut output).await;
    assert_eq!(slow["id"], "slow");
    assert_eq!(slow["result"]["status"], 201);
    input.write_all(&encode(&json!({"id":3,"service":"terminal","params":{"endpoint":"sh -c","command":"printf ready; sleep 30"}})).unwrap()).await.unwrap();
    let data = read_frame(&mut output).await;
    assert_eq!(data["type"], "data");
    assert_eq!(data["event"]["data"], "ready");
    input
        .write_all(&encode(&json!({"id":3,"type":"stop"})).unwrap())
        .await
        .unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(4), read_frame(&mut output))
        .await
        .unwrap();
    assert_eq!(exit["type"], "exit");
    assert_eq!(exit["info"]["signal"], "SIGTERM");
    input
        .write_all(&encode(&json!({"id":4,"service":"vars","op":"list"})).unwrap())
        .await
        .unwrap();
    assert!(read_frame(&mut output).await["result"]["profiles"]["var"].is_object());
    input
        .write_all(&encode(&json!({"id":5,"type":"health"})).unwrap())
        .await
        .unwrap();
    input.shutdown().await.unwrap();
    assert_eq!(read_frame(&mut output).await["id"], 5);
    host.await.unwrap().unwrap();
}

#[tokio::test]
async fn eof_cancels_terminal_and_flushes_its_exit() {
    let data = DataDir::new();
    let (mut input, input_host) = tokio::io::duplex(65536);
    let (output_host, mut output) = tokio::io::duplex(65536);
    let host = tokio::spawn(native::run(data.runtime(), input_host, output_host));
    input.write_all(&encode(&json!({"id":1,"service":"terminal","params":{"endpoint":"sh -c","command":"printf ready; sleep 30"}})).unwrap()).await.unwrap();
    assert_eq!(read_frame(&mut output).await["type"], "data");
    input.shutdown().await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(4), read_frame(&mut output))
            .await
            .unwrap()["type"],
        "exit"
    );
    host.await.unwrap().unwrap();
}

#[tokio::test]
async fn stop_and_eof_cancel_requests_without_touching_siblings() {
    let data = DataDir::new();
    let (server, seen) = http::services().await;
    let (mut input, input_host) = tokio::io::duplex(65536);
    let (output_host, mut output) = tokio::io::duplex(65536);
    let host = tokio::spawn(native::run(data.runtime(), input_host, output_host));
    for (id, path) in [(1, "cancel-slow"), (2, "slow")] {
        input.write_all(&encode(&json!({"id":id,"service":"rest","params":{"method":"POST","url":format!("{}/{path}",server.url),"body":"fixture"}})).unwrap()).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while seen.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    input
        .write_all(&encode(&json!({"id":1,"type":"stop"})).unwrap())
        .await
        .unwrap();
    let mut replies = vec![];
    for _ in 0..2 {
        replies.push(
            tokio::time::timeout(Duration::from_secs(2), read_frame(&mut output))
                .await
                .unwrap(),
        );
    }
    assert_eq!(
        replies.iter().find(|reply| reply["id"] == 1).unwrap()["result"]["cancelled"],
        true
    );
    assert_eq!(
        replies.iter().find(|reply| reply["id"] == 2).unwrap()["result"]["status"],
        201
    );
    for id in [3, 4] {
        input.write_all(&encode(&json!({"id":id,"service":"rest","params":{"method":"GET","url":format!("{}/cancel-slow",server.url)}})).unwrap()).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while seen.lock().unwrap().len() < 4 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    input.shutdown().await.unwrap();
    for _ in 0..2 {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), read_frame(&mut output))
                .await
                .unwrap()["result"]["cancelled"],
            true
        );
    }
    tokio::time::timeout(Duration::from_secs(4), host)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
