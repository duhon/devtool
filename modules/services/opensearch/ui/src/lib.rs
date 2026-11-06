//! The OpenSearch console part: `#endpoint` is `METHOD URL` like REST, the index and API
//! in the URL path; `#body` is JSON, or NDJSON for bulk and multi-search.

use serde_json::{Value, json};
use services_module_ui::{Sections, ServiceUi};

/// The module's manifest, for consoles that know the module before a host lists it.
pub const MANIFEST: &str = include_str!("../../manifest.json");

const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

pub struct OpenSearch;

impl ServiceUi for OpenSearch {
    fn request(&self, sections: &Sections) -> Result<Value, String> {
        let endpoint = sections.text("endpoint");
        let (method, url) = match endpoint.split_whitespace().collect::<Vec<_>>()[..] {
            [method, url] => (method.to_ascii_uppercase(), url),
            _ => return Err(ENDPOINT.into()),
        };
        let http = url.starts_with("http://") || url.starts_with("https://");
        if !METHODS.contains(&method.as_str()) || !http || url.contains('#') {
            return Err(ENDPOINT.into());
        }
        let mut params = json!({"method":method,"url":url,"headers":sections.pairs("headers")});
        if !sections.text("body").is_empty() {
            params["body"] = json!(sections.text("body"));
        }
        Ok(params)
    }
}

const ENDPOINT: &str =
    "#endpoint must be METHOD URL, for example GET http://localhost:9200/logs-*/_search";

services_module_ui::export!(OpenSearch);
