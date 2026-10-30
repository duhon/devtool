//! When discovery runs: connection sections, complete addresses and resolved
//! variables. UI-behaviour tests.

use serde_json::{Value, json};
use services_ui::discovery::{connection_position, key, params};
use std::collections::BTreeMap;

#[test]
fn discovery_commits_belong_to_connection_sections_not_body_input() {
    let source = "#check c\n#endpoint\nhttps://fixture.invalid/graphql\n#headers\nAuthorization: token\n#query\n{\n  field\n}";
    for line in 0..source.lines().count() {
        assert_eq!(
            connection_position("graphql", "", source, line),
            (1..=4).contains(&line),
            "{line}"
        );
    }
    assert!(connection_position("graphql", "endpoint", "", 0));
    assert!(connection_position("graphql", "headers", "", 0));
    assert!(connection_position("grpc", "endpoint", "", 0));
    assert!(connection_position("mysql", "endpoint", "", 0));
    assert!(!connection_position("graphql", "query", "", 0));
    assert!(!connection_position("grpc", "message", "", 0));
    assert!(!connection_position("rest", "body", "", 0));
    assert!(!connection_position("terminal", "command", "", 0));
}

#[test]
fn discovery_params_come_from_the_connection_sections() {
    let sections = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, text)| (name.to_string(), text.to_string()))
            .collect()
    };
    let headers = "Authorization: token";
    for (service, endpoint, expected) in [
        (
            "rest",
            "POST https://fixture.invalid/v1?x=1",
            json!({"url":"https://fixture.invalid/v1?x=1","headers":{"Authorization":"token"}}),
        ),
        (
            "graphql",
            "https://fixture.invalid/graphql",
            json!({"url":"https://fixture.invalid/graphql","headers":{"Authorization":"token"}}),
        ),
        (
            "grpc",
            "grpc://fixture.invalid:50051",
            json!({"endpoint":"grpc://fixture.invalid:50051","headers":{"Authorization":"token"}}),
        ),
        (
            "mysql",
            "mysql://user@fixture.invalid/db",
            json!({"connectionString":"mysql://user@fixture.invalid/db"}),
        ),
    ] {
        let all = sections(&[
            ("endpoint", endpoint),
            ("headers", headers),
            ("body", "{}"),
            ("query", "q"),
        ]);
        assert_eq!(params(service, &all), Some(expected), "{service}");
    }
    assert_eq!(
        params("graphql", &sections(&[])),
        Some(json!({"url":"","headers":{}}))
    );
    assert_eq!(
        params("rest", &sections(&[("endpoint", "GET")])).map(|params| params["url"].clone()),
        Some(json!(""))
    );
    for service in ["terminal", "splunk", "unknown"] {
        assert_eq!(
            params(service, &sections(&[("endpoint", "x")])),
            None,
            "{service}"
        );
    }
}

#[test]
fn incomplete_addresses_are_not_ready_for_discovery() {
    for service in ["rest", "graphql", "grpc", "mysql"] {
        let field = match service {
            "grpc" => "endpoint",
            "mysql" => "connectionString",
            _ => "url",
        };
        let valid: &[&str] = match service {
            "grpc" => &[
                "localhost:50051/",
                "grpc://localhost:50051/Service/",
                "grpcs://localhost:443/Service/Method",
                "[::1]:50051/Service/Method",
            ],
            "mysql" => &[
                "mysql://localhost/database",
                "mysql://user:password@localhost:3306/database",
            ],
            _ => &[
                "https://fixture.invalid/graphql",
                "HTTPS://fixture.invalid/graphql",
                "http://localhost:3000/",
                "http://[::1]:3000/",
            ],
        };
        for address in valid {
            assert!(
                key(service, &json!({field:address}), &Value::Null).is_ok(),
                "{service}: {address}"
            );
        }
        for address in [
            "",
            "https://",
            "http://",
            "grpc://",
            "grpcs://",
            "mysql://",
            "https:///",
            "https://host:",
            "https://host:abc",
            "https://bad host/",
            "http:/host",
            "hhttp://fake.com",
            "{{:unfinished",
            "https://{{:unknown}}/",
        ] {
            assert!(
                key(service, &json!({field:address}), &Value::Null).is_err(),
                "{service}: {address}"
            );
        }
    }
}

#[test]
fn discovery_keys_follow_resolved_connection_variables() {
    let mut variables = json!({"order":["catalog"],"profiles":{"catalog":{"HOST":"fixture.invalid","TOKEN":"first"}}});
    let params =
        json!({"url":"https://{{catalog:HOST}}/graphql","headers":{"X-Token":"{{:TOKEN}}"}});
    let first = key("graphql", &params, &variables).unwrap();
    assert_eq!(
        first,
        json!({"url":"https://fixture.invalid/graphql","headers":{"X-Token":"first"}})
    );
    assert_eq!(
        key(
            "graphql",
            &json!({"url":" https://{{catalog:HOST}}/graphql ","headers":{"X-Token":"{{:TOKEN}}"}}),
            &variables
        )
        .unwrap(),
        first
    );
    variables["profiles"]["catalog"]["TOKEN"] = json!("second");
    assert_ne!(key("graphql", &params, &variables).unwrap(), first);
    variables["profiles"]["catalog"]["HOST"] = json!("");
    assert!(key("graphql", &params, &variables).is_err());
    assert!(
        key(
            "graphql",
            &json!({"url":"https://fixture.invalid","headers":{"X-Token":"{{:unfinished"}}),
            &variables
        )
        .is_err()
    );
    let variables = json!({"order":["catalog"],"profiles":{"catalog":{"URL":"{{other:URL}}"}},"resolvedProfiles":{"catalog":{"URL":"https://fixture.invalid/graphql"}}});
    assert!(key("graphql", &json!({"url":"{{:URL}}"}), &variables).is_ok());
}
