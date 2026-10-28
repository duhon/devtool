use crate::core::{
    error::{Result, text},
    hints::Hints,
    registry::Registry,
    variables::VariableStore,
};
use serde_json::{Value, json};
use std::path::PathBuf;
#[derive(Clone)]
pub struct Runtime {
    pub operations: crate::core::operations::Operations,
    pub agent: crate::agent::Agent,
    pub registry: Registry,
    pub variable_store: VariableStore,
    pub dictionary_store: VariableStore,
    pub hints: Hints,
    pub templates: crate::core::templates::Templates,
    pub history: crate::core::history::History,
}
impl Runtime {
    pub fn agent_session_sensitive_values(&self, params: &Value) -> Result<Vec<String>> {
        let mut values = self.agent_sensitive_values()?;
        if let Some(variables) = params.get("__consoleVariables") {
            let context = self
                .variable_store
                .session_dispatch(&json!({"op":"list"}), variables)?;
            Self::collect_sensitive(&context["context"]["resolvedProfiles"], &mut values);
            Self::collect_sensitive(variables, &mut values);
        }
        Ok(values)
    }
    fn collect_sensitive(value: &Value, out: &mut Vec<String>) {
        if let Some(object) = value.as_object() {
            for (key, value) in object {
                if crate::agent::Redactor::sensitive_name(key) {
                    if let Some(value) = value.as_str() {
                        out.push(value.to_owned());
                    }
                } else {
                    Self::collect_sensitive(value, out);
                }
            }
        }
    }
    pub fn agent_sensitive_values(&self) -> Result<Vec<String>> {
        let mut values = Vec::new();
        Self::collect_sensitive(
            &self.variable_store.list()?["context"]["resolvedProfiles"],
            &mut values,
        );
        for (name, value) in std::env::vars() {
            if crate::agent::Redactor::sensitive_name(&name) {
                values.push(value);
            }
        }
        Ok(values)
    }
    pub fn new() -> Result<Self> {
        let base = std::env::var_os("SERVICES_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        Self::with_data_dir(base)
    }
    pub fn with_data_dir(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let variable_store = VariableStore::new(path.join(".variable-profiles.json"));
        let dictionary_store = variable_store.dictionary();
        let registry = Registry::new(variable_store.clone())?;
        Ok(Self {
            operations: Default::default(),
            agent: crate::agent::Agent::new(path.join(".agent")),
            registry,
            variable_store,
            dictionary_store,
            hints: Hints::new(path.join(".console-hints.txt")),
            templates: crate::core::templates::Templates::new(path.join(".console-templates.json")),
            history: crate::core::history::History::new(path.join(".console-history.sqlite3")),
        })
    }
    /// A single resolution feeds both the real execution and its immutable snapshot.
    pub async fn execute(
        &self,
        message: &Value,
        cancel: tokio_util::sync::CancellationToken,
        events: Option<tokio::sync::mpsc::Sender<Value>>,
    ) -> Result<Value> {
        use crate::core::{
            error::Error,
            history::{self, Snapshot},
        };
        let started = history::now();
        let clock = std::time::Instant::now();
        let service = text(message, "service");
        let empty = json!({});
        let raw = message.get("params").unwrap_or(&empty);
        let context = message.get("history");
        let source = context.and_then(|c| c["source"].as_str());
        let resolved = self
            .variable_store
            .resolve_for_history(&service, raw, source);
        let (params, snapshot) = match resolved {
            Ok((params, snapshot)) => (params, snapshot),
            Err(mut e) => {
                if context.is_some() {
                    e.history = Some(Box::new(
                        json!({"saved":false,"error":"Request variables could not be resolved; no resolved snapshot exists"}),
                    ));
                }
                return Err(e.with_debug(raw.clone()));
            }
        };
        let mut output = String::new();
        let mut oversized = false;
        let result = if let Some(events) = events {
            let (tx, mut rx) = tokio::sync::mpsc::channel(128);
            let run = self
                .registry
                .stream_resolved(&service, params.clone(), cancel.clone(), tx);
            tokio::pin!(run);
            let result = loop {
                tokio::select! {
                    result=&mut run=>break result,
                    event=rx.recv()=>if let Some(event)=event {
                        if context.is_some() && !oversized {
                            let line = format!("{}\n", event);
                            if output.len().saturating_add(line.len()) > history::MAX_BYTES {
                                oversized=true;output.clear();
                            } else { output.push_str(&line); }
                        }
                        // A disconnected viewer must not stop collection/reaping of a running child.
                        if events.send(event).await.is_err() { cancel.cancel(); }
                    }
                }
            };
            while let Some(event) = rx.recv().await {
                if context.is_some() && !oversized {
                    let line = format!("{}\n", event);
                    if output.len().saturating_add(line.len()) > history::MAX_BYTES {
                        oversized = true;
                        output.clear();
                    } else {
                        output.push_str(&line);
                    }
                }
                let _ = events.send(event).await;
            }
            result
        } else {
            self.registry
                .call_resolved(&service, params.clone(), cancel.clone())
                .await
        };
        let Some(context) = context else {
            return result;
        };
        let ended = history::now();
        let duration = clock.elapsed().as_millis() as u64;
        let saved = async {
            let check_id = history::uuid(context, "checkId")?;
            let session_id = history::uuid(context, "sessionId")?;
            let title = context["title"]
                .as_str()
                .ok_or_else(|| Error::new("History title is required"))?;
            if source.is_none() {
                return Err(Error::new("History source is required"));
            }
            let (source, secrets) = snapshot?;
            if oversized {
                return Err(Error::new("Snapshot exceeds the 100 MiB history quota"));
            }
            let response = match &result {
                Ok(value) => value.clone(),
                Err(e) => e.json(),
            };
            let redactor = history::redactor(&params, &source, secrets);
            let status = if response["cancelled"] == true || cancel.is_cancelled() {
                "stopped"
            } else if result.is_err()
                || response.get("error").is_some()
                || response["code"].as_i64().is_some_and(|code| code != 0)
                || (service == "grpc" && response["status"].as_i64().is_some_and(|code| code != 0))
                || response["status"].as_u64().is_some_and(|code| code >= 400)
            {
                "error"
            } else {
                "success"
            };
            let outcome = services_contracts::response_outcome(&service, &response);
            let text = format!(
                "{}\n\n#responce {outcome}\n{}{}",
                redactor.text(&source),
                if output.is_empty() {
                    String::new()
                } else {
                    format!("#terminal-output\n{output}\n")
                },
                history::response_text(&response)?
            );
            self.history
                .save(Snapshot {
                    check_id,
                    session_id,
                    title: redactor.text(title),
                    service,
                    started,
                    ended,
                    duration,
                    status: status.into(),
                    text,
                })
                .await
        }
        .await;
        let metadata = match saved {
            Ok(id) => json!({"saved":true,"id":id}),
            Err(e) => json!({"saved":false,"error":e.to_string()}),
        };
        match result {
            Ok(mut value) => {
                value["history"] = metadata;
                Ok(value)
            }
            Err(mut e) => {
                e.history = Some(Box::new(metadata));
                Err(e)
            }
        }
    }
    pub async fn dispatch(&self, message: &Value) -> Value {
        self.dispatch_cancellable(message, tokio_util::sync::CancellationToken::new())
            .await
    }
    pub async fn dispatch_cancellable(
        &self,
        message: &Value,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Value {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let service = text(message, "service");
        let result = if message["type"] == "health" {
            Ok(json!({"protocolVersion":services_contracts::PROTOCOL_VERSION}))
        } else if service == "updates" {
            crate::core::updates::dispatch(message).await
        } else if service == "history" {
            self.history.dispatch(message).await
        } else if service == "sessions" {
            (|| -> Result<Value> {
                let variables = self.variable_store.list()?;
                let hints: serde_json::Map<String, Value> =
                    ["mysql", "rest", "graphql", "grpc", "splunk", "terminal"]
                        .into_iter()
                        .map(|name| (name.into(), self.hints.get(name)["text"].clone()))
                        .collect();
                Ok(json!({"variables":variables["profiles"]["var"],"hints":hints}))
            })()
        } else if service == "vars" {
            if let Some(variables) = message.get("sessionVariables") {
                self.variable_store.session_dispatch(message, variables)
            } else {
                self.variable_store.dispatch(message)
            }
        } else if service == "dictionary" {
            if let Some(variables) = message.get("sessionVariables") {
                self.dictionary_store.session_dispatch(message, variables)
            } else {
                self.dictionary_store.dispatch(message)
            }
        } else if service == "templates" {
            match message["op"].as_str() {
                Some("get") => self.templates.get(),
                Some("save") => self.templates.save(message),
                _ => Err(crate::core::error::Error::new(
                    "Unknown templates operation",
                )),
            }
        } else if service == "hints" {
            let service = text(message, "serviceName");
            if message["op"] == "save" {
                self.hints.save(&service, &text(message, "text"))
            } else {
                Ok(self.hints.get(&service))
            }
        } else if message["type"] == "discover" {
            Ok(self
                .registry
                .discover_cancellable(
                    &service,
                    message.get("params").unwrap_or(&json!({})),
                    cancel,
                )
                .await
                .unwrap_or_else(|e| e.json()))
        } else {
            self.execute(message, cancel, None).await
        };
        match result {
            Ok(result) => {
                let mut reply = json!({"id":id,"ok":true,"result":result});
                if let Some(metadata) = result.get("history") {
                    reply["history"] = metadata.clone();
                }
                reply
            }
            Err(error) => {
                let mut reply = error.json();
                reply["id"] = id;
                reply["ok"] = json!(false);
                reply
            }
        }
    }
}
