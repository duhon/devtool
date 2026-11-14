//! The REST service: a `#target` (URL), `#action` (method, GET by default), `#headers` and a `#payload` sent through the
//! Host `http` driver, with OpenAPI discovery for completion when the user asks for hints.

pub mod openapi;
mod probe;

use serde_json::Value;
use services_sdk::http::Request;
use services_sdk::lang::rest::{self, Language, discovery::Discovery, highlight};
use services_sdk::lang::{curl, endpoint, key_value};
use services_sdk::{
    Check, Chunk, Cmd, Completions, DriverCall, Event, Fold, Line, Module, Rendered, Subscriptions,
};

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

const LANGUAGE: Language = Language {
    sections: &["target", "action", "headers", "payload"],
    methods: &endpoint::METHODS,
    headers: &HEADERS,
    discovered_header: "OpenAPI header",
};

#[derive(Debug, Default)]
pub struct Rest {
    discovery: Discovery<probe::OpenApi>,
}

/// The `http` request of a check; `{{…}}` placeholders stay for the App to resolve.
pub fn request(check: &Check) -> Result<Request, String> {
    let (method, url) = LANGUAGE.request_line(check)?;
    let body: Vec<_> = check
        .text_of("payload")
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();
    let body = body.join("\n");
    Ok(Request {
        headers: key_value::pairs(check.text_of("headers")),
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

    fn highlight(
        &self,
        state: &[u8],
        lines: &[String],
        _: &[services_sdk::LineColumns],
    ) -> Vec<Line> {
        highlight::highlight(&LANGUAGE, state, lines)
    }

    fn complete(&self, check: &Check, pos: u32) -> Completions {
        rest::complete::complete(&LANGUAGE, &self.discovery, check, pos as usize)
    }

    fn schema(&self, check: &Check) -> Option<Value> {
        self.discovery.schema_of(check).cloned()
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
        let mut sections = vec![format!("#target\n{}", command.url)];
        if !command.method.eq_ignore_ascii_case("GET") {
            sections.push(format!("#action\n{}", command.method));
        }
        if let Some(headers) = curl::imported_headers(&command) {
            sections.push(format!("#headers\n{headers}"));
        }
        if let Some(body) = curl::imported_body(&command) {
            sections.push(format!("#payload\n{body}"));
        }
        Ok(sections.join("\n"))
    }
}

services_sdk::export!(Rest);
