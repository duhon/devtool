//! Service-agnostic Console completion: markers, separators, variables,
//! headers, JSON snippets and UTF-16 offsets. UI-behaviour tests.

use serde_json::{Value, json};
use services_ui::completion::{prepare_insertion, suggest};
use services_ui::console::{self, fuzzy_match};
use services_ui::services;

#[test]
fn fuzzy_matching_accepts_ordered_subsequences_only() {
    assert!(fuzzy_match("Content-Type", "cty"));
    assert!(!fuzzy_match("Content-Type", "xyz"));
}

#[test]
fn empty_console_is_safe_and_offers_request() {
    assert!(console::request_at("", 0).is_none());
    for service in services::ids().iter().map(String::as_str) {
        for source in ["", "#"] {
            let hints = suggest(service, "", source, 0, source.len(), &[], &Value::Null);
            assert_eq!(
                hints
                    .candidates
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["#check"],
                "{service}: {source}"
            );
        }
    }
}

#[test]
fn newline_after_request_header_offers_unused_sections() {
    for service in services::ids().iter().map(String::as_str) {
        for source in ["#check\n", "#check my query\n", "#check my query\n\n"] {
            let hints = suggest(
                service,
                "",
                source,
                source.split('\n').count() - 1,
                0,
                &[],
                &Value::Null,
            );
            assert_eq!(
                hints
                    .candidates
                    .iter()
                    .map(|candidate| candidate.text.clone())
                    .collect::<Vec<_>>(),
                console::markers(service)
                    .iter()
                    .map(String::as_str)
                    .chain(std::iter::once("check"))
                    .map(|marker| format!("#{marker}"))
                    .collect::<Vec<_>>(),
                "{service}: {source}"
            );
        }
    }
    let source = "#check my query\n\n#endpoint\nmysql://db\n";
    let hints = suggest("mysql", "", source, 1, 0, &[], &Value::Null);
    assert_eq!(
        hints
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>(),
        vec!["#options", "#query", "#check"]
    );
}

#[test]
fn non_service_editors_never_offer_console_or_service_syntax() {
    let hints = vec![
        ("#check".into(), String::new()),
        ("Accept: */*".into(), String::new()),
        ("GET".into(), String::new()),
        ("SELECT".into(), String::new()),
        ("query".into(), String::new()),
        ("{{prod:TOKEN}}".into(), "fixture".into()),
    ];
    for (service, field) in [("vars", "variables"), ("", "")] {
        for source in [
            "",
            "#",
            "Ac",
            "GET",
            "SEL",
            "qu",
            "NAME: ",
            "#check example\n",
            "#check example\n#headers\n",
        ] {
            let result = suggest(
                service,
                field,
                source,
                source.split('\n').count() - 1,
                source
                    .split('\n')
                    .next_back()
                    .unwrap()
                    .encode_utf16()
                    .count(),
                &hints,
                &Value::Null,
            );
            assert!(result.candidates.is_empty(), "{service}: {source}");
        }
        let result = suggest(
            service,
            field,
            "NAME: {{prod:TO",
            0,
            15,
            &hints,
            &Value::Null,
        );
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].text, "{{prod:TOKEN}}");
    }
}

#[test]
fn marker_hints_offer_only_unused_sections_and_new_checks() {
    let source = "#check\n#endpoint\nmysql://db\n#";
    let r = suggest("mysql", "", source, 3, 1, &[], &Value::Null);
    assert!(r.candidates.iter().any(|c| c.text == "#query"));
    assert!(!r.candidates.iter().any(|c| c.text == "#endpoint"));
    assert!(r.candidates.iter().any(|c| c.text == "#check"));
}

#[test]
fn separator_hints_are_repeatable_and_scoped_to_each_request() {
    for service in services::ids().iter().map(String::as_str) {
        let filled = console::markers(service)
            .iter()
            .map(|marker| format!("#{marker}\nvalue\n"))
            .collect::<String>();
        let first = format!("#check first\n{filled}");
        let source = format!("{first}#");
        let line = source.split('\n').count() - 1;
        let result = suggest(service, "", &source, line, 1, &[], &Value::Null);
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["#check"],
            "{service}"
        );
        let source = format!("{first}#check second\n#\n#check third\n{filled}");
        let line = first.split('\n').count();
        let result = suggest(service, "", &source, line, 1, &[], &Value::Null);
        let expected = console::markers(service)
            .iter()
            .map(String::as_str)
            .chain(std::iter::once("check"))
            .map(|marker| format!("#{marker}"))
            .collect::<Vec<_>>();
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| candidate.text.clone())
                .collect::<Vec<_>>(),
            expected,
            "{service}"
        );
        let source = format!("{first}#check second\n#endpoint\nvalue\n#\n#check third\n{filled}");
        let line = first.split('\n').count() + 2;
        let result = suggest(service, "", &source, line, 1, &[], &Value::Null);
        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "#check")
        );
        assert!(
            !result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "#endpoint")
        );
        assert_eq!(result.candidates.len(), console::markers(service).len());
    }
}

#[test]
fn markers_advance_to_their_value_but_request_leaves_room_for_a_title() {
    assert_eq!(
        prepare_insertion("rest", "#check", true),
        ("#check ".into(), None)
    );
    for service in services::ids().iter().map(String::as_str) {
        for marker in console::markers(service) {
            let marker = format!("#{marker}");
            assert_eq!(
                prepare_insertion("rest", &marker, true),
                (format!("{marker}\n"), None)
            );
        }
    }
    assert_eq!(
        prepare_insertion("rest", "#endpoint", false),
        ("#endpoint".into(), None)
    );
}

#[test]
fn completed_candidates_are_not_reoffered() {
    let cases = [
        (
            "graphql",
            "query",
            "{\n  name",
            json!({"queryType":"Query","types":{"Query":{"fields":[{"name":"name","type":"String"}]}}}),
            vec![],
        ),
        (
            "grpc",
            "endpoint",
            "grpc://localhost:50051/demo.Greeter/Hello",
            json!({"methods":["Hello"]}),
            vec![],
        ),
        (
            "mysql",
            "query",
            "SELECT * FROM users u WHERE u.email",
            json!({"tables":[{"name":"users","columns":[{"name":"email"}]}]}),
            vec![],
        ),
        (
            "rest",
            "headers",
            "X-Trace: fixture",
            Value::Null,
            vec![("X-Trace: fixture".into(), "Saved".into())],
        ),
        ("mysql", "query", "select", Value::Null, vec![]),
    ];
    for (service, field, source, discovery, hints) in cases {
        let line = source.split('\n').count() - 1;
        let ch = source
            .split('\n')
            .next_back()
            .unwrap()
            .encode_utf16()
            .count();
        assert!(
            suggest(service, field, source, line, ch, &hints, &discovery)
                .candidates
                .is_empty(),
            "{service}: {source}"
        );
    }
}

#[test]
fn case_sensitive_names_can_still_be_corrected() {
    let d =
        json!({"queryType":"Query","types":{"Query":{"fields":[{"name":"name","type":"String"}]}}});
    let r = suggest("graphql", "query", "{\n  Name", 1, 6, &[], &d);
    assert_eq!(r.candidates[0].text, "name");
    let d = json!({"paths":["/Users"]});
    let source = "https://fixture.invalid/users";
    let r = suggest("rest", "endpoint", source, 0, source.len(), &[], &d);
    assert_eq!(r.candidates[0].text, "/Users");
}

#[test]
fn json_object_snippets_preserve_indent_and_value_cursor_positions() {
    let d = json!({"fields":[{"name":"address","kind":"object"}]});
    let r = suggest("grpc", "message", "{\n    ad", 1, 6, &[], &d);
    assert_eq!(r.candidates[0].text, "\"address\": {\n      \n    }");
    for text in ["\"city\": \"\"", "\"values\": []", "\"имя\": \"\""] {
        assert_eq!(
            prepare_insertion("grpc", text, false),
            (text.into(), Some(text.len() - 1))
        );
    }
    for service in ["grpc", "graphql"] {
        let field = if service == "grpc" {
            "message"
        } else {
            "query"
        };
        let r = suggest(service, field, "    ", 0, 4, &[], &Value::Null);
        let index = if service == "graphql" {
            assert_eq!(
                r.candidates[..3]
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>(),
                ["query", "mutation", "subscription"]
            );
            assert_eq!(r.candidates[3].text, "fragment");
            4
        } else {
            0
        };
        assert_eq!(r.candidates[index].text, "{\n      \n    }");
    }
}

#[test]
fn variable_placeholders_replace_the_existing_closing_braces() {
    let r = suggest(
        "rest",
        "body",
        "{{:TOK}}",
        0,
        6,
        &[("{{:TOKEN}}".into(), "value".into())],
        &Value::Null,
    );
    assert_eq!(r.to, 8);
    assert_eq!(r.from, 0);
    assert_eq!(r.candidates[0].text, "{{:TOKEN}}");
}

#[test]
fn profiles_then_scoped_variables() {
    let hints = vec![
        ("{{prod:TOKEN}}".into(), "secret".into()),
        ("{{other:HOST}}".into(), "host".into()),
        ("{{:TOKEN}}".into(), "global".into()),
    ];
    for (service, service_field) in [
        ("mysql", "query"),
        ("rest", "endpoint"),
        ("graphql", "query"),
        ("grpc", "message"),
        ("splunk", "search"),
        ("terminal", "command"),
        ("vars", "variables"),
    ] {
        for field in [service_field, ""] {
            let profiles = suggest(service, field, "{{", 0, 2, &hints, &Value::Null);
            assert!(profiles.candidates.iter().any(|c| c.text == "{{prod:"));
            assert!(profiles.candidates.iter().any(|c| c.text == "{{:"));
            assert!(!profiles.candidates.iter().any(|c| c.text.ends_with("}}")));
            let variables = suggest(service, field, "{{prod:TK", 0, 9, &hints, &Value::Null);
            assert_eq!(variables.candidates.len(), 1);
            assert_eq!(variables.candidates[0].text, "{{prod:TOKEN}}");
        }
    }
}

#[test]
fn endpoint_completion_offsets_use_utf16_after_trimming() {
    for (service, field, endpoint, expected, discovery) in [
        (
            "rest",
            "endpoint",
            "\u{2003}https://пример.рф/api/",
            "\u{2003}https://пример.рф/api/pets",
            json!({"paths":["/api/pets"]}),
        ),
        (
            "rest",
            "",
            "GET \u{a0}https://petst",
            "GET \u{a0}https://petstore3.swagger.io/api/v3/",
            Value::Null,
        ),
        (
            "grpc",
            "endpoint",
            "\u{2003}grpc://fixture.invalid/",
            "\u{2003}grpc://fixture.invalid/fixture.Echo/",
            json!({"services":["fixture.Echo"]}),
        ),
    ] {
        let hints = vec![("https://petstore3.swagger.io/api/v3/".into(), String::new())];
        let source = if field.is_empty() {
            format!("#endpoint\n{endpoint}")
        } else {
            endpoint.into()
        };
        let result = suggest(
            service,
            field,
            &source,
            usize::from(field.is_empty()),
            endpoint.encode_utf16().count(),
            &hints,
            &discovery,
        );
        assert_eq!(result.candidates.len(), 1, "{service}: {endpoint}");
        let mut units = endpoint.encode_utf16().collect::<Vec<_>>();
        units.splice(
            result.from..result.to,
            result.candidates[0].text.encode_utf16(),
        );
        assert_eq!(String::from_utf16(&units).unwrap(), expected);
    }
}

#[test]
fn fuzzy_saved_headers_are_isolated_and_used_names_are_hidden() {
    let hints = vec![("X-Correlation-Id: ".into(), "saved REST".into())];
    let r = suggest("rest", "headers", "xcid", 0, 4, &hints, &Value::Null);
    assert_eq!(r.candidates[0].text, "X-Correlation-Id: ");
    let r = suggest("graphql", "headers", "xcid", 0, 4, &[], &Value::Null);
    assert!(r.candidates.is_empty());
    let r = suggest(
        "rest",
        "headers",
        "xcid\nX-Correlation-Id: used",
        0,
        4,
        &hints,
        &Value::Null,
    );
    assert!(r.candidates.is_empty());
}
