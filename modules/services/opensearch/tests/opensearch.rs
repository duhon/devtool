//! The OpenSearch module as the Kernel drives it: requests, discovery through driver calls
//! and completion of indices, aliases and Query DSL fields.

use serde_json::{Value, json};
use services_module_opensearch::OpenSearch;
use services_sdk::lang::marker;
use services_sdk::{Check, Chunk, Cmd, Completions, Event, Failure, Module, Section};

/// A check as the Kernel builds it: sections split at marker lines.
fn check(text: &str) -> Check {
    let mut sections: Vec<Section> = vec![];
    let mut body: Vec<&str> = vec![];
    let mut current: Option<(String, String, u32)> = Some(("leftover".into(), String::new(), 0));
    let mut close = |current: Option<(String, String, u32)>, body: &mut Vec<&str>| {
        if let Some((name, head, line)) = current {
            let mut text = head;
            if !body.is_empty() {
                text = format!("{text}\n{}", body.join("\n"));
            }
            body.clear();
            let text = text.trim().to_owned();
            if name != "leftover" || !text.is_empty() {
                sections.push(Section { name, text, line });
            }
        }
    };
    for (i, line) in text.split('\n').enumerate() {
        if let Some((name, rest)) = marker(line) {
            close(current.take(), &mut body);
            let head = if name == "target" {
                rest.to_owned()
            } else {
                String::new()
            };
            current = Some((name, head, i as u32));
        } else {
            body.push(line);
        }
    }
    close(current, &mut body);
    Check {
        columns: Vec::new(),
        service: "opensearch".into(),
        text: text.into(),
        sections,
        options: vec![],
    }
}

fn request(module: &OpenSearch, text: &str) -> Result<Value, String> {
    let call = module.build(&check(text))?;
    assert_eq!(call.driver, "http");
    Ok(call.request())
}

fn header<'a>(request: &'a Value, name: &str) -> Vec<&'a str> {
    request["headers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|pair| pair[0].as_str().unwrap().eq_ignore_ascii_case(name))
        .map(|pair| pair[1].as_str().unwrap())
        .collect()
}

fn statuses(cmds: &[Cmd]) -> Vec<String> {
    cmds.iter()
        .filter(|cmd| cmd.name == "status.set")
        .map(|cmd| cmd.args()["text"].as_str().unwrap().to_owned())
        .collect()
}

/// `(tag, url)` of the discovery reads: `GET`s with the check's headers, never redirected.
fn reads(cmds: &[Cmd]) -> Vec<(String, String)> {
    cmds.iter()
        .filter(|cmd| cmd.name == "driver.call")
        .map(|cmd| {
            let args = cmd.args();
            assert_eq!(args["request"]["method"], "GET");
            assert_eq!(args["request"]["redirects"], 0);
            assert_eq!(
                args["request"]["headers"],
                json!([["Authorization", "Basic fixture"]])
            );
            (
                cmd.tag.clone(),
                args["request"]["url"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn response(status: u16, body: &Value) -> Value {
    json!({"status": status, "statusText": "", "body": body.to_string()})
}

fn opensearch() -> Value {
    json!({"version":{"distribution":"opensearch","number":"2.19.0"},"tagline":"The OpenSearch Project"})
}

/// A cluster mounted at `http://example.test/proxy/`; its root answers `info` with `status`.
fn cluster(path: &str, info: &Value, status: u16) -> Value {
    match path {
        "/proxy/" => response(status, info),
        "/proxy/_mapping" => response(
            status,
            &json!({
                "logs": {"mappings":{"properties":{
                    "message":{"type":"text","fields":{"keyword":{"type":"keyword"}}},
                    "status":{"type":"integer"},
                    "created":{"type":"date"}
                }}},
                "metrics": {"mappings":{"properties":{"cpu":{"type":"double"},"status":{"type":"keyword"}}}},
                ".system":{"mappings":{"properties":{"secret":{"type":"keyword"}}}}
            }),
        ),
        "/proxy/_alias" => response(
            status,
            &json!({
                "logs":{"aliases":{"recent":{},"combined":{}}},
                "metrics":{"aliases":{"combined":{}}}
            }),
        ),
        _ => response(404, &json!({"error":"no_such_index"})),
    }
}

/// Hints for `text`, every read answered by `server`: the URLs read and the last commands.
fn discover(
    module: &mut OpenSearch,
    text: &str,
    server: impl Fn(&str) -> Value,
) -> (Vec<String>, Vec<Cmd>) {
    let mut last = module.update(Event::Hints {
        check: check(text),
        pos: text.len() as u32,
    });
    let mut urls = vec![];
    let mut calls = reads(&last);
    while !calls.is_empty() {
        for (tag, url) in std::mem::take(&mut calls) {
            let path = url.strip_prefix("http://example.test").unwrap_or(&url);
            let reply = server(path);
            urls.push(path.to_owned());
            last = module.update(Event::Reply {
                tag,
                result: Ok(reply),
            });
            calls.extend(reads(&last));
        }
    }
    (urls, last)
}

fn source(path: &str) -> String {
    format!(
        "#check test\n#target\nhttp://example.test/proxy/{path}\n#headers\nAuthorization: Basic fixture\n#payload\n"
    )
}

/// A module whose discovery read the fixture cluster.
fn discovered() -> OpenSearch {
    let mut module = OpenSearch::default();
    let (urls, last) = discover(&mut module, &source("logs/_search"), |path| {
        cluster(path, &opensearch(), 200)
    });
    assert_eq!(
        urls,
        [
            "/proxy/logs/",
            "/proxy/",
            "/proxy/_mapping",
            "/proxy/_alias"
        ]
    );
    assert_eq!(statuses(&last), ["Discovery · OK"]);
    module
}

/// Completion at the end of `text`.
fn complete(module: &OpenSearch, text: &str) -> Completions {
    module.complete(&check(text), text.len() as u32)
}

fn inserts(completions: &Completions) -> Vec<String> {
    completions
        .items
        .iter()
        .map(|item| item.insert.clone())
        .collect()
}

/// Body completion for `body` in a request to `path` under the fixture cluster.
fn body(module: &OpenSearch, path: &str, body: &str) -> Vec<String> {
    let text = format!("#target\nhttp://example.test/proxy/{path}\n#payload\n{body}");
    inserts(&complete(module, &text))
}

#[test]
fn target_is_url_action_is_method_and_bodies_pass_unchanged() {
    let module = OpenSearch::default();
    let search = request(
        &module,
        "#target\nhttps://example.test:9200/logs-*/_search?size=5\n#action\nget\n#headers\nAuthorization: Basic eA==\n#payload\n{\"query\":{\"match_all\":{}}}",
    )
    .unwrap();
    assert_eq!(search["method"], "GET");
    assert_eq!(
        search["url"],
        "https://example.test:9200/logs-*/_search?size=5"
    );
    assert_eq!(search["body"], "{\"query\":{\"match_all\":{}}}");
    assert_eq!(header(&search, "Authorization"), ["Basic eA=="]);
    assert_eq!(header(&search, "Content-Type"), ["application/json"]);
    assert_eq!(search["timeoutMs"], 120_000);
    assert_eq!(search["redirects"], 0);
    for endpoint in [
        "",
        "http://localhost:9200/ extra",
        "/_search",
        "file:///tmp/test",
        "http://localhost:9200/#fragment",
    ] {
        let error = request(&module, &format!("#target\n{endpoint}")).unwrap_err();
        assert!(error.contains("#target"), "{endpoint}: {error}");
    }
    let error = request(&module, "#target\nGET http://localhost:9200/").unwrap_err();
    assert!(error.contains("put the method GET in #action"), "{error}");
    let error = request(&module, "#target\nhttp://localhost:9200/\n#action\nTRACE").unwrap_err();
    assert!(error.contains("#action must be one HTTP method"), "{error}");
    let get = request(
        &module,
        "#target\nhttp://localhost:9200/_search\n#payload\n{}",
    )
    .unwrap();
    assert_eq!(get["method"], "GET");
    assert_eq!(get["body"], "{}");
    let empty = request(&module, "#target\nhttp://localhost:9200/\n#payload\n").unwrap();
    assert!(empty["body"].is_null() && empty["headers"] == json!([]));
}

#[test]
fn ndjson_apis_get_their_content_type_and_exactly_one_final_newline() {
    let module = OpenSearch::default();
    for path in [
        "/_bulk?refresh=wait_for",
        "/logs/_bulk",
        "/_msearch",
        "/logs/_msearch/template/",
    ] {
        for body in ["{}\n{}", "{}\n{}\n"] {
            let request = request(
                &module,
                &format!("#target\nhttp://localhost:9200{path}\n#action\nPOST\n#payload\n{body}"),
            )
            .unwrap();
            assert_eq!(request["body"], "{}\n{}\n", "{path}");
            assert_eq!(header(&request, "content-type"), ["application/x-ndjson"]);
        }
    }
    let bulk = request(
        &module,
        "#target\nhttp://localhost:9200/_bulk\n#action\nPOST\n#headers\ncontent-type: application/json\n#payload\n{}",
    )
    .unwrap();
    assert_eq!(header(&bulk, "content-type"), ["application/json"]);
    assert_eq!(bulk["body"], "{}\n");
    let custom = request(
        &module,
        "#target\nhttp://localhost:9200/custom\n#action\nPOST\n#headers\nContent-Type: application/x-ndjson; charset=utf-8\n#payload\n{}",
    )
    .unwrap();
    assert_eq!(custom["body"], "{}\n");
}

#[test]
fn responses_render_like_rest() {
    let module = OpenSearch::default();
    let chunk = |status: u16, body: &str| Chunk {
        call: None,
        response: serde_json::to_vec(&json!({"status": status, "statusText": "", "body": body}))
            .unwrap(),
        index: 0,
        last: true,
    };
    let ok = module.render(&chunk(200, "{\"took\":1}")).unwrap();
    assert_eq!(ok.text, "{\n  \"took\": 1\n}");
    assert_eq!((ok.language.as_str(), ok.outcome.as_str()), ("json", "OK"));
    let missing = module
        .render(&chunk(404, r#"{"error":"no_such_index"}"#))
        .unwrap();
    assert_eq!(missing.outcome, "WARNING");
    assert!(missing.text.contains("no_such_index"));
}

#[test]
fn highlight_styles_options_and_the_ndjson_body() {
    let module = OpenSearch::default();
    let lines: Vec<String> = [
        "#action",
        "OPTIONS",
        "#payload",
        "{\"index\":{}}",
        "{\"a\":1}",
    ]
    .map(String::from)
    .to_vec();
    let highlighted = module.highlight(&[], &lines, &[]);
    assert_eq!(highlighted[1].tokens[0].kind, "property");
    assert_eq!(highlighted[1].tokens[0].len, "OPTIONS".len() as u32);
    assert!(
        highlighted[4]
            .tokens
            .iter()
            .any(|token| token.kind.contains("property"))
    );
    assert_eq!(
        module
            .fold(&check("#payload\n{\n  \"a\": 1\n}"))
            .iter()
            .map(|fold| (fold.start, fold.end))
            .collect::<Vec<_>>(),
        [(1, 3)]
    );
}

#[test]
fn completion_offers_the_methods_in_action_and_valid_urls() {
    let module = OpenSearch::default();
    let methods = inserts(&complete(&module, "#check test\n#action\n"));
    assert_eq!(
        methods,
        ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]
    );
    for endpoint in ["", "h"] {
        let candidates = inserts(&complete(
            &module,
            &format!("#check test\n#target\n{endpoint}"),
        ));
        assert!(!candidates.is_empty(), "{endpoint}");
        for candidate in candidates {
            request(&module, &format!("#target\n{candidate}x"))
                .unwrap_or_else(|error| panic!("{candidate}: {error}"));
        }
    }
}

#[test]
fn discovery_is_cached_at_the_cluster_root_and_service_apis_apart() {
    let mut module = discovered();
    for path in ["recent/_search", "logs/_count", "metrics/_search"] {
        let (urls, last) = discover(&mut module, &source(path), |_| unreachable!());
        assert!(urls.is_empty() && last.is_empty(), "{path}");
    }
    let mut module = OpenSearch::default();
    let (urls, last) = discover(&mut module, &source("_cluster/health"), |path| {
        cluster(path, &opensearch(), 200)
    });
    assert_eq!(urls, ["/proxy/"]);
    assert_eq!(statuses(&last), ["Discovery · OK"]);
    let paths = inserts(&complete(
        &module,
        "#target\nhttp://example.test/proxy/_cluster/he",
    ));
    assert!(
        paths.contains(&"/proxy/_cluster/health".to_owned()),
        "{paths:?}"
    );
    // The paths of a service API do not stand in for the index schema.
    let (urls, _) = discover(&mut module, &source("logs/_search"), |path| {
        cluster(path, &opensearch(), 200)
    });
    assert!(urls.contains(&"/proxy/_mapping".to_owned()), "{urls:?}");
}

#[test]
fn discovery_reports_refused_metadata_transport_failures_and_other_servers() {
    let mut module = OpenSearch::default();
    let (_, last) = discover(&mut module, &source("logs/_search"), |path| {
        cluster(path, &opensearch(), 403)
    });
    assert_eq!(
        statuses(&last),
        [
            "Discovery · ERROR · OpenSearch discovery failed (HTTP 403); check the target and metadata permissions"
        ]
    );
    let elasticsearch = json!({"version":{"number":"8.0.0"},"tagline":"You Know, for Search"});
    let mut module = OpenSearch::default();
    let (urls, last) = discover(&mut module, &source("logs/_search"), |_| {
        response(200, &elasticsearch)
    });
    assert_eq!(urls, ["/proxy/logs/", "/proxy/", "/"]);
    assert_eq!(
        statuses(&last),
        ["Discovery · ERROR · No OpenSearch cluster found at this target"]
    );
    let mut module = OpenSearch::default();
    let cmds = module.update(Event::Hints {
        check: check(&source("logs/_search")),
        pos: 0,
    });
    let (tag, _) = reads(&cmds).remove(0);
    let failure = Failure {
        message: "connection refused".into(),
        details: serde_json::to_vec(&json!({"failure": {"stage": "connect"}})).unwrap(),
    };
    let last = module.update(Event::Reply {
        tag,
        result: Err(failure),
    });
    assert_eq!(statuses(&last), ["Discovery · ERROR · connection refused"]);
}

#[test]
fn paths_complete_with_the_indices_and_aliases() {
    let module = discovered();
    let paths = inserts(&complete(
        &module,
        "#check test\n#target\nhttp://example.test/proxy/re",
    ));
    assert!(
        paths.contains(&"/proxy/recent/_search".to_owned()),
        "{paths:?}"
    );
    assert!(!paths.iter().any(|path| path.starts_with("/proxy/.")));
}

#[test]
fn query_fields_follow_the_index_or_alias_in_the_url() {
    let module = discovered();
    for (path, expected, excluded) in [
        ("logs", "message", Some("cpu")),
        ("recent", "message", Some("cpu")),
        ("metrics", "cpu", Some("message")),
        ("combined", "cpu", None),
        ("*", "cpu", None),
        ("_all", "cpu", None),
        ("logs,metrics", "cpu", None),
    ] {
        let fields = body(
            &module,
            &format!("{path}/_search"),
            "{\"query\":{\"match\":{\"",
        );
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
        &module,
        "logs/_search",
        "{\n  \"query\": {\n    \"match\": {\n      \"",
    );
    assert!(
        fields
            .iter()
            .any(|text| text.starts_with("\"message.keyword\": \"")),
        "{fields:?}"
    );
    let values = body(
        &module,
        "logs/_search",
        "{\n  \"query\": {\n    \"exists\": {\n      \"field\": \"mess",
    );
    assert!(
        values.iter().any(|text| text.contains("message.keyword")),
        "{values:?}"
    );
}

#[test]
fn bool_clauses_offer_the_leaf_queries_in_every_item() {
    let module = discovered();
    for clause in ["must", "filter", "should", "must_not"] {
        for previous in ["", r#"{"match_all": {}}, "#] {
            let prefix = format!("{{\"query\":{{\"bool\":{{\"{clause}\":[{previous}{{");
            for (tail, expected) in [
                ("\"", "\"match\":"),
                ("\"match\":{\"", "\"message.keyword\":"),
                ("\"term\":{\"", "\"status\":"),
                ("\"range\":{\"", "\"created\":"),
                ("\"range\":{\"status\":{\"", "\"gte\":"),
            ] {
                let fields = body(&module, "logs/_search", &format!("{prefix}{tail}"));
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
fn header_hints_lead_with_the_module_ones_and_skip_used_names() {
    let module = discovered();
    let endpoint = "#check test\n#target\nhttp://example.test/proxy/logs/_search\n#headers\n";
    assert_eq!(
        inserts(&complete(&module, endpoint)),
        [
            "Authorization: ",
            "Accept: application/json",
            "Content-Type: application/json",
            "Content-Type: application/x-ndjson",
        ]
    );
    let typed = format!("{endpoint}content-type: application/json\n");
    let headers = inserts(&complete(&module, &typed));
    assert!(!headers.iter().any(|text| text.starts_with("Content-Type:")));
}
