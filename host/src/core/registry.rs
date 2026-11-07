use super::{
    error::{Error, Result},
    modules,
    variables::VariableStore,
};
use serde_json::{Value, json};
use services_sdk::manifest::Manifest;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// The installed modules: wasm packages the App runs.
#[derive(Clone)]
pub struct Registry {
    pub variables: VariableStore,
    /// Modules the App runs, in tab order.
    wasm: Arc<Vec<Wasm>>,
    /// Packages that failed to load: `{"package","module","error"}`.
    failed: Arc<Vec<Value>>,
    /// The modules folder.
    dir: PathBuf,
}

/// A module the App runs: its SDK manifest (`package.json`) and `module.wasm`.
#[derive(Clone, Debug)]
pub struct Wasm {
    pub manifest: Manifest,
    pub path: PathBuf,
    /// The cache key of the App's compiled copy.
    pub digest: String,
}

/// Checks a wasm package before the App gets it: its `package.json` is the SDK manifest of
/// the module, the App can run it and this Host has the drivers it calls.
fn load_wasm(package: &modules::Package, placed: bool) -> std::result::Result<Wasm, String> {
    let path = package.dir.join(services_contracts::package::PACKAGE_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let manifest = Manifest::parse(&text)?;
    services_contracts::package::check_id(&manifest.id)?;
    let folder = package
        .dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    if placed && folder != manifest.id.as_str() {
        return Err(format!(
            "the folder of module \"{}\" is named \"{folder}\"; it must be named by the module id",
            manifest.id
        ));
    }
    let drivers = serde_json::from_value(crate::drivers::versions()).unwrap_or_default();
    manifest.check(env!("CARGO_PKG_VERSION"), &drivers)?;
    let path = package.wasm().ok_or("module.wasm is missing")?;
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Wasm {
        manifest,
        digest: modules::digest(&bytes),
        path,
    })
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
        let dir = variables
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
            .join(modules::DIR);
        let mut failed = Vec::new();
        let mut wasm = Vec::new();
        for package in modules::scan(&dir) {
            match load_wasm(&package, true) {
                Ok(module) => wasm.push(module),
                Err(error) => failed.push(json!({
                    "package": package.dir.display().to_string(),
                    "module": package.dir.file_name().unwrap_or_default().to_string_lossy(),
                    "error": error,
                })),
            }
        }
        wasm.sort_by_key(|module| module.manifest.order.unwrap_or(u32::MAX));
        Ok(Self {
            variables,
            wasm: Arc::new(wasm),
            failed: Arc::new(failed),
            dir,
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

    /// Checks a package folder the way startup does, wherever the folder is; the manifest of
    /// its module.
    pub fn check(&self, dir: &Path) -> std::result::Result<Manifest, String> {
        let package = modules::Package {
            dir: dir.to_path_buf(),
        };
        load_wasm(&package, false).map(|module| module.manifest)
    }

    /// Modules the App runs, in tab order.
    pub fn wasm(&self) -> &[Wasm] {
        &self.wasm
    }
}
