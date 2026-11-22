//! The actions that read an object to judge it (`rollout`) and the ones that write.

use super::flow::{
    Answer, CHANGE_CAUSE, Done, FIELD_MANAGER, Flow, RESTARTED_AT, REVISION, Step, checked, done,
    items, status_error,
};
use super::text;
use crate::render::{Content, Row, Rows, time, yaml};
use crate::resources::{self, Resource};
use serde_json::{Value, json};
use services_sdk::lang::table::{Column, Kind};

const MERGE: &str = "application/merge-patch+json";

impl Flow {
    fn json_body(
        &self,
        mut request: services_sdk::kubernetes::Request,
        content_type: &str,
        body: &Value,
    ) -> services_sdk::kubernetes::Request {
        request.content_type = Some(content_type.into());
        request.body = Some(body.to_string());
        request
    }

    pub(super) fn fetch_object(&mut self) -> Result<Step, String> {
        let resource = self.resource()?;
        let name = self.name()?.to_owned();
        let request = self.request("GET", resource.object_path(self.namespace(), &name));
        self.resource = Some(resource);
        Ok(Step::Calls(vec![request]))
    }

    pub(super) fn rolled_out(&mut self, answers: &[Answer]) -> Result<Step, String> {
        let object = checked(&answers[0])?.clone();
        let (message, complete) = text::rollout_status(&object)?;
        Ok(Step::Done(Done {
            content: self.text(vec![message], object),
            outcome: if complete { "OK" } else { "WARNING" },
        }))
    }

    pub(super) fn rollout_pair(&mut self) -> Result<Step, String> {
        let resource = self.resource()?;
        if resource.kind != "Deployment" {
            return Err(format!(
                "{} works on deployments, not on {}",
                self.action.verb, resource.plural
            ));
        }
        let name = self.name()?.to_owned();
        let sets = resources::by_kind(&self.catalog, "apps/v1", "ReplicaSet")
            .ok_or("the server doesn't have replicasets")?;
        let object = self.request("GET", resource.object_path(self.namespace(), &name));
        let list = self.request("GET", sets.collection_path(self.namespace()));
        self.resource = Some(resource);
        Ok(Step::Calls(vec![object, list]))
    }

    /// The ReplicaSets the deployment owns, oldest revision first.
    fn revisions(&self, answers: &[Answer]) -> Result<(Value, Vec<(u64, Value)>), String> {
        let deployment = checked(&answers[0])?.clone();
        let uid = deployment["metadata"]["uid"].as_str().unwrap_or_default();
        let mut sets: Vec<(u64, Value)> = items(checked(&answers[1])?)
            .into_iter()
            .filter(|set| {
                set["metadata"]["ownerReferences"]
                    .as_array()
                    .is_some_and(|owners| owners.iter().any(|owner| owner["uid"] == uid))
            })
            .filter_map(|set| {
                let revision = set["metadata"]["annotations"][REVISION]
                    .as_str()?
                    .parse()
                    .ok()?;
                Some((revision, set))
            })
            .collect();
        sets.sort_by_key(|(revision, _)| *revision);
        Ok((deployment, sets))
    }

    pub(super) fn history(&mut self, answers: &[Answer]) -> Result<Step, String> {
        let (deployment, sets) = self.revisions(answers)?;
        let name = deployment["metadata"]["name"].as_str().unwrap_or_default();
        if let Some(wanted) = self.config.metadata.revision.filter(|r| *r > 0) {
            let (_, set) = sets
                .iter()
                .find(|(revision, _)| *revision == wanted)
                .ok_or_else(|| format!("unable to find the specified revision {wanted}"))?;
            let mut lines = vec![format!("deployment.apps/{name} with revision #{wanted}")];
            lines.extend(yaml(&set["spec"]["template"]).lines().map(str::to_owned));
            return Ok(done(self.text(lines, set["spec"]["template"].clone())));
        }
        let rows = sets
            .iter()
            .map(|(revision, set)| Row {
                cells: vec![
                    json!(revision.to_string()),
                    json!(
                        set["metadata"]["annotations"][CHANGE_CAUSE]
                            .as_str()
                            .unwrap_or("<none>")
                    ),
                ],
                object: set.clone(),
            })
            .collect();
        Ok(done(Content::Rows(Rows {
            columns: vec![
                Column::new("REVISION", Kind::Text),
                Column::new("CHANGE-CAUSE", Kind::Text),
            ],
            rows,
            data: Value::Array(sets.iter().map(|(_, set)| set.clone()).collect()),
            empty: format!("No rollout history found for deployment {name:?}"),
        })))
    }

    pub(super) fn undone(&mut self, answers: &[Answer]) -> Result<Step, String> {
        if self.stage == 1 {
            return self.single_write("rolled back", answers);
        }
        let (deployment, sets) = self.revisions(answers)?;
        let name = deployment["metadata"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let wanted = self.config.metadata.revision.filter(|r| *r > 0);
        let target = match wanted {
            Some(wanted) => sets.iter().find(|(revision, _)| *revision == wanted),
            None => sets.len().checked_sub(2).and_then(|i| sets.get(i)),
        };
        let Some((_, set)) = target else {
            return Err(match wanted {
                Some(wanted) => format!("unable to find specified revision {wanted} in history"),
                None => "no rollout history found to roll back to".into(),
            });
        };
        let mut template = set["spec"]["template"].clone();
        if let Some(labels) = template["metadata"]["labels"].as_object_mut() {
            labels.remove("pod-template-hash");
        }
        let mut current = deployment["spec"]["template"].clone();
        if let Some(labels) = current["metadata"]["labels"].as_object_mut() {
            labels.remove("pod-template-hash");
        }
        if template == current {
            return Ok(done(self.text(
                vec![format!("deployment.apps/{name} already rolled back")],
                Value::Null,
            )));
        }
        let resource = self.resource()?;
        let mut request = self.request("PATCH", resource.object_path(self.namespace(), &name));
        request.query = self.write_query();
        let body = json!([{"op": "replace", "path": "/spec/template", "value": template}]);
        let request = self.json_body(request, "application/json-patch+json", &body);
        self.stage = 1;
        Ok(Step::Calls(vec![request]))
    }

    pub(super) fn restart(&mut self) -> Result<Step, String> {
        let resource = self.resource()?;
        if !matches!(
            resource.kind.as_str(),
            "Deployment" | "StatefulSet" | "DaemonSet"
        ) {
            return Err(format!(
                "rollout restart doesn't work on {}",
                resource.plural
            ));
        }
        let name = self.name()?.to_owned();
        let mut request = self.request("PATCH", resource.object_path(self.namespace(), &name));
        request.query = self.write_query();
        let body = json!({"spec": {"template": {"metadata": {"annotations": {
            RESTARTED_AT: time::format(self.now)}}}}});
        let request = self.json_body(request, MERGE, &body);
        self.resource = Some(resource);
        Ok(Step::Calls(vec![request]))
    }

    pub(super) fn scale(&mut self) -> Result<Step, String> {
        let resource = self.resource()?;
        let name = self.name()?.to_owned();
        let Some(replicas) = self.config.metadata.replicas else {
            return Err("scale needs the Replicas metadata".into());
        };
        if !resource.has_subresource("scale") {
            return Err(format!("{} can't be scaled", resource.plural));
        }
        let mut request = self.request(
            "PATCH",
            resource.subresource_path(self.namespace(), &name, "scale"),
        );
        request.query = self.write_query();
        let request = self.json_body(request, MERGE, &json!({"spec": {"replicas": replicas}}));
        self.resource = Some(resource);
        Ok(Step::Calls(vec![request]))
    }

    /// The resource and name of each payload object.
    fn objects(&mut self) -> Result<Vec<(Resource, String, Value)>, String> {
        let mut found = Vec::new();
        for object in &self.payload {
            let api_version = object["apiVersion"].as_str().unwrap_or_default();
            let kind = object["kind"].as_str().unwrap_or_default();
            let resource =
                resources::by_kind(&self.catalog, api_version, kind).ok_or_else(|| {
                    format!("no matches for kind {kind:?} in version {api_version:?}")
                })?;
            if !self.action.resource.is_empty() {
                let wanted = resources::resolve(&self.catalog, &self.action.resource);
                if wanted.is_some_and(|wanted| wanted.kind != resource.kind) {
                    return Err(format!(
                        "the payload holds {kind} objects, not {}",
                        self.action.resource
                    ));
                }
            }
            let name = object["metadata"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if name.is_empty() && self.action.verb == "apply" {
                return Err(format!("{kind} needs metadata.name to be applied"));
            }
            found.push((resource, name, object.clone()));
        }
        Ok(found)
    }

    pub(super) fn apply(&mut self) -> Result<Step, String> {
        self.write_objects(true)
    }

    pub(super) fn create(&mut self) -> Result<Step, String> {
        self.write_objects(false)
    }

    fn write_objects(&mut self, apply: bool) -> Result<Step, String> {
        let mut calls = Vec::new();
        self.targets.clear();
        for (resource, name, object) in self.objects()? {
            let namespace = self.namespace().to_owned();
            let mut request;
            if apply {
                request = self.request("PATCH", resource.object_path(&namespace, &name));
                request.query = self.write_query();
                request
                    .query
                    .insert("fieldManager".into(), FIELD_MANAGER.into());
                if self.config.metadata.force {
                    request.query.insert("force".into(), "true".into());
                }
                request = self.json_body(request, "application/apply-patch+yaml", &object);
            } else {
                request = self.request("POST", resource.collection_path(&namespace));
                request.query = self.write_query();
                request = self.json_body(request, "application/json", &object);
            }
            self.targets.push((resource.reference(), name));
            calls.push(request);
        }
        Ok(Step::Calls(calls))
    }

    pub(super) fn applied(&mut self, answers: &[Answer], what: &str) -> Result<Step, String> {
        let mut lines = Vec::new();
        let mut errors = Vec::new();
        let mut data = Vec::new();
        for ((reference, name), answer) in self.targets.iter().zip(answers) {
            match checked(answer) {
                Ok(body) => {
                    let name = body["metadata"]["name"].as_str().unwrap_or(name);
                    lines.push(format!("{reference}/{name} {what}{}", self.suffix()));
                    data.push(body.clone());
                }
                Err(error) => errors.push(error),
            }
        }
        if lines.is_empty() {
            return Err(errors.join("\n"));
        }
        let outcome = if errors.is_empty() { "OK" } else { "ERROR" };
        lines.extend(errors);
        Ok(Step::Done(Done {
            content: self.text(lines, Value::Array(data)),
            outcome,
        }))
    }

    pub(super) fn patch(&mut self) -> Result<Step, String> {
        let resource = self.resource()?;
        let name = self.name()?.to_owned();
        let kind = self
            .config
            .metadata
            .patch_type
            .unwrap_or(crate::config::PatchType::Strategic);
        let mut request = self.request("PATCH", resource.object_path(self.namespace(), &name));
        request.query = self.write_query();
        if kind == crate::config::PatchType::Apply {
            request
                .query
                .insert("fieldManager".into(), FIELD_MANAGER.into());
            if self.config.metadata.force {
                request.query.insert("force".into(), "true".into());
            }
        }
        let request = self.json_body(request, kind.content_type(), &self.payload[0].clone());
        self.resource = Some(resource);
        Ok(Step::Calls(vec![request]))
    }

    pub(super) fn label(&mut self) -> Result<Step, String> {
        let resource = self.resource()?;
        let name = self.name()?.to_owned();
        let field = if self.action.verb == "label" {
            "labels"
        } else {
            "annotations"
        };
        let mut map = serde_json::Map::new();
        for arg in &self.action.args {
            match arg.strip_suffix('-') {
                Some(key) if !arg.contains('=') => map.insert(key.to_owned(), Value::Null),
                _ => {
                    let (key, value) = arg.split_once('=').unwrap_or((arg, ""));
                    map.insert(key.to_owned(), json!(value))
                }
            };
        }
        let mut request = self.request("PATCH", resource.object_path(self.namespace(), &name));
        request.query = self.write_query();
        let request = self.json_body(request, MERGE, &json!({"metadata": {field: map}}));
        self.resource = Some(resource);
        Ok(Step::Calls(vec![request]))
    }

    pub(super) fn single_write(&mut self, what: &str, answers: &[Answer]) -> Result<Step, String> {
        let body = checked(&answers[0])?;
        let resource = self.resource.clone().unwrap_or_default();
        let name = self.action.name.clone().unwrap_or_default();
        let line = self.written(&resource, &name, what);
        Ok(done(self.text(vec![line], body.clone())))
    }

    pub(super) fn delete(&mut self) -> Result<Step, String> {
        let resource = self.resource()?;
        let metadata = &self.config.metadata;
        let path = match &self.action.name {
            Some(name) => resource.object_path(self.namespace(), name),
            None if metadata.selector.is_some() || metadata.field_selector.is_some() => {
                resource.collection_path(self.namespace())
            }
            None => {
                return Err(format!(
                    "delete needs one object or a Selector: delete {}/<name>",
                    self.action.resource
                ));
            }
        };
        let mut request = self.request("DELETE", path);
        request.query = self.list_query(None);
        request.query.remove("limit");
        request.query.extend(self.write_query());
        request
            .query
            .insert("propagationPolicy".into(), "Background".into());
        if let Some(grace) = self.config.metadata.grace_period {
            request
                .query
                .insert("gracePeriodSeconds".into(), grace.to_string());
        }
        self.resource = Some(resource);
        Ok(Step::Calls(vec![request]))
    }

    pub(super) fn deleted(&mut self, answers: &[Answer]) -> Result<Step, String> {
        let body = checked(&answers[0])?;
        let resource = self.resource.clone().unwrap_or_default();
        let reference = resource.reference();
        let named: Vec<String> = match &self.action.name {
            Some(name) => vec![name.clone()],
            None => items(body)
                .iter()
                .filter_map(|item| item["metadata"]["name"].as_str().map(str::to_owned))
                .collect(),
        };
        let lines = if named.is_empty() {
            vec![format!(
                "No resources found in {} namespace.",
                self.namespace()
            )]
        } else {
            named
                .iter()
                .map(|name| format!("{reference} \"{name}\" deleted{}", self.suffix()))
                .collect()
        };
        let _ = status_error;
        Ok(done(self.text(lines, body.clone())))
    }
}
