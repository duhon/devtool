//! The language of REST-like services: a `#endpoint` (`METHOD URL`), `#headers` and a JSON
//! `#body`, with completion from a discovered OpenAPI-like summary (`paths`,
//! `methodsByPath`, `headers`, `bodySchemas`, `baseUrl`/`basePath`) and the discovery runs
//! that find it through the `http` driver.

pub mod complete;
pub mod discovery;
pub mod highlight;

use crate::Rendered;
use crate::http::Response;
use serde_json::Value;

/// What a service's REST language offers.
#[derive(Clone, Copy, Debug)]
pub struct Language {
    pub sections: &'static [&'static str],
    pub methods: &'static [&'static str],
    /// `Name: value` hints of the headers section.
    pub headers: &'static [&'static str],
    /// The detail of header names a discovered summary adds.
    pub discovered_header: &'static str,
}

/// An HTTP response as the console shows it: a JSON body pretty-printed, the outcome by
/// status class.
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
