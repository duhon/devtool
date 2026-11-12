//! Discovery: the collections of a database and the fields of sampled documents. It reads
//! only, with short timeouts and small limits, and is cached per connection and database.

use crate::editor::Catalog;
use serde_json::{Value, json};
use services_sdk::mongodb::{Request, Response};
use std::collections::BTreeMap;

pub const MAX_COLLECTIONS: u64 = 500;
pub const SAMPLE: u64 = 20;
pub const MAX_FIELDS: usize = 100;
const DEPTH: usize = 3;
/// The most values a field offers, and the most distinct ones it may have.
pub const MAX_VALUES: usize = 10;
const LONGEST_VALUE: usize = 40;
pub const SHORT_MS: u64 = 5000;
pub const COLLECTIONS_TTL_MS: f64 = 300_000.0;
pub const FIELDS_TTL_MS: f64 = 300_000.0;
pub const COOLDOWN_MS: f64 = 30_000.0;
/// Connections remembered, the most recent last.
pub const ENTRIES: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    Collections,
    Fields(String),
}

/// What the sample of a collection showed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub names: Vec<String>,
    /// The values of the fields that have few, as JSON text.
    pub values: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct Sampled {
    pub name: String,
    pub at: f64,
    pub found: Found,
}

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub uri: String,
    pub database: String,
    pub collections: Option<(f64, Vec<String>)>,
    pub fields: Vec<Sampled>,
    pub failed: Option<f64>,
}

impl Entry {
    pub fn catalog(&self) -> Catalog {
        Catalog {
            collections: self
                .collections
                .as_ref()
                .map(|(_, names)| names.clone())
                .unwrap_or_default(),
            fields: self
                .fields
                .iter()
                .map(|sampled| (sampled.name.clone(), sampled.found.names.clone()))
                .collect(),
            values: self
                .fields
                .iter()
                .map(|sampled| (sampled.name.clone(), sampled.found.values.clone()))
                .collect(),
        }
    }

    pub fn fresh(&self, scope: &Scope, now: f64) -> bool {
        match scope {
            Scope::Collections => self
                .collections
                .as_ref()
                .is_some_and(|(at, _)| now - at < COLLECTIONS_TTL_MS),
            Scope::Fields(name) => self
                .fields
                .iter()
                .any(|sampled| sampled.name == *name && now - sampled.at < FIELDS_TTL_MS),
        }
    }

    pub fn cooling(&self, now: f64) -> bool {
        self.failed.is_some_and(|at| now - at < COOLDOWN_MS)
    }

    pub fn store(&mut self, scope: &Scope, now: f64, found: Found) {
        self.failed = None;
        match scope {
            Scope::Collections => self.collections = Some((now, found.names)),
            Scope::Fields(name) => {
                self.fields.retain(|sampled| sampled.name != *name);
                self.fields.push(Sampled {
                    name: name.clone(),
                    at: now,
                    found,
                });
                if self.fields.len() > ENTRIES * 4 {
                    self.fields.remove(0);
                }
            }
        }
    }
}

pub fn request(scope: &Scope, uri: &str, database: &str) -> Request {
    let (command, limit) = match scope {
        Scope::Collections => (crate::action::names_command(), MAX_COLLECTIONS),
        Scope::Fields(name) => (
            json!({"find": name, "filter": {}, "limit": SAMPLE, "singleBatch": true}),
            SAMPLE,
        ),
    };
    Request {
        max_documents: Some(limit),
        timeout_ms: Some(SHORT_MS),
        connect_ms: Some(SHORT_MS),
        ..Request::new(uri, database, command)
    }
}

/// The names or fields an answer holds.
pub fn found(scope: &Scope, response: &Response) -> Result<Found, String> {
    if let Some(error) = &response.error {
        return Err(format!("{} ({})", error.message, error.code_name));
    }
    Ok(match scope {
        Scope::Collections => Found {
            names: {
                let mut names: Vec<String> = response
                    .documents
                    .iter()
                    .filter_map(|document| document["name"].as_str().map(str::to_owned))
                    .collect();
                names.sort();
                names
            },
            values: BTreeMap::new(),
        },
        Scope::Fields(_) => Found {
            names: fields(&response.documents),
            values: values(&response.documents),
        },
    })
}

/// The dotted field paths of documents, `_id` first, nested documents to a small depth.
pub fn fields(documents: &[Value]) -> Vec<String> {
    let mut paths = Vec::new();
    for document in documents {
        collect(document, "", 0, &mut paths);
    }
    if let Some(at) = paths.iter().position(|path| path == "_id") {
        let id = paths.remove(at);
        paths.insert(0, id);
    }
    paths.truncate(MAX_FIELDS);
    paths
}

fn collect(value: &Value, prefix: &str, depth: usize, paths: &mut Vec<String>) {
    let Value::Object(map) = value else { return };
    for (key, inner) in map {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if !paths.contains(&path) && paths.len() < MAX_FIELDS * 2 {
            paths.push(path.clone());
        }
        let inner = match inner {
            Value::Array(items) => items.first().unwrap_or(inner),
            other => other,
        };
        let scalar = matches!(inner, Value::Object(map) if map.len() == 1 && map.keys().all(|key| key.starts_with('$')));
        if inner.is_object() && !scalar && depth + 1 < DEPTH {
            collect(inner, &path, depth + 1, paths);
        }
    }
}

/// The values of the fields that have few distinct ones: scalars only, in the order seen.
/// A field of an array contributes its elements; ids, dates, and long strings rule a field
/// out, as does a sample of less than two documents.
pub fn values(documents: &[Value]) -> BTreeMap<String, Vec<String>> {
    let mut seen: BTreeMap<String, Slot> = BTreeMap::new();
    if documents.len() >= 2 {
        for document in documents {
            leaves(document, "", 0, &mut seen);
        }
    }
    seen.into_iter()
        .filter(|(path, slot)| path != "_id" && !slot.ruled_out && !slot.values.is_empty())
        .map(|(path, slot)| (path, slot.values))
        .collect()
}

fn leaves(value: &Value, prefix: &str, depth: usize, seen: &mut BTreeMap<String, Slot>) {
    let Value::Object(map) = value else { return };
    for (key, inner) in map {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let items = match inner {
            Value::Array(items) => items.as_slice(),
            other => std::slice::from_ref(other),
        };
        for item in items {
            match item {
                Value::Object(map) if !map.keys().any(|key| key.starts_with('$')) => {
                    if depth + 1 < DEPTH {
                        leaves(item, &path, depth + 1, seen);
                    }
                }
                Value::Null => {}
                other => note(seen.entry(path.clone()).or_default(), literal(other)),
            }
        }
    }
}

#[derive(Default)]
struct Slot {
    values: Vec<String>,
    ruled_out: bool,
}

fn note(slot: &mut Slot, literal: Option<String>) {
    match literal {
        Some(text) if slot.values.contains(&text) => {}
        Some(text) if slot.values.len() < MAX_VALUES => slot.values.push(text),
        _ => slot.ruled_out = true,
    }
}

/// A scalar as the JSON a filter holds; `None` for what is not worth offering.
fn literal(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if text.chars().count() <= LONGEST_VALUE => Some(value.to_string()),
        Value::Bool(_) | Value::Number(_) => Some(value.to_string()),
        Value::Object(map) if map.len() == 1 => {
            let (key, inner) = map.iter().next()?;
            match (key.as_str(), inner) {
                ("$numberInt" | "$numberLong" | "$numberDouble", Value::String(number))
                    if number.parse::<f64>().is_ok_and(f64::is_finite) =>
                {
                    Some(number.clone())
                }
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_flattened_with_limits() {
        let docs = [
            json!({"name": "a", "_id": {"$oid": "x"}, "address": {"city": "P", "geo": {"lat": {"$numberInt": "1"}, "deep": {"x": 1}}},
                "orders": [{"sku": "s"}]}),
            json!({"name": "b", "extra": 1}),
        ];
        assert_eq!(
            fields(&docs),
            [
                "_id",
                "name",
                "address",
                "address.city",
                "address.geo",
                "address.geo.lat",
                "address.geo.deep",
                "orders",
                "orders.sku",
                "extra"
            ]
        );
        let wide: serde_json::Map<String, Value> =
            (0..300).map(|i| (format!("f{i}"), json!(i))).collect();
        assert_eq!(fields(&[Value::Object(wide)]).len(), MAX_FIELDS);
    }

    #[test]
    fn values_are_few_scalars_of_a_real_sample() {
        let docs = [
            json!({"_id": {"$oid": "a"}, "kind": "a", "n": {"$numberInt": "1"}, "tags": ["x", "y"], "at": {"$date": "t"}, "note": "this note is far longer than forty characters"}),
            json!({"_id": {"$oid": "b"}, "kind": "b", "n": {"$numberLong": "2"}, "tags": ["x"], "at": {"$date": "u"}, "note": "n", "deep": {"ok": true}}),
        ];
        let found = values(&docs);
        assert_eq!(found["kind"], ["\"a\"", "\"b\""]);
        assert_eq!(found["n"], ["1", "2"]);
        assert_eq!(found["tags"], ["\"x\"", "\"y\""]);
        assert_eq!(found["deep.ok"], ["true"]);
        assert!(
            !found.contains_key("_id") && !found.contains_key("at") && !found.contains_key("note")
        );
        assert!(values(&docs[..1]).is_empty());
        let many: Vec<Value> = (0..12).map(|i| json!({"v": i})).collect();
        assert!(values(&many).is_empty());
    }

    #[test]
    fn requests_only_read() {
        let collections = request(&Scope::Collections, "mongodb://h/app", "app");
        assert_eq!(collections.name(), Some("listCollections"));
        assert_eq!(collections.timeout_ms, Some(SHORT_MS));
        let sample = request(&Scope::Fields("users".into()), "mongodb://h/app", "app");
        assert_eq!(sample.command["limit"], SAMPLE);
        assert_eq!(sample.name(), Some("find"));
    }
}
