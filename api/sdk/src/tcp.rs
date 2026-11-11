//! The Host `tcp` driver, the fallback for protocols without a driver: connects (optionally over
//! TLS), writes the request bytes, then reads until the peer closes, `maxBytes` or `idleMs`.
//!
//! A session (`session: true`, a stream) keeps the connection open for a step-by-step exchange:
//! the module writes with [`crate::cmd::write`] when it wants (each write is acknowledged with
//! [`Event::Written`]), reads [`Event::ChunkBase64`] as the bytes arrive (any boundaries) and
//! closes with [`crate::cmd::cancel`]. The stream ends with the [`Response`] when the peer
//! closes (or at `maxBytes` / `idleMs`), or with a failure: `write`, `read`, `cancelled`, …

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
    /// Skips TLS certificate and name checks.
    #[serde(default)]
    pub insecure: bool,
    /// A PEM CA certificate (or several) trusted besides the system roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_pem: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// Binary data; ignored when `data` is set. A session writes it first, as a write of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
    /// Shuts down the write side after the data, for servers that answer at end of input; not
    /// for a session.
    #[serde(default)]
    pub close_write: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Ends reading when nothing arrives for this long.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_ms: Option<u64>,
    /// The whole exchange; not for a session, which lasts until it is closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Connecting and the TLS handshake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<u64>,
    /// Data comes back as text (invalid UTF-8 replaced) instead of base64; a session always
    /// sends base64.
    #[serde(default)]
    pub text: bool,
    /// Keeps the connection open for writes (a stream only).
    #[serde(default)]
    pub session: bool,
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
    /// A session: an open connection the module writes to and reads from.
    pub fn session(host: &str, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            session: true,
            ..Self::default()
        }
    }
    /// A session is a stream.
    pub fn call(&self) -> DriverCall {
        let call = DriverCall::new(DRIVER, self);
        if self.session { call.streaming() } else { call }
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
    /// Bytes a session wrote.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub written: u64,
    pub closed: Closed,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<Remote>,
    pub timing: Timing,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Response {
    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        Self::deserialize(value).map_err(|error| format!("Invalid tcp answer: {error}"))
    }
}

/// One piece of a tcp stream: `connected` once, then the data as it arrives and, in a session,
/// each write done.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Event {
    Connected {
        remote: Option<Remote>,
    },
    Chunk(String),
    ChunkBase64(String),
    /// A write (in the order they were asked for) went out whole.
    Written {
        bytes: u64,
    },
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
        for event in [
            json!({"connected": {"remote": null}}),
            json!({"written": {"bytes": 4}}),
        ] {
            assert_eq!(
                serde_json::to_value(Event::parse(&event).unwrap()).unwrap(),
                event
            );
        }
        let session = Request::session("db", 6379).call();
        assert!(session.stream);
        assert_eq!(
            (
                session.request()["session"].clone(),
                session.request()["text"].clone()
            ),
            (json!(true), json!(false))
        );
    }
}
