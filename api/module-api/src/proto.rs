//! Messages of `proto/host.proto` and `proto/module.proto`.
//!
//! Hand-written prost types; `tests/proto.rs` checks every field against the `.proto` files.

pub mod host {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Empty {}

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Header {
        #[prost(string, tag = "1")]
        pub name: String,
        #[prost(string, tag = "2")]
        pub value: String,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct HttpRequest {
        #[prost(string, tag = "1")]
        pub method: String,
        #[prost(string, tag = "2")]
        pub url: String,
        #[prost(message, repeated, tag = "3")]
        pub headers: Vec<Header>,
        #[prost(bytes = "vec", optional, tag = "4")]
        pub body: Option<Vec<u8>>,
        #[prost(bool, tag = "5")]
        pub text: bool,
        #[prost(uint64, tag = "6")]
        pub timeout_ms: u64,
        #[prost(bool, tag = "7")]
        pub error_for_status: bool,
        #[prost(bool, tag = "8")]
        pub no_redirects: bool,
        #[prost(bool, tag = "9")]
        pub no_retries: bool,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct HttpResponse {
        #[prost(uint32, tag = "1")]
        pub status: u32,
        #[prost(string, tag = "2")]
        pub status_text: String,
        #[prost(string, tag = "3")]
        pub http_version: String,
        #[prost(message, repeated, tag = "4")]
        pub headers: Vec<Header>,
        #[prost(bytes = "vec", tag = "5")]
        pub body: Vec<u8>,
        #[prost(uint64, tag = "6")]
        pub headers_us: u64,
        #[prost(string, optional, tag = "7")]
        pub remote_address: Option<String>,
        #[prost(uint32, tag = "8")]
        pub remote_port: u32,
        #[prost(string, tag = "9")]
        pub remote_family: String,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct HttpStreamMessage {
        #[prost(oneof = "HttpStreamKind", tags = "1, 2")]
        pub kind: Option<HttpStreamKind>,
    }

    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum HttpStreamKind {
        #[prost(message, tag = "1")]
        Head(HttpResponse),
        #[prost(bytes = "vec", tag = "2")]
        Chunk(Vec<u8>),
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct HttpFailure {
        #[prost(string, repeated, tag = "1")]
        pub causes: Vec<String>,
        #[prost(bool, tag = "2")]
        pub connect: bool,
        #[prost(bool, tag = "3")]
        pub builder: bool,
        #[prost(bool, tag = "4")]
        pub timeout: bool,
        #[prost(uint32, optional, tag = "5")]
        pub status: Option<u32>,
        #[prost(bool, tag = "6")]
        pub decode: bool,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Context {
        #[prost(string, tag = "1")]
        pub data_dir: String,
        #[prost(string, tag = "2")]
        pub host_version: String,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct LogRequest {
        #[prost(string, tag = "1")]
        pub level: String,
        #[prost(string, tag = "2")]
        pub message: String,
    }

    pub const HTTP: &str = "Host/Http";
    pub const HTTP_STREAM: &str = "Host/HttpStream";
    pub const CONTEXT: &str = "Host/Context";
    pub const LOG: &str = "Host/Log";
}

pub mod module {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Manifest {
        #[prost(string, tag = "1")]
        pub id: String,
        #[prost(string, tag = "2")]
        pub name: String,
        #[prost(string, tag = "3")]
        pub version: String,
        #[prost(bool, tag = "4")]
        pub execute: bool,
        #[prost(bool, tag = "5")]
        pub discover: bool,
        #[prost(bool, tag = "6")]
        pub stream: bool,
        /// The module's `manifest.json`: description, params and the console section.
        #[prost(string, tag = "7")]
        pub spec_json: String,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ExecuteRequest {
        #[prost(string, tag = "1")]
        pub params_json: String,
        #[prost(bool, tag = "2")]
        pub history: bool,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
    #[repr(i32)]
    pub enum RunStatus {
        Success = 0,
        Error = 1,
        Stopped = 2,
    }

    impl RunStatus {
        pub fn as_str(self) -> &'static str {
            match self {
                Self::Success => "success",
                Self::Error => "error",
                Self::Stopped => "stopped",
            }
        }
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ExecuteResponse {
        /// `string` on the wire; `Bytes` so the host reads it without copying (UTF-8 JSON).
        #[prost(bytes = "bytes", tag = "1")]
        pub result_json: prost::bytes::Bytes,
        #[prost(enumeration = "RunStatus", tag = "2")]
        pub status: i32,
        #[prost(string, tag = "3")]
        pub outcome: String,
        #[prost(string, optional, tag = "4")]
        pub history_text: Option<String>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct DiscoverRequest {
        #[prost(string, tag = "1")]
        pub params_json: String,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct DiscoverResponse {
        #[prost(string, tag = "1")]
        pub result_json: String,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Event {
        #[prost(string, tag = "1")]
        pub json: String,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ModuleError {
        #[prost(string, optional, tag = "1")]
        pub debug_json: Option<String>,
        #[prost(uint64, optional, tag = "2")]
        pub duration_ms: Option<u64>,
    }

    pub const MANIFEST: &str = "Module/Manifest";
    pub const EXECUTE: &str = "Module/Execute";
    pub const DISCOVER: &str = "Module/Discover";
    pub const STREAM: &str = "Module/Stream";
}
