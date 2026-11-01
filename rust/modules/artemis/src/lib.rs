//! Apache ActiveMQ Artemis over the broker's Jolokia management API (the web console, port
//! 8161). `#action` lists queues, addresses and consumers, browses, receives, sends and publishes
//! messages, and purges, moves, retries, pauses and resumes queues.

mod action;

use action::Command;
use base64::Engine;
use serde_json::{Map, Value, json};
use services_contracts::columns;
use services_module_api::error::{self, Error, Result, required, text};
use services_module_api::http;
use services_module_api::proto::host::{HttpRequest, HttpResponse};
use services_module_api::sdk::{self, Call, Host, Manifest, Service, Threads};
use services_module_api::transport::{EndpointArc, EndpointBox, Status};
use std::time::Instant;

const DOMAIN: &str = "org.apache.activemq.artemis";
const JOLOKIA_PATH: &str = "/console/jolokia";
const CALL_TIMEOUT_MS: u64 = 30_000;
/// Characters (text) or bytes (bytes body) of a message body the broker returns over
/// management by default (`management-message-attribute-size-limit`).
const BODY_LIMIT: usize = 256;
const MASK: &str = "******";
const SEND: &str =
    "sendMessage(java.util.Map,int,java.lang.String,boolean,java.lang.String,java.lang.String)";
/// `sendMessage` type of a text message.
const TEXT_MESSAGE: u8 = 3;
/// Headers the debug panel shows masked.
const SECRET_HEADERS: [&str; 3] = ["authorization", "proxy-authorization", "cookie"];
const MESSAGE_COLUMNS: [&str; 5] = ["message-id", "timestamp", "body", "properties", "headers"];
const CUT: &str = "the broker returns at most management-message-attribute-size-limit characters of a body over management (256 by default)";

pub struct Artemis;

impl Service for Artemis {
    fn manifest(&self) -> Manifest {
        Manifest {
            execute: true,
            discover: true,
            stream: false,
            ..Manifest::from_json(include_str!("../manifest.json"), env!("CARGO_PKG_VERSION"))
        }
    }

    async fn execute(&self, call: Call) -> Result<Value> {
        run(&call.host, &call.params).await
    }

    async fn discover(&self, call: Call) -> Result<Value> {
        discover(&call.host, &call.params).await
    }

    fn outcome(&self, response: &Value) -> &'static str {
        sdk::common_outcome(response).unwrap_or(if response.get("warning").is_some() {
            "WARNING"
        } else {
            "OK"
        })
    }
}

pub fn endpoint(host: EndpointArc) -> std::result::Result<EndpointBox, Status> {
    sdk::serve(Artemis, host, Threads::Own("artemis-module"))
}

services_module_api::export!(endpoint);

/// How a list shows a broker field.
#[derive(Clone, Copy, PartialEq)]
enum Cell {
    Text,
    Number,
    Flag,
    /// Milliseconds since the epoch.
    Time,
    /// A JSON list in a string: `["ANYCAST"]`.
    Names,
}

/// A list column, the broker field it shows and filters by, and how it is shown.
type Field = (&'static str, &'static str, Cell);

struct List {
    operation: &'static str,
    sort: &'static str,
    fields: &'static [Field],
    what: &'static str,
}

const QUEUES: List = List {
    operation: "listQueues(java.lang.String,int,int)",
    sort: "name",
    what: "queues",
    fields: &[
        ("queue", "name", Cell::Text),
        ("address", "address", Cell::Text),
        ("routing-type", "routingType", Cell::Text),
        ("messages", "messageCount", Cell::Number),
        ("delivering", "deliveringCount", Cell::Number),
        ("scheduled", "scheduledCount", Cell::Number),
        ("consumers", "consumerCount", Cell::Number),
        ("added", "messagesAdded", Cell::Number),
        ("acked", "messagesAcked", Cell::Number),
        ("killed", "messagesKilled", Cell::Number),
        ("expired", "messagesExpired", Cell::Number),
        ("durable", "durable", Cell::Flag),
        ("paused", "paused", Cell::Flag),
    ],
};

const ADDRESSES: List = List {
    operation: "listAddresses(java.lang.String,int,int)",
    sort: "name",
    what: "addresses",
    fields: &[
        ("address", "name", Cell::Text),
        ("routing-types", "routingTypes", Cell::Names),
        ("queues", "queueCount", Cell::Number),
        ("messages", "messageCount", Cell::Number),
        ("routed", "routedMessageCount", Cell::Number),
        ("unrouted", "unroutedMessageCount", Cell::Number),
        ("size", "addressSize", Cell::Number),
        ("paging", "paging", Cell::Flag),
        ("paused", "paused", Cell::Flag),
        ("internal", "internal", Cell::Flag),
        ("temporary", "temporary", Cell::Flag),
        ("auto-created", "autoCreated", Cell::Flag),
    ],
};

const CONSUMERS: List = List {
    operation: "listConsumers(java.lang.String,int,int)",
    sort: "id",
    what: "consumers",
    fields: &[
        ("consumer", "id", Cell::Number),
        ("queue", "queue", Cell::Text),
        ("address", "address", Cell::Text),
        ("client-id", "clientID", Cell::Text),
        ("user", "user", Cell::Text),
        ("protocol", "protocol", Cell::Text),
        ("remote-address", "remoteAddress", Cell::Text),
        ("filter", "filter", Cell::Text),
        ("created", "creationTime", Cell::Text),
        ("in-transit", "messagesInTransit", Cell::Number),
        ("delivered", "messagesDelivered", Cell::Number),
        ("acked", "messagesAcknowledged", Cell::Number),
        ("last-delivered", "lastDeliveredTime", Cell::Time),
        ("last-acked", "lastAcknowledgedTime", Cell::Time),
        ("status", "status", Cell::Text),
    ],
};

fn list_of(command: Command) -> Option<&'static List> {
    match command {
        Command::Queues => Some(&QUEUES),
        Command::Addresses => Some(&ADDRESSES),
        Command::Consumers => Some(&CONSUMERS),
        _ => None,
    }
}

fn column_names(command: Command) -> Vec<&'static str> {
    if let Some(list) = list_of(command) {
        return list.fields.iter().map(|field| field.0).collect();
    }
    match command {
        Command::Send => vec!["queue", "message-id"],
        Command::Publish => vec!["address", "message-id"],
        Command::Purge => vec!["queue", "removed"],
        Command::Move => vec!["queue", "to", "moved"],
        Command::Retry => vec!["queue", "retried"],
        Command::Pause | Command::Resume => vec!["queue", "paused"],
        Command::Jolokia => vec!["request", "status", "value", "error"],
        _ => MESSAGE_COLUMNS.to_vec(),
    }
}

/// The broker from `#endpoint`: its web console, `http(s)://user:password@host:8161`, or its
/// Jolokia URL.
#[derive(Debug, PartialEq)]
pub struct Target {
    /// The Jolokia URL, without credentials.
    pub jolokia: String,
    pub origin: String,
    pub user: Option<String>,
    pub password: Option<String>,
    /// The endpoint with the password masked.
    pub shown: String,
}

pub fn target(url: &str) -> Result<Target> {
    let url = url.trim();
    let first = url.split_whitespace().next().unwrap_or_default();
    if Command::parse(first).is_some() {
        return Err(Error::new(format!(
            "#endpoint is only the broker URL now; the command goes in #action: {} /queue",
            first.to_ascii_uppercase()
        )));
    }
    let invalid = || Error::new(format!("Invalid Artemis URL: {url}"));
    let parsed = url::Url::parse(url).map_err(|_| invalid())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::new(
            "The Artemis endpoint is the broker's web console URL: http://host:8161",
        ));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(invalid());
    }
    let decode = |part: &str| {
        percent_encoding::percent_decode_str(part)
            .decode_utf8()
            .map(|part| part.into_owned())
            .map_err(|_| invalid())
    };
    let mut jolokia = parsed.clone();
    let _ = jolokia.set_username("");
    let _ = jolokia.set_password(None);
    jolokia.set_fragment(None);
    if matches!(parsed.path(), "" | "/") {
        jolokia.set_path(JOLOKIA_PATH);
    }
    let mut shown = parsed.clone();
    if parsed.password().is_some() {
        let _ = shown.set_password(Some(MASK));
    }
    Ok(Target {
        jolokia: jolokia.into(),
        origin: parsed.origin().ascii_serialization(),
        user: Some(decode(parsed.username())?).filter(|user| !user.is_empty()),
        password: parsed.password().map(decode).transpose()?,
        shown: shown.into(),
    })
}

fn basic(user: &str, password: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}

struct Request {
    /// `#action` as written.
    text: String,
    action: action::Action,
    target: Target,
    /// The HTTP headers of every Jolokia call: the defaults, the endpoint's login and
    /// `#headers`.
    headers: Map<String, Value>,
    body: String,
    persistent: bool,
    columns: columns::Options,
}

impl Request {
    fn parse(params: &Value) -> Result<Self> {
        let target = target(&required(params, "url")?)?;
        let written = text(params, "action");
        let action = action::parse(&written).map_err(Error::new)?;
        let command = action.command;
        let name = command.name();
        let mut headers = Map::new();
        headers.insert("Content-Type".into(), json!("application/json"));
        headers.insert("Accept".into(), json!("application/json"));
        headers.insert("Origin".into(), json!(target.origin));
        if let Some(user) = &target.user {
            let password = target.password.as_deref().unwrap_or("");
            headers.insert("Authorization".into(), json!(basic(user, password)));
        }
        for (header, value) in error::headers(params)?.as_object().into_iter().flatten() {
            if target.user.is_some() && header.eq_ignore_ascii_case("authorization") {
                return Err(Error::new(
                    "Log in either in the endpoint or with an Authorization header, not both",
                ));
            }
            headers.retain(|name, _| !name.eq_ignore_ascii_case(header));
            headers.insert(header.clone(), json!(error::string(value)));
        }
        let body = text(params, "body");
        let takes_body = matches!(command, Command::Send | Command::Publish | Command::Jolokia);
        if !takes_body && !body.trim().is_empty() {
            return Err(Error::new(format!(
                "{name} takes no #body: it is the message of SEND and PUBLISH, or the request of JOLOKIA"
            )));
        }
        if command == Command::Jolokia && body.trim().is_empty() {
            return Err(Error::new(
                "JOLOKIA sends #body to Jolokia: {\"type\":\"read\",\"mbean\":\"java.lang:type=Memory\"}",
            ));
        }
        let persistent = match text(params, "persistent")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "" => true,
            _ if !matches!(command, Command::Send | Command::Publish) => {
                return Err(Error::new(format!(
                    "Persistent applies to SEND or PUBLISH, not {name}"
                )));
            }
            "true" | "yes" => true,
            "false" | "no" => false,
            _ => return Err(Error::new("Persistent must be true or false")),
        };
        let columns = columns::Options::parse(params).map_err(Error::without_debug)?;
        columns
            .apply(&mut grid(&column_names(command), json!({}), vec![]))
            .map_err(Error::new)?;
        Ok(Self {
            text: written,
            action,
            target,
            headers,
            body,
            persistent,
            columns,
        })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .and_then(|(_, value)| value.as_str())
    }

    /// The user and password `sendMessage` takes: the endpoint's login, or a Basic
    /// Authorization header.
    fn credentials(&self) -> (Option<String>, Option<String>) {
        if self.target.user.is_some() {
            return (self.target.user.clone(), self.target.password.clone());
        }
        let login = self
            .header("Authorization")
            .and_then(|value| value.trim().split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
            .and_then(|(_, token)| {
                base64::engine::general_purpose::STANDARD
                    .decode(token.trim())
                    .ok()
            })
            .and_then(|login| String::from_utf8(login).ok());
        match login.as_deref().and_then(|login| login.split_once(':')) {
            Some((user, password)) => (Some(user.into()), Some(password.into())),
            None => (None, None),
        }
    }

    /// The headers as the debug panel shows them.
    fn shown_headers(&self) -> Value {
        let headers: Map<String, Value> = self
            .headers
            .iter()
            .map(|(name, value)| {
                let secret = SECRET_HEADERS.contains(&name.to_ascii_lowercase().as_str());
                (
                    name.clone(),
                    if secret { json!(MASK) } else { value.clone() },
                )
            })
            .collect();
        Value::Object(headers)
    }
}

fn grid(columns: &[&str], types: Value, rows: Vec<Value>) -> Value {
    json!({"columns":columns,"columnTypes":types,"resultCount":rows.len(),"rows":rows})
}

fn warn(result: &mut Value, warnings: Vec<String>) {
    if !warnings.is_empty() {
        result["warning"] = json!(warnings.join(". "));
    }
}

pub async fn run(host: &Host, params: &Value) -> Result<Value> {
    let started = Instant::now();
    let request = Request::parse(params)?;
    let mut jolokia = Jolokia {
        host,
        request: &request,
        started,
        calls: vec![],
    };
    let result = match request.action.command {
        Command::Queues | Command::Addresses | Command::Consumers => {
            list(&mut jolokia, &request).await
        }
        Command::Browse => browse(&mut jolokia, &request).await,
        Command::Receive => receive(&mut jolokia, &request).await,
        Command::Send | Command::Publish => send(&mut jolokia, &request).await,
        Command::Purge => purge(&mut jolokia, &request).await,
        Command::Move => move_messages(&mut jolokia, &request).await,
        Command::Retry => retry(&mut jolokia, &request).await,
        Command::Pause | Command::Resume => pause(&mut jolokia, &request).await,
        Command::Jolokia => raw(&mut jolokia, &request).await,
    };
    let debug = jolokia.debug();
    let duration = started.elapsed().as_millis();
    match result {
        Ok(mut result) => {
            request.columns.apply(&mut result).map_err(Error::new)?;
            result["durationMs"] = json!(duration);
            result["debug"] = debug;
            Ok(result)
        }
        Err(error) => Err(debugged(error, debug, duration)),
    }
}

/// `error` with the request and calls the debug panel shows, the password masked.
fn debugged(mut error: Error, mut debug: Value, duration: u128) -> Error {
    match error.debug.take().map(|debug| *debug) {
        // A failed host call: keep its connection details.
        Some(Value::Object(details)) => {
            for (key, value) in details {
                debug[key] = value;
            }
        }
        _ => debug["error"] = json!({"name":"Error","message":error.message}),
    }
    error.debug = Some(Box::new(debug));
    error.duration_ms = Some(duration);
    error
}

/// How many queues and addresses discovery reads.
const DISCOVERED: u64 = action::MAX_LIMIT;
/// Name prefixes of the broker's own queues and addresses (MQTT sessions, notifications,
/// management replies).
const SYSTEM: [&str; 2] = ["$sys.", "activemq."];

/// The queue and address names `#action` completes, without the broker's own ones.
pub async fn discover(host: &Host, params: &Value) -> Result<Value> {
    let mut params = params.clone();
    params["action"] = json!("QUEUES");
    let request = Request::parse(&params)?;
    let mut jolokia = Jolokia {
        host,
        request: &request,
        started: Instant::now(),
        calls: vec![],
    };
    let started = jolokia.started;
    discovered(&mut jolokia).await.map_err(|error| {
        let mut debug = jolokia.debug();
        if let Some(request) = debug["request"].as_object_mut() {
            request.remove("action");
        }
        debugged(error, debug, started.elapsed().as_millis())
    })
}

async fn discovered(jolokia: &mut Jolokia<'_>) -> Result<Value> {
    let broker = broker(jolokia).await?;
    let mut names = Map::new();
    let lists = [(QUEUES, "internalQueue"), (ADDRESSES, "internal")];
    let operations = lists
        .iter()
        .map(|(list, _)| {
            let options = json!({
                "field":"","operation":"","value":"","sortOrder":"asc","sortColumn":list.sort,
            });
            exec(
                &broker,
                list.operation,
                json!([options.to_string(), 1, DISCOVERED]),
            )
        })
        .collect();
    for ((list, internal), reply) in lists.iter().zip(jolokia.many(operations).await?) {
        let listed: Value = value(&reply)?
            .as_str()
            .and_then(|listed| serde_json::from_str(listed).ok())
            .ok_or_else(|| Error::new(format!("Artemis {} did not return JSON", list.operation)))?;
        let own = |item: &Value| {
            [*internal, "temporary"]
                .iter()
                .any(|flag| item[flag] == true || item[flag] == "true")
        };
        let listed: Vec<_> = listed["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| !own(item))
            .filter_map(|item| item["name"].as_str())
            .filter(|name| !SYSTEM.iter().any(|prefix| name.starts_with(prefix)))
            .collect();
        names.insert(list.what.into(), json!(listed));
    }
    let mut after: Map<String, Value> = action::COMMANDS
        .iter()
        .filter_map(|command| {
            let list = match command.resource()? {
                "queue" => QUEUES.what,
                "address" => ADDRESSES.what,
                _ => return None,
            };
            Some((command.name().into(), json!(list)))
        })
        .collect();
    after.insert("TO".into(), json!(QUEUES.what));
    Ok(json!({"names":names,"after":after}))
}

struct Jolokia<'a> {
    host: &'a Host,
    request: &'a Request,
    started: Instant,
    /// What the debug panel shows: each HTTP call with the password masked.
    calls: Vec<Value>,
}

impl Jolokia<'_> {
    /// The request and the calls so far, as the debug panel shows them.
    fn debug(&self) -> Value {
        let request = self.request;
        json!({
            "request":{
                "action":request.text,"url":request.target.shown,
                "jolokia":request.target.jolokia,"headers":request.shown_headers(),
            },
            "calls":self.calls,
        })
    }

    /// One operation's `value`; a failed operation fails the call.
    async fn one(&mut self, operation: Value) -> Result<Value> {
        let shown = masked(&operation);
        value(&self.post(operation, shown).await?)
    }

    /// Several operations in one HTTP call; each reply keeps its own `status`.
    async fn many(&mut self, operations: Vec<Value>) -> Result<Vec<Value>> {
        let count = operations.len();
        let shown = json!(operations.iter().map(masked).collect::<Vec<_>>());
        match self.post(json!(operations), shown).await? {
            Value::Array(replies) if replies.len() == count => Ok(replies),
            _ => Err(Error::new(
                "Jolokia did not answer each operation of a bulk request",
            )),
        }
    }

    async fn post(&mut self, body: Value, shown: Value) -> Result<Value> {
        let target = &self.request.target;
        let request = HttpRequest {
            method: "POST".into(),
            url: target.jolokia.clone(),
            headers: http::header_list(&Value::Object(self.request.headers.clone())),
            body: Some(body.to_string().into_bytes()),
            text: true,
            timeout_ms: CALL_TIMEOUT_MS,
            no_redirects: true,
            no_retries: true,
            ..Default::default()
        };
        let at = self.started.elapsed();
        self.calls
            .push(json!({"atMs":at.as_millis(),"operations":shown}));
        let response = self.host.http(&request).await?;
        let call = self.calls.last_mut().unwrap();
        call["status"] = json!(response.status);
        call["durationMs"] = json!((self.started.elapsed() - at).as_millis());
        let reply = String::from_utf8_lossy(&response.body);
        if response.status != 200 {
            call["response"] = json!(excerpt(&reply));
            return Err(http_error(self.request, &response, &reply));
        }
        let reply: Value = serde_json::from_str(&reply).map_err(|_| {
            Error::new(format!(
                "Expected a Jolokia reply from {}, got: {}",
                target.jolokia,
                excerpt(&reply)
            ))
        })?;
        let failed: Vec<_> = std::slice::from_ref(&reply)
            .iter()
            .chain(reply.as_array().into_iter().flatten())
            .filter(|reply| reply.get("status").is_some_and(|status| status != 200))
            .map(|reply| json!({"status":reply["status"],"error":reply["error"]}))
            .collect();
        if !failed.is_empty() {
            call["errors"] = json!(failed);
        }
        Ok(reply)
    }
}

/// The operation as the debug panel shows it: a SEND without its password.
fn masked(operation: &Value) -> Value {
    let mut operation = operation.clone();
    if operation["operation"] == SEND && operation["arguments"][5].is_string() {
        operation["arguments"][5] = json!(MASK);
    }
    operation
}

fn value(reply: &Value) -> Result<Value> {
    if reply["status"] == 200 {
        Ok(reply["value"].clone())
    } else {
        let error = reply["error"].as_str().unwrap_or("the operation failed");
        Err(Error::new(format!("Artemis: {error}")))
    }
}

fn excerpt(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.char_indices().nth(300) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text,
    }
}

fn http_error(request: &Request, response: &HttpResponse, reply: &str) -> Error {
    let status = response.status;
    let url = &request.target.jolokia;
    Error::new(match status {
        300..=399 => {
            let location = response
                .headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case("location"))
                .map_or("another address", |header| header.value.as_str());
            format!(
                "{url} redirects to {location} (HTTP {status}): check http or https and the port of the endpoint"
            )
        }
        401 | 403 if request.header("Authorization").is_none() => format!(
            "Artemis asks for a login (HTTP {status}): put it in the endpoint, http://user:password@host:8161, or add an Authorization header"
        ),
        401 | 403 => format!(
            "Artemis refused the request (HTTP {status}): check the login, that the user has a management role, and that jolokia-access.xml allows the origin {}",
            request.header("Origin").unwrap_or_default()
        ),
        404 => format!(
            "No Jolokia at {url} (HTTP 404): the endpoint is the broker's web console, http://host:8161, or its Jolokia URL; the queue goes in #action"
        ),
        _ => format!(
            "Jolokia answered HTTP {status} {}: {}",
            response.status_text,
            excerpt(reply)
        ),
    })
}

/// `"name"` as an ObjectName value, the way Artemis names its MBeans.
fn quote(value: &str) -> String {
    let mut quoted = String::from('"');
    for c in value.chars() {
        match c {
            '"' | '*' | '?' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '\n' => quoted.push_str("\\n"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// The single MBean a search found.
fn found(names: Value, missing: impl FnOnce() -> String) -> Result<String> {
    let names: Vec<String> = names
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|name| name.as_str().map(str::to_owned))
        .collect();
    match names.as_slice() {
        [] => Err(Error::new(missing())),
        [name] => Ok(name.clone()),
        names => Err(Error::new(format!(
            "Jolokia lists several matching MBeans: {}",
            names.join(", ")
        ))),
    }
}

async fn search(jolokia: &mut Jolokia<'_>, pattern: String) -> Result<Value> {
    jolokia.one(json!({"type":"search","mbean":pattern})).await
}

async fn broker(jolokia: &mut Jolokia<'_>) -> Result<String> {
    let names = search(jolokia, format!("{DOMAIN}:broker=*")).await?;
    found(names, || {
        format!(
            "No Artemis broker at {}; is it the web console of Apache ActiveMQ Artemis?",
            jolokia.request.target.jolokia
        )
    })
}

async fn queue(jolokia: &mut Jolokia<'_>, name: &str) -> Result<String> {
    let pattern = format!(
        "{DOMAIN}:component=addresses,subcomponent=queues,queue={},*",
        quote(name)
    );
    found(search(jolokia, pattern).await?, || {
        format!("Queue {name} does not exist; QUEUES lists the broker's queues")
    })
}

async fn address(jolokia: &mut Jolokia<'_>, name: &str) -> Result<String> {
    let pattern = format!(
        "{DOMAIN}:broker=*,component=addresses,address={}",
        quote(name)
    );
    found(search(jolokia, pattern).await?, || {
        format!("Address {name} does not exist; ADDRESSES lists the broker's addresses")
    })
}

fn exec(mbean: &str, operation: &str, arguments: Value) -> Value {
    json!({"type":"exec","mbean":mbean,"operation":operation,"arguments":arguments})
}

/// The WHERE of PURGE and MOVE: `null` for every message.
fn selector(request: &Request) -> Value {
    match request.action.selector.as_str() {
        "" => Value::Null,
        selector => json!(selector),
    }
}

/// QUEUES, ADDRESSES and CONSUMERS: one page of the broker's list, filtered by one condition.
async fn list(jolokia: &mut Jolokia<'_>, request: &Request) -> Result<Value> {
    let action = &request.action;
    let Some(list) = list_of(action.command) else {
        return Err(Error::new(format!(
            "{} is not a list",
            action.command.name()
        )));
    };
    let broker = broker(jolokia).await?;
    let (field, operation, filter) = match &action.condition {
        Some(condition) => {
            let field = list
                .fields
                .iter()
                .find(|field| field.0.eq_ignore_ascii_case(&condition.column))
                .map_or(condition.column.as_str(), |field| field.1);
            (field, condition.operation, condition.value.as_str())
        }
        None => ("", "", ""),
    };
    let options = json!({
        "field":field,"operation":operation,"value":filter,"sortOrder":"asc","sortColumn":list.sort,
    });
    let listed = jolokia
        .one(exec(
            &broker,
            list.operation,
            json!([options.to_string(), 1, action.limit]),
        ))
        .await
        .map_err(|error| match &action.condition {
            Some(condition) if error.message.contains("No enum constant") => {
                let columns: Vec<_> = list.fields.iter().map(|field| field.0).collect();
                Error::new(format!(
                    "{} has no column {}: WHERE takes {}",
                    action.command.name(),
                    condition.column,
                    columns.join(", ")
                ))
            }
            _ => error,
        })?;
    let listed: Value = listed
        .as_str()
        .and_then(|listed| serde_json::from_str(listed).ok())
        .ok_or_else(|| Error::new(format!("Artemis {} did not return JSON", list.operation)))?;
    let rows: Vec<Value> = listed["data"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| row(list.fields, item))
        .collect();
    let shown = rows.len();
    let mut types = Map::new();
    for (column, _, cell) in list.fields {
        let kind = match cell {
            Cell::Number => "number",
            Cell::Flag => "boolean",
            Cell::Time => "datetime",
            Cell::Text | Cell::Names => continue,
        };
        types.insert((*column).into(), json!(kind));
    }
    let columns: Vec<_> = list.fields.iter().map(|field| field.0).collect();
    let mut result = grid(&columns, Value::Object(types), rows);
    let total = number(&listed["count"]).as_u64().unwrap_or(0);
    if total > shown as u64 {
        warn(
            &mut result,
            vec![format!(
                "Showing {shown} of {total} {}; raise LIMIT or narrow the list with WHERE",
                list.what
            )],
        );
    }
    Ok(result)
}

fn row(fields: &[Field], item: &Value) -> Value {
    let mut row = Map::new();
    for (column, field, cell) in fields {
        let value = &item[*field];
        let shown = match cell {
            Cell::Text => value.clone(),
            Cell::Number => number(value),
            Cell::Flag => flag(value),
            Cell::Time => match value.as_i64() {
                Some(ms) if ms > 0 => json!(utc(ms)),
                Some(_) => Value::Null,
                None => value.clone(),
            },
            Cell::Names => match value.as_str().map(serde_json::from_str::<Vec<String>>) {
                Some(Ok(names)) => json!(names.join(", ")),
                _ => value.clone(),
            },
        };
        row.insert((*column).into(), shown);
    }
    Value::Object(row)
}

/// The broker's lists report every field as a string.
fn number(value: &Value) -> Value {
    match value {
        Value::String(text) => text
            .parse::<i64>()
            .map_or_else(|_| value.clone(), |number| json!(number)),
        _ => value.clone(),
    }
}

fn flag(value: &Value) -> Value {
    match value.as_str() {
        Some("true") => json!(true),
        Some("false") => json!(false),
        _ => value.clone(),
    }
}

fn browsing(mbean: &str, request: &Request) -> Value {
    let action = &request.action;
    if action.selector.is_empty() {
        exec(mbean, "browse(int,int)", json!([1, action.limit]))
    } else {
        exec(
            mbean,
            "browse(int,int,java.lang.String)",
            json!([1, action.limit, action.selector]),
        )
    }
}

fn messages(value: Value) -> Result<Vec<Value>> {
    match value {
        Value::Array(messages) => Ok(messages),
        _ => Err(Error::new(
            "Artemis browse did not return a list of messages",
        )),
    }
}

fn message_grid(rows: Vec<Value>) -> Value {
    grid(
        &MESSAGE_COLUMNS,
        json!({"timestamp":"datetime","body":"json","properties":"json","headers":"json"}),
        rows,
    )
}

async fn browse(jolokia: &mut Jolokia<'_>, request: &Request) -> Result<Value> {
    let action = &request.action;
    let mbean = queue(jolokia, &action.resource).await?;
    let counting = if action.selector.is_empty() {
        json!({"type":"read","mbean":mbean,"attribute":["MessageCount","DeliveringCount"]})
    } else {
        exec(
            &mbean,
            "countMessages(java.lang.String)",
            json!([action.selector]),
        )
    };
    let replies = jolokia
        .many(vec![browsing(&mbean, request), counting])
        .await?;
    let messages = messages(value(&replies[0])?)?;
    let cut = messages.iter().filter(|message| is_cut(message)).count();
    let rows: Vec<Value> = messages.iter().map(message_row).collect();
    let shown = rows.len() as u64;
    let (total, delivering) = match value(&replies[1]) {
        Ok(Value::Object(counts)) => (
            counts.get("MessageCount").and_then(Value::as_u64),
            counts.get("DeliveringCount").and_then(Value::as_u64),
        ),
        Ok(total) => (total.as_u64(), None),
        Err(_) => (None, None),
    };
    let mut warnings = vec![];
    if let Some(total) = total.filter(|total| *total > shown) {
        let mut warning = format!("Showing {shown} of {total} messages");
        if shown == action.limit {
            warning.push_str("; raise LIMIT to see more");
        } else if let Some(delivering) = delivering.filter(|delivering| *delivering > 0) {
            warning.push_str(&format!(
                "; {delivering} are out with consumers and cannot be browsed"
            ));
        }
        warnings.push(warning);
    }
    if cut > 0 {
        warnings.push(format!("{cut} message bodies are cut: {CUT}"));
    }
    let mut result = message_grid(rows);
    warn(&mut result, warnings);
    Ok(result)
}

/// SEND to a queue, PUBLISH to an address (each of its queues gets a copy).
async fn send(jolokia: &mut Jolokia<'_>, request: &Request) -> Result<Value> {
    let action = &request.action;
    let (mbean, column) = if action.command == Command::Publish {
        (address(jolokia, &action.resource).await?, "address")
    } else {
        (queue(jolokia, &action.resource).await?, "queue")
    };
    let (user, password) = request.credentials();
    let id = jolokia
        .one(exec(
            &mbean,
            SEND,
            json!([
                action.properties,
                TEXT_MESSAGE,
                request.body,
                request.persistent,
                user,
                password
            ]),
        ))
        .await?;
    Ok(grid(
        &[column, "message-id"],
        json!({}),
        vec![json!({column:action.resource,"message-id":error::string(&id)})],
    ))
}

/// One row of a queue operation that returns a count or a flag.
fn done(request: &Request, column: &str, value: Value, kind: &str) -> Value {
    let action = &request.action;
    let mut row = json!({"queue":action.resource});
    if action.command == Command::Move {
        row["to"] = json!(action.to);
    }
    row[column] = number(&value);
    grid(
        &column_names(action.command),
        json!({column:kind}),
        vec![row],
    )
}

async fn purge(jolokia: &mut Jolokia<'_>, request: &Request) -> Result<Value> {
    let mbean = queue(jolokia, &request.action.resource).await?;
    let removed = jolokia
        .one(exec(
            &mbean,
            "removeMessages(java.lang.String)",
            json!([selector(request)]),
        ))
        .await?;
    Ok(done(request, "removed", removed, "number"))
}

async fn move_messages(jolokia: &mut Jolokia<'_>, request: &Request) -> Result<Value> {
    let action = &request.action;
    let mbean = queue(jolokia, &action.resource).await?;
    // The broker's own error for a missing target names no queue list.
    queue(jolokia, &action.to).await?;
    let moved = jolokia
        .one(exec(
            &mbean,
            "moveMessages(java.lang.String,java.lang.String)",
            json!([selector(request), action.to]),
        ))
        .await?;
    Ok(done(request, "moved", moved, "number"))
}

async fn retry(jolokia: &mut Jolokia<'_>, request: &Request) -> Result<Value> {
    let mbean = queue(jolokia, &request.action.resource).await?;
    let retried = jolokia
        .one(exec(&mbean, "retryMessages()", json!([])))
        .await?;
    Ok(done(request, "retried", retried, "number"))
}

async fn pause(jolokia: &mut Jolokia<'_>, request: &Request) -> Result<Value> {
    let mbean = queue(jolokia, &request.action.resource).await?;
    let operation = if request.action.command == Command::Pause {
        "pause()"
    } else {
        "resume()"
    };
    let replies = jolokia
        .many(vec![
            exec(&mbean, operation, json!([])),
            json!({"type":"read","mbean":mbean,"attribute":"Paused"}),
        ])
        .await?;
    value(&replies[0])?;
    Ok(done(request, "paused", value(&replies[1])?, "boolean"))
}

/// JOLOKIA: `#body` as is, one Jolokia request or a list of them; a row per reply.
async fn raw(jolokia: &mut Jolokia<'_>, request: &Request) -> Result<Value> {
    let body: Value = serde_json::from_str(&request.body).map_err(|error| {
        Error::new(format!(
            "JOLOKIA takes a JSON request or a list of them in #body: {error}"
        ))
    })?;
    let sent = match &body {
        Value::Array(requests) => requests.clone(),
        Value::Object(_) => vec![body.clone()],
        _ => {
            return Err(Error::new(
                "JOLOKIA takes a JSON request or a list of them in #body",
            ));
        }
    };
    let replies = match jolokia.post(body.clone(), body).await? {
        Value::Array(replies) => replies,
        reply => vec![reply],
    };
    let mut failed = 0;
    let rows: Vec<Value> = replies
        .iter()
        .enumerate()
        .map(|(at, reply)| {
            if reply["status"] != 200 {
                failed += 1;
            }
            let sent = sent.get(at).cloned().unwrap_or_default();
            json!({
                "request":reply.get("request").cloned().unwrap_or(sent),"status":reply["status"],
                "value":reply.get("value").cloned().unwrap_or_default(),
                "error":reply.get("error").cloned().unwrap_or_default(),
            })
        })
        .collect();
    let count = rows.len();
    let mut result = grid(
        &column_names(Command::Jolokia),
        json!({"request":"json","status":"number","value":"json"}),
        rows,
    );
    if failed > 0 {
        warn(
            &mut result,
            vec![format!("{failed} of {count} Jolokia requests failed")],
        );
    }
    Ok(result)
}

/// Management has no consumer: RECEIVE browses messages, then removes each one it can show
/// whole.
async fn receive(jolokia: &mut Jolokia<'_>, request: &Request) -> Result<Value> {
    let mbean = queue(jolokia, &request.action.resource).await?;
    let browsed = messages(jolokia.one(browsing(&mbean, request)).await?)?;
    let (whole, cut): (Vec<_>, Vec<_>) = browsed.into_iter().partition(|m| !is_cut(m));
    let mut ids = vec![];
    for message in &whole {
        let id = error::string(&message["messageID"]);
        ids.push(
            id.parse::<i64>()
                .map_err(|_| Error::new(format!("Artemis returned message ID {id:?}")))?,
        );
    }
    let mut rows = vec![];
    let mut taken = 0;
    let mut failures = vec![];
    if !ids.is_empty() {
        let removals = ids
            .iter()
            .map(|id| exec(&mbean, "removeMessage(long)", json!([id])))
            .collect();
        let replies = jolokia.many(removals).await?;
        for (message, reply) in whole.iter().zip(&replies) {
            match value(reply) {
                Ok(Value::Bool(true)) => rows.push(message_row(message)),
                Ok(_) => taken += 1,
                Err(error) => failures.push(error.message),
            }
        }
    }
    let mut warnings = vec![];
    if !cut.is_empty() {
        warnings.push(format!(
            "Left {} messages in the queue: {CUT}, so receiving them would lose the rest of the body",
            cut.len()
        ));
    }
    if taken > 0 {
        warnings.push(format!(
            "{taken} messages were consumed by someone else before RECEIVE removed them"
        ));
    }
    if let Some(first) = failures.first() {
        warnings.push(format!(
            "Could not remove {} messages: {first}",
            failures.len()
        ));
    }
    let mut result = message_grid(rows);
    warn(&mut result, warnings);
    Ok(result)
}

/// A text body ends with `, + N more` when the broker cut it; a bytes body is cut silently.
fn is_cut(message: &Value) -> bool {
    let text_cut = message["text"].as_str().is_some_and(|text| {
        text.strip_suffix(" more")
            .and_then(|rest| rest.rsplit_once(", + "))
            .is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    });
    let bytes_cut = message["BodyPreview"]
        .as_array()
        .is_some_and(|bytes| bytes.len() >= BODY_LIMIT);
    let large = message["largeMessage"] == true && body(message).is_empty();
    text_cut || bytes_cut || large
}

fn body(message: &Value) -> String {
    if let Some(text) = message["text"].as_str() {
        return text.to_owned();
    }
    let Some(preview) = message["BodyPreview"].as_array() else {
        return String::new();
    };
    let bytes: Vec<u8> = preview
        .iter()
        .filter_map(Value::as_i64)
        .map(|byte| byte as u8)
        .collect();
    match String::from_utf8(bytes) {
        Ok(text) => text,
        // The preview may end inside a character.
        Err(error) if error.utf8_error().error_len().is_none() => {
            let valid = error.utf8_error().valid_up_to();
            String::from_utf8_lossy(&error.as_bytes()[..valid]).into_owned()
        }
        Err(error) => error
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Properties merged from the typed maps; the other message fields are its headers.
fn message_row(message: &Value) -> Value {
    let mut properties = Map::new();
    let mut headers = Map::new();
    for (name, value) in message.as_object().into_iter().flatten() {
        match name.as_str() {
            "messageID" | "timestamp" | "text" | "BodyPreview" | "PropertiesText" => {}
            typed if typed.ends_with("Properties") => {
                properties.extend(value.as_object().cloned().unwrap_or_default());
            }
            _ => {
                headers.insert(name.clone(), value.clone());
            }
        }
    }
    let timestamp = match message["timestamp"].as_i64() {
        Some(ms) if ms > 0 => json!(utc(ms)),
        _ => message["timestamp"].clone(),
    };
    json!({
        "message-id":error::string(&message["messageID"]),"timestamp":timestamp,
        "body":body(message),"properties":properties,"headers":headers,
    })
}

/// Milliseconds since the epoch as `2024-05-01T12:30:00.000Z`.
fn utc(ms: i64) -> String {
    let (days, ms) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(params: Value) -> Result<Request> {
        Request::parse(&params)
    }

    #[test]
    fn the_endpoint_is_the_web_console_or_the_jolokia_url() {
        let target = target("https://ad%40min:p%3Ass@broker:8443#x").unwrap();
        assert_eq!(
            target,
            Target {
                jolokia: "https://broker:8443/console/jolokia".into(),
                origin: "https://broker:8443".into(),
                user: Some("ad@min".into()),
                password: Some("p:ss".into()),
                shown: "https://ad%40min:******@broker:8443/#x".into(),
            }
        );
        assert_eq!(
            super::target("http://broker:8161/jolokia/")
                .unwrap()
                .jolokia,
            "http://broker:8161/jolokia/"
        );
        assert!(super::target("tcp://broker:61616").is_err());
        assert_eq!(
            super::target("browse http://broker:8161/orders")
                .unwrap_err()
                .message,
            "#endpoint is only the broker URL now; the command goes in #action: BROWSE /queue"
        );
    }

    #[test]
    fn headers_reach_jolokia_and_log_in_a_send() {
        let request = parse(json!({
            "url":"http://broker:8161","action":"SEND /orders",
            "headers":{"authorization":"Basic YWRtaW46czpw","origin":"http://console"},
        }))
        .unwrap();
        assert_eq!(
            Value::Object(request.headers.clone()),
            json!({
                "Content-Type":"application/json","Accept":"application/json",
                "authorization":"Basic YWRtaW46czpw","origin":"http://console",
            })
        );
        assert_eq!(
            request.credentials(),
            (Some("admin".into()), Some("s:p".into()))
        );
        assert_eq!(request.shown_headers()["authorization"], MASK);
        let login = parse(json!({
            "url":"http://admin:pw@broker:8161","action":"QUEUES","headers":{"Authorization":"x"},
        }));
        assert!(login.err().unwrap().message.starts_with("Log in either"));
    }

    #[test]
    fn body_and_persistent_belong_to_the_message() {
        let error = |params: Value| parse(params).err().unwrap().message;
        assert_eq!(
            error(json!({"url":"http://b:8161","action":"PURGE /q","body":"x"})),
            "PURGE takes no #body: it is the message of SEND and PUBLISH, or the request of JOLOKIA"
        );
        assert!(
            error(json!({"url":"http://b:8161","action":"JOLOKIA"}))
                .starts_with("JOLOKIA sends #body")
        );
        assert_eq!(
            error(json!({"url":"http://b:8161","action":"BROWSE /q","persistent":"false"})),
            "Persistent applies to SEND or PUBLISH, not BROWSE"
        );
        let publish = parse(json!({
            "url":"http://b:8161","action":"PUBLISH /events","persistent":"no","body":"x",
        }))
        .unwrap();
        assert!(!publish.persistent);
    }

    #[test]
    fn list_rows_turn_broker_strings_into_values() {
        let address = json!({
            "name":"events","routingTypes":"[\"ANYCAST\",\"MULTICAST\"]","queueCount":"2",
            "paging":"false","autoCreated":"true",
        });
        let row = row(ADDRESSES.fields, &address);
        assert_eq!(
            (
                &row["address"],
                &row["routing-types"],
                &row["queues"],
                &row["auto-created"]
            ),
            (
                &json!("events"),
                &json!("ANYCAST, MULTICAST"),
                &json!(2),
                &json!(true)
            )
        );
        let consumer =
            json!({"id":"82","lastDeliveredTime":1_714_566_600_123_i64,"lastAcknowledgedTime":0});
        let row = super::row(CONSUMERS.fields, &consumer);
        assert_eq!(
            (&row["consumer"], &row["last-delivered"], &row["last-acked"]),
            (&json!(82), &json!("2024-05-01T12:30:00.123Z"), &Value::Null)
        );
    }

    #[test]
    fn browsed_messages_become_rows_and_cut_bodies_are_detected() {
        let message = json!({
            "messageID":"28","timestamp":1_714_566_600_123_i64,"text":"{\"id\":1}",
            "PropertiesText":"…","StringProperties":{"k":"v"},"IntProperties":{"n":1},
            "LongProperties":null,"address":"orders","durable":true,
        });
        assert_eq!(
            message_row(&message),
            json!({
                "message-id":"28","timestamp":"2024-05-01T12:30:00.123Z","body":"{\"id\":1}",
                "properties":{"k":"v","n":1},"headers":{"address":"orders","durable":true},
            })
        );
        assert!(!is_cut(&message));
        assert!(is_cut(&json!({"text":"xxxx, + 44 more"})));
        assert!(is_cut(&json!({"BodyPreview":vec![65; BODY_LIMIT]})));
        assert_eq!(body(&json!({"BodyPreview":[104, 105, -61]})), "hi");
        assert_eq!(body(&json!({"BodyPreview":[-1, 0]})), "ff 00");
    }
}
