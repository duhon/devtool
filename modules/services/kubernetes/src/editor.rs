//! The editor: highlighting, completion, marks and the schema for the Agent. Owned by the
//! discovery and editor part.
//!
//! The editor reads the text leniently, because it is being typed: `k8s://context/namespace`,
//! the action's verb and `resource[/name]`, `Key: value` lines and YAML or JSON documents. The
//! language of a run is [`crate::target`], [`crate::action`] and [`crate::payload`]; this side
//! must also work on text those reject.

mod payload;
mod yaml;

use crate::SECTIONS;
use crate::action::{self, words};
use crate::config::{self, METADATA, OPTIONS};
use crate::discovery::Catalog;
use crate::resources::{self, Resource};
use serde_json::{Value, json};
use services_sdk::lang::check::At;
use services_sdk::lang::{finish, item, json as json_lang, key_value, marker};
use services_sdk::{Check, Completions, Item, Line, LineColumns, Token};
use std::collections::BTreeSet;

const SCHEME: &str = "k8s://";

/// The verbs of `#action`: the text, what it does and whether a `resource[/name]` follows.
const VERBS: [(&str, &str, bool); 19] = [
    ("get", "Read objects", true),
    ("describe", "Describe an object and its events", true),
    ("logs", "Logs of a pod or workload", true),
    ("events", "Events, of an object or all", true),
    ("top", "Resource usage of pods or nodes", true),
    ("rollout status", "Rollout progress", true),
    ("rollout history", "Revisions of a rollout", true),
    ("rollout restart", "Restart a rollout", true),
    ("rollout undo", "Roll back a rollout", true),
    ("auth can-i", "Whether an action is allowed", true),
    ("watch", "Watch objects change", true),
    ("api-resources", "The resources of the API", false),
    ("apply", "Apply the #payload", false),
    ("create", "Create from the #payload", false),
    ("patch", "Patch with the #payload", true),
    ("scale", "Set the replicas", true),
    ("delete", "Delete objects", true),
    ("label", "Set labels with the #payload", true),
    ("annotate", "Set annotations with the #payload", true),
];

/// Keys the console owns; they are never completed or marked here.
const CONSOLE: [&str; 3] = ["repeat", "repeat-until", "timeout"];

fn known_verb(verb: &str) -> bool {
    VERBS.iter().any(|(name, ..)| *name == verb)
}

// ---------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------

/// `k8s://context/namespace` read from the first line of a target, with byte ranges in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub context: String,
    pub namespace: Option<String>,
    pub context_range: (usize, usize),
    pub namespace_range: Option<(usize, usize)>,
}

/// Reads a target. With the `contexts` known the longest one that is a prefix of the rest wins,
/// since a context may hold `/` (EKS ARNs); otherwise the first `/` ends the context.
pub fn place(text: &str, contexts: Option<&[String]>) -> Option<Place> {
    let line = text.lines().next().unwrap_or("").trim_end();
    let lead = line.len() - line.trim_start().len();
    if !line[lead..]
        .get(..SCHEME.len())?
        .eq_ignore_ascii_case(SCHEME)
    {
        return None;
    }
    let base = lead + SCHEME.len();
    let rest = &line[base..];
    let length = contexts
        .and_then(|list| {
            list.iter()
                .filter(|name| rest == name.as_str() || rest.starts_with(&format!("{name}/")))
                .map(|name| name.len())
                .max()
        })
        .unwrap_or_else(|| rest.find('/').unwrap_or(rest.len()));
    if length == 0 {
        return None;
    }
    let namespace = rest[length..].strip_prefix('/').and_then(|tail| {
        let end = tail.find('/').unwrap_or(tail.len());
        let start = base + length + 1;
        let name = tail[..end].trim();
        (!name.is_empty()).then(|| (name.to_owned(), (start, start + end)))
    });
    Some(Place {
        context: rest[..length].to_owned(),
        context_range: (base, base + length),
        namespace: namespace.as_ref().map(|(name, _)| name.clone()),
        namespace_range: namespace.map(|(_, range)| range),
    })
}

fn two_words(first: &str) -> bool {
    matches!(first, "rollout" | "auth")
}

/// Which argument of the verb is its `resource[/name]`.
fn resource_arg(verb: &str) -> Option<usize> {
    match verb {
        "auth can-i" => Some(1),
        _ => action::takes_resource(verb).then_some(0),
    }
}

fn first_line(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
}

/// The resource of an action, and the object it names.
pub fn action_resource<'a>(
    text: &str,
    resources: &'a [Resource],
) -> Option<(&'a Resource, Option<String>)> {
    let shape = action::shape(first_line(text));
    let (_, resource) = shape.resource?;
    let resource = resources::resolve(resources, resource)?;
    Some((resource, shape.name.map(|(_, name)| name.to_owned())))
}

/// The first line of the action, resolved; empty while a variable is not.
fn action_line(known: &Catalog, check: &Check) -> String {
    let raw = check.text_of("action");
    let text = match known.action.as_deref() {
        Some(text) => text,
        None if raw.contains("{{") => "",
        None => raw,
    };
    first_line(text).to_owned()
}

fn verb_of(known: &Catalog, check: &Check) -> String {
    action::shape(&action_line(known, check)).verb
}

fn has_verb(resource: &Resource, verb: &str) -> bool {
    resource.verbs.is_empty() || resource.has_verb(verb)
}

/// Whether the resource suits the verb.
fn fits(verb: &str, resource: &Resource) -> bool {
    let plural = resource.plural.as_str();
    let apps = resource.group == "apps";
    match verb {
        "logs" => {
            (resource.group.is_empty() && plural == "pods")
                || (apps
                    && matches!(
                        plural,
                        "deployments" | "statefulsets" | "daemonsets" | "replicasets"
                    ))
                || (resource.group == "batch" && matches!(plural, "jobs" | "cronjobs"))
        }
        "scale" => resource.has_subresource("scale"),
        "top" => resource.group.is_empty() && matches!(plural, "pods" | "nodes"),
        verb if verb.starts_with("rollout") => {
            apps && matches!(plural, "deployments" | "statefulsets" | "daemonsets")
        }
        "get" => has_verb(resource, "get"),
        "watch" => has_verb(resource, "watch"),
        "delete" => has_verb(resource, "delete"),
        "patch" | "label" | "annotate" => has_verb(resource, "patch"),
        _ => true,
    }
}

// ---------------------------------------------------------------------------------------------
// What discovery should fetch
// ---------------------------------------------------------------------------------------------

/// What a check needs from the cluster, given its text and the cursor.
#[derive(Debug, Default)]
pub struct Needs {
    pub context: Option<String>,
    pub namespaces: bool,
    pub resources: bool,
    pub objects: Option<(Resource, String)>,
    pub containers: Option<(Resource, String, String)>,
    /// `apiVersion`s whose OpenAPI documents the payload needs.
    pub schemas: Vec<String>,
}

/// Whether the cursor is on the value of metadata `key`.
fn on_value(at: &At, key: &str) -> bool {
    at.name() == Some("metadata") && at.before.contains(':') && key_value::name(at.before) == key
}

/// What the check needs; `pos` is the cursor of a hints request, a document has none.
pub fn needs(check: &Check, pos: Option<usize>, known: &Catalog) -> Needs {
    let mut needs = Needs::default();
    let at = At::new(&check.text, pos.unwrap_or(check.text.len()));
    if at.in_response() || (pos.is_some() && at.in_placeholder()) {
        return needs;
    }
    let Some(place) = known.place() else {
        return needs;
    };
    let in_target = pos.is_some() && at.name() == Some("target");
    if in_target && at.before.len() <= place.context_range.1 {
        return needs;
    }
    let listed = known.contexts.as_ref();
    if listed.is_some_and(|list| !list.contains(&place.context)) {
        return needs;
    }
    needs.context = Some(place.context.clone());
    needs.namespaces = place.namespace.is_some() || in_target;
    needs.resources = true;
    let namespace = place.namespace.clone().unwrap_or_default();
    let Some(action) = known.action.as_deref() else {
        return needs;
    };
    let read = action::shape(first_line(action));
    if let Some(resources) = &known.resources
        && let Some((resource, name)) = action_resource(action, resources)
    {
        // Names are listed once one is being typed: after a `/`, or after the resource word.
        let naming = read.slash
            || read.name.is_some()
            || (pos.is_some()
                && at.name() == Some("action")
                && at.before.ends_with(char::is_whitespace)
                && action::takes_spaced_name(&read.verb));
        if naming || (pos.is_some() && on_value(&at, "selector")) {
            needs.objects = Some((resource.clone(), namespace.clone()));
        }
        let container = check
            .text_of("metadata")
            .lines()
            .any(|line| line.contains(':') && key_value::name(line) == "container");
        if let Some(name) = name
            && (container || (pos.is_some() && on_value(&at, "container")))
        {
            needs.containers = Some((resource.clone(), namespace, name));
        }
        if read.verb == "patch" && !patch_json(check) {
            needs.schemas.push(resource.api_version());
        }
    }
    if matches!(read.verb.as_str(), "apply" | "create") {
        let versions = payload::api_versions(check.text_of("payload"));
        needs.schemas.extend(versions);
    }
    needs.schemas.dedup();
    needs.schemas.truncate(4);
    needs
}

fn patch_json(check: &Check) -> bool {
    key_value::pairs(check.text_of("metadata"))
        .iter()
        .any(|(key, value)| {
            key.eq_ignore_ascii_case("patch-type") && value.trim().eq_ignore_ascii_case("json")
        })
}

// ---------------------------------------------------------------------------------------------
// Completion
// ---------------------------------------------------------------------------------------------

/// Completion in a Kubernetes check: markers, the target, the action, metadata and options
/// keys and values, and the payload.
pub fn complete(check: &Check, pos: usize, known: &Catalog) -> Completions {
    let at = At::new(&check.text, pos);
    if at.in_placeholder() || at.in_response() {
        return at.none();
    }
    if let Some(markers) = at.markers(&SECTIONS) {
        return markers;
    }
    match at.name() {
        Some("target") => at
            .next_section(&SECTIONS)
            .unwrap_or_else(|| complete_target(&at, known)),
        Some("action") => at
            .next_section(&SECTIONS)
            .unwrap_or_else(|| complete_action(&at, known)),
        Some("metadata") => complete_keys(&at, check, known, true),
        Some("options") => complete_keys(&at, check, known, false),
        Some("payload") => {
            let action = action_line(known, check);
            if !matches!(
                action::shape(&action).verb.as_str(),
                "apply" | "create" | "patch"
            ) {
                return at.none();
            }
            let resource = known
                .resources
                .as_ref()
                .and_then(|list| action_resource(&action, list))
                .map(|(resource, _)| resource);
            payload::complete(&at, known, resource)
        }
        _ => at.none(),
    }
}

fn names(list: impl IntoIterator<Item = String>, detail: &str, kind: &str) -> Vec<Item> {
    list.into_iter()
        .map(|name| item(name, detail, kind))
        .collect()
}

fn reopening(mut found: Item) -> Item {
    found.next = true;
    found
}

fn complete_target(at: &At, known: &Catalog) -> Completions {
    let start = at.line_start(at.line);
    let lead = at.before.len() - at.before.trim_start().len();
    let typed = &at.before[lead..];
    let base = start + lead;
    let Some(head) = typed.get(..SCHEME.len()) else {
        if SCHEME.starts_with(&typed.to_ascii_lowercase()) {
            let scheme = reopening(item(SCHEME, "Kubernetes context", "url"));
            return finish(base, at.pos, typed, vec![scheme]);
        }
        return at.none();
    };
    if !head.eq_ignore_ascii_case(SCHEME) {
        return at.none();
    }
    let rest = &typed[SCHEME.len()..];
    let base = base + SCHEME.len();
    let contexts: &[String] = known.contexts.as_ref().map_or(&[], |list| list.as_slice());
    let length = contexts
        .iter()
        .filter(|name| rest.starts_with(&format!("{name}/")))
        .map(|name| name.len())
        .max()
        .or_else(|| rest.find('/').filter(|_| contexts.is_empty()));
    if let Some(length) = length {
        let list = known
            .namespaces
            .iter()
            .flat_map(|list| list.iter().cloned());
        let items = names(list, "Namespace", "class");
        return finish(base + length + 1, at.pos, &rest[length + 1..], items);
    }
    let items = contexts
        .iter()
        .map(|name| {
            let detail = if known.current.as_deref() == Some(name) {
                "Current context"
            } else {
                "Context"
            };
            reopening(item(format!("{name}/"), detail, "class"))
        })
        .collect();
    finish(base, at.pos, rest, items)
}

fn complete_action(at: &At, known: &Catalog) -> Completions {
    let start = at.line_start(at.line);
    let lead = at.before.len() - at.before.trim_start().len();
    let typed = &at.before[lead..];
    let split = typed.rfind(char::is_whitespace).map_or(0, |at| at + 1);
    let word = &typed[split..];
    let from = start + lead + split;
    let done: Vec<String> = typed[..split]
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    let items = match done.as_slice() {
        [] => VERBS
            .iter()
            .map(|(name, detail, takes)| {
                let text = if *takes {
                    format!("{name} ")
                } else {
                    (*name).into()
                };
                let mut found = item(text, detail, "keyword");
                found.next = *takes;
                found
            })
            .collect(),
        [first] if two_words(first) => VERBS
            .iter()
            .filter_map(|(name, detail, _)| {
                let second = name.strip_prefix(first.as_str())?.strip_prefix(' ')?;
                Some(reopening(item(format!("{second} "), detail, "keyword")))
            })
            .collect(),
        [first, second, ..] if two_words(first) => {
            argument(&format!("{first} {second}"), done.len() - 2, word, known)
        }
        [first, ..] => argument(first, done.len() - 1, word, known),
    };
    // After the `/` of `resource/name` the names are completed.
    let (from, query) = match word.rfind('/') {
        Some(slash) => (from + slash + 1, &word[slash + 1..]),
        None => (from, word),
    };
    finish(from, at.pos, query, items)
}

/// The items for argument `index` of `verb`; `word` is what is typed of it.
fn argument(verb: &str, index: usize, word: &str, known: &Catalog) -> Vec<Item> {
    if verb == "auth can-i" && index == 0 {
        let verbs = [
            "get", "list", "watch", "create", "update", "patch", "delete",
        ];
        return names(verbs.map(str::to_owned), "Verb", "keyword");
    }
    let named =
        resource_arg(verb).is_some_and(|at| at + 1 == index) && action::takes_spaced_name(verb);
    if named {
        let found = known
            .objects
            .iter()
            .flat_map(|found| found.names.iter().cloned());
        return names(found, "Name", "text");
    }
    if resource_arg(verb) != Some(index) {
        return vec![];
    }
    if word.contains('/') {
        let found = known
            .objects
            .iter()
            .flat_map(|found| found.names.iter().cloned());
        return names(found, "Name", "text");
    }
    if verb == "top" {
        return names(["pods".into(), "nodes".into()], "Resource", "type");
    }
    known
        .resources
        .as_ref()
        .map_or(vec![], |resources| resource_items(verb, resources))
}

fn resource_items(verb: &str, resources: &[Resource]) -> Vec<Item> {
    let fitting: Vec<&Resource> = resources.iter().filter(|r| fits(verb, r)).collect();
    let mut taken = BTreeSet::new();
    let mut items = Vec::new();
    for resource in &fitting {
        let many = fitting
            .iter()
            .filter(|other| other.plural == resource.plural)
            .count()
            > 1;
        let plural = if many {
            resource.qualified()
        } else {
            resource.plural.clone()
        };
        let detail = if resource.group.is_empty() {
            resource.kind.clone()
        } else {
            format!("{} · {}", resource.kind, resource.group)
        };
        if taken.insert(plural.clone()) {
            items.push(item(plural, &detail, "type"));
        }
        for short in &resource.short_names {
            if taken.insert(short.clone()) {
                let detail = format!("{} · short name", resource.plural);
                items.push(item(short.clone(), &detail, "type"));
            }
        }
    }
    items
}

fn complete_keys(at: &At, check: &Check, known: &Catalog, metadata: bool) -> Completions {
    let line_start = at.line_start(at.line);
    let before = at.before;
    if let Some((key, value)) = before.split_once(':') {
        let name = key.trim().to_ascii_lowercase();
        let lead = value.len() - value.trim_start().len();
        let from = line_start + key.len() + 1 + lead;
        return complete_value(at, &name, value.trim_start(), from, known, metadata);
    }
    let verb = verb_of(known, check);
    let used = key_value::used(at.other_lines());
    let keys: Vec<(&str, &str)> = if metadata {
        METADATA
            .iter()
            .filter(|key| !known_verb(&verb) || config::allowed(&verb, key))
            .map(|key| (*key, "Metadata"))
            .collect()
    } else {
        OPTIONS.iter().map(|key| (*key, "Option")).collect()
    };
    let items = key_value::items(&[], &keys, &used)
        .into_iter()
        .map(reopening)
        .collect();
    let mut result = finish(line_start, at.pos, before.trim(), items);
    if before.trim().is_empty() && !used.is_empty() {
        let mut items = finish(line_start, at.pos, "", at.unused(&SECTIONS)).items;
        items.append(&mut result.items);
        result.items = items;
    }
    result
}

fn complete_value(
    at: &At,
    key: &str,
    value: &str,
    from: usize,
    known: &Catalog,
    metadata: bool,
) -> Completions {
    let fixed = |list: &[&str], detail: &str| {
        names(list.iter().map(|value| (*value).to_owned()), detail, "enum")
    };
    let mut offset = 0;
    let items = if CONSOLE.contains(&key) {
        vec![]
    } else if metadata {
        match key {
            "container" => {
                let list = known
                    .containers
                    .iter()
                    .flat_map(|list| list.iter().cloned());
                names(list, "Container", "class")
            }
            "selector" => {
                let (at, items) = selector_items(value, known);
                offset = at;
                items
            }
            "field-selector" => {
                offset = value.rfind(',').map_or(0, |at| at + 1);
                let fields = [
                    "metadata.name=",
                    "metadata.namespace=",
                    "status.phase=",
                    "spec.nodeName=",
                    "spec.restartPolicy=",
                ];
                fixed(&fields, "Field").into_iter().map(reopening).collect()
            }
            "patch-type" => fixed(&["merge", "json", "strategic", "apply"], "Patch type"),
            "dry-run" => fixed(&["server"], "Validate on the server"),
            "previous" | "timestamps" | "follow" | "force" => fixed(&["true", "false"], "Flag"),
            "since" => fixed(&["5m", "1h", "24h"], "Duration"),
            "tail" => fixed(&["100", "1000"], "Lines"),
            _ => vec![],
        }
    } else {
        match key {
            "format" => fixed(&["raw", "table", "json", "yaml"], "Format"),
            "logs" => fixed(&["raw", "json"], "Logs"),
            _ => vec![],
        }
    };
    finish(from + offset, at.pos, &value[offset..], items)
}

/// The offset of the label selector term at the end of `value`, and what it offers.
fn selector_items(value: &str, known: &Catalog) -> (usize, Vec<Item>) {
    let offset = value.rfind(',').map_or(0, |at| at + 1);
    let term = &value[offset..];
    let Some(objects) = &known.objects else {
        return (offset, vec![]);
    };
    if let Some(at) = term.find('=') {
        let key = term[..at].trim_end_matches('!').trim();
        let values = objects.labels.get(key).into_iter().flatten().cloned();
        return (offset + at + 1, names(values, "Label value", "enum"));
    }
    let keys = objects
        .labels
        .keys()
        .map(|key| reopening(item(format!("{key}="), "Label", "property")));
    (offset, keys.collect())
}

// ---------------------------------------------------------------------------------------------
// Highlighting
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Area {
    Prelude,
    Target,
    Action,
    Metadata,
    Options,
    Payload,
    Other,
    Response,
}

impl Area {
    const ALL: [Area; 8] = [
        Area::Prelude,
        Area::Target,
        Area::Action,
        Area::Metadata,
        Area::Options,
        Area::Payload,
        Area::Other,
        Area::Response,
    ];
}

const UNKNOWN: u8 = 0;
const YAML: u8 = 1;
const JSON: u8 = 2;

fn token(start: usize, end: usize, kind: &str) -> Option<Token> {
    (end > start).then(|| Token {
        start: start as u32,
        len: (end - start) as u32,
        kind: kind.into(),
    })
}

fn target_tokens(line: &str) -> Vec<Token> {
    let Some(place) = place(line, None) else {
        return vec![];
    };
    let lead = line.len() - line.trim_start().len();
    let mut tokens = vec![
        token(lead, lead + SCHEME.len(), "keyword"),
        token(place.context_range.0, place.context_range.1, "def"),
    ];
    if let Some((start, end)) = place.namespace_range {
        tokens.push(token(start, end, "variable-2"));
    }
    tokens.into_iter().flatten().collect()
}

fn action_tokens(line: &str) -> Vec<Token> {
    let all = words(line);
    let read = action::shape(line);
    let verb_words = read.verb_words.min(all.len());
    let resource = read.resource.map(|(start, _)| start);
    let mut tokens = Vec::new();
    for (at, (start, word)) in all.iter().enumerate() {
        let end = start + word.len();
        if at < verb_words {
            tokens.extend(token(*start, end, "keyword"));
        } else if Some(*start) == resource {
            match word.find('/') {
                Some(slash) => {
                    tokens.extend(token(*start, start + slash, "type"));
                    tokens.extend(token(start + slash + 1, end, "string"));
                }
                None => tokens.extend(token(*start, end, "type")),
            }
        } else {
            tokens.extend(token(*start, end, "string"));
        }
    }
    tokens
}

/// Highlighting of a Kubernetes check: target, action, metadata, options, payload and response.
/// The state is `[area | shape << 4, yaml block, json state…]`.
pub fn highlight(state: &[u8], lines: &[String], columns: &[LineColumns]) -> Vec<Line> {
    let _ = columns;
    let (mut area, mut shape, rest) = match state.split_first() {
        Some((byte, rest)) => (
            Area::ALL
                .get((*byte & 0x0f) as usize)
                .copied()
                .unwrap_or(Area::Other),
            *byte >> 4,
            rest,
        ),
        None => (Area::Prelude, UNKNOWN, &[][..]),
    };
    let mut block = rest
        .first()
        .map_or(yaml::State::default(), |byte| yaml::State::decode(*byte));
    let mut payload = json_lang::State::decode(rest.get(1..).unwrap_or(&[]));
    lines
        .iter()
        .map(|line| {
            let named = marker(line).map(|(name, _)| name);
            let is_marker = named.as_deref().is_some_and(|name| {
                area != Area::Response || matches!(name, "responce" | "response")
            });
            let tokens = if is_marker {
                area = match named.as_deref() {
                    Some("target") => Area::Target,
                    Some("action") => Area::Action,
                    Some("metadata") => Area::Metadata,
                    Some("options") => Area::Options,
                    Some("payload") => Area::Payload,
                    Some("responce" | "response") => Area::Response,
                    _ => Area::Other,
                };
                shape = UNKNOWN;
                block = yaml::State::default();
                payload = json_lang::State::default();
                token(0, line.len(), "comment").into_iter().collect()
            } else {
                match area {
                    Area::Target => target_tokens(line),
                    Area::Action => action_tokens(line),
                    Area::Metadata | Area::Options => key_value::tokens(line),
                    Area::Payload => {
                        let trimmed = line.trim_end();
                        if trimmed == "---" || trimmed.starts_with("--- ") {
                            shape = UNKNOWN;
                            payload = json_lang::State::default();
                        } else if shape == UNKNOWN {
                            let text = line.trim();
                            if text.starts_with(['{', '[']) {
                                shape = JSON;
                            } else if !text.is_empty() && !text.starts_with('#') {
                                shape = YAML;
                            }
                        }
                        if shape == JSON {
                            json_lang::tokens(line, &mut payload)
                        } else {
                            yaml::tokens(line, &mut block)
                        }
                    }
                    Area::Prelude | Area::Response => {
                        if shape == UNKNOWN && !line.trim().is_empty() {
                            let json = line.trim_start().starts_with(['{', '[', '"']);
                            shape = if json { JSON } else { YAML };
                        }
                        if shape == JSON && !line.starts_with("ERROR ") {
                            json_lang::tokens(line, &mut payload)
                        } else {
                            vec![]
                        }
                    }
                    Area::Other => vec![],
                }
            };
            let mut state = vec![area as u8 | shape << 4, block.encode()];
            payload.encode(&mut state);
            Line { tokens, state }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Marks
// ---------------------------------------------------------------------------------------------

fn error(start: usize, end: usize) -> Option<Token> {
    token(start, end, "error")
}

fn response(name: &str) -> bool {
    matches!(name, "responce" | "response")
}

/// Tokens that the discovered catalog adds: unknown contexts, namespaces, resources and names,
/// keys the action does not take, containers that are not there, a payload namespace other than
/// the target's and, for `apply` and `create`, fields the schema does not have.
pub fn marks(check: &Check, known: &Catalog) -> Vec<Line> {
    let lines: Vec<&str> = check.text.split('\n').collect();
    let mut out: Vec<Vec<Token>> = vec![Vec::new(); lines.len()];
    let verb = verb_of(known, check);
    let mut area = String::new();
    let mut payload: Option<(usize, usize)> = None;
    let (mut target_done, mut action_done) = (false, false);
    for (index, line) in lines.iter().enumerate() {
        if let Some((name, _)) = marker(line)
            && (!response(&area) || response(&name))
        {
            if let Some((_, end)) = payload.as_mut()
                && *end == usize::MAX
            {
                *end = index;
            }
            if name == "payload" {
                payload = Some((index + 1, usize::MAX));
            }
            area = name;
            continue;
        }
        let tokens = &mut out[index];
        match area.as_str() {
            "target" if !target_done && !line.trim().is_empty() => {
                target_done = true;
                if !line.contains("{{") {
                    tokens.extend(target_marks(line, known));
                }
            }
            "action" if !action_done && !line.trim().is_empty() => {
                action_done = true;
                if !line.contains("{{") {
                    tokens.extend(action_marks(line, known));
                }
            }
            "metadata" if known_verb(&verb) => tokens.extend(metadata_marks(line, &verb, known)),
            _ => {}
        }
    }
    if let Some((first, last)) = payload
        && matches!(verb.as_str(), "apply" | "create" | "patch")
    {
        let last = last.min(lines.len());
        let body = lines[first..last].join("\n");
        let place = known.place();
        let namespace = place.as_ref().and_then(|place| place.namespace.as_deref());
        for mark in payload::marks(&body, known, namespace, verb != "patch") {
            out[first + mark.line].extend(error(mark.start, mark.end));
        }
    }
    out.into_iter()
        .map(|tokens| Line {
            tokens,
            state: Vec::new(),
        })
        .collect()
}

fn target_marks(line: &str, known: &Catalog) -> Vec<Token> {
    let contexts = known.contexts.as_ref().map(|list| list.as_slice());
    let Some(target) = place(line, contexts) else {
        return vec![];
    };
    if let Some(list) = &known.contexts
        && !list.contains(&target.context)
    {
        return error(target.context_range.0, target.context_range.1)
            .into_iter()
            .collect();
    }
    match (&known.namespaces, &target.namespace, target.namespace_range) {
        (Some(list), Some(name), Some((start, end))) if !list.contains(name) => {
            error(start, end).into_iter().collect()
        }
        _ => vec![],
    }
}

fn action_marks(line: &str, known: &Catalog) -> Vec<Token> {
    let all = words(line);
    let read = action::shape(line);
    let Some((start, first)) = all.first() else {
        return vec![];
    };
    let lone = two_words(&first.to_ascii_lowercase()) && all.len() == 1;
    if !known_verb(&read.verb) && !lone {
        return error(*start, start + first.len()).into_iter().collect();
    }
    let (Some(resources), Some((at, resource))) = (&known.resources, read.resource) else {
        return vec![];
    };
    if resources::resolve(resources, resource).is_none() {
        return error(at, at + resource.len()).into_iter().collect();
    }
    if let Some((from, object)) = read.name
        && let Some(found) = &known.objects
        && found.complete
        && !found.names.iter().any(|known| known == object)
    {
        return error(from, from + object.len()).into_iter().collect();
    }
    vec![]
}

fn metadata_marks(line: &str, verb: &str, known: &Catalog) -> Vec<Token> {
    let Some((key, value)) = line.split_once(':') else {
        return vec![];
    };
    let name = key.trim().to_ascii_lowercase();
    if CONSOLE.contains(&name.as_str()) {
        return vec![];
    }
    if !config::allowed(verb, &name) {
        let start = key.len() - key.trim_start().len();
        return error(start, key.trim_end().len()).into_iter().collect();
    }
    let value_text = value.trim();
    if name == "container"
        && let Some(list) = &known.containers
        && !list.is_empty()
        && !value_text.is_empty()
        && !value_text.contains("{{")
        && !list.iter().any(|container| container == value_text)
    {
        let from = key.len() + 1 + (value.len() - value.trim_start().len());
        return error(from, from + value_text.len()).into_iter().collect();
    }
    vec![]
}

// ---------------------------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------------------------

/// The discovered schema for the Agent; `None` before a successful discovery.
pub fn schema(check: &Check, known: &Catalog) -> Option<Value> {
    let _ = check;
    if known.contexts.is_none() && known.resources.is_none() {
        return None;
    }
    let place = known.place();
    let resources: Vec<Value> = known
        .resources
        .iter()
        .flat_map(|list| list.iter())
        .map(|resource| {
            json!({
                "name": resource.plural,
                "kind": resource.kind,
                "apiVersion": resource.api_version(),
                "namespaced": resource.namespaced,
                "shortNames": resource.short_names,
                "verbs": resource.verbs,
            })
        })
        .collect();
    let list = |names: &Option<std::rc::Rc<Vec<String>>>| {
        names.as_ref().map(|list| list.to_vec()).unwrap_or_default()
    };
    Some(json!({
        "context": place.as_ref().map(|place| place.context.clone()),
        "namespace": place.as_ref().and_then(|place| place.namespace.clone()),
        "contexts": list(&known.contexts),
        "namespaces": list(&known.namespaces),
        "resources": resources,
    }))
}
