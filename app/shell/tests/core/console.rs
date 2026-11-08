//! Console document parsing shared by every service: markers, hints, import/
//! export and response replacement. UI-behaviour tests.

use serde_json::json;
use services_sdk::lang::curl::shell_words;
use services_shell::console::{RunOptions, export, import, parse, parse_run};

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
    // A module that runs its checks itself gets the check text; its endpoint shows in
    // approvals.
    let splunk = "#endpoint\nhttps://a\n#options\nFrom: -1h\nCount: 4\n#search\nindex=main";
    assert_eq!(
        parse("splunk", splunk).unwrap(),
        json!({"check": splunk, "endpoint": "https://a"})
    );
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
        }
    );
    // A module with its own options keeps them; the console's are taken out first.
    let (params, options) = parse_run(
        "artemis",
        "#endpoint\nhttp://broker:8161\n#action\nSEND /orders\n#options\nRepeat: 2s\nPersistent: false\n#body\n{}",
    )
    .unwrap();
    assert_eq!(
        params["check"],
        "#endpoint\nhttp://broker:8161\n#action\nSEND /orders\n#options\nPersistent: false\n#body\n{}"
    );
    assert_eq!(options.repeat, Some(2000));
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
        ("rest", "X-Debug: 1", "Unknown option \"X-Debug\""),
    ] {
        let source = format!("#endpoint\nGET https://a\n#options\n{options}");
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
