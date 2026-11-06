//! OpenAPI discovery: when the user asks for hints the check's address is probed for a spec
//! at known locations through the `http` driver; specs are cached per API base and an
//! address is tried once. An address with `{{…}}` variables is resolved through the App
//! first; completion finds the schema of the resolved address it remembers.

use crate::openapi::{self, CANDIDATES};
use serde_json::{Value, json};
use services_sdk::driver::{DriverFailure, Stage};
use services_sdk::http::{Request, Response};
use services_sdk::lang::{endpoint, headers, marker};
use services_sdk::{Check, Cmd, Failure, cmd};

/// Cached schemas, the most addresses remembered as tried and URLs as resolved.
const CACHE: usize = 32;
const ATTEMPTED: usize = 128;
const RESOLVED: usize = 64;
const TAG: &str = "discover:";
const RESOLVE: &str = "resolve:";

#[derive(Clone, Debug, PartialEq)]
struct Target {
    /// `scheme://authority/path`, without query or fragment.
    address: String,
    headers: Vec<(String, String)>,
}

#[derive(Debug)]
struct Run {
    id: u64,
    target: Target,
    groups: Vec<Vec<String>>,
    group: usize,
    replies: Vec<Option<Result<Value, Failure>>>,
    attempts: Vec<Value>,
    /// The first failure, a 401/403 one preferred.
    failure: Option<(String, Option<u16>)>,
}

/// Why a URL with variables is being resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Purpose {
    /// For completion only.
    Remember,
    Hints,
    Manual,
}

#[derive(Debug)]
struct Resolving {
    url: String,
    headers: Vec<(String, String)>,
    purpose: Purpose,
}

#[derive(Debug, Default)]
pub struct Discovery {
    /// `(base, summary)`, most recent last.
    schemas: Vec<(String, Value)>,
    /// Addresses and origins discovery ran for.
    attempted: Vec<String>,
    run: Option<Run>,
    runs: u64,
    /// The latest other address hints asked for while a run was in flight.
    pending: Option<Target>,
    /// URLs with `{{…}}` variables and what they resolved to, most recent last.
    resolved: Vec<(String, String)>,
    resolving: Vec<(u64, Resolving)>,
    resolves: u64,
}

/// The URL of an endpoint section: `METHOD URL` or a bare URL.
pub fn endpoint_url(endpoint: &str) -> Option<&str> {
    let mut words = endpoint.split_whitespace();
    let first = words.next()?;
    Some(words.next().unwrap_or(first))
}

/// The address discovery keys on: an `http(s)://authority/path` URL without its query.
pub fn address(url: &str) -> Option<String> {
    let url = endpoint::Url::parse(url.trim())?;
    matches!(url.scheme.to_ascii_lowercase().as_str(), "http" | "https")
        .then(|| format!("{}{}", url.origin(), url.path))
}

/// The schema discovered at `base` covers `address`: same origin, path under its base.
fn reuses(base: &str, address: &str) -> bool {
    let (Some(base), Some(next)) = (endpoint::Url::parse(base), endpoint::Url::parse(address))
    else {
        return false;
    };
    let path = base.path.trim_end_matches('/');
    base.origin() == next.origin()
        && (next.path == path || next.path.starts_with(&format!("{path}/")))
}

/// The base a summary found for `address` covers.
fn schema_base(address: &str, summary: &Value) -> Option<String> {
    if summary["paths"].as_array().is_none_or(Vec::is_empty) {
        return None;
    }
    let url = endpoint::Url::parse(address)?;
    let path = match summary["baseUrl"].as_str() {
        Some(base) => {
            let base = endpoint::Url::parse(base)?;
            if base.origin() != url.origin() {
                return None;
            }
            base.path.to_owned()
        }
        None => summary["basePath"].as_str()?.to_owned(),
    };
    let path = if path.is_empty() { "/" } else { &path };
    Some(format!("{}{path}", url.origin()))
}

fn candidates(address: &str) -> Vec<Vec<String>> {
    let Some(url) = endpoint::Url::parse(address) else {
        return vec![];
    };
    let parts: Vec<_> = url.path.split('/').filter(|s| !s.is_empty()).collect();
    (0..=parts.len())
        .rev()
        .map(|i| {
            let prefix: String = parts[..i].iter().map(|part| format!("/{part}")).collect();
            CANDIDATES
                .iter()
                .map(|candidate| format!("{}{prefix}{candidate}", url.origin()))
                .collect()
        })
        .collect()
}

fn status(text: &str, indicator: &str) -> Cmd {
    cmd::status(text, indicator)
}

/// The URL a check discovers from: the one of its `#endpoint`. With `pos` on the endpoint,
/// the URL ends there, so a typed `/` commits the URL before it.
fn url_at(check: &Check, pos: Option<usize>) -> Option<&str> {
    let typed = pos.and_then(|pos| {
        let text = &check.text;
        let before = text.get(..pos.min(text.len()))?;
        let line = before.matches('\n').count() as u32;
        let section = check
            .sections
            .iter()
            .filter(|section| section.line <= line)
            .max_by_key(|section| section.line)?;
        if section.name != "endpoint" {
            return None;
        }
        let start = before.rfind('\n').map_or(0, |at| at + 1);
        let prefix = &before[start..];
        let prefix = if section.line == line {
            marker(prefix).map_or("", |(_, rest)| rest)
        } else {
            prefix
        };
        endpoint_url(prefix).filter(|url| address(url).is_some() || url.contains("{{"))
    });
    typed.or_else(|| endpoint_url(check.text_of("endpoint")))
}

fn target(url: &str, headers: Vec<(String, String)>) -> Option<Target> {
    address(url).map(|address| Target { address, headers })
}

impl Discovery {
    /// `url` with its `{{…}}` variables as last resolved; as it is otherwise.
    pub fn resolved<'a>(&'a self, url: &'a str) -> &'a str {
        if !url.contains("{{") {
            return url;
        }
        self.resolved
            .iter()
            .rev()
            .find(|(raw, _)| raw == url)
            .map_or(url, |(_, resolved)| resolved.as_str())
    }

    /// The schema whose base covers `url`, the longest base first.
    pub fn schema(&self, url: &str) -> Option<&Value> {
        let address = address(url)?;
        self.schemas
            .iter()
            .filter(|(base, _)| reuses(base, &address))
            .max_by_key(|(base, _)| base.len())
            .map(|(_, schema)| schema)
    }

    /// The schema for `url`, else the latest one of its origin.
    pub fn schema_or_origin(&self, url: &str) -> Option<&Value> {
        self.schema(url).or_else(|| {
            let origin = endpoint::Url::parse(url)?.origin();
            self.schemas
                .iter()
                .rev()
                .find(|(base, _)| endpoint::Url::parse(base).is_some_and(|b| b.origin() == origin))
                .map(|(_, schema)| schema)
        })
    }

    /// The schema for `url` first, then every cached one, the latest first.
    pub fn schemas<'a>(&'a self, url: &str) -> impl Iterator<Item = &'a Value> {
        self.schema(url)
            .into_iter()
            .chain(self.schemas.iter().rev().map(|(_, schema)| schema))
    }

    fn attempted(&self, key: &str) -> bool {
        self.attempted.iter().any(|tried| tried == key)
    }

    fn attempt(&mut self, key: String) {
        if !self.attempted(&key) {
            self.attempted.push(key);
        }
        if self.attempted.len() > ATTEMPTED {
            self.attempted.remove(0);
        }
    }

    fn known(&self, target: &Target) -> bool {
        let origin = endpoint::Url::parse(&target.address).map(|url| url.origin());
        self.attempted(&target.address)
            || origin.is_some_and(|origin| self.attempted(&origin))
            || self.schema(&target.address).is_some()
    }

    /// The check changed: an endpoint URL with variables is resolved for completion.
    pub fn document(&mut self, check: &Check) -> Vec<Cmd> {
        match url_at(check, None) {
            Some(url)
                if url.contains("{{")
                    && !self.resolved.iter().any(|(raw, _)| raw == url)
                    && !self.resolving.iter().any(|(_, r)| r.url == url) =>
            {
                self.resolve(url, vec![], Purpose::Remember)
            }
            _ => vec![],
        }
    }

    /// The user asked for hints: discover the check's address unless it is known.
    pub fn hints(&mut self, check: &Check, pos: usize) -> Vec<Cmd> {
        let Some(url) = url_at(check, Some(pos)) else {
            return vec![];
        };
        let headers = headers::pairs(check.text_of("headers"));
        if url.contains("{{") {
            return self.resolve(url, headers, Purpose::Hints);
        }
        target(url, headers).map_or_else(Vec::new, |target| self.hinted(target))
    }

    fn hinted(&mut self, target: Target) -> Vec<Cmd> {
        match &self.run {
            Some(run) if run.target.address == target.address => vec![],
            Some(_) => {
                self.pending = Some(target);
                vec![]
            }
            None if self.known(&target) => vec![],
            None => self.start(target),
        }
    }

    /// Discovery on demand: the check's address again, cached or not.
    pub fn manual(&mut self, check: &Check) -> Vec<Cmd> {
        let url = url_at(check, None).unwrap_or("");
        let headers = headers::pairs(check.text_of("headers"));
        if url.contains("{{") {
            return self.resolve(url, headers, Purpose::Manual);
        }
        self.discover(url, headers)
    }

    fn discover(&mut self, url: &str, headers: Vec<(String, String)>) -> Vec<Cmd> {
        match target(url, headers) {
            Some(target) => self.start(target),
            None => vec![status(
                "Discovery · WARNING · Invalid or incomplete service address for discovery",
                "warning",
            )],
        }
    }

    /// Asks the App for `url` with its variables resolved.
    fn resolve(&mut self, url: &str, headers: Vec<(String, String)>, purpose: Purpose) -> Vec<Cmd> {
        self.resolves += 1;
        self.resolving.push((
            self.resolves,
            Resolving {
                url: url.to_owned(),
                headers,
                purpose,
            },
        ));
        vec![cmd::resolve(format!("{RESOLVE}{}", self.resolves), url)]
    }

    fn resolved_reply(&mut self, id: u64, result: Result<Value, Failure>) -> Vec<Cmd> {
        let Some(at) = self.resolving.iter().position(|(n, _)| *n == id) else {
            return vec![];
        };
        let (_, resolving) = self.resolving.remove(at);
        let url = match result {
            Ok(Value::String(url)) => url,
            Ok(other) => return self.unresolved(&resolving, &format!("Unexpected value {other}")),
            Err(failure) => return self.unresolved(&resolving, &failure.message),
        };
        self.resolved.retain(|(raw, _)| *raw != resolving.url);
        self.resolved.push((resolving.url, url.clone()));
        if self.resolved.len() > RESOLVED {
            self.resolved.remove(0);
        }
        match resolving.purpose {
            Purpose::Remember => vec![],
            Purpose::Hints => {
                target(&url, resolving.headers).map_or_else(Vec::new, |target| self.hinted(target))
            }
            Purpose::Manual => self.discover(&url, resolving.headers),
        }
    }

    fn unresolved(&self, resolving: &Resolving, message: &str) -> Vec<Cmd> {
        let message = format!(
            "Variables of {} could not be resolved: {message}",
            resolving.url
        );
        match resolving.purpose {
            Purpose::Manual => vec![
                status(&format!("Discovery · ERROR · {message}"), "error"),
                cmd::log("error", &message),
            ],
            _ => vec![cmd::log("warning", &message)],
        }
    }

    fn start(&mut self, target: Target) -> Vec<Cmd> {
        self.runs += 1;
        self.pending = None;
        let groups = candidates(&target.address);
        self.run = Some(Run {
            id: self.runs,
            target,
            groups,
            group: 0,
            replies: vec![],
            attempts: vec![],
            failure: None,
        });
        let mut cmds = vec![status("Discovery…", "busy")];
        cmds.extend(self.issue());
        cmds
    }

    fn issue(&mut self) -> Vec<Cmd> {
        let Some(run) = &mut self.run else {
            return vec![];
        };
        let group = &run.groups[run.group];
        run.replies = vec![None; group.len()];
        group
            .iter()
            .enumerate()
            .map(|(i, url)| {
                let request = Request {
                    headers: run.target.headers.clone(),
                    timeout_ms: Some(5000),
                    ..Request::new("GET", url)
                };
                cmd::driver(format!("{TAG}{}:{i}", run.id), &request.call())
            })
            .collect()
    }

    /// A driver reply; `None` when the tag is not discovery's.
    pub fn reply(&mut self, tag: &str, result: Result<Value, Failure>) -> Option<Vec<Cmd>> {
        if let Some(id) = tag.strip_prefix(RESOLVE) {
            return Some(self.resolved_reply(id.parse().ok()?, result));
        }
        let (id, index) = tag.strip_prefix(TAG)?.split_once(':')?;
        let (id, index): (u64, usize) = (id.parse().ok()?, index.parse().ok()?);
        let Some(run) = self.run.as_mut().filter(|run| run.id == id) else {
            return Some(vec![]);
        };
        if let Some(slot) = run.replies.get_mut(index) {
            *slot = Some(result);
        }
        if run.replies.iter().any(Option::is_none) {
            return Some(vec![]);
        }
        Some(self.evaluate())
    }

    fn evaluate(&mut self) -> Vec<Cmd> {
        let run = self.run.as_mut().expect("a discovery run");
        let group = run.groups[run.group].clone();
        let search_base = group[0]
            .strip_suffix(CANDIDATES[0])
            .unwrap_or(&group[0])
            .to_owned();
        let mut transport_only = true;
        let mut found = None;
        for (url, reply) in group.iter().zip(std::mem::take(&mut run.replies)) {
            let (status, message, failed) = match reply.expect("every reply") {
                Ok(value) => {
                    transport_only = false;
                    let response = match Response::parse(&value) {
                        Ok(response) => response,
                        Err(error) => {
                            run.attempts.push(json!({"url": url, "error": error}));
                            continue;
                        }
                    };
                    if !(200..300).contains(&response.status) {
                        let text = format!("HTTP {} {}", response.status, response.status_text);
                        let failed = ![404, 410].contains(&response.status);
                        (Some(response.status), text.trim_end().to_owned(), failed)
                    } else {
                        let body = response.body.as_deref().unwrap_or("");
                        match serde_json::from_str::<Value>(body) {
                            Ok(spec) => {
                                if openapi::is_spec(&spec)
                                    && let Ok(mut summary) =
                                        openapi::summarize_at(&spec, &search_base)
                                    && summary["methodsByPath"]
                                        .as_object()
                                        .is_some_and(|methods| !methods.is_empty())
                                {
                                    summary["specUrl"] = json!(url);
                                    found = Some(summary);
                                    break;
                                }
                                run.attempts.push(
                                    json!({"url": url, "outcome": "No OpenAPI or Swagger operations"}),
                                );
                                continue;
                            }
                            Err(error) => (
                                None,
                                format!("error decoding response body: {error}"),
                                false,
                            ),
                        }
                    }
                }
                Err(failure) => {
                    let stage = DriverFailure::parse(&failure.details()).map(|f| f.stage);
                    transport_only &=
                        matches!(stage, Some(Stage::Dns | Stage::Connect | Stage::Tls));
                    (None, failure.message, stage != Some(Stage::Decode))
                }
            };
            run.attempts
                .push(json!({"url": url, "status": status, "error": message}));
            if failed && (run.failure.is_none() || matches!(status, Some(401 | 403))) {
                run.failure = Some((message, status));
            }
        }
        if let Some(summary) = found {
            let run = self.run.take().expect("a discovery run");
            let mut cmds = vec![
                cmd::status_with(
                    "Discovery · OK",
                    "ok",
                    &json!({"address": run.target.address, "specUrl": summary["specUrl"]}),
                ),
                cmd::log(
                    "info",
                    &format!(
                        "OpenAPI schema: {}",
                        summary["specUrl"].as_str().unwrap_or("")
                    ),
                ),
            ];
            self.attempt(run.target.address.clone());
            if let Some(base) = schema_base(&run.target.address, &summary) {
                self.schemas.retain(|(cached, _)| *cached != base);
                self.schemas.push((base, summary));
                if self.schemas.len() > CACHE {
                    self.schemas.remove(0);
                }
            }
            cmds.extend(self.resume());
            return cmds;
        }
        run.group += 1;
        if !transport_only && run.group < run.groups.len() {
            return self.issue();
        }
        let run = self.run.take().expect("a discovery run");
        let log = json!({"url": run.target.address, "attempts": run.attempts}).to_string();
        let mut cmds = match run.failure {
            Some((message, _)) => vec![
                status(&format!("Discovery · ERROR · {message}"), "error"),
                cmd::log("error", &log),
            ],
            None => vec![
                status(
                    "Discovery · WARNING · No OpenAPI or Swagger schema found at known discovery locations",
                    "warning",
                ),
                cmd::log("warning", &log),
            ],
        };
        if transport_only && let Some(url) = endpoint::Url::parse(&run.target.address) {
            self.attempt(url.origin());
        }
        self.attempt(run.target.address);
        cmds.extend(self.resume());
        cmds
    }

    /// Discovers the address hints asked for during a run.
    fn resume(&mut self) -> Vec<Cmd> {
        match self.pending.take() {
            Some(target) if !self.known(&target) => self.start(target),
            _ => vec![],
        }
    }
}
