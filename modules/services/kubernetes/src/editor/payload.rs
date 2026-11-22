//! The `#payload` of a check: its YAML or JSON documents, completion of their fields by the
//! OpenAPI schema of the kind and the marks that follow from what discovery found.

use super::yaml::{self, Walker};
use crate::discovery::Catalog;
use crate::discovery::openapi::{Nested, Property, Step};
use crate::resources::Resource;
use serde_json::Value;
use services_sdk::lang::check::At;
use services_sdk::lang::json::{self, Segment};
use services_sdk::lang::{finish, item};
use services_sdk::{Completions, Item};
use std::collections::BTreeSet;

/// One document of a payload: its lines `first..last` and what it says it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    pub first: usize,
    pub last: usize,
    pub api_version: Option<String>,
    pub kind: Option<String>,
    pub json: bool,
}

fn separator(line: &str) -> bool {
    let line = line.trim_end();
    line == "---" || line.starts_with("--- ")
}

fn is_json(lines: &[&str]) -> bool {
    lines
        .iter()
        .map(|line| line.trim())
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .is_some_and(|line| line.starts_with(['{', '[']))
}

/// The scalar of the top-level `key` of a document; `{{…}}` is a scalar like any other.
fn scalar(lines: &[&str], key: &str, json: bool) -> Option<String> {
    lines.iter().find_map(|line| {
        let text = if json {
            line.trim_start_matches(['{', ',', ' ', '\t'])
        } else if line.starts_with(' ') {
            return None;
        } else {
            line
        };
        let text = text
            .strip_prefix(key)
            .or_else(|| text.strip_prefix(&format!("\"{key}\"")))?;
        let value = text.trim_start().strip_prefix(':')?;
        let value = value.split(" #").next().unwrap_or("");
        let value = value
            .trim()
            .trim_end_matches([',', '}'])
            .trim()
            .trim_matches(['"', '\'']);
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// The documents of a payload body.
pub fn documents(body: &str) -> Vec<Doc> {
    let lines: Vec<&str> = body.split('\n').collect();
    let mut docs = Vec::new();
    let mut first = 0;
    let mut close = |first: usize, last: usize| {
        let part = &lines[first..last];
        let json = is_json(part);
        docs.push(Doc {
            first,
            last,
            api_version: scalar(part, "apiVersion", json),
            kind: scalar(part, "kind", json),
            json,
        });
    };
    for (at, line) in lines.iter().enumerate() {
        if separator(line) {
            close(first, at);
            first = at + 1;
        }
    }
    close(first, lines.len());
    docs
}

/// The distinct `apiVersion`s the documents name, for the schemas to read.
pub fn api_versions(body: &str) -> Vec<String> {
    let mut found = Vec::new();
    for doc in documents(body) {
        if let Some(version) = doc.api_version
            && !version.contains("{{")
            && !found.contains(&version)
        {
            found.push(version);
        }
    }
    found
}

/// The schema of a document: by its `apiVersion` and `kind`, or by the resource of the action.
fn root<'a>(
    known: &'a Catalog,
    doc: &Doc,
    resource: Option<&Resource>,
) -> Option<(
    &'a crate::discovery::openapi::Document,
    &'a Value,
    String,
    String,
)> {
    let (version, kind) = match (&doc.api_version, &doc.kind) {
        (Some(version), Some(kind)) => (version.clone(), kind.clone()),
        _ => {
            let resource = resource?;
            (resource.api_version(), resource.kind.clone())
        }
    };
    let document = known.document(&version)?;
    let root = document.root(&version, &kind)?;
    Some((document, root, version, kind))
}

fn property_item(property: &Property, indent: usize, json: bool) -> Item {
    let detail = if property.required {
        format!("required · {} · {}", property.kind, property.summary)
    } else {
        format!("{} · {}", property.kind, property.summary)
    };
    let detail = detail.trim_end_matches([' ', '·']).to_owned();
    let name = &property.name;
    let mut found = if json {
        item(format!("\"{name}\": "), &detail, "property")
    } else {
        let block = " ".repeat(indent + 2);
        match property.nested {
            Nested::Scalar => item(format!("{name}: "), &detail, "property"),
            Nested::Object => item(format!("{name}:\n{block}"), &detail, "property"),
            Nested::Array => item(format!("{name}:\n{block}- "), &detail, "property"),
        }
    };
    found.next = true;
    found
}

fn values(items: impl IntoIterator<Item = String>, detail: &str) -> Vec<Item> {
    items
        .into_iter()
        .map(|value| item(value, detail, "enum"))
        .collect()
}

/// The `apiVersion`s of the catalog, and the kinds served at one.
fn api_values(known: &Catalog, key: &str, doc: &Doc) -> Vec<Item> {
    let Some(resources) = &known.resources else {
        return vec![];
    };
    let mut found = BTreeSet::new();
    match key {
        "apiVersion" => {
            found.extend(resources.iter().map(Resource::api_version));
        }
        _ => {
            for resource in resources.iter() {
                let version = resource.api_version();
                if doc
                    .api_version
                    .as_deref()
                    .is_none_or(|known| known == version)
                {
                    found.insert(resource.kind.clone());
                }
            }
        }
    }
    values(found, if key == "kind" { "Kind" } else { "API version" })
}

const KEYS: [(&str, &str); 4] = [
    ("apiVersion", "API version of the object"),
    ("kind", "Kind of the object"),
    ("metadata", "Name, namespace, labels…"),
    ("spec", "Desired state"),
];

/// Completion in the payload of an `apply`, `create` or `patch`.
pub fn complete(at: &At, known: &Catalog, resource: Option<&Resource>) -> Completions {
    let body = at.whole();
    let first_line = at.body_start();
    let docs = documents(body);
    let line = at.line - first_line;
    let Some(doc) = docs.iter().find(|doc| line >= doc.first && line < doc.last) else {
        return at.none();
    };
    let lines: Vec<&str> = body.split('\n').collect();
    let schema = root(known, doc, resource);
    let before = at.before;
    let items = if doc.json {
        json_items(&lines[doc.first..line], before, &schema, known, doc)
    } else {
        yaml_items(
            &lines[doc.first..doc.last],
            line - doc.first,
            before,
            &schema,
            known,
            doc,
        )
    };
    let typed = &before[json_word(before, doc.json)..];
    finish(at.pos - typed.len(), at.pos, typed, items)
}

fn json_word(before: &str, json: bool) -> usize {
    if json {
        before
            .rfind([' ', '\t', '{', '[', ',', ':'])
            .map_or(0, |at| at + 1)
    } else {
        yaml::word_start(before)
    }
}

fn yaml_items(
    lines: &[&str],
    cursor: usize,
    before: &str,
    schema: &Option<(&crate::discovery::openapi::Document, &Value, String, String)>,
    known: &Catalog,
    doc: &Doc,
) -> Vec<Item> {
    let mut walker = Walker::default();
    let mut seen = Vec::new();
    for line in &lines[..cursor] {
        if let Some(read) = yaml::read(line) {
            let at = walker.feed(&read);
            if let Some((name, ..)) = read.key {
                seen.push((at.mapping, name.to_owned()));
            }
        }
    }
    let read = yaml::read(before);
    let (dashes, indent) = read.as_ref().map_or((vec![], before.len()), |read| {
        (read.dashes.clone(), read.indent)
    });
    let place = walker.peek(&dashes, indent);
    let key = read.as_ref().and_then(|read| {
        let (name, _, end) = read.key?;
        before[end..].contains(':').then(|| name.to_owned())
    });
    if let Some(key) = key {
        let top = place.path.is_empty();
        if top && matches!(key.as_str(), "apiVersion" | "kind") {
            return api_values(known, &key, doc);
        }
        let namespace = place.path == [Step::Key("metadata".into())] && key == "namespace";
        if namespace {
            let list = known
                .namespaces
                .iter()
                .flat_map(|list| list.iter().cloned());
            return values(list, "Namespace");
        }
        let Some((document, root, ..)) = schema else {
            return vec![];
        };
        let mut path = place.path.clone();
        path.push(Step::Key(key));
        let node = document.at(root, &path);
        return values(
            node.map(|node| document.values(node)).unwrap_or_default(),
            "Value",
        );
    }
    let mut used = BTreeSet::new();
    for (mapping, name) in seen {
        if mapping == place.mapping {
            used.insert(name);
        }
    }
    if dashes.is_empty() {
        for line in &lines[cursor + 1..] {
            let Some(read) = yaml::read(line) else {
                continue;
            };
            let at = walker.feed(&read);
            if at.mapping == place.mapping
                && let Some((name, ..)) = read.key
            {
                used.insert(name.to_owned());
            }
        }
    }
    match schema {
        Some((document, root, ..)) => {
            let Some(node) = document.at(root, &place.path) else {
                return vec![];
            };
            document
                .properties(node)
                .iter()
                .filter(|property| !used.contains(&property.name))
                .map(|property| property_item(property, indent, false))
                .collect()
        }
        None if place.path.is_empty() => KEYS
            .iter()
            .filter(|(name, _)| !used.contains(*name))
            .map(|(name, detail)| {
                let mut found = item(format!("{name}: "), detail, "property");
                found.next = true;
                found
            })
            .collect(),
        None => vec![],
    }
}

fn json_items(
    previous: &[&str],
    before: &str,
    schema: &Option<(&crate::discovery::openapi::Document, &Value, String, String)>,
    known: &Catalog,
    doc: &Doc,
) -> Vec<Item> {
    let mut source = previous.join("\n");
    source.push('\n');
    source.push_str(before);
    let word = json_word(before, true);
    let head = source[..source.len() - (before.len() - word)].trim_end();
    let Some((path, used, pending)) = json::path(head) else {
        return vec![];
    };
    let steps: Vec<Step> = path
        .iter()
        .map(|segment| match segment {
            Segment::Field(name) => Step::Key(name.clone()),
            Segment::Item => Step::Item,
        })
        .collect();
    if head.ends_with(':')
        && let Some(key) = pending
    {
        if steps.is_empty() && matches!(key.as_str(), "apiVersion" | "kind") {
            return api_values(known, &key, doc);
        }
        let Some((document, root, ..)) = schema else {
            return vec![];
        };
        let mut steps = steps;
        steps.push(Step::Key(key));
        let node = document.at(root, &steps);
        return values(
            node.map(|node| document.values(node)).unwrap_or_default(),
            "Value",
        );
    }
    if !(head.ends_with('{') || head.ends_with(',')) {
        return vec![];
    }
    match schema {
        Some((document, root, ..)) => {
            let Some(node) = document.at(root, &steps) else {
                return vec![];
            };
            document
                .properties(node)
                .iter()
                .filter(|property| !used.contains(&property.name))
                .map(|property| property_item(property, 0, true))
                .collect()
        }
        None if steps.is_empty() => KEYS
            .iter()
            .filter(|(name, _)| !used.contains(*name))
            .map(|(name, detail)| item(format!("\"{name}\": "), detail, "property"))
            .collect(),
        None => vec![],
    }
}

/// A mark: byte range of a line and why.
pub struct Mark {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// The errors of a YAML payload `body` the catalog shows: a namespace other than the target's
/// and, for `strict` payloads, fields the schema does not have.
pub fn marks(body: &str, known: &Catalog, namespace: Option<&str>, strict: bool) -> Vec<Mark> {
    let lines: Vec<&str> = body.split('\n').collect();
    let mut found = Vec::new();
    for doc in documents(body) {
        if doc.json {
            continue;
        }
        let schema = root(known, &doc, None);
        let mut walker = Walker::default();
        for (at, line) in lines.iter().enumerate().take(doc.last).skip(doc.first) {
            let Some(read) = yaml::read(line) else {
                continue;
            };
            let place = walker.feed(&read);
            let Some((name, start, end)) = read.key else {
                continue;
            };
            let value = read
                .value
                .split(" #")
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches(['"', '\'']);
            if let Some(namespace) = namespace
                && name == "namespace"
                && place.path == [Step::Key("metadata".into())]
                && !value.is_empty()
                && !value.contains("{{")
                && value != namespace
            {
                let from = read.value_start;
                found.push(Mark {
                    line: at,
                    start: from,
                    end: from + read.value.split(" #").next().unwrap_or("").trim_end().len(),
                });
            }
            if strict
                && let Some((document, root, ..)) = &schema
                && !name.contains("{{")
                && let Some(node) = document.at(root, &place.path)
                && let Some(names) = document.closed(node)
                && !names.contains(name)
            {
                found.push(Mark {
                    line: at,
                    start,
                    end,
                });
            }
        }
    }
    found
}
