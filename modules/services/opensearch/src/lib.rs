//! The OpenSearch service: `#endpoint` (`METHOD URL`, the index and API in the URL path),
//! `#headers` and a JSON `#body`, NDJSON for bulk and multi-search, sent through the Host
//! `http` driver; completion of indices, aliases and mapped fields from discovery.

mod discovery;
pub mod schema;

use services_sdk::http::Request;
use services_sdk::lang::rest::{self, Language, discovery::Discovery, highlight};
use services_sdk::lang::{endpoint, headers};
use services_sdk::{
    Check, Chunk, Cmd, Completions, DriverCall, Event, Fold, Line, Module, Rendered, Subscriptions,
};

/// The methods of the console's `#endpoint`.
const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
/// APIs whose body is NDJSON: one JSON document per line.
const NDJSON_APIS: [&str; 3] = ["/_bulk", "/_msearch", "/_msearch/template"];
/// Searches and bulk loads may run long.
const TIMEOUT_MS: u64 = 120_000;
const ENDPOINT: &str =
    "#endpoint must be METHOD URL, for example GET http://localhost:9200/logs-*/_search";

const LANGUAGE: Language = Language {
    sections: &["endpoint", "headers", "body"],
    methods: &METHODS,
    headers: &[
        "Authorization: ",
        "Accept: application/json",
        "Content-Type: application/json",
        "Content-Type: application/x-ndjson",
    ],
    discovered_header: "OpenSearch header",
};

#[derive(Debug, Default)]
pub struct OpenSearch {
    discovery: Discovery<discovery::Cluster>,
}

/// The `http` request of a check. Unlike REST, a GET keeps its body (search); a body
/// without a Content-Type gets the JSON one, or NDJSON's for bulk and multi-search, whose
/// body then also ends with a newline. Writes are never redirected.
pub fn request(check: &Check) -> Result<Request, String> {
    let words: Vec<_> = check.text_of("endpoint").split_whitespace().collect();
    let [method, url] = words[..] else {
        return Err(ENDPOINT.into());
    };
    let method = method.to_ascii_uppercase();
    let http = url.starts_with("http://") || url.starts_with("https://");
    if !METHODS.contains(&method.as_str()) || !http || url.contains('#') {
        return Err(ENDPOINT.into());
    }
    let mut headers = headers::pairs(check.text_of("headers"));
    let mut body = Some(check.text_of("body").to_owned()).filter(|body| !body.is_empty());
    if let Some(body) = &mut body {
        let given = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.split(';').next().unwrap_or_default().trim());
        let path = endpoint::path(url).trim_end_matches('/');
        let ndjson = NDJSON_APIS.iter().any(|api| path.ends_with(api))
            || given.is_some_and(|media| media.eq_ignore_ascii_case("application/x-ndjson"));
        if ndjson && !body.ends_with('\n') {
            body.push('\n');
        }
        if given.is_none() {
            let media = if ndjson {
                "application/x-ndjson"
            } else {
                "application/json"
            };
            headers.push(("Content-Type".into(), media.into()));
        }
    }
    Ok(Request {
        headers,
        body,
        timeout_ms: Some(TIMEOUT_MS),
        redirects: Some(0),
        ..Request::new(&method, url)
    })
}

impl Module for OpenSearch {
    type Msg = ();

    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Document(check) => self.discovery.document(&check),
            Event::Hints { check, pos } => self.discovery.hints(&check, pos as usize),
            Event::Discover(check) => self.discovery.manual(&check),
            Event::Reply { tag, result } => self.discovery.reply(&tag, result).unwrap_or_default(),
            _ => vec![],
        }
    }

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            document: true,
            hints: true,
            discover: true,
            ..Subscriptions::default()
        }
    }

    fn highlight(&self, state: &[u8], lines: &[String]) -> Vec<Line> {
        highlight::highlight(&LANGUAGE, state, lines)
    }

    fn complete(&self, check: &Check, pos: u32) -> Completions {
        rest::complete::complete(&LANGUAGE, &self.discovery, check, pos as usize)
    }

    fn fold(&self, check: &Check) -> Vec<Fold> {
        highlight::folds(&check.text)
    }

    fn build(&self, check: &Check) -> Result<DriverCall, String> {
        Ok(request(check)?.call())
    }

    fn render(&self, chunk: &Chunk) -> Result<Rendered, String> {
        rest::render(&chunk.response())
    }
}

services_sdk::export!(OpenSearch);
