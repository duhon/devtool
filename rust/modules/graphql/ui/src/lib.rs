//! The GraphQL console part: checks become GraphQL requests, `curl` command lines are
//! imported and exported.

use serde_json::{Value, json};
use services_module_ui::{Imported, Sections, ServiceUi, command, curl, imported_headers};

/// The module's manifest, for consoles that know the module before a host lists it.
pub const MANIFEST: &str = include_str!("../../manifest.json");

pub struct Graphql;

impl ServiceUi for Graphql {
    fn request(&self, s: &Sections) -> Result<Value, String> {
        let endpoint = s.text("endpoint");
        if endpoint.contains(char::is_whitespace) {
            return Err("#endpoint must be a GraphQL URL".into());
        }
        let mut out = json!({"url":endpoint,"headers":s.pairs("headers")});
        if s.has("query") {
            out["query"] = json!(s.text("query"));
        }
        if !s.text("variables").is_empty() {
            out["variables"] = serde_json::from_str(s.text("variables"))
                .map_err(|_| "Variables must be valid JSON")?;
        }
        Ok(out)
    }

    fn import(&self, command: &str) -> Result<Imported, String> {
        let p = command::parse_curl(command)?;
        let body = p["body"].as_str().unwrap_or("");
        let parsed = serde_json::from_str::<Value>(body).unwrap_or(Value::Null);
        let query = parsed["query"].as_str().unwrap_or(body);
        if query.trim().is_empty() {
            return Err("Could not find a GraphQL query in the curl command".into());
        }
        let mut sections = imported_headers(&p);
        if let Some(vars) = parsed
            .get("variables")
            .filter(|v| v.as_object().is_some_and(|m| !m.is_empty()))
        {
            sections.push((
                "variables".into(),
                serde_json::to_string_pretty(vars).unwrap(),
            ));
        }
        sections.push(("query".into(), query.trim().into()));
        Ok(Imported {
            endpoint: p["url"].as_str().unwrap().into(),
            sections,
        })
    }

    fn export(&self, p: &Value) -> Result<String, String> {
        let mut headers = p["headers"].as_object().cloned().unwrap_or_default();
        if !headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("content-type"))
        {
            headers.insert("Content-Type".into(), json!("application/json"));
        }
        let mut body = json!({});
        if let Some(q) = p.get("query") {
            body["query"] = q.clone();
        }
        if let Some(v) = p.get("variables") {
            body["variables"] = v.clone();
        }
        Ok(curl(
            "POST",
            p["url"].as_str().unwrap_or(""),
            headers,
            &serde_json::to_string_pretty(&body).unwrap(),
        ))
    }
}

services_module_ui::export!(Graphql);
