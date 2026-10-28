use serde::{Deserialize, Serialize};

pub const ARCHIVE: &str = "services-macos-arm64.tar.gz";
pub const TAG_PREFIX: &str = "devtool-v";

pub fn version(value: &str) -> Result<[u16; 3], String> {
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
        })
    {
        return Err(format!("Invalid application version: {value}"));
    }
    let mut result = [0; 3];
    for (index, part) in parts.iter().enumerate() {
        result[index] = part
            .parse()
            .map_err(|_| format!("Application version is outside Chrome's range: {value}"))?;
    }
    if result == [0; 3] {
        return Err("Application version cannot be zero".into());
    }
    Ok(result)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub repository: String,
    pub branch: String,
    pub current_version: String,
}

impl Source {
    pub fn validate(&self) -> Result<(), String> {
        version(&self.current_version)?;
        let parts: Vec<_> = self.repository.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|part| {
                part.is_empty()
                    || *part == "."
                    || *part == ".."
                    || !part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            })
        {
            return Err("Update source is not configured with a valid GitHub repository".into());
        }
        if self.branch != "devtool" {
            return Err("This update source does not use the devtool release channel".into());
        }
        Ok(())
    }

    pub fn manifest_url(&self) -> Result<String, String> {
        self.validate()?;
        Ok(format!(
            "https://github.com/{}/releases/latest/download/version.json",
            self.repository
        ))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub schema_version: u32,
    pub application: String,
    pub version: String,
    pub tag: String,
    pub commit: String,
    pub repository: String,
    pub branch: String,
    pub platform: String,
    pub arch: String,
    #[serde(rename = "minimumMacOS")]
    pub minimum_macos: String,
    pub archive: String,
    pub sha256: String,
    pub archive_url: String,
    pub checksums_url: String,
    pub installer_url: String,
}

impl Release {
    pub fn validate(&self, source: &Source) -> Result<(), String> {
        source.validate()?;
        version(&self.version)?;
        let base = format!(
            "https://github.com/{}/releases/download/{TAG_PREFIX}{}",
            source.repository, self.version
        );
        if self.schema_version != 1
            || self.application != "ServicesDevTools"
            || self.repository != source.repository
            || self.branch != source.branch
            || self.platform != "darwin"
            || self.arch != "arm64"
            || self.minimum_macos != "13.5"
            || self.tag != format!("{TAG_PREFIX}{}", self.version)
            || self.archive != ARCHIVE
            || self.archive_url != format!("{base}/{ARCHIVE}")
            || self.checksums_url != format!("{base}/checksums.txt")
            || self.installer_url != format!("{base}/install.sh")
            || !hex(&self.sha256, 64)
            || !hex(&self.commit, 40)
        {
            return Err(
                "Release metadata does not match the trusted application update source".into(),
            );
        }
        Ok(())
    }

    pub fn newer_than(&self, current: &str) -> Result<bool, String> {
        Ok(version(&self.version)? > version(current)?)
    }
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Source {
        Source {
            repository: "owner/repository".into(),
            branch: "devtool".into(),
            current_version: "2.5.22".into(),
        }
    }

    fn release() -> Release {
        let base = "https://github.com/owner/repository/releases/download/devtool-v3.0.0";
        Release {
            schema_version: 1,
            application: "ServicesDevTools".into(),
            version: "3.0.0".into(),
            tag: "devtool-v3.0.0".into(),
            commit: "a".repeat(40),
            repository: "owner/repository".into(),
            branch: "devtool".into(),
            platform: "darwin".into(),
            arch: "arm64".into(),
            minimum_macos: "13.5".into(),
            archive: ARCHIVE.into(),
            sha256: "b".repeat(64),
            archive_url: format!("{base}/{ARCHIVE}"),
            checksums_url: format!("{base}/checksums.txt"),
            installer_url: format!("{base}/install.sh"),
        }
    }

    #[test]
    fn versions_are_numeric_exact_and_chrome_compatible() {
        assert!(version("3.0.0").unwrap() > version("2.99.99").unwrap());
        assert!(version("2.5.10").unwrap() > version("2.5.9").unwrap());
        for value in [
            "0.0.0",
            "2.05.1",
            "2.5",
            "2.5.1.1",
            "2.5.65536",
            "2.5.1-beta",
            " 2.5.1",
        ] {
            assert!(version(value).is_err(), "{value}");
        }
    }

    #[test]
    fn metadata_is_bound_to_the_source_version_platform_hash_and_exact_urls() {
        let source = source();
        let release = release();
        assert!(release.validate(&source).is_ok());
        assert!(release.newer_than(&source.current_version).unwrap());
        assert!(!release.newer_than("3.0.0").unwrap());
        let original = serde_json::to_value(&release).unwrap();
        for (field, value) in [
            ("schemaVersion", serde_json::json!(2)),
            ("repository", serde_json::json!("other/repository")),
            ("branch", serde_json::json!("main")),
            ("arch", serde_json::json!("x64")),
            ("tag", serde_json::json!("devtool-v9.0.0")),
            ("sha256", serde_json::json!("b".repeat(63))),
            ("commit", serde_json::json!("not-a-commit")),
            (
                "archiveUrl",
                serde_json::json!("https://github.com.evil.invalid/package"),
            ),
            (
                "installerUrl",
                serde_json::json!("https://github.com/other/install.sh"),
            ),
        ] {
            let mut changed = original.clone();
            changed[field] = value;
            assert!(
                serde_json::from_value::<Release>(changed)
                    .unwrap()
                    .validate(&source)
                    .is_err(),
                "{field}"
            );
        }
    }
}
