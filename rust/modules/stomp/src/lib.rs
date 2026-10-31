//! STOMP: sends a message to a broker destination or consumes messages from it
//! (Apache ActiveMQ Artemis and other STOMP 1.0-1.2 brokers).

mod connection;
pub mod frame;

use connection::Connection;
use frame::Frame;
use serde_json::{Map, Value, json};
use services_contracts::columns;
use services_module_api::error::{self, Error, Result, required, text};
use services_module_api::sdk::{self, Call, Manifest, RunStatus, Service, Threads};
use services_module_api::transport::{EndpointArc, EndpointBox, Status};
use std::future::Future;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Connecting, the broker's CONNECTED and a SEND receipt each get this long.
const STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// DISCONNECT waits this long for its receipt.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);
/// A subscription ends after this long even while messages keep coming.
const MAX_SUBSCRIBE: Duration = Duration::from_secs(600);
const MAX_COUNT: u64 = 1000;
const MAX_WAIT_SECONDS: u64 = 600;
/// Message bodies one subscription returns; the message past it stays unacknowledged.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const SUBSCRIPTION_ID: &str = "0";
const SEND_RECEIPT: &str = "send-0";
const DISCONNECT_RECEIPT: &str = "disconnect-0";
const SEND_COLUMNS: [&str; 3] = ["destination", "bytes", "headers"];
const MESSAGE_COLUMNS: [&str; 4] = ["destination", "message-id", "headers", "body"];

pub struct Stomp;

impl Service for Stomp {
    fn manifest(&self) -> Manifest {
        Manifest {
            execute: true,
            discover: false,
            stream: false,
            ..Manifest::from_json(include_str!("../manifest.json"), env!("CARGO_PKG_VERSION"))
        }
    }

    async fn execute(&self, call: Call) -> Result<Value> {
        run(&call.params, &call.cancel).await
    }

    /// A stopped subscription still returns the messages it has already acknowledged.
    fn soft_cancel(&self) -> bool {
        true
    }

    fn status(&self, response: &Value) -> RunStatus {
        if response["stopped"] == true {
            RunStatus::Stopped
        } else {
            sdk::status(response)
        }
    }

    fn outcome(&self, response: &Value) -> &'static str {
        sdk::common_outcome(response).unwrap_or(if response["stopped"] == true {
            "Stopped"
        } else if response.get("warning").is_some() {
            "WARNING"
        } else {
            "OK"
        })
    }
}

pub fn endpoint(host: EndpointArc) -> std::result::Result<EndpointBox, Status> {
    sdk::serve(Stomp, host, Threads::Own("stomp-module"))
}

services_module_api::export!(endpoint);

/// Broker address from `stomp://login:passcode@host:port/destination`.
#[derive(Debug, PartialEq)]
pub struct Target {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub login: Option<String>,
    pub passcode: Option<String>,
    /// The path without its leading `/`, query and fragment included (`news.#`).
    pub destination: String,
    /// The URL with the passcode masked.
    pub shown: String,
}

pub fn target(url: &str) -> Result<Target> {
    let url = url.trim();
    let invalid = || Error::new(format!("Invalid STOMP URL: {url}"));
    let parsed = url::Url::parse(url).map_err(|_| invalid())?;
    let tls = match parsed.scheme() {
        "stomp" => false,
        "stomp+ssl" | "stomp+tls" => true,
        _ => {
            return Err(Error::new(
                "The STOMP endpoint must start with stomp:// or stomp+ssl://",
            ));
        }
    };
    let host = parsed
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(invalid)?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let decode = |part: &str| {
        percent_encoding::percent_decode_str(part)
            .decode_utf8()
            .map(|part| part.into_owned())
            .map_err(|_| invalid())
    };
    let mut destination = decode(parsed.path().strip_prefix('/').unwrap_or(parsed.path()))?;
    if let Some(query) = parsed.query() {
        destination = format!("{destination}?{}", decode(query)?);
    }
    if let Some(fragment) = parsed.fragment() {
        destination = format!("{destination}#{}", decode(fragment)?);
    }
    let mut shown = parsed.clone();
    if parsed.password().is_some() {
        let _ = shown.set_password(Some("******"));
    }
    Ok(Target {
        tls,
        host,
        port: parsed.port().unwrap_or(61613),
        login: Some(decode(parsed.username())?).filter(|login| !login.is_empty()),
        passcode: parsed.password().map(decode).transpose()?,
        destination,
        shown: shown.into(),
    })
}

struct Request {
    subscribe: bool,
    target: Target,
    destination: String,
    /// `#headers` in order, without `destination` (the endpoint path or that header).
    headers: Vec<(String, String)>,
    body: String,
    count: u64,
    wait: Duration,
    client_id: String,
    vhost: String,
    columns: columns::Options,
}

fn number(params: &Value, key: &str, label: &str, default: u64, max: u64) -> Result<u64> {
    let invalid = || Error::new(format!("STOMP {label} must be an integer from 1 to {max}"));
    let value = match params.get(key) {
        None | Some(Value::Null) => return Ok(default),
        Some(Value::String(value)) if value.trim().is_empty() => return Ok(default),
        Some(Value::String(value)) => value.trim().parse::<u64>().map_err(|_| invalid())?,
        Some(value) => value.as_u64().ok_or_else(invalid)?,
    };
    if (1..=max).contains(&value) {
        Ok(value)
    } else {
        Err(invalid())
    }
}

impl Request {
    fn parse(params: &Value) -> Result<Self> {
        let subscribe = match required(params, "command")?
            .trim()
            .to_ascii_uppercase()
            .as_str()
        {
            "SEND" => false,
            "SUBSCRIBE" => true,
            _ => return Err(Error::new("The STOMP command must be SEND or SUBSCRIBE")),
        };
        let target = target(&required(params, "url")?)?;
        let mut destination = target.destination.clone();
        let mut headers = vec![];
        for (name, value) in error::headers(params)?.as_object().into_iter().flatten() {
            let value = error::string(value);
            if name == "destination" {
                destination = value;
            } else {
                headers.push((name.clone(), value));
            }
        }
        if destination.is_empty() {
            return Err(Error::new(
                "Add the destination to the endpoint: stomp://host:61613/<destination>",
            ));
        }
        let body = text(params, "body");
        if subscribe && !body.is_empty() {
            return Err(Error::new("SUBSCRIBE takes no #body"));
        }
        let columns = columns::Options::parse(params).map_err(Error::without_debug)?;
        columns
            .apply(&mut grid(if subscribe {
                &MESSAGE_COLUMNS
            } else {
                &SEND_COLUMNS
            }))
            .map_err(Error::new)?;
        Ok(Self {
            subscribe,
            vhost: Some(text(params, "vhost").trim().to_owned())
                .filter(|vhost| !vhost.is_empty())
                .unwrap_or_else(|| target.host.clone()),
            target,
            destination,
            headers,
            body,
            count: number(params, "count", "Count", 10, MAX_COUNT)?,
            wait: Duration::from_secs(number(params, "wait", "Wait", 5, MAX_WAIT_SECONDS)?),
            client_id: text(params, "clientId").trim().to_owned(),
            columns,
        })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

fn grid(columns: &[&str]) -> Value {
    json!({"columns":columns,"columnTypes":{},"rows":[],"resultCount":0})
}

pub async fn run(params: &Value, cancel: &CancellationToken) -> Result<Value> {
    let started = Instant::now();
    let request = Request::parse(params)?;
    let mut log = Log {
        started,
        value: json!({
            "request":{
                "command":if request.subscribe {"SUBSCRIBE"} else {"SEND"},
                "url":request.target.shown,"destination":request.destination,
                "headers":Map::from_iter(request.headers.iter().map(|(k, v)| (k.clone(), json!(v)))),
            },
            "connection":{"host":request.target.host,"port":request.target.port,"tls":request.target.tls},
            "frames":[],
        }),
    };
    let exchanged = exchange(&request, cancel, &mut log).await;
    let mut debug = log.value;
    let duration = started.elapsed().as_millis();
    match exchanged {
        Ok(mut result) => {
            if result["cancelled"] != true {
                request.columns.apply(&mut result).map_err(Error::new)?;
            }
            result["durationMs"] = json!(duration);
            result["debug"] = debug;
            Ok(result)
        }
        Err(message) => {
            debug["error"] = json!({"name":"Error","message":message});
            let mut error = Error::new(message);
            error.debug = Some(Box::new(debug));
            error.duration_ms = Some(duration);
            Err(error)
        }
    }
}

struct Log {
    started: Instant,
    value: Value,
}

impl Log {
    fn frame(&mut self, direction: &str, frame: &Frame) {
        let mut headers = Map::new();
        for (name, value) in &frame.headers {
            if !headers.contains_key(name) {
                let value = if name == "passcode" { "******" } else { value };
                headers.insert(name.clone(), json!(value));
            }
        }
        let mut entry = json!({
            "atMs":self.started.elapsed().as_millis(),"direction":direction,
            "command":frame.command,"headers":headers,
        });
        if !frame.body.is_empty() {
            entry["bodyBytes"] = json!(frame.body.len());
            if frame.command == "ERROR" {
                entry["body"] = json!(String::from_utf8_lossy(&frame.body));
            }
        }
        self.value["frames"].as_array_mut().unwrap().push(entry);
    }
}

/// `None` when the user stopped first.
async fn step<T>(
    cancel: &CancellationToken,
    limit: Duration,
    what: &str,
    future: impl Future<Output = std::result::Result<T, String>>,
) -> std::result::Result<Option<T>, String> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Ok(None),
        result = tokio::time::timeout(limit, future) => match result {
            Ok(result) => result.map(Some),
            Err(_) => Err(format!("Timed out after {} s waiting for {what}", limit.as_secs())),
        },
    }
}

fn stopped_early() -> Value {
    json!({"cancelled":true,"warning":sdk::CANCELLED_WARNING})
}

fn broker_error(frame: &Frame) -> String {
    let body = String::from_utf8_lossy(&frame.body);
    let message = frame
        .get("message")
        .map(str::to_owned)
        .or_else(|| {
            body.lines()
                .find(|line| !line.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "no message".into());
    format!("STOMP ERROR: {message}")
}

async fn exchange(
    request: &Request,
    cancel: &CancellationToken,
    log: &mut Log,
) -> std::result::Result<Value, String> {
    let target = &request.target;
    let Some(mut connection) = step(
        cancel,
        STEP_TIMEOUT,
        "the connection",
        Connection::open(&target.host, target.port, target.tls),
    )
    .await?
    else {
        return Ok(stopped_early());
    };
    let mut connect = Frame::new("CONNECT")
        .header("accept-version", "1.0,1.1,1.2")
        .header("host", &request.vhost)
        .header("heart-beat", "0,0");
    if let Some(login) = &target.login {
        connect = connect.header("login", login);
    }
    if let Some(passcode) = &target.passcode {
        connect = connect.header("passcode", passcode);
    }
    if !request.client_id.is_empty() {
        connect = connect.header("client-id", &request.client_id);
    }
    log.frame("sent", &connect);
    connection.send(&connect).await?;
    let Some(connected) = step(cancel, STEP_TIMEOUT, "CONNECTED", connection.receive()).await?
    else {
        return Ok(stopped_early());
    };
    log.frame("received", &connected);
    match connected.command.as_str() {
        "CONNECTED" => {}
        "ERROR" => return Err(broker_error(&connected)),
        other => return Err(format!("Expected CONNECTED from the broker, got {other}")),
    }
    let version = connected.get("version").unwrap_or("1.0").to_owned();
    connection.set_escapes(version != "1.0");
    log.value["connection"]["version"] = json!(version);
    if let Some(server) = connected.get("server") {
        log.value["connection"]["server"] = json!(server);
    }
    let result = if request.subscribe {
        subscribe(request, &version, &mut connection, cancel, log).await?
    } else {
        send(request, &mut connection, cancel, log).await?
    };
    if result["cancelled"] != true {
        disconnect(&mut connection, log).await;
    }
    connection.close().await;
    Ok(result)
}

async fn send(
    request: &Request,
    connection: &mut Connection,
    cancel: &CancellationToken,
    log: &mut Log,
) -> std::result::Result<Value, String> {
    let body = request.body.as_bytes();
    let mut frame = Frame::new("SEND").header("destination", &request.destination);
    for (name, value) in &request.headers {
        if !["receipt", "content-length"].contains(&name.as_str()) {
            frame = frame.header(name, value);
        }
    }
    // Without content-length the body ends at its first NUL.
    if request.header("content-length").is_some() || body.contains(&0) {
        frame = frame.header("content-length", &body.len().to_string());
    }
    frame = frame.header("receipt", SEND_RECEIPT);
    frame.body = body.to_vec();
    log.frame("sent", &frame);
    connection.send(&frame).await?;
    let receipt = async {
        loop {
            let reply = connection.receive().await?;
            log.frame("received", &reply);
            match reply.command.as_str() {
                "RECEIPT" if reply.get("receipt-id") == Some(SEND_RECEIPT) => return Ok(()),
                "ERROR" => return Err(broker_error(&reply)),
                _ => {}
            }
        }
    };
    if step(cancel, STEP_TIMEOUT, "the SEND receipt", receipt)
        .await?
        .is_none()
    {
        return Ok(stopped_early());
    }
    let headers = Map::from_iter(
        frame
            .headers
            .iter()
            .filter(|(name, _)| name != "receipt")
            .map(|(name, value)| (name.clone(), json!(value))),
    );
    Ok(json!({
        "columns":SEND_COLUMNS,"columnTypes":{"bytes":"number","headers":"json"},
        "rows":[{"destination":request.destination,"bytes":body.len(),"headers":headers}],
        "resultCount":1,
    }))
}

/// Why a subscription returned.
#[derive(Clone, Copy)]
enum End {
    Count,
    Wait,
    Limit,
    Size,
    Stop,
    Failure,
}

impl End {
    fn name(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Wait => "wait",
            Self::Limit => "time-limit",
            Self::Size => "size-limit",
            Self::Stop => "stop",
            Self::Failure => "error",
        }
    }
}

async fn subscribe(
    request: &Request,
    version: &str,
    connection: &mut Connection,
    cancel: &CancellationToken,
    log: &mut Log,
) -> std::result::Result<Value, String> {
    let mut frame = Frame::new("SUBSCRIBE")
        .header("id", SUBSCRIPTION_ID)
        .header("destination", &request.destination);
    for (name, value) in &request.headers {
        if name != "id" {
            frame = frame.header(name, value);
        }
    }
    // Artemis: hand out the next message only after this one is acknowledged.
    for (name, value) in [("ack", "client-individual"), ("consumer-window-size", "0")] {
        if request.header(name).is_none() {
            frame = frame.header(name, value);
        }
    }
    let ack = frame.get("ack").unwrap_or("auto").to_owned();
    log.frame("sent", &frame);
    connection.send(&frame).await?;
    let mut rows = vec![];
    let mut body_bytes = 0;
    let mut acknowledged = 0;
    let mut warning = None;
    let limit = Instant::now() + MAX_SUBSCRIBE;
    let mut idle = Instant::now() + request.wait;
    let end = loop {
        if rows.len() as u64 >= request.count {
            break End::Count;
        }
        let deadline = idle.min(limit);
        let received = tokio::select! {
            biased;
            _ = cancel.cancelled() => break End::Stop,
            received = tokio::time::timeout_at(deadline.into(), connection.receive()) => received,
        };
        let failure = match received {
            Err(_) if deadline == limit => break End::Limit,
            Err(_) => break End::Wait,
            Ok(Err(error)) => error,
            Ok(Ok(message)) => match message.command.as_str() {
                "MESSAGE" => {
                    body_bytes += message.body.len();
                    if body_bytes > MAX_BODY_BYTES {
                        warning = Some(format!(
                            "Stopped at the {} MiB response limit; the next message stays on the broker.",
                            MAX_BODY_BYTES / 1024 / 1024
                        ));
                        break End::Size;
                    }
                    rows.push(row(&message));
                    if ack == "auto" {
                        idle = Instant::now() + request.wait;
                        continue;
                    }
                    match acknowledge(version, &message, connection).await {
                        Ok(()) => {
                            acknowledged += 1;
                            idle = Instant::now() + request.wait;
                            continue;
                        }
                        Err(error) => error,
                    }
                }
                "ERROR" => {
                    log.frame("received", &message);
                    broker_error(&message)
                }
                _ => {
                    log.frame("received", &message);
                    continue;
                }
            },
        };
        if rows.is_empty() {
            return Err(failure);
        }
        // Messages already taken from the broker are shown rather than lost behind an error.
        warning = Some(format!(
            "{failure}. Returned the {} messages received before it.",
            rows.len()
        ));
        break End::Failure;
    };
    log.value["subscription"] = json!({
        "ack":ack,"messages":rows.len(),"acknowledged":acknowledged,"endedBy":end.name(),
    });
    let mut result = json!({
        "columns":MESSAGE_COLUMNS,"columnTypes":{"headers":"json","body":"json"},
        "resultCount":rows.len(),"rows":rows,
    });
    if matches!(end, End::Stop) {
        result["stopped"] = json!(true);
    }
    if let Some(warning) = warning {
        result["warning"] = json!(warning);
    }
    Ok(result)
}

fn row(message: &Frame) -> Value {
    let mut headers = Map::new();
    for (name, value) in &message.headers {
        if !headers.contains_key(name) {
            headers.insert(name.clone(), json!(value));
        }
    }
    json!({
        "destination":message.get("destination"),
        "message-id":message.get("message-id"),
        "headers":headers,
        "body":String::from_utf8_lossy(&message.body),
    })
}

async fn acknowledge(
    version: &str,
    message: &Frame,
    connection: &mut Connection,
) -> std::result::Result<(), String> {
    let id = message.get("message-id").unwrap_or_default();
    let frame = match version {
        "1.2" => Frame::new("ACK").header("id", message.get("ack").unwrap_or(id)),
        "1.1" => Frame::new("ACK")
            .header("subscription", SUBSCRIPTION_ID)
            .header("message-id", id),
        _ => Frame::new("ACK").header("message-id", id),
    };
    tokio::time::timeout(STEP_TIMEOUT, connection.send(&frame))
        .await
        .map_err(|_| "Timed out sending ACK".to_string())?
}

/// Best effort: the receipt confirms the broker has processed every earlier ACK.
async fn disconnect(connection: &mut Connection, log: &mut Log) {
    let frame = Frame::new("DISCONNECT").header("receipt", DISCONNECT_RECEIPT);
    log.frame("sent", &frame);
    let closed = async {
        connection.send(&frame).await?;
        loop {
            let reply = connection.receive().await?;
            if reply.command == "RECEIPT" {
                log.frame("received", &reply);
                return Ok::<_, String>(());
            }
        }
    };
    if let Err(error) = tokio::time::timeout(CLOSE_TIMEOUT, closed)
        .await
        .unwrap_or_else(|_| Err("no DISCONNECT receipt".into()))
    {
        log.value["connection"]["disconnect"] = json!(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_carry_credentials_destination_and_tls() {
        let parsed = target("stomp+ssl://app%40corp:p%3Ass@broker.local:61617/orders.in").unwrap();
        assert_eq!(
            parsed,
            Target {
                tls: true,
                host: "broker.local".into(),
                port: 61617,
                login: Some("app@corp".into()),
                passcode: Some("p:ss".into()),
                destination: "orders.in".into(),
                shown: "stomp+ssl://app%40corp:******@broker.local:61617/orders.in".into(),
            }
        );
        let wildcard = target(" stomp://[::1]/news.# ").unwrap();
        assert_eq!(
            (wildcard.host.as_str(), wildcard.port, wildcard.tls),
            ("::1", 61613, false)
        );
        assert_eq!(wildcard.destination, "news.#");
        assert_eq!(wildcard.login, None);
        assert_eq!(target("stomp://broker").unwrap().destination, "");
        assert_eq!(
            target("http://broker/q").unwrap_err().message,
            "The STOMP endpoint must start with stomp:// or stomp+ssl://"
        );
        assert_eq!(
            target("stomp:orders").unwrap_err().message,
            "Invalid STOMP URL: stomp:orders"
        );
    }

    #[test]
    fn requests_validate_options_and_destination_overrides() {
        let request = Request::parse(&json!({
            "command":"subscribe","url":"stomp://broker/ignored",
            "headers":{"destination":"/queue/orders","selector":"kind = 'a'"},
            "count":"3","wait":2,"vhost":"/","columnsType":"body=text"
        }))
        .unwrap();
        assert!(request.subscribe);
        assert_eq!(request.destination, "/queue/orders");
        assert_eq!(request.headers, [("selector".into(), "kind = 'a'".into())]);
        assert_eq!((request.count, request.wait.as_secs()), (3, 2));
        assert_eq!(request.vhost, "/");
        let error = |params: Value| Request::parse(&params).err().unwrap().message;
        let url = "stomp://broker/q";
        assert_eq!(
            error(json!({"command":"PUBLISH","url":url})),
            "The STOMP command must be SEND or SUBSCRIBE"
        );
        assert_eq!(
            error(json!({"command":"SEND","url":"stomp://broker"})),
            "Add the destination to the endpoint: stomp://host:61613/<destination>"
        );
        assert_eq!(
            error(json!({"command":"SUBSCRIBE","url":url,"body":"x"})),
            "SUBSCRIBE takes no #body"
        );
        for count in ["0", "1001", "x"] {
            assert_eq!(
                error(json!({"command":"SUBSCRIBE","url":url,"count":count})),
                "STOMP Count must be an integer from 1 to 1000"
            );
        }
        assert_eq!(
            error(json!({"command":"SUBSCRIBE","url":url,"wait":"601"})),
            "STOMP Wait must be an integer from 1 to 600"
        );
        assert_eq!(
            error(json!({"command":"SEND","url":url,"columnsType":"body=json"})),
            "Column options refer to an unknown result column: body"
        );
        let send = Request::parse(&json!({"command":"SEND","url":url})).unwrap();
        assert_eq!((send.count, send.wait.as_secs()), (10, 5));
        assert_eq!(send.vhost, "broker");
    }
}
