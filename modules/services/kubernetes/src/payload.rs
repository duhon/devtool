//! The `#payload` of `apply`, `create` and `patch`: YAML or JSON objects. Owned by the language
//! and run part.

use serde::Deserialize;
use serde_json::Value;

/// The documents of the payload: one or more YAML (`---` separated) or JSON values. Empty
/// text gives none. A patch is one document of any shape.
pub fn parse(text: &str) -> Result<Vec<Value>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(vec![]);
    }
    if text.starts_with(['{', '[']) {
        return serde_json::Deserializer::from_str(text)
            .into_iter::<Value>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Invalid JSON in #payload: {error}"));
    }
    let mut documents = Vec::new();
    for document in serde_yaml_ng::Deserializer::from_str(text) {
        let value = Value::deserialize(document)
            .map_err(|error| format!("Invalid YAML in #payload: {error}"))?;
        if !value.is_null() {
            documents.push(value);
        }
    }
    Ok(documents)
}

/// The objects of an `apply` or `create` payload: each has an `apiVersion` and a `kind`, and a
/// `metadata.namespace` that is the target's. A `List` is flattened into its items.
pub fn objects(text: &str, namespace: &str) -> Result<Vec<Value>, String> {
    let mut objects = Vec::new();
    for document in parse(text)? {
        match document {
            Value::Array(items) => objects.extend(items),
            Value::Object(ref map)
                if map.get("kind").and_then(Value::as_str) == Some("List")
                    && map.get("items").is_some_and(Value::is_array) =>
            {
                objects.extend(map["items"].as_array().cloned().unwrap_or_default());
            }
            other => objects.push(other),
        }
    }
    for (index, object) in objects.iter().enumerate() {
        let at = if objects.len() > 1 {
            format!(" (object {})", index + 1)
        } else {
            String::new()
        };
        let Some(map) = object.as_object() else {
            return Err(format!("#payload{at} must be an object"));
        };
        for field in ["apiVersion", "kind"] {
            if map
                .get(field)
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                return Err(format!("#payload{at} needs {field}"));
            }
        }
        if let Some(name) = object["metadata"]["name"].as_str() {
            crate::resources::check_name(name).map_err(|error| format!("#payload{at}: {error}"))?;
        }
        if let Some(found) = object["metadata"]["namespace"].as_str()
            && found != namespace
        {
            return Err(format!(
                "The namespace from #payload{at} ({found}) does not match the namespace of #target ({namespace})"
            ));
        }
    }
    Ok(objects)
}
