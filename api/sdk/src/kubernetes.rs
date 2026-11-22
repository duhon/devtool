//! The Host `kubernetes` driver: a thin authenticated transport to the API server of a kubeconfig
//! context. The module builds the Kubernetes REST call (`method`, `path`, `query`, body); the driver
//! only authenticates it (client certificates, tokens, exec plugins such as `kubelogin`, the
//! cluster's CA) and returns the server's answer. It knows no resources and no verbs.
//!
//! The kubeconfig is `KUBECONFIG` of the login-shell environment, else `~/.kube/config`, like
//! kubectl. One client is cached per context. Two operations: [`Op::Contexts`] lists the
//! contexts of the kubeconfig (`contexts` and `current` of the [`Response`]); [`Op::Request`] sends
//! one call on a context.
//!
//! A JSON answer is the `body` as JSON, any other answer (pod logs) as text. A non-2xx answer is
//! the server's answer, not a failed call: `status` is its code and `error` its `Status` object.
//! Only transport problems fail the call: connect, TLS, timeout, cancel, decoding, and
//! [`Stage::Auth`](crate::driver::Stage::Auth) for a kubeconfig that cannot be read or an exec
//! plugin that fails.
//!
//! `stream` makes `watch=true` calls answer one [`Event::Object`] per watch event and
//! `follow=true` log calls one [`Event::Line`] per line; the [`Response`] then ends the stream
//! with an empty `body`. `maxBytes` cuts the body (or the stream) and sets `truncated`.
//!
//! The host redacts secrets before anything leaves it: in every JSON of `kind` `Secret` or
//! `SecretList` (also in watch events) each value of `data` and `stringData` becomes
//! `"<redacted>"` with its key kept, and so does the annotation
//! `kubectl.kubernetes.io/last-applied-configuration`.

use crate::{http::Timing, wire::DriverCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const DRIVER: &str = "kubernetes";

/// What a call asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Op {
    /// The contexts of the kubeconfig.
    Contexts,
    /// One API call on `context`.
    #[default]
    Request,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub op: Op,
    /// The kubeconfig context; required for [`Op::Request`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// `GET`, `POST`, `PUT`, `PATCH` or `DELETE`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub method: String,
    /// The API path: `/api/v1/namespaces/ns/pods`, `/apis/apps/v1/…`, `/openapi/v3/…`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// `labelSelector`, `fieldSelector`, `limit`, `tailLines`, `follow`, `watch`, `dryRun`…
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query: BTreeMap<String, String>,
    /// For example `application/json;as=Table;v=v1;g=meta.k8s.io` for server-side columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept: Option<String>,
    /// For example `application/merge-patch+json` or `application/apply-patch+yaml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Stream `watch` events and followed log lines as [`Event`]s.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
    /// Bytes of the answer kept; the answer is then `truncated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// The whole call; a stream ends by it too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl Request {
    /// A call listing the kubeconfig contexts.
    pub fn contexts() -> Self {
        Self {
            op: Op::Contexts,
            ..Self::default()
        }
    }

    /// A call of `method` on `path` in `context`.
    pub fn new(context: &str, method: &str, path: &str) -> Self {
        Self {
            op: Op::Request,
            context: Some(context.into()),
            method: method.into(),
            path: path.into(),
            ..Self::default()
        }
    }

    /// A `GET` of `path` in `context`.
    pub fn get(context: &str, path: &str) -> Self {
        Self::new(context, "GET", path)
    }

    pub fn call(&self) -> DriverCall {
        DriverCall::new(DRIVER, self)
    }
}

/// The `Status` object of a non-2xx answer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// `NotFound`, `Forbidden`, `AlreadyExists`, `Invalid`…
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub code: u16,
}

/// A kubeconfig context.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    pub name: String,
    pub cluster: String,
    pub server: String,
    /// The context's default namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

/// A call answer, or the end of a stream (then `body` is empty: it came as [`Event`]s).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    /// The HTTP status; `0` for [`Op::Contexts`].
    #[serde(default)]
    pub status: u16,
    /// The JSON answer, or its text when it is not JSON.
    #[serde(default)]
    pub body: Value,
    /// The server's `Status` of a non-2xx answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Status>,
    /// The answer had more bytes than `maxBytes`.
    #[serde(default)]
    pub truncated: bool,
    /// The kubeconfig contexts ([`Op::Contexts`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<Context>,
    /// The kubeconfig's current context ([`Op::Contexts`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    #[serde(default)]
    pub timing: Timing,
}

impl Response {
    pub fn parse(value: &Value) -> Result<Self, String> {
        Self::deserialize(value).map_err(|error| format!("Invalid kubernetes answer: {error}"))
    }
}

/// One piece of a kubernetes stream: a followed log line or a watch event. The `exit` info is
/// the [`Response`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Event {
    Line(String),
    Object(Value),
}

impl Event {
    pub fn parse(value: &Value) -> Result<Self, String> {
        Self::deserialize(value).map_err(|error| format!("Invalid kubernetes event: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_request_round_trips_and_leaves_out_what_is_unset() {
        let mut request = Request::get("prod", "/api/v1/namespaces/ns/pods");
        request.query.insert("limit".into(), "5".into());
        let value = request.call().request();
        assert_eq!(
            value,
            json!({"op": "request", "context": "prod", "method": "GET",
                "path": "/api/v1/namespaces/ns/pods", "query": {"limit": "5"}})
        );
        assert_eq!(Request::deserialize(&value).unwrap(), request);
        assert_eq!(
            Request::contexts().call().request(),
            json!({"op": "contexts"})
        );
    }

    #[test]
    fn answers_and_events_parse() {
        let answer = json!({
            "status": 404, "body": {"kind": "Status"}, "truncated": false,
            "error": {"reason": "NotFound", "message": "pods \"x\" not found", "code": 404},
            "timing": {"firstByteUs": 1, "totalUs": 2}
        });
        let response = Response::parse(&answer).unwrap();
        assert_eq!(response.error.as_ref().unwrap().reason, "NotFound");
        assert_eq!(serde_json::to_value(&response).unwrap(), answer);
        let contexts = Response::parse(&json!({
            "contexts": [{"name": "a", "cluster": "c", "server": "https://s", "namespace": "ns"}],
            "current": "a"
        }))
        .unwrap();
        assert_eq!(contexts.contexts[0].namespace.as_deref(), Some("ns"));
        assert_eq!(
            Event::parse(&json!({"line": "ready"})).unwrap(),
            Event::Line("ready".into())
        );
        assert_eq!(
            Event::parse(&json!({"object": {"type": "ADDED"}})).unwrap(),
            Event::Object(json!({"type": "ADDED"}))
        );
    }
}
