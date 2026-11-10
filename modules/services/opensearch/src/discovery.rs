//! Discovery: finds the cluster root behind a request URL, also under a reverse-proxy
//! mount, and reads its mappings and aliases with the check's headers. It never writes.

use crate::schema;
use serde_json::{Value, json};
use services_sdk::Failure;
use services_sdk::driver::{DriverFailure, Stage};
use services_sdk::http::{Request, Response};
use services_sdk::lang::rest::discovery::{Probe, Step};
use url::Url;

/// API families that need only their paths: discovering them skips the mappings.
const SERVICE_APIS: [&str; 5] = ["_cluster", "_cat", "_nodes", "_tasks", "_snapshot"];

#[derive(Debug, Default)]
pub struct Cluster;

#[derive(Debug)]
pub struct Run {
    address: Option<Url>,
    /// The roots not tried yet, longest first.
    roots: Vec<Url>,
    /// The cluster root once found.
    base: Option<Url>,
    attempts: Vec<Value>,
    /// The first failure, a 401/403 one preferred.
    failure: Option<String>,
}

/// Where the cluster may be mounted: the URL's path prefixes before its first `_api`
/// segment, longest first, without query.
fn roots(url: &Url) -> Vec<Url> {
    let parts: Vec<_> = url
        .path()
        .split('/')
        .filter(|part| !part.is_empty())
        .take_while(|part| !part.starts_with('_'))
        .collect();
    (0..=parts.len())
        .rev()
        .map(|length| {
            let mut root = url.clone();
            root.set_path(&match length {
                0 => "/".to_owned(),
                _ => format!("/{}/", parts[..length].join("/")),
            });
            root.set_query(None);
            root.set_fragment(None);
            root
        })
        .collect()
}

fn opensearch(info: &Value) -> bool {
    info["version"]["number"].is_string()
        && (info["version"]["distribution"] == "opensearch"
            || info["tagline"]
                .as_str()
                .is_some_and(|tagline| tagline.contains("OpenSearch")))
}

/// The cache scope of a request to a service API under `base`, e.g. `base/_cluster`:
/// its paths-only discovery must not hide the index schema from other requests.
fn service_scope(base: &Url, url: &Url) -> Option<Url> {
    let family = url.path().strip_prefix(base.path())?.split('/').next()?;
    if !SERVICE_APIS.contains(&family) {
        return None;
    }
    let mut scope = base.clone();
    scope.path_segments_mut().ok()?.pop_if_empty().push(family);
    Some(scope)
}

fn http_error(status: u16) -> String {
    format!(
        "OpenSearch discovery failed (HTTP {status}); check the target and metadata permissions"
    )
}

/// A reply as its status and body as JSON (`null` when it is not), or why it failed and
/// whether the server was unreachable.
fn answer(reply: Result<Value, Failure>) -> Result<(u16, Value), (String, bool)> {
    match reply {
        Ok(value) => {
            let response = Response::parse(&value).map_err(|error| (error, false))?;
            let body = response.body.as_deref().unwrap_or("");
            Ok((
                response.status,
                serde_json::from_str(body).unwrap_or(Value::Null),
            ))
        }
        Err(failure) => {
            let stage = DriverFailure::parse(&failure.details()).map(|f| f.stage);
            let unreachable = matches!(stage, Some(Stage::Dns | Stage::Connect | Stage::Tls));
            Err((failure.message, unreachable))
        }
    }
}

impl Run {
    fn failed(&mut self, message: String, unreachable: bool) -> Step {
        let address = self.address.as_ref().map(Url::as_str);
        Step::Failed {
            message,
            warning: false,
            log: json!({"url": address, "attempts": self.attempts}),
            unreachable,
        }
    }

    /// The next root, or the failure that explains why none answered.
    fn next_root(&mut self) -> Step {
        if self.roots.is_empty() {
            let message = self
                .failure
                .take()
                .unwrap_or_else(|| "No OpenSearch cluster found at this target".to_owned());
            return self.failed(message, false);
        }
        Step::Fetch(vec![self.roots.remove(0).to_string()])
    }

    fn root(&mut self, url: String, reply: Result<Value, Failure>) -> Step {
        let (status, info) = match answer(reply) {
            Ok(answer) => answer,
            Err((message, unreachable)) => {
                self.attempts.push(json!({"url": url, "error": message}));
                return self.failed(message, unreachable);
            }
        };
        self.attempts.push(json!({"url": url, "status": status}));
        let Ok(root) = Url::parse(&url) else {
            return self.next_root();
        };
        if status == 200 && opensearch(&info) {
            let address = self.address.clone().unwrap_or_else(|| root.clone());
            if let Some(scope) = service_scope(&root, &address) {
                return found(&address, &root, schema::services(&root, &scope));
            }
            let metadata = ["_mapping", "_alias"]
                .iter()
                .filter_map(|path| root.join(path).ok())
                .map(String::from)
                .collect();
            self.base = Some(root);
            return Step::Fetch(metadata);
        }
        // A refused authorization explains the miss better than a proxy path's 404.
        if status != 200 && (self.failure.is_none() || matches!(status, 401 | 403)) {
            self.failure = Some(http_error(status));
        }
        self.next_root()
    }

    fn metadata(&mut self, base: Url, replies: Vec<(String, Result<Value, Failure>)>) -> Step {
        let mut documents = vec![];
        for ((url, reply), (path, key)) in replies
            .into_iter()
            .zip([("_mapping", "mappings"), ("_alias", "aliases")])
        {
            let (status, document) = match answer(reply) {
                Ok(answer) => answer,
                Err((message, unreachable)) => {
                    self.attempts.push(json!({"url": url, "error": message}));
                    return self.failed(message, unreachable);
                }
            };
            self.attempts.push(json!({"url": url, "status": status}));
            if status != 200 {
                return self.failed(http_error(status), false);
            }
            let valid = document
                .as_object()
                .is_some_and(|indices| indices.values().all(|index| index[key].is_object()));
            if !valid {
                return self.failed(
                    format!("OpenSearch returned invalid discovery metadata for /{path}"),
                    false,
                );
            }
            documents.push(document);
        }
        let [mappings, aliases] = &documents[..] else {
            return self.failed("OpenSearch discovery metadata is incomplete".into(), false);
        };
        let address = self.address.clone().unwrap_or_else(|| base.clone());
        found(&address, &base, schema::indices(&base, mappings, aliases))
    }
}

fn found(address: &Url, base: &Url, summary: Value) -> Step {
    Step::Found {
        details: json!({"address": address.as_str(), "cluster": base.as_str()}),
        log: format!("OpenSearch cluster: {base}"),
        summary,
    }
}

impl Probe for Cluster {
    type Run = Run;

    fn start(&self, address: &str) -> (Run, Vec<String>) {
        let address = Url::parse(address).ok();
        let mut run = Run {
            roots: address.as_ref().map(roots).unwrap_or_default(),
            address,
            base: None,
            attempts: vec![],
            failure: None,
        };
        let first = match run.next_root() {
            Step::Fetch(urls) => urls,
            _ => vec![],
        };
        (run, first)
    }

    /// Discovery reads metadata only: it is neither redirected nor slow.
    fn request(&self, request: Request) -> Request {
        Request {
            redirects: Some(0),
            ..request
        }
    }

    fn step(&self, run: &mut Run, replies: Vec<(String, Result<Value, Failure>)>) -> Step {
        if let Some(base) = run.base.take() {
            return run.metadata(base, replies);
        }
        match replies.into_iter().next() {
            Some((url, reply)) => run.root(url, reply),
            None => run.next_root(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_strip_the_api_and_query_and_keep_a_proxy_mount() {
        let url =
            Url::parse("https://example.test/proxy/opensearch/logs-*/_search?size=5").unwrap();
        assert_eq!(
            roots(&url).iter().map(Url::as_str).collect::<Vec<_>>(),
            [
                "https://example.test/proxy/opensearch/logs-*/",
                "https://example.test/proxy/opensearch/",
                "https://example.test/proxy/",
                "https://example.test/"
            ]
        );
    }

    #[test]
    fn only_service_apis_get_their_own_scope() {
        let base = Url::parse("http://localhost:9201/proxy/").unwrap();
        let scope = service_scope(&base, &base.join("_cluster/health").unwrap()).unwrap();
        assert_eq!(scope.as_str(), "http://localhost:9201/proxy/_cluster");
        for path in ["logs/_search", "_search", "_bulk"] {
            assert!(
                service_scope(&base, &base.join(path).unwrap()).is_none(),
                "{path}"
            );
        }
    }
}
