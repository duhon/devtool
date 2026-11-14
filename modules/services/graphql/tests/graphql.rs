use prost::Message;
use serde_json::{Value, json};
use services_module_graphql::{Graphql, INTROSPECTION_QUERY, render};
use services_sdk::lang::marker;
use services_sdk::wire::{self, answer, event, request};
use services_sdk::{Check, Cmd, Completions, DriverCall, Event, Failure, Linked, Module, Section};

const RUN: &str = "console:graphql:1";

/// Runs `check` with its variables resolved from `vars` the way the App resolves them in
/// JSON mode: the driver call the run makes, or the error it finished with.
fn build(
    graphql: &mut Graphql,
    check: &Check,
    vars: &[(&str, &str)],
) -> Result<DriverCall, String> {
    let cmds = graphql.update(Event::Run {
        tag: RUN.into(),
        check: check.clone(),
    });
    let failed = |cmd: &Cmd| -> Result<DriverCall, String> {
        assert_eq!(cmd.name, "run.result");
        Err(cmd.args()["error"].as_str().unwrap().to_owned())
    };
    if cmds[0].name != "vars.resolve" {
        return failed(&cmds[0]);
    }
    assert_eq!(cmds[0].args()["mode"], "json");
    let mut text = cmds[0].args()["text"].as_str().unwrap().to_owned();
    for (name, value) in vars {
        let escaped = json!(value).to_string();
        text = text.replace(&format!("{{{{{name}}}}}"), &escaped[1..escaped.len() - 1]);
    }
    let cmds = graphql.update(Event::Reply {
        tag: cmds[0].tag.clone(),
        result: Ok(json!(text)),
    });
    if cmds[0].name != "driver.call" {
        return failed(&cmds[0]);
    }
    let args = cmds[0].args();
    Ok(DriverCall::new(
        args["driver"].as_str().unwrap(),
        &args["request"],
    ))
}

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
        service: "graphql".into(),
        text: text.into(),
        sections,
        options: vec![],
    }
}

fn styles(graphql: &Graphql, state: &[u8], lines: &[&str]) -> Vec<Vec<(String, String)>> {
    let lines: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
    graphql
        .highlight(state, &lines, &[])
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
fn complete(graphql: &Graphql, text: &str) -> (String, Completions) {
    let pos = text.find('|').expect("a cursor");
    let text = text.replacen('|', "", 1);
    let result = graphql.complete(&check(&text), pos as u32);
    (text, result)
}

fn labels(c: &Completions) -> Vec<&str> {
    c.items.iter().map(|i| i.label.as_str()).collect()
}

fn statuses(cmds: &[Cmd]) -> Vec<String> {
    cmds.iter()
        .filter(|cmd| cmd.name == "status.set")
        .map(|cmd| cmd.args()["text"].as_str().unwrap().to_owned())
        .collect()
}

/// The discovery calls among `cmds`: their tag and request.
fn driver_calls(cmds: &[Cmd]) -> Vec<(String, Value)> {
    cmds.iter()
        .filter(|cmd| cmd.name == "driver.call")
        .map(|cmd| {
            let args = cmd.args();
            assert_eq!(args["driver"], "http");
            (cmd.tag.clone(), args["request"].clone())
        })
        .collect()
}

fn hints(graphql: &mut Graphql, text: &str) -> Vec<Cmd> {
    graphql.update(Event::Hints {
        check: check(text),
        pos: text.len() as u32,
    })
}

fn reply(graphql: &mut Graphql, tag: &str, status: u16, body: &str) -> Vec<Cmd> {
    graphql.update(Event::Reply {
        tag: tag.into(),
        result: Ok(response(status, body)),
    })
}

fn response(status: u16, body: &str) -> Value {
    json!({"status": status, "statusText": "", "body": body})
}

fn named(kind: &str, name: &str) -> Value {
    json!({"kind": kind, "name": name, "ofType": null})
}

/// An introspection answer: `Query { pets: [Pet] }`, `Pet { name: String }`.
fn introspection() -> String {
    json!({"data": {"__schema": {
        "queryType": {"name": "Query"},
        "mutationType": null,
        "subscriptionType": null,
        "types": [
            {"kind": "OBJECT", "name": "Query", "fields": [{
                "name": "pets", "args": [],
                "type": {"kind": "LIST", "name": null, "ofType": named("OBJECT", "Pet")}
            }]},
            {"kind": "OBJECT", "name": "Pet", "fields": [
                {"name": "name", "args": [], "type": named("SCALAR", "String")}
            ]},
            {"kind": "SCALAR", "name": "String"}
        ]
    }}})
    .to_string()
}

const CONNECTION: &str = "#target\nhttp://svc/graphql\n#metadata\nX-Token: secret\n#payload\n";

/// A Graphql whose discovery introspected `CONNECTION`.
fn discovered() -> Graphql {
    let mut graphql = Graphql::default();
    let cmds = hints(&mut graphql, CONNECTION);
    let (tag, _) = &driver_calls(&cmds)[0];
    let answer = reply(&mut graphql, tag, 200, &introspection());
    assert_eq!(statuses(&answer), ["Discovery · OK"]);
    graphql
}

#[test]
fn the_discovered_schema_is_returned_per_connection() {
    let mut graphql = Graphql::default();
    assert_eq!(graphql.schema(&check(CONNECTION)), None);
    let cmds = hints(&mut graphql, CONNECTION);
    let (tag, _) = &driver_calls(&cmds)[0];
    assert_eq!(graphql.schema(&check(CONNECTION)), None);
    reply(&mut graphql, tag, 200, &introspection());
    let schema = graphql.schema(&check(CONNECTION)).unwrap();
    assert!(schema["types"]["Pet"].is_object());
    assert_eq!(
        graphql.schema(&check("#target\nhttp://other\n#payload\n")),
        None
    );
}

#[test]
fn highlight_styles_each_section_and_carries_state() {
    let graphql = Graphql::default();
    let lines = [
        "#target",
        "https://api.test/graphql",
        "#metadata",
        "Accept: */*",
        "#variables",
        "{\"id\": 1}",
        "#payload",
        "query Q($id: ID!) {",
        "  user(id: $id) { ...F }",
        "}",
        "#responce OK",
        "{\"data\": null}",
    ];
    let s = styles(&graphql, &[], &lines);
    let comment = |text: &str| vec![(text.to_owned(), "comment".to_owned())];
    assert_eq!(s[0], comment("#target"));
    assert!(s[1].contains(&("https://".into(), "keyword".into())));
    assert_eq!(s[3], pairs(&[("Accept", "attribute"), ("*/*", "string")]));
    assert!(s[5].contains(&("\"id\"".into(), "string property".into())));
    assert_eq!(s[6], comment("#payload"));
    assert_eq!(
        s[7],
        pairs(&[
            ("query", "keyword"),
            ("Q", "property"),
            ("$id", "variable-3"),
            ("ID", "property"),
        ])
    );
    assert!(s[8].contains(&("F".into(), "graphql-fragment".into())));
    assert_eq!(s[10], comment("#responce OK"));
    assert!(s[11].contains(&("null".into(), "atom".into())));
    // Text before the first marker is the query.
    assert_eq!(
        styles(&graphql, &[], &["{ pets }"])[0],
        pairs(&[("pets", "property")])
    );

    // The state of a line continues the highlighting on the next call.
    let all: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let whole = graphql.highlight(&[], &all, &[]);
    for at in [4, 7] {
        let split = graphql.highlight(&whole[at].state, &all[at + 1..], &[]);
        assert_eq!(split, whole[at + 1..].to_vec());
    }
}

#[test]
fn folds_cover_selections_variables_and_a_json_response() {
    let graphql = Graphql::default();
    let text = "#variables\n{\n  \"a\": 1\n}\n#payload\nquery {\n  pets {\n    name\n  }\n}\n#responce OK\n{\n  \"b\": 1\n}";
    let folds: Vec<_> = graphql
        .fold(&check(text))
        .iter()
        .map(|f| (f.start, f.end))
        .collect();
    assert_eq!(folds, vec![(1, 3), (5, 9), (6, 8), (11, 13)]);
}

#[test]
fn completion_offers_unused_markers_urls_and_headers() {
    let graphql = Graphql::default();
    let (_, c) = complete(&graphql, "#target\nhttp://x\n#|");
    assert_eq!(labels(&c), ["#metadata", "#variables", "#payload"]);
    let (_, c) = complete(&graphql, "#target\nht|");
    assert_eq!(labels(&c), ["http://", "https://"]);
    assert_eq!((c.from, c.to), (8, 10));
    let (_, c) = complete(&graphql, "#metadata\nX-Api-Key: 1\nX-A|");
    assert!(
        !labels(&c)
            .iter()
            .any(|label| label.starts_with("X-Api-Key"))
    );
    let (_, c) = complete(&graphql, "#variables\n|");
    assert_eq!(labels(&c), ["{}"]);
    let (_, c) = complete(&graphql, "#payload\n|");
    assert_eq!(
        labels(&c)[..4],
        ["query", "mutation", "subscription", "fragment"]
    );
}

#[test]
fn discovery_feeds_query_completion() {
    let mut graphql = Graphql::default();
    let cmds = hints(&mut graphql, CONNECTION);
    assert_eq!(statuses(&cmds), ["Discovery…"]);
    let calls = driver_calls(&cmds);
    let (tag, request) = &calls[0];
    assert_eq!(request["method"], "POST");
    assert_eq!(request["url"], "http://svc/graphql");
    assert_eq!(
        request["headers"],
        json!([["Content-Type", "application/json"], ["X-Token", "secret"]])
    );
    let body: Value = serde_json::from_str(request["body"].as_str().unwrap()).unwrap();
    assert_eq!(body, json!({"query": INTROSPECTION_QUERY}));
    // Hints while it runs, or once it is known, do not introspect again.
    assert!(hints(&mut graphql, CONNECTION).is_empty());
    let answer = reply(&mut graphql, tag, 200, &introspection());
    let status = answer.iter().find(|cmd| cmd.name == "status.set").unwrap();
    assert_eq!(status.args()["detail"]["url"], "http://svc/graphql");
    assert!(hints(&mut graphql, CONNECTION).is_empty());

    let (_, c) = complete(&graphql, &format!("{CONNECTION}{{ pets {{ na|"));
    assert_eq!(labels(&c), ["name"]);
    let (_, c) = complete(&graphql, &format!("{CONNECTION}{{ pe|"));
    assert_eq!(labels(&c), ["pets"]);
    // A wrongly cased name is still corrected.
    let (_, c) = complete(&graphql, &format!("{CONNECTION}{{ pets {{ Na|"));
    assert_eq!(labels(&c), ["name"]);
    // Another connection has no schema yet.
    let (_, c) = complete(&graphql, "#target\nhttp://other\n#payload\n{ pe|");
    assert!(c.items.is_empty());
}

#[test]
fn discovery_waits_for_hints_and_a_web_address() {
    let mut graphql = Graphql::default();
    assert!(
        graphql
            .update(Event::Document(check(CONNECTION)))
            .is_empty()
    );
    assert!(hints(&mut graphql, "#target\nsvc/graphql\n#payload\n").is_empty());
    let cmds = graphql.update(Event::Discover(check("#target\nsvc\n")));
    assert_eq!(
        statuses(&cmds),
        ["Discovery · WARNING · Invalid or incomplete service address for discovery"]
    );
    // A variable address is resolved by the App before the call.
    let cmds = hints(&mut graphql, "#target\n{{base}}/graphql\n#payload\n");
    assert_eq!(driver_calls(&cmds)[0].1["url"], "{{base}}/graphql");
}

#[test]
fn discovery_reports_failures_and_rediscovers_on_demand() {
    let cases = [
        (
            200,
            json!({"errors": [{"message": "Introspection is disabled"}]}).to_string(),
            "GraphQL introspection failed: Introspection is disabled",
        ),
        (
            200,
            json!({"data": {"__schema": null}}).to_string(),
            "GraphQL introspection returned no schema",
        ),
        (
            200,
            json!({"data": {"__schema": {"queryType": {"name": "Query"}, "types": []}}})
                .to_string(),
            "GraphQL introspection returned no query fields",
        ),
        (
            200,
            json!({"data": {"__schema": {}}, "errors": [{"message": "Partial schema denied"}]})
                .to_string(),
            "GraphQL introspection failed: Partial schema denied",
        ),
        (403, "Forbidden".into(), "GraphQL discovery HTTP 403"),
    ];
    for (status, body, expected) in cases {
        let mut graphql = Graphql::default();
        let cmds = hints(&mut graphql, CONNECTION);
        let answer = reply(&mut graphql, &driver_calls(&cmds)[0].0, status, &body);
        assert_eq!(
            statuses(&answer),
            [format!("Discovery · ERROR · {expected}")]
        );
        // A failed address is not retried by hints, only on demand.
        assert!(hints(&mut graphql, CONNECTION).is_empty());
        let cmds = graphql.update(Event::Discover(check(CONNECTION)));
        assert_eq!(driver_calls(&cmds).len(), 1);
    }
    let mut graphql = Graphql::default();
    let cmds = hints(&mut graphql, CONNECTION);
    let answer = reply(&mut graphql, &driver_calls(&cmds)[0].0, 200, "not JSON");
    assert!(
        statuses(&answer)[0].starts_with("Discovery · ERROR · Invalid GraphQL introspection JSON")
    );
    let mut graphql = Graphql::default();
    let cmds = hints(&mut graphql, CONNECTION);
    let answer = graphql.update(Event::Reply {
        tag: driver_calls(&cmds)[0].0.clone(),
        result: Err(Failure::new("refused")),
    });
    assert_eq!(statuses(&answer), ["Discovery · ERROR · refused"]);
}

#[test]
fn hints_for_another_connection_during_a_run_discover_it_next() {
    let mut graphql = Graphql::default();
    let first = hints(&mut graphql, CONNECTION);
    let other = "#target\nhttp://other/graphql\n#payload\n";
    assert!(hints(&mut graphql, other).is_empty());
    let answer = reply(
        &mut graphql,
        &driver_calls(&first)[0].0,
        200,
        &introspection(),
    );
    let next = driver_calls(&answer);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].1["url"], "http://other/graphql");
    assert!(discovered().complete(&check(CONNECTION), 0).items.len() <= 3);
}

fn request(call: &DriverCall) -> Value {
    assert_eq!(call.driver, "http");
    call.request()
}

#[test]
fn build_posts_the_query_and_variables() {
    let mut graphql = Graphql::default();
    let text = "#target\nhttps://x/graphql\n#metadata\nContent-Type: application/graphql+json\nX: 1\n#variables\n{\"id\": \"{{id}}\"}\n#payload\nquery ($id: ID) { user(id: $id) { name } }";
    let r = request(&build(&mut graphql, &check(text), &[("id", "a\"b")]).unwrap());
    assert_eq!(r["method"], "POST");
    assert_eq!(r["url"], "https://x/graphql");
    assert_eq!(
        r["headers"],
        json!([["Content-Type", "application/graphql+json"], ["X", "1"]])
    );
    let body: Value = serde_json::from_str(r["body"].as_str().unwrap()).unwrap();
    assert_eq!(
        body,
        json!({"query": "query ($id: ID) { user(id: $id) { name } }", "variables": {"id": "a\"b"}})
    );
    // The query may precede the markers; without variables none are sent.
    let r = request(
        &build(
            &mut graphql,
            &check("{ pets { name } }\n#target\nhttp://x"),
            &[],
        )
        .unwrap(),
    );
    let body: Value = serde_json::from_str(r["body"].as_str().unwrap()).unwrap();
    assert_eq!(body, json!({"query": "{ pets { name } }"}));
    for (text, error) in [
        ("#target\nhttp://x\n#payload\n", "query is required"),
        (
            "#target\nhttp://x\n#variables\nbad\n#payload\n{ a }",
            "Variables must be valid JSON",
        ),
        (
            "#target\nPOST http://x\n#payload\n{ a }",
            "#target must be a GraphQL URL",
        ),
        (
            "#target\nhttp://x\n#options\nFormat: json\n#payload\n{ a }",
            "Unknown option \"Format\"; GraphQL takes Repeat and Repeat-Until",
        ),
    ] {
        assert_eq!(build(&mut graphql, &check(text), &[]).unwrap_err(), error);
    }
}

#[test]
fn render_pretty_prints_json_and_maps_outcomes() {
    let r = render(&response(200, "{\"data\":{\"a\":1}}")).unwrap();
    assert_eq!(r.text, "{\n  \"data\": {\n    \"a\": 1\n  }\n}");
    assert_eq!((r.language.as_str(), r.outcome.as_str()), ("json", "OK"));
    let r = render(&response(400, "bad")).unwrap();
    assert_eq!(
        (r.text.as_str(), r.language.as_str(), r.outcome.as_str()),
        ("bad", "text", "WARNING")
    );
    let r = render(&response(502, "{}")).unwrap();
    assert_eq!(r.outcome, "ERROR");
}

#[test]
fn export_and_import_round_trip_through_curl() {
    let graphql = Graphql::default();
    let text = "#target\nhttps://x/graphql\n#metadata\nX: it's\n#variables\n{\n  \"id\": 1\n}\n#payload\n{ user(id: $id) { name } }";
    let curl = graphql.export(&check(text)).unwrap();
    assert_eq!(
        curl,
        "curl \\\n  --request POST \\\n  --url 'https://x/graphql' \\\n  --header 'X: it'\\''s' \\\n  --header 'Content-Type: application/json' \\\n  --data '{\n  \"query\": \"{ user(id: $id) { name } }\",\n  \"variables\": {\n    \"id\": 1\n  }\n}'"
    );
    assert_eq!(
        graphql.import(&curl).unwrap(),
        "#target\nhttps://x/graphql\n#metadata\nX: it's\nContent-Type: application/json\n#variables\n{\n  \"id\": 1\n}\n#payload\n{ user(id: $id) { name } }"
    );
    assert_eq!(
        graphql
            .import("curl https://x/graphql --data '{ pets }'")
            .unwrap(),
        "#target\nhttps://x/graphql\n#payload\n{ pets }"
    );
    assert_eq!(
        graphql.import("curl https://x").unwrap_err(),
        "Could not find a GraphQL query in the curl command"
    );
    assert!(graphql.import("wget x").is_err());
}

#[test]
fn linked_module_answers_requests_and_events() {
    let linked = Linked::<Graphql>::default();
    let call = |kind: request::Kind| {
        let bytes = wire::Request { kind: Some(kind) }.encode_to_vec();
        wire::Answer::decode(linked.call(&bytes).as_slice())
            .unwrap()
            .kind
            .unwrap()
    };
    let hints = wire::Event {
        kind: Some(event::Kind::Hints(wire::CompleteRequest {
            check: Some(check(CONNECTION)),
            pos: 0,
        })),
        now: 0.0,
    };
    let answer::Kind::Updated(updated) = call(request::Kind::Update(wire::UpdateRequest {
        event: Some(hints),
        view: false,
    })) else {
        panic!("an update")
    };
    assert_eq!(statuses(&updated.cmds), ["Discovery…"]);
    let run = wire::Event {
        kind: Some(event::Kind::Run(wire::RunEvent {
            tag: RUN.into(),
            check: Some(check(&format!("{CONNECTION}{{ pets }}"))),
        })),
        now: 0.0,
    };
    let answer::Kind::Updated(updated) = call(request::Kind::Update(wire::UpdateRequest {
        event: Some(run),
        view: false,
    })) else {
        panic!("an update")
    };
    assert_eq!(updated.cmds[0].name, "vars.resolve");
}

#[test]
fn curl_matches_the_original_javascript() {
    let graphql = Graphql::default();
    assert_eq!(
        graphql
            .import("curl 'https://api.example.com/graphql' -H 'content-type: application/json' --data-raw '{\"query\":\"query Foo($id: ID!) { foo(id: $id) { name } }\",\"variables\":{\"id\":\"1\"}}'")
            .unwrap(),
        "#target\nhttps://api.example.com/graphql\n#metadata\ncontent-type: application/json\n#variables\n{\n  \"id\": \"1\"\n}\n#payload\nquery Foo($id: ID!) { foo(id: $id) { name } }"
    );
    assert_eq!(
        graphql
            .export(&check("#target\nhttps://api.example.com/graphql\n#variables\n{\"id\":\"1\"}\n#payload\nquery Foo($id: ID!) { foo(id: $id) { name } }"))
            .unwrap(),
        "curl \\\n  --request POST \\\n  --url 'https://api.example.com/graphql' \\\n  --header 'Content-Type: application/json' \\\n  --data '{\n  \"query\": \"query Foo($id: ID!) { foo(id: $id) { name } }\",\n  \"variables\": {\n    \"id\": \"1\"\n  }\n}'"
    );
}
