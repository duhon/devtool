//! What each action does against the API: the driver calls of its batches and how their answers
//! become a [`Content`]. A [`Flow`] is one run's state machine: [`Flow::new`] gives the first
//! [`Step`], [`Flow::next`] the step after a batch of answers.

use super::Action;
use crate::config::{Config, Format};
use crate::render::{self, Content};
use crate::resources::{self, Resource};
use crate::target::Target;
use serde_json::Value;
use services_sdk::kubernetes::{Event, Request, Response};
use std::collections::BTreeMap;
use std::rc::Rc;

/// `Accept` of a server-side printed table.
pub const TABLE: &str = "application/json;as=Table;v=v1;g=meta.k8s.io,application/json;as=Table;v=v1beta1;g=meta.k8s.io,application/json";
pub(super) const FIELD_MANAGER: &str = "services-devtools";
pub(super) const REVISION: &str = "deployment.kubernetes.io/revision";
pub(super) const CHANGE_CAUSE: &str = "kubernetes.io/change-cause";
pub(super) const RESTARTED_AT: &str = "kubectl.kubernetes.io/restartedAt";
/// Pod containers one `logs` run reads.
pub(super) const MAX_LOG_CALLS: usize = 50;
pub(super) const WORKLOADS: [&str; 7] = [
    "Deployment",
    "StatefulSet",
    "DaemonSet",
    "ReplicaSet",
    "ReplicationController",
    "Job",
    "Service",
];

/// What the answer of one request is: its [`Response`], and the events of a streamed one.
#[derive(Clone, Debug, Default)]
pub struct Answer {
    pub response: Response,
    pub events: Vec<Event>,
}

/// The end of a run.
#[derive(Clone, Debug)]
pub struct Done {
    pub content: Content,
    /// `OK`, `WARNING` or `ERROR`.
    pub outcome: &'static str,
}

pub enum Step {
    /// Requests to send together; the next step gets their answers in this order.
    Calls(Vec<Request>),
    Done(Done),
}

pub(super) fn done(content: Content) -> Step {
    Step::Done(Done {
        content,
        outcome: "OK",
    })
}

/// The server's `Status` of a failed answer as kubectl prints it.
pub fn status_error(response: &Response) -> Option<String> {
    if let Some(error) = &response.error {
        let reason = if error.reason.is_empty() {
            error.code.to_string()
        } else {
            error.reason.clone()
        };
        return Some(format!("Error from server ({reason}): {}", error.message));
    }
    (response.status >= 400).then(|| {
        let text = match &response.body {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        format!("Error from server ({}): {text}", response.status)
    })
}

pub(super) fn checked(answer: &Answer) -> Result<&Value, String> {
    match status_error(&answer.response) {
        Some(error) => Err(error),
        None => Ok(&answer.response.body),
    }
}

pub(super) fn items(body: &Value) -> Vec<Value> {
    body["items"].as_array().cloned().unwrap_or_default()
}

pub(super) fn text_of(answer: &Answer) -> String {
    match &answer.response.body {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub struct Flow {
    pub(super) target: Target,
    pub(super) action: Action,
    pub(super) config: Config,
    pub(super) catalog: Rc<Vec<Resource>>,
    pub(super) now: f64,
    pub(super) payload: Vec<Value>,
    pub(super) stage: u8,
    pub(super) resource: Option<Resource>,
    /// Pod containers of a `logs` run, in the order of their requests.
    pub(super) targets: Vec<(String, String)>,
    /// The columns of a `watch` table.
    pub(super) header: Option<Vec<String>>,
}

impl Flow {
    pub fn new(
        target: &Target,
        action: &Action,
        config: &Config,
        payload: &str,
        catalog: Rc<Vec<Resource>>,
        now: f64,
    ) -> Result<(Self, Step), String> {
        let verb = action.verb.as_str();
        let payload = match verb {
            "apply" | "create" => {
                let objects = crate::payload::objects(payload, &target.namespace)?;
                if objects.is_empty() {
                    return Err(format!("{verb} needs the objects in #payload"));
                }
                objects
            }
            "patch" => {
                let mut documents = crate::payload::parse(payload)?;
                if documents.len() != 1 {
                    return Err("patch needs one patch document in #payload".into());
                }
                vec![documents.remove(0)]
            }
            _ if !payload.trim().is_empty() => {
                return Err("#payload is only used by apply, create and patch".into());
            }
            _ => vec![],
        };
        let mut flow = Self {
            target: target.clone(),
            action: action.clone(),
            config: config.clone(),
            catalog,
            now,
            payload,
            stage: 0,
            resource: None,
            targets: Vec::new(),
            header: None,
        };
        let step = flow.start()?;
        Ok((flow, step))
    }

    pub(super) fn namespace(&self) -> &str {
        &self.target.namespace
    }

    pub(super) fn request(&self, method: &str, path: String) -> Request {
        Request::new(&self.target.context, method, &path)
    }

    pub(super) fn structured(&self) -> bool {
        matches!(self.config.options.format, Format::Json | Format::Yaml)
    }

    pub(super) fn resource(&self) -> Result<Resource, String> {
        let name = &self.action.resource;
        resources::resolve(&self.catalog, name)
            .cloned()
            .ok_or_else(|| format!("the server doesn't have a resource type {name:?}"))
    }

    pub(super) fn core(&self, plural: &str) -> Result<Resource, String> {
        self.catalog
            .iter()
            .find(|resource| resource.group.is_empty() && resource.plural == plural)
            .cloned()
            .ok_or_else(|| format!("the server doesn't have {plural}"))
    }

    pub(super) fn name(&self) -> Result<&str, String> {
        self.action.name.as_deref().ok_or_else(|| {
            format!(
                "{} needs one object: {0} {}/<name>",
                self.action.verb, self.action.resource
            )
        })
    }

    /// The query of a write: `dryRun`.
    pub(super) fn write_query(&self) -> BTreeMap<String, String> {
        let mut query = BTreeMap::new();
        if self.config.metadata.dry_run {
            query.insert("dryRun".into(), "All".into());
        }
        query
    }

    pub(super) fn suffix(&self) -> &'static str {
        if self.config.metadata.dry_run {
            " (server dry run)"
        } else {
            ""
        }
    }

    pub(super) fn written(&self, resource: &Resource, name: &str, what: &str) -> String {
        format!("{}/{name} {what}{}", resource.reference(), self.suffix())
    }

    /// A `Text` of the lines, whose JSON is the answers' bodies.
    pub(super) fn text(&self, lines: Vec<String>, data: Value) -> Content {
        Content::text(lines, data)
    }

    fn start(&mut self) -> Result<Step, String> {
        match self.action.verb.as_str() {
            "get" => self.get(),
            "watch" => self.watch(),
            "describe" => self.describe(),
            "logs" => self.logs(),
            "events" => self.events(),
            "top" => self.top(),
            "api-resources" => Ok(self.api_resources()),
            "auth can-i" => self.can_i(),
            "rollout status" => self.fetch_object(),
            "rollout history" | "rollout undo" => self.rollout_pair(),
            "rollout restart" => self.restart(),
            "scale" => self.scale(),
            "apply" => self.apply(),
            "create" => self.create(),
            "patch" => self.patch(),
            "delete" => self.delete(),
            "label" | "annotate" => self.label(),
            other => Err(format!("{other} is not supported")),
        }
    }

    /// The step after the answers of the last batch.
    pub fn next(&mut self, answers: Vec<Answer>) -> Result<Step, String> {
        match self.action.verb.as_str() {
            "get" | "events" => self.got(&answers),
            "watch" => self.watched(&answers),
            "describe" => self.described(&answers),
            "logs" => self.logged(&answers),
            "top" => self.topped(&answers),
            "auth can-i" => self.allowed(&answers),
            "rollout status" => self.rolled_out(&answers),
            "rollout history" => self.history(&answers),
            "rollout undo" => self.undone(&answers),
            "rollout restart" => self.single_write("restarted", &answers),
            "scale" => self.single_write("scaled", &answers),
            "apply" => self.applied(&answers, "serverside-applied"),
            "create" => self.applied(&answers, "created"),
            "patch" => self.single_write("patched", &answers),
            "delete" => self.deleted(&answers),
            "label" => self.single_write("labeled", &answers),
            "annotate" => self.single_write("annotated", &answers),
            other => Err(format!("{other} is not supported")),
        }
    }

    // get, events, watch

    pub(super) fn list_query(&self, fields: Option<String>) -> BTreeMap<String, String> {
        let metadata = &self.config.metadata;
        let mut query = BTreeMap::new();
        if let Some(selector) = &metadata.selector {
            query.insert("labelSelector".into(), selector.clone());
        }
        let field = [fields, metadata.field_selector.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(",");
        if !field.is_empty() {
            query.insert("fieldSelector".into(), field);
        }
        if let Some(limit) = metadata.limit {
            query.insert("limit".into(), limit.to_string());
        }
        query
    }

    pub(super) fn table(&self, mut request: Request) -> Request {
        if !self.structured() {
            request.accept = Some(TABLE.into());
            if !self.config.options.columns.is_empty() {
                request
                    .query
                    .insert("includeObject".into(), "Object".into());
            }
        }
        request
    }

    fn get(&mut self) -> Result<Step, String> {
        let resource = self.resource()?;
        let metadata = &self.config.metadata;
        let path = match &self.action.name {
            Some(name) => {
                if metadata.selector.is_some() || metadata.field_selector.is_some() {
                    return Err("A name and a Selector cannot be combined in get".into());
                }
                resource.object_path(self.namespace(), name)
            }
            None => resource.collection_path(self.namespace()),
        };
        let mut request = self.request("GET", path);
        request.query = self.list_query(None);
        self.resource = Some(resource);
        Ok(Step::Calls(vec![self.table(request)]))
    }

    fn events(&mut self) -> Result<Step, String> {
        let events = self.core("events")?;
        let mut fields = Vec::new();
        if !self.action.resource.is_empty() {
            let resource = self.resource()?;
            fields.push(format!("involvedObject.kind={}", resource.kind));
            if let Some(name) = &self.action.name {
                fields.push(format!("involvedObject.name={name}"));
            }
        }
        let mut request = self.request("GET", events.collection_path(self.namespace()));
        request.query = self.list_query((!fields.is_empty()).then(|| fields.join(",")));
        self.resource = Some(events);
        Ok(Step::Calls(vec![self.table(request)]))
    }

    fn got(&mut self, answers: &[Answer]) -> Result<Step, String> {
        let body = checked(&answers[0])?;
        let resource = self.resource.clone().unwrap_or_default();
        let empty = if resource.namespaced {
            format!("No resources found in {} namespace.", self.namespace())
        } else {
            "No resources found".into()
        };
        let mut rows = render::server_table(body, self.now, &empty);
        rows.data = body.clone();
        if self.action.verb == "events" {
            let when = |object: &Value| -> String {
                object["metadata"]["creationTimestamp"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            };
            rows.rows.sort_by_key(|row| when(&row.object));
            if let Some(list) = rows.data["items"].as_array_mut() {
                list.sort_by_key(|item| when(item));
            }
        }
        Ok(done(Content::Rows(rows)))
    }

    fn watch(&mut self) -> Result<Step, String> {
        let resource = self.resource()?;
        let mut request = self.request("GET", resource.collection_path(self.namespace()));
        request.query = self.list_query(
            self.action
                .name
                .as_ref()
                .map(|name| format!("metadata.name={name}")),
        );
        request.query.remove("limit");
        request.query.insert("watch".into(), "true".into());
        request.stream = true;
        self.resource = Some(resource);
        Ok(Step::Calls(vec![self.table(request)]))
    }

    /// The text lines of a watch event: the first Table brings the header.
    fn watch_lines(&mut self, event: &Value) -> Vec<String> {
        let kind = event["type"].as_str().unwrap_or_default().to_owned();
        let object = &event["object"];
        if object["kind"] != "Table" {
            return vec![format!(
                "{kind:<9} {}/{}",
                object["kind"].as_str().unwrap_or_default().to_lowercase(),
                object["metadata"]["name"].as_str().unwrap_or_default()
            )];
        }
        let rows = render::server_table(object, self.now, "");
        let mut lines = Vec::new();
        if self.header.is_none() && !rows.columns.is_empty() {
            let names: Vec<String> = rows
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect();
            lines.push(format!("{:<9} {}", "EVENT", names.join("   ")));
            self.header = Some(names);
        }
        for row in rows.rows {
            let cells: Vec<String> = row
                .cells
                .iter()
                .map(|cell| match cell {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
                .collect();
            lines.push(format!("{kind:<9} {}", cells.join("   ")));
        }
        lines
    }

    /// The text a streamed event shows while the run goes on.
    pub fn live(&mut self, event: &Event) -> Option<String> {
        match (self.action.verb.as_str(), event) {
            ("watch", Event::Object(value)) => {
                let lines = self.watch_lines(value);
                (!lines.is_empty()).then(|| lines.join("\n"))
            }
            _ => None,
        }
    }

    fn watched(&mut self, answers: &[Answer]) -> Result<Step, String> {
        if let Some(error) = status_error(&answers[0].response) {
            return Err(error);
        }
        self.header = None;
        let mut lines = Vec::new();
        let mut data = Vec::new();
        for event in &answers[0].events {
            if let Event::Object(value) = event {
                lines.extend(self.watch_lines(value));
                data.push(value.clone());
            }
        }
        Ok(done(self.text(lines, Value::Array(data))))
    }
}
