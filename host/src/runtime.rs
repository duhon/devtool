use crate::core::{
    error::{Result, text},
    registry::Registry,
    sources::{self, Sources},
};
use serde_json::{Value, json};
use std::path::PathBuf;
#[derive(Clone)]
pub struct Runtime {
    pub operations: crate::core::operations::Operations,
    pub agent: crate::agent::Agent,
    pub registry: Registry,
    pub history: crate::core::history::History,
    pub sources: Sources,
    pub drivers: crate::drivers::Drivers,
}
impl Runtime {
    /// Secrets the Agent must not see: sensitive environment variables and the session's
    /// variables (`__consoleVariables`, from the Variables panel) with a sensitive name.
    pub fn agent_session_sensitive_values(&self, params: &Value) -> Result<Vec<String>> {
        let mut values = self.agent_sensitive_values()?;
        if let Some(variables) = params.get("__consoleVariables") {
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
        let registry = Registry::new(&path)?;
        Ok(Self {
            operations: Default::default(),
            agent: crate::agent::Agent::new(path.join(".agent")),
            registry,
            history: crate::core::history::History::new(path.join(".console-history.sqlite3")),
            sources: Sources::new(path),
            drivers: crate::drivers::Drivers::new()?,
        })
    }
    /// What the `history.insert` command of a module (the History panel) runs:
    /// `{"service":"history","op":"insert","checkId","sessionId","title","module","started",
    /// "ended","status","text"}` saves a run; `defer:true` keeps it back like a repeat's
    /// earlier runs.
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
    /// `op:"remove"` with a `module` folder change the modules folder; `forget:true` also deletes the module's history. This host keeps the modules it loaded; a new host process loads the change.
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
                    let history = self.history.clone();
                    let forgotten = tokio::task::spawn_blocking(move || history.forget(&name))
                        .await
                        .map_err(|e| Error::new(e.to_string()))
                        .and_then(|result| result);
                    match forgotten {
                        Ok(_) => reply["forgotten"] = json!(true),
                        Err(e) => {
                            reply["warning"] = json!(format!(
                                "The module was removed, but its history was not deleted: {}",
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
        } else if service == "modules" {
            self.modules(message).await
        } else if service == "env" {
            Ok(Value::Object(
                std::env::vars()
                    .map(|(name, value)| (name, json!(value)))
                    .collect(),
            ))
        } else if service == "sources" && message["op"] == "read" {
            sources::read(self.sources.data(), &text(message, "name"))
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
            sources::dispatch(&self.sources, message, &installed).await
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
