//! STOMP module: `SEND`/`SUBSCRIBE` checks, options, endpoint coloring and hints.
//! UI-behaviour tests.

use crate::support::tokens_for;
use serde_json::{Value, json};
use services_ui::completion::suggest;
use services_ui::console::parse;
use services_ui::syntax::{Language, State};

#[test]
fn send_and_subscribe_checks_become_broker_requests() {
    assert_eq!(
        parse(
            "stomp",
            "#endpoint\nSEND stomp://app:pw@broker:61613/orders\n#headers\ndestination-type: ANYCAST\ncontent-type: application/json\n#body\n{\"id\": 1}"
        )
        .unwrap(),
        json!({
            "command":"SEND","url":"stomp://app:pw@broker:61613/orders",
            "headers":{"destination-type":"ANYCAST","content-type":"application/json"},
            "body":"{\"id\": 1}"
        })
    );
    assert_eq!(
        parse(
            "stomp",
            "#endpoint\nsubscribe stomp+ssl://broker/news.#\n#options\nCount: 3\nWait: 20\nClient-Id: devtools\nHost: /\nColumns-Type: body=json"
        )
        .unwrap(),
        json!({
            "command":"SUBSCRIBE","url":"stomp+ssl://broker/news.#","headers":{},
            "count":"3","wait":"20","clientId":"devtools","vhost":"/","columnsType":"body=json"
        })
    );
}

#[test]
fn malformed_checks_explain_what_to_write() {
    for (source, error) in [
        (
            "#endpoint\nstomp://broker/q",
            "#endpoint must be SEND or SUBSCRIBE followed by a stomp:// URL",
        ),
        (
            "#endpoint\nPUBLISH stomp://broker/q",
            "#endpoint must be SEND or SUBSCRIBE followed by a stomp:// URL",
        ),
        (
            "#endpoint\nSUBSCRIBE stomp://broker/q\n#body\nx",
            "SUBSCRIBE takes no #body",
        ),
        (
            "#endpoint\nSUBSCRIBE stomp://broker/q\n#options\nAck: auto",
            "Unknown STOMP option \"ack\"; use Count, Wait, Client-Id, Host, Columns-Type or Columns-Size",
        ),
        (
            "#endpoint\nSEND stomp://broker/q\n#options\nColumns-Size: body=1em",
            "Column size for body must be a positive number of characters",
        ),
    ] {
        assert_eq!(parse("stomp", source).unwrap_err(), error, "{source}");
    }
}

#[test]
fn endpoint_commands_are_colored_like_rest_methods() {
    for command in ["SEND", "subscribe"] {
        let source = format!("{command} stomp://broker:61613/orders");
        let result = tokens_for(Language::Http, &source, &mut State::default());
        assert_eq!(
            result[0],
            (format!("{command} "), Some("property")),
            "{result:?}"
        );
    }
}

#[test]
fn default_endpoint_hints_are_urls_offered_after_the_command() {
    let manifest = services_ui::services::get("stomp").unwrap();
    let hints = manifest.console.hints.split("\n\n").next().unwrap();
    let urls: Vec<_> = hints.lines().skip(1).collect();
    assert_eq!(hints.lines().next(), Some("#endpoint"));
    assert_eq!(urls, ["stomp://", "stomp+ssl://"]);
}

#[test]
fn suggests_sections_commands_and_options() {
    let texts = |source: &str, line: usize, column: usize| {
        suggest("stomp", "", source, line, column, &[], &Value::Null)
            .candidates
            .into_iter()
            .map(|candidate| candidate.text)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        texts("#check STOMP\n#", 1, 1),
        ["#endpoint", "#headers", "#options", "#body", "#check"]
    );
    assert_eq!(
        texts("#check STOMP\n#endpoint\n", 2, 0),
        ["SEND ", "SUBSCRIBE "]
    );
    assert_eq!(
        texts("#check STOMP\n#endpoint\nSEND s", 2, 6),
        ["stomp://", "stomp+ssl://"]
    );
    assert_eq!(
        suggest(
            "rest",
            "",
            "#check REST\n#endpoint\n",
            2,
            0,
            &[],
            &Value::Null
        )
        .candidates
        .into_iter()
        .map(|candidate| candidate.text)
        .collect::<Vec<_>>(),
        ["GET ", "POST ", "PUT ", "PATCH ", "DELETE ", "HEAD "],
        "REST keeps its methods"
    );
    assert_eq!(
        texts("#check STOMP\n#options\n", 2, 0),
        [
            "Count: 10",
            "Wait: 5",
            "Client-Id: ",
            "Host: ",
            "Columns-Type: ",
            "Columns-Size: "
        ]
    );
}
