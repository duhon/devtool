use super::error::{Error, Result};
use serde_json::{Value, json};
use services_contracts::updates::{Release, Source};
use std::path::PathBuf;

fn native_directory() -> Result<PathBuf> {
    std::env::current_exe()
        .map_err(Error::from)?
        .parent()
        .and_then(|path| path.parent())
        .map(PathBuf::from)
        .ok_or_else(|| Error::new("Installed application directory is unavailable"))
}

fn source() -> Result<Source> {
    let path = native_directory()?.join("distribution.json");
    let metadata: Value = serde_json::from_slice(&std::fs::read(path).map_err(|error| {
        Error::new(format!(
            "Installed update metadata is unavailable; reinstall the application once: {error}"
        ))
    })?)
    .map_err(|error| Error::new(format!("Invalid installed update metadata: {error}")))?;
    if metadata["application"] != "ServicesDevTools" {
        return Err(Error::new("Installed application identity is invalid"));
    }
    Ok(Source {
        repository: metadata["repository"]
            .as_str()
            .ok_or_else(|| Error::new("Installed update repository is missing"))?
            .into(),
        branch: metadata["branch"]
            .as_str()
            .ok_or_else(|| Error::new("Installed update branch is missing"))?
            .into(),
        current_version: env!("CARGO_PKG_VERSION").into(),
    })
}

pub async fn dispatch(message: &Value) -> Result<Value> {
    let source = source()?;
    match message["op"].as_str() {
        Some("info") => serde_json::to_value(source).map_err(|error| Error::new(error.to_string())),
        Some("install") => {
            let release: Release = serde_json::from_value(message["release"].clone())
                .map_err(|error| Error::new(format!("Invalid approved update: {error}")))?;
            release.validate(&source).map_err(Error::new)?;
            if !release
                .newer_than(&source.current_version)
                .map_err(Error::new)?
            {
                return Err(Error::new(
                    "Update must be newer than the running application",
                ));
            }
            let archive = message["archivePath"]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| Error::new("Downloaded archive path is missing"))?;
            let archive = PathBuf::from(archive);
            if !archive.is_absolute()
                || archive.file_name().and_then(|name| name.to_str())
                    != Some(release.archive.as_str())
                || !archive.is_file()
            {
                return Err(Error::new(
                    "Downloaded update is not an absolute archive file path",
                ));
            }
            let output =
                tokio::process::Command::new(native_directory()?.join("bin/services-install"))
                    .arg("update")
                    .arg(&archive)
                    .arg(&release.sha256)
                    .arg(&release.version)
                    .output()
                    .await
                    .map_err(|error| {
                        Error::new(format!("Could not start the installed updater: {error}"))
                    })?;
            if !output.status.success() {
                return Err(Error::new(format!(
                    "Update installation failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            Ok(json!({"installedVersion":release.version,"reload":true}))
        }
        _ => Err(Error::new("Unknown update operation")),
    }
}
