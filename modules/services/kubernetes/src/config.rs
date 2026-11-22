//! The `#metadata` (request parameters sent to the API) and `#options` (app-side only) of a
//! check. Owned by the language and run part.
//!
//! The console owns `Repeat`, `Repeat-Until` and `Timeout` of `#options`; they never reach the
//! module and are not handled here.

use services_sdk::lang::key_value;
use services_sdk::lang::table::Options as ColumnOptions;

/// Valid `#metadata` keys; [`allowed`] says which of them an action accepts.
pub const METADATA: [&str; 15] = [
    "Selector",
    "Field-Selector",
    "Container",
    "Tail",
    "Since",
    "Previous",
    "Timestamps",
    "Follow",
    "Replicas",
    "Revision",
    "Patch-Type",
    "Dry-Run",
    "Force",
    "Grace-Period",
    "Limit",
];

/// Valid `#options` keys of the module; the console owns `Repeat`, `Repeat-Until` and `Timeout`.
pub const OPTIONS: [&str; 9] = [
    "Format",
    "Columns",
    "Table-Sort",
    "Table-Count",
    "Table-Offset",
    "Columns-Type",
    "Columns-Size",
    "Logs",
    "Grep",
];

/// Actions that change the cluster, for `Dry-Run`.
const WRITES: [&str; 9] = [
    "apply",
    "create",
    "patch",
    "scale",
    "rollout restart",
    "rollout undo",
    "delete",
    "label",
    "annotate",
];

/// The actions (as [`crate::action::Action::verb`]) a `#metadata` key is valid for; `None` for
/// an unknown key. The key is matched ignoring case.
pub fn actions_of(key: &str) -> Option<Vec<&'static str>> {
    Some(match key.to_ascii_lowercase().as_str() {
        "selector" => vec!["get", "logs", "events", "top", "delete", "watch"],
        "field-selector" => vec!["get", "events", "delete", "watch"],
        "container" | "tail" | "since" | "previous" | "timestamps" | "follow" => vec!["logs"],
        "replicas" => vec!["scale"],
        "revision" => vec!["rollout undo", "rollout history"],
        "patch-type" => vec!["patch"],
        "dry-run" => WRITES.to_vec(),
        "force" => vec!["apply"],
        "grace-period" => vec!["delete"],
        "limit" => vec!["get", "events"],
        _ => return None,
    })
}

/// Whether the `#metadata` key is valid for the action `verb`; hints list only such keys.
pub fn allowed(verb: &str, key: &str) -> bool {
    actions_of(key).is_some_and(|verbs| verbs.contains(&verb))
}

/// `Since`: how far back logs start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Since {
    Seconds(u64),
    /// An RFC 3339 time as written.
    Time(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatchType {
    Merge,
    Json,
    Strategic,
    Apply,
}

impl PatchType {
    /// The `Content-Type` of the patch.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Merge => "application/merge-patch+json",
            Self::Json => "application/json-patch+json",
            Self::Strategic => "application/strategic-merge-patch+json",
            Self::Apply => "application/apply-patch+yaml",
        }
    }
}

/// The request parameters of `#metadata`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata {
    pub selector: Option<String>,
    pub field_selector: Option<String>,
    pub container: Option<String>,
    pub tail: Option<u64>,
    pub since: Option<Since>,
    pub previous: bool,
    pub timestamps: bool,
    pub follow: bool,
    pub replicas: Option<u32>,
    pub revision: Option<u64>,
    pub patch_type: Option<PatchType>,
    pub dry_run: bool,
    pub force: bool,
    pub grace_period: Option<u64>,
    pub limit: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    #[default]
    Raw,
    Table,
    Json,
    Yaml,
}

/// The app-side `#options`.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub format: Format,
    /// Server column names or jsonpath-like paths, as written.
    pub columns: Vec<String>,
    /// A column; a leading `-` sorts descending.
    pub sort: Option<String>,
    /// Rows shown; `0` shows all.
    pub count: usize,
    pub offset: usize,
    pub columns_type: Option<String>,
    pub columns_size: Option<String>,
    /// `Logs: json`: every log line is a JSON object.
    pub logs_json: bool,
    /// A regular expression over lines or objects.
    pub grep: Option<String>,
}

pub const DEFAULT_COUNT: usize = 100;

impl Default for Options {
    fn default() -> Self {
        Self {
            format: Format::Raw,
            columns: Vec::new(),
            sort: None,
            count: DEFAULT_COUNT,
            offset: 0,
            columns_type: None,
            columns_size: None,
            logs_json: false,
            grep: None,
        }
    }
}

/// The parsed sections.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    pub metadata: Metadata,
    pub options: Options,
    /// The `#metadata` keys that were given, in their canonical spelling.
    pub keys: Vec<&'static str>,
}

fn flag(key: &str, value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("{key} takes true or false, not {value:?}")),
    }
}

fn number(key: &str, value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{key} takes a whole number, not {value:?}"))
}

fn nonempty(key: &str, value: &str) -> Result<String, String> {
    if value.is_empty() {
        Err(format!("{key} needs a value"))
    } else {
        Ok(value.into())
    }
}

/// `15m`, `30s`, `2h` in seconds.
fn duration(value: &str) -> Option<u64> {
    let unit = value.chars().last()?;
    let seconds = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        _ => return None,
    };
    let count: u64 = value[..value.len() - 1].parse().ok()?;
    count.checked_mul(seconds)
}

/// `2024-05-01T10:00:00Z`, with optional fraction and an offset.
fn rfc3339(value: &str) -> bool {
    let bytes = value.as_bytes();
    let digits = |from: usize, to: usize| {
        bytes
            .get(from..to)
            .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
    };
    if bytes.len() < 20
        || !digits(0, 4)
        || bytes[4] != b'-'
        || !digits(5, 7)
        || bytes[7] != b'-'
        || !digits(8, 10)
        || !matches!(bytes[10], b'T' | b't' | b' ')
        || !digits(11, 13)
        || bytes[13] != b':'
        || !digits(14, 16)
        || bytes[16] != b':'
        || !digits(17, 19)
    {
        return false;
    }
    let mut at = 19;
    if bytes[at] == b'.' {
        at += 1;
        let start = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        if at == start {
            return false;
        }
    }
    match bytes.get(at) {
        Some(b'Z' | b'z') => at + 1 == bytes.len(),
        Some(b'+' | b'-') => {
            bytes.len() == at + 6
                && digits(at + 1, at + 3)
                && bytes[at + 3] == b':'
                && digits(at + 4, at + 6)
        }
        _ => false,
    }
}

fn since(value: &str) -> Result<Since, String> {
    if let Some(seconds) = duration(value) {
        return Ok(Since::Seconds(seconds));
    }
    if rfc3339(value) {
        return Ok(Since::Time(value.into()));
    }
    Err(format!(
        "Since takes a duration like 15m, 30s or 2h, or an RFC 3339 time, not {value:?}"
    ))
}

/// Splits a `Columns` value at the commas outside brackets.
fn columns(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let (mut depth, mut current) = (0usize, String::new());
    for c in value.chars() {
        match c {
            '[' | '{' => depth += 1,
            ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    parts.push(current);
    parts
        .into_iter()
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect()
}

fn metadata(text: &str) -> Result<(Metadata, Vec<&'static str>), String> {
    let mut data = Metadata::default();
    let mut keys: Vec<&'static str> = Vec::new();
    for (name, value) in key_value::pairs(text) {
        let Some(key) = METADATA
            .iter()
            .find(|key| key.eq_ignore_ascii_case(&name))
            .copied()
        else {
            return Err(format!(
                "Unknown metadata key {name:?}; Kubernetes takes {}",
                METADATA.join(", ")
            ));
        };
        if keys.contains(&key) {
            return Err(format!("Duplicate metadata key {key}"));
        }
        keys.push(key);
        match key {
            "Selector" => data.selector = Some(nonempty(key, &value)?),
            "Field-Selector" => data.field_selector = Some(nonempty(key, &value)?),
            "Container" => data.container = Some(nonempty(key, &value)?),
            "Tail" => data.tail = Some(number(key, &value)?),
            "Since" => data.since = Some(since(&value)?),
            "Previous" => data.previous = flag(key, &value)?,
            "Timestamps" => data.timestamps = flag(key, &value)?,
            "Follow" => data.follow = flag(key, &value)?,
            "Replicas" => {
                data.replicas = Some(
                    u32::try_from(number(key, &value)?)
                        .map_err(|_| format!("{key} is too large: {value}"))?,
                )
            }
            "Revision" => data.revision = Some(number(key, &value)?),
            "Patch-Type" => {
                data.patch_type = Some(match value.to_ascii_lowercase().as_str() {
                    "merge" => PatchType::Merge,
                    "json" => PatchType::Json,
                    "strategic" => PatchType::Strategic,
                    "apply" => PatchType::Apply,
                    _ => {
                        return Err(format!(
                            "Patch-Type takes merge, json, strategic or apply, not {value:?}"
                        ));
                    }
                })
            }
            "Dry-Run" => match value.to_ascii_lowercase().as_str() {
                "server" => data.dry_run = true,
                _ => return Err(format!("Dry-Run takes server, not {value:?}")),
            },
            "Force" => data.force = flag(key, &value)?,
            "Grace-Period" => data.grace_period = Some(number(key, &value)?),
            "Limit" => {
                let limit = number(key, &value)?;
                if limit == 0 {
                    return Err("Limit takes a number of 1 or more".into());
                }
                data.limit = Some(limit);
            }
            _ => unreachable!("every key is listed"),
        }
    }
    Ok((data, keys))
}

fn options(text: &str) -> Result<Options, String> {
    let mut data = Options::default();
    let mut seen: Vec<String> = Vec::new();
    for (name, value) in key_value::pairs(text) {
        let key = name.to_ascii_lowercase();
        if seen.contains(&key) {
            return Err(format!("Duplicate option {name}"));
        }
        seen.push(key.clone());
        match key.as_str() {
            "format" => {
                data.format = match value.to_ascii_lowercase().as_str() {
                    "raw" => Format::Raw,
                    "table" => Format::Table,
                    "json" => Format::Json,
                    "yaml" => Format::Yaml,
                    _ => {
                        return Err(format!(
                            "Format takes raw, table, json or yaml, not {value:?}"
                        ));
                    }
                }
            }
            "columns" => data.columns = columns(&value),
            "table-sort" => data.sort = Some(nonempty("Table-Sort", &value)?),
            "table-count" => data.count = number("Table-Count", &value)? as usize,
            "table-offset" => data.offset = number("Table-Offset", &value)? as usize,
            "columns-type" => {
                ColumnOptions::parse(Some(&value), None)?;
                data.columns_type = Some(value);
            }
            "columns-size" => {
                ColumnOptions::parse(None, Some(&value))?;
                data.columns_size = Some(value);
            }
            "logs" => {
                data.logs_json = match value.to_ascii_lowercase().as_str() {
                    "raw" => false,
                    "json" => true,
                    _ => return Err(format!("Logs takes raw or json, not {value:?}")),
                }
            }
            "grep" => {
                regex::Regex::new(&value)
                    .map_err(|error| format!("Grep is not a valid regular expression: {error}"))?;
                data.grep = Some(value);
            }
            _ => {
                return Err(format!(
                    "Unknown option {name:?}; Kubernetes takes {}",
                    OPTIONS.join(", ")
                ));
            }
        }
    }
    Ok(data)
}

/// Reads `#metadata` and `#options`; [`Config::validate`] says whether the keys fit the action.
pub fn parse(metadata_text: &str, options_text: &str) -> Result<Config, String> {
    let (metadata, keys) = metadata(metadata_text)?;
    Ok(Config {
        metadata,
        options: options(options_text)?,
        keys,
    })
}

impl Config {
    /// Errors for a `#metadata` key the action `verb` does not take.
    pub fn validate(&self, verb: &str) -> Result<(), String> {
        for key in &self.keys {
            if !allowed(verb, key) {
                let valid: Vec<_> = METADATA
                    .iter()
                    .filter(|other| allowed(verb, other))
                    .copied()
                    .collect();
                let takes = if valid.is_empty() {
                    format!("{verb} takes no metadata")
                } else {
                    format!("{verb} takes {}", valid.join(", "))
                };
                return Err(format!("{key} is not valid for {verb}; {takes}"));
            }
        }
        Ok(())
    }
}
