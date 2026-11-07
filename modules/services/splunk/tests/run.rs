//! A Splunk run driven through the module's wire: pacing, the legacy fallback, 429 retries
//! and the early stop.

use prost::Message;
use serde_json::{Value, json};
use services_module_splunk::Splunk;
use services_sdk::Linked;
use services_sdk::wire::{self, answer, event, request};

type Cmds = Vec<(String, String, Value)>;

struct Run {
    linked: Linked<Splunk>,
    now: f64,
}

impl Run {
    fn new() -> Self {
        Self {
            linked: Linked::default(),
            now: 1_000_000.0,
        }
    }

    fn send(&self, kind: event::Kind) -> Cmds {
        let event = wire::Event {
            kind: Some(kind),
            now: self.now,
        };
        let bytes = wire::Request {
            kind: Some(request::Kind::Update(wire::UpdateRequest {
                event: Some(event),
                view: false,
            })),
        }
        .encode_to_vec();
        let answer = wire::Answer::decode(self.linked.call(&bytes).as_slice()).unwrap();
        let Some(answer::Kind::Updated(updated)) = answer.kind else {
            panic!("an update: {answer:?}")
        };
        updated
            .cmds
            .into_iter()
            .map(|cmd| {
                let args = cmd.args();
                (cmd.tag, cmd.name, args)
            })
            .collect()
    }

    fn reply(&self, tag: &str, value: Value) -> Cmds {
        self.send(event::Kind::Reply(wire::Reply {
            tag: tag.into(),
            ok: Some(serde_json::to_vec(&value).unwrap()),
            error: None,
        }))
    }

    fn end(&self, tag: &str) -> Cmds {
        self.send(event::Kind::End(tag.into()))
    }

    fn run(&self, text: &str) -> Cmds {
        let mut sections: Vec<wire::Section> = vec![];
        for (line, row) in text.lines().enumerate() {
            match row.strip_prefix('#') {
                Some(name) => sections.push(wire::Section {
                    name: name.into(),
                    text: String::new(),
                    line: line as u32,
                }),
                None => {
                    let section = sections.last_mut().unwrap();
                    section.text = format!("{}\n{row}", section.text).trim().into();
                }
            }
        }
        let check = wire::Check {
            columns: Vec::new(),
            text: text.into(),
            sections,
            ..wire::Check::default()
        };
        self.send(event::Kind::Run(wire::RunEvent {
            tag: RUN.into(),
            check: Some(check),
        }))
    }

    /// Starts a search with `options` and resolves it: the reply asks for the lock.
    fn resolved(&self, options: &str) -> Cmds {
        let cmds = self.run(&format!(
            "#endpoint\nhttps://splunk.fixture\n#options\n{options}\n#search\nindex={{{{idx}}}}"
        ));
        assert_eq!(names(&cmds), [("console:splunk:1:resolve", "vars.resolve")]);
        assert_eq!(cmds[0].2["mode"], "json");
        let text = cmds[0].2["text"]
            .as_str()
            .unwrap()
            .replace("{{idx}}", "main");
        self.reply("console:splunk:1:resolve", json!(text))
    }

    fn start(&self, options: &str) {
        let cmds = self.resolved(options);
        assert_eq!(cmds[0].1, "lock.acquire");
        assert_eq!(cmds[0].2["name"], "splunk:https://splunk.fixture");
    }
}

fn names(cmds: &Cmds) -> Vec<(&str, &str)> {
    cmds.iter()
        .map(|(tag, name, _)| (tag.as_str(), name.as_str()))
        .collect()
}

const RUN: &str = "console:splunk:1";

#[test]
fn waits_for_the_pacing_then_falls_back_to_the_legacy_api() {
    let mut run = Run::new();
    run.start("Count: 1");
    let cmds = run.reply("console:splunk:1:lock", json!({}));
    assert_eq!(names(&cmds), [("console:splunk:1:next", "kv.get")]);
    let cmds = run.reply("console:splunk:1:next", json!(run.now + 5_000.0));
    assert_eq!(names(&cmds), [("console:splunk:1:wait", "timer.after")]);
    assert_eq!(cmds[0].2["ms"], 5_000);
    run.now += 5_000.0;
    let cmds = run.reply("console:splunk:1:wait", json!({}));
    assert_eq!(
        names(&cmds),
        [
            ("", "kv.put"),
            ("console:splunk:1:call:1", "driver.stream"),
            ("console:splunk:1:idle:1", "timer.after")
        ]
    );
    let request = &cmds[1].2["request"];
    assert_eq!(
        request["url"],
        "https://splunk.fixture/services/search/v2/jobs/export"
    );
    assert!(request["body"].as_str().unwrap().contains("index%3Dmain"));
    assert_eq!(request["redirects"], 0);

    let cmds = run.reply(
        "console:splunk:1:call:1",
        json!({"head": {"status": 404, "statusText": "Not Found"}}),
    );
    assert_eq!(
        names(&cmds),
        [
            ("console:splunk:1:call:1", "driver.cancel"),
            ("", "kv.put"),
            ("console:splunk:1:wait", "timer.after")
        ]
    );
    assert_eq!(cmds[1].2["value"], json!(run.now + 13_000.0));
    run.now += 13_000.0;
    let cmds = run.reply("console:splunk:1:wait", json!({}));
    assert_eq!(
        cmds[1].2["request"]["url"],
        "https://splunk.fixture/services/search/jobs/export"
    );
    assert!(run.reply("console:splunk:1:idle:1", json!({})).is_empty());
    run.reply(
        "console:splunk:1:call:2",
        json!({"head": {"status": 200, "statusText": "OK"}}),
    );
    let cmds = run.reply(
        "console:splunk:1:call:2",
        json!({"chunk": "{\"preview\":false,\"result\":{\"host\":\"a\"}}\n{\"preview\":false,\"result\":{\"host\":\"b\"}}\n"}),
    );
    let called = names(&cmds);
    assert!(called.contains(&("console:splunk:1:call:2", "driver.cancel")));
    assert!(called.contains(&(RUN, "status.set")));
    let (_, _, result) = cmds
        .iter()
        .find(|(_, name, _)| name == "run.result")
        .unwrap();
    assert_eq!(result["outcome"], "OK");
    assert!(
        result["body"].as_str().unwrap().contains("| a "),
        "{result}"
    );
    assert_eq!(result["debug"]["export"]["legacyFallbacks"], 1);
    assert_eq!(result["debug"]["export"]["stoppedAfterCount"], true);
    assert!(run.end("console:splunk:1:call:2").is_empty());
}

#[test]
fn retries_after_429_and_reports_server_errors() {
    let mut run = Run::new();
    run.start("");
    run.reply("console:splunk:1:lock", json!({}));
    let cmds = run.reply("console:splunk:1:next", Value::Null);
    assert_eq!(cmds[1].1, "driver.stream");
    let cmds = run.reply(
        "console:splunk:1:call:1",
        json!({"head": {"status": 429, "headers": [["Retry-After", "20"]]}}),
    );
    assert_eq!(cmds[1].2["value"], json!(run.now + 20_000.0));
    assert_eq!(cmds[2].2["ms"], 20_000);
    run.now += 20_000.0;
    run.reply("console:splunk:1:wait", json!({}));
    run.reply("console:splunk:1:call:2", json!({"head": {"status": 500}}));
    run.reply(
        "console:splunk:1:call:2",
        json!({"chunk": "{\"messages\":[{\"type\":\"FATAL\",\"text\":\"bad search\"}]}"}),
    );
    let cmds = run.end("console:splunk:1:call:2");
    assert_eq!(names(&cmds), [("", "kv.put"), (RUN, "run.result")]);
    assert_eq!(cmds[1].2["error"], "bad search");
    assert_eq!(cmds[1].2["debug"]["export"]["rateLimitedRetries"], 1);
}

#[test]
fn a_stopped_search_still_counts_for_the_pacing() {
    let run = Run::new();
    run.start("");
    run.reply("console:splunk:1:lock", json!({}));
    run.reply("console:splunk:1:next", Value::Null);
    let cmds = run.send(event::Kind::Cancel(RUN.into()));
    assert_eq!(names(&cmds), [("", "kv.put")]);
    assert!(
        run.reply("console:splunk:1:call:1", json!({"chunk": "x"}))
            .is_empty()
    );
}

#[test]
fn bad_checks_finish_the_run_with_an_error() {
    let run = Run::new();
    let cmds = run.run("#endpoint\nhttps://x\n#count\n5\n#search\nindex=main");
    assert_eq!(
        cmds[0].2["error"],
        "Unsupported Splunk section #count; use #options."
    );
    let cmds = run.resolved("Preview: maybe");
    assert_eq!(names(&cmds), [(RUN, "run.result")]);
    assert_eq!(cmds[0].2["error"], "Splunk Preview must be true or false");
}
