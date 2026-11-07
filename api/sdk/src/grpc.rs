//! The Host `grpc` driver: gRPC over HTTP/2 with raw protobuf messages. The Host only moves
//! bytes; `.proto`, JSON ↔ protobuf and reflection stay in the module ([`reflection`] builds
//! and reads the reflection messages).

use crate::{
    driver::{decode, encode},
    http::Timing,
    wire::DriverCall,
};
use serde::{Deserialize, Serialize};

pub const DRIVER: &str = "grpc";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// `host:port` or `grpc://host:port` in plain text, `grpcs://host:port` over TLS.
    pub target: String,
    /// `/package.Service/Method`.
    pub path: String,
    /// Request metadata; the value of a `-bin` name is base64.
    #[serde(default)]
    pub metadata: Vec<(String, String)>,
    /// Encoded request messages in base64; a unary call sends the first.
    #[serde(default)]
    pub messages: Vec<String>,
    /// The whole call, also sent as `grpc-timeout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<u64>,
}

impl Request {
    pub fn new(target: &str, service: &str, method: &str) -> Self {
        Self {
            target: target.into(),
            path: format!("/{service}/{method}"),
            ..Self::default()
        }
    }
    pub fn message(mut self, bytes: &[u8]) -> Self {
        self.messages.push(encode(bytes));
        self
    }
    pub fn with_metadata(mut self, name: &str, value: &str) -> Self {
        self.metadata.push((name.into(), value.into()));
        self
    }
    pub fn call(&self) -> DriverCall {
        DriverCall::new(DRIVER, self)
    }
}

/// A call answer, or the end of a stream (then `messages` is empty: they came as [`Event`]s).
/// A non-zero `status` is the server's answer, not a driver failure.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub status: i32,
    /// `grpc-message`.
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub trailers: Vec<(String, String)>,
    /// Encoded response messages in base64.
    #[serde(default)]
    pub messages: Vec<String>,
    #[serde(default)]
    pub timing: Timing,
}

impl Response {
    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        Self::deserialize(value).map_err(|error| format!("Invalid grpc answer: {error}"))
    }
    pub fn messages(&self) -> Result<Vec<Vec<u8>>, String> {
        self.messages
            .iter()
            .map(|message| decode(message))
            .collect()
    }
    pub fn status_name(&self) -> &'static str {
        status_name(self.status)
    }
}

/// One piece of a grpc stream: `{"headers": […]}` once, then `{"message": base64}` per
/// response message; the end is a [`Response`] without messages.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Event {
    Headers(Vec<(String, String)>),
    Message(String),
}

impl Event {
    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        Self::deserialize(value).map_err(|error| format!("Invalid grpc event: {error}"))
    }
}

pub fn status_name(code: i32) -> &'static str {
    match code {
        0 => "OK",
        1 => "CANCELLED",
        2 => "UNKNOWN",
        3 => "INVALID_ARGUMENT",
        4 => "DEADLINE_EXCEEDED",
        5 => "NOT_FOUND",
        6 => "ALREADY_EXISTS",
        7 => "PERMISSION_DENIED",
        8 => "RESOURCE_EXHAUSTED",
        9 => "FAILED_PRECONDITION",
        10 => "ABORTED",
        11 => "OUT_OF_RANGE",
        12 => "UNIMPLEMENTED",
        13 => "INTERNAL",
        14 => "UNAVAILABLE",
        15 => "DATA_LOSS",
        16 => "UNAUTHENTICATED",
        _ => "UNKNOWN",
    }
}

/// Server reflection over the driver: one call per question, `v1` first and `v1alpha` when the
/// server answers `UNIMPLEMENTED`. Files come back as encoded `FileDescriptorProto`s; their
/// dependencies are asked by file name until the set is complete.
pub mod reflection {
    use super::Response;
    use prost::Message;

    pub const VERSIONS: [&str; 2] = ["v1", "v1alpha"];
    /// The status that moves on to the next version.
    pub const UNIMPLEMENTED: i32 = 12;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Query {
        ListServices,
        FileContainingSymbol(String),
        FileByFilename(String),
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Answer {
        Services(Vec<String>),
        /// Encoded `google.protobuf.FileDescriptorProto`s.
        Files(Vec<Vec<u8>>),
    }

    pub fn path(version: &str) -> String {
        format!("/grpc.reflection.{version}.ServerReflection/ServerReflectionInfo")
    }

    /// The driver request asking `query`; add metadata and a timeout as for any call.
    pub fn request(target: &str, version: &str, query: &Query) -> super::Request {
        let request = match query {
            Query::ListServices => Ask::ListServices("*".into()),
            Query::FileContainingSymbol(symbol) => Ask::FileContainingSymbol(symbol.clone()),
            Query::FileByFilename(name) => Ask::FileByFilename(name.clone()),
        };
        let message = ReflectionRequest {
            host: String::new(),
            request: Some(request),
        };
        super::Request {
            target: target.into(),
            path: path(version),
            ..super::Request::default()
        }
        .message(&message.encode_to_vec())
    }

    /// Reads the reflection answer of an OK response.
    pub fn answer(response: &Response) -> Result<Answer, String> {
        let bytes = response.messages()?;
        let first = bytes.first().ok_or("Reflection returned no response")?;
        let reply = ReflectionResponse::decode(first.as_slice())
            .map_err(|error| format!("Invalid reflection response: {error}"))?;
        if let Some(error) = reply.error_response {
            return Err(format!("Reflection failed: {}", error.error_message));
        }
        if let Some(list) = reply.list_services_response {
            return Ok(Answer::Services(
                list.service
                    .into_iter()
                    .map(|service| service.name)
                    .collect(),
            ));
        }
        reply
            .file_descriptor_response
            .map(|files| Answer::Files(files.file_descriptor_proto))
            .ok_or_else(|| "Reflection returned neither services nor files".into())
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct ReflectionRequest {
        #[prost(string, tag = "1")]
        pub host: String,
        #[prost(oneof = "Ask", tags = "3,4,7")]
        pub request: Option<Ask>,
    }

    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Ask {
        #[prost(string, tag = "3")]
        FileByFilename(String),
        #[prost(string, tag = "4")]
        FileContainingSymbol(String),
        #[prost(string, tag = "7")]
        ListServices(String),
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct ReflectionResponse {
        #[prost(message, optional, tag = "4")]
        pub file_descriptor_response: Option<FileDescriptorResponse>,
        #[prost(message, optional, tag = "6")]
        pub list_services_response: Option<ListServiceResponse>,
        #[prost(message, optional, tag = "7")]
        pub error_response: Option<ErrorResponse>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct FileDescriptorResponse {
        #[prost(bytes = "vec", repeated, tag = "1")]
        pub file_descriptor_proto: Vec<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct ListServiceResponse {
        #[prost(message, repeated, tag = "1")]
        pub service: Vec<ServiceResponse>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct ServiceResponse {
        #[prost(string, tag = "1")]
        pub name: String,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct ErrorResponse {
        #[prost(int32, tag = "1")]
        pub error_code: i32,
        #[prost(string, tag = "2")]
        pub error_message: String,
    }
}

#[cfg(test)]
mod tests {
    use super::{reflection::*, *};
    use prost::Message;
    use serde_json::json;

    #[test]
    fn request_and_response_round_trip() {
        let request = Request::new("grpcs://api:443", "pkg.Echo", "Say")
            .with_metadata("x-token", "t")
            .message(&[1, 2, 255]);
        let value = request.call().request();
        assert_eq!(value["path"], "/pkg.Echo/Say");
        assert_eq!(value["messages"], json!(["AQL/"]));
        assert_eq!(Request::deserialize(&value).unwrap(), request);

        let answer = json!({"status": 3, "message": "bad", "headers": [["x", "1"]],
            "trailers": [], "messages": ["AQL/"], "timing": {"firstByteUs": 1, "totalUs": 2}});
        let response = Response::parse(&answer).unwrap();
        assert_eq!(response.status_name(), "INVALID_ARGUMENT");
        assert_eq!(response.messages().unwrap(), vec![vec![1, 2, 255]]);
        assert_eq!(
            Event::parse(&json!({"message": "AQL/"})).unwrap(),
            Event::Message("AQL/".into())
        );
    }

    #[test]
    fn reflection_builds_queries_and_reads_answers() {
        let request = reflection::request("t:1", "v1alpha", &Query::ListServices);
        assert_eq!(
            request.path,
            "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo"
        );
        let sent = ReflectionRequest::decode(
            crate::driver::decode(&request.messages[0])
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        assert_eq!(sent.request, Some(Ask::ListServices("*".into())));

        let reply = |message: ReflectionResponse| Response {
            messages: vec![crate::driver::encode(&message.encode_to_vec())],
            ..Response::default()
        };
        let services = reply(ReflectionResponse {
            list_services_response: Some(ListServiceResponse {
                service: vec![ServiceResponse {
                    name: "pkg.Echo".into(),
                }],
            }),
            ..ReflectionResponse::default()
        });
        assert_eq!(
            answer(&services).unwrap(),
            Answer::Services(vec!["pkg.Echo".into()])
        );
        let failed = reply(ReflectionResponse {
            error_response: Some(ErrorResponse {
                error_code: 5,
                error_message: "not found".into(),
            }),
            ..ReflectionResponse::default()
        });
        assert_eq!(answer(&failed).unwrap_err(), "Reflection failed: not found");
    }
}
