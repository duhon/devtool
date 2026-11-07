//! Descriptors and messages: `.proto` sources compiled in memory, reflection files, JSON to
//! protobuf and back, and the shape of a message for completion.

use prost::Message;
use prost_reflect::{
    DescriptorPool, DeserializeOptions, DynamicMessage, Kind, MessageDescriptor, MethodDescriptor,
    ReflectMessage, SerializeOptions,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The name the pasted source compiles as.
const INPUT: &str = "input.proto";
/// The most reflection files a service may need.
pub const FILES: usize = 4096;

struct Source(String);

impl protox::file::FileResolver for Source {
    fn open_file(&self, name: &str) -> Result<protox::file::File, protox::Error> {
        if name == INPUT {
            protox::file::File::from_source(name, &self.0)
        } else {
            protox::file::GoogleFileResolver::new().open_file(name)
        }
    }
}

/// A `#proto` source compiled with its imports; the well-known `google/protobuf/*` types
/// are built in.
pub fn compile(source: &str) -> Result<DescriptorPool, String> {
    let mut compiler = protox::Compiler::with_file_resolver(Source(source.into()));
    compiler
        .include_imports(true)
        .open_file(INPUT)
        .map_err(|error| format!("Invalid proto: {error}"))?;
    DescriptorPool::from_file_descriptor_set(compiler.file_descriptor_set())
        .map_err(|error| format!("Invalid proto: {error}"))
}

/// Encoded `FileDescriptorProto`s from reflection, by file name.
#[derive(Clone, Debug, Default)]
pub struct Files(pub BTreeMap<String, prost_types::FileDescriptorProto>);

impl Files {
    /// Adds encoded files; their dependencies not here yet are returned.
    pub fn add(&mut self, files: &[Vec<u8>]) -> Result<Vec<String>, String> {
        for bytes in files {
            let file = prost_types::FileDescriptorProto::decode(bytes.as_slice())
                .map_err(|error| format!("Reflection failed: {error}"))?;
            self.0.insert(file.name().to_owned(), file);
        }
        if self.0.len() > FILES {
            return Err("Reflection descriptor limit exceeded".into());
        }
        Ok(self
            .0
            .values()
            .flat_map(|file| &file.dependency)
            .filter(|name| !self.0.contains_key(*name))
            .cloned()
            .collect())
    }

    pub fn pool(&self) -> Result<DescriptorPool, String> {
        DescriptorPool::from_file_descriptor_set(prost_types::FileDescriptorSet {
            file: self.0.values().cloned().collect(),
        })
        .map_err(|error| format!("Reflection failed: {error}"))
    }
}

/// `service/method` in `pool`.
pub fn method(
    pool: &DescriptorPool,
    service: &str,
    name: &str,
) -> Result<MethodDescriptor, String> {
    pool.get_service_by_name(service)
        .ok_or_else(|| format!("Service not found: {service}"))?
        .methods()
        .find(|method| method.name() == name)
        .ok_or_else(|| format!("Method not found: {service}/{name}"))
}

/// The request messages of a `#message` text: none is `{}`, an array is several.
pub fn messages(text: &str) -> Result<Vec<Value>, String> {
    if text.trim().is_empty() {
        return Ok(vec![json!({})]);
    }
    let parsed: Value =
        serde_json::from_str(text).map_err(|_| "Request message must be valid JSON")?;
    Ok(match parsed {
        Value::Array(items) => items,
        other => vec![other],
    })
}

/// A JSON message encoded as `descriptor`; unknown fields are ignored.
pub fn encode(descriptor: &MessageDescriptor, message: &Value) -> Result<Vec<u8>, String> {
    let raw = message.to_string();
    let mut deserializer = serde_json::Deserializer::from_str(&raw);
    DynamicMessage::deserialize_with_options(
        descriptor.clone(),
        &mut deserializer,
        &DeserializeOptions::new().deny_unknown_fields(false),
    )
    .map(|message| message.encode_to_vec())
    .map_err(|error| format!("Invalid request message: {error}"))
}

/// An encoded message as JSON: proto field names, defaults shown, `bytes` as a Node.js
/// `Buffer` and each set `oneof` named after its field.
pub fn decode(descriptor: &MessageDescriptor, bytes: &[u8]) -> Result<Value, String> {
    let message = DynamicMessage::decode(descriptor.clone(), bytes)
        .map_err(|error| format!("Invalid response message: {error}"))?;
    response_json(&message).map_err(|error| format!("Invalid response message: {error}"))
}

fn response_json(message: &DynamicMessage) -> Result<Value, serde_json::Error> {
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

fn nested(raw: &prost_reflect::Value, serialized: &mut Value) -> Result<(), serde_json::Error> {
    if serialized.is_null() {
        return Ok(());
    }
    match raw {
        prost_reflect::Value::Message(message) => *serialized = response_json(message)?,
        prost_reflect::Value::Bytes(bytes) => {
            *serialized = json!({"type": "Buffer", "data": bytes.as_ref()})
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

/// The fields of a message as JSON completion reads them (`[{name, kind, items, fields}]`),
/// nested messages down to a depth of 5.
pub fn fields(descriptor: &MessageDescriptor, depth: usize) -> Vec<Value> {
    descriptor
        .fields()
        .map(|field| {
            let element = match field.kind() {
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
                match element {
                    "boolean" | "number" => "bare",
                    kind => kind,
                }
            };
            let mut value = json!({"name": field.name(), "kind": kind});
            let shape = if field.is_list() {
                value["items"] = json!({"kind": element});
                &mut value["items"]
            } else {
                &mut value
            };
            if shape["kind"] == "object"
                && !field.is_map()
                && depth <= 5
                && let Kind::Message(nested) = field.kind()
            {
                shape["fields"] = json!(fields(&nested, depth + 1));
            }
            value
        })
        .collect()
}
