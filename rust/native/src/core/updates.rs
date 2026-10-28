use super::error::{Error, Result};
use serde_json::{Value, json};
use services_contracts::updates::{Release, Source};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncWriteExt;

const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

fn trusted_download_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}

fn download_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .https_only(true)
        .gzip(false)
        .brotli(false)
        .deflate(false)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 || !trusted_download_url(attempt.url()) {
                attempt.error("Update redirect is outside the trusted HTTPS release hosts")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(Error::from)
}

async fn download(
    client: &reqwest::Client,
    release: &Release,
    path: &Path,
    maximum: u64,
) -> Result<()> {
    let mut response = client
        .get(&release.archive_url)
        .send()
        .await
        .map_err(|error| {
            Error::new(format!(
                "Native archive download failed: {}",
                error.without_url()
            ))
        })?;
    if !response.status().is_success() {
        return Err(Error::new(format!(
            "Native archive download failed (HTTP {}). Only public GitHub release assets are supported.",
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > maximum)
    {
        return Err(Error::new("Downloaded archive exceeds the size limit"));
    }
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).await.map_err(Error::from)?;
    services_installer::quarantine::mark_download(path).map_err(|error| {
        Error::new(format!(
            "Could not preserve downloaded file provenance: {error}"
        ))
    })?;
    let mut digest = Sha256::new();
    let mut size = 0u64;
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        Error::new(format!(
            "Native archive download interrupted: {}",
            error.without_url()
        ))
    })? {
        size = size
            .checked_add(chunk.len() as u64)
            .filter(|size| *size <= maximum)
            .ok_or_else(|| Error::new("Downloaded archive exceeds the size limit"))?;
        digest.update(&chunk);
        file.write_all(&chunk).await.map_err(Error::from)?;
    }
    file.flush().await.map_err(Error::from)?;
    file.sync_all().await.map_err(Error::from)?;
    if format!("{:x}", digest.finalize()) != release.sha256 {
        return Err(Error::new(
            "Native download checksum mismatch; nothing was installed",
        ));
    }
    Ok(())
}

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
            let temporary = if message.get("archivePath").is_none() {
                Some(
                    tempfile::Builder::new()
                        .prefix("services-download-")
                        .tempdir()
                        .map_err(Error::from)?,
                )
            } else {
                None
            };
            let archive = if let Some(temporary) = &temporary {
                let path = temporary.path().join(&release.archive);
                download(&download_client()?, &release, &path, MAX_ARCHIVE_BYTES).await?;
                path
            } else {
                PathBuf::from(
                    message["archivePath"]
                        .as_str()
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| Error::new("Downloaded archive path is missing"))?,
                )
            };
            if !archive.is_absolute()
                || archive.file_name().and_then(|name| name.to_str())
                    != Some(release.archive.as_str())
                || !archive.is_file()
            {
                return Err(Error::new(
                    "Downloaded update is not an absolute archive file path",
                ));
            }
            let child =
                tokio::process::Command::new(native_directory()?.join("bin/services-install"))
                    .arg(if temporary.is_some() {
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
                    .map_err(|error| {
                        Error::new(format!("Could not start the installed updater: {error}"))
                    })?;
            if let Some(temporary) = temporary {
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
            Ok(json!({"installedVersion":release.version,"reload":true}))
        }
        _ => Err(Error::new("Unknown update operation")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::StatusCode, response::Response};

    fn release(url: String, bytes: &[u8]) -> Release {
        Release {
            schema_version: 1,
            application: "ServicesDevTools".into(),
            version: "2.6.0".into(),
            tag: "devtool-v2.6.0".into(),
            commit: "a".repeat(40),
            repository: "duhon/devtool".into(),
            branch: "main".into(),
            platform: "darwin".into(),
            arch: "arm64".into(),
            minimum_macos: "13.5".into(),
            archive: services_contracts::updates::ARCHIVE.into(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
            archive_url: url,
            checksums_url: String::new(),
            installer_url: String::new(),
        }
    }

    async fn server(status: StatusCode, chunked: bool) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/archive", listener.local_addr().unwrap());
        let app = Router::new().fallback(move || async move {
            let body = if chunked {
                Body::from_stream(futures_util::stream::iter([
                    Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"fixture-")),
                    Ok(bytes::Bytes::from_static(b"archive")),
                ]))
            } else {
                Body::from("fixture-archive")
            };
            Response::builder().status(status).body(body).unwrap()
        });
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, task)
    }

    #[test]
    fn downloads_only_follow_approved_https_asset_hosts() {
        for value in [
            "https://github.com/duhon/devtool/releases/download/devtool-v2.6.0/archive",
            "https://release-assets.githubusercontent.com/asset?signature=fixture",
            "https://objects.githubusercontent.com/asset",
        ] {
            assert!(trusted_download_url(&value.parse().unwrap()), "{value}");
        }
        for value in [
            "http://github.com/archive",
            "https://github.com.evil.invalid/archive",
            "https://foreign.invalid/archive",
            "https://user:password@github.com/archive",
            "https://github.com:444/archive",
            "https://127.0.0.1/archive",
        ] {
            assert!(!trusted_download_url(&value.parse().unwrap()), "{value}");
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn native_download_verifies_streamed_bytes_and_private_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let (url, server) = server(StatusCode::OK, true).await;
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("archive");
        let client = reqwest::Client::new();
        download(&client, &release(url, b"fixture-archive"), &path, 32)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"fixture-archive");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        server.abort();
        drop(temporary);
        assert!(!path.exists());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn native_download_rejects_bad_hash_oversize_and_http_errors() {
        let client = reqwest::Client::new();
        for (status, chunked, limit, hash, expected) in [
            (
                StatusCode::OK,
                false,
                32,
                b"wrong".as_slice(),
                "checksum mismatch",
            ),
            (
                StatusCode::OK,
                false,
                8,
                b"fixture-archive".as_slice(),
                "size limit",
            ),
            (
                StatusCode::OK,
                true,
                8,
                b"fixture-archive".as_slice(),
                "size limit",
            ),
            (
                StatusCode::NOT_FOUND,
                false,
                32,
                b"fixture-archive".as_slice(),
                "HTTP 404",
            ),
        ] {
            let (url, server) = server(status, chunked).await;
            let temporary = tempfile::tempdir().unwrap();
            let path = temporary.path().join("archive");
            let error = download(&client, &release(url, hash), &path, limit)
                .await
                .unwrap_err();
            assert!(error.message.contains(expected), "{error}");
            server.abort();
            drop(temporary);
            assert!(!path.exists());
        }
    }
}
