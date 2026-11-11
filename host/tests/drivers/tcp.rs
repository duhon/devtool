use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use services_sdk::tcp::{self, Closed, Event};
use services_testkit::{DataDir, native::Pipe};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
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

fn session(port: u16) -> tcp::Request {
    tcp::Request::session("127.0.0.1", port)
}

fn input(id: u32, data: &[u8]) -> Value {
    json!({"id": id, "type": "input", "dataBase64": B64.encode(data)})
}

async fn next(pipe: &mut Pipe) -> Value {
    tokio::time::timeout(Duration::from_secs(10), pipe.read())
        .await
        .expect("a message")
}

async fn event(pipe: &mut Pipe) -> Event {
    let message = next(pipe).await;
    assert_eq!(message["type"], "data", "{message}");
    Event::parse(&message["event"]).unwrap()
}

/// Reads `chunkBase64` events until `len` bytes came.
async fn read(pipe: &mut Pipe, len: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    while bytes.len() < len {
        match event(pipe).await {
            Event::ChunkBase64(chunk) => bytes.extend(B64.decode(chunk).unwrap()),
            other => panic!("{other:?}"),
        }
    }
    bytes
}

/// Accepts one connection; reports each read (`None` at the end of input) and answers it with
/// `answer(read)`, written in pieces of `piece` bytes with a pause between them.
async fn peer(
    answer: fn(&[u8]) -> Vec<u8>,
    piece: usize,
) -> (u16, mpsc::UnboundedReceiver<Option<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (heard, reads) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        loop {
            let n = stream.read(&mut buffer).await.unwrap_or(0);
            if n == 0 {
                let _ = heard.send(None);
                return;
            }
            let _ = heard.send(Some(buffer[..n].to_vec()));
            for part in answer(&buffer[..n]).chunks(piece) {
                stream.write_all(part).await.unwrap();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    });
    (port, reads)
}

async fn heard(reads: &mut mpsc::UnboundedReceiver<Option<Vec<u8>>>) -> Option<Vec<u8>> {
    tokio::time::timeout(Duration::from_secs(10), reads.recv())
        .await
        .expect("the peer hears")
        .expect("the peer runs")
}

fn redis(read: &[u8]) -> Vec<u8> {
    match read {
        b"AUTH bad\r\n" => b"-ERR invalid password\r\n".to_vec(),
        b"PING\r\n" => b"+PONG\r\n".to_vec(),
        _ => b"+OK\r\n".to_vec(),
    }
}

#[tokio::test]
async fn tcp_session_writes_when_asked_on_one_connection_until_cancelled() {
    let (data, (port, mut reads)) = (DataDir::new(), peer(redis, 1024).await);
    let mut pipe = Pipe::start(data.runtime());
    pipe.send(&call(1, "stream", &session(port))).await;
    assert!(matches!(
        event(&mut pipe).await,
        Event::Connected { remote: Some(_) }
    ));

    for (asked, reply) in [("PING\r\n", "+PONG\r\n"), ("SELECT 2\r\n", "+OK\r\n")] {
        pipe.send(&input(1, asked.as_bytes())).await;
        let bytes = asked.len() as u64;
        assert_eq!(event(&mut pipe).await, Event::Written { bytes });
        assert_eq!(heard(&mut reads).await.unwrap(), asked.as_bytes());
        assert_eq!(read(&mut pipe, reply.len()).await, reply.as_bytes());
    }

    pipe.send(&json!({"id": 1, "type": "stop"})).await;
    let reply = next(&mut pipe).await;
    assert_eq!(reply["failure"]["stage"], "cancelled", "{reply}");
    assert_eq!(heard(&mut reads).await, None, "cancel closes the socket");
    // Input for a stream that ended goes nowhere; the Host goes on.
    pipe.send(&input(1, b"PING\r\n")).await;
    pipe.send(&input(9, b"PING\r\n")).await;
    pipe.send(&call(2, "call", &request(1, "x"))).await;
    assert_eq!(next(&mut pipe).await["id"], 2);
}

#[tokio::test]
async fn tcp_session_writes_nothing_the_module_did_not_ask_for() {
    let (data, (port, mut reads)) = (DataDir::new(), peer(redis, 1024).await);
    let mut pipe = Pipe::start(data.runtime());
    pipe.send(&call(1, "stream", &session(port))).await;
    event(&mut pipe).await;
    pipe.send(&input(1, b"AUTH bad\r\n")).await;
    event(&mut pipe).await;
    let error = b"-ERR invalid password\r\n";
    assert_eq!(read(&mut pipe, error.len()).await, error);
    pipe.send(&json!({"id": 1, "type": "stop"})).await;
    next(&mut pipe).await;
    assert_eq!(heard(&mut reads).await.unwrap(), b"AUTH bad\r\n");
    assert_eq!(heard(&mut reads).await, None);
}

#[tokio::test]
async fn tcp_session_keeps_binary_data_split_by_the_peer_and_survives_silence() {
    let (data, (port, mut reads)) = (DataDir::new(), peer(|read| read.to_vec(), 7).await);
    let mut pipe = Pipe::start(data.runtime());
    let mut asked = session(port);
    asked.data_base64 = Some(B64.encode(b"\0first"));
    pipe.send(&call(1, "stream", &asked)).await;
    event(&mut pipe).await;
    assert_eq!(event(&mut pipe).await, Event::Written { bytes: 6 });
    assert_eq!(read(&mut pipe, 6).await, b"\0first");
    heard(&mut reads).await;

    assert!(
        tokio::time::timeout(Duration::from_millis(1500), pipe.read())
            .await
            .is_err(),
        "a silent session stays open"
    );
    let binary: Vec<u8> = (0..=255).collect();
    pipe.send(&input(1, &binary)).await;
    event(&mut pipe).await;
    assert_eq!(read(&mut pipe, binary.len()).await, binary);
    let mut sent = Vec::new();
    while sent.len() < binary.len() {
        sent.extend(heard(&mut reads).await.unwrap());
    }
    assert_eq!(sent, binary);
    pipe.send(&json!({"id": 1, "type": "stop"})).await;
    next(&mut pipe).await;
}

#[tokio::test]
async fn tcp_session_ends_at_peer_close_and_fails_on_bad_requests_and_too_many_writes() {
    let data = DataDir::new();
    let mut pipe = Pipe::start(data.runtime());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closing = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream.write_all(b"bye").await.unwrap();
    });
    pipe.send(&call(1, "stream", &session(closing))).await;
    let (events, exit) = super::events(&mut pipe).await;
    assert_eq!(events.len(), 2, "{events:?}");
    let response = tcp::Response::parse(&exit["info"]).unwrap();
    assert_eq!((response.bytes, response.closed), (3, Closed::Peer));

    let mut timed = session(closing);
    timed.timeout_ms = Some(1000);
    pipe.send(&call(2, "stream", &timed)).await;
    let reply = next(&mut pipe).await;
    assert!(
        reply["error"].as_str().unwrap().contains("timeoutMs"),
        "{reply}"
    );
    let reply = data
        .runtime()
        .dispatch(&call(3, "call", &session(closing)))
        .await;
    assert!(
        reply["error"].as_str().unwrap().contains("is a stream"),
        "{reply}"
    );

    // A peer that does not read lets the writes pile up.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let deaf = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
        drop(stream);
    });
    pipe.send(&call(4, "stream", &session(deaf))).await;
    event(&mut pipe).await;
    let big = vec![7u8; 256 * 1024];
    for _ in 0..services_native::drivers::INPUT_QUEUE * 2 {
        pipe.send(&input(4, &big)).await;
    }
    let (_, reply) = super::events(&mut pipe).await;
    assert_eq!(reply["failure"]["stage"], "write", "{reply}");
    assert!(
        reply["error"]
            .as_str()
            .unwrap()
            .contains("writes were waiting")
    );
}

/// A server for `localhost` with a certificate of a test CA (`tls/`; made for these tests only).
async fn tls_echo() -> u16 {
    use tokio_rustls::rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    };
    let chain = CertificateDer::pem_slice_iter(include_bytes!("tls/server.pem"))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(include_bytes!("tls/server.key")).unwrap();
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(stream).await else {
                    return;
                };
                let mut buffer = [0; 1024];
                while let Ok(n @ 1..) = stream.read(&mut buffer).await {
                    let _ = stream.write_all(&buffer[..n]).await;
                }
            });
        }
    });
    port
}

#[tokio::test]
async fn tcp_tls_trusts_an_extra_ca_and_still_checks_the_name() {
    let (data, port) = (DataDir::new(), tls_echo().await);
    let runtime = data.runtime();
    let ca = include_str!("tls/ca.pem");
    let tls = |ca_pem: Option<&str>, server_name: &str| {
        let mut request = request(port, "ping");
        request.tls = true;
        request.server_name = Some(server_name.into());
        request.ca_pem = ca_pem.map(Into::into);
        request.idle_ms = Some(300);
        request
    };
    let reply = runtime
        .dispatch(&call(1, "call", &tls(Some(ca), "localhost")))
        .await;
    assert_eq!(reply["result"]["data"], "ping", "{reply}");
    for (id, request) in [
        (2, tls(None, "localhost")),
        (3, tls(Some(ca), "example.com")),
        (4, tls(Some("not a certificate"), "localhost")),
    ] {
        let reply = runtime.dispatch(&call(id, "call", &request)).await;
        assert_eq!(reply["failure"]["stage"], "tls", "{reply}");
    }
}
