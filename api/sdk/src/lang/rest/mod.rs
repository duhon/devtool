//! The language of REST-like services: a `#target` (the URL), an `#action` (the HTTP
//! method, GET without it), `#headers` and a JSON `#payload`, with completion from a discovered OpenAPI-like summary (`paths`,
//! `methodsByPath`, `headers`, `bodySchemas`, `baseUrl`/`basePath`) and the discovery runs
//! that find it through the `http` driver.

pub mod complete;
pub mod discovery;
pub mod highlight;

use crate::http::Response;
use crate::{Check, Rendered};
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

impl Language {
    /// The uppercased method of the `#action` (GET when absent) and the URL of the
    /// `#target`, which must not carry a method.
    pub fn request_line<'a>(&self, check: &'a Check) -> Result<(String, &'a str), String> {
        let words: Vec<_> = check.text_of("target").split_whitespace().collect();
        let url = match words[..] {
            [] => return Err("#target must be a URL".into()),
            [first, ..] if self.is_method(first) => {
                return Err(format!(
                    "#target holds only the URL: put the method {} in #action",
                    first.to_ascii_uppercase()
                ));
            }
            [url] => url,
            _ => return Err("#target must be a single URL".into()),
        };
        let action = check.text_of("action").trim();
        if action.is_empty() {
            return Ok(("GET".into(), url));
        }
        let method = action.to_ascii_uppercase();
        if self.is_method(&method) {
            Ok((method, url))
        } else {
            Err(format!(
                "#action must be one HTTP method: {}",
                self.methods.join(", ")
            ))
        }
    }

    fn is_method(&self, word: &str) -> bool {
        self.methods
            .iter()
            .any(|method| method.eq_ignore_ascii_case(word))
    }
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
