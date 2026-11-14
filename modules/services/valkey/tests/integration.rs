#![allow(dead_code)]
mod common;

use common::*;
use serde_json::{Value, json};
use services_native::drivers::{Drivers, Input};
use services_sdk::wire::{self, answer, event, request};
use services_sdk::{Cmd, Failure};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct Session {
    cancel: CancellationToken,
    input: mpsc::Sender<Value>,
    task: tokio::task::JoinHandle<()>,
}

struct Runner {
    module: Harness,
    drivers: Drivers,
    sessions: BTreeMap<String, Session>,
    timers: Vec<tokio::task::JoinHandle<()>>,
    tx: mpsc::Sender<(String, Result<Value, String>)>,
    rx: mpsc::Receiver<(String, Result<Value, String>)>,
    result: Option<Value>,
    discovered: bool,
    output: String,
    variables: Vec<(String, String)>,
    started: Instant,
}

impl Runner {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel(256);
        Self {
            module: Harness::wasm(),
            drivers: Drivers::new().unwrap(),
            sessions: BTreeMap::new(),
            timers: Vec::new(),
            tx,
            rx,
            result: None,
            discovered: false,
            output: String::new(),
            variables: Vec::new(),
            started: Instant::now(),
        }
    }

    async fn apply(&mut self, cmds: Vec<Cmd>) {
        let mut cmds = std::collections::VecDeque::from(cmds);
        while let Some(cmd) = cmds.pop_front() {
            let args = cmd.args();
            match cmd.name.as_str() {
                "vars.resolve" => {
                    let mut text = args["text"].as_str().unwrap().to_owned();
                    for (name, value) in &self.variables {
                        let escaped = serde_json::to_string(value).unwrap();
                        text = text
                            .replace(&format!("{{{{{name}}}}}"), &escaped[1..escaped.len() - 1]);
                    }
                    cmds.extend(self.module.reply(&cmd.tag, json!(text)));
                }
                "driver.stream" => {
                    let cancel = CancellationToken::new();
                    let token = cancel.clone();
                    let (input, messages) = mpsc::channel(64);
                    let (events, mut incoming) = mpsc::channel(64);
                    let drivers = self.drivers.clone();
                    let tx = self.tx.clone();
                    let tag = cmd.tag.clone();
                    let task = tokio::spawn(async move {
                        let work = drivers.stream(
                            &args,
                            token,
                            events,
                            Input {
                                messages,
                                overflow: CancellationToken::new(),
                            },
                        );
                        tokio::pin!(work);
                        let result = loop {
                            tokio::select! {
                                result = &mut work => break result,
                                Some(event) = incoming.recv() => {
                                    if tx.send((tag.clone(), Ok(event))).await.is_err() { return; }
                                }
                            }
                        };
                        while let Ok(event) = incoming.try_recv() {
                            if tx.send((tag.clone(), Ok(event))).await.is_err() {
                                return;
                            }
                        }
                        let _ = tx.send((tag, result.map_err(|error| error.message))).await;
                    });
                    assert!(
                        self.sessions
                            .insert(
                                cmd.tag,
                                Session {
                                    cancel,
                                    input,
                                    task
                                }
                            )
                            .is_none()
                    );
                }
                "driver.write" => {
                    self.sessions[&cmd.tag].input.send(args).await.unwrap();
                }
                "driver.cancel" => {
                    if let Some(session) = self.sessions.remove(&cmd.tag) {
                        session.cancel.cancel();
                        tokio::time::timeout(Duration::from_secs(3), session.task)
                            .await
                            .unwrap()
                            .unwrap();
                    }
                }
                "timer.after" => {
                    let tx = self.tx.clone();
                    self.timers.push(tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(args["ms"].as_u64().unwrap()))
                            .await;
                        let _ = tx.send((cmd.tag, Ok(Value::Null))).await;
                    }));
                }
                "run.output" => self.output.push_str(args["text"].as_str().unwrap()),
                "run.result" => self.result = Some(args),
                "editor.relight" => self.discovered = true,
                "status.set" => {}
                other => panic!("Unexpected module command {other}"),
            }
        }
    }

    async fn next(&mut self) {
        let (tag, result) = tokio::time::timeout(Duration::from_secs(12), self.rx.recv())
            .await
            .expect("Module operation stalled")
            .unwrap();
        self.module.now = 1_000.0 + self.started.elapsed().as_secs_f64() * 1000.0;
        let cmds = match result {
            Ok(value) => self.module.reply(&tag, value),
            Err(message) => self.module.send(event::Kind::Reply(wire::Reply {
                tag,
                ok: None,
                error: Some(Failure::new(message)),
            })),
        };
        self.apply(cmds).await;
    }

    async fn start(&mut self, text: &str) {
        self.result = None;
        self.output.clear();
        let cmds = self.module.run(text);
        self.apply(cmds).await;
    }

    async fn command_result(&mut self, url: &str, command: &str) -> Value {
        self.start(&format!("#target\n{url}\n#action\n{command}"))
            .await;
        while self.result.is_none() {
            self.next().await;
        }
        let result = self.result.take().unwrap();
        assert!(
            self.sessions.is_empty(),
            "A finished check must close its connection"
        );
        result
    }

    async fn command(&mut self, url: &str, command: &str) -> String {
        let result = self.command_result(url, command).await;
        assert_eq!(result["outcome"], "OK", "{result}");
        result["body"].as_str().unwrap().to_owned()
    }

    async fn json(&mut self, url: &str, command: &str) -> Value {
        serde_json::from_str(&self.command(url, command).await).unwrap()
    }

    async fn discover(&mut self, text: &str, force: bool) {
        self.discovered = false;
        let event = if force {
            event::Kind::Discover(check(text))
        } else {
            event::Kind::Hints(wire::CompleteRequest {
                check: Some(check(text)),
                pos: text.len() as u32,
            })
        };
        let cmds = self.module.send(event);
        self.apply(cmds).await;
        while !self.discovered {
            self.next().await;
        }
    }

    async fn stop(&mut self) {
        let cmds = self.module.send(event::Kind::Cancel("run:1".into()));
        assert!(
            cmds.iter()
                .all(|cmd| !cmd.args().to_string().contains("warning"))
        );
        self.apply(cmds).await;
        assert!(self.sessions.is_empty());
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        for session in self.sessions.values() {
            session.cancel.cancel();
            session.task.abort();
        }
        for timer in &self.timers {
            timer.abort();
        }
    }
}

fn url(database: u32) -> String {
    let url = std::env::var("VALKEY_TEST_URL").expect("Use scripts/test-integration.sh");
    format!("{}/{database}", url.rsplit_once('/').unwrap().0)
}

#[tokio::test]
#[ignore = "requires scripts/test-integration.sh"]
async fn packaged_wasm_runs_full_command_scenario_through_actual_host_tcp_driver() {
    let mut runner = Runner::new();
    let url = url(14);
    for (command, expected) in [
        ("SET game:1 'The Witcher 3'", "OK"),
        ("GET game:1", "The Witcher 3"),
        ("EXISTS game:1", "1"),
        ("DEL game:1", "1"),
        ("EXISTS game:1", "0"),
        ("GET game:1", "null"),
        ("SET token abc EX 60", "OK"),
        ("GET token", "abc"),
        ("INCR visits", "1"),
        ("INCRBY visits 10", "11"),
        ("GET visits", "11"),
        ("HSET game:2 title Portal year 2007 genre Puzzle", "3"),
        ("HGET game:2 title", "Portal"),
        ("TYPE game:2", "hash"),
        ("LPUSH queue job1 job2", "2"),
        ("RPOP queue", "job1"),
        ("SADD tags:2 puzzle classic", "2"),
        ("ZADD top 100 duhon 80 alex", "2"),
        ("DBSIZE", "6"),
        ("PUBLISH unobserved:news hello", "0"),
        ("TTL visits", "-1"),
        ("TTL missing", "-2"),
    ] {
        assert_eq!(runner.command(&url, command).await, expected, "{command}");
    }
    let ttl = runner.json(&url, "TTL token").await.as_i64().unwrap();
    assert!((1..=60).contains(&ttl));
    assert_eq!(
        runner.json(&url, "LRANGE queue 0 -1").await,
        json!(["job2"])
    );
    let hash = runner.json(&url, "HGETALL game:2").await;
    let hash: BTreeMap<_, _> = hash
        .as_array()
        .unwrap()
        .chunks_exact(2)
        .map(|pair| (pair[0].as_str().unwrap(), pair[1].as_str().unwrap()))
        .collect();
    assert_eq!(
        hash,
        BTreeMap::from([("title", "Portal"), ("year", "2007"), ("genre", "Puzzle")])
    );
    let members = runner.json(&url, "SMEMBERS tags:2").await;
    let mut members = members.as_array().unwrap().clone();
    members.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    assert_eq!(members, [json!("classic"), json!("puzzle")]);
    assert_eq!(
        runner.json(&url, "ZREVRANGE top 0 -1 WITHSCORES").await,
        json!(["duhon", "100", "alex", "80"])
    );
    assert_eq!(runner.json(&url, "KEYS game:*").await, json!(["game:2"]));
    let mut cursor = "0".to_owned();
    let mut found = Vec::new();
    loop {
        let page = runner
            .json(&url, &format!("SCAN {cursor} MATCH game:* COUNT 1"))
            .await;
        found.extend(page[1].as_array().unwrap().clone());
        cursor = page[0].as_str().unwrap().into();
        if cursor == "0" {
            break;
        }
    }
    assert_eq!(found, [json!("game:2")]);
    assert!(
        runner
            .command(&url, "INFO server")
            .await
            .contains("valkey_version")
    );
    let client = redis::Client::open(url.as_str()).unwrap();
    let mut connection = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = redis::cmd("SET")
        .arg("binary")
        .arg(&[0xff_u8, 0][..])
        .query_async(&mut connection)
        .await
        .unwrap();
    assert_eq!(
        runner.json(&url, "GET binary").await,
        json!({"encoding":"base64","data":"/wA="})
    );
    assert_eq!(
        runner.command_result(&url, "SET invalid").await["outcome"],
        "ERROR"
    );
    let wrong = url.replace("test-password", "incorrect");
    assert_eq!(
        runner.command_result(&wrong, "SET auth-leak x").await["outcome"],
        "ERROR"
    );
    assert_eq!(runner.command(&url, "EXISTS auth-leak").await, "0");
}

#[tokio::test]
#[ignore = "requires scripts/test-integration.sh"]
async fn select_denial_never_sends_the_user_command_in_database_zero() {
    let url = url(0);
    let client = redis::Client::open(url.as_str()).unwrap();
    let mut connection = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = redis::cmd("SET")
        .arg("select-guard")
        .arg("keep")
        .query_async(&mut connection)
        .await
        .unwrap();
    let _: () = redis::cmd("ACL")
        .arg("SETUSER")
        .arg("no-select")
        .arg("on")
        .arg(">restricted")
        .arg("~*")
        .arg("+@all")
        .arg("-select")
        .query_async(&mut connection)
        .await
        .unwrap();
    let restricted = format!("{}/2", url.rsplit_once('/').unwrap().0)
        .replace("tester:test-password@", "no-select:restricted@");
    let mut runner = Runner::new();
    let result = runner.command_result(&restricted, "DEL select-guard").await;
    assert_eq!(result["outcome"], "ERROR");
    assert!(result["error"].as_str().unwrap().contains("NOPERM"));
    assert_eq!(runner.command(&url, "GET select-guard").await, "keep");
}

#[tokio::test]
#[ignore = "requires scripts/test-integration.sh"]
async fn discovery_feeds_key_and_hash_completion_with_cache_refresh_and_acl_fallback() {
    let url = url(13);
    let mut runner = Runner::new();
    runner
        .command(&url, "SET game:1 secret-value-must-not-leak")
        .await;
    runner
        .command(&url, "HSET game:2 'title field' Portal year 2007")
        .await;
    let text = format!("#target\n{url}\n#action\nHGET ");
    runner.discover(&text, false).await;
    let items = runner.module.complete(&text, text.len());
    assert!(items.items.iter().any(|item| item.label == "game:2"));
    assert!(!items.items.iter().any(|item| item.label == "game:1"));

    let fields = format!("#target\n{url}\n#action\nHGET game:2 ti");
    runner.discover(&fields, false).await;
    let items = runner.module.complete(&fields, fields.len());
    assert_eq!(items.items[0].label, "title field");
    assert_eq!(items.items[0].insert, "'title field'");
    let quoted = format!("#target\n{url}\n#action\nHGET game:2 'tiXtle field'");
    let pos = quoted.find("tiX").unwrap() + 2;
    let items = runner.module.complete(&quoted, pos);
    assert_eq!(
        &quoted[items.from as usize..items.to as usize],
        "'tiXtle field'"
    );
    assert_eq!(items.items[0].insert, "'title field'");

    runner.command(&url, "HSET game:late title new").await;
    runner.variables.push(("HASH".into(), "game:2".into()));
    let variable = format!("#target\n{url}\n#action\nHGET {{{{HASH}}}} ");
    runner.discover(&variable, false).await;
    assert!(
        runner
            .module
            .complete(&variable, variable.len())
            .items
            .iter()
            .any(|item| item.label == "title field")
    );
    runner.variables[0].1 = "game:missing".into();
    runner.discover(&variable, false).await;
    assert!(
        runner
            .module
            .complete(&variable, variable.len())
            .items
            .is_empty()
    );

    runner.discover(&text, false).await;
    assert!(
        !runner
            .module
            .complete(&text, text.len())
            .items
            .iter()
            .any(|item| item.label == "game:late")
    );
    runner.discover(&text, true).await;
    assert!(
        runner
            .module
            .complete(&text, text.len())
            .items
            .iter()
            .any(|item| item.label == "game:late")
    );
    let answer::Kind::Schema(schema) = runner.module.call(request::Kind::Schema(check(&text)))
    else {
        panic!("schema")
    };
    let schema = String::from_utf8(schema.json.unwrap()).unwrap();
    assert!(!schema.contains("secret-value-must-not-leak"));
    assert!(!schema.contains("test-password"));
    let other = text.replace("/13\n", "/12\n");
    assert!(runner.module.complete(&other, other.len()).items.is_empty());

    let client = redis::Client::open(url.as_str()).unwrap();
    let mut connection = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = redis::cmd("ACL")
        .arg("SETUSER")
        .arg("limited")
        .arg("on")
        .arg(">limited-pass")
        .arg("~*")
        .arg("+@all")
        .arg("-command")
        .arg("-type")
        .query_async(&mut connection)
        .await
        .unwrap();
    let limited = text.replace("tester:test-password@", "limited:limited-pass@");
    runner.discover(&limited, false).await;
    let commands = limited.replace("HGET ", "");
    let items = runner.module.complete(&commands, commands.len());
    assert!(items.items.iter().any(|item| item.label.trim() == "GET"));
}

#[tokio::test]
#[ignore = "requires scripts/test-integration.sh"]
async fn pubsub_is_live_plain_and_tls_validates_ca_hostname_and_stop() {
    let plain = url(0);
    let tls = std::env::var("VALKEY_TEST_TLS_URL").unwrap();
    let ca = std::env::var("VALKEY_TEST_CA_PEM").unwrap();
    let mut runner = Runner::new();
    runner.variables.push(("VALKEY_CA".into(), ca));
    let client = redis::Client::open(plain.as_str()).unwrap();
    let mut publisher = client.get_multiplexed_async_connection().await.unwrap();
    for (url, options, channel) in [
        (plain.as_str(), "", "test:plain-news"),
        (
            tls.as_str(),
            "#metadata\nCA-Certificate: {{VALKEY_CA}}\n",
            "test:tls-news",
        ),
    ] {
        runner
            .start(&format!(
                "#target\n{url}\n{options}#action\nSUBSCRIBE {channel}"
            ))
            .await;
        while !runner.output.contains("subscribed") {
            assert!(runner.result.is_none(), "{:?}", runner.result);
            runner.next().await;
        }
        // The timer is allowed to fire without closing an established subscription.
        if !options.is_empty() {
            tokio::time::sleep(Duration::from_millis(1100)).await;
        }
        let count: i64 = redis::cmd("PUBLISH")
            .arg(channel)
            .arg("hello")
            .query_async(&mut publisher)
            .await
            .unwrap();
        assert_eq!(count, 1);
        while !runner.output.contains("\"payload\":\"hello\"") {
            runner.next().await;
        }
        let _: i64 = redis::cmd("PUBLISH")
            .arg(channel)
            .arg(&[0xff_u8, 0][..])
            .query_async(&mut publisher)
            .await
            .unwrap();
        while !runner.output.contains("/wA=") {
            runner.next().await;
        }
        runner.stop().await;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let count: Vec<redis::Value> = redis::cmd("PUBSUB")
                    .arg("NUMSUB")
                    .arg(channel)
                    .query_async(&mut publisher)
                    .await
                    .unwrap();
                if count[1] == redis::Value::Int(0) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Stop must remove the subscription on the server");
    }
    runner
        .start(&format!("#target\n{tls}\n#action\nPING"))
        .await;
    while runner.result.is_none() {
        runner.next().await;
    }
    assert_eq!(runner.result.as_ref().unwrap()["outcome"], "ERROR");
    let wrong_host = tls.replace("localhost", "127.0.0.1");
    runner
        .start(&format!(
            "#target\n{wrong_host}\n#metadata\nCA-Certificate: {{{{VALKEY_CA}}}}\n#action\nPING"
        ))
        .await;
    while runner.result.is_none() {
        runner.next().await;
    }
    assert_eq!(runner.result.as_ref().unwrap()["outcome"], "ERROR");
    runner
        .start(&format!(
            "#target\n{tls}\n#metadata\nCA-Certificate: {{{{VALKEY_CA}}}}\n#action\nPING"
        ))
        .await;
    while runner.result.is_none() {
        runner.next().await;
    }
    assert_eq!(runner.result.as_ref().unwrap()["body"], "PONG");
}
