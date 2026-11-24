use prost::Message;
use serde_json::{Value, json};
use services_module_prometheus::{
    Prometheus,
    request::{Plan, sections, time},
};
use services_sdk::{Check, Cmd, Event, Failure, Module, Section};

fn check(metadata: &str, options: &str, payload: &str) -> Check {
    Check {
        service: "prometheus".into(),
        sections: [
            ("target", "https://example.invalid/prometheus/"),
            ("metadata", metadata),
            ("options", options),
            ("payload", payload),
        ]
        .into_iter()
        .map(|(name, text)| Section {
            name: name.into(),
            text: text.into(),
            ..Section::default()
        })
        .collect(),
        ..Check::default()
    }
}

fn args(cmd: &Cmd) -> Value {
    serde_json::from_slice(&cmd.args).unwrap()
}
fn form(plan: &Plan) -> std::collections::BTreeMap<String, String> {
    url::form_urlencoded::parse(plan.request.body.as_ref().unwrap().as_bytes())
        .into_owned()
        .collect()
}
fn plan(metadata: &str) -> Result<Plan, String> {
    Plan::parse(&sections(&check(metadata, "", "up"))?, 7_200_123.0)
}

#[test]
fn post_preserves_promql_prefix_headers_and_encoding() {
    let query = "sum(rate(a{label=\"a+b&=☃\"}[5m]))\n# a PromQL comment\n";
    let p = Plan::parse(
        &sections(&check(
            "Authorization: Bearer token\nX-Scope-OrgID: tenant\nQuery-Time: 2024-01-01T00:00:00Z",
            "",
            query,
        ))
        .unwrap(),
        0.0,
    )
    .unwrap();
    assert_eq!(
        p.request.url,
        "https://example.invalid/prometheus/api/v1/query"
    );
    assert_eq!(form(&p)["query"], query);
    assert_eq!(form(&p)["time"], "1704067200");
    assert_eq!(p.request.method, "POST");
    assert_eq!(p.request.redirects, Some(0));
    assert!(p.request.timeout_ms.unwrap() > 0);
    assert!(
        p.request
            .headers
            .contains(&("Authorization".into(), "Bearer token".into()))
    );
    assert!(
        p.request
            .headers
            .contains(&("X-Scope-OrgID".into(), "tenant".into()))
    );
    assert!(
        !p.request
            .headers
            .iter()
            .any(|(k, _)| k.starts_with("Query-"))
    );
}

#[test]
fn raw_payload_preserves_comments_split_by_kernel_and_multiline_literals() {
    let mut check = check("", "", "up");
    check.text = concat!(
        "#target\nhttps://example.invalid/prometheus/\n",
        "#payload\n#comment\nlabel_replace(up, \"job\", `first\n",
        "#metadata\nlast`, \"job\", \".*\")\n",
        "#options\nFormat: table\n",
    )
    .into();
    let resolved = sections(&check).unwrap();
    let expected = concat!(
        "#comment\nlabel_replace(up, \"job\", `first\n",
        "#metadata\nlast`, \"job\", \".*\")\n",
    );
    assert_eq!(resolved["payload"], expected);
    let plan = Plan::parse(&resolved, 0.0).unwrap();
    assert_eq!(form(&plan)["query"], expected);
    check.text = "#payload sum(up)\n#comment\n".into();
    assert_eq!(sections(&check).unwrap()["payload"], "sum(up)\n#comment\n");
    check.text =
        "#payload label_replace(up, \"job\", `first\n#metadata\nlast`, \"job\", \".*\")".into();
    assert_eq!(
        sections(&check).unwrap()["payload"],
        "label_replace(up, \"job\", `first\n#metadata\nlast`, \"job\", \".*\")"
    );
}

#[test]
fn relative_range_uses_one_clock_and_defaults_end() {
    let p = plan("Query-Mode: range\nQuery-Start: now-1h\nQuery-Step: 1m30s\nQuery-Timeout: 5s")
        .unwrap();
    assert!(p.request.url.ends_with("/api/v1/query_range"));
    assert_eq!(form(&p)["start"], "3600.123");
    assert_eq!(form(&p)["end"], "7200.123");
    assert_eq!(form(&p)["step"], "90");
    assert_eq!(form(&p)["timeout"], "5");
    assert_eq!(form(&plan("").unwrap())["time"], "7200.123");
    assert_eq!(time("1970-01-01T01:00:00+01:00", 0.0).unwrap(), 0.0);
    assert_eq!(time("-1.25", 0.0).unwrap(), -1.25);
}

#[test]
fn rejects_invalid_or_contradictory_parameters_and_options() {
    for metadata in [
        "Query-What: x",
        "Query-Mode: nonsense",
        "Query-Step: 1m",
        "Query-Mode: range\nQuery-Start: 2\nQuery-End: 1\nQuery-Step: 1s",
        "Query-Mode: range\nQuery-Step: 1s",
        "Query-Mode: range\nQuery-Start: 0",
        "Query-Mode: range\nQuery-Time: now\nQuery-Start: 0\nQuery-Step: 1s",
        "Query-Mode: range\nQuery-Start: 0\nQuery-Step: 0",
        "Query-Timeout: -1",
        "Query-Timeout: NaN",
        "Query-Time: infinity",
        "Query-Time: bad",
        "Query-Time: now\nquery-time: 2",
        "Authorization no-colon",
        "Content-Type: application/json",
    ] {
        assert!(plan(metadata).is_err(), "{metadata}");
    }
    assert!(Plan::parse(&sections(&check("", "Mystery: yes", "up")).unwrap(), 0.0).is_err());
    let mut resolved = sections(&check("", "", "up")).unwrap();
    resolved["target"] = json!("https://example.invalid/path?x=y");
    assert!(Plan::parse(&resolved, 0.0).is_err());
}

#[test]
fn resolves_all_fields_before_encoding_and_never_persists_encoded_secrets() {
    let mut module = Prometheus::default();
    let mut check = check(
        "Authorization: Bearer {{token}}\nQuery-Time: {{time}}",
        "Format: {{format}}",
        "{{query}}",
    );
    check.sections[0].text = "{{base}}".into();
    let commands = module.update(Event::Run {
        tag: "r".into(),
        check,
        transform: false,
    });
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "vars.resolve");
    assert_eq!(args(&commands[0])["mode"], "json");
    let mut resolved: Value =
        serde_json::from_str(args(&commands[0])["text"].as_str().unwrap()).unwrap();
    resolved["target"] = json!("https://example.invalid/proxy");
    resolved["headers"] = json!([
        ["Authorization", "Bearer sensitive+/&"],
        ["Query-Time", "now"]
    ]);
    resolved["options"] = json!([["Format", "table"]]);
    resolved["payload"] = json!("up{label=\"sensitive+/&\"}\n# unchanged");
    let call = module.update(Event::Reply {
        tag: "r:resolve".into(),
        result: Ok(json!(resolved.to_string())),
    });
    assert_eq!(call[0].name, "driver.call");
    let request = &args(&call[0])["request"];
    let encoded = request["body"].as_str().unwrap();
    assert!(encoded.contains("sensitive%2B%2F%26"));
    let decoded: std::collections::BTreeMap<_, _> = url::form_urlencoded::parse(encoded.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(decoded["query"], resolved["payload"].as_str().unwrap());
    assert_eq!(decoded["time"], "0");
    let response = json!({"status":200, "body": json!({"status":"success", "data":{"resultType":"vector","result":[]}}).to_string()});
    let done = module.update(Event::Reply {
        tag: "r:call".into(),
        result: Ok(response),
    });
    let result = args(done.last().unwrap());
    assert_eq!(result["outcome"], "OK");
    assert!(result["debug"].is_null());
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("sensitive")
    );
    assert!(
        module
            .update(Event::Reply {
                tag: "r:call".into(),
                result: Err(Failure::new("late"))
            })
            .is_empty()
    );
}

#[test]
fn resolution_failure_and_cancellation_do_not_call_server() {
    let mut module = Prometheus::default();
    module.update(Event::Run {
        tag: "r".into(),
        check: check("", "", "not locally parseable !"),
        transform: false,
    });
    let failure = module.update(Event::Reply {
        tag: "r:resolve".into(),
        result: Err(Failure::new("missing variable")),
    });
    assert_eq!(args(&failure[0])["outcome"], "ERROR");
    module.update(Event::Run {
        tag: "c".into(),
        check: check("", "", "up"),
        transform: false,
    });
    assert!(module.update(Event::Cancel { tag: "c".into() }).is_empty());
    let resolved = sections(&check("", "", "up")).unwrap();
    assert!(
        module
            .update(Event::Reply {
                tag: "c:resolve".into(),
                result: Ok(json!(resolved.to_string()))
            })
            .is_empty()
    );
}

#[test]
fn parser_errors_do_not_block_server_and_end_is_explicit() {
    let mut module = Prometheus::default();
    let c = check("", "", "sum( intentionally malformed");
    let resolved = sections(&c).unwrap();
    module.update(Event::Run {
        tag: "r".into(),
        check: c,
        transform: false,
    });
    let cmds = module.update(Event::Reply {
        tag: "r:resolve".into(),
        result: Ok(json!(resolved.to_string())),
    });
    assert_eq!(cmds[0].name, "driver.call");
    let end = module.update(Event::End {
        tag: "r:call".into(),
    });
    assert!(
        args(&end[0])["error"]
            .as_str()
            .unwrap()
            .contains("without a response")
    );
}

#[test]
fn resolved_header_cannot_inject_new_header() {
    let mut resolved = sections(&check("Authorization: {{token}}", "", "up")).unwrap();
    resolved["headers"] = json!([["Authorization", "token\r\nX-Injected: value"]]);
    assert!(Plan::parse(&resolved, 0.0).is_err());
}

#[test]
fn delayed_resolution_keeps_run_clock_and_next_run_refreshes_it() {
    use services_sdk::{Linked, wire};
    let module = Linked::<Prometheus>::default();
    let update = |kind: wire::event::Kind, now| {
        let request = wire::Request {
            kind: Some(wire::request::Kind::Update(wire::UpdateRequest {
                event: Some(wire::Event {
                    kind: Some(kind),
                    now,
                }),
                view: false,
            })),
        };
        let answer =
            wire::Answer::decode(module.call(&request.encode_to_vec()).as_slice()).unwrap();
        let Some(wire::answer::Kind::Updated(updated)) = answer.kind else {
            panic!("expected Updated")
        };
        updated.cmds
    };
    let c = check(
        "Query-Mode: range\nQuery-Start: now-1h\nQuery-Step: 1m",
        "",
        "up",
    );
    let resolved = sections(&c).unwrap();
    for (tag, run_now, expected_end) in [
        ("first", 7_200_123.0, "7200.123"),
        ("repeat", 10_800_123.0, "10800.123"),
    ] {
        update(
            wire::event::Kind::Run(wire::RunEvent {
                tag: tag.into(),
                check: Some(c.clone()),
                transform: false,
            }),
            run_now,
        );
        let cmds = update(
            wire::event::Kind::Reply(wire::Reply {
                tag: format!("{tag}:resolve"),
                ok: Some(serde_json::to_vec(&json!(resolved.to_string())).unwrap()),
                error: None,
            }),
            run_now + 3_600_000.0,
        );
        let request = args(&cmds[0]);
        let body = request["request"]["body"].as_str().unwrap();
        let form: std::collections::BTreeMap<_, _> = url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect();
        assert_eq!(form["end"], expected_end);
        let span = form["end"].parse::<f64>().unwrap() - form["start"].parse::<f64>().unwrap();
        assert!((span - 3600.0).abs() < 1e-9);
    }
}
