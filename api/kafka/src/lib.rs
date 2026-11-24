//! Wire types of the native `kafka` driver (host/src/drivers/kafka.rs). No Kafka client or SDK dependency.

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::BTreeSet;

pub const DRIVER: &str = "kafka";
pub const VERSION: u32 = 1;
pub const MAX_RECORDS: u32 = 10_000;
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_TIMEOUT_MS: u64 = 300_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Offset(String);

impl Offset {
    pub fn new(value: i64) -> Result<Self, String> {
        if value < 0 {
            return Err("Offset must be non-negative".into());
        }
        Ok(Self(value.to_string()))
    }

    pub fn value(&self) -> i64 {
        // Construction and deserialization both enforce a canonical, non-negative i64.
        self.0.parse().expect("validated offset")
    }

    pub fn next(&self) -> Result<Self, String> {
        Self::new(self.value().checked_add(1).ok_or("Offset overflow")?)
    }
}

impl std::fmt::Display for Offset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<'de> Deserialize<'de> for Offset {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        let number = text.parse::<i64>().map_err(de::Error::custom)?;
        let offset = Self::new(number).map_err(de::Error::custom)?;
        if offset.0 != text {
            return Err(de::Error::custom(
                "Offset must be a canonical decimal string",
            ));
        }
        Ok(offset)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Bytes(String);

impl Bytes {
    pub fn new(bytes: &[u8]) -> Self {
        Self(STANDARD.encode(bytes))
    }

    pub fn decode(&self) -> Vec<u8> {
        STANDARD.decode(&self.0).expect("validated base64")
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        let bytes = STANDARD.decode(&text).map_err(de::Error::custom)?;
        if STANDARD.encode(bytes) != text {
            return Err(de::Error::custom("Bytes must be canonical standard base64"));
        }
        Ok(Self(text))
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Connection {
    pub brokers: Vec<String>,
    #[serde(default)]
    pub security: Security,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sasl: Option<Sasl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<Tls>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Security {
    #[default]
    Plaintext,
    Ssl,
    SaslPlaintext,
    SaslSsl,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mechanism", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Sasl {
    Plain {
        username: String,
        password: String,
    },
    ScramSha256 {
        username: String,
        password: String,
    },
    ScramSha512 {
        username: String,
        password: String,
    },
    #[serde(rename = "oauthbearer")]
    OAuthbearer {
        token: String,
    },
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tls {
    pub ca_pem: Option<String>,
    pub certificate_pem: Option<String>,
    pub key_pem: Option<String>,
}

impl Connection {
    pub fn validate(&self) -> Result<(), String> {
        if self.brokers.is_empty() || self.brokers.len() > 32 {
            return Err("Use 1-32 bootstrap brokers".into());
        }
        for broker in &self.brokers {
            let (host, port) = broker.rsplit_once(':').ok_or("Broker requires host:port")?;
            if host.is_empty()
                || broker.chars().any(char::is_whitespace)
                || broker.contains(['/', '@', '?', '#'])
                || port.parse::<u16>().ok().filter(|p| *p > 0).is_none()
                || (host.contains(':') && !(host.starts_with('[') && host.ends_with(']')))
            {
                return Err(
                    "Invalid broker; use host:port or [IPv6]:port, without credentials".into(),
                );
            }
        }
        let uses_sasl = matches!(self.security, Security::SaslPlaintext | Security::SaslSsl);
        let uses_tls = matches!(self.security, Security::Ssl | Security::SaslSsl);
        if uses_sasl != self.sasl.is_some() {
            return Err("SASL credentials require sasl_plaintext or sasl_ssl".into());
        }
        if self.tls.is_some() && !uses_tls {
            return Err("TLS settings require ssl or sasl_ssl".into());
        }
        if let Some(tls) = &self.tls
            && tls.certificate_pem.is_some() != tls.key_pem.is_some()
        {
            return Err("TLS client certificate and key must be provided together".into());
        }
        if let Some(tls) = &self.tls
            && [&tls.ca_pem, &tls.certificate_pem, &tls.key_pem]
                .into_iter()
                .flatten()
                .any(|pem| pem.trim().is_empty())
        {
            return Err("TLS PEM values must not be empty".into());
        }
        if let Some(sasl) = &self.sasl {
            let valid = match sasl {
                Sasl::Plain { username, password }
                | Sasl::ScramSha256 { username, password }
                | Sasl::ScramSha512 { username, password } => {
                    !username.is_empty() && !password.is_empty()
                }
                Sasl::OAuthbearer { token } => !token.is_empty(),
            };
            if !valid {
                return Err("SASL credentials must not be empty".into());
            }
        }
        Ok(())
    }

    pub fn secrets(&self) -> Vec<&str> {
        let mut result = Vec::new();
        if let Some(sasl) = &self.sasl {
            match sasl {
                Sasl::Plain { password, .. }
                | Sasl::ScramSha256 { password, .. }
                | Sasl::ScramSha512 { password, .. } => result.push(password.as_str()),
                Sasl::OAuthbearer { token } => result.push(token.as_str()),
            }
        }
        if let Some(tls) = &self.tls
            && let Some(key) = &tls.key_pem
        {
            result.push(key);
        }
        result.into_iter().filter(|s| !s.is_empty()).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Limits {
    pub max_records: u32,
    pub max_bytes: usize,
    pub timeout_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_records: 100,
            max_bytes: 1024 * 1024,
            timeout_ms: 30_000,
        }
    }
}

impl Limits {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_records == 0 || self.max_records > MAX_RECORDS {
            return Err(format!("Max records must be 1-{MAX_RECORDS}"));
        }
        if self.max_bytes == 0 || self.max_bytes > MAX_BYTES {
            return Err(format!("Max bytes must be 1-{MAX_BYTES}"));
        }
        if self.timeout_ms == 0 || self.timeout_ms > MAX_TIMEOUT_MS {
            return Err(format!("Timeout must be 1-{MAX_TIMEOUT_MS} ms"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Isolation {
    #[default]
    ReadCommitted,
    ReadUncommitted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FetchMode {
    Snapshot,
    Live,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum Start {
    Beginning,
    End,
    Offset(Offset),
    Last(u32),
    Timestamp(i64),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub name: String,
    pub value: Option<Bytes>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProducerRecord {
    pub topic: String,
    pub partition: Option<i32>,
    pub key: Option<Bytes>,
    pub value: Option<Bytes>,
    pub headers: Vec<Header>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
pub enum Operation {
    DescribeCluster,
    ListTopics,
    DescribeTopics {
        topics: Vec<String>,
    },
    ListOffsets {
        topic: String,
        partitions: Option<Vec<i32>>,
        at: Start,
        isolation: Isolation,
    },
    Fetch {
        topic: String,
        partitions: Option<Vec<i32>>,
        start: Start,
        mode: FetchMode,
        isolation: Isolation,
    },
    Produce {
        record: ProducerRecord,
    },
    ListGroups,
    DescribeGroups {
        groups: Vec<String>,
    },
    ListGroupOffsets {
        group: String,
    },
    DescribeConfigs {
        topic: String,
    },
}

impl Operation {
    pub fn name(&self) -> &'static str {
        match self {
            Self::DescribeCluster => "describeCluster",
            Self::ListTopics => "listTopics",
            Self::DescribeTopics { .. } => "describeTopics",
            Self::ListOffsets { .. } => "listOffsets",
            Self::Fetch { .. } => "fetch",
            Self::Produce { .. } => "produce",
            Self::ListGroups => "listGroups",
            Self::DescribeGroups { .. } => "describeGroups",
            Self::ListGroupOffsets { .. } => "listGroupOffsets",
            Self::DescribeConfigs { .. } => "describeConfigs",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub connection: Connection,
    pub operation: Operation,
    pub limits: Limits,
}

impl Request {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != VERSION {
            return Err("Unsupported Kafka contract version".into());
        }
        self.connection.validate()?;
        self.limits.validate()?;
        let name = |s: &str| {
            if s.is_empty() || s.len() > 249 || s.chars().any(char::is_control) {
                Err("Topic/group name must be non-empty and at most 249 bytes".to_string())
            } else {
                Ok(())
            }
        };
        match &self.operation {
            Operation::DescribeTopics { topics } => {
                if topics.is_empty() || topics.len() > 100 {
                    return Err("Use 1-100 topics".into());
                }
                for topic in topics {
                    name(topic)?;
                }
            }
            Operation::DescribeGroups { groups } => {
                if groups.is_empty() || groups.len() > 100 {
                    return Err("Use 1-100 groups".into());
                }
                for group in groups {
                    name(group)?;
                }
            }
            Operation::ListGroupOffsets { group } => name(group)?,
            Operation::DescribeConfigs { topic } => name(topic)?,
            Operation::ListOffsets {
                topic,
                partitions,
                at: start,
                ..
            }
            | Operation::Fetch {
                topic,
                partitions,
                start,
                ..
            } => {
                name(topic)?;
                if let Some(partitions) = partitions {
                    let unique: BTreeSet<_> = partitions.iter().collect();
                    if partitions.is_empty()
                        || partitions.len() > 1024
                        || partitions.iter().any(|p| *p < 0)
                        || unique.len() != partitions.len()
                    {
                        return Err(
                            "Partitions must be unique non-negative integers (1-1024 entries)"
                                .into(),
                        );
                    }
                }
                if matches!(start, Start::Last(0)) {
                    return Err("LAST requires a positive offset range".into());
                }
                if matches!(&self.operation, Operation::ListOffsets { .. })
                    && !matches!(start, Start::Beginning | Start::End | Start::Timestamp(_))
                {
                    return Err("ListOffsets takes beginning, end or timestamp".into());
                }
            }
            Operation::Produce { record } => {
                name(&record.topic)?;
                if record.partition.is_some_and(|p| p < 0) {
                    return Err("Partition must be non-negative".into());
                }
                if record
                    .headers
                    .iter()
                    .any(|h| h.name.is_empty() || h.name.chars().any(char::is_control))
                {
                    return Err("Header names must be non-empty without control characters".into());
                }
                if record.value.is_none() && record.key.is_none() {
                    return Err("A tombstone requires a key".into());
                }
            }
            _ => {}
        }
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > self.limits.max_bytes {
            return Err("Serialized request exceeds Max-Bytes".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KafkaError {
    pub code: String,
    pub message: String,
    pub resource: Option<String>,
    pub retriable: bool,
    pub possibly_written: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Broker {
    pub id: i32,
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cluster {
    pub cluster_id: Option<String>,
    pub controller_id: Option<i32>,
    pub brokers: Vec<Broker>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Partition {
    pub id: i32,
    pub leader: Option<i32>,
    pub replicas: Vec<i32>,
    pub isr: Vec<i32>,
    pub error: Option<KafkaError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Topic {
    pub name: String,
    pub internal: bool,
    pub partitions: Vec<Partition>,
    pub error: Option<KafkaError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionOffset {
    pub topic: String,
    pub partition: i32,
    pub offset: Option<Offset>,
    pub timestamp_ms: Option<i64>,
    pub error: Option<KafkaError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub topic: String,
    pub partition: i32,
    pub offset: Offset,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicPartition {
    pub topic: String,
    pub partition: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub id: String,
    pub client_id: String,
    pub host: String,
    pub assignments: Vec<TopicPartition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub state: String,
    pub protocol: String,
    pub members: Vec<Member>,
    pub error: Option<KafkaError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupOffset {
    pub group: String,
    pub topic: String,
    pub partition: i32,
    pub committed: Option<Offset>,
    pub error: Option<KafkaError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigEntry {
    pub name: String,
    pub value: Option<String>,
    pub source: String,
    pub sensitive: bool,
    pub error: Option<KafkaError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub topic: String,
    pub partition: i32,
    pub offset: Offset,
    pub timestamp_ms: Option<i64>,
    pub timestamp_type: Option<String>,
    pub key: Option<Bytes>,
    pub value: Option<Bytes>,
    pub headers: Vec<Header>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    pub topic: String,
    pub partition: Option<i32>,
    pub offset: Option<Offset>,
    pub status: DeliveryStatus,
    pub error: Option<KafkaError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeliveryStatus {
    Confirmed,
    NotQueued,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EndReason {
    SnapshotEnd,
    RecordLimit,
    ByteLimit,
    Timeout,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchSummary {
    pub scanned: u64,
    pub next_offsets: Vec<Cursor>,
    pub end_reason: EndReason,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch<T> {
    pub items: Vec<T>,
    pub truncated: bool,
}

impl<T> From<Vec<T>> for Batch<T> {
    fn from(items: Vec<T>) -> Self {
        Self {
            items,
            truncated: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "camelCase")]
pub enum Response {
    Cluster(Cluster),
    Topics(Batch<Topic>),
    Offsets(Batch<PartitionOffset>),
    Groups(Batch<Group>),
    GroupOffsets(Batch<GroupOffset>),
    Configs(Batch<ConfigEntry>),
    Produced(Delivery),
    Fetched(FetchSummary),
    Error(KafkaError),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "camelCase")]
pub enum StreamEvent {
    Started { partitions: Vec<Cursor> },
    Records { records: Vec<Record> },
}
