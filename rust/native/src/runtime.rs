use crate::core::{
    error::{Result, text},
    hints::Hints,
    registry::Registry,
    text::SharedStr,
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
        let manifests = registry.manifests().clone();
        Ok(Self {
            operations: Default::default(),
            agent: crate::agent::Agent::new(path.join(".agent")),
            registry,
            variable_store,
            dictionary_store,
            hints: Hints::new(path.join(".console-hints.txt")),
            templates: crate::core::templates::Templates::new(
                path.join(".console-templates.json"),
                manifests,
            ),
            history: crate::core::history::History::new(path.join(".console-history.sqlite3")),
        })
    }
    /// `{"service":"modules"}`: the loaded modules and the packages that failed to load;
    /// `op:"ui"` with a `module` id: the module's console part as base64;
    /// `op:"install"` with a package `file` as base64 and `op:"remove"` with a `module`
    /// folder change the modules folder; `forget:true` also deletes the module's history,
    /// templates and hints. This host keeps the modules it loaded; a new host process loads
    /// the change.
    async fn modules(&self, message: &Value) -> Result<Value> {
        use crate::core::{error::Error, install};
        use base64::Engine;
        match message["op"].as_str() {
            Some("ui") => {}
            Some("install") => {
                let archive = base64::engine::general_purpose::STANDARD
                    .decode(message["file"].as_str().unwrap_or_default())
                    .map_err(|e| Error::new(format!("Invalid package file: {e}")))?;
                let registry = self.registry.clone();
                let installed = tokio::task::spawn_blocking(move || {
                    install::install(registry.dir(), &archive, |dir| registry.check(dir))
                })
                .await
                .map_err(|e| Error::new(format!("Install failed: {e}")))?
                .map_err(Error::new)?;
                let manifest = installed.manifest;
                return Ok(json!({
                    "installed": {"id": manifest.id, "name": manifest.name, "version": manifest.version},
                    "replaced": installed.replaced,
                }));
            }
            Some("remove") => {
                let name = text(message, "module");
                install::remove(self.registry.dir(), &name).map_err(Error::new)?;
                let mut reply = json!({"removed": name});
                if message["forget"] == true {
                    let (history, templates, hints) = (
                        self.history.clone(),
                        self.templates.clone(),
                        self.hints.clone(),
                    );
                    let forgotten = tokio::task::spawn_blocking(move || -> Result<()> {
                        history.forget(&name)?;
                        templates.forget(&name)?;
                        hints.forget(&name)
                    })
                    .await
                    .map_err(|e| Error::new(e.to_string()))
                    .and_then(|result| result);
                    match forgotten {
                        Ok(()) => reply["forgotten"] = json!(true),
                        Err(e) => {
                            reply["warning"] = json!(format!(
                                "The module was removed, but its history, templates or hints were not deleted: {}",
                                e.message
                            ))
                        }
                    }
                }
                return Ok(reply);
            }
            _ => {
                let mut reply = json!({"modules": self.registry.manifests().as_slice()});
                if !self.registry.failed().is_empty() {
                    reply["failed"] = json!(self.registry.failed());
                }
                return Ok(reply);
            }
        }
        let id = text(message, "module");
        let (Some(manifest), Some(path)) = (self.registry.manifest(&id), self.registry.ui(&id))
        else {
            return Err(Error::new(format!("Module \"{id}\" has no console part")));
        };
        let wasm = std::fs::read(path)
            .map_err(|e| Error::new(format!("Cannot read {}: {e}", path.display())))?;
        Ok(json!({
            "module": manifest.id,
            "ui": crate::core::modules::digest(&wasm),
            "wasm": base64::engine::general_purpose::STANDARD.encode(wasm),
        }))
    }
    pub async fn execute(
        &self,
        message: &Value,
        cancel: tokio_util::sync::CancellationToken,
        events: Option<tokio::sync::mpsc::Sender<Value>>,
    ) -> Result<Value> {
        self.execute_raw(message, cancel, events).await?.value()
    }
    /// A single resolution feeds both the real execution and its immutable snapshot.
    /// The module answer stays the JSON text the module wrote.
    pub async fn execute_raw(
        &self,
        message: &Value,
        cancel: tokio_util::sync::CancellationToken,
        events: Option<tokio::sync::mpsc::Sender<Value>>,
    ) -> Result<Executed> {
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
            .registry
            .params(&service)
            .and_then(|config| self.variable_store.resolve_for_history(config, raw, source));
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
        let history_requested = context.is_some();
        let result = if let Some(events) = events {
            let (tx, mut rx) = tokio::sync::mpsc::channel(128);
            let run = self.registry.stream_raw(
                &service,
                params.clone(),
                cancel.clone(),
                tx,
                history_requested,
            );
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
                .call_raw(&service, params.clone(), cancel.clone(), history_requested)
                .await
        };
        let Some(context) = context else {
            return result.map(|output| Executed {
                json: output.json,
                history: None,
                outcome: Some(output.outcome),
            });
        };
        let ended = history::now();
        let duration = clock.elapsed().as_millis() as u64;
        let saved =
            async {
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
                // The module judged its own answer (`Service::status` / `outcome`).
                let (status, outcome, response_text) =
                    match &result {
                        Ok(output) => (
                            output.status,
                            output.outcome.as_str(),
                            std::borrow::Cow::Borrowed(output.history_text.as_deref().ok_or_else(
                                || Error::new("The service returned no history text"),
                            )?),
                        ),
                        Err(e) => (
                            "error",
                            "ERROR",
                            std::borrow::Cow::Owned(history::response_text(&e.json())?),
                        ),
                    };
                let status = if cancel.is_cancelled() {
                    "stopped"
                } else {
                    status
                };
                let redactor = history::redactor(&params, &source, secrets);
                let text = format!(
                    "{}\n\n#responce {outcome}\n{}{}",
                    redactor.text(&source),
                    if output.is_empty() {
                        String::new()
                    } else {
                        format!("#terminal-output\n{output}\n")
                    },
                    response_text
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
            Ok(output) => Ok(Executed {
                json: output.json,
                history: Some(metadata),
                outcome: Some(output.outcome),
            }),
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
        self.dispatch_reply(message, cancel).await.into_value()
    }
    /// Module answers stay the text the module wrote; everything else is a value.
    pub async fn dispatch_reply(
        &self,
        message: &Value,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Reply {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let service = text(message, "service");
        let result = if message["type"] == "health" {
            Ok(json!({"protocolVersion":services_contracts::PROTOCOL_VERSION}))
        } else if service == "updates" {
            crate::core::updates::dispatch(message).await
        } else if service == "agent_models" {
            self.agent.catalog().await
        } else if service == "agent_sessions" {
            self.agent.sessions(message).await
        } else if service == "agent_skills" {
            self.agent.skills(message)
        } else if service == "history" {
            self.history.dispatch(message).await
        } else if service == "sessions" {
            (|| -> Result<Value> {
                let variables = self.variable_store.list()?;
                let hints: serde_json::Map<String, Value> = self
                    .registry
                    .manifests()
                    .iter()
                    .map(|manifest| {
                        (
                            manifest.id.clone(),
                            self.hints.get(&manifest.id, &manifest.console.hints)["text"].clone(),
                        )
                    })
                    .collect();
                Ok(json!({"variables":variables["profiles"]["var"],"hints":hints}))
            })()
        } else if service == "modules" {
            self.modules(message).await
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
                let default = self
                    .registry
                    .manifest(&service)
                    .map(|manifest| manifest.console.hints.as_str())
                    .unwrap_or("");
                Ok(self.hints.get(&service, default))
            }
        } else if message["type"] == "discover" {
            let json = self
                .registry
                .discover_raw(
                    &service,
                    message.get("params").unwrap_or(&json!({})),
                    cancel,
                )
                .await
                .unwrap_or_else(|e| e.json().to_string())
                .into();
            return Reply::result(
                &id,
                &Executed {
                    json,
                    history: None,
                    outcome: None,
                },
            );
        } else {
            match self.execute_raw(message, cancel, None).await {
                Ok(executed) => return Reply::result(&id, &executed),
                Err(error) => Err(error),
            }
        };
        Reply::Value(match result {
            Ok(result) => {
                let mut reply = json!({"id":id,"ok":true,"result":result});
                if let Some(metadata) = result.get("history") {
                    reply["history"] = metadata.clone();
                }
                reply
            }
            Err(error) => error_reply(&error, id),
        })
    }
}

/// The console reply to a failed request.
pub fn error_reply(error: &crate::core::error::Error, id: Value) -> Value {
    let mut reply = error.json();
    reply["id"] = id;
    reply["ok"] = json!(false);
    reply
}

/// A module answer as JSON text plus the history record of the run.
pub struct Executed {
    pub json: SharedStr,
    pub history: Option<Value>,
    /// The module's verdict on the answer (`OK`, `WARNING`, `ERROR`, …).
    pub outcome: Option<String>,
}

impl Executed {
    /// The answer with `history` and `outcome` added, as the console receives it.
    pub fn value(self) -> Result<Value> {
        let mut value: Value = serde_json::from_str(&self.json)
            .map_err(|e| crate::core::error::Error::new(format!("Invalid module response: {e}")))?;
        if let Some(fields) = value.as_object_mut() {
            if let Some(history) = self.history {
                fields.insert("history".into(), history);
            }
            if let Some(outcome) = self.outcome {
                fields.insert("outcome".into(), outcome.into());
            }
        }
        Ok(value)
    }

    /// The fields the console adds to an object answer: `,"history":…,"outcome":…`.
    fn fields(&self, history: Option<&str>) -> String {
        let mut fields = String::new();
        if let Some(history) = history {
            fields.push_str(",\"history\":");
            fields.push_str(history);
        }
        if let Some(outcome) = &self.outcome {
            fields.push_str(",\"outcome\":");
            fields.push_str(&Value::from(outcome.as_str()).to_string());
        }
        fields
    }
}

/// One message to the console.
pub enum Reply {
    Value(Value),
    /// Already serialized, in segments that share the module's answer; `id` labels the chunks
    /// of a large reply.
    Text {
        id: Value,
        parts: Vec<SharedStr>,
    },
}

impl From<Value> for Reply {
    fn from(value: Value) -> Self {
        Self::Value(value)
    }
}

impl Reply {
    /// `{"id":…,"ok":true,"result":…,"history":…}`.
    pub fn result(id: &Value, executed: &Executed) -> Self {
        Self::envelope(id, ",\"ok\":true,\"result\":", executed)
    }

    /// `{"id":…,"type":"exit","info":…,"history":…}`: the end of a streaming run.
    pub fn exit(id: &Value, executed: &Executed) -> Self {
        Self::envelope(id, ",\"type\":\"exit\",\"info\":", executed)
    }

    /// The text `json!` would serialize, without parsing or copying the answer; `history` and
    /// `outcome` also become the answer's last fields.
    fn envelope(id: &Value, head: &str, executed: &Executed) -> Self {
        let history = executed.history.as_ref().map(Value::to_string);
        let json = &executed.json;
        let start = json.len() - json.trim_start().len();
        let answer = json.trim();
        let open = format!("{{\"id\":{id}{head}");
        let mut close = String::new();
        let added = executed.fields(history.as_deref());
        let body = if !added.is_empty() && answer.starts_with('{') && answer.ends_with('}') {
            let fields = answer[..answer.len() - 1].trim_end();
            close.push_str(if fields == "{" { &added[1..] } else { &added });
            close.push('}');
            json.slice(start..start + fields.len())
        } else {
            json.slice(start..start + answer.len())
        };
        if let Some(history) = &history {
            close.push_str(",\"history\":");
            close.push_str(history);
        }
        close.push('}');
        Self::Text {
            id: id.clone(),
            parts: vec![open.into(), body, close.into()],
        }
    }

    pub fn into_value(self) -> Value {
        match self {
            Self::Value(value) => value,
            Self::Text { id, parts } => {
                let text: String = parts.iter().map(|part| &**part).collect();
                serde_json::from_str(&text).unwrap_or_else(|e| {
                    error_reply(
                        &crate::core::error::Error::new(format!("Invalid module response: {e}")),
                        id,
                    )
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The old path: parse the answer, add `history` and `outcome`, serialize the envelope.
    fn parsed(
        head: &str,
        id: &Value,
        answer: &str,
        history: Option<&Value>,
        outcome: Option<&str>,
    ) -> String {
        let mut answer: Value = serde_json::from_str(answer).unwrap();
        if let Some(fields) = answer.as_object_mut() {
            if let Some(history) = history {
                fields.insert("history".into(), history.clone());
            }
            if let Some(outcome) = outcome {
                fields.insert("outcome".into(), outcome.into());
            }
        }
        let mut reply = match head {
            "exit" => json!({"id":id,"type":"exit","info":answer}),
            _ => json!({"id":id,"ok":true,"result":answer}),
        };
        if let Some(history) = history {
            reply["history"] = history.clone();
        }
        reply.to_string()
    }

    #[test]
    fn passthrough_replies_match_the_parsed_ones_byte_for_byte() {
        let history = json!({"saved":true,"id":"0f6c-🦀"});
        for answer in [
            r#"{"status":200,"body":"🦀 \"x\"\n\u0000","headers":{"a":"b"},"n":1.5e300}"#,
            "{}",
            r#"{"rows":[[1,null,true]],"history":"stale","outcome":"OK"}"#,
        ] {
            for id in [json!(7), json!("req-1"), Value::Null] {
                for (history, outcome) in [None, Some(&history)].into_iter().flat_map(|history| {
                    [None, Some("WARN \"x\"")].map(|outcome| (history, outcome))
                }) {
                    let executed = Executed {
                        json: serde_json::from_str::<Value>(answer)
                            .unwrap()
                            .to_string()
                            .into(),
                        history: history.cloned(),
                        outcome: outcome.map(Into::into),
                    };
                    for head in ["ok", "exit"] {
                        let reply = match head {
                            "exit" => Reply::exit(&id, &executed),
                            _ => Reply::result(&id, &executed),
                        };
                        let Reply::Text { id: label, parts } = reply else {
                            panic!("a text reply")
                        };
                        let text: String = parts.iter().map(|part| &**part).collect();
                        assert_eq!(label, id);
                        let expected = parsed(head, &id, answer, history, outcome);
                        if answer.contains("history") && (history.is_some() || outcome.is_some()) {
                            // A stale field is shadowed by the later one, as JSON parsers read it.
                            assert_eq!(
                                serde_json::from_str::<Value>(&text).unwrap(),
                                serde_json::from_str::<Value>(&expected).unwrap()
                            );
                        } else {
                            assert_eq!(text, expected);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn non_object_answers_pass_through_unchanged() {
        let executed = Executed {
            json: "[1,2]".into(),
            history: Some(json!({"saved":false})),
            outcome: Some("OK".into()),
        };
        let Reply::Text { parts, .. } = Reply::result(&json!(1), &executed) else {
            panic!("a text reply")
        };
        let text: String = parts.iter().map(|part| &**part).collect();
        assert_eq!(
            text,
            r#"{"id":1,"ok":true,"result":[1,2],"history":{"saved":false}}"#
        );
        assert_eq!(
            Reply::Text {
                id: json!(3),
                parts: vec!["{".into()]
            }
            .into_value()["ok"],
            false
        );
    }
}
