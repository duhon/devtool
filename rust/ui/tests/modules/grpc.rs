//! gRPC module: service/method discovery, message field completion and
//! grpcurl import. UI-behaviour tests.

use serde_json::json;
use services_ui::completion::{discovery_endpoint, suggest};
use services_ui::console::import;

#[test]
fn nested_objects_offer_their_own_fields_not_ancestors() {
    let d = json!({"fields":[{"name":"address","kind":"object","fields":[{"name":"city","kind":"string"}]},{"name":"other","kind":"string"}]});
    let r = suggest("grpc", "message", "{\"address\": {\n  ", 1, 2, &[], &d);
    assert_eq!(r.candidates[0].text, "\"city\": \"\"");
    assert_eq!(r.candidates.len(), 1);
}

#[test]
fn parent_fields_after_a_closed_object_offer_the_missing_comma() {
    let d = json!({"fields":[
        {"name":"products_by_store_view","kind":"array","items":{"kind":"object","fields":[
            {"name":"store_view_id","kind":"object","fields":[{"name":"store_code","kind":"string"}]},
            {"name":"skus","kind":"array","items":{"kind":"string"}}
        ]}}
    ]});
    for (field, prefix) in [("message", ""), ("", "#check comma\n#message\n")] {
        let body = "{\n  \"products_by_store_view\": [{\n    \"store_view_id\": {\n      \"store_code\": \"{{:store_code}}\"\n    }\n    ";
        let source = format!("{prefix}{body}\n  }}]\n}}");
        let line = prefix.lines().count() + 5;
        let result = suggest("grpc", field, &source, line, 4, &[], &d);
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            ["\"skus\": []"]
        );
        assert_eq!(result.comma, Some((line - 1, 5)));
        let source =
            format!("{prefix}{{\"products_by_store_view\":[{{\"store_view_id\":{{}},\n    ");
        let line = prefix.lines().count() + 1;
        let result = suggest("grpc", field, &source, line, 4, &[], &d);
        assert_eq!(result.candidates[0].text, "\"skus\": []");
        assert_eq!(result.comma, None);
        for body in [
            "{\"products_by_store_view\": []}\n  ",
            "{\"products_by_store_view\": [{\"store_view_id\": \n  ",
        ] {
            let source = format!("{prefix}{body}");
            let line = source.lines().count() - 1;
            let result = suggest("grpc", field, &source, line, 2, &[], &d);
            assert!(result.candidates.is_empty(), "{source}");
            assert_eq!(result.comma, None);
        }
    }
}

#[test]
fn array_items_follow_the_schema_and_isolate_each_objects_fields() {
    let d = json!({"fields":[
        {"name":"products_by_store_view","kind":"array","items":{"kind":"object","fields":[
            {"name":"store_view_code","kind":"string"},
            {"name":"products","kind":"array","items":{"kind":"object","fields":[
                {"name":"sku","kind":"string"},
                {"name":"tags","kind":"array","items":{"kind":"string"}}
            ]}}
        ]}},
        {"name":"root","kind":"string"},
        {"name":"tags","kind":"array","items":{"kind":"string"}},
        {"name":"flags","kind":"array","items":{"kind":"boolean"}},
        {"name":"ids","kind":"array","items":{"kind":"number"}},
        {"name":"unknown","kind":"array"}
    ]});
    let cases: &[(&str, &[&str])] = &[
        (
            "{\n  \"products_by_store_view\": [\n    |",
            &["{\n      \n    }"],
        ),
        ("{\"products_by_store_view\": [|]}", &["{\n  \n}"]),
        (
            "{\"products_by_store_view\": [{\n  |",
            &["\"store_view_code\": \"\"", "\"products\": []"],
        ),
        (
            "{\"products_by_store_view\": [{\"store_view_code\": \"en\"\n  |",
            &["\"products\": []"],
        ),
        (
            "{\"products_by_store_view\": [{\"store_view_code\": \"en\"}, {\n  |",
            &["\"store_view_code\": \"\"", "\"products\": []"],
        ),
        (
            "{\"products_by_store_view\": [{\"products\": [{\n  |",
            &["\"sku\": \"\"", "\"tags\": []"],
        ),
        (
            "{\"products_by_store_view\": [{\"products\": [{\"tags\": [|",
            &["\"\""],
        ),
        (
            "{\"products_by_store_view\": [{\"products\": [{\"sku\":\"x\"}],\n  |",
            &["\"store_view_code\": \"\""],
        ),
        (
            "{\"products_by_store_view\": [{\"store_view_code\":\"😀\"}],\n  ro|",
            &["\"root\": \"\""],
        ),
        ("{\"tags\": [|", &["\"\""]),
        (
            "{\"tags\": [\"comma, brace } escape \\\" 😀\", |",
            &["\"\""],
        ),
        ("{\"flags\": [|", &["true", "false"]),
        ("{\"ids\": [|", &["0"]),
        ("{\"unknown\": [|", &[]),
        ("{\"unknown\": [{\n  |", &[]),
        ("{\"products_by_store_view\": [|{}]}", &[]),
        (
            "{\"products_by_store_view\": [{\"store_view_code\": \"|",
            &[],
        ),
        ("{\"products_by_store_view\": [\"unfinished, |", &[]),
        ("{\"products_by_store_view\": [tru|", &[]),
        ("{\"products_by_store_view\": [}|", &[]),
        ("{\"tags\": []}|", &[]),
        ("{\"tags\": []} {|", &[]),
    ];
    for (field, prefix) in [
        ("message", ""),
        (
            "",
            "#check array\n#endpoint\ngrpcs://{{catalog_domain:cds_paas1}}/ProductService/GetProducts\n#message\n",
        ),
    ] {
        for (body, expected) in cases {
            let (before, after) = body.split_once('|').unwrap();
            let before = format!("{prefix}{before}");
            let source = format!("{before}{after}");
            let line = before.split('\n').count() - 1;
            let ch = before
                .split('\n')
                .next_back()
                .unwrap()
                .encode_utf16()
                .count();
            let result = suggest("grpc", field, &source, line, ch, &[], &d);
            assert_eq!(
                result
                    .candidates
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>(),
                *expected,
                "{field}: {body}"
            );
        }
    }
}

#[test]
fn json_completions_do_not_interrupt_values() {
    let d = json!({"fields":[{"name":"flag","kind":"bare"}]});
    for source in ["{\"value\": ", "{\"value\": 42 f", "{\"value\": {}}\n"] {
        let line = source.split('\n').count() - 1;
        let ch = source.split('\n').next_back().unwrap().len();
        assert!(
            suggest("grpc", "message", source, line, ch, &[], &d)
                .candidates
                .is_empty(),
            "{source}"
        );
    }
}

#[test]
fn discovery_triggers_only_in_endpoint_contexts_with_an_authority() {
    for (field, source, expected) in [
        (
            "endpoint",
            "grpc://localhost:50051/",
            Some("grpc://localhost:50051/"),
        ),
        (
            "endpoint",
            "localhost:50051/demo.Greeter/",
            Some("localhost:50051/demo.Greeter/"),
        ),
        (
            "endpoint",
            "grpcs://localhost:443/demo.Greeter/Hello",
            Some("grpcs://localhost:443/demo.Greeter/Hello"),
        ),
        ("endpoint", "grpc://", None),
        ("endpoint", "grpcs:///", None),
        ("endpoint", "", None),
        ("message", "grpc://localhost:50051/", None),
        (
            "",
            "#check demo\n#endpoint\n  localhost:50051/",
            Some("localhost:50051/"),
        ),
        (
            "",
            "#check demo\n#ENDPOINT\nlocalhost:50051/demo.Greeter/",
            Some("localhost:50051/demo.Greeter/"),
        ),
        (
            "",
            "#check demo\n#endpoint\nlocalhost:50051/\n#message\nlocalhost:50051/",
            None,
        ),
        (
            "",
            "#check demo\n#endpoint\nlocalhost:50051/\n#responce OK\nlocalhost:50051/",
            None,
        ),
    ] {
        let line = source.split('\n').count() - 1;
        let ch = source.rsplit('\n').next().unwrap().encode_utf16().count();
        assert_eq!(
            discovery_endpoint("grpc", field, source, line, ch).as_deref(),
            expected,
            "{source}"
        );
    }
}

#[test]
fn methods_are_scoped_to_the_discovered_target_and_service() {
    let discovery = json!({
        "discoveryEndpoint":"grpc://localhost:50051/demo.Greeter/",
        "services":["demo.Greeter"],"methods":["Hello"]
    });
    for (source, expected) in [
        ("grpc://localhost:50051/de", vec!["demo.Greeter/"]),
        ("grpc://localhost:50051/demo.Greeter/", vec!["Hello"]),
        ("grpc://localhost:50051/demo.Other/", vec![]),
        ("grpc://other.invalid:50051/de", vec![]),
        ("grpc://other.invalid:50051/demo.Greeter/", vec![]),
    ] {
        let result = suggest("grpc", "endpoint", source, 0, source.len(), &[], &discovery);
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            expected,
            "{source}"
        );
    }
}

#[test]
fn plain_target_completes_services_and_methods() {
    let d = json!({"services":["demo.Greeter"],"methods":["Hello"]});
    let r = suggest("grpc", "endpoint", "localhost:50051/de", 0, 18, &[], &d);
    assert_eq!(r.candidates[0].text, "demo.Greeter/");
    let source = "localhost:50051/demo.Greeter/H";
    let r = suggest("grpc", "endpoint", source, 0, source.len(), &[], &d);
    assert_eq!(r.candidates[0].text, "Hello");
}

#[test]
fn grpcurl_import_on_port_443_uses_tls() {
    assert!(
        import("grpc", "grpcurl a:443 demo.S/M")
            .unwrap()
            .contains("grpcs://")
    );
}
