//! Apache ActiveMQ Artemis: lists queues, browses, sends and receives messages through the
//! broker's Jolokia management API (the web console, port 8161).

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
const MAX_COUNT: u64 = 1000;
/// Characters (text) or bytes (bytes body) of a message body the broker returns over
/// management by default (`management-message-attribute-size-limit`).
const BODY_LIMIT: usize = 256;
const MASK: &str = "******";
const SEND: &str =
    "sendMessage(java.util.Map,int,java.lang.String,boolean,java.lang.String,java.lang.String)";
/// `sendMessage` type of a text message.
const TEXT_MESSAGE: u8 = 3;
const QUEUE_COLUMNS: [&str; 13] = [
    "queue",
    "address",
    "routing-type",
    "messages",
    "delivering",
    "scheduled",
    "consumers",
    "added",
    "acked",
    "killed",
    "expired",
    "durable",
    "paused",
];
const MESSAGE_COLUMNS: [&str; 5] = ["message-id", "timestamp", "body", "properties", "headers"];
const SEND_COLUMNS: [&str; 2] = ["queue", "message-id"];
const CUT: &str = "the broker returns at most management-message-attribute-size-limit characters of a body over management (256 by default)";

pub struct Artemis;

impl Service for Artemis {
    fn manifest(&self) -> Manifest {
        Manifest {
            execute: true,
            discover: false,
            stream: false,
            ..Manifest::from_json(include_str!("../manifest.json"), env!("CARGO_PKG_VERSION"))
        }
    }

    async fn execute(&self, call: Call) -> Result<Value> {
        run(&call.host, &call.params).await
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Queues,
    Browse,
    Send,
    Receive,
}

impl Command {
    fn parse(command: &str) -> Result<Self> {
        Ok(match command.trim().to_ascii_uppercase().as_str() {
            "QUEUES" => Self::Queues,
            "BROWSE" => Self::Browse,
            "SEND" => Self::Send,
            "RECEIVE" => Self::Receive,
            _ => {
                return Err(Error::new(
                    "The Artemis command must be QUEUES, BROWSE, SEND or RECEIVE",
                ));
            }
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Queues => "QUEUES",
            Self::Browse => "BROWSE",
            Self::Send => "SEND",
            Self::Receive => "RECEIVE",
        }
    }

    fn columns(self) -> &'static [&'static str] {
        match self {
            Self::Queues => &QUEUE_COLUMNS,
            Self::Browse | Self::Receive => &MESSAGE_COLUMNS,
            Self::Send => &SEND_COLUMNS,
        }
    }
}

/// The broker from `http(s)://user:password@host:8161/<queue>`.
#[derive(Debug, PartialEq)]
pub struct Target {
    /// The Jolokia URL, without credentials.
    pub jolokia: String,
    pub origin: String,
    pub user: Option<String>,
    pub password: Option<String>,
    /// The percent-decoded URL path without its leading `/`; empty for QUEUES.
    pub queue: String,
    /// The endpoint with the password masked.
    pub shown: String,
}

pub fn target(url: &str, jolokia_path: &str) -> Result<Target> {
    let url = url.trim();
    let invalid = || Error::new(format!("Invalid Artemis URL: {url}"));
    let parsed = url::Url::parse(url).map_err(|_| invalid())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::new(
            "The Artemis endpoint is the broker's web console URL: http://user:password@host:8161/<queue>",
        ));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(invalid());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(Error::new(
            "Put only the queue name in the URL path; write ? and # in it as %3F and %23",
        ));
    }
    let decode = |part: &str| {
        percent_encoding::percent_decode_str(part)
            .decode_utf8()
            .map(|part| part.into_owned())
            .map_err(|_| invalid())
    };
    let path = parsed.path().trim_start_matches('/').trim_end_matches('/');
    let jolokia_path = match jolokia_path.trim() {
        "" => JOLOKIA_PATH,
        path if path.starts_with('/') => path,
        _ => return Err(Error::new("Jolokia-Path must start with /")),
    };
    let mut jolokia = parsed.clone();
    let _ = jolokia.set_username("");
    let _ = jolokia.set_password(None);
    jolokia.set_path(jolokia_path);
    let mut shown = parsed.clone();
    if parsed.password().is_some() {
        let _ = shown.set_password(Some(MASK));
    }
    Ok(Target {
        jolokia: jolokia.into(),
        origin: parsed.origin().ascii_serialization(),
        user: Some(decode(parsed.username())?).filter(|user| !user.is_empty()),
        password: parsed.password().map(decode).transpose()?,
        queue: decode(path)?,
        shown: shown.into(),
    })
}

struct Request {
    command: Command,
    target: Target,
    /// `#headers`: string properties of a SEND message.
    properties: Map<String, Value>,
    body: String,
    count: u64,
    filter: String,
    selector: String,
    durable: bool,
    columns: columns::Options,
}

fn count(params: &Value, default: u64) -> Result<u64> {
    let invalid = || Error::new(format!("Count must be an integer from 1 to {MAX_COUNT}"));
    let value = match params.get("count") {
        None | Some(Value::Null) => return Ok(default),
        Some(Value::String(value)) if value.trim().is_empty() => return Ok(default),
        Some(Value::String(value)) => value.trim().parse::<u64>().map_err(|_| invalid())?,
        Some(value) => value.as_u64().ok_or_else(invalid)?,
    };
    if (1..=MAX_COUNT).contains(&value) {
        Ok(value)
    } else {
        Err(invalid())
    }
}

impl Request {
    fn parse(params: &Value) -> Result<Self> {
        let command = Command::parse(&required(params, "command")?)?;
        let name = command.name();
        let target = target(&required(params, "url")?, &text(params, "jolokiaPath"))?;
        match command {
            Command::Queues if !target.queue.is_empty() => {
                return Err(Error::new(
                    "QUEUES lists every queue: drop the queue from the URL, or narrow the list with Filter",
                ));
            }
            Command::Browse | Command::Send | Command::Receive if target.queue.is_empty() => {
                return Err(Error::new(format!(
                    "Add the queue to the endpoint: {name} http://host:8161/<queue>"
                )));
            }
            _ => {}
        }
        let mut properties = Map::new();
        for (key, value) in error::headers(params)?.as_object().into_iter().flatten() {
            properties.insert(key.clone(), json!(error::string(value)));
        }
        let body = text(params, "body");
        if command != Command::Send && (!properties.is_empty() || !body.is_empty()) {
            return Err(Error::new(format!(
                "{name} takes no #headers or #body: they are the message of a SEND"
            )));
        }
        use Command::*;
        for (key, option, commands) in [
            ("count", "Count", &[Queues, Browse, Receive][..]),
            ("filter", "Filter", &[Queues]),
            ("selector", "Selector", &[Browse, Receive]),
            ("durable", "Durable", &[Send]),
        ] {
            if !text(params, key).trim().is_empty() && !commands.contains(&command) {
                let names: Vec<_> = commands.iter().map(|command| command.name()).collect();
                return Err(Error::new(format!(
                    "{option} applies to {}, not {name}",
                    names.join(", ")
                )));
            }
        }
        let durable = match text(params, "durable").trim().to_ascii_lowercase().as_str() {
            "" | "true" | "yes" => true,
            "false" | "no" => false,
            _ => return Err(Error::new("Durable must be true or false")),
        };
        let columns = columns::Options::parse(params).map_err(Error::without_debug)?;
        columns
            .apply(&mut grid(command.columns(), json!({}), vec![]))
            .map_err(Error::new)?;
        Ok(Self {
            count: count(
                params,
                match command {
                    Command::Queues => 100,
                    Command::Browse => 10,
                    Command::Send | Command::Receive => 1,
                },
            )?,
            command,
            target,
            properties,
            body,
            filter: text(params, "filter").trim().to_owned(),
            selector: text(params, "selector").trim().to_owned(),
            durable,
            columns,
        })
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
        target: &request.target,
        started,
        calls: vec![],
    };
    let result = match request.command {
        Command::Queues => queues(&mut jolokia, &request).await,
        Command::Browse => browse(&mut jolokia, &request).await,
        Command::Send => send(&mut jolokia, &request).await,
        Command::Receive => receive(&mut jolokia, &request).await,
    };
    let mut debug = json!({
        "request":{
            "command":request.command.name(),"url":request.target.shown,
            "jolokia":request.target.jolokia,
        },
        "calls":jolokia.calls,
    });
    let duration = started.elapsed().as_millis();
    match result {
        Ok(mut result) => {
            request.columns.apply(&mut result).map_err(Error::new)?;
            result["durationMs"] = json!(duration);
            result["debug"] = debug;
            Ok(result)
        }
        Err(mut error) => {
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
            Err(error)
        }
    }
}

struct Jolokia<'a> {
    host: &'a Host,
    target: &'a Target,
    started: Instant,
    /// What the debug panel shows: each HTTP call with the password masked.
    calls: Vec<Value>,
}

impl Jolokia<'_> {
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
        let mut headers = json!({
            "Content-Type":"application/json","Accept":"application/json",
            "Origin":self.target.origin,
        });
        if let Some(user) = &self.target.user {
            let credentials = format!("{user}:{}", self.target.password.as_deref().unwrap_or(""));
            headers["Authorization"] = json!(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(credentials)
            ));
        }
        let request = HttpRequest {
            method: "POST".into(),
            url: self.target.jolokia.clone(),
            headers: http::header_list(&headers),
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
            return Err(http_error(self.target, &response, &reply));
        }
        let reply: Value = serde_json::from_str(&reply).map_err(|_| {
            Error::new(format!(
                "Expected a Jolokia reply from {}, got: {}",
                self.target.jolokia,
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

fn http_error(target: &Target, response: &HttpResponse, reply: &str) -> Error {
    let status = response.status;
    let url = &target.jolokia;
    Error::new(match status {
        300..=399 => {
            let location = response
                .headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case("location"))
                .map_or("another address", |header| header.value.as_str());
            format!(
                "{url} redirects to {location} (HTTP {status}): check http or https, the port and Jolokia-Path"
            )
        }
        401 | 403 if target.user.is_none() => format!(
            "Artemis asks for a login (HTTP {status}): put it in the endpoint, http://user:password@host:8161/…"
        ),
        401 | 403 => format!(
            "Artemis refused the request (HTTP {status}): check the user and password in the endpoint, that the user has a management role, and that jolokia-access.xml allows the origin {}",
            target.origin
        ),
        404 => format!(
            "No Jolokia at {url} (HTTP 404): use the broker's web console port (8161 by default) or set Jolokia-Path"
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

async fn queue(jolokia: &mut Jolokia<'_>, name: &str) -> Result<String> {
    let pattern = format!(
        "{DOMAIN}:component=addresses,subcomponent=queues,queue={},*",
        quote(name)
    );
    let names = jolokia
        .one(json!({"type":"search","mbean":pattern}))
        .await?;
    found(names, || {
        format!("Queue {name} does not exist; QUEUES lists the broker's queues")
    })
}

async fn queues(jolokia: &mut Jolokia<'_>, request: &Request) -> Result<Value> {
    let names = jolokia
        .one(json!({"type":"search","mbean":format!("{DOMAIN}:broker=*")}))
        .await?;
    let broker = found(names, || {
        format!(
            "No Artemis broker at {}; is it the web console of Apache ActiveMQ Artemis?",
            jolokia.target.jolokia
        )
    })?;
    let filtered = !request.filter.is_empty();
    let options = json!({
        "field":if filtered {"name"} else {""},"operation":if filtered {"CONTAINS"} else {""},
        "value":request.filter,"sortOrder":"asc","sortColumn":"name",
    });
    let listed = jolokia
        .one(json!({
            "type":"exec","mbean":broker,"operation":"listQueues(java.lang.String,int,int)",
            "arguments":[options.to_string(),1,request.count],
        }))
        .await?;
    let listed: Value = listed
        .as_str()
        .and_then(|listed| serde_json::from_str(listed).ok())
        .ok_or_else(|| Error::new("Artemis listQueues did not return JSON"))?;
    let rows: Vec<Value> = listed["data"]
        .as_array()
        .into_iter()
        .flatten()
        .map(queue_row)
        .collect();
    let shown = rows.len();
    let mut types = Map::new();
    for column in &QUEUE_COLUMNS[3..11] {
        types.insert((*column).into(), json!("number"));
    }
    types.insert("durable".into(), json!("boolean"));
    types.insert("paused".into(), json!("boolean"));
    let mut result = grid(&QUEUE_COLUMNS, Value::Object(types), rows);
    let total = number(&listed["count"]).as_u64().unwrap_or(0);
    if total > shown as u64 {
        warn(
            &mut result,
            vec![format!(
                "Showing {shown} of {total} queues; raise Count or narrow the list with Filter"
            )],
        );
    }
    Ok(result)
}

/// `listQueues` reports every field as a string.
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

fn queue_row(queue: &Value) -> Value {
    json!({
        "queue":queue["name"],"address":queue["address"],"routing-type":queue["routingType"],
        "messages":number(&queue["messageCount"]),"delivering":number(&queue["deliveringCount"]),
        "scheduled":number(&queue["scheduledCount"]),"consumers":number(&queue["consumerCount"]),
        "added":number(&queue["messagesAdded"]),"acked":number(&queue["messagesAcked"]),
        "killed":number(&queue["messagesKilled"]),"expired":number(&queue["messagesExpired"]),
        "durable":flag(&queue["durable"]),"paused":flag(&queue["paused"]),
    })
}

fn browsing(mbean: &str, request: &Request) -> Value {
    if request.selector.is_empty() {
        json!({"type":"exec","mbean":mbean,"operation":"browse(int,int)","arguments":[1,request.count]})
    } else {
        json!({
            "type":"exec","mbean":mbean,"operation":"browse(int,int,java.lang.String)",
            "arguments":[1,request.count,request.selector],
        })
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
    let mbean = queue(jolokia, &request.target.queue).await?;
    let counting = if request.selector.is_empty() {
        json!({"type":"read","mbean":mbean,"attribute":["MessageCount","DeliveringCount"]})
    } else {
        json!({
            "type":"exec","mbean":mbean,"operation":"countMessages(java.lang.String)",
            "arguments":[request.selector],
        })
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
        if shown == request.count {
            warning.push_str("; raise Count to see more");
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

async fn send(jolokia: &mut Jolokia<'_>, request: &Request) -> Result<Value> {
    let mbean = queue(jolokia, &request.target.queue).await?;
    let id = jolokia
        .one(json!({
            "type":"exec","mbean":mbean,"operation":SEND,
            "arguments":[
                request.properties,TEXT_MESSAGE,request.body,request.durable,
                request.target.user,request.target.password,
            ],
        }))
        .await?;
    Ok(grid(
        &SEND_COLUMNS,
        json!({}),
        vec![json!({"queue":request.target.queue,"message-id":error::string(&id)})],
    ))
}

/// Management has no consumer: RECEIVE browses messages, then removes each one it can show
/// whole.
async fn receive(jolokia: &mut Jolokia<'_>, request: &Request) -> Result<Value> {
    let mbean = queue(jolokia, &request.target.queue).await?;
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
            .map(|id| {
                json!({"type":"exec","mbean":mbean,"operation":"removeMessage(long)","arguments":[id]})
            })
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

    #[test]
    fn endpoint_names_the_queue_and_keeps_the_password_out_of_the_url() {
        let target = target("https://ad%40min:p%3Ass@broker:8443/orders%3Fnew/", "").unwrap();
        assert_eq!(
            target,
            Target {
                jolokia: "https://broker:8443/console/jolokia".into(),
                origin: "https://broker:8443".into(),
                user: Some("ad@min".into()),
                password: Some("p:ss".into()),
                queue: "orders?new".into(),
                shown: "https://ad%40min:******@broker:8443/orders%3Fnew/".into(),
            }
        );
        assert_eq!(
            super::target("http://broker:8161", "/jolokia")
                .unwrap()
                .jolokia,
            "http://broker:8161/jolokia"
        );
        assert!(super::target("tcp://broker:61616/orders", "").is_err());
        assert!(super::target("http://broker:8161/orders?x", "").is_err());
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
