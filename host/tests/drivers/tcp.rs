use serde_json::{Value, json};
use services_sdk::tcp::{self, Closed, Event};
use services_testkit::{DataDir, native::Pipe};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn call(id: u32, op: &str, request: &tcp::Request) -> Value {
    super::call("tcp", id, op, json!(request))
}

/// Echoes everything back, says `bye` at end of input and closes.
async fn echo() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buffer = [0; 1024];
                loop {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => stream.write_all(&buffer[..n]).await.unwrap(),
                    }
                }
                let _ = stream.write_all(b"bye").await;
            });
        }
    });
    (port, task)
}

fn request(port: u16, data: &str) -> tcp::Request {
    let mut request = tcp::Request::new("127.0.0.1", port);
    request.data = Some(data.into());
    request
}

#[tokio::test]
async fn tcp_call_sends_data_and_reads_until_close_idle_or_limit() {
    let (data, (port, _server)) = (DataDir::new(), echo().await);
    let runtime = data.runtime();
    let mut closing = request(port, "ping");
    closing.close_write = true;
    let reply = runtime.dispatch(&call(1, "call", &closing)).await;
    assert_eq!(reply["ok"], true, "{reply}");
    let response = tcp::Response::parse(&reply["result"]).unwrap();
    assert_eq!(response.data.as_deref(), Some("pingbye"));
    assert_eq!((response.bytes, response.closed), (7, Closed::Peer));
    assert_eq!(response.remote.unwrap().address, "127.0.0.1");
    assert!(response.timing.total_us >= response.timing.connect_us);

    let mut idle = request(port, "ping");
    idle.idle_ms = Some(200);
    let reply = runtime.dispatch(&call(2, "call", &idle)).await;
    let response = tcp::Response::parse(&reply["result"]).unwrap();
    assert_eq!(
        (response.data.as_deref(), response.closed),
        (Some("ping"), Closed::Idle)
    );

    let mut limited = request(port, "ping");
    limited.max_bytes = Some(2);
    limited.text = false;
    let reply = runtime.dispatch(&call(3, "call", &limited)).await;
    let response = tcp::Response::parse(&reply["result"]).unwrap();
    assert_eq!(response.data_base64.as_deref(), Some("cGk="));
    assert_eq!((response.bytes, response.closed), (2, Closed::Limit));
}

#[tokio::test]
async fn tcp_stream_sends_connected_then_chunks_and_stops_on_cancel() {
    let (data, (port, _server)) = (DataDir::new(), echo().await);
    let mut pipe = Pipe::start(data.runtime());
    let mut closing = request(port, "ping");
    closing.close_write = true;
    pipe.send(&call(1, "stream", &closing)).await;
    let (events, exit) = super::events(&mut pipe).await;
    let events: Vec<Event> = events.iter().map(|e| Event::parse(e).unwrap()).collect();
    assert!(matches!(&events[0], Event::Connected { remote: Some(_) }));
    let text: String = events[1..]
        .iter()
        .map(|event| match event {
            Event::Chunk(chunk) => chunk.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(text, "pingbye");
    let response = tcp::Response::parse(&exit["info"]).unwrap();
    assert_eq!((response.bytes, response.data), (7, None));

    pipe.send(&call(2, "stream", &request(port, "ping"))).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    pipe.send(&json!({"id": 2, "type": "stop"})).await;
    let (_, reply) = super::events(&mut pipe).await;
    assert_eq!(reply["failure"]["stage"], "cancelled", "{reply}");
}

#[tokio::test]
async fn tcp_failures_name_their_stage() {
    let (data, (port, _server)) = (DataDir::new(), echo().await);
    let runtime = data.runtime();
    let reply = runtime.dispatch(&call(1, "call", &request(1, "x"))).await;
    assert_eq!(reply["failure"]["stage"], "connect", "{reply}");

    let mut tls = request(port, "x");
    tls.tls = true;
    tls.insecure = true;
    tls.timeout_ms = Some(2000);
    let reply = runtime.dispatch(&call(2, "call", &tls)).await;
    assert_eq!(reply["failure"]["stage"], "tls", "{reply}");

    let mut slow = request(port, "x");
    slow.timeout_ms = Some(200);
    let reply = runtime.dispatch(&call(3, "call", &slow)).await;
    assert_eq!(reply["failure"]["stage"], "timeout", "{reply}");

    let reply = runtime
        .dispatch(&call(
            4,
            "call",
            &tcp::Request::new("no-such-host.invalid", 80),
        ))
        .await;
    assert_eq!(reply["failure"]["stage"], "dns", "{reply}");
}
