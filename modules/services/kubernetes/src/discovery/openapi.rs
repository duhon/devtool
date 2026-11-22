//! The OpenAPI v3 documents of the API server (`GET /openapi/v3`, then one document per group
//! version): where the index puts a document, and reading a document's schemas for completion
//! and marks of `#payload`. Pure.

use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The index of the documents.
pub const INDEX: &str = "/openapi/v3";

/// Deepest chain of `$ref`s followed.
const DEPTH: usize = 16;

/// The key of a group version in the index: `api/v1`, `apis/apps/v1`.
fn key(api_version: &str) -> String {
    match api_version.split_once('/') {
        Some((group, version)) => format!("apis/{group}/{version}"),
        None => format!("api/{api_version}"),
    }
}

/// The path and query the index gives the document of `api_version`.
pub fn locate(index: &Value, api_version: &str) -> Option<(String, BTreeMap<String, String>)> {
    let url = index["paths"][key(api_version)]["serverRelativeURL"].as_str()?;
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let query = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    Some((path.to_owned(), query))
}

/// A step from an object to a nested value of its schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Key(String),
    Item,
}

/// What a field holds, to start its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nested {
    Scalar,
    Object,
    Array,
}

/// A field of an object schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    /// `string`, `[]Container`, `Probe`…
    pub kind: String,
    /// The first sentence of the description.
    pub summary: String,
    pub required: bool,
    pub nested: Nested,
}

/// The schemas of one document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    schemas: Map<String, Value>,
}

impl Document {
    /// The schemas of an OpenAPI v3 document; `None` when it has none.
    pub fn parse(mut body: Value) -> Option<Self> {
        let Value::Object(schemas) = body["components"]["schemas"].take() else {
            return None;
        };
        (!schemas.is_empty()).then_some(Self { schemas })
    }

    /// The schema of the object `kind` of `api_version`.
    pub fn root(&self, api_version: &str, kind: &str) -> Option<&Value> {
        let (group, version) = api_version.split_once('/').unwrap_or(("", api_version));
        self.schemas.values().find(|schema| {
            schema["x-kubernetes-group-version-kind"]
                .as_array()
                .is_some_and(|list| {
                    list.iter().any(|entry| {
                        entry["kind"] == kind
                            && entry["group"].as_str().unwrap_or("") == group
                            && entry["version"] == version
                    })
                })
        })
    }

    /// Follows `$ref` and a lone `allOf`.
    fn deref<'a>(&'a self, mut schema: &'a Value) -> &'a Value {
        for _ in 0..DEPTH {
            if let Some(name) = schema["$ref"]
                .as_str()
                .and_then(|reference| reference.rsplit('/').next())
            {
                match self.schemas.get(name) {
                    Some(found) => schema = found,
                    None => break,
                }
            } else if let Some([only]) = schema["allOf"].as_array().map(Vec::as_slice) {
                schema = only;
            } else {
                break;
            }
        }
        schema
    }

    /// The schema at `path` below `root`.
    pub fn at<'a>(&'a self, root: &'a Value, path: &[Step]) -> Option<&'a Value> {
        let mut schema = self.deref(root);
        for step in path {
            let next = match step {
                Step::Key(name) => schema["properties"]
                    .get(name)
                    .or_else(|| schema.get("additionalProperties").filter(|v| v.is_object())),
                Step::Item => schema.get("items"),
            };
            schema = self.deref(next?);
        }
        Some(schema)
    }

    /// The fields of an object schema, the usual ones first.
    pub fn properties(&self, schema: &Value) -> Vec<Property> {
        let schema = self.deref(schema);
        let required: BTreeSet<&str> = schema["required"]
            .as_array()
            .map(|list| list.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut found: Vec<Property> = schema["properties"]
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(name, property)| {
                        self.property(name, property, required.contains(name.as_str()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let rank = |name: &str| match name {
            "apiVersion" => 0,
            "kind" => 1,
            "metadata" => 2,
            _ => 3,
        };
        found.sort_by_key(|property| (rank(&property.name), !property.required));
        found
    }

    fn property(&self, name: &str, property: &Value, required: bool) -> Property {
        let target = self.deref(property);
        let description = property["description"]
            .as_str()
            .or_else(|| target["description"].as_str())
            .unwrap_or("");
        let nested = if target["type"] == "array" {
            Nested::Array
        } else if target["properties"].is_object()
            || (target["type"] == "object" && !target["additionalProperties"].is_object())
        {
            Nested::Object
        } else {
            Nested::Scalar
        };
        Property {
            name: name.to_owned(),
            kind: self.kind(property),
            summary: sentence(description),
            required,
            nested,
        }
    }

    /// The type of a schema as a short word.
    fn kind(&self, schema: &Value) -> String {
        let named = schema["$ref"]
            .as_str()
            .or_else(|| schema["allOf"][0]["$ref"].as_str())
            .and_then(|reference| reference.rsplit('.').next());
        let target = self.deref(schema);
        match target["type"].as_str() {
            Some("array") => format!("[]{}", self.kind(&target["items"])),
            Some("object") if target["additionalProperties"].is_object() => {
                format!("map[string]{}", self.kind(&target["additionalProperties"]))
            }
            Some(kind) => named.unwrap_or(kind).to_owned(),
            None => named.unwrap_or("object").to_owned(),
        }
    }

    /// The values a scalar at `schema` may take: its enum, or `true` and `false`.
    pub fn values(&self, schema: &Value) -> Vec<String> {
        let schema = self.deref(schema);
        if let Some(list) = schema["enum"].as_array() {
            return list
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect();
        }
        if schema["type"] == "boolean" {
            return vec!["true".into(), "false".into()];
        }
        Vec::new()
    }

    /// The names of the fields of `schema` when it admits no others.
    pub fn closed(&self, schema: &Value) -> Option<BTreeSet<String>> {
        let schema = self.deref(schema);
        let properties = schema["properties"]
            .as_object()
            .filter(|map| !map.is_empty())?;
        let open = schema["additionalProperties"].is_object()
            || schema["additionalProperties"] == true
            || schema["x-kubernetes-preserve-unknown-fields"] == true;
        (!open).then(|| properties.keys().cloned().collect())
    }
}

/// The first sentence of a description, short enough for a hint.
fn sentence(text: &str) -> String {
    let text = text.split('\n').next().unwrap_or("");
    let end = text.find(". ").map_or(text.len(), |at| at + 1);
    let text = &text[..end];
    if text.chars().count() > 90 {
        let cut: String = text.chars().take(87).collect();
        format!("{}...", cut.trim_end())
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document() -> Document {
        Document::parse(json!({"components": {"schemas": {
            "io.k8s.api.apps.v1.Deployment": {
                "type": "object",
                "x-kubernetes-group-version-kind": [{"group": "apps", "kind": "Deployment", "version": "v1"}],
                "required": ["spec"],
                "properties": {
                    "kind": {"type": "string", "description": "Kind. More."},
                    "metadata": {"allOf": [{"$ref": "#/components/schemas/io.k8s.meta.ObjectMeta"}], "description": "Standard metadata."},
                    "spec": {"allOf": [{"$ref": "#/components/schemas/io.k8s.api.apps.v1.DeploymentSpec"}]}
                }
            },
            "io.k8s.meta.ObjectMeta": {"type": "object", "properties": {
                "name": {"type": "string"},
                "labels": {"type": "object", "additionalProperties": {"type": "string"}}
            }},
            "io.k8s.api.apps.v1.DeploymentSpec": {"type": "object", "properties": {
                "replicas": {"type": "integer"},
                "paused": {"type": "boolean"},
                "containers": {"type": "array", "items": {"$ref": "#/components/schemas/io.k8s.meta.ObjectMeta"}},
                "strategy": {"type": "string", "enum": ["Recreate", "RollingUpdate"]}
            }}
        }}}))
        .unwrap()
    }

    #[test]
    fn the_index_locates_documents_by_group_version() {
        let index = json!({"paths": {
            "api/v1": {"serverRelativeURL": "/openapi/v3/api/v1?hash=AB"},
            "apis/apps/v1": {"serverRelativeURL": "/openapi/v3/apis/apps/v1?hash=CD"}
        }});
        let (path, query) = locate(&index, "apps/v1").unwrap();
        assert_eq!(path, "/openapi/v3/apis/apps/v1");
        assert_eq!(query["hash"], "CD");
        assert_eq!(locate(&index, "v1").unwrap().0, "/openapi/v3/api/v1");
        assert_eq!(locate(&index, "batch/v1"), None);
    }

    #[test]
    fn schemas_resolve_references_along_a_path() {
        let document = document();
        let root = document.root("apps/v1", "Deployment").unwrap();
        assert!(document.root("v1", "Deployment").is_none());
        let names: Vec<_> = document
            .properties(root)
            .into_iter()
            .map(|property| (property.name, property.required))
            .collect();
        assert_eq!(
            names,
            [
                ("kind".into(), false),
                ("metadata".into(), false),
                ("spec".into(), true)
            ]
        );
        let spec = document.at(root, &[Step::Key("spec".into())]).unwrap();
        let fields = document.properties(spec);
        assert_eq!(fields.len(), 4);
        let containers = fields.iter().find(|f| f.name == "containers").unwrap();
        assert_eq!(
            (containers.kind.as_str(), containers.nested),
            ("[]ObjectMeta", Nested::Array)
        );
        let item = document
            .at(spec, &[Step::Key("containers".into()), Step::Item])
            .unwrap();
        assert!(document.closed(item).unwrap().contains("name"));
        let metadata = [Step::Key("metadata".into())];
        let label = document
            .at(
                root,
                &[
                    metadata[0].clone(),
                    Step::Key("labels".into()),
                    Step::Key("app".into()),
                ],
            )
            .unwrap();
        assert_eq!(label["type"], "string");
        let strategy = document.at(spec, &[Step::Key("strategy".into())]).unwrap();
        assert_eq!(document.values(strategy), ["Recreate", "RollingUpdate"]);
        let paused = document.at(spec, &[Step::Key("paused".into())]).unwrap();
        assert_eq!(document.values(paused), ["true", "false"]);
        let labels = document
            .at(root, &[metadata[0].clone(), Step::Key("labels".into())])
            .unwrap();
        assert_eq!(document.closed(labels), None);
    }

    #[test]
    fn descriptions_keep_their_first_sentence() {
        assert_eq!(sentence("Kind. More."), "Kind.");
        assert_eq!(sentence("a\nb"), "a");
        assert!(sentence(&"x".repeat(200)).ends_with("..."));
    }
}
