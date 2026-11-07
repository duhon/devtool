//! OpenAPI discovery: the specs at known locations under every prefix of the address's path,
//! the longest prefix first, its candidates at once.

use crate::openapi::{self, CANDIDATES};
use serde_json::{Value, json};
use services_sdk::Failure;
use services_sdk::driver::{DriverFailure, Stage};
use services_sdk::http::Response;
use services_sdk::lang::endpoint;
use services_sdk::lang::rest::discovery::{Probe, Step};

#[derive(Debug, Default)]
pub struct OpenApi;

#[derive(Debug)]
pub struct Run {
    address: String,
    groups: Vec<Vec<String>>,
    group: usize,
    attempts: Vec<Value>,
    /// The first failure, a 401/403 one preferred.
    failure: Option<(String, Option<u16>)>,
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

impl Probe for OpenApi {
    type Run = Run;

    fn start(&self, address: &str) -> (Run, Vec<String>) {
        let groups = candidates(address);
        let first = groups.first().cloned().unwrap_or_default();
        let run = Run {
            address: address.to_owned(),
            groups,
            group: 0,
            attempts: vec![],
            failure: None,
        };
        (run, first)
    }

    fn step(&self, run: &mut Run, replies: Vec<(String, Result<Value, Failure>)>) -> Step {
        let search_base = replies
            .first()
            .map(|(url, _)| url.strip_suffix(CANDIDATES[0]).unwrap_or(url).to_owned())
            .unwrap_or_default();
        let mut transport_only = true;
        for (url, reply) in replies {
            let (status, message, failed) = match reply {
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
                                    return Step::Found {
                                        details: json!({"address": run.address, "specUrl": url}),
                                        log: format!("OpenAPI schema: {url}"),
                                        summary,
                                    };
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
        run.group += 1;
        if !transport_only && run.group < run.groups.len() {
            return Step::Fetch(run.groups[run.group].clone());
        }
        let log = json!({"url": run.address, "attempts": run.attempts});
        let (message, warning) = match run.failure.take() {
            Some((message, _)) => (message, false),
            None => (
                "No OpenAPI or Swagger schema found at known discovery locations".to_owned(),
                true,
            ),
        };
        Step::Failed {
            message,
            warning,
            log,
            unreachable: transport_only,
        }
    }
}
