use kafka_contract::*;
use serde_json::json;

#[test]
fn offsets_above_javascript_precision_are_strings_and_reject_lossy_inputs() {
    let offset = Offset::new(9_007_199_254_740_993).unwrap();
    assert_eq!(
        serde_json::to_value(&offset).unwrap(),
        json!("9007199254740993")
    );
    assert_eq!(
        serde_json::from_value::<Offset>(json!("9007199254740993")).unwrap(),
        offset
    );
    for invalid in [
        json!(9007199254740993_u64),
        json!("-1"),
        json!("01"),
        json!("+1"),
        json!("9223372036854775808"),
    ] {
        assert!(serde_json::from_value::<Offset>(invalid).is_err());
    }
    assert!(Offset::new(i64::MAX).unwrap().next().is_err());
}

#[test]
fn bytes_preserve_binary_empty_and_null_without_accepting_bad_base64() {
    let record = ProducerRecord {
        topic: "orders".into(),
        partition: None,
        key: Some(Bytes::new(&[0, 255])),
        value: None,
        headers: vec![
            Header {
                name: "x".into(),
                value: None,
            },
            Header {
                name: "x".into(),
                value: Some(Bytes::new(&[])),
            },
        ],
    };
    let value = serde_json::to_value(&record).unwrap();
    assert_eq!(value["key"], "AP8=");
    assert!(value["value"].is_null());
    assert!(value["headers"][0]["value"].is_null());
    assert_eq!(value["headers"][1]["value"], "");
    assert_eq!(
        serde_json::from_value::<ProducerRecord>(value).unwrap(),
        record
    );
    assert!(serde_json::from_value::<Bytes>(json!("not base64!")).is_err());
    assert!(serde_json::from_value::<Bytes>(json!("AB==")).is_err());
}

#[test]
fn request_rejects_unsafe_security_combinations_and_invalid_partitions() {
    let mut request = Request {
        version: VERSION,
        connection: Connection {
            brokers: vec!["[::1]:9092".into()],
            ..Connection::default()
        },
        operation: Operation::Fetch {
            topic: "orders".into(),
            partitions: Some(vec![0]),
            start: Start::Beginning,
            mode: FetchMode::Snapshot,
            isolation: Isolation::ReadCommitted,
        },
        limits: Limits::default(),
    };
    request.validate().unwrap();
    request.connection.sasl = Some(Sasl::Plain {
        username: "test".into(),
        password: "fake".into(),
    });
    assert!(request.validate().is_err());
    request.connection.security = Security::SaslSsl;
    request.validate().unwrap();
    request.connection.tls = Some(Tls {
        certificate_pem: Some("fake certificate".into()),
        ..Tls::default()
    });
    assert!(request.validate().is_err());
    request.connection.tls = None;
    if let Operation::Fetch { partitions, .. } = &mut request.operation {
        *partitions = Some(vec![0, 0]);
    }
    assert!(request.validate().is_err());
    request.limits.max_bytes = 1;
    assert!(request.validate().is_err());
}

#[test]
fn sasl_mechanisms_use_their_kafka_names_on_the_wire() {
    let token = Sasl::OAuthbearer { token: "t".into() };
    let value = serde_json::to_value(&token).unwrap();
    assert_eq!(value, json!({"mechanism": "oauthbearer", "token": "t"}));
    assert!(serde_json::from_value::<Sasl>(value).unwrap() == token);
    let scram = Sasl::ScramSha256 {
        username: "u".into(),
        password: "p".into(),
    };
    assert_eq!(
        serde_json::to_value(&scram).unwrap()["mechanism"],
        "scram-sha256"
    );
    assert!(
        serde_json::from_value::<Sasl>(json!({"mechanism": "o-authbearer", "token": "t"})).is_err()
    );
}
