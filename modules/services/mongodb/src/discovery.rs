//! Discovery: the collections of a database and the fields of sampled documents. It reads
//! only, with short timeouts and small limits, and is cached per connection and database.

use crate::editor::Catalog;
use serde_json::{Value, json};
use services_sdk::mongodb::{Request, Response};

pub const MAX_COLLECTIONS: u64 = 500;
pub const SAMPLE: u64 = 20;
pub const MAX_FIELDS: usize = 100;
const DEPTH: usize = 3;
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

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub uri: String,
    pub database: String,
    pub collections: Option<(f64, Vec<String>)>,
    pub fields: Vec<(String, f64, Vec<String>)>,
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
                .map(|(name, _, fields)| (name.clone(), fields.clone()))
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
                .any(|(known, at, _)| known == name && now - at < FIELDS_TTL_MS),
        }
    }

    pub fn cooling(&self, now: f64) -> bool {
        self.failed.is_some_and(|at| now - at < COOLDOWN_MS)
    }

    pub fn store(&mut self, scope: &Scope, now: f64, found: Vec<String>) {
        self.failed = None;
        match scope {
            Scope::Collections => self.collections = Some((now, found)),
            Scope::Fields(name) => {
                self.fields.retain(|(known, ..)| known != name);
                self.fields.push((name.clone(), now, found));
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
pub fn found(scope: &Scope, response: &Response) -> Result<Vec<String>, String> {
    if let Some(error) = &response.error {
        return Err(format!("{} ({})", error.message, error.code_name));
    }
    Ok(match scope {
        Scope::Collections => {
            let mut names: Vec<String> = response
                .documents
                .iter()
                .filter_map(|document| document["name"].as_str().map(str::to_owned))
                .collect();
            names.sort();
            names
        }
        Scope::Fields(_) => fields(&response.documents),
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
    fn requests_only_read() {
        let collections = request(&Scope::Collections, "mongodb://h/app", "app");
        assert_eq!(collections.name(), Some("listCollections"));
        assert_eq!(collections.timeout_ms, Some(SHORT_MS));
        let sample = request(&Scope::Fields("users".into()), "mongodb://h/app", "app");
        assert_eq!(sample.command["limit"], SAMPLE);
        assert_eq!(sample.name(), Some("find"));
    }
}
