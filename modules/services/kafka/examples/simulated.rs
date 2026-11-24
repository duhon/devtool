//! An offline host reply demonstration; not a native Kafka driver.
use kafka_contract::*;
use serde_json::{Value, json};
use services_module_kafka::Kafka;
use services_sdk::{Check, Event, Module, Section};

fn main() {
    let mut module = Kafka::default();
    let check = Check {
        service: "kafka".into(),
        text: "#target\nlocalhost:9092\n#action\nREAD /orders FROM BEGINNING LIMIT 10\n#options\nFormat: vertical".into(),
        sections: [("target", "localhost:9092"),
            ("action", "READ /orders FROM BEGINNING LIMIT 10"), ("options", "Format: vertical")]
            .iter().enumerate().map(|(i, (name, text))| Section {
                name: (*name).into(), text: (*text).into(), line: i as u32,
            }).collect(),
        ..Check::default()
    };
    let commands = module.update(Event::Run {
        tag: "demo".into(),
        check,
        transform: false,
    });
    let resolve = commands
        .iter()
        .find(|cmd| cmd.name == "vars.resolve")
        .unwrap();
    let commands = module.update(Event::Reply {
        tag: resolve.tag.clone(),
        result: Ok(resolve.args()["text"].clone()),
    });
    let call = commands
        .iter()
        .find(|cmd| cmd.name == "driver.stream")
        .unwrap();
    println!(
        "OFFLINE SIMULATION (no network or native driver)\nRequest: {}",
        call.args()["request"]
    );
    let tag = call.tag.clone();
    let cursor = Cursor {
        topic: "orders".into(),
        partition: 0,
        offset: Offset::new(0).unwrap(),
    };
    module.update(Event::Reply {
        tag: tag.clone(),
        result: Ok(serde_json::to_value(StreamEvent::Started {
            partitions: vec![cursor],
        })
        .unwrap()),
    });
    module.update(Event::Reply {
        tag: tag.clone(),
        result: Ok(serde_json::to_value(StreamEvent::Records {
            records: vec![Record {
                topic: "orders".into(),
                partition: 0,
                offset: Offset::new(0).unwrap(),
                timestamp_ms: Some(1795377600000),
                timestamp_type: Some("createTime".into()),
                key: Some(Bytes::new(b"order-123")),
                value: Some(Bytes::new(br#"{"orderId":123,"status":"created"}"#)),
                headers: vec![Header {
                    name: "content-type".into(),
                    value: Some(Bytes::new(b"application/json")),
                }],
            }],
        })
        .unwrap()),
    });
    module.update(Event::Reply {
        tag: tag.clone(),
        result: Ok(serde_json::to_value(Response::Fetched(FetchSummary {
            scanned: 1,
            next_offsets: vec![Cursor {
                topic: "orders".into(),
                partition: 0,
                offset: Offset::new(1).unwrap(),
            }],
            end_reason: EndReason::SnapshotEnd,
        }))
        .unwrap()),
    });
    let commands = module.update(Event::End { tag });
    let output: Value = commands
        .iter()
        .find(|cmd| cmd.name == "run.result")
        .unwrap()
        .args();
    println!("\n{}\n", output["body"].as_str().unwrap());
    assert_eq!(output["outcome"], json!("OK"));
}
