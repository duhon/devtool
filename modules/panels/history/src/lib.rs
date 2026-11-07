//! The History panel: every finished run with a check is saved through `history.insert`,
//! its source, title and response with secrets masked — the values of sensitive variables
//! the run used, sensitive fields and headers of the request, and URL credentials.

use serde_json::{Value, json};
use services_sdk::{Cmd, DriverCall, Module, cmd};

const MASK: &str = "[redacted]";

/// Names whose values never show in history.
pub fn sensitive_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "pwd",
        "token",
        "secret",
        "credential",
        "api_key",
        "apikey",
        "api-key",
        "authorization",
        "cookie",
        "private_key",
    ]
    .iter()
    .any(|part| name.contains(part))
}

/// Masks known secrets, then URL credentials (`scheme://user:pass@`).
pub struct Redactor {
    secrets: Vec<String>,
}

const KEYS: [&str; 10] = [
    "authorization",
    "set-cookie",
    "cookie",
    "password",
    "passwd",
    "pwd",
    "token",
    "api_key",
    "api-key",
    "secret",
];

/// The `key: value` / `key=value` assignments of sensitive keys in `text` (case
/// insensitive; `apikey` too): their values, quoted or up to a line end, `,`, `}` or `&`.
fn assignments(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut keys: Vec<&str> = KEYS.to_vec();
    keys.push("apikey");
    let mut values = Vec::new();
    for key in keys {
        let mut from = 0;
        while let Some(found) = lower[from..].find(key) {
            let start = from + found + key.len();
            from = start;
            let rest = &text[start..];
            let skipped = rest
                .find(|c: char| !(c.is_whitespace() || c == '"' || c == '\''))
                .unwrap_or(rest.len());
            let rest = &rest[skipped..];
            let Some(rest) = rest.strip_prefix([':', '=']) else {
                continue;
            };
            let rest = rest.trim_start();
            let value = match rest.chars().next() {
                Some(quote @ ('"' | '\'')) => rest[1..].split(quote).next().unwrap_or(""),
                _ => rest.split(['\r', '\n', ',', '}', '&']).next().unwrap_or(""),
            };
            values.push(value.trim().to_owned());
        }
    }
    values
}

/// The `user:pass` of each `scheme://user:pass@` in `text`.
fn credentials(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("://") {
        let start = from + at + 3;
        from = start;
        let scheme_ok = text[..start - 3]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
        let rest = &text[start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '/' || c == '@')
            .unwrap_or(rest.len());
        if scheme_ok && end > 0 && rest[end..].starts_with('@') {
            found.push(rest[..end].to_owned());
        }
    }
    found
}

impl Redactor {
    /// Secrets of a run: `secrets` (values of sensitive variables), the values of sensitive
    /// keys and of sensitive assignments in the `request` and the `source`, URL credentials.
    pub fn new(request: &Value, source: &str, mut secrets: Vec<String>) -> Self {
        fn collect(value: &Value, secrets: &mut Vec<String>, leaves: &mut Vec<String>) {
            match value {
                Value::Object(fields) => {
                    for (key, value) in fields {
                        if sensitive_name(key)
                            && let Some(text) = value.as_str()
                        {
                            secrets.push(text.to_owned());
                        }
                        collect(value, secrets, leaves);
                    }
                }
                Value::Array(values) => {
                    // A `[name, value]` header pair.
                    if let [Value::String(name), Value::String(value)] = values.as_slice()
                        && sensitive_name(name)
                    {
                        secrets.push(value.clone());
                    }
                    for value in values {
                        collect(value, secrets, leaves);
                    }
                }
                Value::String(text) => {
                    if let Ok(json) = serde_json::from_str::<Value>(text)
                        && !json.is_string()
                    {
                        collect(&json, secrets, leaves);
                    }
                    leaves.push(text.clone());
                }
                _ => {}
            }
        }
        let mut leaves = vec![source.to_owned()];
        collect(request, &mut secrets, &mut leaves);
        for leaf in &leaves {
            for value in assignments(leaf).into_iter().chain(credentials(leaf)) {
                if let Some(token) = value.strip_prefix("Bearer ") {
                    secrets.push(token.to_owned());
                }
                if let Some((_, password)) = value.split_once(':') {
                    secrets.push(password.to_owned());
                }
                secrets.push(value);
            }
        }
        let mut parts = Vec::new();
        for secret in &secrets {
            if let Some(token) = secret.strip_prefix("Bearer ") {
                parts.push(token.to_owned());
            }
            for cookie in secret.split(';') {
                if let Some((_, value)) = cookie.split_once('=') {
                    parts.push(value.trim().to_owned());
                }
            }
        }
        secrets.extend(parts);
        secrets.retain(|value| !value.is_empty());
        secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
        secrets.dedup();
        Self { secrets }
    }

    pub fn text(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for secret in &self.secrets {
            text = text.replace(secret, MASK);
        }
        for credential in credentials(&text) {
            text = text.replace(&format!("://{credential}@"), &format!("://{MASK}@"));
        }
        text
    }
}

#[derive(Default)]
pub struct History;

impl Module for History {
    type Msg = ();

    /// `response` is the run record (`Modules::finished`); a run without a check is not
    /// kept, nor one whose source did not resolve (it may hold secrets unmasked).
    fn after_response(&self, call: &DriverCall, response: &Value) -> Vec<Cmd> {
        let (Some(source), Some(check), Some(session)) = (
            response["source"].as_str(),
            response["checkId"].as_str(),
            response["sessionId"].as_str(),
        ) else {
            return Vec::new();
        };
        let secrets = response["secrets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let request = if call.driver.is_empty() {
            Value::Null
        } else {
            call.request()
        };
        let redactor = Redactor::new(&request, source, secrets);
        let text = format!(
            "{}\n\n#responce {}\n{}",
            redactor.text(source),
            response["outcome"].as_str().unwrap_or(""),
            response["text"].as_str().unwrap_or("")
        );
        vec![cmd::history_insert(
            check,
            &json!({
                "checkId": check,
                "sessionId": session,
                "title": redactor.text(response["title"].as_str().unwrap_or("")),
                "module": response["service"],
                "started": response["started"],
                "ended": response["ended"],
                "status": response["status"],
                "text": text,
                "defer": response["defer"] == true,
            }),
        )]
    }
}

services_sdk::export!(History);
