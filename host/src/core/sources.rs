//! Sources: an ordered list of named places that supply module packages and shared
//! configuration (Dictionary sets, templates, hints and Agent skills): GitHub
//! repositories and local folders. A source is laid out as
//!
//! ```text
//! dictionary.json          {"order"?: [...], "sets": {"<set>": {"<VAR>": value}}} (or "profiles")
//! templates.json           {"<module>": {"<section>": "<templates text>"}}
//! hints/<module>.md        the module's hints
//! skills/<name>/SKILL.md   Agent skills, with their files
//! modules/*.module.tgz     module packages, `<id>-<version>[-<platform>].module.tgz`
//! distribution/            the application as a build leaves it: version.json, the archive
//!                          and release-notes.txt (GitHub repositories use releases tagged
//!                          devtool-v<version>)
//! ```
//!
//! A GitHub source is a folder of a branch, read through GitHub's API. A check lists the
//! folder's trees and reads a package file's `package.json` and notes only when the file (its
//! git blob) is new, then forgets its bytes; installing downloads it again and checks its
//! git hash.
//!
//! Checking for changes is automatic; the user applies them. The applied copy of a source
//! lives in `<data>/sources/<slug>/`, its skills renamed `<slug>-<name>`. Configuration is
//! layered: later sources override earlier ones, and the user's own changes, which the
//! application keeps apart, override them all. An update replaces the applied copy and
//! never merges.

use super::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use services_contracts::{
    package::{self, APPLICATION_ID, Package, archive},
    updates::{self, Release},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const SETTINGS_FILE: &str = ".sources.json";
const TOKEN_FILE: &str = ".sources-token";
/// The folder of applied sources in the data folder.
pub const DIR: &str = "sources";
const META_FILE: &str = ".source.json";
const CHECK_FILE: &str = ".check.json";
const LOCK_FILE: &str = ".lock";
const MODULES_FILE: &str = ".module-sources.json";
pub const DICTIONARY: &str = "dictionary.json";
pub const TEMPLATES: &str = "templates.json";
pub const HINTS: &str = "hints";
/// The field of dictionary.json with its sets.
const DICTIONARY_SETS: &str = "sets";
pub const SKILLS: &str = "skills";
const MODULES: &str = "modules";
const DISTRIBUTION: &str = "distribution";
const RELEASE_FILE: &str = "version.json";
/// The platform the application is built for.
const APPLICATION_PLATFORM: &str = "macos-arm64";
const APPLICATION_NAME: &str = "Services DevTools";
const APPLICATION_DESCRIPTION: &str = "The application: the extension, its native host and the installer. Installing a newer version replaces it and keeps the modules, sources and your data; it can't be removed here.";
/// The application's release notes, for the version running.
const APPLICATION_NOTES: &str = include_str!("../../../RELEASE_NOTES.md");
const MAX_RELEASE: u64 = 64 * 1024;
const MAX_ARCHIVE: u64 = 1024 * 1024 * 1024;
/// Slugs that would make skill names look like the application's own.
const RESERVED: &[&str] = &["module", "services", "local"];
/// The repository the standard modules are released from.
pub const STANDARD: &str = "duhon/devtool";
/// The branch of [`STANDARD`] that holds the latest package of each standard module.
const STANDARD_BRANCH: &str = "modules";
const MAX_FILE: u64 = 1024 * 1024;
const MAX_FILES: usize = 1000;
const MAX_PACKAGE: u64 = 256 * 1024 * 1024;
const API: &str = "https://api.github.com";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Location {
    /// `owner/repo`, optionally a branch (the default branch otherwise) and a folder in it.
    Github {
        repository: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// A local folder the user keeps up to date (a checkout, a synced drive).
    Folder { path: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub name: String,
    #[serde(flatten)]
    pub location: Location,
}

impl Source {
    pub fn slug(&self) -> String {
        slug(&self.name)
    }

    fn standard() -> Self {
        Self {
            name: "Standard".into(),
            location: Location::Github {
                repository: STANDARD.into(),
                branch: Some(STANDARD_BRANCH.into()),
                path: None,
            },
        }
    }

    /// Standard as saved before 3.3.0, when its modules were releases: the default branch.
    fn migrate(self) -> Self {
        match &self.location {
            Location::Github {
                repository,
                branch: None,
                path: None,
            } if repository.eq_ignore_ascii_case(STANDARD) => Self {
                location: Source::standard().location,
                ..self
            },
            _ => self,
        }
    }

    fn describe(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        value["slug"] = json!(self.slug());
        value
    }
}

/// The prefix of a source's skills: its name in lowercase Latin letters, digits and hyphens.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

#[derive(Serialize, Deserialize)]
struct Settings {
    version: u32,
    sources: Vec<Source>,
}

/// The configured sources in order; Standard alone until the user changes the list.
pub fn sources(data: &Path) -> Result<Vec<Source>> {
    match fs::read(data.join(SETTINGS_FILE)) {
        Ok(bytes) => serde_json::from_slice::<Settings>(&bytes)
            .map(|settings| settings.sources.into_iter().map(Source::migrate).collect())
            .map_err(|e| Error::new(format!("Invalid sources settings: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![Source::standard()]),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    if !private {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o644))?;
    }
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|e| Error::new(format!("Cannot write {}: {}", path.display(), e.error)))?;
    Ok(())
}

fn write_sources(data: &Path, sources: &[Source]) -> Result<()> {
    write_atomic(
        &data.join(SETTINGS_FILE),
        &serde_json::to_vec_pretty(&Settings {
            version: 1,
            sources: sources.to_vec(),
        })?,
        false,
    )
}

fn plain(text: &str, extra: &[char]) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c) || extra.contains(&c))
        && text
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn validate(sources: &[Source]) -> Result<()> {
    let mut slugs = BTreeSet::new();
    for source in sources {
        let name = &source.name;
        if name.trim().is_empty() {
            return Err(Error::new("Every source needs a name"));
        }
        let slug = slug(name);
        if slug.is_empty() {
            return Err(Error::new(format!(
                "Source name \"{name}\" needs Latin letters or digits: they prefix its skills"
            )));
        }
        if RESERVED.contains(&slug.as_str()) {
            return Err(Error::new(format!(
                "Source name \"{name}\" is reserved by the application"
            )));
        }
        if !slugs.insert(slug.clone()) {
            return Err(Error::new(format!(
                "Two sources would prefix their skills with \"{slug}\"; give \"{name}\" another name"
            )));
        }
        match &source.location {
            Location::Github {
                repository,
                branch,
                path,
            } => {
                if repository.split('/').count() != 2 || !plain(repository, &['/']) {
                    return Err(Error::new(format!(
                        "Source \"{name}\": the repository must be owner/name, not \"{repository}\""
                    )));
                }
                if branch
                    .as_deref()
                    .is_some_and(|branch| !plain(branch, &['/']))
                {
                    return Err(Error::new(format!(
                        "Source \"{name}\": invalid branch name"
                    )));
                }
                if path.as_deref().is_some_and(|path| !plain(path, &['/'])) {
                    return Err(Error::new(format!(
                        "Source \"{name}\": the folder must be a path in the repository, like config/devtool"
                    )));
                }
            }
            Location::Folder { path } => {
                if !path.is_absolute() {
                    return Err(Error::new(format!(
                        "Source \"{name}\": the folder must be an absolute path (or start with ~/)"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// A source from the settings form: `{name, kind:"github", repository, branch?, path?}` or
/// `{name, kind:"folder", path}`; blank optional fields are left out and `~/` is expanded.
fn parse(value: &Value) -> Result<Source> {
    let field = |key: &str| {
        value[key]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let name = field("name").unwrap_or_default();
    let location = match value["kind"].as_str() {
        Some("github") => Location::Github {
            repository: field("repository")
                .unwrap_or_default()
                .trim_start_matches("https://github.com/")
                .trim_end_matches('/')
                .trim_end_matches(".git")
                .to_owned(),
            branch: field("branch"),
            path: field("path").map(|path| path.trim_matches('/').to_owned()),
        },
        Some("folder") => {
            let text = field("path").unwrap_or_default();
            let path = match text.strip_prefix("~/") {
                Some(rest) => std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(rest))
                    .ok_or_else(|| Error::new("HOME is not set; enter the full folder path"))?,
                None => PathBuf::from(text),
            };
            Location::Folder { path }
        }
        _ => return Err(Error::new("A source is a GitHub repository or a folder")),
    };
    Ok(Source { name, location })
}

fn lock(data: &Path) -> Result<fs::File> {
    let dir = data.join(DIR);
    fs::create_dir_all(&dir)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(LOCK_FILE))?;
    file.lock()?;
    Ok(file)
}

fn unique(dir: &Path, kind: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    dir.join(format!(
        ".{kind}-{}-{}-{}",
        std::process::id(),
        now_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default()
}

fn now() -> u64 {
    (now_nanos() / 1_000_000_000) as u64
}

// ---------------------------------------------------------------------------------------
// Applied sources: the layers configuration is read from.

/// What an applied copy holds: the source's revision and its files with their hashes (git
/// blob ids for GitHub, SHA-256 for folders) as named in the source.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    revision: String,
    applied_at: u64,
    files: BTreeMap<String, String>,
}

fn read_meta(dir: &Path) -> Option<Meta> {
    serde_json::from_slice(&fs::read(dir.join(META_FILE)).ok()?).ok()
}

/// An applied source: one configuration layer.
#[derive(Clone, Debug)]
pub struct Layer {
    pub name: String,
    pub dir: PathBuf,
}

/// The applied sources, lowest priority first.
pub fn layers(data: &Path) -> Vec<Layer> {
    sources(data)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|source| {
            let dir = data.join(DIR).join(source.slug());
            dir.join(META_FILE).is_file().then_some(Layer {
                name: source.name,
                dir,
            })
        })
        .collect()
}

/// The parsed `name` file of every layer that has one, lowest priority first.
pub fn layer_files(data: &Path, name: &str) -> Result<Vec<(Layer, Value)>> {
    let mut out = Vec::new();
    for layer in layers(data) {
        let path = layer.dir.join(name);
        match fs::read(&path) {
            Ok(bytes) => {
                let value = serde_json::from_slice(&bytes)
                    .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
                out.push((layer, value));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(out)
}

/// The raw file `name` (a relative path like `dictionary.json` or `hints/rest.md`) of every
/// applied source that has it, lowest priority first: `[{source, text}]`.
pub fn read(data: &Path, name: &str) -> Result<Value> {
    let path = Path::new(name);
    if name.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(Error::new(format!("Invalid source file name: {name}")));
    }
    let mut out = Vec::new();
    for layer in layers(data) {
        match fs::read_to_string(layer.dir.join(path)) {
            Ok(text) => out.push(json!({"source": layer.name, "text": text})),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Value::Array(out))
}

/// The hints of `module` from the highest-priority layer that has them.
pub fn hint(data: &Path, module: &str) -> Option<(String, String)> {
    layers(data).into_iter().rev().find_map(|layer| {
        fs::read_to_string(layer.dir.join(HINTS).join(format!("{module}.md")))
            .ok()
            .map(|text| (layer.name, text))
    })
}

/// The skills folders of the applied sources.
pub fn skill_dirs(data: &Path) -> Vec<(String, PathBuf)> {
    layers(data)
        .into_iter()
        .map(|layer| (layer.name, layer.dir.join(SKILLS)))
        .filter(|(_, dir)| dir.is_dir())
        .collect()
}

/// Where a source file is kept in the applied copy: skills get the source's prefix.
fn snapshot_path(slug: &str, path: &str) -> PathBuf {
    match path
        .strip_prefix("skills/")
        .and_then(|rest| rest.split_once('/'))
    {
        Some((name, rest)) => Path::new(SKILLS).join(format!("{slug}-{name}")).join(rest),
        None => PathBuf::from(path),
    }
}

fn skill_folder(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Whether `path` is a configuration file of a source, and why not if it looks like one.
fn config_file(path: &str) -> std::result::Result<bool, String> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || part.starts_with('.'))
    {
        return Ok(false);
    }
    match parts.as_slice() {
        [DICTIONARY] | [TEMPLATES] => Ok(true),
        [HINTS, file] => match file.strip_suffix(".md") {
            Some(module) if package::check_id(module).is_ok() => Ok(true),
            Some(module) => Err(format!("{path}: \"{module}\" is not a module id")),
            None => Ok(false),
        },
        [SKILLS, name, _, ..] if skill_folder(name) => Ok(true),
        [SKILLS, name, _, ..] => Err(format!(
            "skills/{name}: skill folder names use lowercase letters, digits and hyphens"
        )),
        _ => Ok(false),
    }
}

// ---------------------------------------------------------------------------------------
// Reading a source.

type Files = BTreeMap<String, String>;
type Contents = BTreeMap<String, Vec<u8>>;

/// A source's configuration files, the contents fetched and what looked wrong.
struct Fetched {
    revision: String,
    files: Files,
    contents: Contents,
    problems: Vec<String>,
    /// The tree hash of a GitHub source's `modules/`.
    modules: Option<String>,
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The configuration files of a folder source, all read.
fn fetch_folder(root: &Path) -> Result<Fetched> {
    if !root.is_dir() {
        return Err(Error::new(format!(
            "The folder {} does not exist",
            root.display()
        )));
    }
    let mut found = Vec::new();
    for name in [DICTIONARY, TEMPLATES] {
        if root.join(name).is_file() {
            found.push(name.to_owned());
        }
    }
    fn walk(root: &Path, dir: &Path, found: &mut Vec<String>) -> Result<()> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Ok(());
        };
        for entry in entries {
            let path = entry?.path();
            if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
            {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, found)?;
            } else if path.is_file()
                && let Ok(relative) = path.strip_prefix(root)
            {
                found.push(relative.to_string_lossy().replace('\\', "/"));
            }
            if found.len() > MAX_FILES {
                return Err(Error::new(format!(
                    "{} holds more than {MAX_FILES} files",
                    root.display()
                )));
            }
        }
        Ok(())
    }
    walk(root, &root.join(HINTS), &mut found)?;
    walk(root, &root.join(SKILLS), &mut found)?;
    let mut fetched = Fetched {
        revision: String::new(),
        files: Files::new(),
        contents: Contents::new(),
        problems: Vec::new(),
        modules: None,
    };
    for path in found {
        match config_file(&path) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(problem) => {
                fetched.problems.push(problem);
                continue;
            }
        }
        let file = root.join(&path);
        if fs::metadata(&file)?.len() > MAX_FILE {
            fetched
                .problems
                .push(format!("{path} is larger than 1 MiB"));
            continue;
        }
        let bytes = fs::read(&file)?;
        fetched.files.insert(path.clone(), sha256(&bytes));
        fetched.contents.insert(path, bytes);
    }
    fetched.revision = sha256(serde_json::to_string(&fetched.files)?.as_bytes());
    Ok(fetched)
}

/// A module package a source offers, or the application ([`APPLICATION_ID`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Offer {
    id: String,
    version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    min_core: Option<String>,
    /// The module's name and description, when the package tells them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// The changes of this version.
    notes: String,
    size: u64,
    /// Why it can't be installed here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unsupported: Option<String>,
    download: Download,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Download {
    /// A package file in a GitHub source's `modules/` and its git blob hash, which the
    /// download must match.
    Blob {
        name: String,
        sha: String,
    },
    File {
        path: PathBuf,
    },
    /// The application's release assets: the archive and its `version.json`.
    Application {
        archive: u64,
        sha256: Option<String>,
        metadata: u64,
    },
}

/// The application offer of version `version`.
fn application_offer(
    version: &str,
    notes: String,
    size: u64,
    platform: &str,
    download: Download,
) -> Offer {
    Offer {
        id: APPLICATION_ID.into(),
        version: version.into(),
        min_core: None,
        name: Some(APPLICATION_NAME.into()),
        description: Some(APPLICATION_DESCRIPTION.into()),
        notes,
        size,
        unsupported: (platform != APPLICATION_PLATFORM).then(|| {
            format!("Services DevTools {version} is built for {APPLICATION_PLATFORM}, this computer is {platform}")
        }),
        download,
    }
}

/// A checked `version.json`: its URLs, version and hash belong to the repository and channel
/// it names.
fn read_release(bytes: &[u8]) -> std::result::Result<Release, String> {
    let release: Release = serde_json::from_slice(bytes)
        .map_err(|e| format!("{RELEASE_FILE} is not release metadata: {e}"))?;
    release.validate(&updates::Source {
        repository: release.repository.clone(),
        branch: release.branch.clone(),
        current_version: release.version.clone(),
    })?;
    Ok(release)
}

/// The application in a folder source's `distribution/`, if it holds a `version.json`.
fn folder_application(root: &Path, platform: &str) -> Option<std::result::Result<Offer, String>> {
    let dir = root.join(DISTRIBUTION);
    let metadata = dir.join(RELEASE_FILE);
    if !metadata.is_file() {
        return None;
    }
    let read = || {
        let bytes = fs::read(&metadata).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_RELEASE {
            return Err(format!("{RELEASE_FILE} is too large"));
        }
        let release = read_release(&bytes)?;
        let archive = dir.join(&release.archive);
        let size = fs::metadata(&archive)
            .map_err(|e| format!("{}: {e}", release.archive))?
            .len();
        let notes = fs::read_to_string(dir.join("release-notes.txt")).unwrap_or_default();
        Ok(application_offer(
            &release.version,
            notes.trim().to_owned(),
            size,
            platform,
            Download::File { path: archive },
        ))
    };
    Some(read().map_err(|problem| format!("{DISTRIBUTION}/: {problem}")))
}

/// The section of `## <version>` in release notes.
fn notes_section(notes: &str, version: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in notes.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            if inside {
                break;
            }
            inside = title.trim() == version;
            continue;
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim().to_owned()
}

/// The packages in a folder source's `modules/` and the application in its `distribution/`.
fn folder_offers(root: &Path, core: &str, platform: &str) -> (Vec<Offer>, Vec<String>) {
    let (mut offers, mut problems) = (Vec::new(), Vec::new());
    match folder_application(root, platform) {
        Some(Ok(offer)) => offers.push(offer),
        Some(Err(problem)) => problems.push(problem),
        None => {}
    }
    let Ok(entries) = fs::read_dir(root.join(MODULES)) else {
        return (offers, problems);
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(package::ARCHIVE_SUFFIX))
        })
        .collect();
    files.sort();
    for file in files {
        let shown = format!(
            "modules/{}",
            file.file_name().unwrap_or_default().to_string_lossy()
        );
        let read = || -> std::result::Result<Offer, String> {
            let size = fs::metadata(&file).map_err(|e| e.to_string())?.len();
            if size > MAX_PACKAGE {
                return Err("larger than 256 MiB".into());
            }
            let bytes = fs::read(&file).map_err(|e| e.to_string())?;
            let (described, notes) = read_package(&bytes)?;
            Ok(package_offer(
                described,
                notes,
                size,
                core,
                platform,
                Download::File { path: file.clone() },
            ))
        };
        match read() {
            Ok(offer) => offers.push(offer),
            Err(problem) => problems.push(format!("{shown}: {problem}")),
        }
    }
    (offers, problems)
}

/// The `package.json` of a package file's contents and the notes of its version.
fn read_package(bytes: &[u8]) -> std::result::Result<(Package, String), String> {
    let described = archive::read(bytes, package::PACKAGE_FILE)?
        .ok_or_else(|| format!("no {}", package::PACKAGE_FILE))?;
    let described = Package::parse(&described)?;
    let notes = archive::read(bytes, package::NOTES_FILE)?
        .map(|notes| notes_section(&String::from_utf8_lossy(&notes), &described.version))
        .unwrap_or_default();
    Ok((described, notes))
}

fn package_offer(
    described: Package,
    notes: String,
    size: u64,
    core: &str,
    platform: &str,
    download: Download,
) -> Offer {
    Offer {
        unsupported: described.unsupported(core, platform),
        id: described.id,
        version: described.version,
        min_core: described.min_core,
        name: described.name,
        description: described.description,
        notes,
        size,
        download,
    }
}

/// The offer of the contents of `name` in a GitHub source's `modules/`, which must hold the
/// module, version and platform its name says.
fn blob_offer(
    name: &str,
    sha: &str,
    bytes: &[u8],
    core: &str,
    platform: &str,
) -> std::result::Result<Offer, String> {
    let (described, notes) = read_package(bytes)?;
    if package_name(name)
        != Some((
            described.id.clone(),
            described.version.clone(),
            described.platform.clone(),
        ))
    {
        return Err(format!(
            "holds {} {}, not what its name says",
            described.id, described.version
        ));
    }
    let download = Download::Blob {
        name: name.into(),
        sha: sha.into(),
    };
    Ok(package_offer(
        described,
        notes,
        bytes.len() as u64,
        core,
        platform,
        download,
    ))
}

/// The offer of the file `name` (git blob `sha`) a former check (`known`) read, for `core`.
fn known_offer(
    known: &[Offer],
    name: &str,
    sha: &str,
    core: &str,
    platform: &str,
) -> Option<Offer> {
    let mut offer = known
        .iter()
        .find(|offer| {
            matches!(&offer.download, Download::Blob { name: known, sha: hash }
                if known == name && hash.eq_ignore_ascii_case(sha))
        })?
        .clone();
    offer.unsupported = Package {
        id: offer.id.clone(),
        version: offer.version.clone(),
        min_core: offer.min_core.clone(),
        platform: package_name(name)?.2,
        ..Package::default()
    }
    .unsupported(core, platform);
    Some(offer)
}

/// The module id, version and platform a package file's name
/// (`<id>-<version>[-<platform>].module.tgz`) gives.
fn package_name(name: &str) -> Option<(String, String, Option<String>)> {
    let parts: Vec<&str> = name
        .strip_suffix(package::ARCHIVE_SUFFIX)?
        .split('-')
        .collect();
    // An id has no dots, so the first part with one is the version.
    let at = parts.iter().position(|part| part.contains('.'))?;
    let id = parts[..at].join("-");
    package::check_id(&id).ok()?;
    package::version(parts[at]).ok()?;
    let platform = parts[at + 1..].join("-");
    Some((
        id,
        parts[at].to_owned(),
        (!platform.is_empty()).then_some(platform),
    ))
}

/// A file of a GitHub source's `modules/`: its name, git blob hash and size.
type PackageFile = (String, String, u64);

/// The newest package file of each module for `platform` among the files of a source's
/// `modules/`, and the files not named like packages.
fn newest_packages(files: Vec<PackageFile>, platform: &str) -> (Vec<PackageFile>, Vec<String>) {
    let mut newest: BTreeMap<String, ([u64; 3], PackageFile)> = BTreeMap::new();
    let mut problems = Vec::new();
    for file in files {
        let Some((id, version, built)) = package_name(&file.0) else {
            problems.push(format!(
                "{MODULES}/{}: not named <id>-<version>-<platform>{}",
                file.0,
                package::ARCHIVE_SUFFIX
            ));
            continue;
        };
        if built.is_some_and(|built| built != platform) {
            continue;
        }
        let Ok(version) = package::version(&version) else {
            continue;
        };
        if newest.get(&id).is_none_or(|(known, _)| version > *known) {
            newest.insert(id, (version, file));
        }
    }
    (
        newest.into_values().map(|(_, file)| file).collect(),
        problems,
    )
}

/// The git blob hash of `bytes`: the SHA-1 of `blob <length>\0<bytes>`.
fn blob_sha(bytes: &[u8]) -> String {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    digest.update(format!("blob {}\0", bytes.len()).as_bytes());
    digest.update(bytes);
    digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The application a GitHub release offers for `platform`: a release tagged
/// `devtool-v<version>` with the archive and `version.json`.
fn release_offer(release: &Value, platform: &str) -> Option<Offer> {
    if release["draft"] == true || release["prerelease"] == true {
        return None;
    }
    let version = release["tag_name"]
        .as_str()?
        .strip_prefix(updates::TAG_PREFIX)?;
    updates::version(version).ok()?;
    let asset = |name: &str| {
        release["assets"]
            .as_array()?
            .iter()
            .find(|asset| asset["name"] == name)
    };
    let archive = asset(updates::ARCHIVE)?;
    Some(application_offer(
        version,
        release["body"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned(),
        archive["size"].as_u64().unwrap_or_default(),
        platform,
        Download::Application {
            archive: archive["id"].as_u64()?,
            sha256: archive["digest"]
                .as_str()
                .and_then(|digest| digest.strip_prefix("sha256:"))
                .map(str::to_owned),
            metadata: asset(RELEASE_FILE)?["id"].as_u64()?,
        },
    ))
}

fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .https_only(true)
        .user_agent(concat!("services-devtools/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        // Asset downloads redirect to GitHub's storage; the token is not sent there.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let trusted = matches!(
                attempt.url().host_str(),
                Some(
                    "api.github.com"
                        | "github.com"
                        | "release-assets.githubusercontent.com"
                        | "objects.githubusercontent.com"
                )
            );
            if attempt.previous().len() >= 10 || !trusted {
                attempt.error("GitHub redirected outside its hosts")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(Error::from)
}

/// The GitHub token: the one saved in Settings → Sources, else the GitHub CLI's.
async fn token(data: &Path) -> Option<String> {
    if let Ok(token) = fs::read_to_string(data.join(TOKEN_FILE))
        && !token.trim().is_empty()
    {
        return Some(token.trim().to_owned());
    }
    static CLI: tokio::sync::OnceCell<Option<String>> = tokio::sync::OnceCell::const_new();
    CLI.get_or_init(|| async {
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new("gh")
                .args(["auth", "token"])
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .ok()?
        .ok()?;
        let token = String::from_utf8(output.stdout).ok()?.trim().to_owned();
        (output.status.success() && !token.is_empty()).then_some(token)
    })
    .await
    .clone()
}

struct GitHub {
    client: reqwest::Client,
    token: Option<String>,
    repository: String,
}

impl GitHub {
    async fn get(&self, path: &str, accept: &str) -> Result<reqwest::Response> {
        self.send(path, accept)
            .await
            .and_then(|response| self.success(response))
    }

    async fn send(&self, path: &str, accept: &str) -> Result<reqwest::Response> {
        let mut request = self
            .client
            .get(format!("{API}/repos/{}/{path}", self.repository))
            .header(reqwest::header::ACCEPT, accept)
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        request
            .send()
            .await
            .map_err(|e| Error::new(format!("GitHub {}: {}", self.repository, e.without_url())))
    }

    fn success(&self, response: reqwest::Response) -> Result<reqwest::Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let limited = response
            .headers()
            .get("x-ratelimit-remaining")
            .is_some_and(|left| left == "0");
        let repository = &self.repository;
        Err(Error::new(match status.as_u16() {
            401 => format!(
                "GitHub rejected the token for {repository}; change it in Settings → Sources"
            ),
            403 | 429 if limited => {
                "GitHub's rate limit is reached; add a GitHub token in Settings → Sources or sign in with gh auth login".into()
            }
            404 if self.token.is_none() => format!(
                "{repository}: not found. A private repository needs a GitHub token: add one in Settings → Sources or sign in with gh auth login"
            ),
            404 => format!("{repository}: not found; check the repository, branch and folder"),
            _ => format!("GitHub {repository}: HTTP {status}"),
        }))
    }

    async fn json(&self, path: &str) -> Result<Value> {
        self.get(path, "application/vnd.github+json")
            .await?
            .json()
            .await
            .map_err(|e| Error::new(format!("GitHub {}: {}", self.repository, e.without_url())))
    }

    /// The application the repository's latest release offers, if it has one.
    async fn application(&self, platform: &str) -> Result<Option<Offer>> {
        let response = self
            .send("releases/latest", "application/vnd.github+json")
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let release: Value =
            self.success(response)?.json().await.map_err(|e| {
                Error::new(format!("GitHub {}: {}", self.repository, e.without_url()))
            })?;
        Ok(release_offer(&release, platform))
    }

    /// The newest package of each module for `platform` in the tree `tree` (a source's
    /// `modules/`). Only the files `known` (former checks' offers) lacks are downloaded.
    async fn modules(
        &self,
        tree: &str,
        known: &[Offer],
        core: &str,
        platform: &str,
    ) -> Result<(Vec<Offer>, Vec<String>)> {
        let listed = self.json(&format!("git/trees/{tree}")).await?;
        let files = listed["tree"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| entry["type"] == "blob")
                    .filter_map(|entry| {
                        Some((
                            entry["path"].as_str()?.to_owned(),
                            entry["sha"].as_str()?.to_owned(),
                            entry["size"].as_u64().unwrap_or_default(),
                        ))
                    })
                    .filter(|(name, ..)| name.ends_with(package::ARCHIVE_SUFFIX))
                    .collect()
            })
            .unwrap_or_default();
        let (files, mut problems) = newest_packages(files, platform);
        let mut offers = Vec::new();
        for (name, sha, size) in files {
            let shown = format!("{MODULES}/{name}");
            if size > MAX_PACKAGE {
                problems.push(format!("{shown}: larger than 256 MiB"));
                continue;
            }
            let offer = match known_offer(known, &name, &sha, core, platform) {
                Some(offer) => Ok(offer),
                None => blob_offer(&name, &sha, &self.blob(&sha).await?, core, platform),
            };
            match offer {
                Ok(offer) => offers.push(offer),
                Err(problem) => problems.push(format!("{shown}: {problem}")),
            }
        }
        Ok((offers, problems))
    }

    /// The file of the git blob `sha`, which must hash to it.
    async fn blob(&self, sha: &str) -> Result<Vec<u8>> {
        if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::new("Check for updates again"));
        }
        let bytes = self
            .bytes(
                &format!("git/blobs/{sha}"),
                "application/vnd.github.raw+json",
                MAX_PACKAGE,
            )
            .await?;
        if !blob_sha(&bytes).eq_ignore_ascii_case(sha) {
            return Err(Error::new(format!(
                "GitHub {}: a downloaded package is damaged (its git hash differs)",
                self.repository
            )));
        }
        Ok(bytes)
    }

    async fn bytes(&self, path: &str, accept: &str, maximum: u64) -> Result<Vec<u8>> {
        let mut response = self.get(path, accept).await?;
        if response
            .content_length()
            .is_some_and(|length| length > maximum)
        {
            return Err(Error::new(format!("{path} is too large")));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| Error::new(format!("GitHub {}: {}", self.repository, e.without_url())))?
        {
            if (bytes.len() + chunk.len()) as u64 > maximum {
                return Err(Error::new(format!("{path} is too large")));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    /// Streams `path` into the new file `file`, which must hash to `sha256`.
    async fn download(&self, path: &str, file: &Path, maximum: u64, sha256: &str) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let failed = |e: reqwest::Error| {
            Error::new(format!("GitHub {}: {}", self.repository, e.without_url()))
        };
        let mut response = self.get(path, "application/octet-stream").await?;
        if response
            .content_length()
            .is_some_and(|length| length > maximum)
        {
            return Err(Error::new(format!("{path} is too large")));
        }
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let mut out = options.open(file).await?;
        let mut digest = Sha256::new();
        let mut size = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(failed)? {
            size += chunk.len() as u64;
            if size > maximum {
                return Err(Error::new(format!("{path} is too large")));
            }
            digest.update(&chunk);
            out.write_all(&chunk).await?;
        }
        out.flush().await?;
        out.sync_all().await?;
        if !format!("{:x}", digest.finalize()).eq_ignore_ascii_case(sha256) {
            return Err(Error::new(
                "The download is damaged (SHA-256 mismatch); nothing was installed",
            ));
        }
        Ok(())
    }

    /// The configuration files below `path` at `branch` and the contents of those `need`
    /// accepts.
    async fn fetch(
        &self,
        branch: Option<&str>,
        path: Option<&str>,
        need: impl Fn(&str, &str) -> bool,
    ) -> Result<Fetched> {
        let reference = branch.unwrap_or("HEAD");
        let root = self
            .json(&match path {
                Some(path) => format!("git/trees/{reference}:{path}"),
                None => format!("git/trees/{reference}"),
            })
            .await?;
        let mut fetched = Fetched {
            revision: root["sha"].as_str().unwrap_or_default().to_owned(),
            files: Files::new(),
            contents: Contents::new(),
            problems: Vec::new(),
            modules: None,
        };
        let mut blobs = Vec::new();
        let entries = |tree: &Value| -> Vec<(String, String, String, u64)> {
            tree["tree"]
                .as_array()
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| {
                            Some((
                                entry["path"].as_str()?.to_owned(),
                                entry["type"].as_str()?.to_owned(),
                                entry["sha"].as_str()?.to_owned(),
                                entry["size"].as_u64().unwrap_or_default(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        for (name, kind, sha, size) in entries(&root) {
            match (name.as_str(), kind.as_str()) {
                (DICTIONARY | TEMPLATES, "blob") => blobs.push((name, sha, size)),
                (MODULES, "tree") => fetched.modules = Some(sha),
                (HINTS | SKILLS, "tree") => {
                    let tree = self
                        .json(&format!(
                            "git/trees/{sha}{}",
                            if name == SKILLS { "?recursive=1" } else { "" }
                        ))
                        .await?;
                    if tree["truncated"] == true {
                        return Err(Error::new(format!(
                            "{}: {name}/ holds too many files",
                            self.repository
                        )));
                    }
                    for (path, kind, sha, size) in entries(&tree) {
                        if kind == "blob" {
                            blobs.push((format!("{name}/{path}"), sha, size));
                        }
                    }
                }
                _ => {}
            }
        }
        if blobs.len() > MAX_FILES {
            return Err(Error::new(format!(
                "{} holds more than {MAX_FILES} configuration files",
                self.repository
            )));
        }
        for (path, sha, size) in blobs {
            match config_file(&path) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(problem) => {
                    fetched.problems.push(problem);
                    continue;
                }
            }
            if size > MAX_FILE {
                fetched
                    .problems
                    .push(format!("{path} is larger than 1 MiB"));
                continue;
            }
            if need(&path, &sha) {
                let bytes = self
                    .bytes(
                        &format!("git/blobs/{sha}"),
                        "application/vnd.github.raw+json",
                        MAX_FILE,
                    )
                    .await?;
                fetched.contents.insert(path.clone(), bytes);
            }
            fetched.files.insert(path, sha);
        }
        Ok(fetched)
    }
}

async fn github(data: &Path, repository: &str) -> Result<GitHub> {
    Ok(GitHub {
        client: http()?,
        token: token(data).await,
        repository: repository.to_owned(),
    })
}

/// Fetches a source's configuration: the contents of the files `need` accepts (folders
/// read them all).
async fn fetch(data: &Path, source: &Source, need: impl Fn(&str, &str) -> bool) -> Result<Fetched> {
    match &source.location {
        Location::Github {
            repository,
            branch,
            path,
        } => {
            github(data, repository)
                .await?
                .fetch(branch.as_deref(), path.as_deref(), need)
                .await
        }
        Location::Folder { path } => {
            let path = path.clone();
            tokio::task::spawn_blocking(move || fetch_folder(&path))
                .await
                .map_err(|e| Error::new(e.to_string()))?
        }
    }
}

// ---------------------------------------------------------------------------------------
// Changes between the applied copy and the source.

/// A record that differs between the applied copy and the source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Change {
    kind: String,
    /// `set/VARIABLE`, `module/section`, the module of hints or the skill name.
    key: String,
    /// added, changed or removed.
    change: String,
}

fn change(kind: &str, key: String, old: Option<&Value>, new: Option<&Value>) -> Option<Change> {
    let change = match (old, new) {
        (None, Some(_)) => "added",
        (Some(_), None) => "removed",
        (Some(old), Some(new)) if old != new => "changed",
        _ => return None,
    };
    Some(Change {
        kind: kind.into(),
        key,
        change: change.into(),
    })
}

/// Records of a two-level JSON object (`{"<a>": {"<b>": value}}`) at `field`.
fn records(value: Option<&Value>, field: Option<&str>) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let value = match (value, field) {
        // A dictionary written before sets were named so keeps its sets in "profiles".
        (Some(value), Some(DICTIONARY_SETS)) => {
            value.get(DICTIONARY_SETS).or_else(|| value.get("profiles"))
        }
        (Some(value), Some(field)) => value.get(field),
        (value, _) => value,
    };
    if let Some(outer) = value.and_then(Value::as_object) {
        for (a, inner) in outer {
            if let Some(inner) = inner.as_object() {
                for (b, value) in inner {
                    out.insert(format!("{a}/{b}"), value.clone());
                }
            }
        }
    }
    out
}

/// The record changes from the applied copy in `dir` (with `applied` file hashes) to the
/// fetched source.
fn changes(slug: &str, dir: &Path, applied: &Files, fetched: &Fetched) -> Result<Vec<Change>> {
    let mut out = Vec::new();
    for (name, kind, field) in [
        (DICTIONARY, "dictionary", Some(DICTIONARY_SETS)),
        (TEMPLATES, "template", None),
    ] {
        if applied.get(name) == fetched.files.get(name) {
            continue;
        }
        let old: Option<Value> = fs::read(dir.join(name))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let new: Option<Value> = match fetched.contents.get(name) {
            Some(bytes) => Some(
                serde_json::from_slice(bytes)
                    .map_err(|e| Error::new(format!("{name} of the source: {e}")))?,
            ),
            None => None,
        };
        let (old, new) = (records(old.as_ref(), field), records(new.as_ref(), field));
        for key in old.keys().chain(new.keys()).collect::<BTreeSet<_>>() {
            out.extend(change(kind, key.clone(), old.get(key), new.get(key)));
        }
    }
    let group = |files: &Files| -> BTreeMap<String, Vec<String>> {
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (path, hash) in files {
            let key = match path.split('/').collect::<Vec<_>>().as_slice() {
                [HINTS, file] => format!("hint:{}", file.trim_end_matches(".md")),
                [SKILLS, name, ..] => format!("skill:{slug}-{name}"),
                _ => continue,
            };
            groups
                .entry(key)
                .or_default()
                .push(format!("{path}={hash}"));
        }
        groups
    };
    let (old, new) = (group(applied), group(&fetched.files));
    for key in old.keys().chain(new.keys()).collect::<BTreeSet<_>>() {
        let (kind, name) = key.split_once(':').unwrap_or_default();
        let as_value = |files: Option<&Vec<String>>| files.map(|files| json!(files));
        out.extend(change(
            kind,
            name.into(),
            as_value(old.get(key)).as_ref(),
            as_value(new.get(key)).as_ref(),
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Applying a source.

/// Checks an applied copy before it replaces the current one.
fn check_snapshot(dir: &Path) -> Result<()> {
    if let Ok(bytes) = fs::read(dir.join(DICTIONARY)) {
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|e| Error::new(format!("{DICTIONARY}: {e}")))?;
        let sets = value
            .get(DICTIONARY_SETS)
            .or_else(|| value.get("profiles"))
            .and_then(Value::as_object)
            .ok_or_else(|| {
                Error::new(format!(
                    "{DICTIONARY} must hold {{\"sets\": {{\"<set>\": {{\"<VARIABLE>\": value}}}}}}"
                ))
            })?;
        for (name, variables) in sets {
            if name.trim().is_empty() || name == "var" || name == "env" {
                return Err(Error::new(format!(
                    "{DICTIONARY}: \"{name}\" can't be a Dictionary set"
                )));
            }
            if !variables.is_object() {
                return Err(Error::new(format!(
                    "{DICTIONARY}: set \"{name}\" must be an object of variables"
                )));
            }
        }
    }
    if let Ok(bytes) = fs::read(dir.join(TEMPLATES)) {
        let library: services_contracts::templates::Library =
            serde_json::from_slice(&bytes).map_err(|e| Error::new(format!("{TEMPLATES}: {e}")))?;
        services_contracts::templates::validate(&library)
            .map_err(|e| Error::new(format!("{TEMPLATES}: {e}")))?;
    }
    if let Ok(entries) = fs::read_dir(dir.join(SKILLS)) {
        for entry in entries {
            let folder = entry?.path();
            let text = fs::read_to_string(folder.join(super::skills::FILE)).map_err(|_| {
                Error::new(format!(
                    "{}: no {}",
                    folder.file_name().unwrap_or_default().to_string_lossy(),
                    super::skills::FILE
                ))
            })?;
            super::skills::frontmatter(&text).map_err(|e| {
                Error::new(format!(
                    "{}: {e}",
                    folder.file_name().unwrap_or_default().to_string_lossy()
                ))
            })?;
        }
    }
    Ok(())
}

/// Replaces the applied copy of `source` with the fetched files: unchanged files are kept
/// from the current copy, skills renamed with the source's prefix.
fn apply_snapshot(data: &Path, source: &Source, fetched: &Fetched) -> Result<()> {
    let slug = source.slug();
    let _lock = lock(data)?;
    let root = data.join(DIR);
    let current = root.join(&slug);
    let meta = read_meta(&current).unwrap_or_default();
    let stage = unique(&root, "stage");
    struct Remove(PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _stage = Remove(stage.clone());
    fs::create_dir_all(&stage)?;
    for (path, hash) in &fetched.files {
        let placed = snapshot_path(&slug, path);
        let bytes = match fetched.contents.get(path) {
            Some(bytes) => bytes.clone(),
            None if meta.files.get(path) == Some(hash) => fs::read(current.join(&placed))?,
            None => return Err(Error::new(format!("{path} was not downloaded"))),
        };
        let bytes = match path
            .strip_prefix("skills/")
            .map(|rest| rest.split('/').collect::<Vec<_>>())
        {
            Some(parts) if parts.len() == 2 && parts[1] == super::skills::FILE => {
                let text = String::from_utf8(bytes)
                    .map_err(|_| Error::new(format!("{path} is not UTF-8 text")))?;
                super::skills::rename(&text, &format!("{slug}-{}", parts[0]))
                    .map_err(|e| Error::new(format!("{path}: {e}")))?
                    .into_bytes()
            }
            _ => bytes,
        };
        let target = stage.join(&placed);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, bytes)?;
    }
    check_snapshot(&stage).map_err(|e| {
        Error::new(format!(
            "The source \"{}\" was not applied: {}",
            source.name, e.message
        ))
    })?;
    fs::write(
        stage.join(META_FILE),
        serde_json::to_vec_pretty(&Meta {
            revision: fetched.revision.clone(),
            applied_at: now(),
            files: fetched.files.clone(),
        })?,
    )?;
    let previous = unique(&root, "previous");
    let _previous = Remove(previous.clone());
    if current.exists() {
        fs::rename(&current, &previous)?;
    }
    if let Err(error) = fs::rename(&stage, &current) {
        let _ = fs::rename(&previous, &current);
        return Err(error.into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Modules from sources.

/// Which source each installed module came from: `{"<module>": {"source": slug | null}}`;
/// `null` is a package file or a removed source.
fn module_sources(data: &Path) -> BTreeMap<String, Option<String>> {
    fs::read(data.join(MODULES_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<BTreeMap<String, Value>>(&bytes).ok())
        .map(|records| {
            records
                .into_iter()
                .map(|(id, record)| (id, record["source"].as_str().map(str::to_owned)))
                .collect()
        })
        .unwrap_or_default()
}

fn write_module_sources(data: &Path, records: &BTreeMap<String, Option<String>>) -> Result<()> {
    let value: Map<String, Value> = records
        .iter()
        .map(|(id, slug)| (id.clone(), json!({"source": slug})))
        .collect();
    write_atomic(
        &data.join(MODULES_FILE),
        &serde_json::to_vec_pretty(&value)?,
        false,
    )
}

/// Records the source a module was installed from; `None` for a package file.
pub fn set_module_source(data: &Path, id: &str, slug: Option<&str>) -> Result<()> {
    let _lock = lock(data)?;
    let mut records = module_sources(data);
    let slug = slug.map(str::to_owned);
    if records.get(id) != Some(&slug) {
        records.insert(id.into(), slug);
        write_module_sources(data, &records)?;
    }
    Ok(())
}

/// Forgets the source of a removed module.
pub fn remove_module_source(data: &Path, id: &str) -> Result<()> {
    let _lock = lock(data)?;
    let mut records = module_sources(data);
    if records.remove(id).is_some() {
        write_module_sources(data, &records)?;
    }
    Ok(())
}

/// A module the application has: loaded, or a package that failed to load.
pub struct Installed {
    pub id: String,
    /// The loaded module's name and description.
    pub name: Option<String>,
    pub description: Option<String>,
    /// The loaded module's version; a failed package's comes from its `package.json`.
    pub version: Option<String>,
    pub error: Option<String>,
}

/// The source the running application came from: its record, else the GitHub source of the
/// repository it was installed from.
fn application_source(
    sources: &[Source],
    records: &BTreeMap<String, Option<String>>,
) -> Option<String> {
    if let Some(slug) = records.get(APPLICATION_ID) {
        return slug.clone();
    }
    let repository = super::updates::installed_repository().unwrap_or_else(|| STANDARD.into());
    sources
        .iter()
        .find(|source| {
            matches!(&source.location, Location::Github { repository: from, path: None, .. } if from.eq_ignore_ascii_case(&repository))
        })
        .map(Source::slug)
}

// ---------------------------------------------------------------------------------------
// Checking.

/// The last check of a source.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Checked {
    source: Source,
    checked_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Whether the source has configuration files.
    configs: bool,
    changes: Vec<Change>,
    problems: Vec<String>,
    modules: Vec<Offer>,
}

fn read_checks(data: &Path) -> BTreeMap<String, Checked> {
    fs::read(data.join(DIR).join(CHECK_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_checks(data: &Path, checks: &BTreeMap<String, Checked>) -> Result<()> {
    write_atomic(
        &data.join(DIR).join(CHECK_FILE),
        &serde_json::to_vec(checks)?,
        false,
    )
}

/// Checks `source`; `known` are the offers of former checks, whose package files need not
/// be read again.
async fn check_source(
    data: &Path,
    source: &Source,
    known: &[Offer],
    core: &str,
    platform: &str,
) -> Checked {
    let slug = source.slug();
    let dir = data.join(DIR).join(&slug);
    let applied = read_meta(&dir).map(|meta| meta.files).unwrap_or_default();
    let mut checked = Checked {
        source: source.clone(),
        checked_at: now(),
        error: None,
        configs: false,
        changes: Vec::new(),
        problems: Vec::new(),
        modules: Vec::new(),
    };
    let need = |path: &str, hash: &str| {
        [DICTIONARY, TEMPLATES].contains(&path)
            && applied.get(path).map(String::as_str) != Some(hash)
    };
    let result = async {
        let fetched = fetch(data, source, need).await?;
        checked.configs = !fetched.files.is_empty();
        checked.changes = changes(&slug, &dir, &applied, &fetched)?;
        checked.problems = fetched.problems;
        checked.modules = match &source.location {
            Location::Github { repository, .. } => {
                let github = github(data, repository).await?;
                let mut offers: Vec<Offer> =
                    github.application(platform).await?.into_iter().collect();
                if let Some(tree) = &fetched.modules {
                    let (modules, problems) = github.modules(tree, known, core, platform).await?;
                    offers.extend(modules);
                    checked.problems.extend(problems);
                }
                offers
            }
            Location::Folder { path } => {
                let (path, core, platform) = (path.clone(), core.to_owned(), platform.to_owned());
                let (offers, problems) =
                    tokio::task::spawn_blocking(move || folder_offers(&path, &core, &platform))
                        .await
                        .map_err(|e| Error::new(e.to_string()))?;
                checked.problems.extend(problems);
                offers
            }
        };
        checked.modules.sort_by(|a, b| {
            let module = |offer: &Offer| offer.id != APPLICATION_ID;
            (module(a), &a.id).cmp(&(module(b), &b.id)).then_with(|| {
                package::version(&b.version)
                    .ok()
                    .cmp(&package::version(&a.version).ok())
            })
        });
        Ok::<_, Error>(())
    }
    .await;
    if let Err(error) = result {
        checked.error = Some(error.message);
    }
    checked
}

/// The sources service: the configured sources, their checks, applying them and the
/// GitHub token. Installing a module from a source is in the runtime (it loads the module).
#[derive(Clone)]
pub struct Sources {
    data: PathBuf,
    /// When this host process started: checks made since then are not repeated unforced.
    started: u64,
}

impl Sources {
    pub fn new(data: impl Into<PathBuf>) -> Self {
        Self {
            data: data.into(),
            started: now(),
        }
    }

    pub fn data(&self) -> &Path {
        &self.data
    }

    /// `{sources:[{name, slug, kind, …, applied}], tokenSet, warning?}`.
    pub fn get(&self) -> Result<Value> {
        let sources = sources(&self.data)?;
        let described: Vec<Value> = sources
            .iter()
            .map(|source| {
                let mut value = source.describe();
                value["applied"] = read_meta(&self.data.join(DIR).join(source.slug()))
                    .map(|meta| json!({"at": meta.applied_at, "revision": meta.revision}))
                    .unwrap_or(Value::Null);
                value
            })
            .collect();
        let mut reply = json!({
            "sources": described,
            "tokenSet": self.data.join(TOKEN_FILE).is_file(),
        });
        if let Some(warning) = migration_warning(&self.data) {
            reply["warning"] = json!(warning);
        }
        Ok(reply)
    }

    /// Saves the list: `{sources:[{id?, name, kind, …}]}`, where `id` is the slug of a
    /// configured source. A source keeps its place but may be renamed; one left out is
    /// removed with its applied copy.
    pub fn save(&self, message: &Value) -> Result<Value> {
        let entries = message["sources"]
            .as_array()
            .ok_or_else(|| Error::new("sources must be a list"))?;
        let mut sources = Vec::new();
        let mut kept = BTreeMap::new();
        for entry in entries {
            let source = parse(entry)?;
            if let Some(id) = entry["id"].as_str() {
                kept.insert(id.to_owned(), source.clone());
            }
            sources.push(source);
        }
        validate(&sources)?;
        let _lock = lock(&self.data)?;
        let current = self::sources(&self.data)?;
        let mut renames = Vec::new();
        for (id, source) in &kept {
            let old = current
                .iter()
                .find(|old| old.slug() == *id)
                .ok_or_else(|| Error::new(format!("Unknown source \"{id}\"")))?;
            if old.location != source.location {
                return Err(Error::new(format!(
                    "The place of source \"{}\" can't change; add a new source instead",
                    old.name
                )));
            }
            if source.slug() != *id {
                renames.push((id.clone(), source.slug()));
            }
        }
        let root = self.data.join(DIR);
        let mut records = module_sources(&self.data);
        for old in &current {
            let slug = old.slug();
            if !kept.contains_key(&slug) {
                let _ = fs::remove_dir_all(root.join(&slug));
                for source in records.values_mut() {
                    if source.as_deref() == Some(slug.as_str()) {
                        *source = None;
                    }
                }
            }
        }
        // Through temporary names, so that sources may swap names.
        let mut moved = Vec::new();
        for (old, new) in &renames {
            let temporary = unique(&root, "rename");
            if root.join(old).exists() {
                fs::rename(root.join(old), &temporary)?;
            }
            moved.push((old.clone(), new.clone(), temporary));
        }
        for (old, new, temporary) in moved {
            if temporary.exists() {
                fs::rename(&temporary, root.join(&new))?;
                rename_skills(&root.join(&new), &old, &new)?;
            }
            for source in records.values_mut() {
                if source.as_deref() == Some(old.as_str()) {
                    *source = Some(new.clone());
                }
            }
        }
        write_module_sources(&self.data, &records)?;
        write_sources(&self.data, &sources)?;
        let _ = fs::remove_file(root.join(CHECK_FILE));
        drop(_lock);
        self.get()
    }

    /// `{token}` saves the GitHub token for sources, `{token:null}` clears it.
    pub fn set_token(&self, message: &Value) -> Result<Value> {
        let path = self.data.join(TOKEN_FILE);
        match message["token"].as_str().map(str::trim) {
            Some(token) if !token.is_empty() => {
                if token.chars().any(|c| c.is_whitespace() || c.is_control()) {
                    return Err(Error::new("A GitHub token is one word"));
                }
                write_atomic(&path, token.as_bytes(), true)?;
            }
            _ => match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            },
        }
        let _ = fs::remove_file(self.data.join(DIR).join(CHECK_FILE));
        self.get()
    }

    /// Checks every source not checked since this host process started (`force` checks
    /// anyway), so DevTools checks once each time it opens the application:
    /// `{sources:[{name, slug, kind, repository?|path, checkedAt, error?, configs, applied,
    /// changes:[{kind, key, change}], problems, modules:[offers]}],
    /// installed:{"<module>": {version, source?, notes, error?, name?, description?}}}`, where
    /// `installed` has the
    /// application too ([`APPLICATION_ID`]).
    pub async fn check(&self, force: bool, installed: &[Installed]) -> Result<Value> {
        let sources = sources(&self.data)?;
        let mut checks = read_checks(&self.data);
        let core = env!("CARGO_PKG_VERSION");
        let platform = package::host_platform();
        let mut changed = false;
        let known: Vec<Offer> = checks
            .values()
            .flat_map(|checked| checked.modules.iter().cloned())
            .collect();
        for source in &sources {
            let slug = source.slug();
            let fresh = checks.get(&slug).is_some_and(|checked| {
                checked.source == *source && checked.checked_at >= self.started
            });
            if force || !fresh {
                checks.insert(
                    slug,
                    check_source(&self.data, source, &known, core, &platform).await,
                );
                changed = true;
            }
        }
        checks.retain(|slug, _| sources.iter().any(|source| source.slug() == *slug));
        if changed {
            write_checks(&self.data, &checks)?;
        }
        Ok(self.describe_checks(&sources, &checks, installed))
    }

    fn describe_checks(
        &self,
        sources: &[Source],
        checks: &BTreeMap<String, Checked>,
        installed: &[Installed],
    ) -> Value {
        let described: Vec<Value> = sources
            .iter()
            .filter_map(|source| {
                let slug = source.slug();
                let checked = checks.get(&slug)?;
                let applied = self.data.join(DIR).join(&slug).join(META_FILE).is_file();
                let changes: Vec<Value> = checked
                    .changes
                    .iter()
                    .map(|change| serde_json::to_value(change).unwrap_or_default())
                    .collect();
                let mut described = source.describe();
                for (key, value) in [
                    ("checkedAt", json!(checked.checked_at)),
                    ("error", json!(checked.error)),
                    ("configs", json!(checked.configs)),
                    ("applied", json!(applied)),
                    ("changes", json!(changes)),
                    ("problems", json!(checked.problems)),
                    ("modules", json!(checked.modules)),
                ] {
                    described[key] = value;
                }
                Some(described)
            })
            .collect();
        // Modules without a record came with the application before 3.2.0, from Standard.
        let records = module_sources(&self.data);
        let standard = sources.iter().find(|source| {
            matches!(&source.location, Location::Github { repository, .. } if repository.eq_ignore_ascii_case(STANDARD))
        });
        let mut origins: Map<String, Value> = installed
            .iter()
            .map(|module| {
                let id = &module.id;
                let slug = match records.get(id) {
                    Some(slug) => slug.clone(),
                    None => standard
                        .filter(|standard| {
                            checks.get(&standard.slug()).is_some_and(|checked| {
                                checked.modules.iter().any(|offer| offer.id == *id)
                            })
                        })
                        .map(Source::slug),
                };
                let dir = self.data.join(super::modules::DIR).join(id);
                let version = module
                    .version
                    .clone()
                    .or_else(|| Package::read(&dir).ok().map(|package| package.version));
                let notes = version
                    .as_deref()
                    .and_then(|version| {
                        let notes = fs::read_to_string(dir.join(package::NOTES_FILE)).ok()?;
                        Some(notes_section(&notes, version))
                    })
                    .unwrap_or_default();
                (
                    id.clone(),
                    json!({"version": version, "source": slug, "notes": notes, "error": module.error,
                        "name": module.name, "description": module.description}),
                )
            })
            .collect();
        let core = env!("CARGO_PKG_VERSION");
        origins.insert(
            APPLICATION_ID.into(),
            json!({
                "version": core,
                "source": application_source(sources, &records),
                "notes": notes_section(APPLICATION_NOTES, core),
                "name": APPLICATION_NAME,
                "description": APPLICATION_DESCRIPTION,
            }),
        );
        json!({"sources": described, "installed": origins})
    }

    /// Applies the configuration of the source `slug`.
    pub async fn apply(&self, slug: &str) -> Result<Value> {
        let source = sources(&self.data)?
            .into_iter()
            .find(|source| source.slug() == slug)
            .ok_or_else(|| Error::new(format!("Unknown source \"{slug}\"")))?;
        let applied = read_meta(&self.data.join(DIR).join(slug))
            .map(|meta| meta.files)
            .unwrap_or_default();
        let fetched = fetch(&self.data, &source, |path, hash| {
            applied.get(path).map(String::as_str) != Some(hash)
        })
        .await?;
        let data = self.data.clone();
        let applied_source = source.clone();
        let fetched = tokio::task::spawn_blocking(move || {
            apply_snapshot(&data, &applied_source, &fetched).map(|()| fetched)
        })
        .await
        .map_err(|e| Error::new(e.to_string()))??;
        let mut checks = read_checks(&self.data);
        if let Some(checked) = checks.get_mut(slug) {
            checked.changes.clear();
            checked.configs = !fetched.files.is_empty();
            checked.problems = fetched.problems;
            let _ = write_checks(&self.data, &checks);
        }
        self.get()
    }

    /// The source `slug` and what it offers of `id` `version`, as its last check found it.
    fn offer(&self, slug: &str, id: &str, version: &str) -> Result<(Source, Offer)> {
        let source = self.source(slug)?;
        let offer = read_checks(&self.data)
            .remove(slug)
            .and_then(|checked| {
                checked
                    .modules
                    .into_iter()
                    .find(|offer| offer.id == id && offer.version == version)
            })
            .ok_or_else(|| {
                Error::new(format!(
                    "Source \"{}\" doesn't offer {id} {version}; check for updates again",
                    source.name
                ))
            })?;
        if let Some(reason) = &offer.unsupported {
            return Err(Error::new(reason.clone()));
        }
        Ok((source, offer))
    }

    fn source(&self, slug: &str) -> Result<Source> {
        sources(&self.data)?
            .into_iter()
            .find(|source| source.slug() == slug)
            .ok_or_else(|| Error::new(format!("Unknown source \"{slug}\"")))
    }

    /// Downloads version `version` of module `id` from the source `slug`, as its last check
    /// found it.
    pub async fn package(&self, slug: &str, id: &str, version: &str) -> Result<Vec<u8>> {
        if id == APPLICATION_ID {
            return Err(Error::new("Services DevTools is installed by its updater"));
        }
        let (source, offer) = self.offer(slug, id, version)?;
        match (&source.location, &offer.download) {
            (Location::Github { repository, .. }, Download::Blob { sha, .. }) => {
                github(&self.data, repository).await?.blob(sha).await
            }
            (Location::Folder { path: root }, Download::File { path })
                if path.starts_with(root.join(MODULES)) =>
            {
                fs::read(path).map_err(|e| Error::new(format!("{}: {e}", path.display())))
            }
            _ => Err(Error::new("Check for updates again")),
        }
    }

    /// Fetches version `version` of the application from the source `slug`, as its last check
    /// found it: its checked release metadata and the archive, downloaded into the folder
    /// `into` or, from a folder source, where it lies.
    pub async fn application(
        &self,
        slug: &str,
        version: &str,
        into: &Path,
    ) -> Result<(Release, PathBuf)> {
        let (source, offer) = self.offer(slug, APPLICATION_ID, version)?;
        let core = env!("CARGO_PKG_VERSION");
        if updates::version(version).map_err(Error::new)?
            <= updates::version(core).map_err(Error::new)?
        {
            return Err(Error::new(format!(
                "Services DevTools {version} is not newer than the running {core}"
            )));
        }
        let changed = || {
            Error::new(format!(
                "The release metadata of Services DevTools {version} in \"{}\" names another release; check for updates again",
                source.name
            ))
        };
        match (&source.location, &offer.download) {
            (
                Location::Github { repository, .. },
                Download::Application {
                    archive,
                    sha256,
                    metadata,
                },
            ) => {
                let github = github(&self.data, repository).await?;
                let bytes = github
                    .bytes(
                        &format!("releases/assets/{metadata}"),
                        "application/octet-stream",
                        MAX_RELEASE,
                    )
                    .await?;
                let release = read_release(&bytes).map_err(Error::new)?;
                if release.version != version
                    || !release.repository.eq_ignore_ascii_case(repository)
                    || sha256
                        .as_ref()
                        .is_some_and(|sha256| !sha256.eq_ignore_ascii_case(&release.sha256))
                {
                    return Err(changed());
                }
                let path = into.join(&release.archive);
                github
                    .download(
                        &format!("releases/assets/{archive}"),
                        &path,
                        MAX_ARCHIVE,
                        &release.sha256,
                    )
                    .await?;
                Ok((release, path))
            }
            (Location::Folder { path: root }, Download::File { path })
                if path.starts_with(root.join(DISTRIBUTION)) =>
            {
                let dir = root.join(DISTRIBUTION);
                let bytes = fs::read(dir.join(RELEASE_FILE))
                    .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
                let release = read_release(&bytes).map_err(Error::new)?;
                if release.version != version {
                    return Err(changed());
                }
                let path = dir.join(&release.archive);
                Ok((release, path))
            }
            _ => Err(Error::new("Check for updates again")),
        }
    }

    /// Opens a folder source's folder in Finder.
    pub fn open(&self, slug: &str) -> Result<Value> {
        let source = self.source(slug)?;
        let Location::Folder { path } = &source.location else {
            return Err(Error::new(format!("\"{}\" is not a folder", source.name)));
        };
        std::process::Command::new("/usr/bin/open")
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        Ok(json!({}))
    }
}

/// Renames the skills of an applied copy from prefix `old` to `new`.
fn rename_skills(dir: &Path, old: &str, new: &str) -> Result<()> {
    let Ok(entries) = fs::read_dir(dir.join(SKILLS)) else {
        return Ok(());
    };
    for entry in entries {
        let folder = entry?.path();
        let name = folder
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let Some(rest) = name.strip_prefix(&format!("{old}-")) else {
            continue;
        };
        let renamed = format!("{new}-{rest}");
        let target = dir.join(SKILLS).join(&renamed);
        fs::rename(&folder, &target)?;
        super::skills::rename_file(&target, &renamed).map_err(Error::new)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// The team skills folder of 3.0.4–3.1: a folder of skill folders handed to the Agent.

fn legacy_skills(data: &Path) -> PathBuf {
    data.join(".agent").join("skills.json")
}

fn legacy_folder(data: &Path) -> Option<PathBuf> {
    let value: Value = serde_json::from_slice(&fs::read(legacy_skills(data)).ok()?).ok()?;
    value["teamFolder"].as_str().map(PathBuf::from)
}

fn migration_warning(data: &Path) -> Option<String> {
    legacy_folder(data).map(|folder| {
        format!(
            "Team skills now come from sources. The team skills folder {} was not added: a source folder holds its skills in skills/. Add a folder source whose skills/ folder holds these skills, then save the list.",
            folder.display()
        )
    })
}

/// Turns the team skills folder setting into a folder source: when the folder is named
/// `skills`, its parent is a source folder, added as "Team" and applied at once. Otherwise
/// the setting stays, and Settings → Sources explains what to do until the list is saved.
pub fn migrate_team_folder(data: &Path) -> Result<()> {
    let Some(folder) = legacy_folder(data) else {
        if legacy_skills(data).exists() {
            fs::remove_file(legacy_skills(data))?;
        }
        return Ok(());
    };
    let parent = folder
        .parent()
        .filter(|_| folder.file_name().is_some_and(|name| name == SKILLS));
    let Some(parent) = parent else {
        return Ok(());
    };
    let mut list = sources(data)?;
    if !list.iter().any(|source| {
        source.location
            == Location::Folder {
                path: parent.to_path_buf(),
            }
    }) {
        let mut name = "Team".to_owned();
        let mut next = 2;
        while list.iter().any(|source| source.slug() == slug(&name)) {
            name = format!("Team {next}");
            next += 1;
        }
        let source = Source {
            name,
            location: Location::Folder {
                path: parent.to_path_buf(),
            },
        };
        list.push(source.clone());
        validate(&list)?;
        write_sources(data, &list)?;
        if let Ok(fetched) = fetch_folder(parent) {
            apply_snapshot(data, &source, &fetched)?;
        }
    }
    fs::remove_file(legacy_skills(data))?;
    Ok(())
}

/// The sources service with `op`: get, save, token, check, apply, open. Install is in the
/// runtime.
pub async fn dispatch(
    sources: &Sources,
    message: &Value,
    installed: &[Installed],
) -> Result<Value> {
    match message["op"].as_str().unwrap_or("get") {
        "get" => sources.get(),
        "save" => {
            let result = sources.save(message);
            if result.is_ok() {
                let _ = fs::remove_file(legacy_skills(sources.data()));
            }
            result
        }
        "token" => sources.set_token(message),
        "check" => sources.check(message["force"] == true, installed).await,
        "apply" => {
            let slug = message["source"]
                .as_str()
                .ok_or_else(|| Error::new("Which source?"))?;
            sources.apply(slug).await
        }
        "open" => sources.open(
            message["source"]
                .as_str()
                .ok_or_else(|| Error::new("Which source?"))?,
        ),
        op => Err(Error::new(format!("Unknown sources op: {op}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release_metadata(version: &str, sha256: &str) -> Release {
        let base = format!("https://github.com/duhon/devtool/releases/download/devtool-v{version}");
        Release {
            schema_version: 1,
            application: "ServicesDevTools".into(),
            version: version.into(),
            tag: format!("devtool-v{version}"),
            commit: "a".repeat(40),
            repository: "duhon/devtool".into(),
            branch: "main".into(),
            platform: "darwin".into(),
            arch: "arm64".into(),
            minimum_macos: "13.5".into(),
            archive: updates::ARCHIVE.into(),
            sha256: sha256.into(),
            archive_url: format!("{base}/{}", updates::ARCHIVE),
            checksums_url: format!("{base}/checksums.txt"),
            installer_url: format!("{base}/install.sh"),
        }
    }

    fn folder_source(name: &str, path: &Path) -> Value {
        json!({"name": name, "kind": "folder", "path": path})
    }

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn names_become_unique_prefixes() {
        assert_eq!(slug("  My Team/QA  "), "my-team-qa");
        assert_eq!(slug("Команда"), "");
        let source = |name: &str| Source {
            name: name.into(),
            location: Location::Folder {
                path: "/tmp".into(),
            },
        };
        assert!(validate(&[source("Team"), source("team")]).is_err());
        assert!(validate(&[source("Local")]).is_err());
        assert!(validate(&[source("Команда")]).is_err());
        assert!(validate(&[source("Team"), Source::standard()]).is_ok());
        let github = |repository: &str, path: Option<&str>| Source {
            name: "G".into(),
            location: Location::Github {
                repository: repository.into(),
                branch: None,
                path: path.map(str::to_owned),
            },
        };
        assert!(validate(&[github("a/b", Some("config/devtool"))]).is_ok());
        assert!(validate(&[github("a", None)]).is_err());
        assert!(validate(&[github("a/b", Some("../x"))]).is_err());
        let parsed = parse(&json!({"name":"G","kind":"github","repository":"https://github.com/a/b.git","branch":" ","path":"/cfg/"})).unwrap();
        assert_eq!(
            parsed.location,
            Location::Github {
                repository: "a/b".into(),
                branch: None,
                path: Some("cfg".into()),
            }
        );
    }

    #[tokio::test]
    async fn a_folder_source_is_checked_applied_renamed_and_removed() {
        let data = tempfile::tempdir().unwrap();
        let team = tempfile::tempdir().unwrap();
        let data = data.path();
        write(
            team.path(),
            "dictionary.json",
            r#"{"sets":{"stage":{"HOST":"a","PORT":"1"}}}"#,
        );
        write(team.path(), "hints/rest.md", "Use staging");
        write(
            team.path(),
            "skills/deploy/SKILL.md",
            "---\nname: deploy\ndescription: Deploys\n---\nSteps\n",
        );
        write(team.path(), "skills/deploy/refs/a.md", "a");
        write(team.path(), "skills/Bad/SKILL.md", "x");
        write(team.path(), "notes.md", "not configuration");
        let sources = Sources::new(data);
        sources
            .save(&json!({"sources": [folder_source("Team", team.path())]}))
            .unwrap();
        let checked = sources.check(false, &[]).await.unwrap();
        let team_check = &checked["sources"][0];
        assert_eq!(team_check["error"], Value::Null, "{team_check}");
        assert_eq!(team_check["applied"], false);
        assert!(
            team_check["problems"][0]
                .as_str()
                .unwrap()
                .contains("skills/Bad")
        );
        assert_eq!(
            team_check["changes"],
            json!([
                {"kind":"dictionary","key":"stage/HOST","change":"added"},
                {"kind":"dictionary","key":"stage/PORT","change":"added"},
                {"kind":"hint","key":"rest","change":"added"},
                {"kind":"skill","key":"team-deploy","change":"added"},
            ])
        );
        sources.apply("team").await.unwrap();
        let skill = data.join("sources/team/skills/team-deploy");
        assert_eq!(
            super::super::skills::frontmatter(&fs::read_to_string(skill.join("SKILL.md")).unwrap())
                .unwrap()
                .0,
            "team-deploy"
        );
        assert!(skill.join("refs/a.md").is_file());
        assert_eq!(
            hint(data, "rest"),
            Some(("Team".into(), "Use staging".into()))
        );
        let changes = |checked: Value| checked["sources"][0]["changes"].clone();
        let before = changes(sources.check(false, &[]).await.unwrap());
        write(
            team.path(),
            "dictionary.json",
            r#"{"sets":{"stage":{"HOST":"b","PORT":"1"}}}"#,
        );
        // A host process checks once; a new one checks again.
        assert_eq!(changes(sources.check(false, &[]).await.unwrap()), before);
        let restarted = Sources {
            started: now() + 1,
            ..Sources::new(data)
        };
        assert_eq!(
            changes(restarted.check(false, &[]).await.unwrap()),
            json!([{"kind":"dictionary","key":"stage/HOST","change":"changed"}])
        );
        // An invalid source keeps the applied copy.
        write(
            team.path(),
            "templates.json",
            r#"{"rest":{"request":"@a\n@a\n"}}"#,
        );
        let error = sources.apply("team").await.unwrap_err();
        assert!(
            error.message.contains("was not applied"),
            "{}",
            error.message
        );
        assert_eq!(
            layer_files(data, DICTIONARY).unwrap()[0].1["sets"]["stage"]["HOST"],
            "a"
        );
        fs::remove_file(team.path().join("templates.json")).unwrap();
        set_module_source(data, "rest", Some("team")).unwrap();
        sources
            .save(
                &json!({"sources": [{"id":"team","name":"QA","kind":"folder","path":team.path()}]}),
            )
            .unwrap();
        let renamed = data.join("sources/qa/skills/qa-deploy/SKILL.md");
        assert!(
            fs::read_to_string(renamed)
                .unwrap()
                .contains("name: qa-deploy")
        );
        assert_eq!(module_sources(data)["rest"].as_deref(), Some("qa"));
        assert_eq!(skill_dirs(data)[0].0, "QA");
        let moved = sources.save(
            &json!({"sources": [{"id":"qa","name":"QA","kind":"folder","path":"/elsewhere"}]}),
        );
        assert!(moved.unwrap_err().message.contains("can't change"));
        sources.save(&json!({"sources": []})).unwrap();
        assert!(!data.join("sources/qa").exists());
        assert_eq!(
            module_sources(data)["rest"],
            None,
            "the module stays, its source is gone"
        );
        assert!(layers(data).is_empty());
    }

    #[tokio::test]
    async fn a_folder_source_offers_its_packages() {
        let data = tempfile::tempdir().unwrap();
        let team = tempfile::tempdir().unwrap();
        let package = tempfile::tempdir().unwrap();
        let described = |version: &str, min_core: &str| Package {
            id: "redis".into(),
            version: version.into(),
            min_core: Some(min_core.into()),
            platform: Some(package::host_platform()),
            name: Some("Redis".into()),
            description: None,
        };
        fs::create_dir_all(team.path().join(MODULES)).unwrap();
        let distribution = team.path().join(DISTRIBUTION);
        fs::create_dir_all(&distribution).unwrap();
        fs::write(distribution.join(updates::ARCHIVE), b"archive").unwrap();
        fs::write(distribution.join("release-notes.txt"), "- New app\n").unwrap();
        fs::write(
            distribution.join(RELEASE_FILE),
            serde_json::to_vec(&release_metadata("65000.0.0", &self::sha256(b"archive"))).unwrap(),
        )
        .unwrap();
        let installed = data.path().join("modules/redis");
        fs::create_dir_all(&installed).unwrap();
        fs::write(
            installed.join(package::NOTES_FILE),
            "## 1.0.0\n- Installed notes\n",
        )
        .unwrap();
        for (version, min_core) in [("1.0.0", "1.0.0"), ("2.0.0", "99.0.0")] {
            fs::write(
                package.path().join(package::PACKAGE_FILE),
                serde_json::to_vec(&described(version, min_core)).unwrap(),
            )
            .unwrap();
            fs::write(package.path().join(package::WASM_FILE), b"\0asm").unwrap();
            fs::write(
                package.path().join(package::NOTES_FILE),
                format!("## {version}\n- Notes of {version}\n\n## 0.1.0\n- Old\n"),
            )
            .unwrap();
            fs::write(
                team.path()
                    .join(MODULES)
                    .join(package::archive_name(&described(version, min_core))),
                archive::pack(package.path()).unwrap(),
            )
            .unwrap();
        }
        fs::write(team.path().join(MODULES).join("broken.module.tgz"), b"x").unwrap();
        let sources = Sources::new(data.path());
        sources
            .save(&json!({"sources": [folder_source("Team", team.path())]}))
            .unwrap();
        let checked = sources
            .check(
                false,
                &[
                    Installed {
                        id: "redis".into(),
                        name: Some("Redis".into()),
                        description: None,
                        version: Some("1.0.0".into()),
                        error: None,
                    },
                    Installed {
                        id: "broken".into(),
                        name: None,
                        description: None,
                        version: None,
                        error: Some("not a services module".into()),
                    },
                ],
            )
            .await
            .unwrap();
        assert_eq!(checked["sources"][0]["kind"], "folder");
        let modules = &checked["sources"][0]["modules"];
        assert_eq!(
            (
                &modules[0]["id"],
                &modules[0]["version"],
                &modules[0]["notes"]
            ),
            (&json!("app"), &json!("65000.0.0"), &json!("- New app"))
        );
        assert_eq!(modules[1]["version"], "2.0.0");
        assert!(
            modules[1]["unsupported"]
                .as_str()
                .unwrap()
                .contains("99.0.0")
        );
        assert_eq!(modules[2]["version"], "1.0.0");
        assert_eq!(modules[2]["name"], "Redis");
        assert_eq!(modules[2]["notes"], "- Notes of 1.0.0");
        assert!(
            checked["sources"][0]["problems"][0]
                .as_str()
                .unwrap()
                .contains("broken")
        );
        assert_eq!(
            checked["installed"]["redis"],
            json!({"version": "1.0.0", "source": null, "notes": "- Installed notes", "error": null,
                "name": "Redis", "description": null})
        );
        assert_eq!(
            checked["installed"]["broken"],
            json!({"version": null, "source": null, "notes": "", "error": "not a services module",
                "name": null, "description": null})
        );
        assert_eq!(
            checked["installed"]["app"]["version"],
            env!("CARGO_PKG_VERSION")
        );
        assert!(sources.package("team", "app", "65000.0.0").await.is_err());
        let into = tempfile::tempdir().unwrap();
        let (release, path) = sources
            .application("team", "65000.0.0", into.path())
            .await
            .unwrap();
        assert_eq!(
            (release.version.as_str(), path),
            ("65000.0.0", distribution.join(updates::ARCHIVE))
        );
        fs::write(
            distribution.join(RELEASE_FILE),
            serde_json::to_vec(&release_metadata("65001.0.0", &self::sha256(b"archive"))).unwrap(),
        )
        .unwrap();
        assert!(
            sources
                .application("team", "65000.0.0", into.path())
                .await
                .unwrap_err()
                .message
                .contains("names another release")
        );
        let bytes = sources.package("team", "redis", "1.0.0").await.unwrap();
        assert_eq!(
            Package::parse(
                &archive::read(&bytes, package::PACKAGE_FILE)
                    .unwrap()
                    .unwrap()
            )
            .unwrap()
            .version,
            "1.0.0"
        );
        assert!(sources.package("team", "redis", "2.0.0").await.is_err());
        assert!(sources.package("team", "redis", "3.0.0").await.is_err());
    }

    #[test]
    fn a_modules_folder_offers_the_newest_package_of_each_module_for_this_platform() {
        assert_eq!(
            package_name("my-rest-1.2.3-macos-arm64.module.tgz"),
            Some(("my-rest".into(), "1.2.3".into(), Some("macos-arm64".into())))
        );
        assert_eq!(
            package_name("rest-1.2.3.module.tgz"),
            Some(("rest".into(), "1.2.3".into(), None))
        );
        for name in [
            "rest.module.tgz",
            "rest-1.2-macos.module.tgz",
            "Rest-1.2.3.module.tgz",
            "rest-1.2.3.tgz",
        ] {
            assert_eq!(package_name(name), None, "{name}");
        }
        let file = |name: &str| (name.to_owned(), format!("sha of {name}"), 1);
        let (newest, problems) = newest_packages(
            vec![
                file("rest-1.9.0-macos-arm64.module.tgz"),
                file("rest-1.10.0-macos-arm64.module.tgz"),
                file("rest-2.0.0-linux-x86_64.module.tgz"),
                file("echo-1.0.0.module.tgz"),
                file("notes.module.tgz"),
            ],
            "macos-arm64",
        );
        let names: Vec<&str> = newest.iter().map(|(name, ..)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "echo-1.0.0.module.tgz",
                "rest-1.10.0-macos-arm64.module.tgz"
            ]
        );
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("modules/notes.module.tgz"));
    }

    #[test]
    fn a_github_package_file_is_read_once_by_its_git_hash() {
        // `git hash-object` of "hello\n".
        assert_eq!(
            blob_sha(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        let folder = tempfile::tempdir().unwrap();
        let described = Package {
            id: "rest".into(),
            version: "1.2.0".into(),
            min_core: Some("4.0.0".into()),
            platform: Some("macos-arm64".into()),
            name: Some("REST".into()),
            description: None,
        };
        fs::write(
            folder.path().join(package::PACKAGE_FILE),
            serde_json::to_vec(&described).unwrap(),
        )
        .unwrap();
        fs::write(folder.path().join(package::WASM_FILE), b"\0asm").unwrap();
        fs::write(folder.path().join(package::NOTES_FILE), "## 1.2.0\n- Fix\n").unwrap();
        let bytes = archive::pack(folder.path()).unwrap();
        let name = package::archive_name(&described);
        let sha = blob_sha(&bytes);
        let offer = blob_offer(&name, &sha, &bytes, "3.3.0", "macos-arm64").unwrap();
        assert_eq!(
            (offer.name.as_deref(), offer.notes.as_str()),
            (Some("REST"), "- Fix")
        );
        assert!(offer.unsupported.as_deref().unwrap().contains("4.0.0"));
        for misnamed in ["rest-1.1.0-macos-arm64.module.tgz", "rest-1.2.0.module.tgz"] {
            assert!(
                blob_offer(misnamed, &sha, &bytes, "3.3.0", "macos-arm64")
                    .unwrap_err()
                    .contains("not what its name says")
            );
        }

        // A later check reuses it while the file is the same, for the core running then.
        let known = [offer];
        let again =
            known_offer(&known, &name, &sha.to_uppercase(), "4.0.0", "macos-arm64").unwrap();
        assert_eq!(again.unsupported, None);
        assert_eq!(again.notes, "- Fix");
        assert!(known_offer(&known, &name, &"0".repeat(40), "4.0.0", "macos-arm64").is_none());
        assert!(
            known_offer(
                &known,
                "rest-1.2.0.module.tgz",
                &sha,
                "4.0.0",
                "macos-arm64"
            )
            .is_none()
        );
    }

    #[test]
    fn standard_saved_before_its_modules_branch_moves_to_it() {
        let data = tempfile::tempdir().unwrap();
        let team = Source {
            name: "Team".into(),
            location: Location::Github {
                repository: "team/devtools".into(),
                branch: None,
                path: None,
            },
        };
        let mut old = Source::standard();
        old.location = Location::Github {
            repository: "Duhon/DevTool".into(),
            branch: None,
            path: None,
        };
        write_sources(data.path(), &[old, team.clone()]).unwrap();
        assert_eq!(sources(data.path()).unwrap(), [Source::standard(), team]);
    }

    #[test]
    fn the_latest_release_offers_the_application() {
        let mut release = json!({"tag_name": "devtool-v3.3.0", "draft": false, "prerelease": false,
            "body": "- Fixes", "assets": [
                {"id": 7, "name": updates::ARCHIVE, "size": 10, "digest": "sha256:ab"},
                {"id": 8, "name": RELEASE_FILE, "size": 1}]});
        let offer = release_offer(&release, "macos-arm64").unwrap();
        assert_eq!(
            (
                offer.id.as_str(),
                offer.version.as_str(),
                offer.notes.as_str()
            ),
            (APPLICATION_ID, "3.3.0", "- Fixes")
        );
        assert_eq!(
            offer.download,
            Download::Application {
                archive: 7,
                sha256: Some("ab".into()),
                metadata: 8
            }
        );
        assert!(offer.unsupported.is_none());
        assert!(
            release_offer(&release, "linux-x86_64")
                .unwrap()
                .unsupported
                .is_some()
        );
        release["tag_name"] = json!("module-rest-v1.0.0");
        assert!(release_offer(&release, "macos-arm64").is_none());
        assert_eq!(
            notes_section("## 2.0.0\n- b\n## 1.0.0\n- a\n", "1.0.0"),
            "- a"
        );
    }

    #[test]
    fn a_team_skills_folder_named_skills_becomes_an_applied_source() {
        let data = tempfile::tempdir().unwrap();
        let team = tempfile::tempdir().unwrap();
        write(
            team.path(),
            "skills/deploy/SKILL.md",
            "---\nname: deploy\ndescription: Deploys\n---\n",
        );
        write(
            data.path(),
            ".agent/skills.json",
            &json!({"teamFolder": team.path().join("skills")}).to_string(),
        );
        migrate_team_folder(data.path()).unwrap();
        let list = sources(data.path()).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].name, "Team");
        assert!(
            data.path()
                .join("sources/team/skills/team-deploy/SKILL.md")
                .is_file()
        );
        assert!(!data.path().join(".agent/skills.json").exists());

        let other = tempfile::tempdir().unwrap();
        write(
            other.path(),
            ".agent/skills.json",
            &json!({"teamFolder": team.path()}).to_string(),
        );
        migrate_team_folder(other.path()).unwrap();
        assert_eq!(sources(other.path()).unwrap().len(), 1);
        let warning = Sources::new(other.path()).get().unwrap()["warning"].clone();
        assert!(warning.as_str().unwrap().contains("skills/"));
    }
}
