//! Module errors and the JSON helpers every service uses.

use crate::proto::{host::HttpFailure, module::ModuleError};
use crate::transport::{Status, code};
use serde_json::{Value, json};

/// A failed console request: `{"error","debug","durationMs"}` in the reply.
#[derive(Debug, Clone)]
pub struct Error {
    pub message: String,
    pub debug: Option<Box<Value>>,
    pub duration_ms: Option<u128>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            debug: None,
            duration_ms: None,
        }
    }

    /// An error whose reply carries no debug data: the host does not attach the request.
    pub fn without_debug(message: impl Into<String>) -> Self {
        let mut error = Self::new(message);
        error.debug = Some(Box::new(Value::Null));
        error
    }

    /// Records the request; keeps richer debug data (connection, server error) if present.
    pub fn with_debug(mut self, request: Value) -> Self {
        if self.debug.as_deref().is_some_and(Value::is_null) {
            return self;
        }
        if let Some(debug) = &mut self.debug {
            debug["request"] = request;
        } else {
            self.debug = Some(Box::new(
                json!({"request":request,"error":{"name":"Error","message":self.message}}),
            ));
        }
        self
    }

    pub fn json(&self) -> Value {
        let mut value = json!({"error":self.message});
        if let Some(debug) = self.debug.as_deref().filter(|debug| !debug.is_null()) {
            value["debug"] = debug.clone();
        }
        if let Some(duration) = self.duration_ms {
            value["durationMs"] = json!(duration);
        }
        value
    }

    /// A failed host call; HTTP failures keep the cause chain the host reported.
    pub fn from_host(status: Status) -> Self {
        match http_failure(&status) {
            Some(failure) => Self::http(&failure),
            None => Self::new(status.message.to_string()),
        }
    }

    pub fn http(failure: &HttpFailure) -> Self {
        fn details(causes: &[String]) -> Value {
            let mut value = json!({"name":"Error","message":causes[0]});
            if causes.len() > 1 {
                value["cause"] = details(&causes[1..]);
            }
            value
        }
        let detail = details(&failure.causes);
        let mut error = Self::new(format!("Request failed: {}", failure.causes[0]));
        error.debug = Some(Box::new(
            json!({"error":detail,"connection":{"events":[],"error":detail}}),
        ));
        error
    }

    pub fn into_status(self) -> Status {
        let details = ModuleError {
            debug_json: self.debug.map(|debug| debug.to_string()),
            duration_ms: self.duration_ms.map(|ms| ms as u64),
        };
        Status::new(code::UNKNOWN, &self.message).with_details(&details)
    }

    /// The host side of [`Error::into_status`].
    pub fn from_status(status: &Status) -> Self {
        let mut error = Self::new(status.message.to_string());
        if let Ok(details) = <ModuleError as prost::Message>::decode(&*status.details) {
            error.debug = details
                .debug_json
                .and_then(|debug| serde_json::from_str(&debug).ok())
                .map(Box::new);
            error.duration_ms = details.duration_ms.map(u128::from);
        }
        error
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::new(e.to_string())
    }
}

/// The details of a failed `Host/Http` or `Host/HttpStream` call.
pub fn http_failure(status: &Status) -> Option<HttpFailure> {
    if status.code != code::UNAVAILABLE {
        return None;
    }
    <HttpFailure as prost::Message>::decode(&*status.details)
        .ok()
        .filter(|failure| !failure.causes.is_empty())
}

pub fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

pub fn string(v: &Value) -> String {
    v.as_str()
        .map(str::to_string)
        .unwrap_or_else(|| v.to_string())
}

pub fn required(v: &Value, key: &str) -> Result<String> {
    let value = text(v, key);
    if value.trim().is_empty() {
        Err(Error::new(format!("{key} is required")))
    } else {
        Ok(value)
    }
}

/// The `headers` param as an object of name/value pairs.
pub fn headers(params: &Value) -> Result<Value> {
    match params.get("headers") {
        None | Some(Value::Null) => Ok(json!({})),
        Some(v) if v.is_object() => Ok(v.clone()),
        _ => Err(Error::new("headers must be an object of name/value pairs")),
    }
}

/// The response as stored in history: the body text, else the response without `debug`.
pub fn history_text(response: &Value) -> Result<String> {
    if let Some(body) = response.get("body") {
        return match body {
            Value::String(body) => Ok(body.clone()),
            body => Ok(serde_json::to_string_pretty(body)?),
        };
    }
    let mut response = response.clone();
    if let Some(fields) = response.as_object_mut() {
        fields.remove("debug");
    }
    Ok(serde_json::to_string_pretty(&response)?)
}
