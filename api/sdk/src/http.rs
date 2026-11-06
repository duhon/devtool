//! The Host `http` driver: request and response shapes.

use crate::wire::DriverCall;
use serde::{Deserialize, Serialize};

pub const DRIVER: &str = "http";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// A binary body; ignored when `body` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<u64>,
    /// Redirects the driver follows; 10 when absent, 0 returns the first answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirects: Option<u32>,
    /// The body comes back as text (invalid UTF-8 replaced) instead of base64.
    #[serde(default)]
    pub text: bool,
}

impl Request {
    pub fn new(method: &str, url: &str) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            text: true,
            ..Self::default()
        }
    }
    pub fn call(&self) -> DriverCall {
        DriverCall::new(DRIVER, self)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hop {
    pub status: u16,
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Remote {
    pub address: String,
    pub port: u16,
    pub family: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Timing {
    pub first_byte_us: u64,
    pub total_us: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub status: u16,
    #[serde(default)]
    pub status_text: String,
    #[serde(default)]
    pub http_version: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The final URL after redirects.
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
    #[serde(default)]
    pub redirects: Vec<Hop>,
    #[serde(default)]
    pub timing: Timing,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<Remote>,
}

impl Response {
    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        Self::deserialize(value).map_err(|error| format!("Invalid http answer: {error}"))
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}
