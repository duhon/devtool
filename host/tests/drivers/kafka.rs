//! The kafka driver: validation, limits and mapping without a broker; with
//! `KAFKA_TEST_BOOTSTRAP` (see `scripts/kafka-integration.sh`) every operation against a real one.

use kafka_contract::{
    Batch, Bytes, Connection, DeliveryStatus, EndReason, FetchMode, Header, Isolation, Limits,
    Offset, Operation, ProducerRecord, Record, Request, Response, Sasl, Security, Start,
    StreamEvent, Tls,
};
use serde_json::json;
use services_native::drivers::{
    Failure,
    kafka::{self, Kafka, Packer},
};
use services_sdk::driver::Stage;
use services_testkit::{DataDir, native::Pipe};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn request(brokers: &str, operation: Operation) -> Request {
    Request {
        version: kafka_contract::VERSION,
        connection: Connection {
            brokers: brokers.split(',').map(String::from).collect(),
            ..Connection::default()
        },
        operation,
        limits: Limits {
            timeout_ms: 5_000,
            ..Limits::default()
        },
    }
}

fn bytes(text: &str) -> Option<Bytes> {
    Some(Bytes::new(text.as_bytes()))
}

fn text(bytes: &Option<Bytes>) -> Option<String> {
    bytes
        .as_ref()
        .map(|b| String::from_utf8(b.decode()).unwrap())
}

fn produce(topic: &str, key: Option<&str>, value: Option<&str>) -> Operation {
    Operation::Produce {
        record: ProducerRecord {
            topic: topic.into(),
            partition: None,
            key: key.and_then(bytes),
            value: value.and_then(bytes),
            headers: Vec::new(),
        },
    }
}

fn fetch(topic: &str, start: Start, mode: FetchMode) -> Operation {
    Operation::Fetch {
        topic: topic.into(),
        partitions: None,
        start,
        mode,
        isolation: Isolation::ReadCommitted,
    }
}

/// Runs a request on `driver`, the stream events it sent and its answer.
async fn run(driver: &Kafka, request: Request) -> (Vec<StreamEvent>, Result<Response, Failure>) {
    let (tx, mut rx) = mpsc::channel(64);
    let stream = matches!(request.operation, Operation::Fetch { .. });
    let answer = driver
        .run(request, CancellationToken::new(), stream.then_some(&tx))
        .await;
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(serde_json::from_value(event).unwrap());
    }
    let answer = answer.map(|value| serde_json::from_value(value).unwrap());
    (events, answer)
}

async fn answer(driver: &Kafka, request: Request) -> Response {
    match run(driver, request).await.1 {
        Ok(response) => response,
        Err(failure) => panic!("{}: {:?}", failure.message, failure.failure.causes),
    }
}

fn records(events: &[StreamEvent]) -> Vec<Record> {
    events
        .iter()
        .flat_map(|event| match event {
            StreamEvent::Records { records } => records.clone(),
            StreamEvent::Started { .. } => Vec::new(),
        })
        .collect()
}

fn record(offset: i64, value: &str) -> Record {
    Record {
        topic: "alpha".into(),
        partition: 0,
        offset: Offset::new(offset).unwrap(),
        timestamp_ms: Some(0),
        timestamp_type: Some("createTime".into()),
        key: None,
        value: bytes(value),
        headers: Vec::new(),
    }
}

#[tokio::test]
async fn kafka_requests_fail_validation_before_any_connection() {
    let driver = Kafka::default();
    let unreachable = "kafka.fixture.invalid:9092";
    let mut wrong_version = request(unreachable, Operation::ListTopics);
    wrong_version.version = 2;
    let mut duplicate = request(
        unreachable,
        fetch("alpha", Start::Beginning, FetchMode::Snapshot),
    );
    if let Operation::Fetch { partitions, .. } = &mut duplicate.operation {
        *partitions = Some(vec![0, 0]);
    }
    let mut sasl_without_protocol = request(unreachable, Operation::ListTopics);
    sasl_without_protocol.connection.sasl = Some(Sasl::Plain {
        username: "a".into(),
        password: "b".into(),
    });
    let mut half_identity = request(unreachable, Operation::ListTopics);
    half_identity.connection.security = Security::Ssl;
    half_identity.connection.tls = Some(Tls {
        certificate_pem: Some("certificate".into()),
        ..Tls::default()
    });
    let mut limits = request(unreachable, Operation::ListTopics);
    limits.limits.max_records = kafka_contract::MAX_RECORDS + 1;
    for request in [
        wrong_version,
        duplicate,
        sasl_without_protocol,
        half_identity,
        limits,
        request(unreachable, produce("alpha", None, None)),
        request(
            unreachable,
            fetch("alpha", Start::Last(0), FetchMode::Snapshot),
        ),
        request("user@kafka.fixture.invalid:9092", Operation::ListTopics),
    ] {
        let started = Instant::now();
        let Err(failure) = run(&driver, request).await.1 else {
            panic!("an invalid request must fail");
        };
        assert_eq!(failure.failure.stage, Stage::Send, "{}", failure.message);
        assert!(started.elapsed() < Duration::from_millis(500));
    }
    assert_eq!(driver.cached(), 0, "nothing is connected");
}

#[tokio::test]
async fn kafka_fetch_must_be_streamed() {
    let data = DataDir::new();
    let runtime = data.runtime();
    let request = request(
        "127.0.0.1:1",
        fetch("alpha", Start::Beginning, FetchMode::Snapshot),
    );
    let reply = runtime
        .dispatch(&super::call("kafka", 1, "call", json!(request)))
        .await;
    assert_eq!(reply["ok"], false, "{reply}");
    assert_eq!(reply["failure"]["stage"], "send", "{reply}");
}

#[tokio::test]
async fn kafka_connection_failures_name_their_stage_and_hide_secrets() {
    let driver = Kafka::default();
    let mut refused = request("127.0.0.1:1", Operation::ListTopics);
    refused.connection.security = Security::SaslPlaintext;
    refused.connection.sasl = Some(Sasl::ScramSha512 {
        username: "a".into(),
        password: "very-secret-password".into(),
    });
    let Err(failure) = run(&driver, refused).await.1 else {
        panic!("nothing listens on port 1");
    };
    assert!(
        failure.failure.stage == Stage::Connect,
        "{:?}: {}",
        failure.failure.stage,
        failure.message
    );
    let all = format!("{} {:?}", failure.message, failure.failure.causes);
    assert!(!all.contains("very-secret-password"), "{all}");

    let Err(failure) = run(
        &driver,
        request("kafka.fixture.invalid:9092", Operation::ListTopics),
    )
    .await
    .1
    else {
        panic!("the name does not resolve");
    };
    assert!(
        matches!(
            failure.failure.stage,
            Stage::Dns | Stage::Connect | Stage::Timeout
        ),
        "{:?}: {:?}",
        failure.failure.stage,
        failure.failure.causes
    );
}

#[tokio::test]
async fn kafka_caches_a_bounded_number_of_connections() {
    let driver = Kafka::default();
    for port in 1..=kafka::CACHE as u16 + 4 {
        let mut request = request(&format!("127.0.0.1:{port}"), Operation::ListTopics);
        request.limits.timeout_ms = 1_000;
        let _ = run(&driver, request).await;
        assert!(driver.cached() <= kafka::CACHE);
    }
}

#[test]
fn kafka_error_codes_keep_their_kafka_names() {
    use krafka::error::ErrorCode;
    assert_eq!(
        kafka::code_name(ErrorCode::UnknownTopicOrPartition),
        "UNKNOWN_TOPIC_OR_PARTITION"
    );
    assert_eq!(
        kafka::code_name(ErrorCode::OffsetOutOfRange),
        "OFFSET_OUT_OF_RANGE"
    );
    assert_eq!(
        kafka::code_name(ErrorCode::GroupIdNotFound),
        "GROUP_ID_NOT_FOUND"
    );
    assert_eq!(kafka::config_source(1), "DYNAMIC_TOPIC_CONFIG");
    assert_eq!(kafka::config_source(5), "DEFAULT_CONFIG");
}

#[test]
fn kafka_key_partitioning_is_the_java_clients() {
    // Java: Utils.murmur2("21".getBytes()) and ("abc"), the vectors of the Java client tests.
    assert_eq!(krafka::producer::murmur2(b"21"), 0xC5F2_F8EC);
    assert_eq!(krafka::producer::murmur2(b"abc"), 0x1C94_221B);
    let partition = |key: &[u8], n: u32| (krafka::producer::murmur2(key) & 0x7fff_ffff) % n;
    assert_eq!(partition(b"21", 3), (0xC5F2_F8EC_u32 & 0x7fff_ffff) % 3);
}

#[test]
fn kafka_producer_records_keep_null_empty_and_duplicate_headers() {
    let record = ProducerRecord {
        topic: "alpha".into(),
        partition: Some(2),
        key: bytes("a"),
        value: bytes(""),
        headers: vec![
            Header {
                name: "trace".into(),
                value: bytes("first"),
            },
            Header {
                name: "trace".into(),
                value: bytes("second"),
            },
            Header {
                name: "flag".into(),
                value: None,
            },
        ],
    };
    let out = kafka::producer_record(&record);
    assert_eq!(out.partition, Some(2));
    assert_eq!(out.key.as_deref(), Some(&b"a"[..]));
    assert_eq!(out.value.as_deref(), Some(&b""[..]), "empty is not null");
    let headers: Vec<_> = out
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_deref()))
        .collect();
    assert_eq!(
        headers,
        [
            ("trace", Some(&b"first"[..])),
            ("trace", Some(&b"second"[..])),
            ("flag", None)
        ]
    );
    let tombstone = kafka::producer_record(&ProducerRecord {
        value: None,
        headers: Vec::new(),
        ..record
    });
    assert_eq!(tombstone.value, None);
}

#[test]
fn kafka_packer_stops_at_the_record_and_byte_limits() {
    let limits = Limits {
        max_records: 2,
        ..Limits::default()
    };
    let mut packer = Packer::new(&limits);
    assert!(packer.push(record(0, "one")).unwrap().is_none());
    assert!(packer.push(record(1, "two")).unwrap().is_none());
    assert!(packer.full());
    assert_eq!(packer.push(record(2, "three")), Err(EndReason::RecordLimit));
    let Some(StreamEvent::Records { records }) = packer.take() else {
        panic!("two records are packed");
    };
    assert_eq!(records.len(), 2);
    assert_eq!(packer.scanned, 2);

    let one = serde_json::to_vec(&record(0, "x")).unwrap().len();
    let limits = Limits {
        max_bytes: one * 2 + 40,
        ..Limits::default()
    };
    let mut packer = Packer::new(&limits);
    packer.push(record(0, "x")).unwrap();
    packer.push(record(1, "y")).unwrap();
    assert_eq!(packer.push(record(2, "z")), Err(EndReason::ByteLimit));
    assert_eq!(packer.bytes, one * 2);
    let event = serde_json::to_vec(&packer.take().unwrap()).unwrap();
    assert!(event.len() <= limits.max_bytes, "an event fits Max-Bytes");
}

#[test]
fn kafka_packer_splits_events_into_bounded_chunks() {
    let limits = Limits {
        max_records: 1000,
        max_bytes: kafka_contract::MAX_BYTES,
        ..Limits::default()
    };
    let mut packer = Packer::new(&limits);
    let big = "v".repeat(10_000);
    let mut events = Vec::new();
    for offset in 0..100 {
        events.extend(packer.push(record(offset, &big)).unwrap());
    }
    events.extend(packer.take());
    assert!(events.len() > 1);
    let total: usize = events
        .iter()
        .map(|event| match event {
            StreamEvent::Records { records } => records.len(),
            StreamEvent::Started { .. } => 0,
        })
        .sum();
    assert_eq!(total, 100);
    for event in &events {
        assert!(serde_json::to_vec(event).unwrap().len() <= 256 * 1024);
    }
}

#[test]
fn kafka_lists_are_cut_to_the_limits_and_say_so() {
    let wrap = |batch: Batch<String>| {
        Response::Topics(Batch {
            items: Vec::new(),
            truncated: batch.truncated,
        })
    };
    let limits = Limits {
        max_records: 2,
        ..Limits::default()
    };
    let items = vec!["a".to_owned(), "b".into(), "c".into()];
    let Response::Offsets(batch) = kafka::fit(items.clone(), false, &limits, |b| {
        Response::Offsets(Batch {
            items: b
                .items
                .into_iter()
                .map(|topic| kafka_contract::PartitionOffset {
                    topic,
                    partition: 0,
                    offset: None,
                    timestamp_ms: None,
                    error: None,
                })
                .collect(),
            truncated: b.truncated,
        })
    }) else {
        unreachable!()
    };
    assert_eq!(batch.items.len(), 2);
    assert!(batch.truncated);
    let Response::Topics(batch) = kafka::fit(items.clone(), false, &Limits::default(), wrap) else {
        unreachable!()
    };
    assert!(!batch.truncated);
    let tiny = Limits {
        max_bytes: 40,
        ..Limits::default()
    };
    let Response::Offsets(batch) = kafka::fit(items, false, &tiny, |b| {
        Response::Offsets(Batch {
            items: Vec::new(),
            truncated: b.truncated,
        })
    }) else {
        unreachable!()
    };
    assert!(batch.truncated, "what does not fit Max-Bytes is dropped");
}

// Against a real broker (`scripts/kafka-integration.sh`): topics `alpha` (3 partitions, empty),
// `beta` (1 partition, values one..five), `gamma` (1 partition, empty), group `g1` committed at
// beta:0 offset 3; automatic topic creation is off.

fn broker() -> Option<String> {
    let bootstrap = std::env::var("KAFKA_TEST_BOOTSTRAP").ok();
    if bootstrap.is_none() {
        eprintln!("KAFKA_TEST_BOOTSTRAP is not set; skipping");
    }
    bootstrap
}

#[tokio::test]
async fn kafka_broker_metadata_topics_and_configs() {
    let Some(broker) = broker() else { return };
    let driver = Kafka::default();
    let Response::Cluster(cluster) =
        answer(&driver, request(&broker, Operation::DescribeCluster)).await
    else {
        panic!("a cluster");
    };
    assert_eq!(cluster.brokers.len(), 1);
    assert!(cluster.cluster_id.is_some());

    let Response::Topics(topics) = answer(&driver, request(&broker, Operation::ListTopics)).await
    else {
        panic!("topics");
    };
    let names: Vec<_> = topics.items.iter().map(|t| t.name.as_str()).collect();
    for topic in ["alpha", "beta", "gamma"] {
        assert!(names.contains(&topic), "{names:?}");
    }
    assert!(
        topics
            .items
            .iter()
            .all(|t| t.internal == t.name.starts_with("__")),
        "internal topics are listed and flagged"
    );

    let describe = Operation::DescribeTopics {
        topics: vec!["alpha".into(), "missing".into()],
    };
    let Response::Topics(topics) = answer(&driver, request(&broker, describe)).await else {
        panic!("topics");
    };
    let alpha = topics.items.iter().find(|t| t.name == "alpha").unwrap();
    assert_eq!(alpha.partitions.len(), 3);
    assert!(alpha.partitions.iter().all(|p| p.leader.is_some()));
    let missing = topics.items.iter().find(|t| t.name == "missing").unwrap();
    assert_eq!(
        missing.error.as_ref().map(|e| e.code.as_str()),
        Some("UNKNOWN_TOPIC_OR_PARTITION")
    );

    let config = Operation::DescribeConfigs {
        topic: "beta".into(),
    };
    let Response::Configs(configs) = answer(&driver, request(&broker, config)).await else {
        panic!("configs");
    };
    let policy = configs
        .items
        .iter()
        .find(|c| c.name == "cleanup.policy")
        .unwrap();
    assert_eq!(policy.value.as_deref(), Some("delete"));
    assert!(
        configs
            .items
            .iter()
            .all(|c| !c.sensitive || c.value.is_none())
    );
    let mut limited = request(
        &broker,
        Operation::DescribeConfigs {
            topic: "beta".into(),
        },
    );
    limited.limits.max_records = 3;
    let Response::Configs(configs) = answer(&driver, limited).await else {
        panic!("configs");
    };
    assert_eq!(configs.items.len(), 3);
    assert!(configs.truncated);
}

#[tokio::test]
async fn kafka_broker_offsets() {
    let Some(broker) = broker() else { return };
    let driver = Kafka::default();
    let offsets = |at| Operation::ListOffsets {
        topic: "beta".into(),
        partitions: None,
        at,
        isolation: Isolation::ReadCommitted,
    };
    let offset = |response: Response| match response {
        Response::Offsets(batch) => {
            assert_eq!(batch.items.len(), 1);
            batch.items[0].offset.as_ref().map(Offset::value)
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(
        offset(answer(&driver, request(&broker, offsets(Start::Beginning))).await),
        Some(0)
    );
    assert_eq!(
        offset(answer(&driver, request(&broker, offsets(Start::End))).await),
        Some(5)
    );
    assert_eq!(
        offset(answer(&driver, request(&broker, offsets(Start::Timestamp(0)))).await),
        Some(0)
    );
    let future = Start::Timestamp(4_000_000_000_000);
    assert_eq!(
        offset(answer(&driver, request(&broker, offsets(future))).await),
        None
    );
}

#[tokio::test]
async fn kafka_broker_reads_bounded_snapshots() {
    let Some(broker) = broker() else { return };
    let driver = Kafka::default();
    let read = |start| request(&broker, fetch("beta", start, FetchMode::Snapshot));
    let values = |events: &[StreamEvent]| -> Vec<String> {
        records(events)
            .iter()
            .filter_map(|r| text(&r.value))
            .collect()
    };

    let (events, answer) = run(&driver, read(Start::Beginning)).await;
    let StreamEvent::Started { partitions } = &events[0] else {
        panic!("Started comes first");
    };
    assert_eq!(partitions.len(), 1);
    assert_eq!(partitions[0].offset.value(), 0);
    assert_eq!(values(&events), ["one", "two", "three", "four", "five"]);
    let Ok(Response::Fetched(summary)) = answer else {
        panic!("{:?}", answer.err().map(|f| f.message));
    };
    assert_eq!(summary.end_reason, EndReason::SnapshotEnd);
    assert_eq!(summary.scanned, 5);
    assert_eq!(summary.next_offsets[0].offset.value(), 5);

    let mut limited = read(Start::Beginning);
    limited.limits.max_records = 2;
    let (events, answer) = run(&driver, limited).await;
    assert_eq!(values(&events), ["one", "two"]);
    let Ok(Response::Fetched(summary)) = answer else {
        panic!()
    };
    assert_eq!(summary.end_reason, EndReason::RecordLimit);
    assert_eq!(summary.next_offsets[0].offset.value(), 2);

    let (events, _) = run(&driver, read(Start::Offset(Offset::new(3).unwrap()))).await;
    assert_eq!(values(&events), ["four", "five"]);
    let (events, _) = run(&driver, read(Start::Last(2))).await;
    assert_eq!(values(&events), ["four", "five"]);
    let (events, _) = run(&driver, read(Start::Timestamp(0))).await;
    assert_eq!(values(&events).len(), 5);
    let (events, answer) = run(&driver, read(Start::End)).await;
    assert!(records(&events).is_empty());
    let Ok(Response::Fetched(summary)) = answer else {
        panic!()
    };
    assert_eq!(summary.end_reason, EndReason::SnapshotEnd);

    let mut small = read(Start::Beginning);
    small.limits.max_bytes = 700;
    let (events, answer) = run(&driver, small).await;
    let Ok(Response::Fetched(summary)) = answer else {
        panic!()
    };
    assert_eq!(summary.end_reason, EndReason::ByteLimit);
    assert!(records(&events).len() < 5);

    let (_, answer) = run(&driver, read(Start::Offset(Offset::new(99).unwrap()))).await;
    let Ok(Response::Error(error)) = answer else {
        panic!("an offset past the end is an error");
    };
    assert_eq!(error.code, "OFFSET_OUT_OF_RANGE");

    let mut partitions = read(Start::Beginning);
    if let Operation::Fetch { partitions, .. } = &mut partitions.operation {
        *partitions = Some(vec![7]);
    }
    let (_, answer) = run(&driver, partitions).await;
    assert!(matches!(answer, Ok(Response::Error(_))));
    let (_, answer) = run(
        &driver,
        request(
            &broker,
            fetch("missing", Start::Beginning, FetchMode::Snapshot),
        ),
    )
    .await;
    let Ok(Response::Error(error)) = answer else {
        panic!()
    };
    assert_eq!(error.code, "UNKNOWN_TOPIC_OR_PARTITION");
}

#[tokio::test]
async fn kafka_broker_produces_with_java_partitioning_and_reads_back() {
    let Some(broker) = broker() else { return };
    let driver = Kafka::default();
    let mut sent = Vec::new();
    for (key, value) in [("a", Some("{\"n\":1}")), ("b", Some("")), ("a", None)] {
        let mut op = produce("alpha", Some(key), value);
        if let Operation::Produce { record } = &mut op {
            record.headers = vec![
                Header {
                    name: "trace".into(),
                    value: bytes("first"),
                },
                Header {
                    name: "trace".into(),
                    value: bytes("second"),
                },
                Header {
                    name: "flag".into(),
                    value: None,
                },
            ];
        }
        let Response::Produced(delivery) = answer(&driver, request(&broker, op)).await else {
            panic!("a delivery");
        };
        assert_eq!(delivery.status, DeliveryStatus::Confirmed, "{delivery:?}");
        let expected = (krafka::producer::murmur2(key.as_bytes()) & 0x7fff_ffff) % 3;
        assert_eq!(delivery.partition, Some(expected as i32), "key {key}");
        sent.push((
            key,
            value,
            delivery.partition.unwrap(),
            delivery.offset.unwrap(),
        ));
    }
    let mut binary = produce("alpha", None, None);
    if let Operation::Produce { record } = &mut binary {
        record.partition = Some(2);
        record.value = Some(Bytes::new(&[0, 159, 255]));
    }
    let Response::Produced(delivery) = answer(&driver, request(&broker, binary)).await else {
        panic!("a delivery");
    };
    assert_eq!(delivery.status, DeliveryStatus::Confirmed);
    assert_eq!(delivery.partition, Some(2));

    for (key, value, partition, offset) in sent {
        let mut read = request(
            &broker,
            fetch("alpha", Start::Offset(offset.clone()), FetchMode::Snapshot),
        );
        read.limits.max_records = 1;
        if let Operation::Fetch { partitions, .. } = &mut read.operation {
            *partitions = Some(vec![partition]);
        }
        let (events, _) = run(&driver, read).await;
        let records = records(&events);
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.offset, offset);
        assert_eq!(text(&record.key).as_deref(), Some(key));
        assert_eq!(
            text(&record.value).as_deref(),
            value,
            "null and empty stay apart"
        );
        let headers: Vec<_> = record
            .headers
            .iter()
            .map(|h| (h.name.as_str(), text(&h.value)))
            .collect();
        assert_eq!(
            headers,
            [
                ("trace", Some("first".into())),
                ("trace", Some("second".into())),
                ("flag", None)
            ]
        );
    }

    let (_, missing) = run(
        &driver,
        request(&broker, produce("missing", None, Some("x"))),
    )
    .await;
    match missing {
        Ok(Response::Produced(delivery)) => {
            assert_ne!(delivery.status, DeliveryStatus::Confirmed);
        }
        Ok(Response::Error(_)) => {}
        other => panic!("{:?}", other.err().map(|f| f.message)),
    }
    let Response::Topics(topics) = answer(&driver, request(&broker, Operation::ListTopics)).await
    else {
        panic!("topics");
    };
    assert!(
        !topics.items.iter().any(|t| t.name == "missing"),
        "no topic is created"
    );
}

#[tokio::test]
async fn kafka_broker_tails_live_records_until_the_deadline() {
    let Some(broker) = broker() else { return };
    let driver = Kafka::default();
    let mut tail = request(&broker, fetch("gamma", Start::End, FetchMode::Live));
    tail.limits.timeout_ms = 4_000;
    let (tx, mut rx) = mpsc::channel(64);
    let reader = driver.clone();
    let task = tokio::spawn(async move {
        reader
            .run(tail, CancellationToken::new(), Some(&tx))
            .await
            .map(|v| serde_json::from_value::<Response>(v).unwrap())
    });
    let started: StreamEvent = serde_json::from_value(rx.recv().await.unwrap()).unwrap();
    assert!(matches!(started, StreamEvent::Started { .. }));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let Response::Produced(delivery) = answer(
        &driver,
        request(&broker, produce("gamma", Some("a"), Some("live"))),
    )
    .await
    else {
        panic!("a delivery");
    };
    assert_eq!(delivery.status, DeliveryStatus::Confirmed);
    let started = Instant::now();
    let answer = task.await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(4));
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(serde_json::from_value(event).unwrap());
    }
    let values: Vec<_> = records(&events)
        .iter()
        .filter_map(|r| text(&r.value))
        .collect();
    assert_eq!(values, ["live"]);
    let Ok(Response::Fetched(summary)) = answer else {
        panic!("{:?}", answer.err().map(|f| f.message));
    };
    assert_eq!(summary.end_reason, EndReason::Timeout);
    assert_eq!(summary.scanned, 1);
}

#[tokio::test]
async fn kafka_broker_stop_cancels_a_tail() {
    let Some(broker) = broker() else { return };
    let data = DataDir::new();
    let mut pipe = Pipe::start(data.runtime());
    let mut tail = request(&broker, fetch("gamma", Start::End, FetchMode::Live));
    tail.limits.timeout_ms = 60_000;
    pipe.send(&super::call("kafka", 1, "stream", json!(tail)))
        .await;
    let first = pipe.read().await;
    assert_eq!(first["type"], "data", "{first}");
    assert_eq!(first["event"]["event"], "started", "{first}");
    let stopped = Instant::now();
    pipe.send(&json!({"id": 1, "type": "stop"})).await;
    let (_, reply) = super::events(&mut pipe).await;
    assert_eq!(reply["failure"]["stage"], "cancelled", "{reply}");
    assert!(stopped.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn kafka_broker_groups_and_committed_offsets() {
    let Some(broker) = broker() else { return };
    let driver = Kafka::default();
    let Response::Groups(groups) = answer(&driver, request(&broker, Operation::ListGroups)).await
    else {
        panic!("groups");
    };
    assert!(groups.items.iter().any(|g| g.id == "g1"), "{groups:?}");

    let describe = Operation::DescribeGroups {
        groups: vec!["g1".into()],
    };
    let Response::Groups(groups) = answer(&driver, request(&broker, describe)).await else {
        panic!("groups");
    };
    assert_eq!(groups.items[0].id, "g1");
    assert!(groups.items[0].members.is_empty());

    let offsets = Operation::ListGroupOffsets { group: "g1".into() };
    let Response::GroupOffsets(offsets) = answer(&driver, request(&broker, offsets)).await else {
        panic!("group offsets");
    };
    let beta: Vec<_> = offsets
        .items
        .iter()
        .filter(|o| o.topic == "beta")
        .map(|o| (o.partition, o.committed.as_ref().map(Offset::value)))
        .collect();
    assert_eq!(beta, [(0, Some(3))]);
    assert!(!offsets.truncated);
}

#[tokio::test]
async fn kafka_broker_answers_through_the_native_protocol() {
    let Some(broker) = broker() else { return };
    let data = DataDir::new();
    let reply = data
        .runtime()
        .dispatch(&super::call(
            "kafka",
            1,
            "call",
            json!(request(&broker, Operation::DescribeCluster)),
        ))
        .await;
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["result"]["type"], "cluster", "{reply}");
}

#[tokio::test]
async fn kafka_broker_sasl_mechanisms_and_a_wrong_password() {
    let Some(_) = broker() else { return };
    let var = |name: &str| std::env::var(name).unwrap();
    let (bootstrap, username, password) = (
        var("KAFKA_TEST_SASL_BOOTSTRAP"),
        var("KAFKA_TEST_SASL_USERNAME"),
        var("KAFKA_TEST_SASL_PASSWORD"),
    );
    let driver = Kafka::default();
    let with = |sasl: Sasl| {
        let mut request = request(&bootstrap, Operation::DescribeCluster);
        request.connection.security = Security::SaslPlaintext;
        request.connection.sasl = Some(sasl);
        request
    };
    let (u, p) = (username.clone(), password.clone());
    for sasl in [
        Sasl::Plain {
            username: u.clone(),
            password: p.clone(),
        },
        Sasl::ScramSha256 {
            username: u.clone(),
            password: p.clone(),
        },
        Sasl::ScramSha512 {
            username: u,
            password: p,
        },
    ] {
        let response = answer(&driver, with(sasl)).await;
        assert!(matches!(response, Response::Cluster(_)), "{response:?}");
    }
    let wrong = "wrong-password-for-the-test";
    let (_, answer) = run(
        &driver,
        with(Sasl::ScramSha512 {
            username,
            password: wrong.into(),
        }),
    )
    .await;
    let Err(failure) = answer else {
        panic!("a wrong password must fail");
    };
    assert_eq!(failure.failure.stage, Stage::Auth, "{}", failure.message);
    let all = format!("{} {:?}", failure.message, failure.failure.causes);
    assert!(!all.contains(wrong) && !all.contains(&password), "{all}");
}
