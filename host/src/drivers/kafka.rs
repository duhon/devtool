//! The kafka driver ([`kafka_contract`]) over krafka: admin reads, standalone consumers with
//! explicit assignment (no group, no commits, no topic creation, no metrics push) and one shared
//! idempotent `acks=all` producer per connection. Clients are cached per connection identity: a
//! salted hash, at most [`CACHE`] of them, dropped after [`TTL`] unused.

use super::{CONNECT_MS, Failure, emit, millis};
use kafka_contract::{
    Batch, Broker, Bytes, Cluster, ConfigEntry, Connection, Cursor, Delivery, DeliveryStatus,
    EndReason, FetchMode, FetchSummary, Group, GroupOffset, Header, Isolation, KafkaError, Limits,
    Member, Offset, Operation, Partition, PartitionOffset, ProducerRecord, Record, Request,
    Response, Sasl, Security, Start, StreamEvent, Tls, Topic, TopicPartition,
};
use krafka::{
    Kafka as Client, KrafkaError,
    admin::{
        AdminClient, ConfigResource, DescribeClusterOptions, DescribeConfigsOptions,
        DescribeConsumerGroupsOptions, DescribeTopicsOptions, ListConsumerGroupOffsetsOptions,
        ListConsumerGroupsOptions, ListOffsetsOptions, ListTopicsOptions, OffsetSpec,
    },
    auth::{AuthConfig, TlsConfig},
    consumer::{AutoOffsetReset, Consumer, ConsumerRecord, IsolationLevel},
    error::ErrorCode,
    producer::{Acks, DeliveryConfirmation, Producer},
};
use serde::Serialize;
use serde_json::{Value, json};
use services_sdk::driver::Stage;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    hash::BuildHasher,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{OnceCell, mpsc},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

/// Connection identities kept at most.
pub const CACHE: usize = 16;
/// How long an unused connection stays cached.
pub const TTL: Duration = Duration::from_secs(60);
/// Serialized bytes per `records` event, at most.
const CHUNK: usize = 256 * 1024;
/// One poll's wait, so that a snapshot notices its end and a run its deadline.
const POLL: Duration = Duration::from_millis(250);
const REQUEST: Duration = Duration::from_secs(30);
const CLOSE: Duration = Duration::from_secs(2);

#[derive(Clone, Default)]
pub struct Kafka {
    cache: Arc<Mutex<Cache>>,
}

#[derive(Default)]
struct Cache {
    salt: Option<[u64; 2]>,
    entries: HashMap<[u8; 32], Entry>,
}

#[derive(Clone)]
struct Entry {
    client: Arc<OnceCell<Client>>,
    producer: Arc<OnceCell<Arc<Producer>>>,
    used: Instant,
}

impl Kafka {
    /// Answers a [`Response`]; `fetch` must stream (`events`) and sends [`StreamEvent`]s first.
    /// Failures before the connection is up name their stage; broker answers after it are
    /// [`Response::Error`]s or per-item errors.
    pub async fn run(
        &self,
        request: Request,
        cancel: CancellationToken,
        events: Option<&mpsc::Sender<Value>>,
    ) -> Result<Value, Failure> {
        request
            .validate()
            .map_err(|error| Failure::new(Stage::Send, error))?;
        if matches!(request.operation, Operation::Fetch { .. }) && events.is_none() {
            return Err(Failure::new(Stage::Send, "A Kafka fetch must be streamed"));
        }
        let secrets: Vec<String> = request
            .connection
            .secrets()
            .into_iter()
            .map(String::from)
            .collect();
        let hard = Instant::now() + millis(request.limits.timeout_ms);
        let soft = hard - margin(request.limits.timeout_ms);
        let queued = Arc::new(AtomicBool::new(false));
        let work = self.operate(&request, events, &cancel, soft, &queued);
        let result = tokio::select! {
            result = work => result,
            () = cancel.cancelled() => Err(stopped(Stage::Cancelled, "Cancelled", &queued)),
            () = tokio::time::sleep_until(hard) => {
                Err(stopped(Stage::Timeout, "The Kafka request timed out", &queued))
            }
        };
        match result {
            Ok(Response::Error(mut error)) => {
                error.message = scrub(&error.message, &secrets);
                Ok(json!(Response::Error(error)))
            }
            Ok(response) => Ok(json!(response)),
            Err(mut failure) => {
                failure.message = scrub(&failure.message, &secrets);
                for cause in &mut failure.failure.causes {
                    *cause = scrub(cause, &secrets);
                }
                Err(failure)
            }
        }
    }

    async fn operate(
        &self,
        request: &Request,
        events: Option<&mpsc::Sender<Value>>,
        cancel: &CancellationToken,
        soft: Instant,
        queued: &AtomicBool,
    ) -> Result<Response, Failure> {
        let key = self.key(&request.connection);
        let entry = self.entry(key);
        let client = connect(&entry, &request.connection, soft).await?;
        let limits = &request.limits;
        let admin = client.admin();
        let response = match &request.operation {
            Operation::DescribeCluster => describe_cluster(&admin, soft).await,
            Operation::ListTopics => list_topics(&admin, limits, soft).await,
            Operation::DescribeTopics { topics } => {
                describe_topics(&admin, topics, limits, soft).await
            }
            Operation::ListOffsets {
                topic,
                partitions,
                at,
                isolation,
            } => list_offsets(
                &admin,
                topic,
                partitions.as_deref(),
                at,
                *isolation,
                limits,
                soft,
            )
            .await
            .unwrap_or_else(Response::Error),
            Operation::Fetch {
                topic,
                partitions,
                start,
                mode,
                isolation,
            } => {
                let fetch = Fetch {
                    topic,
                    partitions: partitions.as_deref(),
                    start,
                    mode: *mode,
                    isolation: *isolation,
                    limits,
                };
                let events = events.expect("checked by run");
                fetch.run(&client, events, cancel, soft).await?
            }
            Operation::Produce { record } => {
                let producer = entry
                    .producer
                    .get_or_try_init(|| async {
                        tokio::time::timeout_at(soft, producer(&client))
                            .await
                            .unwrap_or_else(|_| Err(KrafkaError::timeout("Building the producer")))
                            .map(Arc::new)
                    })
                    .await;
                match producer {
                    Ok(producer) => {
                        Response::Produced(produce(producer, record, soft, queued).await)
                    }
                    Err(error) => Response::Produced(Delivery {
                        topic: record.topic.clone(),
                        partition: None,
                        offset: None,
                        status: DeliveryStatus::NotQueued,
                        error: Some(KafkaError {
                            possibly_written: Some(false),
                            ..kafka_error(&error, None)
                        }),
                    }),
                }
            }
            Operation::ListGroups => list_groups(&admin, limits, soft).await,
            Operation::DescribeGroups { groups } => {
                describe_groups(&admin, groups, limits, soft).await
            }
            Operation::ListGroupOffsets { group } => {
                list_group_offsets(&admin, group, limits, soft).await
            }
            Operation::DescribeConfigs { topic } => {
                describe_configs(&admin, topic, limits, soft).await
            }
        };
        if let Response::Error(error) = &response
            && matches!(error.code.as_str(), "AUTHENTICATION" | "CLOSED")
        {
            self.forget(key);
        }
        Ok(response)
    }

    /// The connection's identity: a hash salted per process, so it means nothing elsewhere.
    fn key(&self, connection: &Connection) -> [u8; 32] {
        let salt = {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            *cache.salt.get_or_insert_with(|| {
                let state = std::hash::RandomState::new();
                [state.hash_one(1u8), state.hash_one(2u8)]
            })
        };
        let mut hash = Sha256::new();
        hash.update(salt[0].to_le_bytes());
        hash.update(salt[1].to_le_bytes());
        hash.update(serde_json::to_vec(connection).unwrap_or_default());
        hash.finalize().into()
    }

    fn entry(&self, key: [u8; 32]) -> Entry {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        cache
            .entries
            .retain(|k, entry| *k == key || now.duration_since(entry.used) < TTL);
        if !cache.entries.contains_key(&key) && cache.entries.len() >= CACHE {
            let oldest = cache
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(k, _)| *k);
            if let Some(oldest) = oldest {
                cache.entries.remove(&oldest);
            }
        }
        let entry = cache.entries.entry(key).or_insert_with(|| Entry {
            client: Arc::default(),
            producer: Arc::default(),
            used: now,
        });
        entry.used = now;
        entry.clone()
    }

    fn forget(&self, key: [u8; 32]) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.entries.remove(&key);
    }

    /// Cached connection identities, for tests.
    pub fn cached(&self) -> usize {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len()
    }
}

/// The share of a run's budget kept to answer before the caller's own deadline.
fn margin(timeout_ms: u64) -> Duration {
    millis((timeout_ms / 10).clamp(50, 2_000))
}

fn stopped(stage: Stage, message: &str, queued: &AtomicBool) -> Failure {
    let mut failure = Failure::new(stage, message);
    if queued.load(Ordering::SeqCst) {
        failure.failure.causes.push(
            "The record was queued before this and may have been written; it is not resent.".into(),
        );
    }
    failure
}

fn scrub(text: &str, secrets: &[String]) -> String {
    secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .fold(text.to_owned(), |text, secret| {
            text.replace(secret.as_str(), "[REDACTED]")
        })
}

fn auth(connection: &Connection) -> AuthConfig {
    let tls = || tls(connection.tls.as_ref());
    let sasl = match &connection.sasl {
        Some(Sasl::Plain { username, password }) => {
            Some(AuthConfig::sasl_plain(username, password))
        }
        Some(Sasl::ScramSha256 { username, password }) => {
            Some(AuthConfig::sasl_scram_sha256(username, password))
        }
        Some(Sasl::ScramSha512 { username, password }) => {
            Some(AuthConfig::sasl_scram_sha512(username, password))
        }
        Some(Sasl::OAuthbearer { token }) => Some(AuthConfig::sasl_oauthbearer(token)),
        None => None,
    };
    match (connection.security, sasl) {
        (Security::Ssl, _) => AuthConfig::ssl(tls()),
        (Security::SaslSsl, Some(sasl)) => sasl.with_tls(tls()),
        (Security::SaslPlaintext, Some(sasl)) => sasl,
        _ => AuthConfig::plaintext(),
    }
}

/// A pinned CA when one is given (only it is trusted), else the platform's roots.
fn tls(config: Option<&Tls>) -> TlsConfig {
    let config = config.cloned().unwrap_or_default();
    let mut tls = TlsConfig::new();
    tls = match config.ca_pem {
        Some(ca) => tls.with_ca_cert_pem(ca.into_bytes()),
        None => tls.with_native_roots(),
    };
    if let (Some(certificate), Some(key)) = (config.certificate_pem, config.key_pem) {
        tls = tls.with_client_cert_pem(certificate.into_bytes(), key.into_bytes());
    }
    tls
}

async fn connect(entry: &Entry, connection: &Connection, soft: Instant) -> Result<Client, Failure> {
    let client = entry
        .client
        .get_or_try_init(|| async {
            let connect = Client::builder(connection.brokers.join(","))
                .client_id("services-devtools")
                .security(auth(connection))
                .connect_timeout(millis(CONNECT_MS))
                .request_timeout(REQUEST)
                .allow_auto_create_topics(false)
                .max_connections(Some(64))
                .connect();
            match tokio::time::timeout_at(soft, connect).await {
                Ok(result) => result.map_err(|error| connect_failure(&error)),
                Err(_) => Err(Failure::new(
                    Stage::Timeout,
                    "Connecting to Kafka timed out",
                )),
            }
        })
        .await?;
    Ok(client.clone())
}

fn connect_failure(error: &KrafkaError) -> Failure {
    match error {
        KrafkaError::Auth { message, .. } if message.starts_with("TLS handshake failed") => {
            Failure::at(Stage::Tls, error)
        }
        KrafkaError::Auth { .. } => Failure::at(Stage::Auth, error),
        KrafkaError::Broker { code, .. } if sasl_code(*code) => Failure::at(Stage::Auth, error),
        KrafkaError::Timeout { .. } => Failure::at(Stage::Timeout, error),
        KrafkaError::Config { .. } => Failure::chain(Stage::Send, error),
        _ => Failure::chain(Stage::Connect, error),
    }
}

fn sasl_code(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::SaslAuthenticationFailed
            | ErrorCode::UnsupportedSaslMechanism
            | ErrorCode::IllegalSaslState
    )
}

/// A krafka error as a typed contract error; broker codes keep Kafka's names
/// (`UNKNOWN_TOPIC_OR_PARTITION`).
pub fn kafka_error(error: &KrafkaError, resource: Option<String>) -> KafkaError {
    let code = match error {
        KrafkaError::Broker { code, .. } if sasl_code(*code) => "AUTHENTICATION".into(),
        KrafkaError::Broker { code, .. } => code_name(*code),
        KrafkaError::Network(_) => "NETWORK".into(),
        KrafkaError::Protocol { .. } => "PROTOCOL".into(),
        KrafkaError::Auth { .. } => "AUTHENTICATION".into(),
        KrafkaError::Timeout { .. } => "TIMEOUT".into(),
        KrafkaError::DeliveryTimeout { .. } => "DELIVERY_TIMEOUT".into(),
        KrafkaError::Config { .. } => "CONFIG".into(),
        KrafkaError::Closed { .. } => "CLOSED".into(),
        KrafkaError::NoOffset { .. } => "OFFSET_OUT_OF_RANGE".into(),
        KrafkaError::UnknownTopic { .. } => "UNKNOWN_TOPIC_OR_PARTITION".into(),
        KrafkaError::RecordDeserialization { .. } => "CORRUPT_MESSAGE".into(),
        _ => "CLIENT".into(),
    };
    let possibly_written = match error {
        KrafkaError::DeliveryTimeout {
            possibly_written, ..
        } => Some(*possibly_written),
        _ => None,
    };
    KafkaError {
        code,
        message: error.to_string(),
        resource,
        retriable: error.is_retriable(),
        possibly_written,
    }
}

/// `UnknownTopicOrPartition` → `UNKNOWN_TOPIC_OR_PARTITION`; a code krafka does not name is
/// `BROKER_ERROR_<n>`.
pub fn code_name(code: ErrorCode) -> String {
    let name = format!("{code:?}");
    if !name.chars().all(char::is_alphanumeric) {
        return format!("BROKER_ERROR_{}", code.to_i16());
    }
    let mut out = String::with_capacity(name.len() + 8);
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

fn error(code: &str, message: impl Into<String>, resource: Option<String>) -> KafkaError {
    KafkaError {
        code: code.into(),
        message: message.into(),
        resource,
        retriable: false,
        possibly_written: None,
    }
}

fn remaining(soft: Instant) -> Duration {
    soft.saturating_duration_since(Instant::now())
        .max(Duration::from_millis(1))
}

fn offset(value: i64) -> Option<Offset> {
    Offset::new(value).ok()
}

/// At most `max_records` items and `max_bytes` serialized: what does not fit is dropped and
/// reported as `truncated`.
pub fn fit<T: Serialize>(
    mut items: Vec<T>,
    truncated: bool,
    limits: &Limits,
    wrap: impl Fn(Batch<T>) -> Response,
) -> Response {
    let mut truncated = truncated || items.len() > limits.max_records as usize;
    items.truncate(limits.max_records as usize);
    let mut total = size(&wrap(Batch {
        items: Vec::new(),
        truncated: true,
    }));
    let mut keep = 0;
    for (i, item) in items.iter().enumerate() {
        total = total.saturating_add(size(item).saturating_add(usize::from(i > 0)));
        if total > limits.max_bytes {
            truncated = true;
            break;
        }
        keep += 1;
    }
    items.truncate(keep);
    wrap(Batch { items, truncated })
}

fn size(value: &impl Serialize) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |v| v.len())
}

async fn describe_cluster(admin: &AdminClient, soft: Instant) -> Response {
    match admin
        .describe_cluster(DescribeClusterOptions::default().timeout(remaining(soft)))
        .await
    {
        Ok(cluster) => Response::Cluster(Cluster {
            cluster_id: Some(cluster.cluster_id).filter(|id| !id.is_empty()),
            controller_id: Some(cluster.controller_id).filter(|id| *id >= 0),
            brokers: cluster
                .brokers
                .into_iter()
                .map(|broker| Broker {
                    id: broker.id,
                    host: broker.host,
                    port: u16::try_from(broker.port).unwrap_or_default(),
                })
                .collect(),
        }),
        Err(e) => Response::Error(kafka_error(&e, None)),
    }
}

async fn list_topics(admin: &AdminClient, limits: &Limits, soft: Instant) -> Response {
    let names = match admin
        .list_topics(
            ListTopicsOptions::default()
                .include_internal(true)
                .timeout(remaining(soft)),
        )
        .await
    {
        Ok(names) => names,
        Err(e) => return Response::Error(kafka_error(&e, None)),
    };
    let mut names: Vec<String> = names
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let truncated = names.len() > limits.max_records as usize;
    names.truncate(limits.max_records as usize);
    if names.is_empty() {
        return fit(Vec::new(), truncated, limits, Response::Topics);
    }
    match topics(admin, &names, soft).await {
        Ok(topics) => fit(topics, truncated, limits, Response::Topics),
        Err(e) => Response::Error(e),
    }
}

async fn describe_topics(
    admin: &AdminClient,
    names: &[String],
    limits: &Limits,
    soft: Instant,
) -> Response {
    match topics(admin, names, soft).await {
        Ok(topics) => fit(topics, false, limits, Response::Topics),
        Err(e) => Response::Error(e),
    }
}

async fn topics(
    admin: &AdminClient,
    names: &[String],
    soft: Instant,
) -> Result<Vec<Topic>, KafkaError> {
    let mut described = admin
        .describe_topics(
            names,
            DescribeTopicsOptions::default().timeout(remaining(soft)),
        )
        .await
        .map_err(|e| kafka_error(&e, None))?;
    Ok(names
        .iter()
        .map(|name| match described.remove(name) {
            Some(Ok(topic)) => {
                let mut partitions: Vec<Partition> = topic
                    .partitions
                    .into_iter()
                    .map(|p| Partition {
                        id: p.partition,
                        leader: p.leader.filter(|id| *id >= 0),
                        replicas: p.replicas,
                        isr: p.isr,
                        error: None,
                    })
                    .collect();
                partitions.sort_by_key(|p| p.id);
                Topic {
                    name: topic.name,
                    internal: topic.is_internal,
                    partitions,
                    error: None,
                }
            }
            failed => Topic {
                name: name.clone(),
                internal: false,
                partitions: Vec::new(),
                error: Some(match failed {
                    Some(Err(e)) => kafka_error(&e, Some(name.clone())),
                    _ => error(
                        "UNKNOWN_TOPIC_OR_PARTITION",
                        "The broker did not describe the topic",
                        Some(name.clone()),
                    ),
                }),
            },
        })
        .collect())
}

/// The topic's partitions, or the requested ones once they are known to exist.
async fn partitions(
    admin: &AdminClient,
    topic: &str,
    requested: Option<&[i32]>,
    soft: Instant,
) -> Result<Vec<i32>, KafkaError> {
    let described = topics(admin, &[topic.to_owned()], soft).await?;
    let topic_info = described.into_iter().next().expect("one topic described");
    if let Some(error) = topic_info.error {
        return Err(error);
    }
    let known: BTreeSet<i32> = topic_info.partitions.iter().map(|p| p.id).collect();
    match requested {
        None => Ok(known.into_iter().collect()),
        Some(requested) => {
            if let Some(missing) = requested.iter().find(|p| !known.contains(p)) {
                return Err(error(
                    "UNKNOWN_TOPIC_OR_PARTITION",
                    format!("Partition {missing} does not exist"),
                    Some(format!("{topic}-{missing}")),
                ));
            }
            let mut requested = requested.to_vec();
            requested.sort_unstable();
            Ok(requested)
        }
    }
}

/// One offset per partition; a partition that fails fails the whole lookup.
async fn bounds(
    admin: &AdminClient,
    topic: &str,
    ids: &[i32],
    spec: OffsetSpec,
    committed: bool,
    soft: Instant,
) -> Result<BTreeMap<i32, i64>, KafkaError> {
    let listed = admin
        .list_offsets(
            ids.iter()
                .map(|p| (krafka::admin::TopicPartition::new(topic, *p), spec)),
            ListOffsetsOptions::default()
                .read_committed(committed)
                .timeout(remaining(soft)),
        )
        .await
        .map_err(|e| kafka_error(&e, Some(topic.to_owned())))?;
    let mut out = BTreeMap::new();
    for (tp, result) in listed {
        let resource = Some(format!("{}-{}", tp.topic, tp.partition));
        match result {
            Ok(listed) => out.insert(tp.partition, listed.offset),
            Err(e) => return Err(kafka_error(&e, resource)),
        };
    }
    match ids.iter().find(|p| !out.contains_key(p)) {
        Some(p) => Err(error(
            "UNKNOWN_TOPIC_OR_PARTITION",
            "The broker did not list the partition's offset",
            Some(format!("{topic}-{p}")),
        )),
        None => Ok(out),
    }
}

async fn list_offsets(
    admin: &AdminClient,
    topic: &str,
    requested: Option<&[i32]>,
    at: &Start,
    isolation: Isolation,
    limits: &Limits,
    soft: Instant,
) -> Result<Response, KafkaError> {
    let ids = partitions(admin, topic, requested, soft).await?;
    let spec = match at {
        Start::Beginning => OffsetSpec::Earliest,
        Start::Timestamp(ms) => OffsetSpec::Timestamp(*ms),
        _ => OffsetSpec::Latest,
    };
    let mut listed = admin
        .list_offsets(
            ids.iter()
                .map(|p| (krafka::admin::TopicPartition::new(topic, *p), spec)),
            ListOffsetsOptions::default()
                .read_committed(isolation == Isolation::ReadCommitted)
                .timeout(remaining(soft)),
        )
        .await
        .map_err(|e| kafka_error(&e, Some(topic.to_owned())))?;
    let items = ids
        .iter()
        .map(|p| {
            let key = krafka::admin::TopicPartition::new(topic, *p);
            let (offset, timestamp_ms, error) = match listed.remove(&key) {
                Some(Ok(listed)) => (
                    self::offset(listed.offset),
                    listed.timestamp.filter(|ms| *ms >= 0),
                    None,
                ),
                Some(Err(e)) => (
                    None,
                    None,
                    Some(kafka_error(&e, Some(format!("{topic}-{p}")))),
                ),
                None => (
                    None,
                    None,
                    Some(error(
                        "UNKNOWN_TOPIC_OR_PARTITION",
                        "The broker did not list the partition's offset",
                        Some(format!("{topic}-{p}")),
                    )),
                ),
            };
            PartitionOffset {
                topic: topic.to_owned(),
                partition: *p,
                offset,
                timestamp_ms,
                error,
            }
        })
        .collect();
    Ok(fit(items, false, limits, Response::Offsets))
}

async fn list_groups(admin: &AdminClient, limits: &Limits, soft: Instant) -> Response {
    let listed = match admin
        .list_consumer_groups(ListConsumerGroupsOptions::default().timeout(remaining(soft)))
        .await
    {
        Ok(listed) => listed,
        Err(e) => return Response::Error(kafka_error(&e, None)),
    };
    let mut groups = BTreeMap::new();
    let mut failed = None;
    for (broker, result) in listed {
        match result {
            Ok(listings) => {
                for listing in listings {
                    groups.insert(
                        listing.group_id.clone(),
                        Group {
                            id: listing.group_id,
                            state: listing.state.unwrap_or_default(),
                            protocol: listing.protocol_type,
                            members: Vec::new(),
                            error: None,
                        },
                    );
                }
            }
            Err(e) => failed = Some(kafka_error(&e, Some(format!("broker {broker}")))),
        }
    }
    // A broker that did not answer leaves the list incomplete: say so, or fail when it is empty.
    match failed {
        Some(error) if groups.is_empty() => Response::Error(error),
        failed => fit(
            groups.into_values().collect(),
            failed.is_some(),
            limits,
            Response::Groups,
        ),
    }
}

async fn describe_groups(
    admin: &AdminClient,
    ids: &[String],
    limits: &Limits,
    soft: Instant,
) -> Response {
    let mut described = match admin
        .describe_consumer_groups(
            ids,
            DescribeConsumerGroupsOptions::default().timeout(remaining(soft)),
        )
        .await
    {
        Ok(described) => described,
        Err(e) => return Response::Error(kafka_error(&e, None)),
    };
    let groups =
        ids.iter()
            .map(|id| match described.remove(id) {
                Some(Ok(group)) => Group {
                    id: group.group_id,
                    state: group.state,
                    protocol: group.protocol_type.unwrap_or_default(),
                    members: group
                        .members
                        .into_iter()
                        .map(|member| Member {
                            id: member.member_id,
                            client_id: member.client_id,
                            host: member.client_host,
                            assignments: member
                                .assignment
                                .unwrap_or_default()
                                .into_iter()
                                .flat_map(|topic| {
                                    let name = topic.topic_name;
                                    topic.partitions.into_iter().map(move |partition| {
                                        TopicPartition {
                                            topic: name.clone(),
                                            partition,
                                        }
                                    })
                                })
                                .collect(),
                        })
                        .collect(),
                    error: None,
                },
                failed => Group {
                    id: id.clone(),
                    state: String::new(),
                    protocol: String::new(),
                    members: Vec::new(),
                    error: Some(match failed {
                        Some(Err(e)) => kafka_error(&e, Some(id.clone())),
                        _ => error(
                            "GROUP_ID_NOT_FOUND",
                            "The broker did not describe the group",
                            Some(id.clone()),
                        ),
                    }),
                },
            })
            .collect();
    fit(groups, false, limits, Response::Groups)
}

async fn list_group_offsets(
    admin: &AdminClient,
    group: &str,
    limits: &Limits,
    soft: Instant,
) -> Response {
    let listed = match admin
        .list_consumer_group_offsets(
            group,
            ListConsumerGroupOffsetsOptions::default().timeout(remaining(soft)),
        )
        .await
    {
        Ok(listed) => listed,
        Err(e) => return Response::Error(kafka_error(&e, Some(group.to_owned()))),
    };
    let mut items: Vec<GroupOffset> = listed
        .into_iter()
        .map(|(tp, result)| {
            let (committed, error) = match result {
                Ok(committed) => (committed.offset.and_then(offset), None),
                Err(e) => (
                    None,
                    Some(kafka_error(
                        &e,
                        Some(format!("{}-{}", tp.topic, tp.partition)),
                    )),
                ),
            };
            GroupOffset {
                group: group.to_owned(),
                topic: tp.topic,
                partition: tp.partition,
                committed,
                error,
            }
        })
        .collect();
    items.sort_by(|a, b| (&a.topic, a.partition).cmp(&(&b.topic, b.partition)));
    fit(items, false, limits, Response::GroupOffsets)
}

/// Kafka's `ConfigSource` names.
pub fn config_source(source: i8) -> &'static str {
    match source {
        1 => "DYNAMIC_TOPIC_CONFIG",
        2 => "DYNAMIC_BROKER_CONFIG",
        3 => "DYNAMIC_DEFAULT_BROKER_CONFIG",
        4 => "STATIC_BROKER_CONFIG",
        5 => "DEFAULT_CONFIG",
        6 => "DYNAMIC_BROKER_LOGGER_CONFIG",
        7 => "CLIENT_METRICS_CONFIG",
        8 => "GROUP_CONFIG",
        _ => "UNKNOWN",
    }
}

async fn describe_configs(
    admin: &AdminClient,
    topic: &str,
    limits: &Limits,
    soft: Instant,
) -> Response {
    let resource = ConfigResource::topic(topic);
    let mut described = match admin
        .describe_configs(
            [resource.clone()],
            DescribeConfigsOptions::default().timeout(remaining(soft)),
        )
        .await
    {
        Ok(described) => described,
        Err(e) => return Response::Error(kafka_error(&e, Some(topic.to_owned()))),
    };
    match described.remove(&resource) {
        Some(Ok(entries)) => {
            let mut entries: Vec<ConfigEntry> = entries
                .into_iter()
                .map(|entry| ConfigEntry {
                    value: entry.value.filter(|_| !entry.is_sensitive),
                    name: entry.name,
                    source: config_source(entry.config_source).into(),
                    sensitive: entry.is_sensitive,
                    error: None,
                })
                .collect();
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            fit(entries, false, limits, Response::Configs)
        }
        Some(Err(e)) => Response::Error(kafka_error(&e, Some(topic.to_owned()))),
        None => Response::Error(error(
            "UNKNOWN_TOPIC_OR_PARTITION",
            "The broker did not describe the topic's configuration",
            Some(topic.to_owned()),
        )),
    }
}

async fn producer(client: &Client) -> krafka::Result<Producer> {
    client
        .producer()
        .acks(Acks::All)
        .idempotent(true)
        .linger(Duration::ZERO)
        .max_block(Duration::from_secs(10))
        .buffer_memory(16 * 1024 * 1024)
        .metrics_push(false)
        .build()
        .await
}

/// A record as krafka's: null and empty stay apart, headers keep their order and duplicates.
pub fn producer_record(record: &ProducerRecord) -> krafka::Record {
    let mut out = match &record.value {
        Some(value) => krafka::Record::new(&record.topic, value.decode()),
        None => krafka::Record::new(&record.topic, Vec::new()).without_value(),
    };
    if let Some(key) = &record.key {
        out = out.key(key.decode());
    }
    if let Some(partition) = record.partition {
        out = out.partition(partition);
    }
    for header in &record.headers {
        out = match &header.value {
            Some(value) => out.header(&header.name, value.decode()),
            None => out.null_header(&header.name),
        };
    }
    out
}

/// What became of the record. Once queued it is never resent: a deadline then is `unknown`.
async fn produce(
    producer: &Producer,
    record: &ProducerRecord,
    soft: Instant,
    queued: &AtomicBool,
) -> Delivery {
    let delivery = |partition, offset, status, error| Delivery {
        topic: record.topic.clone(),
        partition,
        offset,
        status,
        error,
    };
    let handle =
        match tokio::time::timeout_at(soft, producer.enqueue(producer_record(record))).await {
            Ok(Ok(handle)) => handle,
            Ok(Err(e)) => {
                let error = KafkaError {
                    possibly_written: Some(false),
                    ..kafka_error(&e, Some(record.topic.clone()))
                };
                return delivery(None, None, DeliveryStatus::NotQueued, Some(error));
            }
            Err(_) => {
                let error = KafkaError {
                    possibly_written: Some(false),
                    ..self::error("TIMEOUT", "The record could not be queued in time", None)
                };
                return delivery(None, None, DeliveryStatus::NotQueued, Some(error));
            }
        };
    queued.store(true, Ordering::SeqCst);
    let partition = Some(handle.partition());
    let unknown = |code: &str, message: &str| KafkaError {
        possibly_written: Some(true),
        ..self::error(code, message, Some(record.topic.clone()))
    };
    match tokio::time::timeout_at(soft, handle).await {
        Ok(Ok(meta)) if meta.delivery == DeliveryConfirmation::Offset && meta.offset >= 0 => {
            delivery(
                Some(meta.partition),
                offset(meta.offset),
                DeliveryStatus::Confirmed,
                None,
            )
        }
        Ok(Ok(meta)) => delivery(
            Some(meta.partition),
            None,
            DeliveryStatus::Unknown,
            Some(unknown(
                "DUPLICATE_SEQUENCE_NUMBER",
                "The broker reported the record as already written; its offset is unknown",
            )),
        ),
        Ok(Err(e)) => {
            let error = kafka_error(&e, Some(record.topic.clone()));
            let status = match error.possibly_written {
                Some(true) => DeliveryStatus::Unknown,
                _ => DeliveryStatus::Failed,
            };
            delivery(partition, None, status, Some(error))
        }
        Err(_) => delivery(
            partition,
            None,
            DeliveryStatus::Unknown,
            Some(unknown(
                "TIMEOUT",
                "No answer before the deadline; the record may have been written and is not resent",
            )),
        ),
    }
}

/// A consumed record as the contract's.
pub fn record(record: ConsumerRecord) -> Record {
    fn bytes(value: Option<impl AsRef<[u8]>>) -> Option<Bytes> {
        value.map(|value| Bytes::new(value.as_ref()))
    }
    Record {
        topic: record.topic.to_string(),
        partition: record.partition,
        offset: Offset::new(record.offset).unwrap_or_else(|_| Offset::new(0).unwrap()),
        timestamp_ms: Some(record.timestamp).filter(|ms| *ms >= 0),
        timestamp_type: match record.timestamp_type {
            krafka::TimestampType::CreateTime => Some("createTime".into()),
            krafka::TimestampType::LogAppendTime => Some("logAppendTime".into()),
            _ => None,
        },
        key: bytes(record.key),
        value: bytes(record.value),
        headers: record
            .headers
            .into_iter()
            .map(|(name, value)| Header {
                name,
                value: bytes(value),
            })
            .collect(),
    }
}

/// Packs records into `records` events and counts them against the run's limits.
pub struct Packer {
    max_records: u64,
    max_bytes: usize,
    chunk: usize,
    overhead: usize,
    pub scanned: u64,
    pub bytes: usize,
    batch: Vec<Record>,
    batch_bytes: usize,
}

impl Packer {
    pub fn new(limits: &Limits) -> Self {
        let overhead = serde_json::to_vec(&StreamEvent::Records {
            records: Vec::new(),
        })
        .map_or(0, |v| v.len());
        Self {
            max_records: u64::from(limits.max_records),
            max_bytes: limits.max_bytes,
            chunk: limits.max_bytes.min(CHUNK),
            overhead,
            scanned: 0,
            bytes: 0,
            batch: Vec::new(),
            batch_bytes: 0,
        }
    }

    pub fn full(&self) -> bool {
        self.scanned >= self.max_records
    }

    /// Takes the record, or answers why it may not: the record or byte limit. A full event is
    /// handed back to be sent first.
    pub fn push(&mut self, record: Record) -> Result<Option<StreamEvent>, EndReason> {
        if self.full() {
            return Err(EndReason::RecordLimit);
        }
        let size = size(&record);
        if self.bytes.saturating_add(size) > self.max_bytes
            || size.saturating_add(self.overhead) > self.max_bytes
        {
            return Err(EndReason::ByteLimit);
        }
        let mut ready = None;
        if !self.batch.is_empty() && self.overhead + self.batch_bytes + 1 + size > self.chunk {
            ready = self.take();
        }
        self.batch_bytes += size + usize::from(!self.batch.is_empty());
        self.batch.push(record);
        self.bytes += size;
        self.scanned += 1;
        Ok(ready)
    }

    pub fn take(&mut self) -> Option<StreamEvent> {
        self.batch_bytes = 0;
        (!self.batch.is_empty()).then(|| StreamEvent::Records {
            records: std::mem::take(&mut self.batch),
        })
    }
}

struct Fetch<'a> {
    topic: &'a str,
    partitions: Option<&'a [i32]>,
    start: &'a Start,
    mode: FetchMode,
    isolation: Isolation,
    limits: &'a Limits,
}

impl Fetch<'_> {
    async fn run(
        &self,
        client: &Client,
        events: &mpsc::Sender<Value>,
        cancel: &CancellationToken,
        soft: Instant,
    ) -> Result<Response, Failure> {
        let admin = client.admin();
        let (ids, starts, ends) = match self.plan(&admin, soft).await {
            Ok(plan) => plan,
            Err(error) => return Ok(Response::Error(error)),
        };
        let cursor = |partition: i32, offset: i64| Cursor {
            topic: self.topic.to_owned(),
            partition,
            offset: self::offset(offset).unwrap_or_else(|| Offset::new(0).unwrap()),
        };
        let started = StreamEvent::Started {
            partitions: ids.iter().map(|p| cursor(*p, starts[p])).collect(),
        };
        emit(events, json!(started)).await?;

        let mut next = starts.clone();
        let mut done: BTreeSet<i32> = match &ends {
            Some(ends) => ids
                .iter()
                .copied()
                .filter(|p| starts[p] >= ends[p])
                .collect(),
            None => BTreeSet::new(),
        };
        let active: Vec<i32> = ids.iter().copied().filter(|p| !done.contains(p)).collect();
        let mut packer = Packer::new(self.limits);
        let reason = if active.is_empty() {
            EndReason::SnapshotEnd
        } else {
            let consumer = match self.consumer(client, &active, &starts).await {
                Ok(consumer) => consumer,
                Err(e) => return Ok(Response::Error(kafka_error(&e, Some(self.topic.into())))),
            };
            let polled = self
                .poll(
                    &consumer,
                    ends.as_ref(),
                    &mut next,
                    &mut done,
                    &mut packer,
                    events,
                    cancel,
                    soft,
                )
                .await;
            let _ = tokio::time::timeout(CLOSE, consumer.close()).await;
            match polled? {
                Ok(reason) => reason,
                Err(error) => return Ok(Response::Error(error)),
            }
        };
        let next_offsets = ids
            .iter()
            .map(|p| {
                let mut offset = next[p];
                if let Some(ends) = &ends
                    && done.contains(p)
                {
                    offset = offset.max(ends[p]);
                }
                cursor(*p, offset)
            })
            .collect();
        Ok(Response::Fetched(FetchSummary {
            scanned: packer.scanned,
            next_offsets,
            end_reason: reason,
        }))
    }

    /// The partitions, where each starts and, for a snapshot, its exclusive end.
    #[allow(clippy::type_complexity)]
    async fn plan(
        &self,
        admin: &AdminClient,
        soft: Instant,
    ) -> Result<(Vec<i32>, BTreeMap<i32, i64>, Option<BTreeMap<i32, i64>>), KafkaError> {
        let topic = self.topic;
        let ids = partitions(admin, topic, self.partitions, soft).await?;
        let committed = self.isolation == Isolation::ReadCommitted;
        let latest = bounds(admin, topic, &ids, OffsetSpec::Latest, committed, soft).await?;
        let earliest = || bounds(admin, topic, &ids, OffsetSpec::Earliest, false, soft);
        let starts = match self.start {
            Start::Beginning => earliest().await?,
            Start::End => latest.clone(),
            Start::Last(n) => {
                let earliest = earliest().await?;
                latest
                    .iter()
                    .map(|(p, end)| (*p, (end - i64::from(*n)).max(earliest[p])))
                    .collect()
            }
            Start::Timestamp(ms) => {
                let found = bounds(
                    admin,
                    topic,
                    &ids,
                    OffsetSpec::Timestamp(*ms),
                    committed,
                    soft,
                )
                .await?;
                found
                    .into_iter()
                    .map(|(p, offset)| (p, if offset < 0 { latest[&p] } else { offset }))
                    .collect()
            }
            Start::Offset(offset) => {
                let offset = offset.value();
                let earliest = earliest().await?;
                let high = bounds(admin, topic, &ids, OffsetSpec::Latest, false, soft).await?;
                for p in &ids {
                    if offset < earliest[p] || offset > high[p] {
                        return Err(error(
                            "OFFSET_OUT_OF_RANGE",
                            format!(
                                "Offset {offset} is outside {}..{} of partition {p}",
                                earliest[p], high[p]
                            ),
                            Some(format!("{topic}-{p}")),
                        ));
                    }
                }
                ids.iter().map(|p| (*p, offset)).collect()
            }
        };
        let ends = (self.mode == FetchMode::Snapshot).then_some(latest);
        Ok((ids, starts, ends))
    }

    async fn consumer(
        &self,
        client: &Client,
        active: &[i32],
        starts: &BTreeMap<i32, i64>,
    ) -> krafka::Result<Consumer> {
        let records = self.limits.max_records.clamp(1, 500) as i32;
        let fetch_bytes = self.limits.max_bytes.clamp(64 * 1024, 8 * 1024 * 1024) as i32;
        let consumer = client
            .consumer_without_group()
            .auto_offset_reset(AutoOffsetReset::None)
            .enable_auto_commit(false)
            .isolation_level(match self.isolation {
                Isolation::ReadCommitted => IsolationLevel::ReadCommitted,
                Isolation::ReadUncommitted => IsolationLevel::ReadUncommitted,
            })
            .metrics_push(false)
            .max_poll_records(records)
            .max_buffered_records(records)
            .fetch_max_bytes(fetch_bytes)
            .max_partition_fetch_bytes(fetch_bytes.min(1024 * 1024))
            .fetch_max_wait(Duration::from_millis(200))
            .build()
            .await?;
        consumer.assign(self.topic, active.to_vec()).await?;
        for p in active {
            consumer.seek(self.topic, *p, starts[p]).await?;
        }
        Ok(consumer)
    }

    #[allow(clippy::too_many_arguments)]
    async fn poll(
        &self,
        consumer: &Consumer,
        ends: Option<&BTreeMap<i32, i64>>,
        next: &mut BTreeMap<i32, i64>,
        done: &mut BTreeSet<i32>,
        packer: &mut Packer,
        events: &mpsc::Sender<Value>,
        cancel: &CancellationToken,
        soft: Instant,
    ) -> Result<Result<EndReason, KafkaError>, Failure> {
        let all = next.len();
        loop {
            if ends.is_some() && done.len() == all {
                return Ok(Ok(EndReason::SnapshotEnd));
            }
            if packer.full() {
                return Ok(Ok(EndReason::RecordLimit));
            }
            let now = Instant::now();
            if now >= soft {
                return Ok(Ok(EndReason::Timeout));
            }
            let polled = tokio::select! {
                polled = consumer.poll(POLL.min(soft - now)) => polled,
                () = cancel.cancelled() => {
                    return Err(Failure::new(Stage::Cancelled, "Cancelled"));
                }
            };
            let records = match polled {
                Ok(records) => records,
                Err(e) => {
                    if let Some(event) = packer.take() {
                        emit(events, json!(event)).await?;
                    }
                    return Ok(Err(kafka_error(&e, Some(self.topic.into()))));
                }
            };
            let mut stop = None;
            for consumed in records {
                let p = consumed.partition;
                if done.contains(&p) || next.get(&p).is_none_or(|n| consumed.offset < *n) {
                    continue;
                }
                if let Some(end) = ends.map(|ends| ends[&p])
                    && consumed.offset >= end
                {
                    done.insert(p);
                    continue;
                }
                let offset = consumed.offset;
                match packer.push(record(consumed)) {
                    Ok(ready) => {
                        next.insert(p, offset + 1);
                        if let Some(event) = ready {
                            emit(events, json!(event)).await?;
                        }
                    }
                    Err(reason) => {
                        stop = Some(reason);
                        break;
                    }
                }
            }
            if let Some(event) = packer.take() {
                emit(events, json!(event)).await?;
            }
            if let Some(reason) = stop {
                return Ok(Ok(reason));
            }
            if let Some(ends) = ends {
                for (p, end) in ends {
                    if !done.contains(p)
                        && (next[p] >= *end
                            || consumer
                                .position(self.topic, *p)
                                .await
                                .is_some_and(|position| position >= *end))
                    {
                        done.insert(*p);
                    }
                }
            }
        }
    }
}
