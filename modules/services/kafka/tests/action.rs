use kafka_contract::{FetchMode, Isolation, Operation, Start};
use services_module_kafka::action::{self, Defaults, Format, Source};

fn source(action: &str) -> Source {
    Source {
        target: "localhost:9092".into(),
        action: action.into(),
        metadata: String::new(),
        options: String::new(),
        payload: String::new(),
        defaults: Defaults::default(),
    }
}

#[test]
fn reads_parse_partitions_timestamp_filters_and_independent_scan_budget() {
    let mut input = source(
        "READ /orders PARTITIONS 0, 2 FROM TIMESTAMP '2026-11-22T20:00:00Z' WHERE KEY = 'a b' LIMIT 3",
    );
    input.options = "Format: vertical\nScan-Limit: 100\nTimeout: 2500ms".into();
    let plan = action::plan(input).unwrap();
    assert_eq!(plan.format, Format::Vertical);
    assert_eq!(plan.limit, 3);
    assert_eq!(plan.request.limits.max_records, 100);
    assert_eq!(plan.request.limits.timeout_ms, 2500);
    let Operation::Fetch {
        partitions,
        start,
        mode,
        isolation,
        ..
    } = plan.request.operation
    else {
        panic!()
    };
    assert_eq!(partitions, Some(vec![0, 2]));
    assert_eq!(start, Start::Timestamp(1_795_377_600_000));
    assert_eq!(mode, FetchMode::Snapshot);
    assert_eq!(isolation, Isolation::ReadCommitted);
}

#[test]
fn producer_keeps_multiline_payload_as_one_record_and_duplicate_headers() {
    let mut input = source("PRODUCE /orders PARTITION 2");
    input.metadata = "Key: order-1\nHeader: x=first\nHeader: x=second\nNull-Header: x".into();
    input.payload = "{\n  \"value\": \"hello\"\n}\n".into();
    input.options = "Payload-Encoding: json".into();
    let payload = input.payload.clone();
    let plan = action::plan(input).unwrap();
    let Operation::Produce { record } = plan.request.operation else {
        panic!()
    };
    assert_eq!(record.value.unwrap().decode(), payload.as_bytes());
    assert_eq!(record.partition, Some(2));
    assert_eq!(record.headers.len(), 3);
    assert!(record.headers[2].value.is_none());
}

#[test]
fn producer_distinguishes_tombstones_binary_and_empty_values() {
    let mut input = source("PRODUCE /users");
    input.metadata = "Key-Base64: AP8=".into();
    input.options = "Tombstone: true".into();
    let Operation::Produce { record } = action::plan(input.clone()).unwrap().request.operation
    else {
        panic!()
    };
    assert!(record.value.is_none());
    input.options.clear();
    let Operation::Produce { record } = action::plan(input.clone()).unwrap().request.operation
    else {
        panic!()
    };
    assert_eq!(record.value.unwrap().decode(), b"");
    input.options = "Payload-Encoding: base64".into();
    input.payload = "AP8=".into();
    let Operation::Produce { record } = action::plan(input).unwrap().request.operation else {
        panic!()
    };
    assert_eq!(record.value.unwrap().decode(), vec![0, 255]);
}

#[test]
fn invalid_actions_options_and_ignored_producer_inputs_fail_explicitly() {
    for action in [
        "PURGE /orders LIMIT 5",
        "RESET /svc /orders",
        "RESET /svc /orders TO LAST 3",
        "COMMIT /svc /orders",
        "ALTER /orders",
        "ALTER CONFIG /orders",
        "ALTER CONFIG /orders =1",
        "CREATE /orders REPLICAS x",
        "DELETE /orders REPLICAS 1",
        "READ /orders BEFORE OFFSET 1",
        "READ /orders LIMIT 0",
        "READ /orders FROM OFFSET -1",
        "READ /orders PARTITIONS 0,0",
        "READ /orders FROM LAST 0",
        "READ /orders LIMIT 1 LIMIT 2",
        "PRODUCE /orders LIMIT 2",
        "OFFSETS /orders AT OFFSET 2",
        "READ /orders WHERE body = 1",
        "READ /orders FROM TIMESTAMP '2026-11-22 20:00:00'",
        "READ '/unterminated",
    ] {
        assert!(action::plan(source(action)).is_err(), "{action}");
    }
    for options in [
        "Max-Bytes: 0",
        "Scan-Limit: 0",
        "Timeout: 0s",
        "Timeout: 999999999999999999999s",
        "Scan-Limit: 1\nScan-Limit: 2",
        "Unknown: value",
        "missing colon",
        "Sasl-Mechanism: PLAIN\nSasl-Username: user\nSasl-Password: fake",
        "Tombstone: true",
    ] {
        let mut input = source("READ /orders");
        input.options = options.into();
        assert!(action::plan(input).is_err(), "{options}");
    }
    let mut input = source("PRODUCE /orders");
    input.metadata = "Key: a\nKey-Base64: Yg==".into();
    assert!(action::plan(input).is_err());
}

#[test]
fn write_commands_parse_into_admin_operations() {
    let plan = |action: &str| action::plan(source(action)).unwrap().request.operation;
    let mut input = source("CREATE /orders PARTITIONS 3 REPLICAS 1");
    input.payload = "retention.ms=60000\ncleanup.policy = compact\n".into();
    let Operation::CreateTopic {
        topic,
        partitions,
        replicas,
        configs,
    } = action::plan(input).unwrap().request.operation
    else {
        panic!()
    };
    assert_eq!(
        (topic.as_str(), partitions, replicas),
        ("orders", Some(3), Some(1))
    );
    assert_eq!(configs["cleanup.policy"], "compact");
    assert!(
        matches!(plan("DELETE /orders"), Operation::DeleteTopic { topic } if topic == "orders")
    );
    assert!(
        matches!(plan("delete group /svc"), Operation::DeleteGroup { group } if group == "svc")
    );
    assert!(matches!(
        plan("ALTER /orders PARTITIONS 6"),
        Operation::CreatePartitions { total: 6, .. }
    ));
    let Operation::AlterConfigs { changes, .. } =
        plan("ALTER CONFIG /orders cleanup.policy=compact,delete retention.ms=")
    else {
        panic!()
    };
    assert_eq!(changes[0].name, "cleanup.policy");
    assert_eq!(changes[0].value.as_deref(), Some("compact,delete"));
    assert_eq!(changes[1].value, None);
    let Operation::DeleteRecords {
        partitions, before, ..
    } = plan("PURGE /orders PARTITIONS 0, 1 BEFORE OFFSET 10")
    else {
        panic!()
    };
    assert_eq!(partitions, Some(vec![0, 1]));
    assert_eq!(before.unwrap().value(), 10);
    let Operation::AlterGroupOffsets {
        group, topic, to, ..
    } = plan("RESET /svc /orders TO BEGINNING")
    else {
        panic!()
    };
    assert_eq!(
        (group.as_str(), topic.as_str(), to),
        ("svc", "orders", Start::Beginning)
    );
    let Operation::AlterGroupOffsets { partitions, to, .. } =
        plan("COMMIT /svc /orders PARTITION 2 OFFSET 42")
    else {
        panic!()
    };
    assert_eq!(partitions, Some(vec![2]));
    assert!(matches!(to, Start::Offset(offset) if offset.value() == 42));
}

#[test]
fn quoting_and_credentials_never_create_shell_requests() {
    assert_eq!(
        action::words("GROUP '/it''s group'").unwrap(),
        ["GROUP", "/it's group"]
    );
    let mut input = source("TOPICS");
    input.options = "Security-Protocol: sasl_ssl\nSasl-Mechanism: SCRAM-SHA-512\nSasl-Username: tester\nSasl-Password: fake ; $(echo ignored)".into();
    let plan = action::plan(input).unwrap();
    assert_eq!(
        plan.request.connection.secrets(),
        ["fake ; $(echo ignored)"]
    );
    assert!(matches!(plan.request.operation, Operation::ListTopics));
}
