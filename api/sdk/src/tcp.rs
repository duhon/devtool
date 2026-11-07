//! The Host `tcp` driver, the fallback for protocols without a driver: connects (optionally over
//! TLS), writes the request bytes, then reads until the peer closes, `maxBytes` or `idleMs`.

use crate::{http::Remote, wire::DriverCall};
use serde::{Deserialize, Serialize};

pub const DRIVER: &str = "tcp";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub tls: bool,
    /// The TLS server name when it is not `host`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    /// Skips TLS certificate checks.
    #[serde(default)]
    pub insecure: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// Binary data; ignored when `data` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
    /// Shuts down the write side after the data, for servers that answer at end of input.
    #[serde(default)]
    pub close_write: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Ends reading when nothing arrives for this long.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<u64>,
    /// Data comes back as text (invalid UTF-8 replaced) instead of base64.
    #[serde(default)]
    pub text: bool,
}

impl Request {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            text: true,
            ..Self::default()
        }
    }
    pub fn call(&self) -> DriverCall {
        DriverCall::new(DRIVER, self)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Closed {
    /// The peer closed the connection.
    #[default]
    Peer,
    /// `maxBytes` arrived.
    Limit,
    /// Nothing arrived for `idleMs`.
    Idle,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Timing {
    pub connect_us: u64,
    /// When the TLS handshake ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_us: Option<u64>,
    #[serde(default)]
    pub first_byte_us: u64,
    pub total_us: u64,
}

/// A call answer, or the end of a stream (then without data: it came as [`Event`]s).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
    /// Bytes read.
    pub bytes: u64,
    pub closed: Closed,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<Remote>,
    pub timing: Timing,
}

impl Response {
    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        Self::deserialize(value).map_err(|error| format!("Invalid tcp answer: {error}"))
    }
}

/// One piece of a tcp stream: `connected` once, then the data as it arrives.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Event {
    Connected { remote: Option<Remote> },
    Chunk(String),
    ChunkBase64(String),
}

impl Event {
    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        Self::deserialize(value).map_err(|error| format!("Invalid tcp event: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_response_and_events_round_trip() {
        let mut request = Request::new("db", 6379);
        request.data = Some("PING\r\n".into());
        request.idle_ms = Some(200);
        let value = request.call().request();
        assert_eq!(value["idleMs"], 200);
        assert_eq!(Request::deserialize(&value).unwrap(), request);

        let answer = json!({"data": "+PONG\r\n", "bytes": 7, "closed": "idle",
            "remote": {"address": "127.0.0.1", "port": 6379, "family": "IPv4"},
            "timing": {"connectUs": 1, "firstByteUs": 2, "totalUs": 3}});
        let response = Response::parse(&answer).unwrap();
        assert_eq!(response.closed, Closed::Idle);
        assert_eq!(serde_json::to_value(&response).unwrap(), answer);
        let event = json!({"connected": {"remote": null}});
        assert_eq!(
            serde_json::to_value(Event::parse(&event).unwrap()).unwrap(),
            event
        );
    }
}
