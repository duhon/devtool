//! A module package: `<modules>/<id>/` with `package.json`, `module.dylib` and an optional
//! `ui.wasm`. `package.json` says what the package is without loading its library.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// The package description inside a package folder.
pub const PACKAGE_FILE: &str = "package.json";
/// The native part of a package on macOS.
pub const LIBRARY_FILE: &str = "module.dylib";
/// The optional console part of a package.
pub const UI_FILE: &str = "ui.wasm";
/// A package as one file, `<id>-<version>.module.tgz`: a gzip'd tar with the package folder's
/// files at its root and nothing else.
pub const ARCHIVE_SUFFIX: &str = ".module.tgz";
/// The extension a file picker filters package files by.
pub const ARCHIVE_EXTENSION: &str = ".tgz";
/// What a package archive may hold; `package.json` and the library are required.
pub const ARCHIVE_FILES: &[&str] = &[PACKAGE_FILE, LIBRARY_FILE, UI_FILE];

/// The file name of a package archive.
pub fn archive_name(package: &Package) -> String {
    format!("{}-{}{ARCHIVE_SUFFIX}", package.id, package.version)
}

/// Ids the host answers itself (`{"service": id}`); a module can't take them.
pub const RESERVED_IDS: &[&str] = &[
    "agent",
    "agent_models",
    "agent_sessions",
    "agent_skills",
    "dictionary",
    "hints",
    "history",
    "modules",
    "sessions",
    "templates",
    "updates",
    "vars",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    /// The module id; the package folder has the same name.
    pub id: String,
    /// `X.Y.Z`, the version the module reports in its manifest.
    pub version: String,
}

impl Package {
    /// The checked `package.json` of a package folder.
    pub fn read(dir: &Path) -> Result<Self, String> {
        let path = dir.join(PACKAGE_FILE);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let package: Self = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{}: invalid package: {e}", path.display()))?;
        check_id(&package.id)?;
        version(&package.version)?;
        Ok(package)
    }

    /// Whether this package's version is newer than `other`'s.
    pub fn newer_than(&self, other: &Self) -> bool {
        matches!((version(&self.version), version(&other.version)), (Ok(a), Ok(b)) if a > b)
    }
}

/// A module id names its folder, console mode and settings: a lowercase letter, then
/// lowercase letters, digits, `-` or `_`; never one the host answers itself.
pub fn check_id(id: &str) -> Result<(), String> {
    let mut chars = id.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase())
        || !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(format!(
            "module id \"{id}\" must be a lowercase letter followed by lowercase letters, digits, - or _"
        ));
    }
    if RESERVED_IDS.contains(&id) {
        return Err(format!("module id \"{id}\" is reserved by the host"));
    }
    Ok(())
}

/// A package version `X.Y.Z`.
pub fn version(value: &str) -> Result<[u64; 3], String> {
    let invalid = || format!("module version \"{value}\" must be X.Y.Z");
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
        })
    {
        return Err(invalid());
    }
    let mut result = [0; 3];
    for (index, part) in parts.iter().enumerate() {
        result[index] = part.parse().map_err(|_| invalid())?;
    }
    Ok(result)
}

/// Package files: [`ARCHIVE_FILES`] of a package folder in one gzip'd tar.
#[cfg(feature = "archive")]
pub mod archive {
    use super::{ARCHIVE_FILES, LIBRARY_FILE, PACKAGE_FILE};
    use std::{
        fs,
        io::{self, Read},
        path::Path,
    };

    /// How much a package file may unpack to.
    pub const MAX_UNPACKED: u64 = 512 * 1024 * 1024;

    /// The package file of a package folder; symlinked files are packed as their targets.
    pub fn pack(dir: &Path) -> Result<Vec<u8>, String> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        builder.mode(tar::HeaderMode::Deterministic);
        for name in ARCHIVE_FILES {
            let path = dir.join(name);
            if !path.is_file() {
                if [PACKAGE_FILE, LIBRARY_FILE].contains(name) {
                    return Err(format!("{} is missing", path.display()));
                }
                continue;
            }
            builder
                .append_path_with_name(&path, name)
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        builder
            .into_inner()
            .and_then(|gzip| gzip.finish())
            .map_err(|e| format!("Cannot pack {}: {e}", dir.display()))
    }

    /// Unpacks a package file into the empty folder `into`: only [`ARCHIVE_FILES`] at the
    /// archive root, as regular files; `package.json` and the library are required.
    pub fn unpack(archive: &[u8], into: &Path) -> Result<(), String> {
        let invalid = |detail: String| format!("Invalid package file: {detail}");
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive));
        let mut left = MAX_UNPACKED;
        for entry in archive.entries().map_err(|e| invalid(e.to_string()))? {
            let mut entry = entry.map_err(|e| invalid(e.to_string()))?;
            let kind = entry.header().entry_type();
            let name = {
                let path = entry.path().map_err(|e| invalid(e.to_string()))?;
                path.strip_prefix("./")
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned()
            };
            // Pax headers, the root folder and macOS AppleDouble files carry no package data.
            if kind.is_pax_global_extensions()
                || kind.is_pax_local_extensions()
                || (kind.is_dir() && name.is_empty())
                || name.starts_with("._")
            {
                continue;
            }
            if !ARCHIVE_FILES.contains(&name.as_str()) {
                return Err(invalid(format!(
                    "unexpected \"{name}\"; a package file holds {} at its root",
                    ARCHIVE_FILES.join(", ")
                )));
            }
            if !kind.is_file() {
                return Err(invalid(format!("\"{name}\" is not a regular file")));
            }
            let target = into.join(&name);
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| invalid(format!("\"{name}\": {e}")))?;
            let copied = io::copy(&mut (&mut entry).take(left + 1), &mut file)
                .map_err(|e| invalid(format!("\"{name}\": {e}")))?;
            if copied > left {
                return Err(invalid("it unpacks to more than 512 MiB".into()));
            }
            left -= copied;
        }
        for required in [PACKAGE_FILE, LIBRARY_FILE] {
            if !into.join(required).is_file() {
                return Err(invalid(format!("it has no {required}")));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_module_id_is_a_plain_lowercase_name_the_host_does_not_answer_itself() {
        for id in ["rest", "grpc", "my-redis", "kafka_2", "s3"] {
            assert_eq!(check_id(id), Ok(()), "{id}");
        }
        for id in [
            "", "Rest", "1rest", "-rest", "re st", "rest/x", "../x", ".rest", "rést",
        ] {
            assert!(
                check_id(id)
                    .unwrap_err()
                    .contains("must be a lowercase letter"),
                "{id}"
            );
        }
        for id in ["vars", "modules", "agent", "history"] {
            assert_eq!(
                check_id(id),
                Err(format!("module id \"{id}\" is reserved by the host"))
            );
        }
    }

    #[test]
    fn versions_compare_numerically_and_anything_else_is_invalid() {
        let package = |version: &str| Package {
            id: "rest".into(),
            version: version.into(),
        };
        assert!(package("3.0.10").newer_than(&package("3.0.9")));
        assert!(package("10.0.0").newer_than(&package("9.9.9")));
        assert!(!package("3.0.0").newer_than(&package("3.0.0")));
        assert!(!package("3.0.0").newer_than(&package("3.0.1")));
        for invalid in ["3.0", "3.0.0-beta", "03.0.0", "a.b.c", ""] {
            assert!(version(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn a_package_file_is_read_and_checked() {
        let dir = std::env::temp_dir().join(format!("services-package-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(PACKAGE_FILE);
        std::fs::write(&file, r#"{"id":"redis","version":"1.2.3"}"#).unwrap();
        assert_eq!(
            Package::read(&dir),
            Ok(Package {
                id: "redis".into(),
                version: "1.2.3".into()
            })
        );
        std::fs::write(&file, r#"{"id":"vars","version":"1.2.3"}"#).unwrap();
        assert!(Package::read(&dir).unwrap_err().contains("reserved"));
        std::fs::write(&file, r#"{"id":"redis","version":"1.2"}"#).unwrap();
        assert!(Package::read(&dir).unwrap_err().contains("X.Y.Z"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(Package::read(&dir).is_err());
    }

    #[cfg(feature = "archive")]
    #[test]
    fn a_package_file_holds_only_the_package_files_at_its_root() {
        use super::archive::{pack, unpack};
        let root = std::env::temp_dir().join(format!("services-archive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let folder = |name: &str| {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        };
        let package = folder("package");
        std::fs::write(
            package.join(PACKAGE_FILE),
            r#"{"id":"redis","version":"1.0.0"}"#,
        )
        .unwrap();
        std::fs::write(package.join(LIBRARY_FILE), b"library").unwrap();
        std::fs::write(package.join("notes.txt"), b"not packed").unwrap();
        assert_eq!(
            archive_name(&Package::read(&package).unwrap()),
            "redis-1.0.0.module.tgz"
        );
        assert!(ARCHIVE_SUFFIX.ends_with(ARCHIVE_EXTENSION));
        let file = pack(&package).unwrap();
        let unpacked = folder("unpacked");
        unpack(&file, &unpacked).unwrap();
        assert_eq!(
            std::fs::read(unpacked.join(LIBRARY_FILE)).unwrap(),
            b"library"
        );
        assert!(!unpacked.join(UI_FILE).exists());
        assert!(!unpacked.join("notes.txt").exists());

        let tar = |build: &dyn Fn(&mut tar::Builder<Vec<u8>>)| {
            let mut builder = tar::Builder::new(Vec::new());
            build(&mut builder);
            let mut gzip =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            std::io::Write::write_all(&mut gzip, &builder.into_inner().unwrap()).unwrap();
            gzip.finish().unwrap()
        };
        let file_entry = |builder: &mut tar::Builder<Vec<u8>>, name: &str, kind| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_size(if kind == tar::EntryType::Regular {
                1
            } else {
                0
            });
            header.set_mode(0o644);
            header.set_cksum();
            let data: &[u8] = if kind == tar::EntryType::Regular {
                b"x"
            } else {
                b""
            };
            builder.append_data(&mut header, name, data).unwrap();
        };
        let regular = tar::EntryType::Regular;
        let mac = tar(&|b| {
            file_entry(b, "./", tar::EntryType::Directory);
            file_entry(b, "./._package.json", regular);
            file_entry(b, "./package.json", regular);
            file_entry(b, "./module.dylib", regular);
        });
        unpack(&mac, &folder("mac")).unwrap();
        for (name, file, error) in [
            (
                "nested",
                tar(&|b| file_entry(b, "redis/package.json", regular)),
                "unexpected \"redis/package.json\"",
            ),
            (
                "linked",
                tar(&|b| {
                    let mut header = tar::Header::new_gnu();
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_size(0);
                    b.append_link(&mut header, "module.dylib", "/usr/lib/libc.dylib")
                        .unwrap();
                }),
                "\"module.dylib\" is not a regular file",
            ),
            (
                "repeated",
                tar(&|b| {
                    file_entry(b, "package.json", regular);
                    file_entry(b, "package.json", regular);
                }),
                "\"package.json\": File exists",
            ),
            (
                "libraryless",
                tar(&|b| file_entry(b, "package.json", regular)),
                "it has no module.dylib",
            ),
            ("garbage", b"not a package".to_vec(), "Invalid package file"),
        ] {
            let error_text = unpack(&file, &folder(name)).unwrap_err();
            assert!(error_text.contains(error), "{name}: {error_text}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
