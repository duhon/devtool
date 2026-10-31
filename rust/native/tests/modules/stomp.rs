use serde_json::{Value, json};
use services_testkit::Host;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
struct Frame {
    command: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Frame {
    fn new(command: &str, headers: &[(&str, &str)], body: &str) -> Self {
        Self {
            command: command.into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.into(),
        }
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = format!("{}\n", self.command);
        for (k, v) in &self.headers {
            out.push_str(&format!("{k}:{v}\n"));
        }
        out.push('\n');
        let mut out = out.into_bytes();
        out.extend(&self.body);
        out.push(0);
        out
    }
}

/// Reads one frame; `None` at EOF. Heart-beat EOLs between frames are skipped.
async fn read(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Option<Frame> {
    loop {
        while buffer.first().is_some_and(|b| *b == b'\n' || *b == b'\r') {
            buffer.remove(0);
        }
        if let Some(end) = buffer.windows(2).position(|w| w == b"\n\n") {
            let head = String::from_utf8(buffer[..end].to_vec()).unwrap();
            let mut lines = head.lines();
            let command = lines.next().unwrap().to_owned();
            let headers: Vec<(String, String)> = lines
                .map(|line| {
                    let (k, v) = line.split_once(':').unwrap();
                    (k.to_owned(), v.to_owned())
                })
                .collect();
            let start = end + 2;
            let length = headers
                .iter()
                .find(|(k, _)| k == "content-length")
                .map(|(_, v)| v.parse::<usize>().unwrap());
            let stop = match length {
                Some(length) => Some(start + length).filter(|stop| *stop < buffer.len()),
                None => buffer[start..]
                    .iter()
                    .position(|b| *b == 0)
                    .map(|p| start + p),
            };
            if let Some(stop) = stop {
                let body = buffer[start..stop].to_vec();
                buffer.drain(..=stop);
                return Some(Frame {
                    command,
                    headers,
                    body,
                });
            }
        }
        let mut chunk = [0; 4096];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buffer.extend(&chunk[..n]),
        }
    }
}

/// `(destination, body, headers)` of a stored message.
type Message = (String, Vec<u8>, Vec<(String, String)>);

#[derive(Default)]
struct State {
    /// Messages waiting for a consumer.
    queue: VecDeque<Message>,
    /// Every frame the broker received, in order.
    received: Vec<Frame>,
    next: u64,
}

/// A STOMP 1.2 broker with Artemis' `consumer-window-size:0` behavior: one
/// message in flight per subscription until it is acknowledged.
struct Broker {
    url: String,
    state: Arc<Mutex<State>>,
    delivered: Arc<Notify>,
}

impl Broker {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("stomp://app:secret@{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let delivered = Arc::new(Notify::new());
        let (shared, notify) = (state.clone(), delivered.clone());
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(connection(stream, shared.clone(), notify.clone()));
            }
        });
        Self {
            url,
            state,
            delivered,
        }
    }

    fn enqueue(&self, destination: &str, body: &str) {
        let mut state = self.state.lock().unwrap();
        state
            .queue
            .push_back((destination.into(), body.into(), vec![]));
    }

    fn received(&self, command: &str) -> Vec<Frame> {
        self.state
            .lock()
            .unwrap()
            .received
            .iter()
            .filter(|frame| frame.command == command)
            .cloned()
            .collect()
    }

    fn queued(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .queue
            .iter()
            .map(|(_, body, _)| String::from_utf8_lossy(body).into_owned())
            .collect()
    }
}

async fn connection(mut stream: TcpStream, state: Arc<Mutex<State>>, delivered: Arc<Notify>) {
    let mut buffer = vec![];
    // The subscription `(id, destination)` and the message awaiting its ACK.
    let mut subscription: Option<(String, String)> = None;
    let mut in_flight: Option<(String, Message)> = None;
    let deliver = |subscription: &Option<(String, String)>,
                   in_flight: &mut Option<(String, Message)>|
     -> Option<Frame> {
        let (id, destination) = subscription.as_ref()?;
        if in_flight.is_some() {
            return None;
        }
        let mut state = state.lock().unwrap();
        let index = state.queue.iter().position(|(d, _, _)| d == destination)?;
        let message = state.queue.remove(index).unwrap();
        state.next += 1;
        let ack = format!("ack-{}", state.next);
        let message_id = format!("ID:{}", state.next);
        let mut frame = Frame::new(
            "MESSAGE",
            &[
                ("subscription", id.as_str()),
                ("message-id", message_id.as_str()),
                ("destination", destination.as_str()),
                ("ack", ack.as_str()),
            ],
            "",
        );
        frame.headers.extend(message.2.iter().cloned());
        frame.body = message.1.clone();
        *in_flight = Some((ack, message));
        Some(frame)
    };
    while let Some(frame) = read(&mut stream, &mut buffer).await {
        state.lock().unwrap().received.push(frame.clone());
        let mut replies = vec![];
        match frame.command.as_str() {
            "CONNECT" | "STOMP" if frame.get("passcode") != Some("secret") => {
                replies.push(Frame::new(
                    "ERROR",
                    &[(
                        "message",
                        "Security Error occurred: User name [app] or password is invalid",
                    )],
                    "AMQ229031: Unable to validate user",
                ));
            }
            "CONNECT" | "STOMP" => replies.push(Frame::new(
                "CONNECTED",
                &[("version", "1.2"), ("server", "FakeArtemis/2")],
                "",
            )),
            "SEND" => {
                let destination = frame.get("destination").unwrap().to_owned();
                let headers = frame
                    .headers
                    .iter()
                    .filter(|(k, _)| {
                        !["destination", "receipt", "content-length"].contains(&k.as_str())
                    })
                    .cloned()
                    .collect();
                state
                    .lock()
                    .unwrap()
                    .queue
                    .push_back((destination, frame.body.clone(), headers));
                if let Some(receipt) = frame.get("receipt") {
                    replies.push(Frame::new("RECEIPT", &[("receipt-id", receipt)], ""));
                }
            }
            "SUBSCRIBE" => {
                subscription = Some((
                    frame.get("id").unwrap().into(),
                    frame.get("destination").unwrap().into(),
                ));
            }
            "ACK" => {
                if in_flight.as_ref().map(|(ack, _)| ack.as_str()) == frame.get("id") {
                    in_flight = None;
                } else {
                    replies.push(Frame::new("ERROR", &[("message", "Unknown ACK id")], ""));
                }
            }
            "DISCONNECT" => {
                if let Some(receipt) = frame.get("receipt") {
                    replies.push(Frame::new("RECEIPT", &[("receipt-id", receipt)], ""));
                }
            }
            _ => {}
        }
        replies.extend(deliver(&subscription, &mut in_flight));
        let delivering = replies.iter().any(|reply| reply.command == "MESSAGE");
        for reply in replies {
            if stream.write_all(&reply.encode()).await.is_err() {
                break;
            }
        }
        if delivering {
            delivered.notify_one();
        }
        if ["DISCONNECT", "ERROR"].contains(&frame.command.as_str())
            || frame.get("passcode").is_some_and(|p| p != "secret")
        {
            break;
        }
    }
    // Unacknowledged messages go back to the front of the queue, as on a broker.
    if let Some((_, message)) = in_flight {
        state.lock().unwrap().queue.push_front(message);
    }
}

fn headers(frame: &Frame) -> Vec<(&str, &str)> {
    frame
        .headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

fn commands(debug: &Value) -> Vec<String> {
    debug["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|frame| format!("{} {}", frame["direction"], frame["command"]).replace('"', ""))
        .collect()
}

#[tokio::test]
async fn send_then_subscribe_acknowledges_each_message() {
    let broker = Broker::start().await;
    let host = Host::new();
    let url = format!("{}/orders", broker.url);
    let reply = host
        .call(
            "stomp",
            json!({"command":"SEND","url":url,"body":"{\"id\":1}",
                "headers":{"destination-type":"ANYCAST","content-type":"application/json"}}),
        )
        .await;
    let result = reply.ok();
    assert_eq!(result["outcome"], "OK", "{reply}");
    assert_eq!(
        result["columns"],
        json!(["destination", "bytes", "headers"])
    );
    assert_eq!(
        result["rows"],
        json!([{"destination":"orders","bytes":8,"headers":{
            "destination":"orders","destination-type":"ANYCAST","content-type":"application/json"
        }}])
    );
    assert_eq!(result["debug"]["connection"]["version"], "1.2");
    assert_eq!(result["debug"]["connection"]["server"], "FakeArtemis/2");
    assert!(result["debug"]["connection"].get("disconnect").is_none());
    assert_eq!(
        commands(&result["debug"]),
        [
            "sent CONNECT",
            "received CONNECTED",
            "sent SEND",
            "received RECEIPT",
            "sent DISCONNECT",
            "received RECEIPT"
        ]
    );
    assert_eq!(
        result["debug"]["frames"][0]["headers"]["passcode"],
        "******"
    );
    assert!(!result["debug"].to_string().contains("secret"));
    let connect = &broker.received("CONNECT")[0];
    assert_eq!(connect.get("login"), Some("app"));
    assert_eq!(connect.get("passcode"), Some("secret"));
    assert_eq!(connect.get("accept-version"), Some("1.0,1.1,1.2"));
    assert_eq!(connect.get("host"), Some("127.0.0.1"));
    assert_eq!(
        headers(&broker.received("SEND")[0]),
        [
            ("destination", "orders"),
            ("destination-type", "ANYCAST"),
            ("content-type", "application/json"),
            ("receipt", "send-0")
        ]
    );
    broker.enqueue("orders", "second");
    let reply = host
        .call(
            "stomp",
            json!({"command":"SUBSCRIBE","url":url,"wait":"1","columnsType":"body=json"}),
        )
        .await;
    let result = reply.ok();
    assert_eq!(result["outcome"], "OK", "{reply}");
    assert_eq!(result["resultCount"], 2);
    assert_eq!(result["rows"][0]["body"], "{\"id\":1}");
    assert_eq!(result["rows"][0]["message-id"], "ID:1");
    assert_eq!(
        result["rows"][0]["headers"]["content-type"],
        "application/json"
    );
    assert_eq!(result["rows"][1]["body"], "second");
    assert_eq!(result["columnTypes"]["body"], "json");
    assert_eq!(
        result["debug"]["subscription"],
        json!({"ack":"client-individual","messages":2,"acknowledged":2,"endedBy":"wait"})
    );
    let subscribe = &broker.received("SUBSCRIBE")[0];
    assert_eq!(
        headers(subscribe),
        [
            ("id", "0"),
            ("destination", "orders"),
            ("ack", "client-individual"),
            ("consumer-window-size", "0")
        ]
    );
    let acks: Vec<_> = broker
        .received("ACK")
        .iter()
        .map(|ack| ack.get("id").unwrap().to_owned())
        .collect();
    assert_eq!(acks, ["ack-1", "ack-2"]);
    assert!(broker.queued().is_empty());
}

#[tokio::test]
async fn count_leaves_the_rest_on_the_broker() {
    let broker = Broker::start().await;
    for body in ["a", "b", "c"] {
        broker.enqueue("/queue/jobs", body);
    }
    let host = Host::new();
    let params = json!({"command":"subscribe","url":format!("{}/ignored", broker.url),
        "headers":{"destination":"/queue/jobs"},"count":2});
    let reply = host.call("stomp", params.clone()).await;
    let result = reply.ok();
    let bodies = |result: &Value| {
        result["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["body"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(bodies(result), ["a", "b"], "{reply}");
    assert_eq!(result["debug"]["subscription"]["endedBy"], "count");
    assert_eq!(result["debug"]["request"]["destination"], "/queue/jobs");
    tokio::time::timeout(Duration::from_secs(3), async {
        while broker.queued() != ["c"] {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the unacknowledged message must return to the queue");
    let reply = host.call("stomp", params).await;
    assert_eq!(bodies(reply.ok()), ["c"], "{reply}");
}

#[tokio::test]
async fn broker_errors_fail_with_their_message() {
    let broker = Broker::start().await;
    let host = Host::new();
    let url = broker.url.replace(":secret@", ":wrong@") + "/orders";
    let reply = host
        .call("stomp", json!({"command":"SEND","url":url,"body":"x"}))
        .await;
    assert_eq!(
        reply.error(),
        "STOMP ERROR: Security Error occurred: User name [app] or password is invalid"
    );
    let debug = &reply["debug"];
    assert_eq!(
        commands(debug),
        ["sent CONNECT", "received ERROR"],
        "{reply}"
    );
    assert_eq!(
        debug["frames"][1]["body"],
        "AMQ229031: Unable to validate user"
    );
    assert_eq!(
        debug["request"]["url"],
        broker.url.replace(":secret@", ":******@") + "/orders"
    );
    assert!(!debug.to_string().contains("wrong"));
    assert!(broker.received("SEND").is_empty());
}

#[tokio::test]
async fn stop_returns_the_messages_already_acknowledged() {
    let broker = Broker::start().await;
    broker.enqueue("orders", "kept");
    let host = Host::new();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let params = json!({"command":"SUBSCRIBE","url":format!("{}/orders", broker.url),"wait":"30"});
    let task = tokio::spawn(async move { host.call_cancellable("stomp", params, cancel).await });
    tokio::time::timeout(Duration::from_secs(3), broker.delivered.notified())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop.cancel();
    let reply = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    let result = reply.ok();
    assert_eq!(result["outcome"], "Stopped", "{reply}");
    assert_eq!(result["stopped"], true);
    assert_eq!(result["rows"][0]["body"], "kept");
    assert_eq!(result["debug"]["subscription"]["endedBy"], "stop");
    assert_eq!(broker.received("ACK").len(), 1);
}

#[tokio::test]
async fn invalid_requests_fail_before_connecting() {
    let host = Host::new();
    for (params, error) in [
        (
            json!({"command":"PUBLISH","url":"stomp://127.0.0.1:1/q"}),
            "The STOMP command must be SEND or SUBSCRIBE",
        ),
        (
            json!({"command":"SEND","url":"http://127.0.0.1:1/q"}),
            "The STOMP endpoint must start with stomp:// or stomp+ssl://",
        ),
        (
            json!({"command":"SUBSCRIBE","url":"stomp://127.0.0.1:1/q","count":"0"}),
            "STOMP Count must be an integer from 1 to 1000",
        ),
    ] {
        let reply = host.call("stomp", params).await;
        assert_eq!(reply.error(), error);
    }
}
