use kafka_contract::*;
use prost::Message;
use serde_json::{Value, json};
use services_module_kafka::{Kafka, Msg};
use services_sdk::{
    Linked,
    wire::{self, answer, event, request},
};

type Commands = Vec<wire::Cmd>;

#[derive(Default)]
struct Harness(Linked<Kafka>);

fn check(text: &str) -> wire::Check {
    let mut sections: Vec<wire::Section> = Vec::new();
    for (line, text) in text.split('\n').enumerate() {
        if let Some(name) = text.strip_prefix('#') {
            sections.push(wire::Section {
                name: name.into(),
                text: String::new(),
                line: line as u32,
            });
        } else if let Some(section) = sections.last_mut() {
            if !section.text.is_empty() {
                section.text.push('\n');
            }
            section.text.push_str(text);
        }
    }
    wire::Check {
        service: "kafka".into(),
        text: text.into(),
        sections,
        ..wire::Check::default()
    }
}

impl Harness {
    fn send(&self, kind: event::Kind) -> Commands {
        let request = wire::Request {
            kind: Some(request::Kind::Update(wire::UpdateRequest {
                event: Some(wire::Event {
                    kind: Some(kind),
                    now: 1000.0,
                }),
                view: true,
            })),
        };
        let answer =
            wire::Answer::decode(self.0.call(&request.encode_to_vec()).as_slice()).unwrap();
        let Some(answer::Kind::Updated(updated)) = answer.kind else {
            panic!("{answer:?}")
        };
        updated.cmds
    }

    fn reply(&self, tag: &str, value: &impl serde::Serialize) -> Commands {
        self.send(event::Kind::Reply(wire::Reply {
            tag: tag.into(),
            ok: Some(serde_json::to_vec(value).unwrap()),
            error: None,
        }))
    }

    fn failure(&self, tag: &str, message: &str) -> Commands {
        self.send(event::Kind::Reply(wire::Reply {
            tag: tag.into(),
            ok: None,
            error: Some(wire::Failure::new(message)),
        }))
    }

    fn run(&self, name: &str, text: &str) -> Commands {
        self.run_with(name, text, false)
    }

    fn run_with(&self, name: &str, text: &str, transform: bool) -> Commands {
        self.send(event::Kind::Run(wire::RunEvent {
            tag: name.into(),
            check: Some(check(text)),
            transform,
        }))
    }

    fn resolve(&self, cmds: &Commands, replacements: &[(&str, &str)]) -> Commands {
        let cmd = named(cmds, "vars.resolve");
        assert_eq!(cmd.args()["mode"], "json");
        let mut value: Value = serde_json::from_str(cmd.args()["text"].as_str().unwrap()).unwrap();
        fn replace(value: &mut Value, replacements: &[(&str, &str)]) {
            match value {
                Value::String(s) => {
                    for (key, replacement) in replacements {
                        *s = s.replace(key, replacement);
                    }
                }
                Value::Array(v) => {
                    for value in v {
                        replace(value, replacements);
                    }
                }
                Value::Object(v) => {
                    for value in v.values_mut() {
                        replace(value, replacements);
                    }
                }
                _ => {}
            }
        }
        replace(&mut value, replacements);
        self.reply(&cmd.tag, &value.to_string())
    }

    fn start(&self, action: &str) -> String {
        let commands = self.run(
            "run",
            &format!("#target\nlocalhost:9092\n#action\n{action}"),
        );
        named(
            &self.resolve(&commands, &[]),
            if action.starts_with("READ") || action.starts_with("TAIL") {
                "driver.stream"
            } else {
                "driver.call"
            },
        )
        .tag
        .clone()
    }
}

fn named<'a>(commands: &'a Commands, name: &str) -> &'a wire::Cmd {
    commands
        .iter()
        .find(|cmd| cmd.name == name)
        .unwrap_or_else(|| panic!("Missing {name}: {commands:?}"))
}

fn result(commands: &Commands) -> Value {
    named(commands, "run.result").args()
}

fn cursor(topic: &str, partition: i32, offset: i64) -> Cursor {
    Cursor {
        topic: topic.into(),
        partition,
        offset: Offset::new(offset).unwrap(),
    }
}

fn record(offset: i64, key: &str) -> Record {
    Record {
        topic: "orders".into(),
        partition: 0,
        offset: Offset::new(offset).unwrap(),
        timestamp_ms: Some(1700000000000),
        timestamp_type: Some("createTime".into()),
        key: Some(Bytes::new(key.as_bytes())),
        value: Some(Bytes::new(br#"{"id":1}"#)),
        headers: Vec::new(),
    }
}

fn topics(names: &[&str]) -> Response {
    Response::Topics(
        names
            .iter()
            .map(|name| Topic {
                name: (*name).into(),
                internal: false,
                partitions: Vec::new(),
                error: None,
            })
            .collect::<Vec<_>>()
            .into(),
    )
}

#[test]
fn wire_run_builds_only_a_future_kafka_driver_call_and_formats_metadata() {
    let h = Harness::default();
    let cmds = h.run("run", "#target\n{{:BROKERS}}\n#action\nTOPICS");
    let cmds = h.resolve(&cmds, &[("{{:BROKERS}}", "broker-a:9092,broker-b:9092")]);
    let call = named(&cmds, "driver.call");
    assert_eq!(call.args()["driver"], "kafka");
    let request: Request = serde_json::from_value(call.args()["request"].clone()).unwrap();
    assert_eq!(
        request.connection.brokers,
        ["broker-a:9092", "broker-b:9092"]
    );
    let cmds = h.reply(&call.tag, &topics(&["orders"]));
    assert_eq!(result(&cmds)["outcome"], "OK");
    assert!(result(&cmds)["body"].as_str().unwrap().contains("orders"));
}

#[test]
fn a_transform_sees_decoded_records_and_renders_what_comes_back() {
    let h = Harness::default();
    let cmds = h.run_with(
        "run",
        "#target\nlocalhost:9092\n#action\nREAD /orders FROM BEGINNING LIMIT 10",
        true,
    );
    let tag = named(&h.resolve(&cmds, &[]), "driver.stream").tag.clone();
    h.reply(
        &tag,
        &StreamEvent::Started {
            partitions: vec![cursor("orders", 0, 0)],
        },
    );
    h.reply(
        &tag,
        &StreamEvent::Records {
            records: vec![record(0, "a")],
        },
    );
    let summary = FetchSummary {
        scanned: 1,
        next_offsets: vec![cursor("orders", 0, 1)],
        end_reason: EndReason::SnapshotEnd,
    };
    h.reply(&tag, &Response::Fetched(summary));
    let cmds = h.send(event::Kind::End(tag));
    assert!(cmds.iter().all(|cmd| cmd.name != "run.result"));
    let data = named(&cmds, "run.data").args()["data"].clone();
    assert_eq!(data[0]["key"], "a");
    assert_eq!(data[0]["value"], json!({"id": 1}));
    assert_eq!(data[0]["offset"], "0");

    let cmds = h.reply("run:data", &json!([{"key": "a", "id": 1}]));
    let shown = result(&cmds);
    assert_eq!(shown["outcome"], "OK");
    let body = shown["body"].as_str().unwrap();
    assert!(body.contains("key") && body.contains('a'), "{body}");
}

#[test]
fn filter_stops_at_match_limit_and_resume_cursor_does_not_skip_rest_of_batch() {
    let h = Harness::default();
    let tag = h.start("READ /orders FROM BEGINNING WHERE KEY = 'match' LIMIT 1");
    h.reply(
        &tag,
        &StreamEvent::Started {
            partitions: vec![cursor("orders", 0, 0)],
        },
    );
    let cmds = h.reply(
        &tag,
        &StreamEvent::Records {
            records: vec![
                record(0, "other"),
                record(1, "match"),
                record(2, "not examined"),
            ],
        },
    );
    let output = result(&cmds);
    assert!(output["body"].as_str().unwrap().contains("match"));
    assert_eq!(output["debug"]["details"]["summary"]["scanned"], 2);
    assert_eq!(
        output["debug"]["details"]["summary"]["nextOffsets"][0]["offset"],
        "2"
    );
    assert_eq!(named(&cmds, "driver.cancel").tag, tag);
    assert!(
        h.reply(
            &tag,
            &StreamEvent::Records {
                records: vec![record(2, "late")]
            }
        )
        .is_empty()
    );
}

#[test]
fn stream_batches_are_appended_and_final_summary_waits_for_end() {
    let h = Harness::default();
    let tag = h.start("READ /orders FROM BEGINNING LIMIT 10");
    h.reply(
        &tag,
        &StreamEvent::Started {
            partitions: vec![cursor("orders", 0, 0)],
        },
    );
    let cmds = h.reply(
        &tag,
        &StreamEvent::Records {
            records: vec![record(2, "key")],
        },
    );
    assert!(
        named(&cmds, "run.output").args()["text"]
            .as_str()
            .unwrap()
            .contains("key")
    );
    let summary = FetchSummary {
        scanned: 1,
        next_offsets: vec![cursor("orders", 0, 3)],
        end_reason: EndReason::SnapshotEnd,
    };
    assert!(h.reply(&tag, &Response::Fetched(summary)).is_empty());
    let cmds = h.send(event::Kind::End(tag));
    assert_eq!(result(&cmds)["outcome"], "OK");
    assert!(
        result(&cmds)["body"]
            .as_str()
            .unwrap()
            .contains("Next offsets")
    );
}

#[test]
fn invalid_stream_order_and_wrong_partition_fail_without_hiding_prior_records() {
    let h = Harness::default();
    let tag = h.start("TAIL /orders LIMIT 10");
    assert_eq!(
        result(&h.reply(
            &tag,
            &StreamEvent::Records {
                records: vec![record(0, "x")]
            }
        ))["outcome"],
        "ERROR"
    );

    let tag = h.start("READ /orders FROM BEGINNING LIMIT 10");
    h.reply(
        &tag,
        &StreamEvent::Started {
            partitions: vec![cursor("orders", 0, 0)],
        },
    );
    h.reply(
        &tag,
        &StreamEvent::Records {
            records: vec![record(0, "kept")],
        },
    );
    let cmds = h.reply(
        &tag,
        &StreamEvent::Records {
            records: vec![record(0, "duplicate")],
        },
    );
    let output = result(&cmds);
    assert_eq!(output["outcome"], "ERROR");
    assert!(output["body"].as_str().unwrap().contains("kept"));
    assert!(output["body"].as_str().unwrap().contains("non-monotonic"));
}

#[test]
fn cancellation_isolated_between_runs_and_late_resolution_is_ignored() {
    let h = Harness::default();
    let a = h.run("a", "#target\nlocalhost:9092\n#action\nTOPICS");
    let b = h.run("b", "#target\nlocalhost:9092\n#action\nTOPICS");
    h.send(event::Kind::Cancel("a".into()));
    assert!(h.resolve(&a, &[]).is_empty());
    let b = h.resolve(&b, &[]);
    let output = h.reply(&named(&b, "driver.call").tag, &topics(&["orders"]));
    assert_eq!(named(&output, "run.result").tag, "b");
}

#[test]
fn missing_driver_is_explicit_and_secrets_do_not_leak_into_json_errors_or_debug() {
    let h = Harness::default();
    let secret = "fake \"quoted\"\npassword";
    let cmds = h.run("run", "#target\nlocalhost:9092\n#action\nTOPICS\n#options\nFormat: json\nSecurity-Protocol: sasl_ssl\nSasl-Mechanism: PLAIN\nSasl-Username: test\nSasl-Password: {{:PASSWORD}}");
    // Multiline secrets must be resolved as values, not reparsed as extra option lines.
    let cmds = h.resolve(&cmds, &[("{{:PASSWORD}}", secret)]);
    let tag = named(&cmds, "driver.call").tag.clone();
    let error = KafkaError {
        code: "AUTHENTICATION_FAILED".into(),
        message: format!("{secret} rejected"),
        resource: None,
        retriable: false,
        possibly_written: None,
    };
    let output = result(&h.reply(&tag, &Response::Error(error)));
    let body: Value = serde_json::from_str(output["body"].as_str().unwrap()).unwrap();
    assert!(!body.to_string().contains("quoted"));
    assert!(!output["debug"].to_string().contains("quoted"));
    assert!(body.to_string().contains("[redacted]"));
    assert!(!output.to_string().contains(secret));

    let tag = h.start("TOPICS");
    let output = result(&h.failure(&tag, "Unknown driver \"kafka\""));
    assert!(
        output["body"]
            .as_str()
            .unwrap()
            .contains("requires native driver")
    );
}

#[test]
fn lag_joins_topics_without_joining_a_group_and_does_not_clamp_invalid_lag() {
    let h = Harness::default();
    let cmds = h.run(
        "run",
        "#target\nlocalhost:9092\n#action\nLAG /worker\n#options\nFormat: json",
    );
    let cmds = h.resolve(&cmds, &[]);
    let tag = named(&cmds, "driver.call").tag.clone();
    let cmds = h.reply(
        &tag,
        &Response::GroupOffsets(
            vec![
                GroupOffset {
                    group: "worker".into(),
                    topic: "orders".into(),
                    partition: 0,
                    committed: Some(Offset::new(10).unwrap()),
                    error: None,
                },
                GroupOffset {
                    group: "worker".into(),
                    topic: "orders".into(),
                    partition: 1,
                    committed: Some(Offset::new(30).unwrap()),
                    error: None,
                },
                GroupOffset {
                    group: "worker".into(),
                    topic: "orders".into(),
                    partition: 2,
                    committed: None,
                    error: None,
                },
            ]
            .into(),
        ),
    );
    let request: Request =
        serde_json::from_value(named(&cmds, "driver.call").args()["request"].clone()).unwrap();
    assert!(matches!(request.operation, Operation::ListOffsets { .. }));
    let cmds = h.reply(
        &tag,
        &Response::Offsets(
            vec![
                PartitionOffset {
                    topic: "orders".into(),
                    partition: 0,
                    offset: Some(Offset::new(20).unwrap()),
                    timestamp_ms: None,
                    error: None,
                },
                PartitionOffset {
                    topic: "orders".into(),
                    partition: 1,
                    offset: Some(Offset::new(25).unwrap()),
                    timestamp_ms: None,
                    error: None,
                },
                PartitionOffset {
                    topic: "orders".into(),
                    partition: 2,
                    offset: Some(Offset::new(40).unwrap()),
                    timestamp_ms: None,
                    error: None,
                },
            ]
            .into(),
        ),
    );
    let output = result(&cmds);
    let rows: Value = serde_json::from_str(output["body"].as_str().unwrap()).unwrap();
    assert_eq!(rows[0]["lag"], 10);
    assert!(rows[1]["lag"].is_null());
    assert!(rows[2]["lag"].is_null());
    assert_eq!(output["outcome"], "WARNING");
}

#[test]
fn discovery_is_scoped_to_connection_and_completions_use_real_metadata() {
    let h = Harness::default();
    let text = "#target\nlocalhost:9092\n#action\nREAD /ord";
    let cmds = h.send(event::Kind::Discover(check(text)));
    let cmds = h.resolve(&cmds, &[]);
    h.reply(&named(&cmds, "driver.call").tag, &topics(&["orders"]));
    let bytes = wire::Request {
        kind: Some(request::Kind::Complete(wire::CompleteRequest {
            check: Some(check(text)),
            pos: text.len() as u32,
        })),
    }
    .encode_to_vec();
    let answer = wire::Answer::decode(h.0.call(&bytes).as_slice()).unwrap();
    let Some(answer::Kind::Completions(completions)) = answer.kind else {
        panic!()
    };
    assert!(
        completions
            .items
            .iter()
            .any(|item| item.insert == "/orders")
    );
    let other = text.replace("localhost", "different-host");
    let bytes = wire::Request {
        kind: Some(request::Kind::Complete(wire::CompleteRequest {
            check: Some(check(&other)),
            pos: other.len() as u32,
        })),
    }
    .encode_to_vec();
    let answer = wire::Answer::decode(h.0.call(&bytes).as_slice()).unwrap();
    let Some(answer::Kind::Completions(completions)) = answer.kind else {
        panic!()
    };
    assert!(
        !completions
            .items
            .iter()
            .any(|item| item.insert == "/orders")
    );
}

#[test]
fn config_redaction_truncation_and_unknown_delivery_are_explicit() {
    let h = Harness::default();
    let cmds = h.run(
        "run",
        "#target\nlocalhost:9092\n#action\nCONFIG /orders LIMIT 1\n#options\nFormat: json",
    );
    let cmds = h.resolve(&cmds, &[]);
    let output = result(
        &h.reply(
            &named(&cmds, "driver.call").tag,
            &Response::Configs(
                vec![
                    ConfigEntry {
                        name: "secret".into(),
                        value: Some("do not show".into()),
                        source: "dynamic".into(),
                        sensitive: true,
                        error: None,
                    },
                    ConfigEntry {
                        name: "another".into(),
                        value: None,
                        source: "default".into(),
                        sensitive: false,
                        error: None,
                    },
                ]
                .into(),
            ),
        ),
    );
    assert!(!output.to_string().contains("do not show"));
    assert_eq!(output["outcome"], "WARNING");
    let body: Value = serde_json::from_str(output["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["data"]["truncated"], true);
    let tag = h.start("PRODUCE /orders");
    let output = result(&h.reply(
        &tag,
        &Response::Produced(Delivery {
            topic: "orders".into(),
            partition: None,
            offset: None,
            status: DeliveryStatus::Unknown,
            error: None,
        }),
    ));
    assert_eq!(output["outcome"], "WARNING");
    assert!(output["body"].as_str().unwrap().contains("unknown"));
}

#[test]
fn malformed_reply_and_stream_without_summary_cancel_owned_call() {
    let h = Harness::default();
    let tag = h.start("TOPICS");
    let cmds = h.reply(&tag, &json!({"unexpected": true}));
    assert_eq!(result(&cmds)["outcome"], "ERROR");
    assert_eq!(named(&cmds, "driver.cancel").tag, tag);
    let tag = h.start("READ /orders LIMIT 10");
    let cmds = h.send(event::Kind::End(tag));
    assert!(
        result(&cmds)["body"]
            .as_str()
            .unwrap()
            .contains("without a final summary")
    );
}

#[test]
fn defaults_are_session_scoped_and_invalid_ui_values_do_not_replace_them() {
    let h = Harness::default();
    let cmds = h.send(event::Kind::Ui(wire::UiEvent {
        msg: serde_json::to_vec(&Msg::Format).unwrap(),
        value: Some("json".into()),
    }));
    assert_eq!(named(&cmds, "kv.put").args()["session"], true);
    h.send(event::Kind::Ui(wire::UiEvent {
        msg: serde_json::to_vec(&Msg::MaxBytes).unwrap(),
        value: Some("0".into()),
    }));
    let tag = h.start("TOPICS");
    let output = result(&h.reply(&tag, &topics(&["orders"])));
    assert_eq!(output["language"], "json");
}

#[test]
fn multiline_pem_and_quoted_topic_variables_keep_their_original_argument_boundaries() {
    let h = Harness::default();
    let cmds = h.run("run", "#target\nlocalhost:9092\n#action\nDESCRIBE /{{:TOPIC}}\n#options\nSecurity-Protocol: ssl\nCa-Pem: {{:CA}}\nCertificate-Pem: {{:CERT}}\nKey-Pem: {{:KEY}}");
    let pem = "-----BEGIN FAKE-----\nline two\n-----END FAKE-----";
    let cmds = h.resolve(
        &cmds,
        &[
            ("{{:TOPIC}}", "topic LIMIT 1"),
            ("{{:CA}}", pem),
            ("{{:CERT}}", pem),
            ("{{:KEY}}", pem),
        ],
    );
    let request: Request =
        serde_json::from_value(named(&cmds, "driver.call").args()["request"].clone()).unwrap();
    assert!(
        matches!(request.operation, Operation::DescribeTopics { topics } if topics == ["topic LIMIT 1"])
    );
    assert_eq!(
        request.connection.tls.unwrap().key_pem.as_deref(),
        Some(pem)
    );
}

#[test]
fn timeout_cancel_and_byte_limits_are_bounded_without_cross_run_effects() {
    let h = Harness::default();
    let cmds = h.run(
        "run",
        "#target\nlocalhost:9092\n#action\nTAIL /orders\n#options\nMax-Bytes: 1024",
    );
    let cmds = h.resolve(&cmds, &[]);
    let tag = named(&cmds, "driver.stream").tag.clone();
    let deadline = named(&cmds, "timer.after").tag.clone();
    let output = h.reply(
        &tag,
        &json!({"event": "records", "data": {"oversized": "x".repeat(2000)}}),
    );
    assert!(
        result(&output)["body"]
            .as_str()
            .unwrap()
            .contains("exceeds Max-Bytes")
    );
    assert!(h.reply(&deadline, &Value::Null).is_empty());
    let cmds = h.run("next", "#target\nlocalhost:9092\n#action\nTOPICS");
    let deadline = named(&cmds, "timer.after").tag.clone();
    let output = h.reply(&deadline, &Value::Null);
    assert_eq!(named(&output, "run.result").tag, "next");
    assert!(h.resolve(&cmds, &[]).is_empty());
}

#[test]
fn driver_errors_preserve_records_and_produce_stop_warns_about_unknown_delivery() {
    let h = Harness::default();
    let tag = h.start("READ /orders FROM BEGINNING LIMIT 10");
    h.reply(
        &tag,
        &StreamEvent::Started {
            partitions: vec![cursor("orders", 0, 0)],
        },
    );
    h.reply(
        &tag,
        &StreamEvent::Records {
            records: vec![record(0, "kept")],
        },
    );
    let output = result(&h.reply(
        &tag,
        &Response::Error(KafkaError {
            code: "NOT_LEADER".into(),
            message: "leader changed".into(),
            resource: None,
            retriable: true,
            possibly_written: None,
        }),
    ));
    assert!(output["body"].as_str().unwrap().contains("kept"));
    assert!(!output["body"].as_str().unwrap().contains("written"));
    assert_eq!(output["outcome"], "ERROR");

    let tag = h.start("PRODUCE /orders");
    let output = result(&h.reply(
        &tag,
        &Response::Error(KafkaError {
            code: "DELIVERY_TIMEOUT".into(),
            message: "no acknowledgement".into(),
            resource: None,
            retriable: true,
            possibly_written: Some(true),
        }),
    ));
    assert!(
        output["body"]
            .as_str()
            .unwrap()
            .contains("may have been written")
    );
    h.start("PRODUCE /orders");
    let cmds = h.send(event::Kind::Cancel("run".into()));
    assert!(
        named(&cmds, "run.output").args()["text"]
            .as_str()
            .unwrap()
            .contains("delivery may continue")
    );
}

#[test]
fn manifest_requires_the_future_driver_instead_of_silently_loading_as_a_mock() {
    let manifest = services_sdk::Manifest::parse(include_str!("../manifest.json")).unwrap();
    let mut drivers = std::collections::BTreeMap::from([("http".into(), "1".into())]);
    assert!(
        manifest
            .check("5.1.0", &drivers)
            .unwrap_err()
            .contains("Host has none")
    );
    drivers.insert("kafka".into(), "1".into());
    assert!(manifest.check("5.1.0", &drivers).is_err());
    drivers.insert("kafka".into(), "1.1".into());
    manifest.check("5.1.0", &drivers).unwrap();
}

#[test]
fn explicit_offset_cannot_be_silently_reset_by_the_driver() {
    let h = Harness::default();
    let tag = h.start("READ /orders FROM OFFSET 100");
    let cmds = h.reply(
        &tag,
        &StreamEvent::Started {
            partitions: vec![cursor("orders", 0, 0)],
        },
    );
    assert_eq!(result(&cmds)["outcome"], "ERROR");
}

#[test]
fn init_leaves_the_hints_contribution_to_the_hints_panel() {
    let h = Harness::default();
    let contribution = h
        .send(event::Kind::Init(Vec::new()))
        .into_iter()
        .find(|cmd| cmd.name == "editor.contribute")
        .expect("a contribution")
        .args();
    assert!(contribution.get("hints").is_none(), "{contribution}");
    assert!(contribution["templates"]["kafka"]["action"].is_string());
}

fn complete(h: &Harness, text: &str) -> Vec<String> {
    let bytes = wire::Request {
        kind: Some(request::Kind::Complete(wire::CompleteRequest {
            check: Some(check(text)),
            pos: text.len() as u32,
        })),
    }
    .encode_to_vec();
    let answer = wire::Answer::decode(h.0.call(&bytes).as_slice()).unwrap();
    let Some(answer::Kind::Completions(completions)) = answer.kind else {
        panic!()
    };
    completions
        .items
        .into_iter()
        .map(|item| item.insert)
        .collect()
}

#[test]
fn discovery_adds_groups_to_group_slots_and_survives_a_group_listing_error() {
    let h = Harness::default();
    let text = "#target\nlocalhost:9092\n#action\nRESET ";
    let cmds = h.send(event::Kind::Discover(check(text)));
    let cmds = h.resolve(&cmds, &[]);
    let cmds = h.reply(&named(&cmds, "driver.call").tag, &topics(&["orders"]));
    let call = named(&cmds, "driver.call");
    let request: Request = serde_json::from_value(call.args()["request"].clone()).unwrap();
    assert_eq!(request.operation, Operation::ListGroups);
    let group = Group {
        id: "billing".into(),
        state: "Empty".into(),
        protocol: String::new(),
        members: Vec::new(),
        error: None,
    };
    h.reply(&call.tag, &Response::Groups(vec![group].into()));
    let items = complete(&h, text);
    assert_eq!(items, ["/billing"]);
    let items = complete(&h, &format!("{text}/billing "));
    assert!(items.contains(&"/orders".to_owned()) && !items.contains(&"/billing".to_owned()));
    assert_eq!(
        complete(&h, "#target\nlocalhost:9092\n#action\nDELETE GROUP "),
        ["/billing"]
    );

    let other = "#target\nother:9092\n#action\nREAD ";
    let cmds = h.send(event::Kind::Discover(check(other)));
    let cmds = h.resolve(&cmds, &[]);
    let cmds = h.reply(&named(&cmds, "driver.call").tag, &topics(&["events"]));
    let cmds = h.failure(&named(&cmds, "driver.call").tag, "denied");
    assert!(cmds.iter().all(|cmd| cmd.name != "notify"));
    assert!(complete(&h, other).contains(&"/events".to_owned()));
}

#[test]
fn write_results_report_each_change_and_partial_failures() {
    let h = Harness::default();
    let tag = h.start("PURGE /orders BEFORE OFFSET 5");
    let change = |partition, error: Option<&str>| Change {
        resource: format!("orders-{partition}"),
        partition: Some(partition),
        offset: Some(Offset::new(5).unwrap()),
        value: None,
        error: error.map(|message| KafkaError {
            code: "POLICY_VIOLATION".into(),
            message: message.into(),
            resource: None,
            retriable: false,
            possibly_written: None,
        }),
    };
    let cmds = h.reply(
        &tag,
        &Response::Changed(vec![change(0, None), change(1, Some("denied"))].into()),
    );
    let rendered = result(&cmds);
    assert_eq!(rendered["outcome"], "WARNING");
    let body = rendered["body"].as_str().unwrap();
    assert!(body.contains("purged") && body.contains("failed") && body.contains("denied"));

    let tag = h.start("DELETE /orders");
    let cmds = h.reply(
        &tag,
        &Response::Changed(
            vec![Change {
                resource: "orders".into(),
                partition: None,
                offset: None,
                value: None,
                error: None,
            }]
            .into(),
        ),
    );
    assert_eq!(result(&cmds)["outcome"], "OK");
    let tag = h.start("DELETE /orders");
    let cmds = h.reply(&tag, &topics(&["orders"]));
    assert_eq!(result(&cmds)["outcome"], "ERROR");
}

#[test]
fn discovery_starts_once_the_check_is_written_past_its_target() {
    let h = Harness::default();
    let hints = |text: &str| {
        h.send(event::Kind::Hints(wire::CompleteRequest {
            check: Some(check(text)),
            pos: text.len() as u32,
        }))
    };
    assert!(hints("#target\nlocalhost:90").is_empty());
    let text = "#target\nlocalhost:9092\n#action\n";
    let cmds = hints(text);
    let cmds = h.resolve(&cmds, &[]);
    let cmds = h.reply(&named(&cmds, "driver.call").tag, &topics(&["orders"]));
    h.failure(&named(&cmds, "driver.call").tag, "no groups");
    assert!(hints(text).is_empty(), "a fresh discovery is not repeated");
    assert!(complete(&h, &format!("{text}READ ")).contains(&"/orders".to_owned()));
}

#[test]
fn action_completions_follow_the_command_grammar() {
    let h = Harness::default();
    let at = |action: &str| complete(&h, &format!("#target\nlocalhost:9092\n#action\n{action}"));
    let first = at("");
    assert!(first.contains(&"READ ".to_owned()) && !first.iter().any(|i| i.starts_with("FROM")));
    let read = at("READ /orders ");
    assert!(read.contains(&"FROM BEGINNING".to_owned()) && read.contains(&"LIMIT ".to_owned()));
    assert!(!read.iter().any(|i| i.starts_with("TO ") || i == "READ "));
    let used = at("READ /orders PARTITION 1 FROM END ");
    assert!(
        !used
            .iter()
            .any(|i| i.starts_with("PARTITION") || i.starts_with("FROM"))
    );
    assert_eq!(
        at("READ /orders FROM "),
        ["BEGINNING", "END", "LAST ", "OFFSET ", "TIMESTAMP "]
    );
    assert!(at("READ /orders LIMIT ").is_empty());
    assert_eq!(at("PRODUCE /orders "), ["PARTITION "]);
    assert_eq!(at("DELETE "), ["GROUP "]);
    assert_eq!(at("CREATE /new "), ["PARTITIONS ", "REPLICAS "]);
    assert!(at("RESET /svc /orders ").contains(&"TO BEGINNING".to_owned()));
}

#[test]
fn a_blank_line_after_a_section_offers_the_unused_sections() {
    let h = Harness::default();
    let next = complete(&h, "#target\nkafka://127.0.0.1:9092\n");
    for section in ["#action", "#metadata", "#options", "#payload"] {
        assert!(
            next.iter().any(|i| i.starts_with(section)),
            "{section}: {next:?}"
        );
    }
    assert!(!next.iter().any(|i| i.starts_with("#target")));
    let payload = complete(
        &h,
        "#target\nlocalhost:9092\n#action\nPRODUCE /o\n#payload\n{}\n",
    );
    assert!(!payload.iter().any(|i| i.starts_with('#')));
}

#[test]
fn a_failed_or_pending_refresh_keeps_the_names_already_discovered() {
    let h = Harness::default();
    let text = "#target\nlocalhost:9092\n#action\nREAD ";
    let cmds = h.resolve(&h.send(event::Kind::Discover(check(text))), &[]);
    let cmds = h.reply(&named(&cmds, "driver.call").tag, &topics(&["orders"]));
    h.failure(&named(&cmds, "driver.call").tag, "no groups");
    let cmds = h.resolve(&h.send(event::Kind::Discover(check(text))), &[]);
    let call = named(&cmds, "driver.call").tag.clone();
    assert!(
        complete(&h, text).contains(&"/orders".to_owned()),
        "served while refreshing"
    );
    h.failure(&call, "broker down");
    assert!(
        complete(&h, text).contains(&"/orders".to_owned()),
        "kept after a failure"
    );
}
