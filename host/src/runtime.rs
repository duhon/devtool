use crate::core::{
    error::{Result, text},
    hints::Hints,
    registry::Registry,
    sources::{self, Sources},
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
    pub sources: Sources,
    pub drivers: crate::drivers::Drivers,
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
            .ok_or_else(|| crate::Error::new("SERVICES_DATA_DIR is not set"))?;
        Self::with_data_dir(PathBuf::from(base))
    }
    pub fn with_data_dir(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Err(error) = sources::migrate_team_folder(&path) {
            eprintln!("Team skills folder migration failed: {}", error.message);
        }
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
            templates: crate::core::templates::Templates::new(
                path.join(".console-templates.json"),
                // Modules declare no template sections yet.
                Default::default(),
            ),
            history: crate::core::history::History::new(path.join(".console-history.sqlite3")),
            sources: Sources::new(path),
            drivers: crate::drivers::Drivers::new()?,
        })
    }
    /// Interim until the Variables panel module: `{"service":"vars","op":"resolve","value",
    /// "source","title","variables"}` resolves `{{…}}` in every string of `value` with the
    /// session `variables`; `source` and `title` come back resolved with secrets redacted,
    /// ready for history. `"mode":"json"` escapes each value of a `value` string as the
    /// content of a JSON string.
    fn resolve(&self, message: &Value) -> Result<Value> {
        if message["mode"] == "json"
            && let Some(text) = message["value"].as_str()
        {
            let references: Vec<&str> = text
                .match_indices("{{")
                .filter_map(|(start, _)| {
                    let end = text[start..].find("}}")?;
                    Some(&text[start..start + end + 2])
                })
                .collect();
            let mut each = message.clone();
            each["value"] = json!(references);
            each["mode"] = Value::Null;
            let mut resolved = self.resolve(&each)?;
            let mut out = text.to_owned();
            for (reference, value) in references
                .iter()
                .zip(resolved["value"].as_array().cloned().unwrap_or_default())
            {
                let quoted = serde_json::to_string(value.as_str().unwrap_or(""))?;
                out = out.replace(reference, &quoted[1..quoted.len() - 1]);
            }
            resolved["value"] = json!(out);
            return Ok(resolved);
        }
        let config = [services_contracts::ParamConfig {
            name: "value".into(),
            kind: "text".into(),
            required: false,
            multi: false,
            default: None,
            resolve: Some("text".into()),
            keys: Default::default(),
        }];
        let mut params = json!({"value": message["value"]});
        if let Some(variables) = message.get("variables") {
            params["__consoleVariables"] = variables.clone();
        }
        let (resolved, snapshot) = self.variable_store.resolve_for_history(
            &config,
            &params,
            message["source"].as_str(),
        )?;
        let (source, secrets) = snapshot?;
        let redactor = crate::core::history::redactor(&resolved, &source, secrets);
        Ok(json!({
            "value": resolved["value"],
            "source": redactor.text(&source),
            "title": redactor.text(message["title"].as_str().unwrap_or("")),
        }))
    }

    /// Interim until the History panel module: `{"service":"history","op":"insert","checkId",
    /// "sessionId","title","module","started","ended","status","text"}` saves a run;
    /// `defer:true` keeps it back like a repeat's earlier runs.
    async fn insert_history(&self, message: &Value) -> Result<Value> {
        use crate::core::{error::Error, history};
        let number = |key: &str| {
            message[key]
                .as_u64()
                .ok_or_else(|| Error::new(format!("{key} must be a number")))
        };
        let (started, ended) = (number("started")?, number("ended")?);
        let snapshot = history::Snapshot {
            check_id: history::uuid(message, "checkId")?,
            session_id: history::uuid(message, "sessionId")?,
            title: text(message, "title"),
            service: text(message, "module"),
            started,
            ended,
            duration: ended.saturating_sub(started),
            status: match message["status"].as_str() {
                Some(status @ ("success" | "error" | "stopped")) => status.into(),
                _ => return Err(Error::new("status must be success, error or stopped")),
            },
            text: text(message, "text"),
        };
        if snapshot.text.len() > history::MAX_BYTES {
            return Err(Error::new("Snapshot exceeds the 100 MiB history quota"));
        }
        if message["defer"] == true {
            self.history.defer(snapshot);
            return Ok(json!({"saved": false, "deferred": true}));
        }
        let id = self.history.save(snapshot).await?;
        Ok(json!({"saved": true, "id": id}))
    }

    /// `{"service":"modules"}`: the wasm modules the App runs (`wasm`: SDK manifest plus
    /// `digest`) and the packages that failed to load;
    /// `op:"wasm"` with a `module` id: the wasm module as base64;
    /// `op:"install"` with the `source`, `module` and `version` of a source's offer, and
    /// `op:"remove"` with a `module` folder change the modules folder; `forget:true` also deletes the module's history, local templates and
    /// hints. This host keeps the modules it loaded; a new host process loads the change.
    async fn modules(&self, message: &Value) -> Result<Value> {
        use crate::core::{error::Error, install};
        use base64::Engine;
        match message["op"].as_str() {
            Some("wasm") => {
                let id = text(message, "module");
                let module = self
                    .registry
                    .wasm()
                    .iter()
                    .find(|module| module.manifest.id == id)
                    .ok_or_else(|| Error::new(format!("Module \"{id}\" is not a wasm module")))?;
                let wasm = std::fs::read(&module.path).map_err(|e| {
                    Error::new(format!("Cannot read {}: {e}", module.path.display()))
                })?;
                Ok(json!({
                    "module": id,
                    "digest": crate::core::modules::digest(&wasm),
                    "wasm": base64::engine::general_purpose::STANDARD.encode(wasm),
                }))
            }
            Some("install") => {
                let source = message["source"]
                    .as_str()
                    .ok_or_else(|| Error::new("Modules are installed from sources"))?;
                let archive = self
                    .sources
                    .package(source, &text(message, "module"), &text(message, "version"))
                    .await?;
                let registry = self.registry.clone();
                let installed = tokio::task::spawn_blocking(move || {
                    install::install(registry.dir(), &archive, |dir| registry.check(dir))
                })
                .await
                .map_err(|e| Error::new(format!("Install failed: {e}")))?
                .map_err(Error::new)?;
                let manifest = installed.manifest;
                sources::set_module_source(self.sources.data(), &manifest.id, Some(source))?;
                Ok(json!({
                    "installed": {"id": manifest.id, "name": manifest.name, "version": manifest.version},
                    "replaced": installed.replaced,
                }))
            }
            Some("remove") => {
                let name = text(message, "module");
                install::remove(self.registry.dir(), &name).map_err(Error::new)?;
                sources::remove_module_source(self.sources.data(), &name)?;
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
                Ok(reply)
            }
            _ => {
                let wasm: Vec<Value> = self
                    .registry
                    .wasm()
                    .iter()
                    .map(|module| {
                        let mut manifest = json!(module.manifest);
                        manifest["digest"] = json!(module.digest);
                        manifest
                    })
                    .collect();
                let mut reply = json!({"wasm": wasm});
                if !self.registry.failed().is_empty() {
                    reply["failed"] = json!(self.registry.failed());
                }
                Ok(reply)
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
            Ok(json!({
                "protocolVersion": services_contracts::PROTOCOL_VERSION,
                "drivers": crate::drivers::versions(),
            }))
        } else if service == "drivers" {
            return match self.drivers.call(message, cancel).await {
                Ok(result) => json!({"id": id, "ok": true, "result": result}),
                Err(failure) => failure.reply(&id),
            };
        } else if service == "updates" {
            crate::core::updates::dispatch(&self.sources, self.sources.data(), message).await
        } else if service == "agent_models" {
            self.agent.catalog().await
        } else if service == "agent_sessions" {
            self.agent.sessions(message).await
        } else if service == "history" {
            if message["op"] == "insert" {
                self.insert_history(message).await
            } else {
                self.history.dispatch(message).await
            }
        } else if service == "sessions" {
            (|| -> Result<Value> {
                let variables = self.variable_store.list()?;
                let hints: serde_json::Map<String, Value> = self
                    .registry
                    .wasm()
                    .iter()
                    .map(|module| {
                        let id = module.manifest.id.clone();
                        let text = self.hints.get(&id, "")["text"].clone();
                        (id, text)
                    })
                    .collect();
                Ok(json!({"variables":variables["profiles"]["var"],"hints":hints}))
            })()
        } else if service == "modules" {
            self.modules(message).await
        } else if service == "sources" {
            let installed: Vec<sources::Installed> = self
                .registry
                .wasm()
                .iter()
                .map(|module| sources::Installed {
                    id: module.manifest.id.clone(),
                    name: Some(module.manifest.name.clone()),
                    description: Some(module.manifest.description.clone())
                        .filter(|description| !description.is_empty()),
                    version: Some(module.manifest.version.clone()),
                    error: None,
                })
                .chain(
                    self.registry
                        .failed()
                        .iter()
                        .map(|failed| sources::Installed {
                            id: text(failed, "module"),
                            name: None,
                            description: None,
                            version: None,
                            error: failed["error"].as_str().map(str::to_owned),
                        }),
                )
                .collect();
            let local = || sources::Local {
                dictionary: self.dictionary_store.local().unwrap_or_default(),
                templates: self.templates.local().unwrap_or_default(),
                hints: self.hints.local(),
            };
            sources::dispatch(&self.sources, message, local, &installed).await
        } else if service == "vars" && message["op"] == "resolve" {
            self.resolve(message)
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
                Some("reset") => self.templates.reset(message),
                _ => Err(crate::core::error::Error::new(
                    "Unknown templates operation",
                )),
            }
        } else if service == "hints" {
            let service = text(message, "serviceName");
            let default = "";
            match message["op"].as_str() {
                Some("save") => self.hints.save(&service, &text(message, "text"), default),
                Some("reset") => self
                    .hints
                    .forget(&service)
                    .map(|()| self.hints.get(&service, default)),
                _ => Ok(self.hints.get(&service, default)),
            }
        } else {
            Err(crate::core::error::Error::new(format!(
                "Unknown service \"{service}\""
            )))
        };
        match result {
            Ok(result) => {
                let mut reply = json!({"id":id,"ok":true,"result":result});
                if let Some(metadata) = result.get("history") {
                    reply["history"] = metadata.clone();
                }
                reply
            }
            Err(error) => error_reply(&error, id),
        }
    }
}

/// The console reply to a failed request.
pub fn error_reply(error: &crate::core::error::Error, id: Value) -> Value {
    let mut reply = error.json();
    reply["id"] = id;
    reply["ok"] = json!(false);
    reply
}
