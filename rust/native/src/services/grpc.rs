use crate::core::error::{Error, Result, string, text};
use base64::Engine;
use bytes::Buf;
use futures_util::stream;
use prost::Message;
use prost_reflect::{
    DescriptorPool, DeserializeOptions, DynamicMessage, Kind, MessageDescriptor, ReflectMessage,
    SerializeOptions,
};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    time::{Duration, Instant},
};
use tonic::{
    Status,
    codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder},
    metadata::{KeyAndValueRef, MetadataMap},
    transport::{Channel, ClientTlsConfig, Endpoint},
};

struct Source(String);
impl protox::file::FileResolver for Source {
    fn open_file(&self, name: &str) -> std::result::Result<protox::file::File, protox::Error> {
        if name == "input.proto" {
            protox::file::File::from_source(name, &self.0)
        } else {
            protox::file::FileResolver::open_file(&protox::file::GoogleFileResolver::new(), name)
        }
    }
}
pub fn compile_proto(source: &str) -> Result<DescriptorPool> {
    let mut compiler = protox::Compiler::with_file_resolver(Source(source.into()));
    compiler
        .include_imports(true)
        .open_file("input.proto")
        .map_err(|e| Error::new(format!("Invalid proto: {e}")))?;
    DescriptorPool::from_file_descriptor_set(compiler.file_descriptor_set())
        .map_err(|e| Error::new(format!("Invalid proto: {e}")))
}
#[derive(Clone)]
pub struct DynamicCodec {
    pub output: MessageDescriptor,
}
pub struct DynamicEncoder;
pub struct DynamicDecoder(MessageDescriptor);
impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynamicEncoder;
    type Decoder = DynamicDecoder;
    fn encoder(&mut self) -> Self::Encoder {
        DynamicEncoder
    }
    fn decoder(&mut self) -> Self::Decoder {
        DynamicDecoder(self.output.clone())
    }
}
impl Encoder for DynamicEncoder {
    type Item = DynamicMessage;
    type Error = Status;
    fn encode(
        &mut self,
        item: Self::Item,
        buf: &mut EncodeBuf<'_>,
    ) -> std::result::Result<(), Status> {
        item.encode(buf)
            .map_err(|e| Status::internal(e.to_string()))
    }
}
impl Decoder for DynamicDecoder {
    type Item = DynamicMessage;
    type Error = Status;
    fn decode(
        &mut self,
        buf: &mut DecodeBuf<'_>,
    ) -> std::result::Result<Option<Self::Item>, Status> {
        DynamicMessage::decode(self.0.clone(), buf.copy_to_bytes(buf.remaining()))
            .map(Some)
            .map_err(|e| Status::internal(e.to_string()))
    }
}
pub fn parse_endpoint(endpoint: &str) -> (String, String, String) {
    let endpoint = endpoint.trim();
    let offset = endpoint.find("://").map(|i| i + 3).unwrap_or(0);
    match endpoint[offset..].find('/') {
        None => (endpoint.into(), String::new(), String::new()),
        Some(i) => {
            let i = i + offset;
            let path = &endpoint[i + 1..];
            if let Some((service, method)) = path.rsplit_once('/') {
                (
                    endpoint[..i].into(),
                    service.trim().into(),
                    method.trim().into(),
                )
            } else {
                (endpoint[..i].into(), String::new(), String::new())
            }
        }
    }
}
async fn channel(target: &str) -> Result<Channel> {
    let lower = target.to_lowercase();
    let (secure, address) = if lower.starts_with("grpcs://") {
        (true, &target[8..])
    } else if lower.starts_with("grpc://") {
        (false, &target[7..])
    } else {
        (false, target)
    };
    let mut endpoint = Endpoint::from_shared(format!(
        "{}://{address}",
        if secure { "https" } else { "http" }
    ))
    .map_err(|e| Error::new(e.to_string()))?
    .connect_timeout(Duration::from_secs(5))
    .timeout(Duration::from_secs(120));
    if secure {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_native_roots())
            .map_err(|e| Error::new(e.to_string()))?;
    }
    Ok(endpoint.connect_lazy())
}
pub fn messages(raw: Option<&Value>) -> Result<Vec<Value>> {
    let Some(raw) = raw.filter(|v| !v.is_null() && **v != "") else {
        return Ok(vec![json!({})]);
    };
    let parsed = if let Some(s) = raw.as_str() {
        serde_json::from_str(s).map_err(|_| Error::new("request message must be valid JSON"))?
    } else {
        raw.clone()
    };
    Ok(if let Value::Array(a) = parsed {
        a
    } else {
        vec![parsed]
    })
}
fn metadata(params: &Value) -> Result<MetadataMap> {
    let headers = super::http::headers(params)?;
    let mut metadata = MetadataMap::new();
    for (name, value) in headers.as_object().unwrap() {
        let name = name.to_lowercase();
        if name.ends_with("-bin") {
            let key = tonic::metadata::MetadataKey::<tonic::metadata::Binary>::from_bytes(
                name.as_bytes(),
            )
            .map_err(|e| Error::new(e.to_string()))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(string(value))
                .map_err(|e| Error::new(e.to_string()))?;
            metadata.append_bin(key, tonic::metadata::MetadataValue::from_bytes(&bytes));
        } else {
            let key =
                tonic::metadata::MetadataKey::<tonic::metadata::Ascii>::from_bytes(name.as_bytes())
                    .map_err(|e| Error::new(e.to_string()))?;
            let value = string(value)
                .parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>()
                .map_err(|e| Error::new(e.to_string()))?;
            metadata.append(key, value);
        }
    }
    Ok(metadata)
}
fn metadata_json(metadata: &MetadataMap) -> Value {
    let mut out = Map::new();
    for item in metadata.iter() {
        match item {
            KeyAndValueRef::Ascii(key, v) => {
                out.entry(key.to_string())
                    .or_insert(json!(v.to_str().unwrap_or("")));
            }
            KeyAndValueRef::Binary(key, v) => {
                out.entry(key.to_string()).or_insert(json!(
                    base64::engine::general_purpose::STANDARD
                        .encode(v.to_bytes().unwrap_or_default())
                ));
            }
        }
    }
    Value::Object(out)
}
fn response_json(message: &DynamicMessage) -> std::result::Result<Value, serde_json::Error> {
    let mut bytes = Vec::new();
    message.serialize_with_options(
        &mut serde_json::Serializer::new(&mut bytes),
        &SerializeOptions::new()
            .use_proto_field_name(true)
            .skip_default_fields(false),
    )?;
    let mut value: Value = serde_json::from_slice(&bytes)?;
    if !value.is_object()
        || message
            .descriptor()
            .full_name()
            .starts_with("google.protobuf.")
    {
        return Ok(value);
    }
    fn nested(
        raw: &prost_reflect::Value,
        serialized: &mut Value,
    ) -> std::result::Result<(), serde_json::Error> {
        if serialized.is_null() {
            return Ok(());
        }
        match raw {
            prost_reflect::Value::Message(message) => *serialized = response_json(message)?,
            prost_reflect::Value::Bytes(bytes) => {
                *serialized = json!({"type":"Buffer","data":bytes.as_ref()})
            }
            prost_reflect::Value::List(items) => {
                if let Some(values) = serialized.as_array_mut() {
                    for (raw, value) in items.iter().zip(values) {
                        nested(raw, value)?;
                    }
                }
            }
            prost_reflect::Value::Map(items) => {
                if let Some(values) = serialized.as_object_mut() {
                    for (key, raw) in items {
                        let key = match key {
                            prost_reflect::MapKey::Bool(v) => v.to_string(),
                            prost_reflect::MapKey::I32(v) => v.to_string(),
                            prost_reflect::MapKey::I64(v) => v.to_string(),
                            prost_reflect::MapKey::U32(v) => v.to_string(),
                            prost_reflect::MapKey::U64(v) => v.to_string(),
                            prost_reflect::MapKey::String(v) => v.clone(),
                        };
                        if let Some(value) = values.get_mut(&key) {
                            nested(raw, value)?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    for field in message.descriptor().fields() {
        if let Some(value) = value.get_mut(field.name()) {
            nested(message.get_field(&field).as_ref(), value)?;
        }
    }
    for oneof in message.descriptor().oneofs() {
        if let Some(field) = oneof.fields().find(|field| message.has_field(field)) {
            value[oneof.name()] = json!(field.name());
        }
    }
    Ok(value)
}
pub fn status_name(code: tonic::Code) -> &'static str {
    match code {
        tonic::Code::Ok => "OK",
        tonic::Code::Cancelled => "CANCELLED",
        tonic::Code::Unknown => "UNKNOWN",
        tonic::Code::InvalidArgument => "INVALID_ARGUMENT",
        tonic::Code::DeadlineExceeded => "DEADLINE_EXCEEDED",
        tonic::Code::NotFound => "NOT_FOUND",
        tonic::Code::AlreadyExists => "ALREADY_EXISTS",
        tonic::Code::PermissionDenied => "PERMISSION_DENIED",
        tonic::Code::ResourceExhausted => "RESOURCE_EXHAUSTED",
        tonic::Code::FailedPrecondition => "FAILED_PRECONDITION",
        tonic::Code::Aborted => "ABORTED",
        tonic::Code::OutOfRange => "OUT_OF_RANGE",
        tonic::Code::Unimplemented => "UNIMPLEMENTED",
        tonic::Code::Internal => "INTERNAL",
        tonic::Code::Unavailable => "UNAVAILABLE",
        tonic::Code::DataLoss => "DATA_LOSS",
        tonic::Code::Unauthenticated => "UNAUTHENTICATED",
    }
}
pub async fn send(params: &Value) -> Result<Value> {
    let (mut target, mut service, mut method) = parse_endpoint(&text(params, "endpoint"));
    if target.is_empty() {
        target = text(params, "target");
    }
    if service.is_empty() {
        service = text(params, "serviceName");
    }
    if method.is_empty() {
        method = text(params, "methodName");
    }
    let request = json!({"target":target,"serviceName":service,"methodName":method,"headers":super::http::headers(params)?});
    let started = Instant::now();
    let setup = async {
        if target.is_empty() {
            return Err(Error::new("target is required"));
        }
        if service.is_empty() || method.is_empty() {
            return Err(Error::new("serviceName and methodName are required"));
        }
        let channel = channel(&target).await?;
        let proto = text(params, "proto");
        let pool = if proto.trim().is_empty() {
            reflection_pool(channel.clone(), &service, &metadata(params)?).await?
        } else {
            compile_proto(&proto)?
        };
        let descriptor = pool
            .get_service_by_name(&service)
            .ok_or_else(|| Error::new(format!("Service not found: {service}")))?;
        let method = descriptor
            .methods()
            .find(|m| m.name() == method)
            .ok_or_else(|| Error::new(format!("Method not found: {service}/{method}")))?;
        let mut messages = messages(params.get("requestMessage"))?;
        if !method.is_client_streaming() {
            messages.truncate(1);
            if messages.is_empty() {
                messages.push(json!({}));
            }
        }
        let mut encoded = vec![];
        for message in messages {
            let raw = message.to_string();
            let mut deserializer = serde_json::Deserializer::from_str(&raw);
            encoded.push(
                DynamicMessage::deserialize_with_options(
                    method.input(),
                    &mut deserializer,
                    &DeserializeOptions::new().deny_unknown_fields(false),
                )
                .map_err(|e| Error::new(format!("Invalid request message: {e}")))?,
            );
        }
        let metadata = metadata(params)?;
        Ok((channel, method, encoded, metadata))
    }
    .await;
    let (channel, descriptor, messages, metadata) = setup.map_err(|mut e| {
        e = e.with_debug(request.clone());
        e.duration_ms = Some(started.elapsed().as_millis());
        e
    })?;
    let started = Instant::now();
    let mut client = tonic::client::Grpc::new(channel)
        .max_decoding_message_size(64 * 1024 * 1024)
        .max_encoding_message_size(64 * 1024 * 1024);
    client
        .ready()
        .await
        .map_err(|e| Error::new(e.to_string()).with_debug(request.clone()))?;
    let path =
        http::uri::PathAndQuery::from_maybe_shared(format!("/{service}/{}", descriptor.name()))
            .map_err(|e| Error::new(e.to_string()))?;
    let mut req = tonic::Request::new(stream::iter(messages));
    *req.metadata_mut() = metadata;
    let mut headers = json!({});
    let mut events = vec![];
    let mut responses = vec![];
    let mut trailers = json!({});
    let result=async {
        let response=client.streaming(req,path,DynamicCodec{output:descriptor.output()}).await?;
        headers=metadata_json(response.metadata());
        events.push(json!({"event":"metadata","atMs":started.elapsed().as_millis(),"data":headers}));
        let mut stream=response.into_inner();
        while let Some(message)=stream.message().await? {
            responses.push(response_json(&message).map_err(|e|Status::internal(e.to_string()))?);
            if descriptor.is_server_streaming(){events.push(json!({"event":"data","atMs":started.elapsed().as_millis(),"data":{"index":responses.len()-1}}));}
        }
        if let Some(metadata)=stream.trailers().await?{trailers=metadata_json(&metadata);}
        Ok::<(),Status>(())
    }.await;
    let (code, details, body) = match result {
        Ok(()) => (
            tonic::Code::Ok,
            String::new(),
            if descriptor.is_server_streaming() {
                json!(responses)
            } else {
                responses.into_iter().next().unwrap_or(json!({}))
            },
        ),
        Err(status) => {
            trailers = metadata_json(status.metadata());
            events.push(json!({"event":"error","atMs":started.elapsed().as_millis(),"data":{"name":"Error","message":status.message(),"code":status.code() as i32}}));
            (
                status.code(),
                status.message().to_string(),
                json!({"error":status.message()}),
            )
        }
    };
    events.push(json!({"event":"status","atMs":started.elapsed().as_millis(),"data":{"code":code as i32,"details":details,"metadata":trailers}}));
    let response = json!({"status":code as i32,"statusText":status_name(code),"headers":headers});
    Ok(
        json!({"status":code as i32,"statusText":status_name(code),"headers":headers,"body":body.to_string(),"durationMs":started.elapsed().as_millis(),"debug":{"request":request,"connection":{"events":events},"response":response}}),
    )
}

#[derive(Clone, PartialEq, Message)]
struct ReflectionRequest {
    #[prost(string, tag = "1")]
    host: String,
    #[prost(oneof = "reflection_request::Request", tags = "3,4,7")]
    request: Option<reflection_request::Request>,
}
mod reflection_request {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Request {
        #[prost(string, tag = "3")]
        FileByFilename(String),
        #[prost(string, tag = "4")]
        FileContainingSymbol(String),
        #[prost(string, tag = "7")]
        ListServices(String),
    }
}
#[derive(Clone, PartialEq, Message)]
struct ReflectionResponse {
    #[prost(message, optional, tag = "4")]
    file_descriptor_response: Option<FileDescriptorResponse>,
    #[prost(message, optional, tag = "6")]
    list_services_response: Option<ListServiceResponse>,
    #[prost(message, optional, tag = "7")]
    error_response: Option<ReflectionError>,
}
#[derive(Clone, PartialEq, Message)]
struct FileDescriptorResponse {
    #[prost(bytes = "vec", repeated, tag = "1")]
    file_descriptor_proto: Vec<Vec<u8>>,
}
#[derive(Clone, PartialEq, Message)]
struct ListServiceResponse {
    #[prost(message, repeated, tag = "1")]
    service: Vec<ServiceResponse>,
}
#[derive(Clone, PartialEq, Message)]
struct ServiceResponse {
    #[prost(string, tag = "1")]
    name: String,
}
#[derive(Clone, PartialEq, Message)]
struct ReflectionError {
    #[prost(int32, tag = "1")]
    error_code: i32,
    #[prost(string, tag = "2")]
    error_message: String,
}
async fn reflect(
    channel: Channel,
    request: reflection_request::Request,
    metadata: &MetadataMap,
) -> Result<ReflectionResponse> {
    let mut last = String::new();
    for version in ["v1", "v1alpha"] {
        let mut client =
            tonic::client::Grpc::new(channel.clone()).max_decoding_message_size(64 * 1024 * 1024);
        client
            .ready()
            .await
            .map_err(|e| Error::new(format!("Reflection failed: {e}")))?;
        let path = http::uri::PathAndQuery::from_maybe_shared(format!(
            "/grpc.reflection.{version}.ServerReflection/ServerReflectionInfo"
        ))
        .unwrap();
        let message = ReflectionRequest {
            host: String::new(),
            request: Some(request.clone()),
        };
        let mut request = tonic::Request::new(stream::iter([message]));
        *request.metadata_mut() = metadata.clone();
        let result = async {
            let response = client
                .streaming(
                    request,
                    path,
                    tonic::codec::ProstCodec::<ReflectionRequest, ReflectionResponse>::default(),
                )
                .await?;
            response
                .into_inner()
                .message()
                .await?
                .ok_or_else(|| Status::unknown("reflection returned no response"))
        };
        match tokio::time::timeout(Duration::from_secs(5), result).await {
            Ok(Ok(response)) => {
                if let Some(error) = response.error_response {
                    return Err(Error::new(format!(
                        "Reflection failed: {}",
                        error.error_message
                    )));
                }
                return Ok(response);
            }
            Ok(Err(status)) => last = status.to_string(),
            Err(_) => last = "reflection timed out".into(),
        }
    }
    Err(Error::new(format!("Reflection failed: {last}")))
}
async fn reflection_pool(
    channel: Channel,
    service: &str,
    metadata: &MetadataMap,
) -> Result<DescriptorPool> {
    let mut queue = VecDeque::from([reflection_request::Request::FileContainingSymbol(
        service.into(),
    )]);
    let mut files = BTreeMap::new();
    let mut requested = HashSet::new();
    while let Some(request) = queue.pop_front() {
        let response = reflect(channel.clone(), request, metadata).await?;
        let descriptors = response
            .file_descriptor_response
            .ok_or_else(|| Error::new("Reflection failed: missing file descriptors"))?;
        for bytes in descriptors.file_descriptor_proto {
            let file = prost_types::FileDescriptorProto::decode(bytes.as_slice())
                .map_err(|e| Error::new(e.to_string()))?;
            files.insert(file.name().to_string(), file);
        }
        for file in files.values() {
            for dep in &file.dependency {
                if !files.contains_key(dep) && requested.insert(dep.clone()) {
                    queue.push_back(reflection_request::Request::FileByFilename(dep.clone()));
                }
            }
        }
        if files.len() > 4096 {
            return Err(Error::new("Reflection descriptor limit exceeded"));
        }
    }
    DescriptorPool::from_file_descriptor_set(prost_types::FileDescriptorSet {
        file: files.into_values().collect(),
    })
    .map_err(|e| Error::new(format!("Reflection failed: {e}")))
}
pub fn message_fields(descriptor: MessageDescriptor, depth: usize) -> Vec<Value> {
    descriptor
        .fields()
        .map(|field| {
            let element_kind = match field.kind() {
                Kind::Message(_) => "object",
                Kind::String | Kind::Bytes | Kind::Enum(_) => "string",
                Kind::Bool => "boolean",
                _ => "number",
            };
            let kind = if field.is_list() {
                "array"
            } else if field.is_map() {
                "object"
            } else {
                match element_kind {
                    "boolean" | "number" => "bare",
                    kind => kind,
                }
            };
            let mut value = json!({"name":field.name(),"kind":kind});
            let shape = if field.is_list() {
                value["items"] = json!({"kind":element_kind});
                &mut value["items"]
            } else {
                &mut value
            };
            if shape["kind"] == "object"
                && !field.is_map()
                && depth <= 5
                && let Kind::Message(nested) = field.kind()
            {
                shape["fields"] = json!(message_fields(nested, depth + 1));
            }
            value
        })
        .collect()
}
pub async fn discover(params: &Value) -> Result<Value> {
    let (target, service, method) = parse_endpoint(&text(params, "endpoint"));
    async {
        let channel = channel(&target).await?;
        let metadata = metadata(params)?;
        if service.is_empty() {
            let response = reflect(
                channel,
                reflection_request::Request::ListServices("*".into()),
                &metadata,
            )
            .await?;
            let services: Vec<_> = response
                .list_services_response
                .ok_or_else(|| Error::new("Reflection returned no service list"))?
                .service
                .into_iter()
                .map(|s| s.name)
                .filter(|s| !s.starts_with("grpc.reflection."))
                .collect();
            return Ok(json!({"services":services}));
        }
        let proto = text(params, "proto");
        let pool = if proto.trim().is_empty() {
            reflection_pool(channel, &service, &metadata).await?
        } else {
            compile_proto(&proto)?
        };
        let service = pool
            .get_service_by_name(&service)
            .ok_or_else(|| Error::new("Unknown service"))?;
        let methods: Vec<_> = service.methods().map(|m| m.name().to_string()).collect();
        let mut result = json!({"methods":methods});
        if !method.is_empty() {
            result["fields"] = json!(
                service
                    .methods()
                    .find(|m| m.name() == method)
                    .map(|m| message_fields(m.input(), 0))
                    .unwrap_or_default()
            );
        }
        Ok::<_, Error>(result)
    }
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::{Request, State},
        response::Response,
    };
    use std::sync::{Arc, Mutex};
    type Seen = Arc<Mutex<Vec<(String, Vec<Value>, String)>>>;
    const PROTO: &str = r#"syntax = "proto3";
        package fixture;
        import "google/protobuf/timestamp.proto";
        message Message { string text = 1; int64 count = 2; Nested child = 3; repeated string tags = 4; google.protobuf.Timestamp at = 5; bytes payload = 6; oneof choice { string selected = 7; int32 number = 8; } repeated StoreView products_by_store_view = 9; repeated bool flags = 10; repeated int32 ids = 11; }
        message Nested { bool enabled = 1; }
        message StoreView { string store_view_code = 1; repeated Product products = 2; }
        message Product { string sku = 1; repeated string tags = 2; }
        service Echo {
            rpc Unary(Message) returns (Message);
            rpc Server(Message) returns (stream Message);
            rpc Client(stream Message) returns (Message);
            rpc Bidi(stream Message) returns (stream Message);
            rpc Fails(Message) returns (Message);
        }"#;
    #[derive(Clone)]
    struct Fixture {
        pool: DescriptorPool,
        alpha_only: bool,
        reflection_auth: bool,
        seen: Seen,
    }
    struct Server {
        target: String,
        task: tokio::task::JoinHandle<()>,
        fixture: Fixture,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    fn frames(bytes: &[u8]) -> Vec<&[u8]> {
        let mut input = bytes;
        let mut out = vec![];
        while input.len() >= 5 {
            assert_eq!(input[0], 0);
            let len = u32::from_be_bytes(input[1..5].try_into().unwrap()) as usize;
            assert!(input.len() >= len + 5);
            out.push(&input[5..len + 5]);
            input = &input[len + 5..];
        }
        out
    }
    fn framed(message: impl Message) -> Vec<u8> {
        let raw = message.encode_to_vec();
        let mut bytes = vec![0];
        bytes.extend_from_slice(&(raw.len() as u32).to_be_bytes());
        bytes.extend(raw);
        bytes
    }
    fn response(bytes: Vec<u8>, status: i32) -> Response {
        Response::builder()
            .header("content-type", "application/grpc")
            .header("grpc-status", status.to_string())
            .header(
                "grpc-message",
                if status == 3 { "fixture-invalid" } else { "" },
            )
            .header("x-reply", "fixture")
            .body(Body::from(bytes))
            .unwrap()
    }
    async fn handle(State(fixture): State<Fixture>, request: Request) -> Response {
        let path = request.uri().path().to_string();
        let token = request
            .headers()
            .get("x-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
        let messages = frames(&bytes);
        if path.contains(".ServerReflection/") {
            if fixture.reflection_auth && token != "reflection-token" {
                return response(vec![], 16);
            }
            if fixture.alpha_only && path.contains(".v1.") {
                return response(vec![], 12);
            }
            let request = ReflectionRequest::decode(messages[0]).unwrap();
            let mut reply = ReflectionResponse::default();
            match request.request.unwrap() {
                reflection_request::Request::ListServices(_) => {
                    reply.list_services_response = Some(ListServiceResponse {
                        service: vec![
                            ServiceResponse {
                                name: "fixture.Echo".into(),
                            },
                            ServiceResponse {
                                name: "grpc.reflection.v1alpha.ServerReflection".into(),
                            },
                        ],
                    });
                }
                _ => {
                    reply.file_descriptor_response = Some(FileDescriptorResponse {
                        file_descriptor_proto: fixture
                            .pool
                            .file_descriptor_protos()
                            .map(|f| f.encode_to_vec())
                            .collect(),
                    });
                }
            }
            return response(framed(reply), 0);
        }
        let descriptor = fixture.pool.get_message_by_name("fixture.Message").unwrap();
        let decoded: Vec<DynamicMessage> = messages
            .iter()
            .map(|m| DynamicMessage::decode(descriptor.clone(), *m).unwrap())
            .collect();
        let values = decoded
            .iter()
            .map(|m| serde_json::to_value(m).unwrap())
            .collect::<Vec<_>>();
        fixture
            .seen
            .lock()
            .unwrap()
            .push((path.clone(), values.clone(), token));
        if path.ends_with("/Fails") {
            return response(vec![], 3);
        }
        let mut out = Vec::new();
        if path.ends_with("/Client") {
            let value = json!({"text":values.iter().map(|v|v["text"].as_str().unwrap_or("")).collect::<Vec<_>>().join("+"),"count":values.len().to_string()});
            let raw = value.to_string();
            out.extend(framed(
                DynamicMessage::deserialize(
                    descriptor,
                    &mut serde_json::Deserializer::from_str(&raw),
                )
                .unwrap(),
            ));
        } else if path.ends_with("/Server") {
            for _ in 0..3 {
                out.extend(framed(decoded[0].clone()));
            }
        } else {
            for message in decoded {
                out.extend(framed(message));
            }
        }
        response(out, 0)
    }
    async fn server(alpha_only: bool) -> Server {
        server_with_auth(alpha_only, false).await
    }
    async fn server_with_auth(alpha_only: bool, reflection_auth: bool) -> Server {
        let fixture = Fixture {
            pool: compile_proto(PROTO).unwrap(),
            alpha_only,
            reflection_auth,
            seen: Default::default(),
        };
        let router = Router::new().fallback(handle).with_state(fixture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = listener.local_addr().unwrap().to_string();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Server {
            target,
            task,
            fixture,
        }
    }
    #[tokio::test]
    async fn reflection_uses_authentication_for_services_methods_and_calls() {
        for alpha_only in [false, true] {
            let server = server_with_auth(alpha_only, true).await;
            assert!(discover(&json!({"endpoint":server.target})).await.is_err());
            let headers = json!({"X-Token":"reflection-token"});
            let services = discover(&json!({"endpoint":server.target,"headers":headers}))
                .await
                .unwrap();
            assert_eq!(services["services"], json!(["fixture.Echo"]));
            let endpoint = format!("{}/fixture.Echo/Unary", server.target);
            let schema = discover(&json!({"endpoint":endpoint,"headers":headers}))
                .await
                .unwrap();
            assert_eq!(schema["fields"][0]["name"], "text");
            let result = send(&json!({"endpoint":endpoint,"headers":headers,"requestMessage":{"text":"authenticated"}})).await.unwrap();
            assert_eq!(result["status"], 0);
            assert_eq!(server.fixture.seen.lock().unwrap()[0].2, "reflection-token");
        }
    }
    #[test]
    fn endpoint_messages_proto_well_known_import_and_nested_discovery() {
        assert_eq!(
            parse_endpoint("grpcs://localhost:443/fixture.Echo/Unary"),
            (
                "grpcs://localhost:443".into(),
                "fixture.Echo".into(),
                "Unary".into()
            )
        );
        assert_eq!(
            parse_endpoint("localhost:443/fixture.Echo/"),
            ("localhost:443".into(), "fixture.Echo".into(), "".into())
        );
        assert_eq!(
            messages(Some(&json!("[{\"text\":\"a\"},{\"text\":\"b\"}]")))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(messages(None).unwrap(), vec![json!({})]);
        assert!(messages(Some(&json!("not json"))).is_err());
        assert!(compile_proto("not proto").is_err());
        let pool = compile_proto(PROTO).unwrap();
        let fields = message_fields(pool.get_message_by_name("fixture.Message").unwrap(), 0);
        assert_eq!(
            fields[2],
            json!({"name":"child","kind":"object","fields":[{"name":"enabled","kind":"bare"}]})
        );
        assert_eq!(fields[3]["kind"], "array");
        assert_eq!(fields[3]["items"], json!({"kind":"string"}));
    }
    #[test]
    fn repeated_fields_keep_their_element_types_and_nested_fields() {
        let pool = compile_proto(PROTO).unwrap();
        let fields = message_fields(pool.get_message_by_name("fixture.Message").unwrap(), 0);
        let field = |name: &str| fields.iter().find(|field| field["name"] == name).unwrap();
        assert_eq!(
            field("products_by_store_view"),
            &json!({"name":"products_by_store_view","kind":"array","items":{
                "kind":"object","fields":[
                    {"name":"store_view_code","kind":"string"},
                    {"name":"products","kind":"array","items":{"kind":"object","fields":[
                        {"name":"sku","kind":"string"},
                        {"name":"tags","kind":"array","items":{"kind":"string"}}
                    ]}}
                ]
            }})
        );
        assert_eq!(field("flags")["items"], json!({"kind":"boolean"}));
        assert_eq!(field("ids")["items"], json!({"kind":"number"}));
        let bounded = message_fields(pool.get_message_by_name("fixture.Message").unwrap(), 6);
        assert_eq!(bounded[8]["items"], json!({"kind":"object"}));
    }
    #[tokio::test]
    async fn pasted_proto_all_four_call_shapes_metadata_status_and_wkt() {
        let server = server(false).await;
        let call = |method: &str, message: Value| json!({"endpoint":format!("grpc://{}/fixture.Echo/{method}",server.target),"proto":PROTO,"headers":{"x-token":"sent-token"},"requestMessage":message});
        let result = send(&call(
            "Unary",
            json!({"text":"one","count":"9007199254740993","at":"2026-10-17T01:00:00Z","payload":"AQID","selected":"yes"}),
        ))
        .await
        .unwrap();
        assert_eq!(result["status"], 0);
        assert_eq!(result["statusText"], "OK");
        assert_eq!(result["headers"]["x-reply"], "fixture");
        let body: Value = serde_json::from_str(result["body"].as_str().unwrap()).unwrap();
        assert_eq!(body["count"], "9007199254740993");
        assert_eq!(body["at"], "2026-10-17T01:00:00Z");
        assert_eq!(body["tags"], json!([]));
        assert_eq!(body["payload"], json!({"type":"Buffer","data":[1,2,3]}));
        assert_eq!(body["choice"], "selected");
        assert_eq!(server.fixture.seen.lock().unwrap()[0].2, "sent-token");
        let result = send(&call("Server", json!({"text":"server"})))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(result["body"].as_str().unwrap())
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            3
        );
        let result = send(&call("Client", json!([{"text":"a"},{"text":"b"}])))
            .await
            .unwrap();
        let body: Value = serde_json::from_str(result["body"].as_str().unwrap()).unwrap();
        assert_eq!(body["text"], "a+b");
        assert_eq!(body["count"], "2");
        let result = send(&call("Bidi", json!([{"text":"a"},{"text":"b"}])))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(result["body"].as_str().unwrap())
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let result = send(&call("Fails", json!({}))).await.unwrap();
        assert_eq!(result["status"], 3);
        assert_eq!(result["statusText"], "INVALID_ARGUMENT");
        assert_eq!(
            serde_json::from_str::<Value>(result["body"].as_str().unwrap()).unwrap()["error"],
            "fixture-invalid"
        );
    }
    #[tokio::test]
    async fn reflection_v1_and_v1alpha_discover_and_invoke_without_proto() {
        for alpha_only in [false, true] {
            let server = server(alpha_only).await;
            let result = discover(&json!({"endpoint":server.target})).await.unwrap();
            assert_eq!(result, json!({"services":["fixture.Echo"]}));
            let methods = discover(&json!({"endpoint":format!("{}/fixture.Echo/",server.target)}))
                .await
                .unwrap();
            assert_eq!(methods["methods"].as_array().unwrap().len(), 5);
            assert!(methods.get("fields").is_none());
            let endpoint = format!("{}/fixture.Echo/Unary", server.target);
            let schema = discover(&json!({"endpoint":endpoint})).await.unwrap();
            assert_eq!(schema["methods"].as_array().unwrap().len(), 5);
            assert_eq!(schema["fields"][0], json!({"name":"text","kind":"string"}));
            assert_eq!(
                schema["fields"][8]["items"]["fields"][0]["name"],
                "store_view_code"
            );
            assert_eq!(
                schema["fields"][8]["items"]["fields"][1]["items"]["fields"][0]["name"],
                "sku"
            );
            let result = send(&json!({"endpoint":endpoint,"requestMessage":{"text":"reflected"}}))
                .await
                .unwrap();
            assert_eq!(result["status"], 0);
            assert_eq!(
                serde_json::from_str::<Value>(result["body"].as_str().unwrap()).unwrap()["text"],
                "reflected"
            );
        }
    }
    #[tokio::test]
    async fn discovery_errors_are_not_reported_as_successful_empty_lists() {
        let server = server(false).await;
        let error = discover(&json!({"endpoint":format!("{}/fixture.Missing/",server.target)}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Unknown service"), "{error}");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        assert!(
            discover(&json!({"endpoint":address.to_string()}))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn unreachable_pasted_proto_returns_grpc_status_not_success() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let result = send(&json!({"endpoint":format!("{addr}/fixture.Echo/Unary"),"proto":PROTO}))
            .await
            .unwrap();
        assert_eq!(result["status"], 14);
        assert_eq!(result["statusText"], "UNAVAILABLE");
    }
}
