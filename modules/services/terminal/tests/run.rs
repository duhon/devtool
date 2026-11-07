//! Terminal runs through the module's wire: variables resolved before `#env` is quoted, the
//! output streamed as it comes, the exit status in the run's status.

use prost::Message;
use serde_json::{Value, json};
use services_module_terminal::Terminal;
use services_sdk::Linked;
use services_sdk::wire::{self, answer, event, request};

type Cmds = Vec<(String, String, Value)>;

const RUN: &str = "console:terminal:1";

#[derive(Default)]
struct Module(Linked<Terminal>);

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
}

/// What the App does with a JSON-mode resolve: each value escaped as JSON string content.
fn resolve(cmds: &Cmds, vars: &[(&str, &str)]) -> (String, Value) {
    let (tag, name, args) = &cmds[0];
    assert_eq!(name, "vars.resolve");
    assert_eq!(args["mode"], "json");
    let mut text = args["text"].as_str().unwrap().to_owned();
    for (name, value) in vars {
        let escaped = serde_json::to_string(value).unwrap();
        text = text.replace(&format!("{{{{{name}}}}}"), &escaped[1..escaped.len() - 1]);
    }
    (tag.clone(), json!(text))
}

fn names(cmds: &Cmds) -> Vec<(&str, &str)> {
    cmds.iter()
        .map(|(tag, name, _)| (tag.as_str(), name.as_str()))
        .collect()
}

#[test]
fn env_values_are_resolved_then_quoted_and_output_streams() {
    let module = Module::default();
    let cmds = module
        .run("#endpoint\nssh {{host}}\n#command\necho \"$V\"; echo {{word}}\n#env\nV: {{value}}");
    let (tag, text) = resolve(
        &cmds,
        &[("host", "a b"), ("value", "it's \"x\""), ("word", "w")],
    );
    let cmds = module.reply(&tag, text);
    assert_eq!(names(&cmds), [("console:terminal:1:call", "driver.stream")]);
    assert_eq!(
        cmds[0].2["request"]["args"],
        json!(["a b", "export V='it'\\''s \"x\"'; echo \"$V\"; echo w"])
    );
    let call = "console:terminal:1:call";
    assert!(module.reply(call, json!({"spawned":{"pid":7}})).is_empty());
    let cmds = module.reply(call, json!({"stdout":"out\n"}));
    assert_eq!(names(&cmds), [(RUN, "run.output")]);
    assert_eq!(cmds[0].2["text"], "out\n");
    module.reply(call, json!({"stderr":"warning: one\n"}));
    module.reply(call, json!({"stdout":"more\n"}));
    assert!(module.reply(call, json!({"pid":7,"code":0})).is_empty());
    let cmds = module.send(event::Kind::End(call.into()));
    assert_eq!(names(&cmds), [(RUN, "status.set"), (RUN, "run.result")]);
    assert_eq!(cmds[0].2["text"], "warning: one");
    assert_eq!(cmds[1].2["body"], "out\nwarning: one\nmore\n");
    assert_eq!(cmds[1].2["outcome"], "WARNING");
    assert_eq!(cmds[1].2["language"], "text");
}

#[test]
fn failed_exits_signals_and_errors_finish_the_run() {
    let module = Module::default();
    let cmds = module.run("#endpoint\nbash -c\n#command\nfalse");
    let (tag, text) = resolve(&cmds, &[]);
    module.reply(&tag, text);
    let call = "console:terminal:1:call";
    module.reply(call, json!({"stderr":"boom\n"}));
    module.reply(call, json!({"pid":7,"code":2}));
    let cmds = module.send(event::Kind::End(call.into()));
    assert_eq!(cmds[0].2["text"], "exit 2 · boom");
    assert_eq!(cmds[1].2["outcome"], "ERROR");

    let cmds = module.run("#endpoint\nbash -c\n#command\nsleep 9");
    let (tag, text) = resolve(&cmds, &[]);
    module.reply(&tag, text);
    module.reply(
        call,
        json!({"pid":7,"code":null,"signal":"SIGKILL","outputIncomplete":true}),
    );
    let cmds = module.send(event::Kind::End(call.into()));
    assert_eq!(cmds[0].2["text"], "SIGKILL");
    assert!(
        cmds[1].2["body"]
            .as_str()
            .unwrap()
            .starts_with("Warning: Output capture")
    );

    let cmds = module.run("#endpoint\nbash -c\n#command\nls\n#options\nFormat: json");
    assert_eq!(names(&cmds), [(RUN, "run.result")]);
    assert_eq!(
        cmds[0].2["error"],
        "Unknown option \"Format\"; Terminal takes Repeat and Repeat-Until"
    );
    let cmds = module.run("#endpoint\nbash -c");
    assert_eq!(cmds[0].2["error"], "command is required");
}
