#![allow(dead_code)]
mod common;

use common::*;
use serde_json::json;
use services_sdk::wire::{self, answer, event, request};

fn connected(
    harness: &mut Harness,
    endpoint: &str,
    command: &str,
) -> (String, Vec<services_sdk::Cmd>) {
    let cmds = harness.run(&format!("#target\n{endpoint}\n#action\n{command}"));
    let cmds = resolve(harness, &cmds, &[]);
    let call = cmds.iter().find(|cmd| cmd.name == "driver.stream").unwrap();
    assert_eq!(call.args()["request"]["session"], true);
    let tag = call.tag.clone();
    (
        tag.clone(),
        harness.reply(&tag, json!({"connected": {"remote": null}})),
    )
}

#[test]
fn auth_and_select_are_gates_and_late_responses_after_stop_are_ignored() {
    for error_at in ["AUTH", "SELECT"] {
        let mut module = Harness::default();
        let (tag, cmds) = connected(
            &mut module,
            "redis://user:p%40ss@localhost:6380/2",
            "DEL game:1",
        );
        assert_eq!(
            write(&cmds),
            b"*3\r\n$4\r\nAUTH\r\n$4\r\nuser\r\n$4\r\np@ss\r\n"
        );
        let cmds = if error_at == "SELECT" {
            let cmds = frame(&mut module, &tag, b"+OK\r\n");
            assert_eq!(write(&cmds), b"*2\r\n$6\r\nSELECT\r\n$1\r\n2\r\n");
            frame(&mut module, &tag, b"-NOPERM select\r\n")
        } else {
            frame(&mut module, &tag, b"-WRONGPASS user p@ss\r\n")
        };
        assert!(cmds.iter().all(|cmd| cmd.name != "driver.write"));
        let failed = cmds
            .iter()
            .find(|cmd| cmd.name == "run.result")
            .unwrap()
            .args();
        assert_eq!(failed["outcome"], "ERROR");
        assert!(!failed.to_string().contains("p@ss"));
        assert!(frame(&mut module, &tag, b"+OK\r\n").is_empty());
    }
    let mut module = Harness::default();
    let (tag, _) = connected(&mut module, "redis://localhost:6380/0", "GET k");
    let cmds = module.send(event::Kind::Cancel("run:1".into()));
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].name, "driver.cancel");
    assert_eq!(cmds[0].tag, tag);
    assert!(frame(&mut module, &tag, b"+OK\r\n").is_empty());
}

#[test]
fn successful_setup_sends_one_command_and_returns_visible_binary_safe_results() {
    let mut module = Harness::default();
    let (tag, _) = connected(&mut module, "redis://:password@localhost:6380/2", "KEYS *");
    let cmds = frame(&mut module, &tag, b"+OK\r\n");
    assert!(String::from_utf8(write(&cmds)).unwrap().contains("SELECT"));
    let cmds = frame(&mut module, &tag, b"+OK\r\n");
    assert!(String::from_utf8(write(&cmds)).unwrap().contains("KEYS"));
    assert!(frame(&mut module, &tag, b"*2\r\n$6\r\ngame:1").is_empty());
    let cmds = frame(&mut module, &tag, b"\r\n$2\r\n\xff\0\r\n");
    let result = cmds
        .iter()
        .find(|cmd| cmd.name == "run.result")
        .unwrap()
        .args();
    assert_eq!(result["outcome"], "OK");
    let body: serde_json::Value = serde_json::from_str(result["body"].as_str().unwrap()).unwrap();
    assert_eq!(
        body,
        json!(["game:1", {"encoding":"base64", "data":"/wA="}])
    );
    assert!(!result.to_string().contains("password"));
}

#[test]
fn variables_are_resolved_after_argument_parsing_and_ca_is_not_a_file_path() {
    let mut module = Harness::default();
    let cmds =
        module.run("#target\n{{URL}}\n#options\nCA-PEM: {{CA}}\n#action\nSET key '{{VALUE}}'");
    let cmds = resolve(
        &mut module,
        &cmds,
        &[
            ("URL", "rediss://user:password@localhost:6380/0"),
            (
                "CA",
                "-----BEGIN CERTIFICATE-----\nline\n-----END CERTIFICATE-----",
            ),
            ("VALUE", "it's \"hello\"\nworld"),
        ],
    );
    let call = cmds.iter().find(|cmd| cmd.name == "driver.stream").unwrap();
    assert_eq!(call.args()["request"]["tls"], true);
    assert_eq!(call.args()["request"]["insecure"], false);
    assert!(
        call.args()["request"]["caPem"]
            .as_str()
            .unwrap()
            .contains('\n')
    );
    let tag = call.tag.clone();
    module.reply(&tag, json!({"connected": {"remote":null}}));
    let cmds = frame(&mut module, &tag, b"+OK\r\n");
    assert!(
        String::from_utf8(write(&cmds))
            .unwrap()
            .contains("it's \"hello\"\nworld")
    );

    let cmds =
        module.run("#target\nredis://localhost/0\n#options\nCA-Cert: /tmp/ca.pem\n#action\nPING");
    let cmds = resolve(&mut module, &cmds, &[]);
    assert!(
        cmds.iter()
            .find(|cmd| cmd.name == "run.result")
            .unwrap()
            .args()["error"]
            .as_str()
            .unwrap()
            .contains("CA-PEM")
    );
}

#[test]
fn pubsub_setup_timeout_does_not_expire_an_established_subscription() {
    let mut module = Harness::default();
    let (tag, _) = connected(&mut module, "redis://localhost:6380/0", "SUBSCRIBE news");
    let cmds = frame(
        &mut module,
        &tag,
        b"*3\r\n$9\r\nsubscribe\r\n$4\r\nnews\r\n:1\r\n",
    );
    assert!(
        cmds[0].args()["text"]
            .as_str()
            .unwrap()
            .contains("subscribed")
    );
    let deadline = tag.replace(":tcp", ":timeout");
    assert!(module.reply(&deadline, json!(null)).is_empty());
    let cmds = frame(
        &mut module,
        &tag,
        b"*3\r\n$7\r\nmessage\r\n$4\r\nnews\r\n$5\r\nhello\r\n",
    );
    assert_eq!(cmds[0].name, "run.output");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(cmds[0].args()["text"].as_str().unwrap())
            .unwrap()["payload"],
        "hello"
    );
    assert!(
        module
            .send(event::Kind::Cancel("run:1".into()))
            .iter()
            .all(|cmd| !cmd.args().to_string().contains("warning"))
    );

    let (_, _) = connected(&mut module, "redis://localhost:6380/0", "SUBSCRIBE news");
    let cmds = module.reply("valkey:2:timeout", json!(null));
    assert_eq!(
        cmds.iter()
            .find(|cmd| cmd.name == "run.result")
            .unwrap()
            .args()["outcome"],
        "ERROR"
    );
}

#[test]
fn command_completion_is_immediate_and_replaces_the_whole_current_word() {
    let mut module = Harness::default();
    let text = "#target\nredis://localhost:6380/0\n#action\n";
    let items = module.complete(text, text.len());
    assert!(items.items.iter().any(|item| item.label.trim() == "GET"));
    let text = format!("{text}GETDEL key");
    let pos = text.find("GETDEL").unwrap() + 2;
    let items = module.complete(&text, pos);
    assert_eq!(&text[items.from as usize..items.to as usize], "GETDEL");
    assert!(items.items.iter().any(|item| item.label.trim() == "GET"));
    for suffix in ["GET {{VA", "GET key\n#responce OK\nGE"] {
        let text = format!("#action\n{suffix}");
        assert!(module.complete(&text, text.len()).items.is_empty());
    }
    let text = "#action\nSET game:1 'Portal' ";
    let items = module.complete(text, text.len());
    assert!(items.items.iter().any(|item| item.label.trim() == "EX"));
    assert!(
        !items
            .items
            .iter()
            .any(|item| item.label.contains("discovery"))
    );
    let text = "#action\nSET game:1 EX ";
    assert!(
        module
            .complete(text, text.len())
            .items
            .iter()
            .any(|item| item.label.trim() == "EX")
    );
    let text = "#action\nSET game:1 value EX ";
    assert!(module.complete(text, text.len()).items.is_empty());
    let text = "#action\nSET game:1 value EX 60 NX ";
    let items = module.complete(text, text.len());
    assert!(
        !items
            .items
            .iter()
            .any(|item| matches!(item.label.trim(), "XX" | "PX" | "EX"))
    );
}

#[test]
fn highlighting_is_valkey_owned_and_response_markers_do_not_reenter_request_mode() {
    let mut module = Harness::default();
    let answer::Kind::Highlighted(lines) =
        module.call(request::Kind::Highlight(wire::HighlightRequest {
            state: vec![],
            lines: [
                "#action",
                "SET game:1 \"The Witcher 3\" EX 60",
                "#responce OK",
                "#action",
                "GET",
            ]
            .map(str::to_owned)
            .into(),
            columns: vec![],
        }))
    else {
        panic!("highlight")
    };
    let tokens = &lines.lines[1].tokens;
    let source = "SET game:1 \"The Witcher 3\" EX 60";
    let styled: Vec<_> = tokens
        .iter()
        .map(|token| {
            (
                &source[token.start as usize..(token.start + token.len) as usize],
                token.kind.as_str(),
            )
        })
        .collect();
    assert_eq!(
        styled,
        [
            ("SET", "keyword"),
            ("game:1", "property"),
            ("\"The Witcher 3\"", "string"),
            ("EX", "keyword"),
            ("60", "number")
        ]
    );
    assert!(lines.lines[4].tokens.is_empty());

    let answer::Kind::Highlighted(first) =
        module.call(request::Kind::Highlight(wire::HighlightRequest {
            state: vec![],
            lines: ["#action", "SET key \"first"].map(str::to_owned).into(),
            columns: vec![],
        }))
    else {
        panic!("highlight")
    };
    let answer::Kind::Highlighted(rest) =
        module.call(request::Kind::Highlight(wire::HighlightRequest {
            state: first.lines.last().unwrap().state.clone(),
            lines: ["second\" EX 60"].map(str::to_owned).into(),
            columns: vec![],
        }))
    else {
        panic!("highlight")
    };
    assert_eq!(rest.lines[0].tokens[0].kind, "string");
    assert_eq!(rest.lines[0].tokens[1].kind, "keyword");
    assert_eq!(rest.lines[0].tokens[2].kind, "number");
}

#[test]
fn malformed_commands_are_rejected_before_io_and_no_manual_discovery_exists() {
    let mut module = Harness::default();
    for command in ["SET k '", "[1]", "[]", "SUBSCRIBE", "MONITOR", "HELLO 3"] {
        let cmds = module.run(&format!("#target\nredis://localhost/0\n#action\n{command}"));
        assert_eq!(cmds.len(), 1, "{command}");
        assert_eq!(cmds[0].name, "run.result");
    }
    let cmds = module.run("#target\nredis://localhost/0\n#discovery\nMatch: *");
    assert_eq!(cmds[0].args()["outcome"], "ERROR");
    let manifest = services_sdk::Manifest::parse(include_str!("../manifest.json")).unwrap();
    assert!(
        manifest
            .check(
                "4.1.0",
                &std::collections::BTreeMap::from([("tcp".into(), "1.1".into())])
            )
            .is_ok()
    );
    assert!(
        manifest
            .check(
                "4.0.1",
                &std::collections::BTreeMap::from([("tcp".into(), "1".into())])
            )
            .is_err()
    );
}
