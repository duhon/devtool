//! Discovery: contexts, namespaces, API resources, object names and labels, containers and the
//! OpenAPI v3 documents, read through the `kubernetes` driver and cached per context. Owned by
//! the discovery and editor part; follows `services_sdk::lang::rest` discovery.
//!
//! What a check needs is the editor's to say ([`editor::needs`]) from its text and the cursor;
//! discovery fetches what is missing and asks again after every answer, so a resource found
//! leads to its object names. A key is tried once per context: a failed call is not repeated
//! until `Event::Discover` drops the cache of the check's context (and the contexts). Object
//! lists, container lists and documents are bounded per context, the least recently fetched
//! first out. A `{{…}}` variable of `#target` or `#action` is resolved through the App first.
//! Failures never break editing: they go to the log and, for hints, to the status.
//!
//! Reply tags: `discover:<n>` for its driver calls and `resolve:<n>` for variables (see the
//! crate documentation).

pub mod openapi;

use crate::editor::{self, Needs};
use crate::resources::{self, Parsed, Resource};
use crate::{DISCOVER, RESOLVE};
use openapi::Document;
use serde_json::Value;
use services_sdk::kubernetes::{Request, Response};
use services_sdk::lang::resolve::Resolver;
use services_sdk::{Check, Cmd, Failure, cmd};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

/// The most contexts remembered, and per context the most object lists, container lists and
/// OpenAPI documents, and tried keys.
const CLUSTERS: usize = 8;
const OBJECTS: usize = 32;
const CONTAINERS: usize = 32;
const DOCUMENTS: usize = 6;
const TRIED: usize = 256;
/// Objects read per list: names and labels only, the first page.
const PAGE: &str = "500";
const TIMEOUT_MS: u64 = 15_000;
/// Metadata-only lists; a server that does not know the form answers the full list.
const METADATA: &str =
    "application/json;as=PartialObjectMetadataList;g=meta.k8s.io;v=v1,application/json";
/// The most label keys, and values per key, kept of a list.
const LABEL_KEYS: usize = 200;
const LABEL_VALUES: usize = 50;

/// The objects of one list: their names and the labels they carry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Objects {
    pub names: Vec<String>,
    pub labels: BTreeMap<String, BTreeSet<String>>,
    /// The list holds every object, not just its first page.
    pub complete: bool,
}

impl Objects {
    fn parse(body: &Value) -> Self {
        let items = body["items"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let mut found = Self {
            complete: body["metadata"]["continue"]
                .as_str()
                .is_none_or(str::is_empty),
            ..Self::default()
        };
        for item in items {
            let Some(name) = item["metadata"]["name"].as_str() else {
                continue;
            };
            found.names.push(name.to_owned());
            for (key, value) in item["metadata"]["labels"].as_object().into_iter().flatten() {
                let Some(value) = value.as_str() else {
                    continue;
                };
                if found.labels.len() >= LABEL_KEYS && !found.labels.contains_key(key) {
                    continue;
                }
                let values = found.labels.entry(key.clone()).or_default();
                if values.len() < LABEL_VALUES {
                    values.insert(value.to_owned());
                }
            }
        }
        found.names.sort();
        found
    }
}

/// What discovery found, as the editor reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    /// The kubeconfig contexts, once read.
    pub contexts: Option<Rc<Vec<String>>>,
    /// The kubeconfig's current context.
    pub current: Option<String>,
    /// The first line of `#target` and the text of `#action`, `{{…}}` variables resolved;
    /// `None` while a variable is not.
    pub target: Option<String>,
    pub action: Option<String>,
    pub namespaces: Option<Rc<Vec<String>>>,
    pub resources: Option<Rc<Vec<Resource>>>,
    /// The objects of the action's resource in the target namespace.
    pub objects: Option<Rc<Objects>>,
    /// The containers of the action's object.
    pub containers: Option<Rc<Vec<String>>>,
    /// OpenAPI documents by group version (`v1`, `apps/v1`).
    pub documents: Vec<(String, Rc<Document>)>,
}

impl Catalog {
    /// The target's place, read with the contexts known.
    pub fn place(&self) -> Option<editor::Place> {
        let contexts = self.contexts.as_ref().map(|list| list.as_slice());
        editor::place(self.target.as_deref()?, contexts)
    }

    /// The OpenAPI document of `api_version`.
    pub fn document(&self, api_version: &str) -> Option<&Document> {
        self.documents
            .iter()
            .find(|(known, _)| known == api_version)
            .map(|(_, document)| document.as_ref())
    }
}

/// What a request was for.
#[derive(Debug)]
enum Call {
    Contexts,
    Namespaces(String),
    Api {
        context: String,
        group: Group,
    },
    Objects {
        context: String,
        path: String,
    },
    Containers {
        context: String,
        path: String,
    },
    Index(String),
    Document {
        context: String,
        api_version: String,
    },
}

impl Call {
    fn label(&self) -> String {
        match self {
            Call::Contexts => "contexts".into(),
            Call::Namespaces(context) => format!("namespaces of {context}"),
            Call::Api { context, .. } => format!("API resources of {context}"),
            Call::Objects { path, .. } | Call::Containers { path, .. } => path.clone(),
            Call::Index(context) => format!("OpenAPI index of {context}"),
            Call::Document { api_version, .. } => format!("OpenAPI document of {api_version}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Core,
    Named,
}

/// The API resource answers of a context while they arrive: `/api` and `/apis`, and for a
/// server without aggregated discovery the list of each version.
#[derive(Debug, Default)]
struct Loading {
    core: Part,
    named: Part,
}

#[derive(Debug)]
struct Part {
    waiting: usize,
    found: Vec<Resource>,
}

impl Default for Part {
    fn default() -> Self {
        Self {
            waiting: 1,
            found: vec![],
        }
    }
}

#[derive(Default)]
struct Cluster {
    namespaces: Option<Rc<Vec<String>>>,
    resources: Option<Rc<Vec<Resource>>>,
    loading: Option<Loading>,
    /// By collection path.
    objects: Vec<(String, Rc<Objects>)>,
    /// By object path.
    containers: Vec<(String, Rc<Vec<String>>)>,
    /// The `paths` of the OpenAPI index.
    index: Option<Rc<Value>>,
    documents: Vec<(String, Rc<Document>)>,
    tried: BTreeSet<String>,
}

fn remember<T>(list: &mut Vec<(String, T)>, key: String, value: T, bound: usize) {
    list.retain(|(known, _)| *known != key);
    list.push((key, value));
    if list.len() > bound {
        list.remove(0);
    }
}

/// How a round of discovery was asked for.
#[derive(Clone, Copy, Debug, Default)]
struct Mode {
    /// Hints or an explicit discovery: its outcome is shown in the status.
    loud: bool,
    /// Drop the context's cache first.
    reset: bool,
}

#[derive(Default)]
struct Batch {
    loud: bool,
    failed: Vec<String>,
}

#[derive(Default)]
pub struct Discovery {
    resolver: Resolver<Mode>,
    contexts: Option<Rc<Vec<String>>>,
    current: Option<String>,
    contexts_tried: bool,
    /// Most recently used last.
    clusters: Vec<(String, Cluster)>,
    pending: BTreeMap<u64, Call>,
    calls: u64,
    batch: Option<Batch>,
    /// The check and cursor the latest request was for: asked again after every answer.
    last: Option<(Check, Option<usize>)>,
}

impl Discovery {
    /// Hints at `pos`, or for the whole document when `pos` is `None`: fetches what completion
    /// there needs.
    pub fn hints(&mut self, check: &Check, pos: Option<usize>) -> Vec<Cmd> {
        let mode = Mode {
            loud: pos.is_some(),
            reset: false,
        };
        self.start(check, pos, mode)
    }

    /// The user asked to discover again: forget the cache and fetch.
    pub fn discover(&mut self, check: &Check) -> Vec<Cmd> {
        self.contexts = None;
        self.contexts_tried = false;
        let mode = Mode {
            loud: true,
            reset: true,
        };
        self.start(check, None, mode)
    }

    /// A session was restored: drop the cache.
    pub fn reset(&mut self) -> Vec<Cmd> {
        self.contexts = None;
        self.contexts_tried = false;
        self.clusters.clear();
        self.last = None;
        vec![cmd::relight()]
    }

    fn start(&mut self, check: &Check, pos: Option<usize>, mode: Mode) -> Vec<Cmd> {
        self.last = Some((check.clone(), pos));
        let mut cmds = Vec::new();
        for (name, text) in [
            ("target", check.text_of("target")),
            ("action", check.text_of("action")),
        ] {
            let raw = text.trim();
            if raw.contains("{{") && self.resolver.get(raw).is_none() && !self.resolver.pending(raw)
            {
                let purpose = Mode {
                    reset: mode.reset && name == "target",
                    ..mode
                };
                cmds.push(self.resolver.request(raw, purpose));
            }
        }
        cmds.extend(self.replan(mode));
        cmds
    }

    /// Fetches what the last check needs and is not known yet.
    fn replan(&mut self, mut mode: Mode) -> Vec<Cmd> {
        let Some((check, pos)) = self.last.clone() else {
            return vec![];
        };
        if mode.reset
            && let Some(place) = self.catalog(&check).place()
        {
            self.clusters.retain(|(known, _)| *known != place.context);
            mode.reset = false;
        }
        let needs = editor::needs(&check, pos, &self.catalog(&check));
        self.issue(&needs, mode.loud)
    }

    fn cluster(&mut self, context: &str) -> &mut Cluster {
        match self.clusters.iter().position(|(known, _)| known == context) {
            Some(at) => {
                let entry = self.clusters.remove(at);
                self.clusters.push(entry);
            }
            None => {
                self.clusters.push((context.to_owned(), Cluster::default()));
                if self.clusters.len() > CLUSTERS {
                    self.clusters.remove(0);
                }
            }
        }
        &mut self.clusters.last_mut().expect("a cluster").1
    }

    /// Marks `key` tried in `context`; whether it was not before.
    fn want(&mut self, context: &str, key: String) -> bool {
        let cluster = self.cluster(context);
        if cluster.tried.len() >= TRIED {
            cluster.tried.clear();
        }
        cluster.tried.insert(key)
    }

    fn issue(&mut self, needs: &Needs, loud: bool) -> Vec<Cmd> {
        let mut calls: Vec<(Call, Request)> = Vec::new();
        if !self.contexts_tried {
            self.contexts_tried = true;
            calls.push((Call::Contexts, Request::contexts()));
        }
        if let Some(context) = needs.context.as_deref() {
            if needs.namespaces && self.want(context, "namespaces".into()) {
                let mut request = Request::get(context, "/api/v1/namespaces");
                request.query.insert("limit".into(), PAGE.into());
                request.accept = Some(METADATA.into());
                calls.push((Call::Namespaces(context.into()), request));
            }
            if needs.resources && self.want(context, "resources".into()) {
                self.cluster(context).loading = Some(Loading::default());
                for (at, request) in resources::requests(context).into_iter().enumerate() {
                    let group = if at == 0 { Group::Core } else { Group::Named };
                    let context = context.into();
                    calls.push((Call::Api { context, group }, request));
                }
            }
            if let Some((resource, namespace)) = &needs.objects {
                let path = resource.collection_path(namespace);
                if self.want(context, format!("objects:{path}")) {
                    let mut request = Request::get(context, &path);
                    request.query.insert("limit".into(), PAGE.into());
                    request.accept = Some(METADATA.into());
                    let context = context.into();
                    calls.push((Call::Objects { context, path }, request));
                }
            }
            if let Some((resource, namespace, name)) = &needs.containers {
                let path = resource.object_path(namespace, name);
                if self.want(context, format!("containers:{path}")) {
                    let request = Request::get(context, &path);
                    let context = context.into();
                    calls.push((Call::Containers { context, path }, request));
                }
            }
            for api_version in &needs.schemas {
                let cluster = self.cluster(context);
                if cluster
                    .documents
                    .iter()
                    .any(|(known, _)| known == api_version)
                {
                    continue;
                }
                let Some(index) = cluster.index.clone() else {
                    if self.want(context, "index".into()) {
                        let request = Request::get(context, openapi::INDEX);
                        calls.push((Call::Index(context.into()), request));
                    }
                    continue;
                };
                if !self.want(context, format!("document:{api_version}")) {
                    continue;
                }
                if let Some((path, query)) = openapi::locate(&index, api_version) {
                    let mut request = Request::get(context, &path);
                    request.query = query;
                    let call = Call::Document {
                        context: context.into(),
                        api_version: api_version.clone(),
                    };
                    calls.push((call, request));
                }
            }
        }
        self.send(calls, loud)
    }

    fn send(&mut self, calls: Vec<(Call, Request)>, loud: bool) -> Vec<Cmd> {
        if calls.is_empty() {
            return vec![];
        }
        let mut cmds = Vec::new();
        let batch = self.batch.get_or_insert_with(Batch::default);
        if loud && !batch.loud {
            batch.loud = true;
            cmds.push(cmd::status("Discovery…", "busy"));
        }
        for (call, mut request) in calls {
            self.calls += 1;
            request.timeout_ms = Some(TIMEOUT_MS);
            let tag = format!("{DISCOVER}{}", self.calls);
            cmds.push(cmd::driver(tag, &request.call()));
            self.pending.insert(self.calls, call);
        }
        cmds
    }

    /// A reply of discovery: a driver call or a variable.
    pub fn reply(&mut self, tag: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        if tag.starts_with(RESOLVE) {
            return self.resolved(tag, result);
        }
        let Some(call) = tag
            .strip_prefix(DISCOVER)
            .and_then(|n| n.parse::<u64>().ok())
            .and_then(|n| self.pending.remove(&n))
        else {
            return vec![];
        };
        let label = call.label();
        let api = match &call {
            Call::Api { context, group } => Some((context.clone(), *group)),
            _ => None,
        };
        let mut cmds = Vec::new();
        let mut follow = Vec::new();
        let outcome = match answer(result) {
            Ok(response) => self.learn(call, response),
            Err(message) => {
                if let Some((context, group)) = api {
                    let _ = self.learn_api(&context, group, Err(message.clone()));
                }
                Err(message)
            }
        };
        match outcome {
            Ok(next) => {
                follow = next;
                cmds.push(cmd::relight());
            }
            Err(message) => {
                if let Some(batch) = &mut self.batch {
                    batch.failed.push(format!("{label}: {message}"));
                }
            }
        }
        let loud = self.batch.as_ref().is_some_and(|batch| batch.loud);
        cmds.extend(self.send(follow, loud));
        cmds.extend(self.replan(Mode { loud, reset: false }));
        if self.pending.is_empty()
            && let Some(batch) = self.batch.take()
        {
            cmds.extend(finished(&batch));
        }
        cmds
    }

    fn resolved(&mut self, tag: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        let Some(done) = self.resolver.reply(tag, result) else {
            return vec![];
        };
        match done.result {
            Ok(_) => {
                let mut cmds = self.replan(done.purpose);
                cmds.push(cmd::relight());
                cmds
            }
            Err(message) => {
                let text = format!("Variables of {} could not be resolved: {message}", done.raw);
                let mut cmds = vec![cmd::log("warning", &text)];
                if done.purpose.loud {
                    cmds.push(cmd::status(&format!("Discovery · ERROR · {text}"), "error"));
                }
                cmds
            }
        }
    }

    /// Takes an answer into the cache; the calls it leads to.
    fn learn(&mut self, call: Call, response: Response) -> Result<Vec<(Call, Request)>, String> {
        match call {
            Call::Contexts => {
                let names = response.contexts.into_iter().map(|c| c.name).collect();
                self.contexts = Some(Rc::new(names));
                self.current = response.current;
            }
            Call::Namespaces(context) => {
                let found = Objects::parse(&response.body).names;
                self.cluster(&context).namespaces = Some(Rc::new(found));
            }
            Call::Api { context, group } => {
                let body = &response.body;
                let parsed = if body["kind"] == "APIResourceList" {
                    Ok(Parsed::Resources(resources::parse_list(body)))
                } else {
                    resources::parse(body)
                };
                return self.learn_api(&context, group, parsed);
            }
            Call::Objects { context, path } => {
                let found = Rc::new(Objects::parse(&response.body));
                remember(&mut self.cluster(&context).objects, path, found, OBJECTS);
            }
            Call::Containers { context, path } => {
                let found = Rc::new(containers(&response.body));
                let list = &mut self.cluster(&context).containers;
                remember(list, path, found, CONTAINERS);
            }
            Call::Index(context) => {
                if !response.body["paths"].is_object() {
                    return Err("No OpenAPI v3 paths".into());
                }
                let paths = serde_json::json!({"paths": response.body["paths"]});
                self.cluster(&context).index = Some(Rc::new(paths));
            }
            Call::Document {
                context,
                api_version,
            } => {
                let document = Document::parse(response.body).ok_or("No OpenAPI v3 schemas")?;
                let list = &mut self.cluster(&context).documents;
                remember(list, api_version, Rc::new(document), DOCUMENTS);
            }
        }
        Ok(vec![])
    }

    /// An answer of the API resource discovery; for a server without aggregated discovery the
    /// calls for the version lists.
    fn learn_api(
        &mut self,
        context: &str,
        group: Group,
        parsed: Result<Parsed, String>,
    ) -> Result<Vec<(Call, Request)>, String> {
        let cluster = self.cluster(context);
        let Some(loading) = cluster.loading.as_mut() else {
            return Ok(vec![]);
        };
        let part = match group {
            Group::Core => &mut loading.core,
            Group::Named => &mut loading.named,
        };
        part.waiting -= 1;
        let mut next = Vec::new();
        let mut failure = None;
        match parsed {
            Ok(Parsed::Resources(found)) => part.found.extend(found),
            Ok(Parsed::Legacy(paths)) => {
                part.waiting += paths.len();
                for path in paths {
                    let call = Call::Api {
                        context: context.into(),
                        group,
                    };
                    next.push((call, Request::get(context, &path)));
                }
            }
            Err(message) => failure = Some(message),
        }
        if loading.core.waiting == 0
            && loading.named.waiting == 0
            && let Some(done) = cluster.loading.take()
        {
            let all: Vec<Resource> = done
                .core
                .found
                .into_iter()
                .chain(done.named.found)
                .collect();
            if !all.is_empty() {
                cluster.resources = Some(Rc::new(all));
            }
        }
        failure.map_or(Ok(next), Err)
    }
}

fn finished(batch: &Batch) -> Vec<Cmd> {
    let mut cmds = Vec::new();
    if let Some(first) = batch.failed.first() {
        cmds.push(cmd::log("warning", &batch.failed.join("\n")));
        if batch.loud {
            let text = format!("Discovery · WARNING · {first}");
            cmds.push(cmd::status(&text, "warning"));
        }
    } else if batch.loud {
        cmds.push(cmd::status("Discovery · OK", "ok"));
    }
    cmds
}

/// The driver's answer: its body, or why there is none. A server error is a failed call.
fn answer(result: Result<Value, Failure>) -> Result<Response, String> {
    let value = result.map_err(|failure| failure.message)?;
    let response = Response::parse(&value)?;
    if response.status >= 400 || response.error.is_some() {
        let message = response
            .error
            .map(|error| error.message)
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| format!("HTTP {}", response.status));
        return Err(message);
    }
    Ok(response)
}

/// The container names of a pod, or of the pod template of a workload.
fn containers(body: &Value) -> Vec<String> {
    let spec = if body["kind"] == "Pod" {
        &body["spec"]
    } else if body["kind"] == "CronJob" {
        &body["spec"]["jobTemplate"]["spec"]["template"]["spec"]
    } else {
        &body["spec"]["template"]["spec"]
    };
    ["containers", "initContainers", "ephemeralContainers"]
        .into_iter()
        .flat_map(|key| spec[key].as_array().into_iter().flatten())
        .filter_map(|container| container["name"].as_str().map(str::to_owned))
        .collect()
}

impl Discovery {
    /// The catalog for the check's target; empty before discovery.
    pub fn catalog(&self, check: &Check) -> Catalog {
        let resolved = |text: &str| self.resolver.get(text.trim()).map(str::to_owned);
        let mut catalog = Catalog {
            contexts: self.contexts.clone(),
            current: self.current.clone(),
            target: resolved(check.text_of("target")),
            action: resolved(check.text_of("action")),
            ..Catalog::default()
        };
        let Some(place) = catalog.place() else {
            return catalog;
        };
        let Some((_, cluster)) = self
            .clusters
            .iter()
            .find(|(name, _)| *name == place.context)
        else {
            return catalog;
        };
        catalog.namespaces = cluster.namespaces.clone();
        catalog.resources = cluster.resources.clone();
        catalog.documents = cluster.documents.clone();
        let (Some(resources), Some(action)) = (&cluster.resources, &catalog.action) else {
            return catalog;
        };
        if let Some((resource, name)) = editor::action_resource(action, resources) {
            let namespace = place.namespace.unwrap_or_default();
            let path = resource.collection_path(&namespace);
            catalog.objects = cluster
                .objects
                .iter()
                .find(|(known, _)| *known == path)
                .map(|(_, found)| found.clone());
            if let Some(name) = name {
                let path = resource.object_path(&namespace, &name);
                catalog.containers = cluster
                    .containers
                    .iter()
                    .find(|(known, _)| *known == path)
                    .map(|(_, found)| found.clone());
            }
        }
        catalog
    }
}
