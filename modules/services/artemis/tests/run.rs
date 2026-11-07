//! Artemis runs and discovery driven through the module's wire: the Jolokia calls a command
//! takes, one after another, and the table they end in.

use prost::Message;
use serde_json::{Value, json};
use services_module_artemis::Artemis;
use services_sdk::Linked;
use services_sdk::wire::{self, answer, event, request};

type Cmds = Vec<(String, String, Value)>;

const RUN: &str = "console:artemis:1";

#[derive(Default)]
struct Module(Linked<Artemis>);

fn check(text: &str) -> wire::Check {
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
    wire::Check {
        text: text.into(),
        sections,
        ..wire::Check::default()
    }
}

impl Module {
    fn send(&self, kind: event::Kind) -> Cmds {
        let bytes = wire::Request {
            kind: Some(request::Kind::Update(wire::UpdateRequest {
                event: Some(wire::Event {
                    kind: Some(kind),
                    now: 1_000.0,
                }),
                view: false,
            })),
        }
        .encode_to_vec();
        let answer = wire::Answer::decode(self.0.call(&bytes).as_slice()).unwrap();
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

    fn run(&self, text: &str) -> Cmds {
        self.send(event::Kind::Run(wire::RunEvent {
            tag: RUN.into(),
            check: Some(check(text)),
        }))
    }

    /// Resolves the variables of the `resolve` command `cmds` holds with `vars`.
    fn resolve(&self, cmds: &Cmds, vars: &[(&str, &str)]) -> Cmds {
        let (tag, name, args) = &cmds[cmds.len() - 1];
        assert_eq!(name, "vars.resolve");
        assert_eq!(args["mode"], "json");
        let mut text = args["text"].as_str().unwrap().to_owned();
        for (name, value) in vars {
            text = text.replace(&format!("{{{{{name}}}}}"), value);
        }
        self.reply(tag, json!(text))
    }
}

/// A Jolokia answer of the `http` driver.
fn answer(body: Value) -> Value {
    json!({"status":200,"statusText":"OK","httpVersion":"HTTP/1.1","headers":[],"url":"","body":body.to_string()})
}

fn names(cmds: &Cmds) -> Vec<(&str, &str)> {
    cmds.iter()
        .map(|(tag, name, _)| (tag.as_str(), name.as_str()))
        .collect()
}

const BROKER: &str = "org.apache.activemq.artemis:broker=\"b\"";

#[test]
fn queues_finds_the_broker_then_lists_and_shows_a_table() {
    let module = Module::default();
    let cmds = module.run("#endpoint\nhttp://{{host}}:8161\n#action\nQUEUES WHERE messages > 0 LIMIT 1\n#options\nColumns-Size: queue=3");
    let cmds = module.resolve(&cmds, &[("host", "broker")]);
    assert_eq!(cmds[0].1, "driver.call", "{cmds:?}");
    assert_eq!(names(&cmds), [("console:artemis:1:call:0", "driver.call")]);
    let request = &cmds[0].2["request"];
    assert_eq!(request["url"], "http://broker:8161/console/jolokia");
    assert_eq!(
        serde_json::from_str::<Value>(request["body"].as_str().unwrap()).unwrap()["type"],
        "search"
    );
    let cmds = module.reply(
        "console:artemis:1:call:0",
        answer(json!({"status":200,"value":[BROKER]})),
    );
    assert_eq!(names(&cmds), [("console:artemis:1:call:1", "driver.call")]);
    let listed = json!({"count":"2","data":[{"name":"orders","address":"orders","messageCount":"4","routingType":"ANYCAST"}]});
    let cmds = module.reply(
        "console:artemis:1:call:1",
        answer(json!({"status":200,"value":listed.to_string()})),
    );
    assert_eq!(names(&cmds), [(RUN, "status.set"), (RUN, "run.result")]);
    assert_eq!(cmds[0].2["text"], "1 row");
    let result = &cmds[1].2;
    assert_eq!(result["outcome"], "WARNING");
    let text = result["body"].as_str().unwrap();
    assert!(text.contains("| que |"), "{text}");
    assert!(text.contains("orders"), "{text}");
    assert_eq!(result["debug"]["calls"].as_array().unwrap().len(), 2);
    assert!(
        result["debug"]["warning"]
            .as_str()
            .unwrap()
            .starts_with("Showing 1 of 2 queues")
    );
}

#[test]
fn checks_fail_before_any_call() {
    for (source, error) in [
        (
            "#endpoint\nBROWSE http://broker:8161/orders",
            "#endpoint is only the broker URL now; move BROWSE to #action",
        ),
        (
            "#endpoint\nhttp://broker:8161\n#action\nLIST",
            "#action starts with a command: QUEUES, ADDRESSES, CONSUMERS, BROWSE, RECEIVE, SEND, PUBLISH, PURGE, MOVE, RETRY, PAUSE, RESUME, JOLOKIA",
        ),
        (
            "#endpoint\nhttp://broker:8161\n#action\nBROWSE /orders\n#body\nx",
            "BROWSE takes no #body",
        ),
        (
            "#endpoint\nhttp://broker:8161\n#options\nCount: 5",
            "Option \"count\" is now LIMIT n in #action",
        ),
        (
            "#endpoint\nhttp://broker:8161\n#options\nFormat: wide",
            "Format takes table, vertical or json, not \"wide\"",
        ),
    ] {
        let cmds = Module::default().run(source);
        assert_eq!(names(&cmds), [(RUN, "run.result")], "{source}");
        assert_eq!(cmds[0].2["error"], error);
    }
    let module = Module::default();
    let cmds = module
        .run("#endpoint\nhttp://broker:8161\n#action\nBROWSE /q\n#options\nPersistent: false");
    let cmds = module.resolve(&cmds, &[]);
    assert_eq!(
        cmds[0].2["error"],
        "Persistent applies to SEND or PUBLISH, not BROWSE"
    );
}

#[test]
fn a_failed_call_fails_the_run_and_cancel_forgets_it() {
    let module = Module::default();
    let cmds = module.run("#endpoint\nhttp://broker:8161\n#action\nPURGE /orders");
    module.resolve(&cmds, &[]);
    let cmds = module.send(event::Kind::Reply(wire::Reply {
        tag: "console:artemis:1:call:0".into(),
        ok: None,
        error: Some(wire::Failure {
            message: "Connection refused".into(),
            ..wire::Failure::default()
        }),
    }));
    assert_eq!(names(&cmds), [(RUN, "run.result")]);
    assert_eq!(cmds[0].2["error"], "Connection refused");

    let cmds = module.run("#endpoint\nhttp://broker:8161\n#action\nPURGE /orders");
    module.resolve(&cmds, &[]);
    assert!(module.send(event::Kind::Cancel(RUN.into())).is_empty());
    let late = module.reply(
        "console:artemis:1:call:0",
        answer(json!({"status":200,"value":[BROKER]})),
    );
    assert!(late.is_empty());
}

#[test]
fn discovery_feeds_action_completion() {
    let module = Module::default();
    let source = "#endpoint\nhttp://broker:8161\n#action\nBROWSE ";
    let cmds = module.send(event::Kind::Hints(wire::CompleteRequest {
        check: Some(check(source)),
        pos: source.len() as u32,
    }));
    assert_eq!(cmds[0].1, "status.set");
    let cmds = module.resolve(&cmds, &[]);
    assert_eq!(names(&cmds), [("discover:1:call:0", "driver.call")]);
    let cmds = module.reply(
        "discover:1:call:0",
        answer(json!({"status":200,"value":[BROKER]})),
    );
    let lists = [
        json!({"data":[{"name":"orders"},{"name":"$sys.x"},{"name":"tmp","temporary":"true"}]}),
        json!({"data":[{"name":"events"}]}),
    ];
    let bulk: Vec<Value> = lists
        .iter()
        .map(|list| json!({"status":200,"value":list.to_string()}))
        .collect();
    let cmds = module.reply(&cmds[0].0, answer(json!(bulk)));
    assert_eq!(cmds[0].1, "status.set");
    assert_eq!(cmds[0].2["text"], "Discovery · OK");
    let again = module.send(event::Kind::Hints(wire::CompleteRequest {
        check: Some(check(source)),
        pos: source.len() as u32,
    }));
    assert!(again.is_empty());
}
