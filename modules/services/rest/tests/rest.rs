use prost::Message;
use serde_json::{Value, json};
use services_module_rest::Rest;
use services_sdk::lang::marker;
use services_sdk::wire::{self, answer, event, request};
use services_sdk::{
    Check, Chunk, Cmd, Completions, DriverCall, Event, Failure, Linked, Module, Section,
};

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
        service: "rest".into(),
        text: text.into(),
        sections,
        options: vec![],
    }
}

fn styles(rest: &Rest, state: &[u8], lines: &[&str]) -> Vec<Vec<(String, String)>> {
    let lines: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
    rest.highlight(state, &lines, &[])
        .iter()
        .zip(&lines)
        .map(|(line, text)| {
            line.tokens
                .iter()
                .map(|t| {
                    let start = t.start as usize;
                    (
                        text[start..start + t.len as usize].to_owned(),
                        t.kind.clone(),
                    )
                })
                .collect()
        })
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

/// Completion at the `|` in `text`.
fn complete(rest: &Rest, text: &str) -> (String, Completions) {
    let pos = text.find('|').expect("a cursor");
    let text = text.replacen('|', "", 1);
    let result = rest.complete(&check(&text), pos as u32);
    (text, result)
}

fn labels(c: &Completions) -> Vec<&str> {
    c.items.iter().map(|i| i.label.as_str()).collect()
}

fn inserts(c: &Completions) -> Vec<&str> {
    c.items.iter().map(|i| i.insert.as_str()).collect()
}

fn args(cmd: &Cmd) -> Value {
    cmd.args()
}

fn statuses(cmds: &[Cmd]) -> Vec<String> {
    cmds.iter()
        .filter(|cmd| cmd.name == "status.set")
        .map(|cmd| args(cmd)["text"].as_str().unwrap().to_owned())
        .collect()
}

fn driver_calls(cmds: &[Cmd]) -> Vec<(String, String)> {
    cmds.iter()
        .filter(|cmd| cmd.name == "driver.call")
        .map(|cmd| {
            let args = args(cmd);
            assert_eq!(args["driver"], "http");
            assert_eq!(args["request"]["method"], "GET");
            assert_eq!(args["request"]["timeoutMs"], 5000);
            (
                cmd.tag.clone(),
                args["request"]["url"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// Hints asked at `|` in `text` (at its end without one).
fn hints(rest: &mut Rest, text: &str) -> Vec<Cmd> {
    let pos = text.find('|').unwrap_or(text.len());
    let text = text.replacen('|', "", 1);
    rest.update(Event::Hints {
        check: check(&text),
        pos: pos as u32,
    })
}

/// Answers every discovery call: `found` gets `spec`, the rest 404.
fn answer(rest: &mut Rest, cmds: &[Cmd], found: &str, spec: &Value) -> Vec<Cmd> {
    let mut last = vec![];
    let mut calls = driver_calls(cmds);
    while !calls.is_empty() {
        for (tag, url) in std::mem::take(&mut calls) {
            let reply = if url == found {
                response(200, &spec.to_string())
            } else {
                response(404, "")
            };
            last = rest.update(Event::Reply {
                tag,
                result: Ok(reply),
            });
            calls.extend(driver_calls(&last));
        }
    }
    last
}

fn response(status: u16, body: &str) -> Value {
    json!({"status": status, "statusText": "", "body": body})
}

fn spec() -> Value {
    json!({
        "openapi": "3.0.0",
        "paths": {
            "/api/users": {
                "get": {"parameters": [{"in": "header", "name": "X-Tenant"}]},
                "post": {"requestBody": {"content": {"application/json": {"schema": {
                    "type": "object",
                    "required": ["name", "role"],
                    "properties": {
                        "id": {"type": "integer", "readOnly": true},
                        "name": {"type": "string"},
                        "role": {"type": "string", "enum": ["admin", "user"]},
                        "born": {"type": "string", "format": "date-time"}
                    }
                }}}}}
            },
            "/api/users/{id}": {"delete": {}}
        }
    })
}

/// A Rest whose discovery found `spec()` for `http://svc`.
fn discovered() -> Rest {
    let mut rest = Rest::default();
    let cmds = hints(&mut rest, "#target\nhttp://svc/api/users\n#payload\n");
    let answer = answer(
        &mut rest,
        &cmds,
        "http://svc/api/users/openapi.json",
        &spec(),
    );
    assert_eq!(statuses(&answer), ["Discovery · OK"]);
    let status = answer.iter().find(|cmd| cmd.name == "status.set").unwrap();
    assert_eq!(
        args(status)["detail"]["specUrl"],
        "http://svc/api/users/openapi.json"
    );
    rest
}

#[test]
fn highlight_styles_each_section_and_carries_state() {
    let rest = Rest::default();
    let lines = [
        "#target",
        "https://api.test/users/{id}?q=1",
        "#headers",
        "Accept: */*",
        "#payload",
        "{",
        "  \"name\": \"{{user}}\",",
        "  # note",
        "  \"n\": 1",
        "}",
        "#responce OK",
        "{\"ok\": true}",
    ];
    let s = styles(&rest, &[], &lines);
    let comment = |text: &str| vec![(text.to_owned(), "comment".to_owned())];
    assert_eq!(s[0], comment("#target"));
    assert_eq!(
        s[1],
        pairs(&[
            ("https://", "keyword"),
            ("api.test", "def"),
            ("/users/", "string"),
            ("{id}", "rest-path-param"),
            ("q=1", "number"),
        ])
    );
    assert_eq!(s[2], comment("#headers"));
    assert_eq!(s[3], pairs(&[("Accept", "attribute"), ("*/*", "string")]));
    assert_eq!(s[4], comment("#payload"));
    assert!(s[6].contains(&("\"name\"".into(), "string property".into())));
    assert_eq!(s[7], comment("  # note"));
    assert!(s[8].contains(&("1".into(), "number".into())));
    assert_eq!(s[10], comment("#responce OK"));
    assert!(s[11].contains(&("\"ok\"".into(), "string property".into())));
    assert!(s[11].contains(&("true".into(), "atom".into())));

    // The state of a line continues the highlighting on the next call.
    let all: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let whole = rest.highlight(&[], &all, &[]);
    let split = rest.highlight(&whole[5].state, &all[6..], &[]);
    assert_eq!(split, whole[6..].to_vec());
}

#[test]
fn highlight_starts_from_a_synthetic_marker_line() {
    let rest = Rest::default();
    let start = rest.highlight(&[], &["#payload".into()], &[])[0]
        .state
        .clone();
    let body = &styles(&rest, &start, &["{\"a\": [1]}"])[0];
    assert!(body.contains(&("\"a\"".into(), "string property".into())));
    assert!(body.contains(&("1".into(), "number".into())));
    let start = rest.highlight(&[], &["#headers".into()], &[])[0]
        .state
        .clone();
    assert_eq!(
        styles(&rest, &start, &["X: 1"])[0],
        pairs(&[("X", "attribute"), ("1", "string")])
    );
    // Unknown markers are comments; a plain-text response stays unstyled.
    assert_eq!(
        styles(
            &rest,
            &[],
            &[
                "#options",
                "Repeat: 2",
                "#weird",
                "text",
                "#responce ERROR",
                "boom"
            ]
        ),
        vec![
            pairs(&[("#options", "comment")]),
            pairs(&[("Repeat", "attribute"), ("2", "string")]),
            pairs(&[("#weird", "comment")]),
            vec![],
            pairs(&[("#responce ERROR", "comment")]),
            vec![],
        ]
    );
}

#[test]
fn highlight_marks_the_action_method() {
    let rest = Rest::default();
    let s = styles(&rest, &[], &["#action", "  post", "#target", "post"]);
    assert_eq!(s[1], pairs(&[("post", "property")]));
    assert_ne!(s[3], pairs(&[("post", "property")]));
}

#[test]
fn highlight_is_fast() {
    let rest = Rest::default();
    let mut lines = vec![
        "#target".to_owned(),
        "http://x/{id}".into(),
        "#payload".into(),
    ];
    lines.extend((0..100).map(|i| format!("  \"field{i}\": [1, \"two\", {{\"three\": null}}],")));
    let started = std::time::Instant::now();
    for _ in 0..10 {
        rest.highlight(&[], &lines, &[]);
    }
    assert!(started.elapsed().as_millis() < 50 * if cfg!(debug_assertions) { 10 } else { 1 });
}

#[test]
fn folds_cover_the_body_and_a_json_response() {
    let rest = Rest::default();
    let text = "#target\nhttp://x\n#action\nPOST\n#payload\n{\n  \"a\": [\n    1\n  ]\n}\n#responce OK\n{\n  \"b\": 1\n}";
    let folds: Vec<_> = rest
        .fold(&check(text))
        .iter()
        .map(|f| (f.start, f.end))
        .collect();
    assert_eq!(folds, vec![(5, 9), (6, 8), (11, 13)]);
}

#[test]
fn completion_offers_unused_markers_methods_and_urls() {
    let rest = Rest::default();
    let (_, c) = complete(&rest, "#target\nhttp://x\n#|");
    assert_eq!(labels(&c), ["#action", "#headers", "#payload"]);
    assert_eq!(c.items[0].insert, "#action\n");
    assert_eq!(c.items[0].detail, "Section");

    let (_, c) = complete(&rest, "|");
    assert_eq!(
        inserts(&c),
        ["#target\n", "#action\n", "#headers\n", "#payload\n"]
    );

    let (_, c) = complete(&rest, "#action\nP|");
    assert_eq!(inserts(&c), ["POST", "PUT", "PATCH", "OPTIONS"]);
    assert_eq!(c.items[0].detail, "Method");
    assert_eq!((c.from, c.to), (8, 9));

    let (_, c) = complete(&rest, "#target\nht|");
    assert_eq!(labels(&c), ["http://", "https://"]);
    assert_eq!((c.from, c.to), (8, 10));

    // `{{…}}` variables belong to the App.
    let (_, c) = complete(&rest, "#target\n{{ba|");
    assert!(c.items.is_empty());
    // Nothing inside a response.
    let (_, c) = complete(&rest, "#target\nhttp://x\n#responce OK\n|");
    assert!(c.items.is_empty());
}

#[test]
fn header_completion_skips_used_names() {
    let rest = Rest::default();
    let (_, c) = complete(&rest, "#target\nhttp://x\n#headers\nAccept: */*\n|");
    assert_eq!(&labels(&c)[..2], ["#action", "#payload"]);
    assert!(!labels(&c).contains(&"Accept: */*"));
    assert!(!labels(&c).contains(&"#headers"));

    let (_, c) = complete(&rest, "#target\nhttp://x\n#headers\nx-api|");
    assert_eq!(labels(&c), ["X-Api-Key:"]);
    assert_eq!(c.items[0].detail, "Header");
}

#[test]
fn the_discovered_spec_is_returned_for_its_address() {
    let target = check("#target\nhttp://svc/api/users\n#payload\n");
    assert_eq!(Rest::default().schema(&target), None);
    let rest = discovered();
    let schema = rest.schema(&target).unwrap();
    assert!(schema["paths"].is_array());
    assert_eq!(rest.schema(&check("#target\nhttp://other/x\n")), None);
}

#[test]
fn discovery_feeds_paths_headers_and_body_fields() {
    let mut rest = Rest::default();
    let cmds = hints(&mut rest, "#target\nhttp://svc/api/users?q=1");
    assert_eq!(statuses(&cmds), ["Discovery…"]);
    let calls = driver_calls(&cmds);
    assert_eq!(calls.len(), 7);
    assert!(
        calls
            .iter()
            .any(|(_, url)| url == "http://svc/api/users/openapi.json")
    );
    // The same address is not discovered twice while in flight or after.
    assert!(hints(&mut rest, "#target\nhttp://svc/api/users\n#payload\n").is_empty());

    let mut rest = discovered();
    let (_, c) = complete(&rest, "#target\nhttp://svc/|\n#action\nPOST");
    assert_eq!(labels(&c), ["/api/users"]);
    assert_eq!(c.items[0].detail, "OpenAPI path");
    let (_, c) = complete(&rest, "#target\nhttp://svc/|\n#action\nDELETE");
    assert_eq!(labels(&c), ["/api/users/{id}"]);
    // The path after the cursor is replaced.
    let (text, c) = complete(&rest, "#target\nhttp://svc|/api/u");
    assert_eq!(labels(&c), ["/api/users"]);
    assert_eq!(c.to as usize, text.len());

    let (_, c) = complete(&rest, "#target\nhttp://svc/api/users\n#headers\nX-T|");
    let tenant = c.items.iter().find(|i| i.label == "X-Tenant: ").unwrap();
    assert_eq!(tenant.detail, "OpenAPI header");

    let (_, c) = complete(
        &rest,
        "#target\nhttp://svc/api/users\n#action\nPOST\n#payload\n|",
    );
    assert_eq!(c.items[0].detail, "Required fields");
    assert_eq!(
        c.items[0].insert,
        "{\n  \"name\": \"$0\",\n  \"role\": \"admin\"\n}"
    );
    let (_, c) = complete(
        &rest,
        "#target\nhttp://svc/api/users\n#action\nPOST\n#payload\n{\n  \"name\": \"x\",\n  |\n}",
    );
    assert_eq!(labels(&c), ["\"role\": \"\"", "\"born\": \"\""]);
    assert_eq!(c.items[1].detail, "date-time (ISO 8601)");
    assert_eq!(c.items[0].detail, "string · enum");
    let (_, c) = complete(
        &rest,
        "#target\nhttp://svc/api/users\n#action\nPOST\n#payload\n{\"role\": \"|\"}",
    );
    assert_eq!(labels(&c), ["\"admin\"", "\"user\""]);

    // A manual Discover runs again despite the cache.
    let cmds = rest.update(Event::Discover(check("#target\nhttp://svc/api/users")));
    assert_eq!(statuses(&cmds), ["Discovery…"]);
}

#[test]
fn discovery_waits_for_hints() {
    let mut rest = Rest::default();
    let text = "#target\nhttp://svc/api/users\n#payload\n";
    assert!(rest.update(Event::Document(check(text))).is_empty());
    assert!(rest.subscriptions().hints);
    assert_eq!(statuses(&hints(&mut rest, text)), ["Discovery…"]);
}

#[test]
fn a_typed_slash_discovers_the_url_before_it() {
    let mut rest = Rest::default();
    let cmds = hints(&mut rest, "#target\nhttp://svc/api/|users/1\n#payload\n");
    let calls = driver_calls(&cmds);
    assert!(
        calls
            .iter()
            .any(|(_, url)| url == "http://svc/api/openapi.json")
    );
    assert!(!calls.iter().any(|(_, url)| url.contains("users")));
    // Not on the endpoint (or before any address): the check's whole URL.
    let mut rest = Rest::default();
    let cmds = hints(&mut rest, "#target\n|http://svc/api/users\n#payload\n");
    let calls = driver_calls(&cmds);
    assert!(
        calls
            .iter()
            .any(|(_, url)| url == "http://svc/api/users/openapi.json")
    );
}

/// The `vars.resolve` commands in `cmds`: (tag, text).
fn resolves(cmds: &[Cmd]) -> Vec<(String, String)> {
    cmds.iter()
        .filter(|cmd| cmd.name == "vars.resolve")
        .map(|cmd| {
            (
                cmd.tag.clone(),
                args(cmd)["text"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// Answers the one `vars.resolve` in `cmds` with `{{var:api}}` → `http://svc/api`.
fn resolve(rest: &mut Rest, cmds: &[Cmd]) -> Vec<Cmd> {
    let [(tag, text)] = &resolves(cmds)[..] else {
        panic!("one resolve in {cmds:?}")
    };
    rest.update(Event::Reply {
        tag: tag.clone(),
        result: Ok(json!(text.replace("{{var:api}}", "http://svc/api"))),
    })
}

#[test]
fn an_address_with_variables_is_resolved_through_the_app() {
    let mut rest = Rest::default();
    let text =
        "#target\n{{var:api}}/users\n#action\nPOST\n#headers\nX-Token: {{var:token}}\n#payload\n";
    let cmds = hints(&mut rest, text);
    assert_eq!(
        resolves(&cmds)
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>(),
        ["{{var:api}}/users"]
    );
    assert!(driver_calls(&cmds).is_empty());
    let cmds = resolve(&mut rest, &cmds);
    let found = "http://svc/api/users/openapi.json";
    let calls = driver_calls(&cmds);
    assert!(calls.iter().any(|(_, url)| url == found));
    // Headers keep their variables: the App resolves a driver call's.
    let call = cmds.iter().find(|cmd| cmd.name == "driver.call").unwrap();
    assert_eq!(
        args(call)["request"]["headers"],
        json!([["X-Token", "{{var:token}}"]])
    );
    let answer = answer(&mut rest, &cmds, found, &spec());
    let status = answer.iter().find(|cmd| cmd.name == "status.set").unwrap();
    assert_eq!(args(status)["detail"]["specUrl"], found);
    assert_eq!(args(status)["detail"]["address"], "http://svc/api/users");

    // Completion knows the resolved address of the endpoint.
    let (_, c) = complete(
        &rest,
        "#target\n{{var:api}}/users\n#action\nPOST\n#payload\n{\n  \"name\": \"x\",\n  |\n}",
    );
    assert_eq!(labels(&c), ["\"role\": \"\"", "\"born\": \"\""]);
    // Another template is resolved when the check changes, without discovery.
    let other = "#target\n{{var:api}}/users/\n#action\nPOST\n#payload\n";
    let (_, c) = complete(&rest, &format!("{other}|"));
    assert_ne!(
        c.items.first().map(|i| i.detail.as_str()),
        Some("Required fields")
    );
    let cmds = rest.update(Event::Document(check(other)));
    assert!(resolve(&mut rest, &cmds).is_empty());
    assert!(rest.update(Event::Document(check(other))).is_empty());
    let (_, c) = complete(&rest, &format!("{other}|"));
    assert_eq!(c.items[0].detail, "Required fields");
    // The same address through another template is cached.
    let cmds = hints(&mut rest, other);
    assert!(resolve(&mut rest, &cmds).is_empty());

    // A manual Discover reports variables that do not resolve.
    let cmds = rest.update(Event::Discover(check("#target\n{{var:nope}}/a")));
    let (tag, _) = resolves(&cmds).remove(0);
    let cmds = rest.update(Event::Reply {
        tag,
        result: Err(Failure::new("Unresolved variable reference: {{var:nope}}")),
    });
    assert_eq!(
        statuses(&cmds),
        [
            "Discovery · ERROR · Variables of {{var:nope}}/a could not be resolved: Unresolved variable reference: {{var:nope}}"
        ]
    );
}

#[test]
fn discovery_caches_each_api_and_tries_a_failed_address_once() {
    let mut rest = Rest::default();
    let site = |name: &str, token: &str| {
        format!(
            "#target\nhttp://svc/{name}/rest/v1/product\n#action\nPOST\n#headers\nX-Token: {token}\n#payload\n"
        )
    };
    for name in ["site1", "site2"] {
        let mut spec = spec();
        spec["basePath"] = json!(format!("/{name}"));
        let cmds = hints(&mut rest, &site(name, "first"));
        let found = format!("http://svc/{name}/swagger.json");
        assert_eq!(
            statuses(&answer(&mut rest, &cmds, &found, &spec)),
            ["Discovery · OK"]
        );
    }
    for name in ["site1", "site2", "site1"] {
        assert!(hints(&mut rest, &site(name, "changed")).is_empty());
    }

    // A refused address is tried once, until the address changes.
    let refused = Failure {
        message: "connection refused".into(),
        details: serde_json::to_vec(&json!({"failure": {"stage": "connect"}})).unwrap(),
    };
    let cmds = hints(&mut rest, "#target\nhttp://down/api/");
    for (tag, _) in driver_calls(&cmds) {
        rest.update(Event::Reply {
            tag,
            result: Err(refused.clone()),
        });
    }
    assert!(hints(&mut rest, "#target\nhttp://down/api/").is_empty());
    assert_eq!(
        statuses(&hints(&mut rest, "#target\nhttp://up/api/")),
        ["Discovery…"]
    );
}

#[test]
fn hints_for_another_address_during_a_run_discover_it_next() {
    let mut rest = Rest::default();
    let first = hints(&mut rest, "#target\nhttp://a/x\n#payload\n");
    assert!(hints(&mut rest, "#target\nhttp://b/x\n#payload\n").is_empty());
    let mut next = vec![];
    for (tag, _) in driver_calls(&first) {
        next = rest.update(Event::Reply {
            tag,
            result: Ok(response(404, "")),
        });
    }
    let mut calls = driver_calls(&next);
    while calls.iter().all(|(_, url)| url.starts_with("http://a/")) && !calls.is_empty() {
        let mut more = vec![];
        for (tag, _) in calls {
            more = rest.update(Event::Reply {
                tag,
                result: Ok(response(404, "")),
            });
        }
        next = more;
        calls = driver_calls(&next);
    }
    assert!(statuses(&next).contains(&"Discovery…".to_owned()));
    assert!(calls.iter().all(|(_, url)| url.starts_with("http://b/")));
}

#[test]
fn discovery_reports_failures() {
    let mut rest = Rest::default();
    let cmds = hints(&mut rest, "#target\nhttp://down");
    let failure = Failure {
        message: "connection refused".into(),
        details: serde_json::to_vec(&json!({"failure": {"stage": "connect"}})).unwrap(),
    };
    let mut last = vec![];
    for (tag, _) in driver_calls(&cmds) {
        last = rest.update(Event::Reply {
            tag,
            result: Err(failure.clone()),
        });
    }
    assert_eq!(statuses(&last), ["Discovery · ERROR · connection refused"]);
    assert!(last.iter().any(|cmd| cmd.name == "log"));

    let mut rest = Rest::default();
    let cmds = hints(&mut rest, "#target\nhttps://svc");
    let mut last = vec![];
    for (tag, _) in driver_calls(&cmds) {
        last = rest.update(Event::Reply {
            tag,
            result: Ok(response(401, "")),
        });
    }
    assert_eq!(statuses(&last), ["Discovery · ERROR · HTTP 401"]);

    // No scheme once resolved: nothing to discover; a manual Discover says so.
    let mut rest = Rest::default();
    let unresolved = |rest: &mut Rest, cmds: Vec<Cmd>| {
        let (tag, _) = resolves(&cmds).remove(0);
        rest.update(Event::Reply {
            tag,
            result: Ok(json!("svc/x")),
        })
    };
    let cmds = hints(&mut rest, "#target\n{{base}}/x");
    assert!(unresolved(&mut rest, cmds).is_empty());
    let cmds = rest.update(Event::Discover(check("#target\n{{base}}/x")));
    assert_eq!(
        statuses(&unresolved(&mut rest, cmds)),
        ["Discovery · WARNING · Invalid or incomplete service address for discovery"]
    );
    assert_eq!(
        statuses(&rest.update(Event::Discover(check("#target\nsvc/x")))),
        ["Discovery · WARNING · Invalid or incomplete service address for discovery"]
    );
}

fn request(call: &DriverCall) -> Value {
    assert_eq!(call.driver, "http");
    assert!(!call.stream);
    call.request()
}

#[test]
fn build_sends_the_endpoint_headers_and_body() {
    let rest = Rest::default();
    let call = rest
        .build(&check(
            "#target\n{{base}}/users\n#action\npost\n#headers\nAccept: */*\n# off: 1\n\nX-Id: {{id}}\n#payload\n{\"a\": 1}",
        ))
        .unwrap();
    let request = request(&call);
    assert_eq!(request["method"], "POST");
    assert_eq!(request["url"], "{{base}}/users");
    assert_eq!(
        request["headers"],
        json!([["Accept", "*/*"], ["X-Id", "{{id}}"]])
    );
    assert_eq!(request["body"], "{\"a\": 1}");
    assert_eq!(request["text"], true);

    let call = rest.build(&check("#target http://x\n#payload\n")).unwrap();
    assert_eq!(call.request()["body"], Value::Null);
    assert_eq!(
        request_of(&rest, "#target\nhttp://x\n#payload\n{}")["method"],
        "GET"
    );
    for (bad, error) in [
        ("#headers\nA: 1", "#target must be a URL"),
        (
            "#target\nGET http://x",
            "#target holds only the URL: put the method GET in #action",
        ),
        ("#target\nhttp://x y", "#target must be a single URL"),
        (
            "#target\nhttp://x\n#action\nFETCH",
            "#action must be one HTTP method: GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS",
        ),
    ] {
        assert_eq!(rest.build(&check(bad)).unwrap_err(), error);
    }
}

fn request_of(rest: &Rest, text: &str) -> Value {
    request(&rest.build(&check(text)).unwrap())
}

fn chunk(response: Value) -> Chunk {
    Chunk {
        call: None,
        response: serde_json::to_vec(&response).unwrap(),
        index: 0,
        last: true,
    }
}

#[test]
fn render_pretty_prints_json_and_maps_outcomes() {
    let rest = Rest::default();
    let r = rest.render(&chunk(response(200, "{\"a\":[1]}"))).unwrap();
    assert_eq!(r.text, "{\n  \"a\": [\n    1\n  ]\n}");
    assert_eq!((r.language.as_str(), r.outcome.as_str()), ("json", "OK"));
    let r = rest.render(&chunk(response(404, "not found"))).unwrap();
    assert_eq!(
        (r.text.as_str(), r.language.as_str(), r.outcome.as_str()),
        ("not found", "text", "WARNING")
    );
    let r = rest.render(&chunk(response(503, "{\"e\":1}"))).unwrap();
    assert_eq!((r.language.as_str(), r.outcome.as_str()), ("json", "ERROR"));
    assert_eq!(
        rest.render(&chunk(response(302, ""))).unwrap().outcome,
        "OK"
    );
}

#[test]
fn export_and_import_round_trip_through_curl() {
    let rest = Rest::default();
    let text = "#target\nhttps://x/a\n#action\nPOST\n#headers\nAccept: */*\nX: it's\n#payload\n{\n  \"a\": 1\n}";
    let curl = rest.export(&check(text)).unwrap();
    assert_eq!(
        curl,
        "curl \\\n  --request POST \\\n  --url 'https://x/a' \\\n  --header 'Accept: */*' \\\n  --header 'X: it'\\''s' \\\n  --data '{\n  \"a\": 1\n}'"
    );
    assert_eq!(rest.import(&curl).unwrap(), text);
    assert_eq!(rest.import("curl https://x").unwrap(), "#target\nhttps://x");
    assert!(rest.import("wget x").is_err());
}

#[test]
fn linked_module_answers_requests_and_events() {
    let linked = Linked::<Rest>::default();
    let call = |kind: request::Kind| {
        let bytes = wire::Request { kind: Some(kind) }.encode_to_vec();
        wire::Answer::decode(linked.call(&bytes).as_slice())
            .unwrap()
            .kind
            .unwrap()
    };
    let document = wire::Event {
        kind: Some(event::Kind::Hints(wire::CompleteRequest {
            check: Some(check("#target\nhttp://svc")),
            pos: 0,
        })),
        now: 0.0,
    };
    let answer::Kind::Updated(updated) = call(request::Kind::Update(wire::UpdateRequest {
        event: Some(document),
        view: false,
    })) else {
        panic!("an update")
    };
    assert_eq!(statuses(&updated.cmds), ["Discovery…"]);
    let answer::Kind::Call(call) = call(request::Kind::Build(check("#target\nhttp://svc"))) else {
        panic!("a call")
    };
    assert_eq!(call.request()["url"], "http://svc");
    let answer::Kind::Schema(schema) = call_schema(&linked) else {
        panic!("a schema")
    };
    assert_eq!(schema.json, None);
}

fn call_schema(linked: &Linked<Rest>) -> answer::Kind {
    let bytes = wire::Request {
        kind: Some(request::Kind::Schema(check("#target\nhttp://svc"))),
    }
    .encode_to_vec();
    wire::Answer::decode(linked.call(&bytes).as_slice())
        .unwrap()
        .kind
        .unwrap()
}
