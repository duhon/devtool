//! The grpc driver: HTTP/2 transport with raw protobuf bytes. One channel per target and
//! connect timeout, reused across calls (it reconnects by itself).

use super::{CONNECT_MS, Failure, emit, micros, millis};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use bytes::{Buf, BufMut, Bytes};
use serde_json::{Value, json};
use services_sdk::{
    driver::Stage,
    grpc::{Request, Response},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::mpsc;
use tonic::{
    Status,
    codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder},
    codegen::http::uri::PathAndQuery,
    metadata::{AsciiMetadataValue, BinaryMetadataValue, KeyAndValueRef, MetadataKey, MetadataMap},
    transport::{Channel, ClientTlsConfig, Endpoint},
};

const MESSAGE_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Clone, Default)]
pub struct Grpc {
    channels: Arc<Mutex<HashMap<(String, u64), Channel>>>,
}

impl Grpc {
    fn channel(&self, target: &str, connect_ms: u64) -> Result<Channel, Failure> {
        let key = (target.to_owned(), connect_ms);
        let mut channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(channel) = channels.get(&key) {
            return Ok(channel.clone());
        }
        let lower = target.to_ascii_lowercase();
        let (secure, address) = if lower.starts_with("grpcs://") {
            (true, &target[8..])
        } else if lower.starts_with("grpc://") {
            (false, &target[7..])
        } else {
            (false, target)
        };
        let scheme = if secure { "https" } else { "http" };
        let invalid = |error: &dyn std::fmt::Display| {
            Failure::new(Stage::Send, format!("Invalid target \"{target}\": {error}"))
        };
        let mut endpoint = Endpoint::from_shared(format!("{scheme}://{address}"))
            .map_err(|error| invalid(&error))?
            .connect_timeout(millis(connect_ms));
        if secure {
            endpoint = endpoint
                .tls_config(ClientTlsConfig::new().with_native_roots())
                .map_err(|error| Failure::chain(Stage::Tls, &error))?;
        }
        let channel = endpoint.connect_lazy();
        channels.insert(key, channel.clone());
        Ok(channel)
    }

    /// With `events`, the stream sends `{"headers"}` and `{"message"}` events and the answer
    /// carries no messages.
    pub async fn run(
        &self,
        request: Request,
        events: Option<&mpsc::Sender<Value>>,
    ) -> Result<Value, Failure> {
        let started = Instant::now();
        let invalid = |message: String| Failure::new(Stage::Send, message);
        if request.target.trim().is_empty() {
            return Err(invalid("The target is empty".into()));
        }
        let path = PathAndQuery::from_maybe_shared(request.path.clone())
            .ok()
            .filter(|path| path.as_str().starts_with('/'))
            .ok_or_else(|| invalid(format!("Invalid path \"{}\"", request.path)))?;
        let messages = request
            .messages
            .iter()
            .map(|message| B64.decode(message).map(Bytes::from))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| invalid(format!("Invalid message: {error}")))?;
        let mut call = tonic::Request::new(futures_util::stream::iter(messages));
        *call.metadata_mut() = metadata(&request.metadata)?;
        if let Some(ms) = request.timeout_ms {
            call.set_timeout(millis(ms));
        }
        let channel = self.channel(
            request.target.trim(),
            request.connect_ms.unwrap_or(CONNECT_MS),
        )?;
        let work = async {
            let mut client = tonic::client::Grpc::new(channel)
                .max_decoding_message_size(MESSAGE_LIMIT)
                .max_encoding_message_size(MESSAGE_LIMIT);
            client
                .ready()
                .await
                .map_err(|error| Failure::chain(Stage::Connect, &error))?;
            let mut answer = Response::default();
            let response = match client.streaming(call, path, Raw).await {
                Ok(response) => response,
                Err(status) => return finish(answer, status, started),
            };
            answer.timing.first_byte_us = micros(started);
            answer.headers = pairs(response.metadata());
            if let Some(events) = events {
                emit(events, json!({ "headers": answer.headers })).await?;
            }
            let mut stream = response.into_inner();
            loop {
                match stream.message().await {
                    Ok(Some(message)) => {
                        let message = B64.encode(&message);
                        match events {
                            Some(events) => emit(events, json!({ "message": message })).await?,
                            None => answer.messages.push(message),
                        }
                    }
                    Ok(None) => break,
                    Err(status) => return finish(answer, status, started),
                }
            }
            match stream.trailers().await {
                Ok(trailers) => {
                    answer.trailers = trailers.as_ref().map(pairs).unwrap_or_default();
                    answer.timing.total_us = micros(started);
                    Ok(json!(answer))
                }
                Err(status) => finish(answer, status, started),
            }
        };
        match request.timeout_ms {
            Some(ms) => tokio::time::timeout(millis(ms), work)
                .await
                .unwrap_or_else(|_| Err(Failure::new(Stage::Timeout, "The call timed out"))),
            None => work.await,
        }
    }
}

/// A status the server sent ends the call; one with a source is the transport failing.
fn finish(mut answer: Response, status: Status, started: Instant) -> Result<Value, Failure> {
    if std::error::Error::source(&status).is_some() {
        let mut failure = Failure::chain(Stage::Send, &status);
        failure.message = format!("Call failed: {}", status.message());
        return Err(failure);
    }
    answer.status = status.code() as i32;
    answer.message = status.message().to_owned();
    answer.trailers = pairs(status.metadata());
    let total_us = micros(started);
    if answer.timing.first_byte_us == 0 {
        answer.timing.first_byte_us = total_us;
    }
    answer.timing.total_us = total_us;
    Ok(json!(answer))
}

fn metadata(pairs: &[(String, String)]) -> Result<MetadataMap, Failure> {
    let invalid = |name: &str| Failure::new(Stage::Send, format!("Invalid metadata \"{name}\""));
    let mut metadata = MetadataMap::new();
    for (name, value) in pairs {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with("-bin") {
            let key = MetadataKey::from_bytes(lower.as_bytes()).map_err(|_| invalid(name))?;
            let bytes = B64.decode(value).map_err(|_| invalid(name))?;
            metadata.append_bin(key, BinaryMetadataValue::from_bytes(&bytes));
        } else {
            let key = MetadataKey::from_bytes(lower.as_bytes()).map_err(|_| invalid(name))?;
            let value: AsciiMetadataValue = value.parse().map_err(|_| invalid(name))?;
            metadata.append(key, value);
        }
    }
    Ok(metadata)
}

fn pairs(metadata: &MetadataMap) -> Vec<(String, String)> {
    metadata
        .iter()
        .map(|item| match item {
            KeyAndValueRef::Ascii(key, value) => (
                key.to_string(),
                String::from_utf8_lossy(value.as_encoded_bytes()).into_owned(),
            ),
            KeyAndValueRef::Binary(key, value) => (
                key.to_string(),
                B64.encode(value.to_bytes().unwrap_or_default()),
            ),
        })
        .collect()
}

/// Messages pass through as bytes.
#[derive(Clone, Copy, Default)]
struct Raw;

impl Codec for Raw {
    type Encode = Bytes;
    type Decode = Bytes;
    type Encoder = Raw;
    type Decoder = Raw;
    fn encoder(&mut self) -> Raw {
        Raw
    }
    fn decoder(&mut self) -> Raw {
        Raw
    }
}

impl Encoder for Raw {
    type Item = Bytes;
    type Error = Status;
    fn encode(&mut self, item: Bytes, buf: &mut EncodeBuf<'_>) -> Result<(), Status> {
        buf.put(item);
        Ok(())
    }
}

impl Decoder for Raw {
    type Item = Bytes;
    type Error = Status;
    fn decode(&mut self, buf: &mut DecodeBuf<'_>) -> Result<Option<Bytes>, Status> {
        Ok(Some(buf.copy_to_bytes(buf.remaining())))
    }
}
