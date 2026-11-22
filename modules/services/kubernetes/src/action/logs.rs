//! `logs`: the logs of a pod, or of all pods of a workload, for the containers asked for.

use super::flow::{
    Answer, Flow, MAX_LOG_CALLS, Step, WORKLOADS, checked, done, items, status_error, text_of,
};
use super::text;
use crate::config::Since;
use crate::render::{Content, LogLine, Logs};
use serde_json::Value;
use services_sdk::kubernetes::{Event, Request};

impl Flow {
    pub(super) fn logs(&mut self) -> Result<Step, String> {
        let resource = self.resource()?;
        let selector = self.config.metadata.selector.clone();
        let step = match (
            &self.action.name,
            resource.plural.as_str(),
            resource.group.is_empty(),
        ) {
            (Some(name), "pods", true) => match &self.config.metadata.container {
                Some(container) => {
                    self.targets = vec![(name.clone(), container.clone())];
                    self.stage = 3;
                    self.log_calls()
                }
                None => {
                    self.stage = 1;
                    Ok(Step::Calls(vec![self.request(
                        "GET",
                        resource.object_path(self.namespace(), name),
                    )]))
                }
            },
            (None, "pods", true) => {
                let Some(selector) = selector else {
                    return Err("logs needs a pod name or a Selector".into());
                };
                self.stage = 2;
                let mut request = self.request("GET", resource.collection_path(self.namespace()));
                request.query.insert("labelSelector".into(), selector);
                Ok(Step::Calls(vec![request]))
            }
            (Some(name), _, _) if WORKLOADS.contains(&resource.kind.as_str()) => {
                self.stage = 4;
                Ok(Step::Calls(vec![self.request(
                    "GET",
                    resource.object_path(self.namespace(), name),
                )]))
            }
            (Some(_), _, _) => Err(format!(
                "logs works on pods and on workloads with a selector, not on {}",
                resource.plural
            )),
            (None, _, _) => Err(format!(
                "logs needs one object: logs {}/<name>",
                self.action.resource
            )),
        };
        self.resource = Some(resource);
        step
    }

    /// The pod containers of `pods`: the one asked for, else all of them.
    fn containers_of(&mut self, pods: &[Value]) {
        let wanted = self.config.metadata.container.clone();
        self.targets.clear();
        for pod in pods {
            let name = pod["metadata"]["name"].as_str().unwrap_or_default();
            for list in ["initContainers", "containers"] {
                for container in pod["spec"][list].as_array().into_iter().flatten() {
                    let container = container["name"].as_str().unwrap_or_default();
                    if wanted.as_deref().is_none_or(|wanted| wanted == container) {
                        self.targets.push((name.to_owned(), container.to_owned()));
                    }
                }
            }
        }
    }

    fn log_calls(&mut self) -> Result<Step, String> {
        if self.targets.is_empty() {
            return Err(match &self.config.metadata.container {
                Some(container) => format!("no container {container:?} found in the pods"),
                None => "no pods found for the logs".into(),
            });
        }
        if self.targets.len() > MAX_LOG_CALLS {
            return Err(format!(
                "{} pod containers match; narrow them down with Container or Selector (up to {MAX_LOG_CALLS})",
                self.targets.len()
            ));
        }
        let pods = self.core("pods")?;
        let metadata = &self.config.metadata;
        let mut calls = Vec::new();
        for (pod, container) in &self.targets {
            let mut request = Request::new(
                &self.target.context,
                "GET",
                &pods.subresource_path(self.namespace(), pod, "log"),
            );
            request.query.insert("container".into(), container.clone());
            if let Some(tail) = metadata.tail {
                request.query.insert("tailLines".into(), tail.to_string());
            }
            match &metadata.since {
                Some(Since::Seconds(seconds)) => {
                    request
                        .query
                        .insert("sinceSeconds".into(), seconds.to_string());
                }
                Some(Since::Time(time)) => {
                    request.query.insert("sinceTime".into(), time.clone());
                }
                None => {}
            }
            if metadata.previous {
                request.query.insert("previous".into(), "true".into());
            }
            if metadata.timestamps {
                request.query.insert("timestamps".into(), "true".into());
            }
            if metadata.follow {
                request.query.insert("follow".into(), "true".into());
                request.stream = true;
            }
            calls.push(request);
        }
        self.stage = 3;
        Ok(Step::Calls(calls))
    }

    pub(super) fn logged(&mut self, answers: &[Answer]) -> Result<Step, String> {
        match self.stage {
            1 => {
                let pod = checked(&answers[0])?.clone();
                self.containers_of(&[pod]);
                self.log_calls()
            }
            2 => {
                let pods = items(checked(&answers[0])?);
                self.containers_of(&pods);
                self.log_calls()
            }
            4 => {
                let workload = checked(&answers[0])?;
                let selector = text::selector(&workload["spec"]["selector"]).ok_or_else(|| {
                    format!(
                        "{} has no selector to find its pods by",
                        self.action.resource
                    )
                })?;
                let selector = match &self.config.metadata.selector {
                    Some(extra) => format!("{selector},{extra}"),
                    None => selector,
                };
                let pods = self.core("pods")?;
                let mut request = self.request("GET", pods.collection_path(self.namespace()));
                request.query.insert("labelSelector".into(), selector);
                self.stage = 2;
                Ok(Step::Calls(vec![request]))
            }
            _ => self.gathered(answers),
        }
    }

    fn gathered(&mut self, answers: &[Answer]) -> Result<Step, String> {
        let mut lines = Vec::new();
        let mut notes = Vec::new();
        for ((pod, container), answer) in self.targets.iter().zip(answers) {
            if let Some(error) = status_error(&answer.response) {
                notes.push(format!("[{pod}/{container}] {error}"));
                continue;
            }
            let texts: Vec<String> = if answer.events.is_empty() && self.config.metadata.follow {
                Vec::new()
            } else if self.config.metadata.follow {
                answer
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        Event::Line(text) => Some(text.clone()),
                        Event::Object(_) => None,
                    })
                    .collect()
            } else {
                text_of(answer).lines().map(str::to_owned).collect()
            };
            lines.extend(texts.into_iter().map(|text| LogLine {
                pod: pod.clone(),
                container: container.clone(),
                text,
            }));
        }
        if notes.len() == answers.len() {
            return Err(notes.join("\n"));
        }
        let multi = self.targets.len() > 1;
        Ok(done(Content::Logs(Logs {
            lines,
            multi,
            notes,
        })))
    }

    /// The text a followed log line shows while the run goes on.
    pub fn live_log(&self, stream: usize, text: &str) -> String {
        match self.targets.get(stream) {
            Some((pod, container)) if self.targets.len() > 1 && !self.config.options.logs_json => {
                format!("[{pod}/{container}] {text}")
            }
            _ => text.to_owned(),
        }
    }
}
