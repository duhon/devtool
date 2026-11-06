//! Console document parsing shared by every service: markers, hints, import/
//! export and response replacement. UI-behaviour tests.

use serde_json::json;
use services_module_ui::command::shell_words;
use services_ui::console::{
    self, Format, RunOptions, export, import, parse, parse_run, replace_response,
};

#[test]
fn hints_allow_free_text_but_reject_storage_boundaries_and_broken_references() {
    for text in [
        "#endpoint\nhttps://{{var:host}}/api\n#headers\nX-Token: value",
        "#query\nquery { viewer { name } }",
        "#body\n{\"name\": \"\"}",
        "plain completion",
    ] {
        assert!(console::validate_hints(text).is_ok(), "{text}");
    }
    for text in [
        "## graphql\nleaked",
        "https://{{var:host",
        "{{broken}}",
        "a\0b",
    ] {
        assert!(console::validate_hints(text).is_err(), "{text}");
    }
}

#[test]
fn request_content_requires_an_explicit_section_marker() {
    for (service, source, required) in [
        (
            "mysql",
            "#endpoint\nmysql://localhost/db\nSELECT 1",
            "#query",
        ),
        ("grpc", "#endpoint\ngrpc://a/b/c\n{}", "#message"),
        ("graphql", "#endpoint\nhttps://a\nquery { a }", "#query"),
    ] {
        assert!(parse(service, source).unwrap_err().contains(required));
    }
}

#[test]
fn every_service_console_parses_into_its_request_shape() {
    assert_eq!(
        parse(
            "rest",
            "#headers\nX: y\n#body\n{}\n#endpoint\nPOST https://a"
        )
        .unwrap(),
        json!({"driver":"http","stream":false,"request":{"method":"POST","url":"https://a",
            "headers":[["X","y"]],"body":"{}","text":true}})
    );
    assert_eq!(
        parse("terminal", "#endpoint\nbash -c\necho hi").unwrap()["command"],
        "echo hi"
    );
    assert_eq!(
        parse(
            "splunk",
            "#endpoint\nhttps://a\n#options\nFrom: -1h\nCount: 4\n#search\nindex=main"
        )
        .unwrap()["count"],
        "4"
    );
    assert!(parse("graphql", "#endpoint\nhttps://a\n#variables\nbad").is_err());
}

#[test]
fn console_options_run_the_check_and_never_reach_the_module() {
    let (params, options) = parse_run(
        "rest",
        "#endpoint\nGET https://a\n#options\nrepeat: 2m\nRepeat-Until: Changed",
    )
    .unwrap();
    assert_eq!(params, parse("rest", "#endpoint\nGET https://a").unwrap());
    assert_eq!(
        options,
        RunOptions {
            repeat: Some(120_000),
            until_changed: true,
            format: None
        }
    );
    // A module with its own options keeps them; the console's are taken out first.
    let (params, options) = parse_run(
        "artemis",
        "#endpoint\nhttp://broker:8161\n#action\nSEND /orders\n#options\nRepeat: 2s\nPersistent: false\n#body\n{}",
    )
    .unwrap();
    assert_eq!(params["persistent"], "false");
    assert_eq!(options.repeat, Some(2000));
    let (params, options) = parse_run(
        "mysql",
        "#endpoint\nmysql://fixture/db\n#options\nFormat: JSON\n#query\nselect 1",
    )
    .unwrap();
    assert_eq!(
        params,
        parse("mysql", "#endpoint\nmysql://fixture/db\n#query\nselect 1").unwrap()
    );
    assert_eq!(options.format, Some(Format::Json));
    for (service, options, error) in [
        ("rest", "Repeat-Until: changed", "Repeat-Until needs Repeat"),
        // A time has its unit: s, m or h, 1s to 24h.
        ("rest", "Repeat: 5", "Repeat takes a time from 1s to 24h"),
        ("rest", "Repeat: 25h", "Repeat takes a time from 1s to 24h"),
        ("rest", "Repeat: 1.5s", "Repeat takes a time from 1s to 24h"),
        (
            "rest",
            "Repeat: 5s\nRepeat-Until: never",
            "Repeat-Until takes \"changed\"",
        ),
        (
            "rest",
            "Repeat: 5s\nUntil: changed",
            "Unknown option \"Until\"; REST takes Repeat and Repeat-Until",
        ),
        ("rest", "Format: json", "Format applies to table responses"),
        ("rest", "X-Debug: 1", "Unknown option \"X-Debug\""),
        (
            "mysql",
            "Format: csv",
            "Format takes table, vertical or json",
        ),
    ] {
        let endpoint = if service == "rest" {
            "GET https://a"
        } else {
            "mysql://fixture/db\n#query\nselect 1"
        };
        let source = format!("#endpoint\n{endpoint}\n#options\n{options}");
        let result = parse_run(service, &source).unwrap_err();
        assert!(result.contains(error), "{options}: {result}");
    }
}

#[test]
fn import_export_roundtrip() {
    for (service, command) in [
        (
            "rest",
            "curl -X POST 'https://a' -H 'X: y' -d '{\"a\":\"it is\"}'",
        ),
        (
            "graphql",
            "curl https://a -H 'Content-Type: application/json' -d '{\"query\":\"query { a }\",\"variables\":{\"x\":1}}'",
        ),
        (
            "grpc",
            "grpcurl -plaintext -H 'x: y' -d '{\"a\":1}' localhost:50051 demo.Service.Method",
        ),
    ] {
        let block = import(service, command).unwrap();
        let exported = export(service, &block).unwrap();
        let second = import(service, &exported).unwrap();
        assert_eq!(
            parse(service, &block).unwrap(),
            parse(service, &second).unwrap()
        );
    }
}

#[test]
fn response_replaced_not_stacked() {
    let s = "#check\n#endpoint\nGET https://a\n\n#responce old\nold\n\n#check\n#endpoint\nGET https://b";
    let n = replace_response(s, 2, "OK", "new");
    assert_eq!(n.matches("#responce").count(), 1);
    assert!(n.contains("new"));
    assert!(n.contains("https://b"));
    assert!(!n.contains("old"));
}

#[test]
fn exported_commands_quote_text_so_it_splits_back_unchanged() {
    let value = "it's \"quoted\"";
    let command = export(
        "rest",
        &format!("#endpoint\nGET https://fixture.invalid\n#headers\nX-Note: {value}"),
    )
    .unwrap();
    assert!(
        shell_words(&command)
            .unwrap()
            .contains(&format!("X-Note: {value}"))
    );
    assert!(shell_words("'bad").is_err());
}
