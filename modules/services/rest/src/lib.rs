//! The REST service: a `#endpoint` (`METHOD URL`), `#headers` and a `#body` sent through the
//! Host `http` driver, with OpenAPI discovery for completion when the user asks for hints.

mod complete;
mod discovery;
mod highlight;
pub mod openapi;

use serde_json::Value;
use services_sdk::http::{Request, Response};
use services_sdk::lang::{curl, endpoint, headers};
use services_sdk::{
    Check, Chunk, Cmd, Completions, DriverCall, Event, Fold, Line, Module, Rendered, Subscriptions,
};

pub use complete::HEADERS;

#[derive(Debug, Default)]
pub struct Rest {
    discovery: discovery::Discovery,
}

/// The `http` request of a check; `{{…}}` placeholders stay for the App to resolve.
pub fn request(check: &Check) -> Result<Request, String> {
    let (method, url) =
        endpoint::request_line(check.text_of("endpoint")).ok_or("#endpoint must be METHOD URL")?;
    let body: Vec<_> = check
        .text_of("body")
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();
    let body = body.join("\n");
    Ok(Request {
        headers: headers::pairs(check.text_of("headers")),
        body: (!body.trim().is_empty()).then_some(body),
        ..Request::new(&method, url)
    })
}

impl Module for Rest {
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
        highlight::highlight(state, lines)
    }

    fn complete(&self, check: &Check, pos: u32) -> Completions {
        complete::complete(&self.discovery, check, pos as usize)
    }

    fn fold(&self, check: &Check) -> Vec<Fold> {
        highlight::folds(&check.text)
    }

    fn build(&self, check: &Check) -> Result<DriverCall, String> {
        Ok(request(check)?.call())
    }

    fn render(&self, chunk: &Chunk) -> Result<Rendered, String> {
        let response = Response::parse(&chunk.response())?;
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
        })
    }

    fn export(&self, check: &Check) -> Result<String, String> {
        let request = request(check)?;
        Ok(curl::export(
            &request.method,
            &request.url,
            &request.headers,
            request.body.as_deref().unwrap_or(""),
        ))
    }

    fn import(&self, text: &str) -> Result<String, String> {
        let command = curl::parse(text)?;
        let mut sections = vec![format!("#endpoint\n{} {}", command.method, command.url)];
        if let Some(headers) = curl::imported_headers(&command) {
            sections.push(format!("#headers\n{headers}"));
        }
        if let Some(body) = curl::imported_body(&command) {
            sections.push(format!("#body\n{body}"));
        }
        Ok(sections.join("\n"))
    }
}

services_sdk::export!(Rest);
