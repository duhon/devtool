use chrono::DateTime;
use kafka_contract::{
    Bytes, ConfigChange, Connection, FetchMode, Header, Isolation, Limits, Offset, Operation,
    ProducerRecord, Request, Sasl, Security, Start, Tls, VERSION,
};
use serde::{Deserialize, Serialize};
use services_sdk::Check;
use std::collections::{BTreeMap, BTreeSet};

pub const COMMANDS: &[&str] = &[
    "BROKERS", "TOPICS", "DESCRIBE", "OFFSETS", "READ", "TAIL", "PRODUCE", "GROUPS", "GROUP",
    "LAG", "CONFIG", "CREATE", "DELETE", "ALTER", "PURGE", "RESET", "COMMIT",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Table,
    Vertical,
    Json,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Defaults {
    pub format: Format,
    pub isolation: Isolation,
    pub timeout_ms: u64,
    pub max_bytes: usize,
    pub scan_limit: u32,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            format: Format::Table,
            isolation: Isolation::ReadCommitted,
            timeout_ms: 30_000,
            max_bytes: 1024 * 1024,
            scan_limit: 1000,
        }
    }
}

impl Defaults {
    pub fn validate(&self) -> Result<(), String> {
        Limits {
            max_records: self.scan_limit,
            max_bytes: self.max_bytes,
            timeout_ms: self.timeout_ms,
        }
        .validate()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Source {
    pub target: String,
    pub action: String,
    pub metadata: String,
    pub options: String,
    pub payload: String,
    pub defaults: Defaults,
}

impl Source {
    pub fn new(check: &Check, defaults: &Defaults) -> Self {
        Self {
            target: check.text_of("target").into(),
            action: check.text_of("action").into(),
            metadata: check.text_of("metadata").into(),
            options: check.text_of("options").into(),
            payload: check.text_of("payload").into(),
            defaults: defaults.clone(),
        }
    }

    pub fn prepare(self) -> Result<PreparedSource, String> {
        Ok(PreparedSource {
            target: self.target,
            action: words(&self.action)?,
            metadata: pairs(&self.metadata)?,
            options: pairs(&self.options)?,
            payload: self.payload,
            defaults: self.defaults,
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PreparedSource {
    pub target: String,
    pub action: Vec<String>,
    pub metadata: Vec<(String, String)>,
    pub options: Vec<(String, String)>,
    pub payload: String,
    pub defaults: Defaults,
}

#[derive(Clone)]
pub enum Filter {
    Key(String),
    ValueContains(String),
}

impl Filter {
    pub fn matches(&self, record: &kafka_contract::Record) -> bool {
        match self {
            Self::Key(value) => record
                .key
                .as_ref()
                .is_some_and(|key| key.decode() == value.as_bytes()),
            Self::ValueContains(value) => record.value.as_ref().is_some_and(|bytes| {
                String::from_utf8(bytes.decode()).is_ok_and(|text| text.contains(value))
            }),
        }
    }
}

pub struct Plan {
    pub request: Request,
    pub format: Format,
    pub limit: u32,
    pub filter: Option<Filter>,
    pub lag: bool,
}

pub fn pairs(text: &str) -> Result<Vec<(String, String)>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (key, value) = line
                .split_once(':')
                .filter(|(key, _)| !key.trim().is_empty())
                .ok_or("Expected Name: value")?;
            Ok((key.trim().to_ascii_lowercase(), value.trim().into()))
        })
        .collect()
}

pub fn words(text: &str) -> Result<Vec<String>, String> {
    let mut chars = text.chars().peekable();
    let mut result = Vec::new();
    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch == ',' {
            result.push(",".into());
            continue;
        }
        let mut word = String::new();
        if ch == '\'' || ch == '"' {
            loop {
                match chars.next() {
                    Some(c) if c == ch => {
                        if chars.peek() == Some(&ch) {
                            chars.next();
                            word.push(ch);
                        } else {
                            break;
                        }
                    }
                    Some(c) => word.push(c),
                    None => return Err("Unterminated quote in #action".into()),
                }
            }
            if chars
                .peek()
                .is_some_and(|c| !c.is_whitespace() && *c != ',')
            {
                return Err("Quoted argument must end at whitespace or comma".into());
            }
        } else {
            word.push(ch);
            while let Some(c) = chars.next_if(|c| !c.is_whitespace() && *c != ',') {
                word.push(c);
            }
        }
        result.push(word);
    }
    Ok(result)
}

fn take<'a>(words: &'a [String], at: &mut usize) -> Result<&'a str, String> {
    let word = words.get(*at).ok_or("Missing argument in #action")?;
    *at += 1;
    Ok(word)
}

fn resource(words: &[String], at: &mut usize) -> Result<String, String> {
    let word = take(words, at)?;
    let name = word.strip_prefix('/').unwrap_or(word);
    if name.is_empty() {
        return Err("Topic/group name is required".into());
    }
    Ok(name.into())
}

fn start(words: &[String], at: &mut usize) -> Result<Start, String> {
    let word = take(words, at)?;
    Ok(match word.to_ascii_uppercase().as_str() {
        "BEGINNING" => Start::Beginning,
        "END" => Start::End,
        "LAST" => Start::Last(
            take(words, at)?
                .parse()
                .map_err(|_| "LAST takes a positive integer")?,
        ),
        "OFFSET" => Start::Offset(Offset::new(
            take(words, at)?.parse().map_err(|_| "Invalid offset")?,
        )?),
        "TIMESTAMP" => {
            let text = take(words, at)?;
            let millis = text
                .parse::<i64>()
                .or_else(|_| DateTime::parse_from_rfc3339(text).map(|time| time.timestamp_millis()))
                .map_err(|_| "Timestamp takes epoch milliseconds or RFC3339 with a timezone")?;
            Start::Timestamp(millis)
        }
        _ => return Err("Use BEGINNING, END, LAST n, OFFSET n or TIMESTAMP value".into()),
    })
}

pub fn plan(source: Source) -> Result<Plan, String> {
    plan_prepared(source.prepare()?)
}

pub fn plan_prepared(source: PreparedSource) -> Result<Plan, String> {
    source.defaults.validate()?;
    let mut format = source.defaults.format;
    let mut isolation = source.defaults.isolation;
    let mut limits = Limits {
        max_records: 100,
        max_bytes: source.defaults.max_bytes,
        timeout_ms: source.defaults.timeout_ms,
    };
    let mut scan_limit = source.defaults.scan_limit;
    let mut config = BTreeMap::new();
    for (key, value) in source.options {
        if matches!(key.as_str(), "repeat" | "repeat-until") {
            continue;
        }
        if config.insert(key, value).is_some() {
            return Err("Duplicate #options key".into());
        }
    }
    for (key, value) in &config {
        match key.as_str() {
            "format" => {
                format = match value.to_ascii_lowercase().as_str() {
                    "table" => Format::Table,
                    "vertical" => Format::Vertical,
                    "json" => Format::Json,
                    _ => return Err("Format takes table, vertical or json".into()),
                }
            }
            "isolation" => {
                isolation = match value.to_ascii_lowercase().as_str() {
                    "read-committed" => Isolation::ReadCommitted,
                    "read-uncommitted" => Isolation::ReadUncommitted,
                    _ => return Err("Isolation takes read-committed or read-uncommitted".into()),
                }
            }
            "timeout" => {
                limits.timeout_ms = value
                    .strip_suffix("ms")
                    .map(str::trim)
                    .map(|v| v.parse::<u64>().map_err(|_| "Invalid Timeout"))
                    .unwrap_or_else(|| {
                        value
                            .strip_suffix('s')
                            .unwrap_or(value)
                            .trim()
                            .parse::<u64>()
                            .map_err(|_| "Invalid Timeout")
                            .and_then(|n| n.checked_mul(1000).ok_or("Timeout overflow"))
                    })?;
            }
            "max-bytes" => {
                limits.max_bytes = value
                    .parse()
                    .map_err(|_| "Max-Bytes takes bytes as an integer")?
            }
            "scan-limit" => scan_limit = value.parse().map_err(|_| "Invalid Scan-Limit")?,
            "security-protocol" | "sasl-mechanism" | "sasl-username" | "sasl-password"
            | "sasl-token" | "ca-pem" | "certificate-pem" | "key-pem" | "payload-encoding"
            | "tombstone" => {}
            _ => return Err(format!("Unknown Kafka option {key:?}")),
        }
    }
    let get = |key: &str| config.get(key).map(String::as_str).unwrap_or("");
    let security = match get("security-protocol").to_ascii_lowercase().as_str() {
        "" | "plaintext" => Security::Plaintext,
        "ssl" => Security::Ssl,
        "sasl_plaintext" => Security::SaslPlaintext,
        "sasl_ssl" => Security::SaslSsl,
        _ => {
            return Err(
                "Security-Protocol takes plaintext, ssl, sasl_plaintext or sasl_ssl".into(),
            );
        }
    };
    let sasl = if [
        "sasl-mechanism",
        "sasl-username",
        "sasl-password",
        "sasl-token",
    ]
    .iter()
    .any(|key| config.contains_key(*key))
    {
        let username = get("sasl-username").to_owned();
        let password = get("sasl-password").to_owned();
        let mechanism = get("sasl-mechanism").to_ascii_uppercase();
        if mechanism != "OAUTHBEARER" && config.contains_key("sasl-token")
            || mechanism == "OAUTHBEARER"
                && (config.contains_key("sasl-username") || config.contains_key("sasl-password"))
        {
            return Err("Do not mix token and username/password authentication".into());
        }
        Some(match mechanism.as_str() {
            "PLAIN" => Sasl::Plain { username, password },
            "SCRAM-SHA-256" => Sasl::ScramSha256 { username, password },
            "SCRAM-SHA-512" => Sasl::ScramSha512 { username, password },
            "OAUTHBEARER" => Sasl::OAuthbearer {
                token: get("sasl-token").into(),
            },
            _ => return Err("Sasl-Mechanism takes PLAIN, SCRAM-SHA-256/512 or OAUTHBEARER".into()),
        })
    } else {
        None
    };
    let tls = if ["ca-pem", "certificate-pem", "key-pem"]
        .iter()
        .any(|k| config.contains_key(*k))
    {
        Some(Tls {
            ca_pem: config.get("ca-pem").cloned(),
            certificate_pem: config.get("certificate-pem").cloned(),
            key_pem: config.get("key-pem").cloned(),
        })
    } else {
        None
    };
    let connection = Connection {
        brokers: source.target.split(',').map(broker).collect(),
        security,
        sasl,
        tls,
    };
    let words = source.action;
    let command = words
        .first()
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or("TOPICS".into());
    let mut at = usize::from(!words.is_empty());
    let mut operation = match command.as_str() {
        "BROKERS" => Operation::DescribeCluster,
        "TOPICS" => Operation::ListTopics,
        "DESCRIBE" => Operation::DescribeTopics {
            topics: vec![resource(&words, &mut at)?],
        },
        "GROUPS" => Operation::ListGroups,
        "GROUP" => Operation::DescribeGroups {
            groups: vec![resource(&words, &mut at)?],
        },
        "LAG" => Operation::ListGroupOffsets {
            group: resource(&words, &mut at)?,
        },
        "CONFIG" => Operation::DescribeConfigs {
            topic: resource(&words, &mut at)?,
        },
        "OFFSETS" => Operation::ListOffsets {
            topic: resource(&words, &mut at)?,
            partitions: None,
            at: Start::End,
            isolation,
        },
        "READ" | "TAIL" => Operation::Fetch {
            topic: resource(&words, &mut at)?,
            partitions: None,
            start: if command == "TAIL" {
                Start::End
            } else {
                Start::Last(20)
            },
            mode: if command == "TAIL" {
                FetchMode::Live
            } else {
                FetchMode::Snapshot
            },
            isolation,
        },
        "PRODUCE" => Operation::Produce {
            record: ProducerRecord {
                topic: resource(&words, &mut at)?,
                partition: None,
                key: None,
                value: None,
                headers: Vec::new(),
            },
        },
        "CREATE" => Operation::CreateTopic {
            topic: resource(&words, &mut at)?,
            partitions: None,
            replicas: None,
            configs: topic_configs(&source.payload)?,
        },
        "DELETE" if keyword(&words, at, "GROUP") => {
            at += 1;
            Operation::DeleteGroup {
                group: resource(&words, &mut at)?,
            }
        }
        "DELETE" => Operation::DeleteTopic {
            topic: resource(&words, &mut at)?,
        },
        "ALTER" if keyword(&words, at, "CONFIG") => {
            at += 1;
            let topic = resource(&words, &mut at)?;
            let changes = config_changes(&words[at..])?;
            at = words.len();
            Operation::AlterConfigs { topic, changes }
        }
        "ALTER" => {
            let topic = resource(&words, &mut at)?;
            if !keyword(&words, at, "PARTITIONS") {
                return Err(
                    "Use ALTER /topic PARTITIONS n or ALTER CONFIG /topic key=value".into(),
                );
            }
            at += 1;
            let total = take(&words, &mut at)?
                .parse()
                .map_err(|_| "PARTITIONS takes the new partition total")?;
            Operation::CreatePartitions { topic, total }
        }
        "PURGE" => Operation::DeleteRecords {
            topic: resource(&words, &mut at)?,
            partitions: None,
            before: None,
        },
        "RESET" | "COMMIT" => Operation::AlterGroupOffsets {
            group: resource(&words, &mut at)?,
            topic: resource(&words, &mut at)?,
            partitions: None,
            // Replaced by the required TO or OFFSET clause.
            to: Start::Last(0),
        },
        _ => {
            return Err(format!(
                "Unknown Kafka command; use {}",
                COMMANDS.join(", ")
            ));
        }
    };
    let mut limit = if command == "READ" { 20 } else { 100 };
    let mut filter = None;
    let mut seen = BTreeSet::new();
    while at < words.len() {
        let clause = take(&words, &mut at)?.to_ascii_uppercase();
        let canonical = if clause == "PARTITION" {
            "PARTITIONS"
        } else {
            &clause
        };
        if !seen.insert(canonical.to_owned()) {
            return Err(format!("Duplicate clause {clause}"));
        }
        match clause.as_str() {
            "LIMIT" if command != "PRODUCE" && !command_changes(&command) => {
                limit = take(&words, &mut at)?
                    .parse()
                    .map_err(|_| "LIMIT takes a positive integer")?;
            }
            "REPLICAS" => match &mut operation {
                Operation::CreateTopic { replicas, .. } => {
                    *replicas = Some(
                        take(&words, &mut at)?
                            .parse()
                            .map_err(|_| "REPLICAS takes the replication factor")?,
                    )
                }
                _ => return Err("REPLICAS takes CREATE".into()),
            },
            "BEFORE" => match &mut operation {
                Operation::DeleteRecords { before, .. } if keyword(&words, at, "OFFSET") => {
                    at += 1;
                    *before = Some(Offset::new(
                        take(&words, &mut at)?
                            .parse()
                            .map_err(|_| "Invalid offset")?,
                    )?);
                }
                _ => return Err("BEFORE OFFSET n takes PURGE".into()),
            },
            "TO" if command == "RESET" => {
                if let Operation::AlterGroupOffsets { to, .. } = &mut operation {
                    *to = start(&words, &mut at)?;
                }
            }
            "OFFSET" if command == "COMMIT" => {
                if let Operation::AlterGroupOffsets { to, .. } = &mut operation {
                    *to = Start::Offset(Offset::new(
                        take(&words, &mut at)?
                            .parse()
                            .map_err(|_| "Invalid offset")?,
                    )?);
                }
            }
            "PARTITION" | "PARTITIONS" => {
                let mut ids = vec![
                    take(&words, &mut at)?
                        .parse::<i32>()
                        .map_err(|_| "Invalid partition")?,
                ];
                if clause == "PARTITIONS" {
                    while words.get(at).is_some_and(|w| w == ",") {
                        at += 1;
                        ids.push(
                            take(&words, &mut at)?
                                .parse()
                                .map_err(|_| "Invalid partition")?,
                        );
                    }
                }
                match &mut operation {
                    Operation::Fetch { partitions, .. }
                    | Operation::ListOffsets { partitions, .. }
                    | Operation::DeleteRecords { partitions, .. }
                    | Operation::AlterGroupOffsets { partitions, .. } => *partitions = Some(ids),
                    Operation::Produce { record } if ids.len() == 1 => {
                        record.partition = Some(ids[0])
                    }
                    Operation::CreateTopic { partitions, .. }
                        if clause == "PARTITIONS" && ids.len() == 1 =>
                    {
                        *partitions = Some(ids[0])
                    }
                    _ => {
                        return Err(
                            "PARTITION(S) takes READ, TAIL, OFFSETS, PRODUCE, CREATE, PURGE, RESET or COMMIT"
                                .into(),
                        );
                    }
                }
            }
            "FROM" => match &mut operation {
                Operation::Fetch { start: value, .. } => *value = start(&words, &mut at)?,
                _ => return Err("FROM takes READ or TAIL".into()),
            },
            "AT" => match &mut operation {
                Operation::ListOffsets { at: value, .. } => *value = start(&words, &mut at)?,
                _ => return Err("AT takes OFFSETS".into()),
            },
            "WHERE" if matches!(operation, Operation::Fetch { .. }) => {
                let field = take(&words, &mut at)?.to_ascii_uppercase();
                let op = take(&words, &mut at)?.to_ascii_uppercase();
                let value = take(&words, &mut at)?.to_owned();
                filter = Some(match (field.as_str(), op.as_str()) {
                    ("KEY", "=") => Filter::Key(value),
                    ("VALUE", "CONTAINS") => Filter::ValueContains(value),
                    _ => return Err("WHERE supports KEY = 'text' or VALUE CONTAINS 'text'".into()),
                });
            }
            _ => return Err(format!("Unexpected Kafka clause {clause:?}")),
        }
    }
    if limit == 0 || limit > kafka_contract::MAX_RECORDS {
        return Err(format!("LIMIT must be 1-{}", kafka_contract::MAX_RECORDS));
    }
    if matches!(
        operation,
        Operation::AlterGroupOffsets {
            to: Start::Last(0),
            ..
        }
    ) {
        return Err(if command == "COMMIT" {
            "COMMIT takes OFFSET n".into()
        } else {
            "RESET takes TO BEGINNING, END, OFFSET n or TIMESTAMP value".into()
        });
    }
    if scan_limit == 0 || scan_limit > kafka_contract::MAX_RECORDS {
        return Err(format!(
            "Scan-Limit must be 1-{}",
            kafka_contract::MAX_RECORDS
        ));
    }
    if let Operation::Produce { record } = &mut operation {
        let mut keys = BTreeSet::new();
        for (key, value) in source.metadata {
            if !matches!(key.as_str(), "header" | "header-base64" | "null-header")
                && !keys.insert(key.clone())
            {
                return Err("Duplicate message metadata".into());
            }
            match key.as_str() {
                "key" | "key-base64" => {
                    if record.key.is_some() {
                        return Err("Use Key or Key-Base64, not both".into());
                    }
                    record.key = Some(if key == "key" {
                        Bytes::new(value.as_bytes())
                    } else {
                        serde_json::from_value(serde_json::Value::String(value))
                            .map_err(|_| "Invalid Key-Base64")?
                    });
                }
                "header" | "header-base64" => {
                    let (name, content) = value.split_once('=').ok_or("Header takes name=value")?;
                    let bytes = if key == "header" {
                        Bytes::new(content.as_bytes())
                    } else {
                        serde_json::from_value(serde_json::Value::String(content.into()))
                            .map_err(|_| "Invalid Header-Base64")?
                    };
                    record.headers.push(Header {
                        name: name.into(),
                        value: Some(bytes),
                    });
                }
                "null-header" => record.headers.push(Header {
                    name: value,
                    value: None,
                }),
                _ => {
                    return Err(
                        "Metadata takes Key, Key-Base64, Header, Header-Base64 or Null-Header"
                            .into(),
                    );
                }
            }
        }
        let tombstone = match get("tombstone").to_ascii_lowercase().as_str() {
            "" | "false" => false,
            "true" => true,
            _ => return Err("Tombstone takes true or false".into()),
        };
        if tombstone && !source.payload.is_empty() {
            return Err("Tombstone takes no #payload".into());
        }
        if !tombstone {
            record.value = Some(
                match get("payload-encoding").to_ascii_lowercase().as_str() {
                    "" | "text" => Bytes::new(source.payload.as_bytes()),
                    "json" => {
                        serde_json::from_str::<serde_json::Value>(&source.payload)
                            .map_err(|_| "Invalid JSON payload")?;
                        Bytes::new(source.payload.as_bytes())
                    }
                    "base64" => serde_json::from_value(serde_json::Value::String(
                        source.payload.trim().into(),
                    ))
                    .map_err(|_| "Invalid base64 payload")?,
                    _ => return Err("Payload-Encoding takes text, json or base64".into()),
                },
            );
        }
    } else if !source.payload.trim().is_empty() && command != "CREATE"
        || !source.metadata.is_empty()
        || config.contains_key("tombstone")
        || config.contains_key("payload-encoding")
    {
        return Err("Payload, metadata and producer options take PRODUCE only (CREATE reads topic configs from #payload)".into());
    }
    limits.max_records = if filter.is_some() {
        if scan_limit < limit {
            return Err("Scan-Limit must be at least LIMIT".into());
        }
        scan_limit
    } else {
        limit
    };
    let request = Request {
        version: VERSION,
        connection,
        operation,
        limits,
    };
    request.validate()?;
    Ok(Plan {
        request,
        format,
        limit,
        filter,
        lag: command == "LAG",
    })
}

/// A bootstrap address: `host:port`, also accepted as `kafka://host:port`.
fn broker(address: &str) -> String {
    let address = address.trim();
    let address = match address.get(..8) {
        Some(scheme) if scheme.eq_ignore_ascii_case("kafka://") => &address[8..],
        _ => address,
    };
    address.trim_end_matches('/').to_owned()
}

/// Whether the command changes the cluster.
fn command_changes(command: &str) -> bool {
    matches!(
        command,
        "CREATE" | "DELETE" | "ALTER" | "PURGE" | "RESET" | "COMMIT"
    )
}

/// The word at `at` is the bare keyword (a `/`-prefixed name never is).
fn keyword(words: &[String], at: usize, keyword: &str) -> bool {
    words
        .get(at)
        .is_some_and(|w| w.eq_ignore_ascii_case(keyword))
}

/// The topic configs of a CREATE: one `key=value` per #payload line.
fn topic_configs(payload: &str) -> Result<BTreeMap<String, String>, String> {
    let mut configs = BTreeMap::new();
    for line in payload.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (key, value) = line
            .split_once('=')
            .filter(|(key, _)| !key.trim().is_empty())
            .ok_or("CREATE takes topic configs as key=value lines in #payload")?;
        if configs
            .insert(key.trim().to_owned(), value.trim().to_owned())
            .is_some()
        {
            return Err(format!("Duplicate topic config {:?}", key.trim()));
        }
    }
    Ok(configs)
}

/// `key=value` sets a key, `key=` resets it to the default. A value may hold commas
/// (`cleanup.policy=compact,delete`).
fn config_changes(words: &[String]) -> Result<Vec<ConfigChange>, String> {
    let mut joined: Vec<String> = Vec::new();
    let mut glue = false;
    for word in words {
        match joined.last_mut() {
            Some(last) if word == "," || glue => {
                last.push_str(word);
                glue = word == ",";
            }
            _ => joined.push(word.clone()),
        }
    }
    if joined.is_empty() {
        return Err("ALTER CONFIG takes one or more key=value".into());
    }
    joined
        .into_iter()
        .map(|pair| {
            let (name, value) = pair
                .split_once('=')
                .filter(|(name, _)| !name.is_empty())
                .ok_or("ALTER CONFIG takes key=value; key= resets the default")?;
            Ok(ConfigChange {
                name: name.into(),
                value: (!value.is_empty()).then(|| value.into()),
            })
        })
        .collect()
}
