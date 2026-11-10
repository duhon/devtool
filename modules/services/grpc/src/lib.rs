//! The gRPC service: a `#target` (`grpc[s]://target/package.Service/Method`), metadata
//! `#headers`, an optional `#proto` and a JSON `#payload`, called through the Host `grpc`
//! driver. Without `#proto` the descriptors come from server reflection, asked while the
//! check is edited; the response prints as JSON. `grpcurl` command lines are imported and
//! exported.

pub mod proto;
pub mod reflection;

use prost_reflect::DescriptorPool;
use reflection::{Found, Goal, Run, Server, Step};
use serde_json::{Value, json};
use services_sdk::grpc::{self, Request, Response};
use services_sdk::lang::check::At;
use services_sdk::lang::{curl, endpoint, finish, headers, item, json, marker, placeholder};
use services_sdk::run::{self, Answer, OneCall};
use services_sdk::{
    Check, Cmd, Completions, DriverCall, Event, Failure, Fold, Line, Module, Rendered,
    Subscriptions, Token, cmd,
};
use std::collections::BTreeMap;

pub const SECTIONS: [&str; 4] = ["target", "headers", "proto", "payload"];

pub const HEADERS: [&str; 9] = [
    "Accept: */*",
    "Accept-Encoding: gzip, deflate, br",
    "Authorization: ****** application/json",
    "Cookie:",
    "Host:",
    "User-Agent:",
    "X-Api-Key:",
    "X-Forwarded-For:",
    "X-Requested-With: XMLHttpRequest",
];

/// A call may take this long.
pub const TIMEOUT_MS: u64 = 120_000;
pub const CONNECT_MS: u64 = 5000;

/// The field of a driver request with what its answer is decoded with (`proto`, the
/// `#proto` source): the parts of a check the driver does not take, and ignores.
pub const CONSOLE: &str = "console";

const RESOLVE: &str = ":resolve";

/// Reflection results kept, the most recent last.
const KEPT: usize = 16;

/// `target`, `package.Service` and `Method` of an endpoint; the last two are empty
/// without a `/Service/Method` path.
pub fn parse_endpoint(endpoint: &str) -> (String, String, String) {
    let endpoint = endpoint.trim();
    let offset = endpoint.find("://").map_or(0, |i| i + 3);
    let Some(slash) = endpoint[offset..].find('/').map(|i| i + offset) else {
        return (endpoint.into(), String::new(), String::new());
    };
    match endpoint[slash + 1..].rsplit_once('/') {
        Some((service, method)) => (
            endpoint[..slash].into(),
            service.trim().into(),
            method.trim().into(),
        ),
        None => (endpoint[..slash].into(), String::new(), String::new()),
    }
}

/// The target and the path after it (`""` without one).
fn target_path(endpoint: &str) -> (&str, &str) {
    let offset = endpoint.find("://").map_or(0, |i| i + 3);
    match endpoint[offset..].find('/').map(|i| offset + i) {
        Some(slash) => (&endpoint[..slash], &endpoint[slash + 1..]),
        None => (endpoint, ""),
    }
}

/// The request metadata: `#headers` with lowercased names; a `-bin` value is base64.
pub fn metadata(check: &Check) -> Vec<(String, String)> {
    headers::pairs(check.text_of("headers"))
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value))
        .collect()
}

fn server(check: &Check, target: &str) -> Server {
    Server {
        target: target.into(),
        metadata: metadata(check),
    }
}

/// The `grpc` request of a check whose variables are resolved, with descriptors from
/// `pool`.
pub fn request(check: &Check, pool: &DescriptorPool, message: &str) -> Result<Value, String> {
    let (target, service, method) = parse_endpoint(check.text_of("target"));
    if target.is_empty() {
        return Err("target is required".into());
    }
    if service.is_empty() || method.is_empty() {
        return Err("serviceName and methodName are required".into());
    }
    let method = proto::method(pool, &service, &method)?;
    let mut messages = proto::messages(message)?;
    if !method.is_client_streaming() {
        messages.truncate(1);
        if messages.is_empty() {
            messages.push(json!({}));
        }
    }
    let mut request = Request {
        metadata: metadata(check),
        timeout_ms: Some(TIMEOUT_MS),
        connect_ms: Some(CONNECT_MS),
        ..Request::new(&target, &service, method.name())
    };
    for message in &messages {
        request = request.message(&proto::encode(&method.input(), message)?);
    }
    let mut value = serde_json::to_value(&request).map_err(|error| error.to_string())?;
    let source = check.text_of("proto");
    value[CONSOLE] = if source.trim().is_empty() {
        json!({})
    } else {
        json!({"proto": source})
    };
    Ok(value)
}

/// A response printed as JSON: the message, every message of a server stream, or
/// `{"error": …}` for a non-zero status. Without descriptors the messages print in base64.
pub fn render(
    path: &str,
    pool: Option<&DescriptorPool>,
    response: &Value,
) -> Result<Rendered, String> {
    let response = Response::parse(response)?;
    let outcome = if response.status == 0 { "OK" } else { "ERROR" };
    let (service, method) = path
        .trim_start_matches('/')
        .rsplit_once('/')
        .unwrap_or_default();
    let method = pool.map(|pool| proto::method(pool, service, method));
    let body = match method {
        _ if response.status != 0 => json!({"error": response.message}),
        Some(Ok(method)) => {
            let mut messages = response
                .messages()?
                .iter()
                .map(|bytes| proto::decode(&method.output(), bytes))
                .collect::<Result<Vec<_>, _>>()?;
            if method.is_server_streaming() {
                json!(messages)
            } else if messages.is_empty() {
                json!({})
            } else {
                messages.remove(0)
            }
        }
        Some(Err(message)) => return Err(message),
        None => json!({"messages": response.messages}),
    };
    Ok(Rendered {
        text: serde_json::to_string_pretty(&body).unwrap_or_default(),
        language: "json".into(),
        outcome: outcome.into(),
        columns: Vec::new(),
    })
}

/// A `grpcurl` command line for a check.
pub fn export(check: &Check) -> Result<String, String> {
    let endpoint = check.text_of("target").trim();
    let secure = endpoint.to_ascii_lowercase().starts_with("grpcs://");
    let bare = endpoint
        .get(..8)
        .filter(|scheme| scheme.eq_ignore_ascii_case("grpcs://"))
        .map(|_| &endpoint[8..])
        .or_else(|| {
            endpoint
                .get(..7)
                .filter(|scheme| scheme.eq_ignore_ascii_case("grpc://"))
                .map(|_| &endpoint[7..])
        })
        .unwrap_or(endpoint);
    let (target, symbol) = bare
        .split_once('/')
        .ok_or("#target must be target/Service/Method")?;
    let mut lines = vec!["grpcurl".to_owned()];
    if !secure {
        lines.push("  -plaintext".into());
    }
    lines.push("  -emit-defaults".into());
    for (name, value) in headers::pairs(check.text_of("headers")) {
        lines.push(format!("  -H {}", curl::quote(&format!("{name}: {value}"))));
    }
    let message = check.text_of("payload");
    if !message.trim().is_empty() {
        let value: Value =
            serde_json::from_str(message).map_err(|_| "Request message must be valid JSON")?;
        let pretty = serde_json::to_string_pretty(&value).unwrap_or_default();
        lines.push(format!("  -d {}", curl::quote(&pretty)));
    }
    lines.extend([format!("  {target}"), format!("  {symbol}")]);
    Ok(lines.join(" \\\n"))
}

/// A check from a `grpcurl` command line; `-plaintext` picks `grpc://`, else `grpcs://`.
pub fn import(command: &str) -> Result<String, String> {
    let words = curl::shell_words(command)?;
    if !words
        .first()
        .is_some_and(|word| word.eq_ignore_ascii_case("grpcurl"))
    {
        return Err("Command must start with \"grpcurl\"".into());
    }
    let mut headers: Vec<(String, String)> = vec![];
    let mut data = None;
    let mut positional = vec![];
    let mut plaintext = false;
    let mut i = 1;
    while i < words.len() {
        let word = &words[i];
        let value = words.get(i + 1).cloned().unwrap_or_default();
        match word.to_ascii_lowercase().as_str() {
            "-h" | "--header" | "-header" | "-rpc-header" | "-metadata" => {
                if let Some((name, value)) = value.split_once(':') {
                    let (name, value) = (name.trim(), value.trim());
                    match headers.iter_mut().find(|(known, _)| known == name) {
                        Some(pair) => pair.1 = value.into(),
                        None => headers.push((name.into(), value.into())),
                    }
                }
                i += 1;
            }
            "-d" | "-data" | "--data" => {
                data = Some(value);
                i += 1;
            }
            "-plaintext" => plaintext = true,
            "-insecure"
            | "-v"
            | "-verbose"
            | "-vv"
            | "-format-error"
            | "-emit-defaults"
            | "-unix"
            | "-keepalive-time"
            | "-allow-unknown-fields" => {}
            _ if word.starts_with('-') => {
                if !value.is_empty() && !value.starts_with('-') {
                    i += 1;
                }
            }
            _ => positional.push(word.clone()),
        }
        i += 1;
    }
    let (Some(mut symbol), Some(target)) = (positional.pop(), positional.pop()) else {
        return Err(
            "Could not find both a target and a Service/Method in the grpcurl command".into(),
        );
    };
    if !symbol.contains('/')
        && let Some(dot) = symbol.rfind('.')
    {
        symbol.replace_range(dot..=dot, "/");
    }
    let scheme = if plaintext { "grpc" } else { "grpcs" };
    let mut sections = vec![format!("#target\n{scheme}://{target}/{symbol}")];
    if !headers.is_empty() {
        let lines: Vec<_> = headers.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        sections.push(format!("#headers\n{}", lines.join("\n")));
    }
    if let Some(data) = data.filter(|data| !data.is_empty() && data != "@") {
        sections.push(format!("#payload\n{}", curl::pretty(&data)));
    }
    Ok(sections.join("\n"))
}

#[derive(Debug, Default)]
pub struct Grpc {
    /// Services of servers, the most recent last.
    services: Vec<(Server, Result<Vec<String>, String>)>,
    /// Descriptors of a server's service, the most recent last.
    pools: Vec<((Server, String), Result<DescriptorPool, String>)>,
    runs: Vec<Run>,
    count: u64,
    /// A quiet reflection asked for while another ran.
    waiting: Option<(Server, Goal)>,
    /// Runs resolving their variables: the check and its descriptors.
    resolving: BTreeMap<String, (Check, DescriptorPool)>,
    calls: OneCall,
}

fn keep<K: PartialEq, V>(list: &mut Vec<(K, V)>, key: K, value: V) {
    list.retain(|(known, _)| *known != key);
    list.push((key, value));
    if list.len() > KEPT {
        list.remove(0);
    }
}

impl Grpc {
    fn known_pool(
        &self,
        server: &Server,
        service: &str,
    ) -> Option<&Result<DescriptorPool, String>> {
        self.pools
            .iter()
            .rev()
            .find(|((known, name), _)| known == server && name == service)
            .map(|(_, pool)| pool)
    }

    fn running(&self, server: &Server, goal: &Goal) -> bool {
        self.runs
            .iter()
            .any(|run| run.server == *server && run.goal == *goal)
    }

    /// The descriptors of the check's service: its `#proto` compiled, or what reflection
    /// found.
    pub fn pool(&self, check: &Check) -> Result<DescriptorPool, String> {
        let source = check.text_of("proto");
        if !source.trim().is_empty() {
            return proto::compile(source);
        }
        let (target, service, _) = parse_endpoint(check.text_of("target"));
        if target.is_empty() || service.is_empty() {
            return Err("serviceName and methodName are required".into());
        }
        let server = server(check, &target);
        match self.known_pool(&server, &service) {
            Some(pool) => pool.clone(),
            None if self.running(&server, &Goal::Service(service.clone())) => Err(format!(
                "The descriptors of {service} are being loaded through server reflection; run again in a moment"
            )),
            None => Err(format!(
                "The descriptors of {service} are not loaded; add #proto or run Discover"
            )),
        }
    }

    /// The services reflection found on the check's server.
    pub fn services(&self, check: &Check, target: &str) -> Vec<String> {
        let server = server(check, target);
        self.services
            .iter()
            .rev()
            .find(|(known, _)| *known == server)
            .and_then(|(_, services)| services.as_ref().ok().cloned())
            .unwrap_or_default()
    }

    fn start(&mut self, server: Server, goal: Goal, loud: bool) -> Vec<Cmd> {
        if self.running(&server, &goal) {
            return vec![];
        }
        if !loud && !self.runs.is_empty() {
            self.waiting = Some((server, goal));
            return vec![];
        }
        self.count += 1;
        let (run, ask) = Run::start(self.count, server, goal, loud);
        self.runs.push(run);
        let mut cmds = vec![ask];
        if loud {
            cmds.insert(0, cmd::status("Discovery…", "busy"));
        }
        cmds
    }

    /// The descriptors of a service for a run, unless known or `#proto` has them.
    fn descriptors(&mut self, check: &Check, retry: bool, loud: bool) -> Vec<Cmd> {
        let (target, service, _) = parse_endpoint(check.text_of("target"));
        if !check.text_of("proto").trim().is_empty() || target.is_empty() || service.is_empty() {
            return vec![];
        }
        let server = server(check, &target);
        match self.known_pool(&server, &service) {
            Some(Ok(_)) if !retry => vec![],
            Some(Err(_)) if !retry => vec![],
            _ => self.start(server, Goal::Service(service), loud),
        }
    }

    fn document(&mut self, check: &Check) -> Vec<Cmd> {
        let (_, _, method) = parse_endpoint(check.text_of("target"));
        if method.is_empty() {
            return vec![];
        }
        self.descriptors(check, false, false)
    }

    /// A run: the descriptors must be known; the endpoint, metadata and message are
    /// resolved when it starts.
    fn run(&mut self, run: String, check: Check) -> Vec<Cmd> {
        let pool = match run::no_options(&check, "gRPC").and_then(|()| self.pool(&check)) {
            Ok(pool) => pool,
            Err(error) => return vec![cmd::fail(&run, &error, &Value::Null)],
        };
        let sections = json!({"target": check.text_of("target"),
            "headers": check.text_of("headers"), "payload": check.text_of("payload")});
        self.resolving.insert(run.clone(), (check, pool));
        vec![run::resolve_value(format!("{run}{RESOLVE}"), &sections)]
    }

    fn resolved(&mut self, run: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        let Some((check, pool)) = self.resolving.remove(run) else {
            return vec![];
        };
        let call = run::resolved_value(result).and_then(|sections| {
            let check = run::with_sections(&check, &sections);
            request(&check, &pool, sections["payload"].as_str().unwrap_or(""))
        });
        match call {
            Ok(request) => vec![
                self.calls
                    .call(run, DriverCall::new(grpc::DRIVER, &request)),
            ],
            Err(error) => vec![cmd::fail(run, &error, &Value::Null)],
        }
    }

    fn hints(&mut self, check: &Check, pos: usize) -> Vec<Cmd> {
        let at = At::new(&check.text, pos);
        let endpoint = if at.name() == Some("target") {
            at.before.trim()
        } else {
            check.text_of("target").trim()
        };
        let (target, path) = target_path(endpoint);
        if !discoverable(target) {
            return vec![];
        }
        if !path.contains('/') {
            let server = server(check, target);
            let known = self
                .services
                .iter()
                .any(|(known, found)| *known == server && found.is_ok());
            return if known {
                vec![]
            } else {
                self.start(server, Goal::Services, true)
            };
        }
        let (target, service, _) = parse_endpoint(check.text_of("target"));
        let failed = matches!(
            self.known_pool(&server(check, &target), &service),
            Some(Err(_))
        );
        self.descriptors(check, failed, true)
    }

    fn manual(&mut self, check: &Check) -> Vec<Cmd> {
        let source = check.text_of("proto");
        if !source.trim().is_empty() {
            return vec![match proto::compile(source) {
                Ok(_) => cmd::status("Discovery · OK", "ok"),
                Err(message) => cmd::status(&format!("Discovery · ERROR · {message}"), "error"),
            }];
        }
        let (target, service, _) = parse_endpoint(check.text_of("target"));
        if !discoverable(&target) {
            return vec![cmd::status(
                "Discovery · WARNING · Invalid or incomplete service address for discovery",
                "warning",
            )];
        }
        let goal = if service.is_empty() {
            Goal::Services
        } else {
            Goal::Service(service)
        };
        self.start(server(check, &target), goal, true)
    }

    fn reflected(&mut self, id: u64, result: Result<Value, Failure>) -> Vec<Cmd> {
        let Some(at) = self.runs.iter().position(|run| run.id == id) else {
            return vec![];
        };
        let step = self.runs[at].reply(result);
        if let Ok(Step::Ask(ask)) = step {
            return vec![ask];
        }
        let run = self.runs.remove(at);
        let mut cmds = match (step, &run.goal) {
            (Ok(Step::Done(Found::Services(services))), _) => {
                let count = services.len();
                keep(&mut self.services, run.server.clone(), Ok(services));
                vec![
                    cmd::status_with("Discovery · OK", "ok", &json!({"services": count})),
                    cmd::log("info", &format!("gRPC reflection found {count} services")),
                ]
            }
            (Ok(Step::Done(Found::Pool(pool))), Goal::Service(service)) => {
                let count = pool
                    .get_service_by_name(service)
                    .map_or(0, |found| found.methods().len());
                keep(
                    &mut self.pools,
                    (run.server.clone(), service.clone()),
                    Ok(pool),
                );
                vec![
                    cmd::status_with("Discovery · OK", "ok", &json!({"methods": count})),
                    cmd::log(
                        "info",
                        &format!("gRPC reflection found {count} methods of {service}"),
                    ),
                ]
            }
            (Err(message), goal) => {
                match goal {
                    Goal::Services => {
                        keep(&mut self.services, run.server.clone(), Err(message.clone()))
                    }
                    Goal::Service(service) => keep(
                        &mut self.pools,
                        (run.server.clone(), service.clone()),
                        Err(message.clone()),
                    ),
                }
                vec![
                    cmd::status(&format!("Discovery · ERROR · {message}"), "error"),
                    cmd::log("error", &format!("gRPC reflection failed: {message}")),
                ]
            }
            _ => vec![],
        };
        if !run.loud {
            cmds.retain(|cmd| cmd.name != "status.set");
        }
        if self.runs.is_empty()
            && let Some((server, goal)) = self.waiting.take()
        {
            cmds.extend(self.start(server, goal, false));
        }
        cmds
    }

    fn reply(&mut self, tag: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        if let Some(id) = reflection::id(tag) {
            return self.reflected(id, result);
        }
        if let Some(run) = tag.strip_suffix(RESOLVE) {
            return self.resolved(run, result);
        }
        let mut calls = std::mem::take(&mut self.calls);
        let render = |call: &DriverCall, response: &Value| {
            let call = call.request();
            render(
                call["path"].as_str().unwrap_or(""),
                self.response_pool(&call).as_ref(),
                response,
            )
            .map(Answer::from)
        };
        let cmds = calls.reply(tag, result, render).unwrap_or_default();
        self.calls = calls;
        cmds
    }

    /// The descriptors a response is decoded with: its call's `#proto`, else what
    /// reflection found for its service.
    fn response_pool(&self, call: &Value) -> Option<DescriptorPool> {
        if let Some(source) = call[CONSOLE]["proto"].as_str() {
            return proto::compile(source).ok();
        }
        let service = call["path"]
            .as_str()?
            .trim_start_matches('/')
            .rsplit_once('/')?
            .0;
        self.pools
            .iter()
            .rev()
            .find_map(|((_, name), pool)| (name == service).then(|| pool.as_ref().ok()).flatten())
            .cloned()
    }
}

/// A target reflection can ask: a host, without spaces.
fn discoverable(target: &str) -> bool {
    let authority = target.split_once("://").map_or(target, |(_, rest)| rest);
    !authority.is_empty() && !target.contains(char::is_whitespace)
}

/// The part of a check a line is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Area {
    /// Before the first marker, or a rendered response highlighted alone: JSON.
    Prelude,
    Endpoint,
    Headers,
    Proto,
    Message,
    Other,
    Response,
}

impl Area {
    const ALL: [Area; 7] = [
        Area::Prelude,
        Area::Endpoint,
        Area::Headers,
        Area::Proto,
        Area::Message,
        Area::Other,
        Area::Response,
    ];

    fn json(self) -> bool {
        matches!(self, Area::Prelude | Area::Message | Area::Response)
    }
}

fn token(start: usize, end: usize, kind: &str) -> Token {
    Token {
        start: start as u32,
        len: (end - start) as u32,
        kind: kind.into(),
    }
}

/// An endpoint line: the scheme `keyword`, the target `def`, the service `property` and the
/// method `variable-3`.
pub fn endpoint_tokens(line: &str) -> Vec<Token> {
    let (target, _) = target_path(line);
    let mut out: Vec<Token> = endpoint::tokens(target, false)
        .into_iter()
        .filter(|token| token.kind != "string")
        .collect();
    let mut separators = 0;
    let mut pos = target.len();
    while pos < line.len() {
        if let Some(end) = placeholder(line, pos) {
            pos = end;
            continue;
        }
        if line[pos..].starts_with('/') {
            separators += 1;
            pos += 1;
            continue;
        }
        let end = line[pos..]
            .find(['/', '{'])
            .map_or(line.len(), |end| pos + end.max(1));
        let kind = if separators > 1 {
            "variable-3"
        } else {
            "property"
        };
        out.push(token(pos, end, kind));
        pos = end;
    }
    out
}

fn whole(line: &str, kind: &str) -> Vec<Token> {
    if line.is_empty() {
        return vec![];
    }
    vec![token(0, line.len(), kind)]
}

/// Highlighting of a gRPC check: the endpoint, metadata, the JSON message and response.
pub fn highlight(state: &[u8], lines: &[String]) -> Vec<Line> {
    let (mut area, rest) = match state.split_first() {
        Some((area, rest)) => (
            Area::ALL
                .get(*area as usize)
                .copied()
                .unwrap_or(Area::Other),
            rest,
        ),
        None => (Area::Prelude, &[][..]),
    };
    let mut body = json::State::decode(rest);
    lines
        .iter()
        .map(|line| {
            let named = marker(line).map(|(name, _)| name);
            let is_marker = named.as_deref().is_some_and(|name| {
                area != Area::Response || matches!(name, "responce" | "response")
            });
            let tokens = if is_marker {
                area = match named.as_deref() {
                    Some("target") => Area::Endpoint,
                    Some("headers") => Area::Headers,
                    Some("proto") => Area::Proto,
                    Some("payload") => Area::Message,
                    Some("responce" | "response") => Area::Response,
                    _ => Area::Other,
                };
                body = json::State::default();
                whole(line, "comment")
            } else {
                match area {
                    Area::Endpoint => endpoint_tokens(line),
                    Area::Headers => headers::tokens(line),
                    Area::Prelude | Area::Message | Area::Response => json::tokens(line, &mut body),
                    Area::Proto | Area::Other => vec![],
                }
            };
            let mut state = vec![area as u8];
            if area.json() {
                body.encode(&mut state);
            }
            Line { tokens, state }
        })
        .collect()
}

/// Folds of the `{`/`[` blocks in `#proto`, `#payload` and the response.
pub fn folds(text: &str) -> Vec<Fold> {
    let mut out = vec![];
    let mut region: Vec<&str> = vec![];
    let mut start = 0;
    let mut flush = |region: &mut Vec<&str>, start: usize| {
        out.extend(json::folds(region.drain(..)).into_iter().map(|fold| Fold {
            start: fold.start + start as u32,
            end: fold.end + start as u32,
        }));
    };
    let mut blocks = false;
    for (i, line) in text.split('\n').enumerate() {
        if let Some((name, _)) = marker(line) {
            flush(&mut region, start);
            blocks = matches!(name.as_str(), "proto" | "payload" | "responce" | "response");
            start = i + 1;
            continue;
        }
        if blocks {
            region.push(line);
        }
    }
    flush(&mut region, start);
    out
}

/// Completion in a gRPC check: markers, schemes, services and methods from reflection,
/// metadata and `#payload` fields of the method's input.
pub fn complete(module: &Grpc, check: &Check, pos: usize) -> Completions {
    let at = At::new(&check.text, pos);
    if at.in_placeholder() || at.in_response() {
        return at.none();
    }
    if let Some(markers) = at.markers(&SECTIONS) {
        return markers;
    }
    let line_start = at.line_start(at.line);
    match at.name() {
        Some("target") => {
            if let Some(next) = at.next_section(&SECTIONS) {
                return next;
            }
            let value = at.before.trim_start();
            let from = line_start + at.before.len() - value.len();
            let (target, path) = target_path(value);
            if value.len() > target.len() {
                let method = path.contains('/');
                let names = if method {
                    let service = path.rsplit_once('/').map_or("", |(service, _)| service);
                    module
                        .known_pool(&server(check, target), service)
                        .and_then(|pool| pool.as_ref().ok())
                        .and_then(|pool| pool.get_service_by_name(service))
                        .map(|found| found.methods().map(|m| m.name().to_owned()).collect())
                        .unwrap_or_default()
                } else {
                    module
                        .services(check, target)
                        .into_iter()
                        .map(|service| format!("{service}/"))
                        .collect::<Vec<_>>()
                };
                let start = if method {
                    value.rfind('/').map_or(0, |i| i + 1)
                } else {
                    target.len() + 1
                };
                let items = names
                    .into_iter()
                    .map(|name| item(name, "gRPC reflection", "path"))
                    .collect();
                return finish(from + start, at.pos, &value[start..], items);
            }
            finish(
                from,
                at.pos,
                value,
                vec![
                    item("grpc://", "Plain text", "url"),
                    item("grpcs://", "TLS", "url"),
                ],
            )
        }
        Some("headers") => {
            let used = headers::used(at.other_lines());
            let mut result = finish(
                line_start,
                at.pos,
                at.before.trim(),
                headers::items(&HEADERS, &[], &used),
            );
            if at.before.trim().is_empty() && !used.is_empty() {
                let mut items = finish(line_start, at.pos, "", at.unused(&SECTIONS)).items;
                items.append(&mut result.items);
                result.items = items;
            }
            result
        }
        Some("payload") => {
            let (_, service, method) = parse_endpoint(check.text_of("target"));
            let fields = module
                .pool(check)
                .and_then(|pool| proto::method(&pool, &service, &method))
                .map(|method| proto::fields(&method.input(), 0))
                .unwrap_or_default();
            let start = at.line_start(at.body_start()).min(at.pos);
            let mut result = json::complete(&Value::Array(fields), at.whole(), at.pos - start);
            result.from += start as u32;
            result.to += start as u32;
            result
        }
        _ => at.none(),
    }
}

impl Module for Grpc {
    type Msg = ();

    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Document(check) => self.document(&check),
            Event::Hints { check, pos } => self.hints(&check, pos as usize),
            Event::Discover(check) => self.manual(&check),
            Event::Reply { tag, result } => self.reply(&tag, result),
            Event::Run { tag, check } => self.run(tag, check),
            Event::Cancel { tag } => {
                self.resolving.remove(&tag);
                self.calls.cancel(&tag);
                vec![]
            }
            _ => vec![],
        }
    }

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            document: true,
            hints: true,
            discover: true,
            run: true,
            ..Subscriptions::default()
        }
    }

    fn highlight(
        &self,
        state: &[u8],
        lines: &[String],
        _: &[services_sdk::LineColumns],
    ) -> Vec<Line> {
        highlight(state, lines)
    }

    fn complete(&self, check: &Check, pos: u32) -> Completions {
        complete(self, check, pos as usize)
    }

    fn fold(&self, check: &Check) -> Vec<Fold> {
        folds(&check.text)
    }

    fn export(&self, check: &Check) -> Result<String, String> {
        export(check)
    }

    fn import(&self, text: &str) -> Result<String, String> {
        import(text)
    }
}

services_sdk::export!(Grpc);
