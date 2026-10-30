use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;
use serde_json::Value;
use services_contracts::updates::{Source, version};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

struct DownloadDirectory(PathBuf);

impl Drop for DownloadDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!(
                "Could not clean native download {}: {error}",
                self.0.display()
            );
        }
    }
}

pub fn install_downloaded_archive(
    home: &Path,
    archive: &Path,
    expected_sha: &str,
    expected_version: &str,
) -> Result<super::Locations> {
    let directory = archive
        .parent()
        .context("Native download directory is missing")?;
    let metadata = fs::symlink_metadata(directory)?;
    ensure!(
        archive.is_absolute()
            && archive.file_name().and_then(|name| name.to_str())
                == Some(services_contracts::updates::ARCHIVE)
            && metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("services-download-"))
            && directory
                .parent()
                .context("Native download parent is missing")?
                .canonicalize()?
                == std::env::temp_dir().canonicalize()?,
        "Refusing ownership of an unexpected native download directory"
    );
    let _download = DownloadDirectory(directory.to_path_buf());
    install_archive(home, archive, expected_sha, expected_version)
}

pub fn install_archive(
    home: &Path,
    archive: &Path,
    expected_sha: &str,
    expected_version: &str,
) -> Result<super::Locations> {
    version(expected_version).map_err(anyhow::Error::msg)?;
    ensure!(
        expected_sha.len() == 64
            && expected_sha
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "Invalid update checksum"
    );
    let temporary = tempfile::Builder::new()
        .prefix("services-update-")
        .tempdir()?;
    let verified = temporary.path().join("archive.tar.gz");
    let mut file = fs::File::open(archive).context("Open downloaded update archive")?;
    let mut copy = fs::File::create(&verified)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut compressed = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        compressed += count as u64;
        ensure!(
            compressed <= 2 * 1024 * 1024 * 1024,
            "Downloaded update exceeds the size limit"
        );
        digest.update(&buffer[..count]);
        copy.write_all(&buffer[..count])?;
    }
    ensure!(
        format!("{:x}", digest.finalize()) == expected_sha,
        "Update checksum mismatch; nothing was installed"
    );
    drop(copy);
    super::quarantine::copy(archive, &verified)?;
    let mut package = tar::Archive::new(GzDecoder::new(fs::File::open(&verified)?));
    let mut bytes = 0u64;
    for (count, entry) in package.entries()?.enumerate() {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        ensure!(
            count < 100_000
                && path.components().next() == Some(Component::Normal("ServicesDevTools".as_ref()))
                && path
                    .components()
                    .all(|component| matches!(component, Component::Normal(_))),
            "Unsafe update archive path"
        );
        ensure!(
            entry.header().entry_type().is_file() || entry.header().entry_type().is_dir(),
            "Update archive contains a link or unsupported file type"
        );
        bytes = bytes
            .checked_add(entry.size())
            .context("Update archive size overflow")?;
        ensure!(
            bytes <= 2 * 1024 * 1024 * 1024,
            "Update archive exceeds the extraction limit"
        );
        ensure!(
            entry.unpack_in(temporary.path())?,
            "Unsafe update archive entry"
        );
        super::quarantine::copy(archive, &temporary.path().join(&path))?;
    }
    let payload = temporary.path().join("ServicesDevTools");
    let metadata = super::validate_payload(&payload)?;
    ensure!(
        metadata["version"] == expected_version,
        "Downloaded package version differs from the approved release"
    );
    super::install_payload(home, &payload, true)
}

pub(super) fn validate_upgrade(
    paths: &super::Locations,
    payload: &Path,
    metadata: &Value,
) -> Result<()> {
    let installed: Value = serde_json::from_slice(
        &fs::read(paths.base.join("extension/native/distribution.json"))
            .context("Installed update metadata is missing; reinstall the application once")?,
    )?;
    let source = Source {
        repository: installed["repository"]
            .as_str()
            .context("Installed update repository is missing")?
            .into(),
        branch: installed["branch"]
            .as_str()
            .context("Installed update branch is missing")?
            .into(),
        current_version: installed["version"]
            .as_str()
            .context("Installed version is missing")?
            .into(),
    };
    source.validate().map_err(anyhow::Error::msg)?;
    ensure!(
        version(
            metadata["version"]
                .as_str()
                .context("Update version is missing")?
        )
        .map_err(anyhow::Error::msg)?
            > version(&source.current_version).map_err(anyhow::Error::msg)?,
        "Update must be newer than the installed version"
    );
    ensure!(
        metadata["repository"] == installed["repository"]
            && metadata["branch"] == installed["branch"],
        "Downloaded package update source differs from the installed application"
    );
    let current_manifest: Value =
        serde_json::from_slice(&fs::read(paths.base.join("extension/manifest.json"))?)?;
    let next_manifest: Value =
        serde_json::from_slice(&fs::read(payload.join("extension/manifest.json"))?)?;
    for field in [
        "permissions",
        "host_permissions",
        "optional_permissions",
        "optional_host_permissions",
    ] {
        let previous = current_manifest[field]
            .as_array()
            .cloned()
            .unwrap_or_default();
        ensure!(
            next_manifest[field]
                .as_array()
                .is_none_or(|values| values.iter().all(|value| previous.contains(value))),
            "This update requests new Chrome permissions; install it manually to approve them"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_native_archive_is_cleaned_by_the_installer() {
        let directory = tempfile::Builder::new()
            .prefix("services-download-")
            .tempdir()
            .unwrap();
        let archive = directory.path().join(services_contracts::updates::ARCHIVE);
        fs::write(&archive, b"not an archive").unwrap();
        let directory = directory.keep();
        let home = tempfile::tempdir().unwrap();
        let sha = format!("{:x}", Sha256::digest(b"not an archive"));
        assert!(install_downloaded_archive(home.path(), &archive, &sha, "2.6.0").is_err());
        assert!(!directory.exists());
    }

    #[test]
    fn ordinary_archive_directories_are_never_claimed_or_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join(services_contracts::updates::ARCHIVE);
        fs::write(&archive, b"keep").unwrap();
        assert!(
            install_downloaded_archive(directory.path(), &archive, &"a".repeat(64), "2.6.0")
                .is_err()
        );
        assert_eq!(fs::read(&archive).unwrap(), b"keep");
    }
}
