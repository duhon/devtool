//! Bounded, read-only, on-demand discovery. Connections are resolved before cache lookup.
use crate::language::{self, Context};
use serde_json::{Value, json};
use services_sdk::http::{Request, Response};
use services_sdk::lang::key_value;
use services_sdk::{Check, Cmd, Failure, cmd};
use std::collections::BTreeMap;
use url::Url;

const TAG: &str = "prom-discovery:";
const CACHE: usize = 16;
const ITEMS: usize = 1000;

#[derive(Clone, PartialEq, Eq)]
struct Connection {
    url: String,
    headers: Vec<(String, String)>,
}

impl Connection {
    fn of(check: &Check) -> Self {
        Self {
            url: check.text_of("target").trim().to_owned(),
            headers: key_value::pairs(check.http_metadata())
                .into_iter()
                .filter(|(name, _)| !name.to_ascii_lowercase().starts_with("query-"))
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Query {
    Metrics,
    Labels(Option<String>),
    Values(String, Option<String>),
}

impl Query {
    fn of(context: &Context) -> Option<Self> {
        match context {
            Context::Metrics => Some(Self::Metrics),
            Context::Labels { selector } => Some(Self::Labels(selector.clone())),
            Context::Values { label, selector } => {
                Some(Self::Values(label.clone(), selector.clone()))
            }
            _ => None,
        }
    }
}

struct Entry {
    connection: Connection,
    data: BTreeMap<Query, Vec<String>>,
    attempted: Vec<Query>,
}

struct Run {
    id: u64,
    raw: Connection,
    resolved: Option<Connection>,
    query: Query,
    manual: bool,
}

#[derive(Default)]
pub struct Discovery {
    current: Option<Connection>,
    alias: Option<(Connection, Connection)>,
    cache: Vec<Entry>,
    run: Option<Run>,
    requested: Option<Query>,
    ids: u64,
}

impl std::fmt::Debug for Discovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Discovery")
            .field("cached_connections", &self.cache.len())
            .field("in_flight", &self.run.is_some())
            .finish()
    }
}

impl Discovery {
    pub fn document(&mut self, check: &Check) -> Vec<Cmd> {
        let raw = Connection::of(check);
        if self.current.as_ref() != Some(&raw) {
            let mut commands = self.cancel();
            self.current = Some(raw);
            self.alias = None;
            self.requested = None;
            commands.push(cmd::status("", "idle"));
            return commands;
        }
        vec![]
    }

    pub fn hints(&mut self, check: &Check, pos: usize) -> Vec<Cmd> {
        let Some(query) = Query::of(&language::context(check, pos)) else {
            return vec![];
        };
        self.start(check, query, false)
    }

    pub fn manual(&mut self, check: &Check) -> Vec<Cmd> {
        let query = if self.current.as_ref() == Some(&Connection::of(check)) {
            self.requested.clone().unwrap_or(Query::Metrics)
        } else {
            Query::Metrics
        };
        self.start(check, query, true)
    }

    fn start(&mut self, check: &Check, query: Query, manual: bool) -> Vec<Cmd> {
        let mut commands = self.document(check);
        let raw = Connection::of(check);
        if raw.url.is_empty() {
            commands.push(cmd::status(
                "Discovery · WARNING · Missing service address",
                "warning",
            ));
            return commands;
        }
        if !manual
            && self
                .run
                .as_ref()
                .is_some_and(|run| run.raw == raw && run.query == query)
        {
            return commands;
        }
        commands.extend(self.cancel());
        self.ids += 1;
        let id = self.ids;
        self.alias = None;
        self.requested = Some(query.clone());
        let text = json!({"url": raw.url, "headers": raw.headers}).to_string();
        self.run = Some(Run {
            id,
            raw,
            resolved: None,
            query,
            manual,
        });
        commands.extend([
            cmd::status("Discovery…", "busy"),
            cmd::resolve_json(format!("{TAG}{id}:resolve"), &text),
        ]);
        commands
    }

    fn cancel(&mut self) -> Vec<Cmd> {
        self.run
            .take()
            .filter(|run| run.resolved.is_some())
            .map(|run| vec![cmd::cancel(&format!("{TAG}{}:http", run.id))])
            .unwrap_or_default()
    }

    fn entry(&self, check: &Check) -> Option<&Entry> {
        let raw = Connection::of(check);
        let (known, resolved) = self.alias.as_ref()?;
        if *known != raw {
            return None;
        }
        self.cache
            .iter()
            .find(|entry| entry.connection == *resolved)
    }

    pub fn schema_of(&self, check: &Check) -> Option<Value> {
        let entry = self.entry(check)?;
        Some(json!({
            "metrics": entry.data.get(&Query::Metrics).cloned().unwrap_or_default(),
            "labels": entry.data.get(&Query::Labels(None)).cloned().unwrap_or_default(),
        }))
    }

    pub fn candidates(&self, check: &Check, context: &Context) -> Vec<String> {
        let Some(query) = Query::of(context) else {
            return vec![];
        };
        self.entry(check)
            .and_then(|entry| entry.data.get(&query))
            .cloned()
            .unwrap_or_default()
    }

    pub fn reply(&mut self, tag: &str, result: Result<Value, Failure>) -> Option<Vec<Cmd>> {
        let rest = tag.strip_prefix(TAG)?;
        let (id, stage) = rest.split_once(':')?;
        let id: u64 = id.parse().ok()?;
        if !matches!(stage, "resolve" | "http") {
            return None;
        }
        let Some(mut run) = self.run.take() else {
            return Some(vec![]);
        };
        if run.id != id {
            self.run = Some(run);
            return Some(vec![]);
        }
        if self.current.as_ref() != Some(&run.raw) {
            return Some(vec![]);
        }
        if stage == "resolve" {
            let resolved = result
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| {
                    Some(Connection {
                        url: v["url"].as_str()?.to_owned(),
                        headers: serde_json::from_value(v["headers"].clone()).ok()?,
                    })
                });
            let Some(resolved) = resolved else {
                return Some(warning("Variable resolution failed"));
            };
            let request = match request(&resolved, &run.query) {
                Ok(request) => request,
                Err(message) => return Some(warning(message)),
            };
            self.alias = Some((run.raw.clone(), resolved.clone()));
            let index = self
                .cache
                .iter()
                .position(|entry| entry.connection == resolved);
            let index = index.unwrap_or_else(|| {
                if self.cache.len() == CACHE {
                    self.cache.remove(0);
                }
                self.cache.push(Entry {
                    connection: resolved.clone(),
                    data: BTreeMap::new(),
                    attempted: vec![],
                });
                self.cache.len() - 1
            });
            let entry = &mut self.cache[index];
            if !run.manual
                && (entry.data.contains_key(&run.query) || entry.attempted.contains(&run.query))
            {
                return Some(if entry.data.contains_key(&run.query) {
                    vec![cmd::status("", "idle"), cmd::relight()]
                } else {
                    warning("Previous discovery failed; use Discover to retry")
                });
            }
            if entry.attempted.len() >= 64 {
                entry.attempted.remove(0);
            }
            entry.attempted.push(run.query.clone());
            run.resolved = Some(resolved);
            self.run = Some(run);
            return Some(vec![cmd::driver(
                format!("{TAG}{id}:http"),
                &request.call(),
            )]);
        }
        let Some(connection) = run.resolved else {
            return Some(warning("Unexpected discovery response"));
        };
        let data = result
            .map_err(|_| "HTTP discovery failed".to_owned())
            .and_then(|v| {
                Response::parse(&v).map_err(|_| "Malformed HTTP discovery response".to_owned())
            })
            .and_then(|response| decode(&response));
        match data {
            Ok((values, warned)) => {
                if let Some(entry) = self
                    .cache
                    .iter_mut()
                    .find(|entry| entry.connection == connection)
                {
                    if entry.data.len() >= 64
                        && !entry.data.contains_key(&run.query)
                        && let Some(first) = entry.data.keys().next().cloned()
                    {
                        entry.data.remove(&first);
                    }
                    entry.data.insert(run.query, values);
                }
                Some(vec![
                    cmd::status(
                        if warned {
                            "Discovery · WARNING · Results limited or server warnings"
                        } else {
                            "Discovery · OK"
                        },
                        if warned { "warning" } else { "ok" },
                    ),
                    cmd::relight(),
                ])
            }
            Err(message) => Some(warning(&message)),
        }
    }
}

fn warning(message: &str) -> Vec<Cmd> {
    vec![cmd::status(
        &format!("Discovery · WARNING · {message}"),
        "warning",
    )]
}

fn request(connection: &Connection, query: &Query) -> Result<Request, &'static str> {
    let mut url = Url::parse(&connection.url).map_err(|_| "Invalid service address")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || connection.url.contains("{{")
    {
        return Err("Invalid or unresolved service address");
    }
    url.set_query(None);
    url.set_fragment(None);
    let base = url.path().trim_end_matches('/').to_owned();
    let base = base
        .split("/api/v1/")
        .next()
        .unwrap_or(&base)
        .trim_end_matches('/')
        .to_owned();
    let base = base.strip_suffix("/api/v1").unwrap_or(&base);
    url.set_path(&format!("{base}/api/v1"));
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| "Invalid service address")?;
        match query {
            Query::Metrics => {
                path.extend(["label", "__name__", "values"]);
            }
            Query::Labels(_) => {
                path.push("labels");
            }
            Query::Values(label, _) => {
                path.extend(["label", label, "values"]);
            }
        }
    }
    let scope = match query {
        Query::Labels(scope) | Query::Values(_, scope) => scope.as_deref(),
        _ => None,
    };
    if let Some(scope) = scope {
        url.query_pairs_mut().append_pair("match[]", scope);
    }
    url.query_pairs_mut()
        .append_pair("limit", &ITEMS.to_string());
    let mut headers = Vec::new();
    for (name, value) in &connection.headers {
        if name.to_ascii_lowercase().starts_with("query-") {
            continue;
        }
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || value.contains(['\r', '\n'])
            || value.contains("{{")
        {
            return Err("Invalid or unresolved discovery headers");
        }
        headers.push((name.clone(), value.clone()));
    }
    Ok(Request {
        headers,
        timeout_ms: Some(10_000),
        connect_ms: Some(5_000),
        redirects: Some(0),
        ..Request::new("GET", url.as_str())
    })
}

fn decode(response: &Response) -> Result<(Vec<String>, bool), String> {
    if !(200..300).contains(&response.status) {
        return Err(format!("HTTP status {}", response.status));
    }
    let body: Value = serde_json::from_str(response.body.as_deref().unwrap_or(""))
        .map_err(|_| "Malformed Prometheus discovery JSON".to_owned())?;
    if body["status"] != "success" {
        return Err("Prometheus discovery returned an error".into());
    }
    let values = body["data"]
        .as_array()
        .ok_or("Invalid Prometheus discovery data")?;
    if values.iter().any(|v| !v.is_string()) {
        return Err("Invalid Prometheus discovery values".into());
    }
    let warned = values.len() > ITEMS
        || body
            .get("warnings")
            .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()));
    let mut values: Vec<_> = values
        .iter()
        .take(ITEMS)
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    values.sort();
    values.dedup();
    Ok((values, warned))
}
