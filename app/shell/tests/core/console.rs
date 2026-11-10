//! Console document parsing shared by every service: markers, hints, import/
//! export and response replacement. UI-behaviour tests.

use serde_json::json;
use services_kernel::requests::{RunOptions, export, import, parse, parse_run};
use services_sdk::lang::curl::shell_words;

#[test]
fn every_service_console_parses_into_its_request_shape() {
    services_shell::link();
    assert_eq!(
        parse(
            "rest",
            "#headers\nX: y\n#payload\n{}\n#target\nhttps://a\n#action\nPOST"
        )
        .unwrap(),
        json!({"driver":"http","stream":false,"request":{"method":"POST","url":"https://a",
            "headers":[["X","y"]],"body":"{}","text":true}})
    );
    // A module that runs its checks itself gets the check text; its endpoint shows in
    // approvals.
    let splunk = "#target\nhttps://a\n#options\nFrom: -1h\nCount: 4\n#payload\nindex=main";
    assert_eq!(
        parse("splunk", splunk).unwrap(),
        json!({"check": splunk, "target": "https://a"})
    );
}

#[test]
fn console_options_run_the_check_and_never_reach_the_module() {
    services_shell::link();
    let (params, options) = parse_run(
        "rest",
        "#target\nhttps://a\n#options\nrepeat: 2m\nRepeat-Until: Changed",
    )
    .unwrap();
    assert_eq!(params, parse("rest", "#target\nhttps://a").unwrap());
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
        "#target\nhttp://broker:8161\n#action\nSEND /orders\n#options\nRepeat: 2s\nPersistent: false\n#payload\n{}",
    )
    .unwrap();
    assert_eq!(
        params["check"],
        "#target\nhttp://broker:8161\n#action\nSEND /orders\n#options\nPersistent: false\n#payload\n{}"
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
        let source = format!("#target\nhttps://a\n#options\n{options}");
        let result = parse_run(service, &source).unwrap_err();
        assert!(result.contains(error), "{options}: {result}");
    }
}

#[test]
fn import_export_roundtrip() {
    services_shell::link();
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
    services_shell::link();
    let value = "it's \"quoted\"";
    let command = export(
        "rest",
        &format!("#target\nhttps://fixture.invalid\n#headers\nX-Note: {value}"),
    )
    .unwrap();
    assert!(
        shell_words(&command)
            .unwrap()
            .contains(&format!("X-Note: {value}"))
    );
    assert!(shell_words("'bad").is_err());
}
