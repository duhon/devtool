//! The read-only composite actions: `describe`, `top`, `auth can-i` and `api-resources`.

use super::flow::{Answer, Flow, Step, checked, done, items, status_error};
use super::text;
use crate::render::{Content, Row, Rows};
use serde_json::{Value, json};
use services_sdk::lang::table::{Column, Kind};

const METRICS: &str = "/apis/metrics.k8s.io/v1beta1";

impl Flow {
    pub(super) fn describe(&mut self) -> Result<Step, String> {
        let resource = self.resource()?;
        let name = self.name()?.to_owned();
        let object = self.request("GET", resource.object_path(self.namespace(), &name));
        let mut fields = vec![
            format!("involvedObject.name={name}"),
            format!("involvedObject.kind={}", resource.kind),
        ];
        if resource.namespaced {
            fields.push(format!("involvedObject.namespace={}", self.namespace()));
        }
        let namespace = if resource.namespaced {
            self.namespace()
        } else {
            ""
        };
        let mut events = self.request("GET", self.core("events")?.collection_path(namespace));
        events
            .query
            .insert("fieldSelector".into(), fields.join(","));
        self.resource = Some(resource);
        Ok(Step::Calls(vec![object, events]))
    }

    pub(super) fn described(&mut self, answers: &[Answer]) -> Result<Step, String> {
        let object = checked(&answers[0])?.clone();
        let events = checked(&answers[1]).map(items).unwrap_or_default();
        let lines = text::describe(&object, &events, self.now);
        Ok(done(
            self.text(lines, json!({"object": object, "events": events})),
        ))
    }

    pub(super) fn top(&mut self) -> Result<Step, String> {
        let resource = self.resource()?;
        if !resource.group.is_empty() || !matches!(resource.plural.as_str(), "pods" | "nodes") {
            return Err("top takes pods or nodes".into());
        }
        let selector = self.config.metadata.selector.clone();
        let mut calls = Vec::new();
        let mut paths = Vec::new();
        if resource.plural == "pods" {
            paths.push(
                resource
                    .collection_path(self.namespace())
                    .replacen("/api/v1", METRICS, 1),
            );
        } else {
            paths.push(format!("{METRICS}/nodes"));
            paths.push("/api/v1/nodes".into());
        }
        for path in paths {
            let path = match &self.action.name {
                Some(name) => format!("{path}/{name}"),
                None => path,
            };
            let mut request = self.request("GET", path);
            if let Some(selector) = &selector {
                request
                    .query
                    .insert("labelSelector".into(), selector.clone());
            }
            calls.push(request);
        }
        self.resource = Some(resource);
        Ok(Step::Calls(calls))
    }

    pub(super) fn topped(&mut self, answers: &[Answer]) -> Result<Step, String> {
        let metrics = &answers[0];
        if metrics.response.status == 404 && self.action.name.is_none() {
            return Err("Metrics API not available".into());
        }
        let body = checked(metrics)?;
        let list = if body["items"].is_array() {
            items(body)
        } else {
            vec![body.clone()]
        };
        let pods = self.resource.as_ref().is_some_and(|r| r.plural == "pods");
        let nodes: Vec<Value> = answers
            .get(1)
            .and_then(|answer| checked(answer).ok())
            .map(|body| {
                if body["items"].is_array() {
                    items(body)
                } else {
                    vec![body.clone()]
                }
            })
            .unwrap_or_default();
        let cpu =
            |usage: &Value| text::quantity(usage["cpu"].as_str().unwrap_or("0")).unwrap_or(0.0);
        let memory =
            |usage: &Value| text::quantity(usage["memory"].as_str().unwrap_or("0")).unwrap_or(0.0);
        let mut rows = Vec::new();
        for item in &list {
            let name = item["metadata"]["name"].as_str().unwrap_or_default();
            if pods {
                let containers = item["containers"].as_array().cloned().unwrap_or_default();
                let cores: f64 = containers.iter().map(|c| cpu(&c["usage"])).sum();
                let bytes: f64 = containers.iter().map(|c| memory(&c["usage"])).sum();
                rows.push(Row {
                    cells: vec![
                        json!(name),
                        json!(format!("{}m", text::millicores(cores))),
                        json!(format!("{}Mi", text::mebibytes(bytes))),
                    ],
                    object: item.clone(),
                });
            } else {
                let allocatable = nodes
                    .iter()
                    .find(|node| node["metadata"]["name"].as_str() == Some(name))
                    .map(|node| node["status"]["allocatable"].clone())
                    .unwrap_or(Value::Null);
                let percent = |used: f64, total: f64| {
                    if total > 0.0 {
                        format!("{}%", (used * 100.0 / total).floor() as u64)
                    } else {
                        "<unknown>".into()
                    }
                };
                let (cores, bytes) = (cpu(&item["usage"]), memory(&item["usage"]));
                rows.push(Row {
                    cells: vec![
                        json!(name),
                        json!(format!("{}m", text::millicores(cores))),
                        json!(percent(cores, cpu(&allocatable))),
                        json!(format!("{}Mi", text::mebibytes(bytes))),
                        json!(percent(bytes, memory(&allocatable))),
                    ],
                    object: item.clone(),
                });
            }
        }
        let names: &[&str] = if pods {
            &["NAME", "CPU(cores)", "MEMORY(bytes)"]
        } else {
            &["NAME", "CPU(cores)", "CPU%", "MEMORY(bytes)", "MEMORY%"]
        };
        let empty = if pods {
            format!("No resources found in {} namespace.", self.namespace())
        } else {
            "No resources found".into()
        };
        Ok(done(Content::Rows(Rows {
            columns: names
                .iter()
                .map(|name| Column::new(*name, Kind::Text))
                .collect(),
            rows,
            data: body.clone(),
            empty,
        })))
    }

    pub(super) fn can_i(&mut self) -> Result<Step, String> {
        let verb = self.action.args.first().cloned().unwrap_or_default();
        let name = self.action.resource.clone();
        let (group, plural, namespaced) = match crate::resources::resolve(&self.catalog, &name) {
            Some(resource) => (
                resource.group.clone(),
                resource.plural.clone(),
                resource.namespaced,
            ),
            None if name == "*" => ("*".into(), "*".into(), true),
            None => match name.split_once('.') {
                Some((plural, group)) => (group.to_owned(), plural.to_owned(), true),
                None => (String::new(), name.clone(), true),
            },
        };
        let mut attributes = json!({"verb": verb, "group": group, "resource": plural});
        if namespaced {
            attributes["namespace"] = json!(self.namespace());
        }
        if let Some(object) = &self.action.name {
            attributes["name"] = json!(object);
        }
        let mut request = self.request(
            "POST",
            "/apis/authorization.k8s.io/v1/selfsubjectaccessreviews".into(),
        );
        request.content_type = Some("application/json".into());
        request.body = Some(
            json!({"apiVersion": "authorization.k8s.io/v1", "kind": "SelfSubjectAccessReview",
                "spec": {"resourceAttributes": attributes}})
            .to_string(),
        );
        Ok(Step::Calls(vec![request]))
    }

    pub(super) fn allowed(&mut self, answers: &[Answer]) -> Result<Step, String> {
        let body = checked(&answers[0])?;
        let status = &body["status"];
        let line = if status["allowed"] == true {
            "yes".to_owned()
        } else {
            match status["reason"]
                .as_str()
                .filter(|reason| !reason.is_empty())
            {
                Some(reason) => format!("no - {reason}"),
                None => "no".into(),
            }
        };
        Ok(done(self.text(vec![line], body.clone())))
    }

    pub(super) fn api_resources(&mut self) -> Step {
        let columns = ["NAME", "SHORTNAMES", "APIVERSION", "NAMESPACED", "KIND"]
            .map(|name| Column::new(name, Kind::Text));
        let mut rows = Vec::new();
        let mut data = Vec::new();
        for resource in self.catalog.iter() {
            let short = resource.short_names.join(",");
            data.push(
                json!({"name": resource.plural, "shortNames": resource.short_names,
                "apiVersion": resource.api_version(), "namespaced": resource.namespaced,
                "kind": resource.kind, "verbs": resource.verbs}),
            );
            rows.push(Row {
                cells: vec![
                    json!(resource.plural),
                    json!(short),
                    json!(resource.api_version()),
                    json!(resource.namespaced.to_string()),
                    json!(resource.kind),
                ],
                object: data.last().cloned().unwrap_or_default(),
            });
        }
        let _ = status_error;
        done(Content::Rows(Rows {
            columns: columns.to_vec(),
            rows,
            data: Value::Array(data),
            empty: "No resources found".into(),
        }))
    }
}
