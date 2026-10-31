//! A module package: `<modules>/<id>/` with `package.json`, `module.dylib`, an optional
//! `ui.wasm`, optional `RELEASE_NOTES.md` and an optional Agent skill in
//! `skills/module-<id>/`. `package.json` says what the package is without loading its library.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// The package description inside a package folder.
pub const PACKAGE_FILE: &str = "package.json";
/// The native part of a package on macOS.
pub const LIBRARY_FILE: &str = "module.dylib";
/// The optional console part of a package.
pub const UI_FILE: &str = "ui.wasm";
/// The optional changes of the package's versions, newest first (`## X.Y.Z` sections).
pub const NOTES_FILE: &str = "RELEASE_NOTES.md";
/// The folder of the package's Agent skill: `skills/module-<id>/SKILL.md` and its files.
pub const SKILLS_DIR: &str = "skills";
/// A package as one file, `<id>-<version>[-<platform>].module.tgz`: a gzip'd tar with the
/// package folder's files at its root and nothing else.
pub const ARCHIVE_SUFFIX: &str = ".module.tgz";
/// The extension a file picker filters package files by.
pub const ARCHIVE_EXTENSION: &str = ".tgz";
/// The files a package archive may hold at its root; `package.json` and the library are
/// required. Besides them only the skill folder ([`SKILLS_DIR`]).
pub const ARCHIVE_FILES: &[&str] = &[PACKAGE_FILE, LIBRARY_FILE, UI_FILE, NOTES_FILE];

/// The file name of a package archive.
pub fn archive_name(package: &Package) -> String {
    match &package.platform {
        Some(platform) => format!(
            "{}-{}-{platform}{ARCHIVE_SUFFIX}",
            package.id, package.version
        ),
        None => format!("{}-{}{ARCHIVE_SUFFIX}", package.id, package.version),
    }
}

/// The platform this build runs on, as packages name it: `macos-arm64`, `linux-x86_64`, ….
pub fn host_platform() -> String {
    let os = std::env::consts::OS;
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// The name of a module's Agent skill (its folder in [`SKILLS_DIR`] and its frontmatter
/// `name`); skill names use hyphens only.
pub fn skill_name(id: &str) -> String {
    format!("module-{}", id.replace('_', "-"))
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
    "sources",
    "templates",
    "updates",
    "vars",
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    /// The module id; the package folder has the same name.
    pub id: String,
    /// `X.Y.Z`, the version the module reports in its manifest.
    pub version: String,
    /// The oldest application version (`X.Y.Z`) the module works with.
    #[serde(rename = "minCore", default, skip_serializing_if = "Option::is_none")]
    pub min_core: Option<String>,
    /// The platform the library was built for ([`host_platform`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

impl Package {
    /// The checked `package.json` of a package folder.
    pub fn read(dir: &Path) -> Result<Self, String> {
        let path = dir.join(PACKAGE_FILE);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// A checked `package.json`.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let package: Self =
            serde_json::from_slice(bytes).map_err(|e| format!("invalid package: {e}"))?;
        check_id(&package.id)?;
        version(&package.version)?;
        if let Some(min_core) = &package.min_core {
            version(min_core).map_err(|_| format!("minCore \"{min_core}\" must be X.Y.Z"))?;
        }
        Ok(package)
    }

    /// Why this package can't run on `platform` with application version `core`.
    pub fn unsupported(&self, core: &str, platform: &str) -> Option<String> {
        if let Some(built) = self.platform.as_deref().filter(|built| *built != platform) {
            return Some(format!(
                "module {} {} is built for {built}, this computer is {platform}",
                self.id, self.version
            ));
        }
        let min_core = self.min_core.as_deref()?;
        matches!((version(min_core), version(core)), (Ok(min), Ok(core)) if core < min).then(|| {
            format!(
                "module {} {} needs Services DevTools {min_core} or newer; this is {core}",
                self.id, self.version
            )
        })
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

/// Package files: [`ARCHIVE_FILES`] and the skill folder of a package folder in one gzip'd tar.
#[cfg(feature = "archive")]
pub mod archive {
    use super::{ARCHIVE_FILES, LIBRARY_FILE, PACKAGE_FILE, SKILLS_DIR};
    use std::{
        fs,
        io::{self, Read},
        path::{Component, Path, PathBuf},
    };

    /// How much a package file may unpack to.
    pub const MAX_UNPACKED: u64 = 512 * 1024 * 1024;
    /// How large `package.json` or `RELEASE_NOTES.md` may be when read without unpacking.
    const MAX_DESCRIPTION: u64 = 1024 * 1024;

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
        let mut skills = Vec::new();
        files(&dir.join(SKILLS_DIR), Path::new(SKILLS_DIR), &mut skills)?;
        skills.sort();
        for (path, name) in skills {
            builder
                .append_path_with_name(&path, &name)
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        builder
            .into_inner()
            .and_then(|gzip| gzip.finish())
            .map_err(|e| format!("Cannot pack {}: {e}", dir.display()))
    }

    /// The files under `dir` with their archive names under `name`.
    fn files(dir: &Path, name: &Path, out: &mut Vec<(PathBuf, PathBuf)>) -> Result<(), String> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Ok(());
        };
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = entry.path();
            let named = name.join(entry.file_name());
            if path.is_dir() {
                files(&path, &named, out)?;
            } else if path.is_file() {
                out.push((path, named));
            }
        }
        Ok(())
    }

    /// A skill file's path below [`SKILLS_DIR`]: plain names only, at least a folder and a file.
    fn skill_path(name: &str) -> Option<PathBuf> {
        let path = Path::new(name).strip_prefix(SKILLS_DIR).ok()?;
        let plain = path.components().all(|component| {
            matches!(component, Component::Normal(part) if !part.to_string_lossy().starts_with("._"))
        });
        (plain && path.components().count() >= 2).then(|| Path::new(SKILLS_DIR).join(path))
    }

    /// Unpacks a package file into the empty folder `into`: only [`ARCHIVE_FILES`] at the
    /// archive root and files in skill folders, as regular files; `package.json` and the
    /// library are required.
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
                    .trim_end_matches('/')
                    .to_owned()
            };
            // Pax headers, the root folder, folders of skills and macOS AppleDouble files
            // carry no package data.
            if kind.is_pax_global_extensions()
                || kind.is_pax_local_extensions()
                || (kind.is_dir()
                    && (name.is_empty()
                        || name == SKILLS_DIR
                        || Path::new(&name).starts_with(SKILLS_DIR)))
                || Path::new(&name)
                    .file_name()
                    .is_some_and(|file| file.to_string_lossy().starts_with("._"))
            {
                continue;
            }
            let target = if ARCHIVE_FILES.contains(&name.as_str()) {
                PathBuf::from(&name)
            } else if let Some(path) = skill_path(&name) {
                path
            } else {
                return Err(invalid(format!(
                    "unexpected \"{name}\"; a package file holds {} at its root and skill folders in {SKILLS_DIR}/",
                    ARCHIVE_FILES.join(", ")
                )));
            };
            if !kind.is_file() {
                return Err(invalid(format!("\"{name}\" is not a regular file")));
            }
            let target = into.join(target);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| invalid(format!("\"{name}\": {e}")))?;
            }
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

    /// A root file of a package file (`package.json`, `RELEASE_NOTES.md`) without unpacking it.
    pub fn read(archive: &[u8], file: &str) -> Result<Option<Vec<u8>>, String> {
        let invalid = |detail: String| format!("Invalid package file: {detail}");
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive));
        for entry in archive.entries().map_err(|e| invalid(e.to_string()))? {
            let mut entry = entry.map_err(|e| invalid(e.to_string()))?;
            let name = {
                let path = entry.path().map_err(|e| invalid(e.to_string()))?;
                path.strip_prefix("./")
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned()
            };
            if name != file || !entry.header().entry_type().is_file() {
                continue;
            }
            let mut bytes = Vec::new();
            (&mut entry)
                .take(MAX_DESCRIPTION + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| invalid(format!("\"{name}\": {e}")))?;
            if bytes.len() as u64 > MAX_DESCRIPTION {
                return Err(invalid(format!("\"{name}\" is larger than 1 MiB")));
            }
            return Ok(Some(bytes));
        }
        Ok(None)
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
            ..Package::default()
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
    fn a_package_names_the_oldest_application_and_the_platform_it_needs() {
        let package = Package {
            id: "rest".into(),
            version: "1.0.0".into(),
            min_core: Some("3.2.0".into()),
            platform: Some("macos-arm64".into()),
        };
        assert_eq!(archive_name(&package), "rest-1.0.0-macos-arm64.module.tgz");
        assert_eq!(package.unsupported("3.2.0", "macos-arm64"), None);
        assert_eq!(package.unsupported("10.0.0", "macos-arm64"), None);
        assert!(
            package
                .unsupported("3.1.9", "macos-arm64")
                .unwrap()
                .contains("needs Services DevTools 3.2.0 or newer")
        );
        assert!(
            package
                .unsupported("3.2.0", "linux-x86_64")
                .unwrap()
                .contains("built for macos-arm64")
        );
        assert_eq!(skill_name("my_redis"), "module-my-redis");
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
                version: "1.2.3".into(),
                ..Package::default()
            })
        );
        std::fs::write(
            &file,
            r#"{"id":"redis","version":"1.2.3","minCore":"3.2","platform":"macos-arm64"}"#,
        )
        .unwrap();
        assert!(Package::read(&dir).unwrap_err().contains("minCore"));
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
        std::fs::write(package.join(NOTES_FILE), b"## 1.0.0\nFirst\n").unwrap();
        std::fs::write(package.join("notes.txt"), b"not packed").unwrap();
        let skill = package.join("skills/module-redis/references");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("../SKILL.md"), b"skill").unwrap();
        std::fs::write(skill.join("syntax.md"), b"syntax").unwrap();
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
        assert_eq!(
            std::fs::read(unpacked.join("skills/module-redis/references/syntax.md")).unwrap(),
            b"syntax"
        );
        assert_eq!(
            super::archive::read(&file, NOTES_FILE).unwrap().unwrap(),
            b"## 1.0.0\nFirst\n"
        );
        assert_eq!(super::archive::read(&file, UI_FILE).unwrap(), None);

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
        // tar refuses to write `..`; the name goes into the header as is.
        let raw_entry = |builder: &mut tar::Builder<Vec<u8>>, name: &str| {
            let mut header = tar::Header::new_gnu();
            header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(1);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, &b"x"[..]).unwrap();
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
                "skill-file-at-skills-root",
                tar(&|b| file_entry(b, "skills/SKILL.md", regular)),
                "unexpected \"skills/SKILL.md\"",
            ),
            (
                "skill-escape",
                tar(&|b| raw_entry(b, "skills/x/../../module.dylib")),
                "unexpected",
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
