//! Artemis module: `#endpoint` and `#action` requests, action coloring and completion.
//! UI-behaviour tests.

use crate::support::tokens;
use serde_json::{Value, json};
use services_ui::completion::suggest;
use services_ui::console::parse;
use services_ui::syntax::{Language, State};

fn texts(source: &str, hints: &[(String, String)]) -> Vec<String> {
    discovered(source, hints, &Value::Null).1
}

/// The replaced range start and the candidates at the end of `source`.
fn discovered(source: &str, hints: &[(String, String)], discovery: &Value) -> (usize, Vec<String>) {
    let lines: Vec<_> = source.split('\n').collect();
    let ch = lines.last().unwrap().encode_utf16().count();
    let completion = suggest("artemis", "", source, lines.len() - 1, ch, hints, discovery);
    (
        completion.from,
        completion
            .candidates
            .into_iter()
            .map(|candidate| candidate.text)
            .collect(),
    )
}

#[test]
fn endpoint_is_the_broker_and_action_the_command() {
    assert_eq!(
        parse(
            "artemis",
            "#endpoint\nhttp://broker:8161\n#headers\nAuthorization: Basic eA==\n#action\nSEND /orders\nSET a = 'b'\n#options\nPersistent: false\n#body\n{}"
        )
        .unwrap(),
        json!({
            "url":"http://broker:8161","action":"SEND /orders\nSET a = 'b'",
            "headers":{"Authorization":"Basic eA=="},"persistent":"false","body":"{}"
        })
    );
    assert_eq!(
        parse("artemis", "#endpoint\nhttp://broker:8161").unwrap(),
        json!({"url":"http://broker:8161","action":""})
    );
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
    ] {
        assert_eq!(parse("artemis", source).unwrap_err(), error);
    }
}

#[test]
fn action_colors_command_resource_clauses_and_quotes_across_lines() {
    let mut state = State::default();
    let first = tokens(Language::Action, "BROWSE /DLQ", &mut state);
    assert_eq!(first[0], ("BROWSE".into(), Some("property")));
    assert_eq!(first[2], ("/DLQ".into(), Some("def")));
    let next = tokens(
        Language::Action,
        "WHERE name = 'it''s' AND n >= 4 LIMIT 20",
        &mut state,
    );
    assert!(next.contains(&("WHERE".into(), Some("keyword"))));
    assert!(next.contains(&("name".into(), None)));
    assert!(next.contains(&("'it''s'".into(), Some("string"))));
    assert!(next.contains(&(">=".into(), Some("operator"))));
    assert!(next.contains(&("20".into(), Some("number"))));
}

#[test]
fn action_offers_commands_first_then_clauses_never_headers() {
    let hints = vec![
        ("Authorization: Basic ".into(), "Saved hint".into()),
        ("http://localhost:8161".into(), "Saved hint".into()),
    ];
    let commands = texts("#check q\n#endpoint\nhttp://b\n#action\n", &hints);
    assert_eq!(commands.len(), 13);
    assert!(commands.iter().all(|text| text.ends_with(' ')));
    let clauses = texts("#check q\n#action\nBROWSE\n/orders W", &hints);
    assert_eq!(clauses[0], "WHERE ");
    assert!(!clauses.iter().any(|text| text.contains(':')));
    let body = texts("#check q\n#action\nSEND /q\n#body\nA", &hints);
    assert!(
        !body
            .iter()
            .any(|text| text.starts_with("Authorization") || text.contains("://")),
        "Headers and addresses stay in their sections: {body:?}"
    );
}

#[test]
fn action_offers_discovered_names_after_their_command() {
    let discovery = json!({
        "names":{"queues":["orders","my queue","it's"],"addresses":["events"]},
        "after":{"BROWSE":"queues","PUBLISH":"addresses","TO":"queues"},
    });
    let at = |source| discovered(source, &[], &discovery);
    assert_eq!(
        at("#check q\n#action\nBROWSE "),
        (
            7,
            vec!["/orders ".into(), "'my queue' ".into(), "'it''s' ".into()]
        )
    );
    assert_eq!(
        at("#check q\n#action\nbrowse /or"),
        (7, vec!["/orders ".into()])
    );
    assert_eq!(
        at("#check q\n#action\nPUBLISH\n"),
        (0, vec!["/events ".into()])
    );
    assert_eq!(
        at("#check q\n#action\nMOVE /orders TO /o").1,
        vec!["/orders "]
    );
    assert_eq!(at("#check q\n#action\nBROWSE /orders W").1[0], "WHERE ");
    assert_eq!(
        discovered("#check q\n#action\nBROWSE ", &[], &Value::Null).1[0],
        "WHERE ",
        "Without discovery the clauses are offered"
    );
}
