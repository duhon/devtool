//! The module through the real host driver against a throwaway broker
//! (`host/scripts/kafka-integration.sh`): topics `alpha`, `beta` (values one..five), `gamma`,
//! `delta` (2 partitions, for this test only) and group `g1` committed at beta:0 offset 3. The
//! tests pass without running when `KAFKA_TEST_BOOTSTRAP` is not set.

use prost::Message;
use serde_json::{Value, json};
use services_module_kafka::Kafka;
use services_native::drivers::{Drivers, Input};
use services_sdk::{
    Linked,
    wire::{self, answer, event, request},
};
use std::collections::VecDeque;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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

struct Runner {
    module: Linked<Kafka>,
    drivers: Drivers,
    result: Option<Value>,
    data: Option<Value>,
}

impl Runner {
    fn new() -> Self {
        Self {
            module: Linked::default(),
            drivers: Drivers::new().unwrap(),
            result: None,
            data: None,
        }
    }

    fn send(&self, kind: event::Kind) -> Vec<wire::Cmd> {
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
            wire::Answer::decode(self.module.call(&request.encode_to_vec()).as_slice()).unwrap();
        let Some(answer::Kind::Updated(updated)) = answer.kind else {
            panic!("{answer:?}")
        };
        updated.cmds
    }

    fn reply(&self, tag: &str, result: Result<Value, String>) -> Vec<wire::Cmd> {
        self.send(event::Kind::Reply(wire::Reply {
            tag: tag.into(),
            ok: result
                .as_ref()
                .ok()
                .map(|value| serde_json::to_vec(value).unwrap()),
            error: result.err().map(|message| wire::Failure::new(&message)),
        }))
    }

    async fn apply(&mut self, cmds: Vec<wire::Cmd>) {
        let mut cmds = VecDeque::from(cmds);
        while let Some(cmd) = cmds.pop_front() {
            let args = cmd.args();
            match cmd.name.as_str() {
                "vars.resolve" => cmds.extend(self.reply(&cmd.tag, Ok(args["text"].clone()))),
                "driver.call" => {
                    let result = self.drivers.call(&args, CancellationToken::new()).await;
                    cmds.extend(self.reply(&cmd.tag, result.map_err(|f| f.message)));
                }
                "driver.stream" => {
                    let (tx, mut rx) = mpsc::channel(1024);
                    let (_input, messages) = mpsc::channel(1);
                    let input = Input {
                        messages,
                        overflow: CancellationToken::new(),
                    };
                    let result = self
                        .drivers
                        .stream(&args, CancellationToken::new(), tx, input)
                        .await;
                    let mut cancelled = false;
                    while let Some(event) = rx.recv().await {
                        let out = self.reply(&cmd.tag, Ok(event));
                        cancelled = out
                            .iter()
                            .any(|c| c.name == "driver.cancel" && c.tag == cmd.tag);
                        cmds.extend(out);
                        if cancelled {
                            break;
                        }
                    }
                    if !cancelled {
                        cmds.extend(self.reply(&cmd.tag, result.map_err(|f| f.message)));
                        cmds.extend(self.send(event::Kind::End(cmd.tag.clone())));
                    }
                }
                "run.data" => {
                    self.data = Some(args["data"].clone());
                }
                "run.result" => self.result = Some(args),
                _ => {}
            }
        }
    }

    async fn run(&mut self, action: &str, metadata: &str, options: &str, payload: &str) -> Value {
        self.run_with(action, metadata, options, payload, false)
            .await
    }

    async fn run_with(
        &mut self,
        action: &str,
        metadata: &str,
        options: &str,
        payload: &str,
        transform: bool,
    ) -> Value {
        self.result = None;
        let broker = std::env::var("KAFKA_TEST_BOOTSTRAP").unwrap();
        let text = format!(
            "#target\n{broker}\n#action\n{action}\n#metadata\n{metadata}\n#options\n{options}\n#payload\n{payload}"
        );
        let cmds = self.send(event::Kind::Run(wire::RunEvent {
            tag: "run:1".into(),
            check: Some(check(&text)),
            transform,
        }));
        self.apply(cmds).await;
        if transform && let Some(data) = self.data.clone() {
            return data;
        }
        let result = self.result.take().expect("the run did not finish");
        eprintln!("--- {action}\n[{}] {}", result["outcome"], body(&result));
        result
    }

    async fn ok(&mut self, action: &str, metadata: &str, options: &str, payload: &str) -> String {
        let result = self.run(action, metadata, options, payload).await;
        assert_eq!(result["outcome"], "OK", "{action}: {result}");
        body(&result)
    }
}

fn body(result: &Value) -> String {
    result["body"]
        .as_str()
        .or_else(|| result["error"].as_str())
        .unwrap_or_default()
        .to_owned()
}

fn broker() -> bool {
    let set = std::env::var("KAFKA_TEST_BOOTSTRAP").is_ok();
    if !set {
        eprintln!("KAFKA_TEST_BOOTSTRAP is not set; skipping");
    }
    set
}

#[tokio::test(flavor = "multi_thread")]
async fn kafka_module_runs_every_command_through_the_host_driver() {
    if !broker() {
        return;
    }
    let mut runner = Runner::new();

    let brokers = runner.ok("BROKERS", "", "", "").await;
    assert!(brokers.contains("127.0.0.1"), "{brokers}");
    let topics = runner.ok("TOPICS", "", "", "").await;
    assert!(
        topics.contains("alpha") && topics.contains("beta"),
        "{topics}"
    );
    let describe = runner.ok("DESCRIBE /delta", "", "", "").await;
    assert!(describe.contains("delta"), "{describe}");
    let config = runner.ok("CONFIG /beta", "", "", "").await;
    assert!(config.contains("cleanup.policy"), "{config}");
    let offsets = runner.ok("OFFSETS /beta AT END", "", "", "").await;
    assert!(offsets.contains('5'), "{offsets}");

    let read = runner
        .ok("READ /beta FROM BEGINNING LIMIT 3", "", "", "")
        .await;
    assert!(read.contains("one") && read.contains("three"), "{read}");
    assert!(!read.contains("four"), "{read}");
    let last = runner.ok("READ /beta FROM LAST 2", "", "", "").await;
    assert!(last.contains("four") && last.contains("five") && !last.contains("three"));
    let filtered = runner
        .ok(
            "READ /beta FROM BEGINNING WHERE VALUE CONTAINS 'iv'",
            "",
            "",
            "",
        )
        .await;
    assert!(
        filtered.contains("five") && !filtered.contains("one"),
        "{filtered}"
    );

    let produced = runner
        .ok(
            "PRODUCE /delta",
            "Key: a\nHeader: trace=first\nHeader: trace=second\nNull-Header: flag",
            "Payload-Encoding: json",
            "{\"n\": 42}",
        )
        .await;
    assert!(produced.to_lowercase().contains("confirmed"), "{produced}");
    let tombstone = runner
        .ok("PRODUCE /delta", "Key: b", "Tombstone: true", "")
        .await;
    assert!(
        tombstone.to_lowercase().contains("confirmed"),
        "{tombstone}"
    );
    let back = runner
        .ok(
            "READ /delta FROM BEGINNING WHERE KEY = 'a'",
            "",
            "Format: vertical",
            "",
        )
        .await;
    assert!(back.contains("42") && back.contains("second"), "{back}");

    let groups = runner.ok("GROUPS", "", "", "").await;
    assert!(groups.contains("g1"), "{groups}");
    let group = runner.ok("GROUP /g1", "", "", "").await;
    assert!(group.contains("g1"), "{group}");
    let lag = runner.ok("LAG /g1", "", "Format: json", "").await;
    let rows: Value = serde_json::from_str(&lag).unwrap();
    let beta = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["topic"] == "beta")
        .unwrap_or_else(|| panic!("{lag}"));
    assert_eq!(beta["lag"], 2, "{lag}");

    let missing = runner.run("READ /missing", "", "", "").await;
    assert_ne!(missing["outcome"], "OK", "{missing}");

    let data = runner
        .run_with("READ /beta FROM BEGINNING LIMIT 2", "", "", "", true)
        .await;
    assert_eq!(
        data,
        json!([
            {"topic": "beta", "partition": 0, "offset": "0", "key": null, "headers": [], "value": "one",
             "timestampMs": data[0]["timestampMs"], "timestampType": "createTime"},
            {"topic": "beta", "partition": 0, "offset": "1", "key": null, "headers": [], "value": "two",
             "timestampMs": data[1]["timestampMs"], "timestampType": "createTime"},
        ])
    );
}
