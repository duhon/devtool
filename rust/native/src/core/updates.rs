//! Application updates: Settings → Modules offers the application ([`APPLICATION_ID`]) from
//! sources like a module, and installing it runs the installed updater on the archive.

use super::{
    error::{Error, Result},
    sources::{self, Sources},
};
use serde_json::{Value, json};
use services_contracts::package::APPLICATION_ID;
use std::path::{Path, PathBuf};

fn native_directory() -> Result<PathBuf> {
    std::env::current_exe()
        .map_err(Error::from)?
        .parent()
        .and_then(|path| path.parent())
        .map(PathBuf::from)
        .ok_or_else(|| Error::new("Installed application directory is unavailable"))
}

/// The repository the running application was installed from (its `distribution.json`).
pub fn installed_repository() -> Option<String> {
    let path = native_directory().ok()?.join("distribution.json");
    let metadata: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (metadata["application"] == "ServicesDevTools")
        .then(|| metadata["repository"].as_str().map(str::to_owned))
        .flatten()
}

/// `{op:"install", source, version}` installs version `version` of the application from the
/// source `source`: `{installedVersion, reload:true}`.
pub async fn dispatch(sources: &Sources, data: &Path, message: &Value) -> Result<Value> {
    if message["op"] != "install" {
        return Err(Error::new("Unknown update operation"));
    }
    let slug = message["source"]
        .as_str()
        .ok_or_else(|| Error::new("Which source?"))?;
    let version = message["version"]
        .as_str()
        .ok_or_else(|| Error::new("Which version?"))?;
    let temporary = tempfile::Builder::new()
        .prefix("services-download-")
        .tempdir()
        .map_err(Error::from)?;
    let (release, archive) = sources.application(slug, version, temporary.path()).await?;
    let downloaded = archive.starts_with(temporary.path());
    let child = tokio::process::Command::new(native_directory()?.join("bin/services-install"))
        .arg(if downloaded {
            "update-downloaded"
        } else {
            "update"
        })
        .arg(&archive)
        .arg(&release.sha256)
        .arg(&release.version)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| Error::new(format!("Could not start the installed updater: {error}")))?;
    if downloaded {
        // The independent installer now owns cleanup if Chrome disconnects.
        let _download_directory = temporary.keep();
    }
    let output = child.wait_with_output().await.map_err(Error::from)?;
    if !output.status.success() {
        return Err(Error::new(format!(
            "Update installation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    sources::set_module_source(data, APPLICATION_ID, Some(slug))?;
    Ok(json!({"installedVersion": release.version, "reload": true}))
}
