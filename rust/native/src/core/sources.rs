//! Sources: an ordered list of named places that supply module packages and shared
//! configuration (Dictionary profiles, templates, hints and Agent skills): GitHub
//! repositories and local folders. A source is laid out as
//!
//! ```text
//! dictionary.json          {"order"?: [...], "profiles": {"<profile>": {"<VAR>": value}}}
//! templates.json           {"<module>": {"<section>": "<templates text>"}}
//! hints/<module>.md        the module's hints
//! skills/<name>/SKILL.md   Agent skills, with their files
//! modules/*.module.tgz     module packages (folders; GitHub repositories use releases
//!                          tagged module-<id>-v<version>)
//! ```
//!
//! Checking for changes is automatic; the user applies them. The applied copy of a source
//! lives in `<data>/sources/<slug>/`, its skills renamed `<slug>-<name>`. Configuration is
//! layered: later sources override earlier ones, and the user's own changes, which the
//! application keeps apart, override them all. An update replaces the applied copy and
//! never merges.

use super::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use services_contracts::package::{self, Package, archive};
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
pub const SKILLS: &str = "skills";
const MODULES: &str = "modules";
/// Slugs that would make skill names look like the application's own.
const RESERVED: &[&str] = &["module", "services", "local"];
/// The repository the standard modules are released from.
pub const STANDARD: &str = "duhon/devtool";
/// How long a check answers from its last result unless forced.
const CHECK_AGE: u64 = 15 * 60;
const MAX_FILE: u64 = 1024 * 1024;
const MAX_FILES: usize = 1000;
const MAX_PACKAGE: u64 = 256 * 1024 * 1024;
const MAX_RELEASE_PAGES: u32 = 5;
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
                branch: None,
                path: None,
            },
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
            .map(|settings| settings.sources)
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

/// A module package a source offers.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Offer {
    id: String,
    version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    min_core: Option<String>,
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
    /// A release asset and the SHA-256 GitHub reports for it.
    Asset {
        id: u64,
        sha256: Option<String>,
    },
    File {
        path: PathBuf,
    },
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

/// The packages in a folder source's `modules/`.
fn folder_offers(root: &Path, core: &str, platform: &str) -> (Vec<Offer>, Vec<String>) {
    let (mut offers, mut problems) = (Vec::new(), Vec::new());
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
            let described = archive::read(&bytes, package::PACKAGE_FILE)?
                .ok_or_else(|| format!("no {}", package::PACKAGE_FILE))?;
            let described = Package::parse(&described)?;
            let notes = archive::read(&bytes, package::NOTES_FILE)?
                .map(|notes| notes_section(&String::from_utf8_lossy(&notes), &described.version))
                .unwrap_or_default();
            Ok(Offer {
                unsupported: described.unsupported(core, platform),
                id: described.id,
                version: described.version,
                min_core: described.min_core,
                notes,
                size,
                download: Download::File { path: file.clone() },
            })
        };
        match read() {
            Ok(offer) => offers.push(offer),
            Err(problem) => problems.push(format!("{shown}: {problem}")),
        }
    }
    (offers, problems)
}

/// The module package a GitHub release offers for `platform`: a release tagged
/// `module-<id>-v<version>` with the asset `<id>-<version>-<platform>.module.tgz`.
fn release_offer(release: &Value, platform: &str) -> Option<Offer> {
    if release["draft"] == true || release["prerelease"] == true {
        return None;
    }
    let (id, version) = release["tag_name"]
        .as_str()?
        .strip_prefix("module-")?
        .rsplit_once("-v")?;
    package::check_id(id).ok()?;
    package::version(version).ok()?;
    let name = package::archive_name(&Package {
        id: id.into(),
        version: version.into(),
        platform: Some(platform.into()),
        ..Package::default()
    });
    let asset = release["assets"]
        .as_array()?
        .iter()
        .find(|asset| asset["name"] == name.as_str())?;
    Some(Offer {
        id: id.into(),
        version: version.into(),
        min_core: None,
        notes: release["body"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned(),
        size: asset["size"].as_u64().unwrap_or_default(),
        unsupported: None,
        download: Download::Asset {
            id: asset["id"].as_u64()?,
            sha256: asset["digest"]
                .as_str()
                .and_then(|digest| digest.strip_prefix("sha256:"))
                .map(str::to_owned),
        },
    })
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
        let mut request = self
            .client
            .get(format!("{API}/repos/{}/{path}", self.repository))
            .header(reqwest::header::ACCEPT, accept)
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|e| Error::new(format!("GitHub {}: {}", self.repository, e.without_url())))?;
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

    async fn offers(&self, platform: &str) -> Result<Vec<Offer>> {
        let mut offers = Vec::new();
        for page in 1..=MAX_RELEASE_PAGES {
            let releases = self
                .json(&format!("releases?per_page=100&page={page}"))
                .await?;
            let releases = releases.as_array().cloned().unwrap_or_default();
            offers.extend(
                releases
                    .iter()
                    .filter_map(|release| release_offer(release, platform)),
            );
            if releases.len() < 100 {
                break;
            }
        }
        Ok(offers)
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

/// The user's own configuration: what overrides source records.
#[derive(Default)]
pub struct Local {
    /// `(profile, variable)` pairs the user changed or added.
    pub dictionary: BTreeSet<(String, String)>,
    /// `(module, section)` templates the user changed.
    pub templates: BTreeSet<(String, String)>,
    /// Modules whose hints the user changed.
    pub hints: BTreeSet<String>,
}

/// A record that differs between the applied copy and the source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Change {
    kind: String,
    /// `profile/VARIABLE`, `module/section`, the module of hints or the skill name.
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
        (DICTIONARY, "dictionary", Some("profiles")),
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

fn overridden(change: &Change, local: &Local) -> bool {
    let pair = || {
        change
            .key
            .split_once('/')
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .unwrap_or_default()
    };
    match change.kind.as_str() {
        "dictionary" => local.dictionary.contains(&pair()),
        "template" => local.templates.contains(&pair()),
        "hint" => local.hints.contains(&change.key),
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------
// Applying a source.

/// Checks an applied copy before it replaces the current one.
fn check_snapshot(dir: &Path) -> Result<()> {
    if let Ok(bytes) = fs::read(dir.join(DICTIONARY)) {
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|e| Error::new(format!("{DICTIONARY}: {e}")))?;
        let profiles = value["profiles"].as_object().ok_or_else(|| {
            Error::new(format!(
                "{DICTIONARY} must hold {{\"profiles\": {{\"<profile>\": {{\"<VARIABLE>\": value}}}}}}"
            ))
        })?;
        for (name, variables) in profiles {
            if name.trim().is_empty() || name == "var" || name == "env" {
                return Err(Error::new(format!(
                    "{DICTIONARY}: \"{name}\" can't be a Dictionary profile"
                )));
            }
            if !variables.is_object() {
                return Err(Error::new(format!(
                    "{DICTIONARY}: profile \"{name}\" must be an object of variables"
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

async fn check_source(data: &Path, source: &Source, core: &str, platform: &str) -> Checked {
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
                github(data, repository).await?.offers(platform).await?
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
            a.id.cmp(&b.id).then_with(|| {
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
}

impl Sources {
    pub fn new(data: impl Into<PathBuf>) -> Self {
        Self { data: data.into() }
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

    /// Checks every source unless its last check is recent (`force` checks anyway):
    /// `{sources:[{name, slug, checkedAt, error?, configs, applied, changes:[{kind, key,
    /// change, overridden}], problems, modules:[offers]}], installed:{"<module>": {version,
    /// source?}}}`; `installed` lists the loaded modules as `(id, version)`.
    pub async fn check(
        &self,
        force: bool,
        local: &Local,
        installed: &[(String, String)],
    ) -> Result<Value> {
        let sources = sources(&self.data)?;
        let mut checks = read_checks(&self.data);
        let core = env!("CARGO_PKG_VERSION");
        let platform = package::host_platform();
        let mut changed = false;
        for source in &sources {
            let slug = source.slug();
            let fresh = checks.get(&slug).is_some_and(|checked| {
                checked.source == *source && now().saturating_sub(checked.checked_at) < CHECK_AGE
            });
            if force || !fresh {
                checks.insert(
                    slug,
                    check_source(&self.data, source, core, &platform).await,
                );
                changed = true;
            }
        }
        checks.retain(|slug, _| sources.iter().any(|source| source.slug() == *slug));
        if changed {
            write_checks(&self.data, &checks)?;
        }
        Ok(self.describe_checks(&sources, &checks, local, installed))
    }

    fn describe_checks(
        &self,
        sources: &[Source],
        checks: &BTreeMap<String, Checked>,
        local: &Local,
        installed: &[(String, String)],
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
                    .map(|change| {
                        let mut value = serde_json::to_value(change).unwrap_or_default();
                        value["overridden"] = json!(overridden(change, local));
                        value
                    })
                    .collect();
                Some(json!({
                    "name": source.name,
                    "slug": slug,
                    "checkedAt": checked.checked_at,
                    "error": checked.error,
                    "configs": checked.configs,
                    "applied": applied,
                    "changes": changes,
                    "problems": checked.problems,
                    "modules": checked.modules,
                }))
            })
            .collect();
        // Modules without a record came with the application before 3.2.0, from Standard.
        let records = module_sources(&self.data);
        let standard = sources.iter().find(|source| {
            matches!(&source.location, Location::Github { repository, .. } if repository.eq_ignore_ascii_case(STANDARD))
        });
        let origins: Map<String, Value> = installed
            .iter()
            .map(|(id, version)| {
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
                (id.clone(), json!({"version": version, "source": slug}))
            })
            .collect();
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

    /// Downloads version `version` of module `id` from the source `slug`, as its last check
    /// found it.
    pub async fn package(&self, slug: &str, id: &str, version: &str) -> Result<Vec<u8>> {
        let source = sources(&self.data)?
            .into_iter()
            .find(|source| source.slug() == slug)
            .ok_or_else(|| Error::new(format!("Unknown source \"{slug}\"")))?;
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
        if let Some(reason) = offer.unsupported {
            return Err(Error::new(reason));
        }
        match (&source.location, &offer.download) {
            (Location::Github { repository, .. }, Download::Asset { id: asset, sha256 }) => {
                let bytes = github(&self.data, repository)
                    .await?
                    .bytes(
                        &format!("releases/assets/{asset}"),
                        "application/octet-stream",
                        MAX_PACKAGE,
                    )
                    .await?;
                if let Some(expected) = sha256
                    && !self::sha256(&bytes).eq_ignore_ascii_case(expected)
                {
                    return Err(Error::new(format!(
                        "The downloaded {id} {version} is damaged (SHA-256 mismatch)"
                    )));
                }
                Ok(bytes)
            }
            (Location::Folder { path: root }, Download::File { path })
                if path.starts_with(root.join(MODULES)) =>
            {
                fs::read(path).map_err(|e| Error::new(format!("{}: {e}", path.display())))
            }
            _ => Err(Error::new("Check for updates again")),
        }
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

/// The sources service with `op`: get, save, token, check, apply. Install is in the runtime.
pub async fn dispatch(
    sources: &Sources,
    message: &Value,
    local: impl FnOnce() -> Local,
    installed: &[(String, String)],
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
        "check" => {
            sources
                .check(message["force"] == true, &local(), installed)
                .await
        }
        "apply" => {
            let slug = message["source"]
                .as_str()
                .ok_or_else(|| Error::new("Which source?"))?;
            sources.apply(slug).await
        }
        op => Err(Error::new(format!("Unknown sources op: {op}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            r#"{"profiles":{"stage":{"HOST":"a","PORT":"1"}}}"#,
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
        let mut local = Local::default();
        local.dictionary.insert(("stage".into(), "HOST".into()));
        let checked = sources.check(false, &local, &[]).await.unwrap();
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
                {"kind":"dictionary","key":"stage/HOST","change":"added","overridden":true},
                {"kind":"dictionary","key":"stage/PORT","change":"added","overridden":false},
                {"kind":"hint","key":"rest","change":"added","overridden":false},
                {"kind":"skill","key":"team-deploy","change":"added","overridden":false},
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
        write(
            team.path(),
            "dictionary.json",
            r#"{"profiles":{"stage":{"HOST":"b","PORT":"1"}}}"#,
        );
        let checked = sources.check(true, &local, &[]).await.unwrap();
        assert_eq!(
            checked["sources"][0]["changes"],
            json!([{"kind":"dictionary","key":"stage/HOST","change":"changed","overridden":true}])
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
            layer_files(data, DICTIONARY).unwrap()[0].1["profiles"]["stage"]["HOST"],
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
        };
        fs::create_dir_all(team.path().join(MODULES)).unwrap();
        for (version, min_core) in [("1.0.0", "1.0.0"), ("2.0.0", "99.0.0")] {
            fs::write(
                package.path().join(package::PACKAGE_FILE),
                serde_json::to_vec(&described(version, min_core)).unwrap(),
            )
            .unwrap();
            fs::write(package.path().join(package::LIBRARY_FILE), b"library").unwrap();
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
                &Local::default(),
                &[("redis".into(), "1.0.0".into())],
            )
            .await
            .unwrap();
        let modules = &checked["sources"][0]["modules"];
        assert_eq!(modules[0]["version"], "2.0.0");
        assert!(
            modules[0]["unsupported"]
                .as_str()
                .unwrap()
                .contains("99.0.0")
        );
        assert_eq!(modules[1]["version"], "1.0.0");
        assert_eq!(modules[1]["notes"], "- Notes of 1.0.0");
        assert!(
            checked["sources"][0]["problems"][0]
                .as_str()
                .unwrap()
                .contains("broken")
        );
        assert_eq!(
            checked["installed"],
            json!({"redis": {"version": "1.0.0", "source": null}})
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
    fn releases_offer_the_package_for_this_platform() {
        let release = |tag: &str, asset: &str| {
            json!({"tag_name": tag, "draft": false, "prerelease": false, "body": "- Fixes",
                "assets": [{"id": 7, "name": asset, "size": 10, "digest": "sha256:ab"}]})
        };
        let offer = release_offer(
            &release(
                "module-my-rest-v1.2.3",
                "my-rest-1.2.3-macos-arm64.module.tgz",
            ),
            "macos-arm64",
        )
        .unwrap();
        assert_eq!(
            (offer.id.as_str(), offer.version.as_str()),
            ("my-rest", "1.2.3")
        );
        assert_eq!(
            offer.download,
            Download::Asset {
                id: 7,
                sha256: Some("ab".into())
            }
        );
        assert_eq!(offer.notes, "- Fixes");
        assert!(
            release_offer(
                &release("module-rest-v1.2.3", "rest-1.2.3-macos-arm64.module.tgz"),
                "linux-x86_64"
            )
            .is_none()
        );
        assert!(release_offer(&release("devtool-v3.2.0", "x"), "macos-arm64").is_none());
        let mut draft = release("module-rest-v1.0.0", "rest-1.0.0-macos-arm64.module.tgz");
        draft["draft"] = json!(true);
        assert!(release_offer(&draft, "macos-arm64").is_none());
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
