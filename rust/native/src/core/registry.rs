use super::{
    error::{Error, Result},
    history, modules,
    text::SharedStr,
    variables::VariableStore,
};
use crate::host::HostEndpoint;
use serde_json::{Value, json};
use services_contracts::{ParamConfig, manifest::Manifest};
use services_module_api::{
    client::Module,
    error::Error as ModuleError,
    proto::module::RunStatus,
    transport::{EndpointArc, Status},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Registry {
    pub variables: VariableStore,
    modules: Arc<Vec<Module>>,
    /// What each module declared (`Module/Manifest`), in the same order as `modules`.
    manifests: Arc<Vec<Manifest>>,
    /// The console part of each package that ships one, by module id.
    uis: Arc<HashMap<String, PathBuf>>,
    /// Packages that failed to load: `{"package","module","error"}`.
    failed: Arc<Vec<Value>>,
    /// The modules folder.
    dir: PathBuf,
    /// The host functions every module gets.
    host: EndpointArc,
}

/// A module answer as the module serialized it; the host passes `json` through unparsed.
pub struct Output {
    pub json: SharedStr,
    /// History status: `success` / `error` / `stopped`.
    pub status: &'static str,
    pub outcome: String,
    /// Response text for history, when it was requested.
    pub history_text: Option<String>,
}

impl Output {
    /// An answer the host produced itself.
    fn host(value: Value, status: &'static str, outcome: &str, history: bool) -> Result<Self> {
        Ok(Self {
            history_text: history
                .then(|| history::response_text(&value))
                .transpose()?,
            json: value.to_string().into(),
            status,
            outcome: outcome.into(),
        })
    }

    fn module(response: services_module_api::proto::module::ExecuteResponse) -> Result<Self> {
        Ok(Self {
            json: SharedStr::from_utf8(response.result_json)
                .map_err(|e| Error::new(format!("Invalid module response: {e}")))?,
            status: RunStatus::try_from(response.status)
                .unwrap_or(RunStatus::Error)
                .as_str(),
            outcome: response.outcome,
            history_text: response.history_text,
        })
    }

    pub fn value(&self) -> Result<Value> {
        serde_json::from_str(&self.json)
            .map_err(|e| Error::new(format!("Invalid module response: {e}")))
    }
}

/// Loads a package. Its `package.json` must describe the module it holds, and a placed
/// package's folder is named by the module id; both are checked before any module code runs.
fn load_package(
    package: &modules::Package,
    host: &EndpointArc,
    placed: bool,
) -> std::result::Result<(Module, Manifest), String> {
    let described = services_contracts::package::Package::read(&package.dir)?;
    let folder = package
        .dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    if placed && folder != described.id.as_str() {
        return Err(format!(
            "the folder of module \"{}\" is named \"{folder}\"; it must be named by the module id",
            described.id
        ));
    }
    let endpoint = modules::load(&package.library(), host.clone())?;
    let module = Module::new(endpoint).map_err(|status| format!("no manifest: {status}"))?;
    let manifest = manifest(&module.manifest).map_err(|error| error.message)?;
    if described.id != manifest.id || described.version != manifest.version {
        return Err(format!(
            "{} describes {} {} but the module is {} {}",
            services_contracts::package::PACKAGE_FILE,
            described.id,
            described.version,
            manifest.id,
            manifest.version
        ));
    }
    Ok((module, manifest))
}

/// The full manifest of a module: its `manifest.json` plus the version and capabilities.
fn manifest(module: &services_module_api::proto::module::Manifest) -> Result<Manifest> {
    let invalid = |detail: String| {
        Error::new(format!(
            "Module {} has an invalid manifest: {detail}",
            module.id
        ))
    };
    let mut manifest: Manifest =
        serde_json::from_str(&module.spec_json).map_err(|e| invalid(e.to_string()))?;
    if manifest.id != module.id || manifest.name != module.name {
        return Err(invalid("id and name differ from Module/Manifest".into()));
    }
    manifest.version = module.version.clone();
    manifest.execute = module.execute;
    manifest.discover = module.discover;
    manifest.stream = module.stream;
    Ok(manifest)
}

/// The console error of a failed module call; the request is attached unless the module
/// already reported richer debug data or asked for none.
fn module_error(status: &Status, params: Value) -> Error {
    let error = ModuleError::from_status(status);
    let bare = error.debug.as_deref().is_some_and(Value::is_null);
    let mut converted = Error::new(error.message);
    converted.duration_ms = error.duration_ms;
    converted.debug = error.debug.filter(|debug| !debug.is_null());
    if bare || converted.debug.is_some() {
        converted
    } else {
        converted.with_debug(params)
    }
}

impl Registry {
    pub fn new(variables: VariableStore) -> Result<Self> {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let installed = rustls::crypto::ring::default_provider().install_default();
            // A concurrent runtime may have installed the process-wide provider first.
            if installed.is_err() && rustls::crypto::CryptoProvider::get_default().is_none() {
                return Err(Error::new("Failed to initialize the TLS crypto provider"));
            }
        }
        let data_dir = variables
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let dir = data_dir.join(modules::DIR);
        let packages = modules::scan(&dir);
        let host = HostEndpoint::new(data_dir)?.arc();
        let mut loaded = Vec::new();
        let mut uis = HashMap::new();
        let mut failed = Vec::new();
        for package in packages {
            match load_package(&package, &host, true) {
                Ok((module, mut manifest)) => {
                    if let Some(ui) = package.ui() {
                        let wasm = std::fs::read(&ui).map_err(|e| {
                            Error::new(format!("Cannot read {}: {e}", ui.display()))
                        })?;
                        manifest.ui = Some(modules::digest(&wasm));
                        uis.insert(manifest.id.clone(), ui);
                    }
                    loaded.push((module, manifest));
                }
                Err(error) => failed.push(json!({
                    "package": package.dir.display().to_string(),
                    "module": package.dir.file_name().unwrap_or_default().to_string_lossy(),
                    "error": error,
                })),
            }
        }
        loaded.sort_by_key(|(_, manifest)| manifest.console.order.unwrap_or(u32::MAX));
        let (modules, manifests) = loaded.into_iter().unzip();
        Ok(Self {
            variables,
            modules: Arc::new(modules),
            manifests: Arc::new(manifests),
            uis: Arc::new(uis),
            failed: Arc::new(failed),
            dir,
            host,
        })
    }

    /// Packages that failed to load: `{"package","module","error"}`, where `module` is the
    /// package folder's name.
    pub fn failed(&self) -> &[Value] {
        &self.failed
    }

    /// The modules folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Loads a package folder the way startup does, wherever the folder is; the manifest of
    /// its module.
    pub fn check(&self, dir: &Path) -> std::result::Result<Manifest, String> {
        let package = modules::Package {
            dir: dir.to_path_buf(),
        };
        load_package(&package, &self.host, false).map(|(_, manifest)| manifest)
    }

    /// The console part of a package's module.
    pub fn ui(&self, service: &str) -> Option<&Path> {
        self.uis.get(service).map(PathBuf::as_path)
    }

    fn module(&self, service: &str) -> Option<&Module> {
        self.modules
            .iter()
            .find(|module| module.manifest.id == service)
    }

    /// Manifests of the loaded modules, in the console's service order.
    pub fn manifests(&self) -> &Arc<Vec<Manifest>> {
        &self.manifests
    }

    pub fn manifest(&self, service: &str) -> Option<&Manifest> {
        self.manifests
            .iter()
            .find(|manifest| manifest.id == service)
    }

    /// The params `service` declares; resolution drops everything else.
    pub fn params(&self, service: &str) -> Result<&[ParamConfig]> {
        self.manifest(service)
            .map(|manifest| manifest.params.as_slice())
            .ok_or_else(|| Error::new(format!("Unknown service \"{service}\"")))
    }

    /// Whether executions of `service` stream events (Terminal).
    pub fn is_streaming(&self, service: &str) -> bool {
        self.module(service)
            .is_some_and(|module| module.manifest.stream)
    }

    pub fn resolve(&self, service: &str, params: &Value) -> Result<Value> {
        self.variables
            .resolve_params(self.params(service)?, params)
            .map_err(|e| e.with_debug(params.clone()))
    }

    pub async fn call(&self, service: &str, params: &Value) -> Result<Value> {
        self.call_cancellable(service, params, CancellationToken::new())
            .await
    }

    pub async fn call_cancellable(
        &self,
        service: &str,
        params: &Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let params = self.resolve(service, params)?;
        self.call_resolved(service, params, cancel).await
    }

    pub async fn call_resolved(
        &self,
        service: &str,
        params: Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        self.call_raw(service, params, cancel, false).await?.value()
    }

    pub async fn call_raw(
        &self,
        service: &str,
        params: Value,
        cancel: CancellationToken,
        history: bool,
    ) -> Result<Output> {
        if cancel.is_cancelled() {
            return Output::host(
                json!({"cancelled":true,"phase":"before-dispatch"}),
                "stopped",
                "Stopped",
                history,
            );
        }
        let module = match self.module(service) {
            Some(module) if module.manifest.execute => module,
            Some(module) if module.manifest.stream => {
                return Err(
                    Error::new(format!("Service \"{service}\" is streaming-only"))
                        .with_debug(params),
                );
            }
            _ => {
                return Err(Error::new(format!("Unknown service \"{service}\"")).with_debug(params));
            }
        };
        let params_json = serde_json::to_string(&params)?;
        match module.execute(params_json, history, cancel).await {
            Ok(response) => Output::module(response),
            Err(status) => Err(module_error(&status, params)),
        }
    }

    pub async fn discover(&self, service: &str, params: &Value) -> Result<Value> {
        self.discover_cancellable(service, params, CancellationToken::new())
            .await
    }

    pub async fn discover_cancellable(
        &self,
        service: &str,
        params: &Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let json = self.discover_raw(service, params, cancel).await?;
        serde_json::from_str(&json).map_err(|e| Error::new(format!("Invalid module response: {e}")))
    }

    /// The discovery result JSON as the module serialized it.
    pub async fn discover_raw(
        &self,
        service: &str,
        params: &Value,
        cancel: CancellationToken,
    ) -> Result<String> {
        let params = self.resolve(service, params)?;
        if cancel.is_cancelled() {
            return Ok(json!({"cancelled":true}).to_string());
        }
        let Some(module) = self
            .module(service)
            .filter(|module| module.manifest.discover)
        else {
            return Err(
                Error::new(format!("Service \"{service}\" doesn't support discovery"))
                    .with_debug(params),
            );
        };
        let params_json = serde_json::to_string(&params)?;
        module
            .discover(params_json, cancel)
            .await
            .map_err(|status| module_error(&status, params))
    }

    pub async fn stream(
        &self,
        service: &str,
        params: &Value,
        cancel: CancellationToken,
        events: mpsc::Sender<Value>,
    ) -> Result<Value> {
        let params = self.resolve(service, params)?;
        self.stream_resolved(service, params, cancel, events).await
    }

    pub async fn stream_resolved(
        &self,
        service: &str,
        params: Value,
        cancel: CancellationToken,
        events: mpsc::Sender<Value>,
    ) -> Result<Value> {
        self.stream_raw(service, params, cancel, events, false)
            .await?
            .value()
    }

    pub async fn stream_raw(
        &self,
        service: &str,
        params: Value,
        cancel: CancellationToken,
        events: mpsc::Sender<Value>,
        history: bool,
    ) -> Result<Output> {
        let Some(module) = self.module(service).filter(|module| module.manifest.stream) else {
            return Err(Error::new(format!(
                "Service \"{service}\" is not streaming"
            )));
        };
        let params_json = serde_json::to_string(&params)?;
        let on_event = |json: String| {
            let events = events.clone();
            async move {
                let event = serde_json::from_str(&json).unwrap_or(Value::String(json));
                let _ = events.send(event).await;
            }
        };
        match module.stream(params_json, history, cancel, on_event).await {
            Ok(response) => Output::module(response),
            Err(status) => Err(module_error(&status, params)),
        }
    }
}
