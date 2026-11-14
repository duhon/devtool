//! The GraphQL service: a `#target` URL, `#metadata`, `#variables` (JSON) and a `#payload`
//! (also the text before the first marker) POSTed through the Host `http` driver, with
//! introspection for completion when the user asks for hints.

mod complete;
mod discovery;
mod highlight;
mod language;

use discovery::Connection;
use serde_json::{Value, json};
use services_sdk::http::Response;
use services_sdk::lang::{curl, key_value};
use services_sdk::run::{self, OneCall};
use services_sdk::{
    Check, Cmd, Completions, Event, Failure, Fold, Line, Module, Rendered, Subscriptions, cmd,
};
use std::collections::BTreeSet;

pub use complete::HEADERS;
pub use discovery::{INTROSPECTION_QUERY, schema};

#[derive(Debug, Default)]
pub struct Graphql {
    discovery: discovery::Discovery,
    resolving: BTreeSet<String>,
    calls: OneCall,
}

const RESOLVE: &str = ":resolve";

/// The query of a check: its `#payload`, else the text before the first marker.
fn query(check: &Check) -> &str {
    match check.section("payload") {
        Some(query) => query,
        None => check.text_of("leftover"),
    }
}

/// What a check sends.
struct Parts<'a> {
    url: &'a str,
    headers: Vec<(String, String)>,
    query: &'a str,
    variables: Option<Value>,
}

fn parts(check: &Check) -> Result<Parts<'_>, String> {
    let url = check.text_of("target").trim();
    if url.contains(char::is_whitespace) {
        return Err("#target must be a GraphQL URL".into());
    }
    let variables = check.text_of("variables").trim();
    let variables = (!variables.is_empty())
        .then(|| serde_json::from_str(variables).map_err(|_| "Variables must be valid JSON"))
        .transpose()?;
    Ok(Parts {
        url,
        headers: key_value::pairs(check.http_metadata()),
        query: query(check),
        variables,
    })
}

/// The JSON document a run resolves: the variables are spliced in as JSON text, so the
/// values the App inserts into their strings are escaped where they land.
fn document(check: &Check) -> String {
    let variables = check.text_of("variables").trim();
    format!(
        "{{\"target\":{},\"headers\":{},\"payload\":{},\"variables\":{}}}",
        json!(check.text_of("target")),
        json!(check.http_metadata()),
        json!(query(check)),
        if variables.is_empty() {
            "null"
        } else {
            variables
        }
    )
}

/// The request of a resolved document.
fn resolved_request(document: Value) -> Result<services_sdk::DriverCall, String> {
    let url = document["target"].as_str().unwrap_or("").trim();
    if url.contains(char::is_whitespace) {
        return Err("#target must be a GraphQL URL".into());
    }
    let connection = Connection {
        url: url.to_owned(),
        headers: key_value::pairs(document["headers"].as_str().unwrap_or("")),
    };
    let query = document["payload"].as_str().unwrap_or("");
    let variables = Some(document["variables"].clone()).filter(|v| !v.is_null());
    Ok(discovery::request(&connection, query, variables).call())
}

/// A GraphQL response: pretty JSON, else its text; 4xx warns, 5xx fails.
pub fn render(response: &Value) -> Result<Rendered, String> {
    let response = Response::parse(response)?;
    let body = response.body.unwrap_or_default();
    let pretty = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok());
    let outcome = match response.status {
        ..400 => "OK",
        400..500 => "WARNING",
        _ => "ERROR",
    };
    let (text, language) = match pretty {
        Some(text) => (text, "json"),
        None => (body, "text"),
    };
    Ok(Rendered {
        text,
        language: language.into(),
        outcome: outcome.into(),
        columns: Vec::new(),
    })
}

impl Graphql {
    fn run(&mut self, run: String, check: Check) -> Vec<Cmd> {
        let valid = run::no_options(&check, "GraphQL").and_then(|()| {
            if query(&check).trim().is_empty() {
                return Err("query is required".to_owned());
            }
            Ok(())
        });
        if let Err(error) = valid {
            return vec![cmd::fail(&run, &error, &Value::Null)];
        }
        self.resolving.insert(run.clone());
        vec![cmd::resolve_json(
            format!("{run}{RESOLVE}"),
            &document(&check),
        )]
    }

    fn resolved(&mut self, run: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        if !self.resolving.remove(run) {
            return vec![];
        }
        let call = result
            .map_err(|failure| failure.message)
            .and_then(|text| {
                serde_json::from_str::<Value>(text.as_str().unwrap_or(""))
                    .map_err(|_| "Variables must be valid JSON".to_owned())
            })
            .and_then(resolved_request);
        match call {
            Ok(call) => vec![self.calls.call(run, call)],
            Err(error) => vec![cmd::fail(run, &error, &Value::Null)],
        }
    }

    fn reply(&mut self, tag: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        if let Some(run) = tag.strip_suffix(RESOLVE) {
            return self.resolved(run, result);
        }
        let mut calls = std::mem::take(&mut self.calls);
        let answered = calls.reply(tag, result.clone(), |_, response| {
            render(response).map(Into::into)
        });
        self.calls = calls;
        match answered {
            Some(cmds) => cmds,
            None => self.discovery.reply(tag, result).unwrap_or_default(),
        }
    }
}

impl Module for Graphql {
    type Msg = ();

    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Hints { check, .. } => self.discovery.hints(&check),
            Event::Discover(check) => self.discovery.manual(&check),
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
        highlight::highlight(state, lines)
    }

    fn complete(&self, check: &Check, pos: u32) -> Completions {
        complete::complete(self.discovery.schema(check), check, pos as usize)
    }

    fn fold(&self, check: &Check) -> Vec<Fold> {
        highlight::folds(&check.text)
    }

    fn schema(&self, check: &Check) -> Option<Value> {
        self.discovery.schema(check).cloned()
    }

    fn marks(&self, check: &Check) -> Vec<Line> {
        self.discovery
            .schema(check)
            .map_or_else(Vec::new, |schema| highlight::marks(schema, &check.text))
    }

    fn export(&self, check: &Check) -> Result<String, String> {
        let parts = parts(check)?;
        let mut headers = parts.headers;
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        {
            headers.push(("Content-Type".into(), "application/json".into()));
        }
        let mut body = json!({});
        if !parts.query.is_empty() {
            body["query"] = json!(parts.query);
        }
        if let Some(variables) = parts.variables {
            body["variables"] = variables;
        }
        let body = serde_json::to_string_pretty(&body).map_err(|e| e.to_string())?;
        Ok(curl::export("POST", parts.url, &headers, &body))
    }

    fn import(&self, text: &str) -> Result<String, String> {
        let command = curl::parse(text)?;
        let body = command.body.as_deref().unwrap_or("");
        let parsed = serde_json::from_str::<Value>(body).unwrap_or(Value::Null);
        let query = parsed["query"].as_str().unwrap_or(body);
        if query.trim().is_empty() {
            return Err("Could not find a GraphQL query in the curl command".into());
        }
        let mut sections = vec![format!("#target\n{}", command.url)];
        if let Some(headers) = curl::imported_headers(&command) {
            sections.push(format!("#metadata\n{headers}"));
        }
        if let Some(variables) = parsed
            .get("variables")
            .filter(|v| v.as_object().is_some_and(|m| !m.is_empty()))
        {
            let variables = serde_json::to_string_pretty(variables).map_err(|e| e.to_string())?;
            sections.push(format!("#variables\n{variables}"));
        }
        sections.push(format!("#payload\n{}", query.trim()));
        Ok(sections.join("\n"))
    }
}

services_sdk::export!(Graphql);

#[cfg(test)]
mod tests;
