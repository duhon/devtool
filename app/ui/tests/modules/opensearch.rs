//! OpenSearch module: `METHOD URL` requests, and completion from the module's own discovery
//! of index mappings: paths, Query DSL fields per index or alias, header hints.

use serde_json::{Value, json};
use services_module_opensearch::schema;
use services_ui::completion::suggest;
use services_ui::console::parse;
use services_ui::discovery::{self, LazyCache};
use services_ui::services;
use url::Url;

fn metadata() -> (Value, Value) {
    (
        json!({
            "logs": {"mappings":{"properties":{
                "message":{"type":"text","fields":{"keyword":{"type":"keyword"}}},
                "status":{"type":"integer"},
                "created":{"type":"date"}
            }}},
            "metrics": {"mappings":{"properties":{"cpu":{"type":"double"},"status":{"type":"keyword"}}}}
        }),
        json!({
            "logs":{"aliases":{"recent":{},"combined":{}}},
            "metrics":{"aliases":{"combined":{}}}
        }),
    )
}

/// The discovery of the fixture cluster mounted at `http://example.test/proxy/`.
fn discovered() -> Value {
    let (mappings, aliases) = metadata();
    schema::indices(
        &Url::parse("http://example.test/proxy/").unwrap(),
        &mappings,
        &aliases,
    )
}

/// The candidates at the end of `source` (`section` "" for the whole console).
fn texts(
    section: &str,
    source: &str,
    hints: &[(String, String)],
    discovery: &Value,
) -> Vec<String> {
    let lines: Vec<_> = source.split('\n').collect();
    let ch = lines.last().unwrap().encode_utf16().count();
    suggest(
        "opensearch",
        section,
        source,
        lines.len() - 1,
        ch,
        hints,
        discovery,
    )
    .candidates
    .into_iter()
    .map(|candidate| candidate.text)
    .collect()
}

/// Body completion for a request to `path` under the fixture cluster.
fn body(path: &str, body: &str) -> Vec<String> {
    let mut discovery = discovered();
    discovery["requestEndpoint"] = json!(format!("GET http://example.test/proxy/{path}"));
    texts("body", body, &[], &discovery)
}

#[test]
fn endpoint_is_method_and_url_and_bodies_pass_unchanged() {
    assert_eq!(
        parse(
            "opensearch",
            "#endpoint\nget https://example.test:9200/logs-*/_search?size=5\n#headers\nAuthorization: Basic eA==\n#body\n{\"query\":{\"match_all\":{}}}"
        )
        .unwrap(),
        json!({
            "method":"GET","url":"https://example.test:9200/logs-*/_search?size=5",
            "headers":{"Authorization":"Basic eA=="},"body":"{\"query\":{\"match_all\":{}}}"
        })
    );
    let bulk = parse(
        "opensearch",
        "#endpoint\nPOST http://localhost:9200/_bulk\n#body\n{}\n{\"field\":1}",
    )
    .unwrap();
    assert_eq!(bulk["body"], "{}\n{\"field\":1}");
    for endpoint in [
        "GET",
        "GET http://localhost:9200/ extra",
        "TRACE http://localhost:9200/",
        "http://localhost:9200/",
        "GET /_search",
        "GET file:///tmp/test",
        "GET http://localhost:9200/#fragment",
    ] {
        let error = parse("opensearch", &format!("#endpoint\n{endpoint}")).unwrap_err();
        assert!(error.contains("METHOD URL"), "{endpoint}: {error}");
    }
}

#[test]
fn endpoint_hints_complete_to_valid_requests() {
    let defaults: Vec<_> = services::get("opensearch")
        .unwrap()
        .console
        .hints
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| (line.to_owned(), "Saved hint".to_owned()))
        .collect();
    for hints in [&[][..], &defaults] {
        for endpoint in ["GET ", "GET h", "POST ", "POST h"] {
            let candidates = texts(
                "",
                &format!("#check test\n#endpoint\n{endpoint}"),
                hints,
                &Value::Null,
            );
            assert!(!candidates.is_empty(), "{endpoint}");
            for candidate in candidates {
                let method = endpoint.split_whitespace().next().unwrap();
                let params = parse("opensearch", &format!("#endpoint\n{method} {candidate}"))
                    .unwrap_or_else(|error| panic!("{candidate}: {error}"));
                assert_eq!(params["method"], method);
                assert!(params["url"].as_str().unwrap().starts_with("http://"));
            }
        }
    }
}

#[test]
fn discovery_is_cached_at_the_cluster_root_and_service_apis_apart() {
    let source = "#check test\n#endpoint\nGET http://example.test/proxy/logs/_search\n#headers\nX-Test: discovery\n#body\n{}";
    let params = discovery::params(
        "opensearch",
        &services_ui::console::sections("opensearch", source),
    )
    .unwrap();
    assert_eq!(params["url"], "http://example.test/proxy/logs/_search");
    assert_eq!(params["headers"]["X-Test"], "discovery");
    let mut cache = LazyCache::default();
    cache.insert("http://example.test/proxy/logs/_search", discovered());
    for url in [
        "http://example.test/proxy/recent/_search",
        "http://example.test/proxy/logs/_count",
        "http://example.test/proxy/metrics/_search",
    ] {
        assert!(cache.get(url).is_some(), "{url}");
    }
    assert!(
        cache
            .get("http://example.test/other/logs/_search")
            .is_none()
    );
    let base = Url::parse("http://example.test/proxy/").unwrap();
    let mut cache = LazyCache::default();
    let health = "http://example.test/proxy/_cluster/health";
    let scope = Url::parse("http://example.test/proxy/_cluster").unwrap();
    cache.insert(health, schema::services(&base, &scope));
    assert!(cache.get(health).is_some());
    assert!(
        cache
            .get("http://example.test/proxy/logs/_search")
            .is_none()
    );
}

#[test]
fn paths_complete_with_the_indices_and_aliases() {
    let source = "#check test\n#endpoint\nGET http://example.test/proxy/re";
    let paths = texts("", source, &[], &discovered());
    assert!(
        paths.contains(&"/proxy/recent/_search".to_owned()),
        "{paths:?}"
    );
}

#[test]
fn query_fields_follow_the_index_or_alias_in_the_url() {
    for (path, expected, excluded) in [
        ("logs", "message", Some("cpu")),
        ("recent", "message", Some("cpu")),
        ("metrics", "cpu", Some("message")),
        ("combined", "cpu", None),
        ("*", "cpu", None),
        ("_all", "cpu", None),
        ("logs,metrics", "cpu", None),
    ] {
        let fields = body(&format!("{path}/_search"), "{\"query\":{\"match\":{\"");
        assert!(
            fields
                .iter()
                .any(|text| text.starts_with(&format!("\"{expected}\":"))),
            "{path}: {fields:?}"
        );
        if let Some(excluded) = excluded {
            assert!(
                !fields
                    .iter()
                    .any(|text| text.starts_with(&format!("\"{excluded}\":"))),
                "{path}"
            );
        }
    }
    let fields = body(
        "logs/_search",
        "{\n  \"query\": {\n    \"match\": {\n      \"",
    );
    assert!(
        fields.contains(&"\"message.keyword\": \"\"".to_owned()),
        "{fields:?}"
    );
    let values = body(
        "logs/_search",
        "{\n  \"query\": {\n    \"exists\": {\n      \"field\": \"mess",
    );
    assert!(
        values.contains(&"\"message.keyword\"".to_owned()),
        "{values:?}"
    );
}

#[test]
fn bool_clauses_offer_the_leaf_queries_in_every_item() {
    for clause in ["must", "filter", "should", "must_not"] {
        for previous in ["", r#"{"match_all": {}}, "#] {
            let prefix = format!("{{\"query\":{{\"bool\":{{\"{clause}\":[{previous}{{");
            for (tail, expected) in [
                ("\"", "\"match\":"),
                ("\"match\":{\"", "\"message.keyword\":"),
                ("\"term\":{\"", "\"status\":"),
                ("\"range\":{\"", "\"created\":"),
                ("\"range\":{\"status\":{\"", "\"gte\":"),
                ("\"exists\":{\"field\":\"mess", "\"message.keyword\""),
            ] {
                let fields = body("logs/_search", &format!("{prefix}{tail}"));
                assert!(
                    fields.iter().any(|text| text.starts_with(expected)),
                    "{prefix}{tail}: {fields:?}"
                );
                assert!(!fields.iter().any(|text| text.contains("cpu")));
            }
        }
    }
}

#[test]
fn header_hints_lead_with_the_module_ones_without_saved_bodies() {
    let saved = [
        (r#"{"query":{"match_all":{}}}"#.into(), "Saved hint".into()),
        ("X-Unrelated: saved".into(), "Saved hint".into()),
    ];
    let endpoint = "#check test\n#endpoint\nGET http://example.test/proxy/logs/_search\n#headers\n";
    let headers: Vec<_> = texts("", endpoint, &saved, &discovered())
        .into_iter()
        .filter(|text| !text.starts_with('#'))
        .collect();
    assert_eq!(
        headers,
        [
            "Authorization: ",
            "Accept: application/json",
            "Content-Type: application/json",
            "Content-Type: application/x-ndjson",
            "X-Unrelated: saved"
        ]
    );
    let typed = format!("{endpoint}content-type: application/json\n");
    let headers = texts("", &typed, &saved, &discovered());
    assert!(!headers.iter().any(|text| text.starts_with("Content-Type:")));
}
