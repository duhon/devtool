use serde_json::{Value, json};
use services_module_prometheus::{
    discovery::Discovery,
    language::{self, Context},
};
use services_sdk::{Check, Cmd, Failure, Section};

fn check(target: &str, metadata: &str, query: &str) -> Check {
    Check {
        text: format!("#target\n{target}\n#metadata\n{metadata}\n#payload\n{query}"),
        sections: [
            ("target", target),
            ("metadata", metadata),
            ("payload", query),
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

fn resolve(discovery: &mut Discovery, commands: &[Cmd], token: &str) -> Vec<Cmd> {
    let command = commands.iter().find(|c| c.name == "vars.resolve").unwrap();
    let text = command.args()["text"]
        .as_str()
        .unwrap()
        .replace("{{.base}}", "https://example.invalid/proxy/prom")
        .replace("{{.token}}", token);
    discovery
        .reply(&command.tag, Ok(Value::String(text)))
        .unwrap()
}

fn request(commands: &[Cmd]) -> (&str, Value) {
    let command = commands.iter().find(|c| c.name == "driver.call").unwrap();
    (&command.tag, command.args()["request"].clone())
}

fn success(data: Value) -> Value {
    json!({"status": 200, "body": json!({"status":"success", "data":data}).to_string()})
}

#[test]
fn open_is_quiet_and_hints_resolve_before_bounded_scoped_request() {
    let mut discovery = Discovery::default();
    let check = check(
        "{{.base}}",
        "Authorization: Bearer {{.token}}\nQuery-Time: now\nQuery-Mode: range",
        "requests_total{job=\"a+b&雪\", zone=\"",
    );
    assert!(
        !discovery
            .document(&check)
            .iter()
            .any(|c| c.name == "driver.call" || c.name == "vars.resolve")
    );
    let commands = discovery.hints(&check, check.text.len());
    assert!(!commands.iter().any(|c| c.name == "driver.call"));
    let commands = resolve(&mut discovery, &commands, "test-secret");
    let (tag, request) = request(&commands);
    let url = url::Url::parse(request["url"].as_str().unwrap()).unwrap();
    assert_eq!(url.path(), "/proxy/prom/api/v1/label/zone/values");
    let parameters: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
    assert_eq!(parameters["match[]"], "requests_total{job=\"a+b&雪\"}");
    assert_eq!(parameters["limit"], "1000");
    assert_eq!(
        request["headers"],
        json!([["Authorization", "Bearer test-secret"]])
    );
    assert_eq!(request["redirects"], 0);
    assert!(request["timeoutMs"].as_u64().unwrap() > 0);
    discovery
        .reply(tag, Ok(success(json!(["east", "west"]))))
        .unwrap();
    assert_eq!(
        discovery.candidates(&check, &language::context(&check, check.text.len())),
        ["east", "west"]
    );
    assert!(!format!("{discovery:?}").contains("test-secret"));
}

#[test]
fn cached_hints_re_resolve_and_credentials_do_not_share_schema() {
    let mut discovery = Discovery::default();
    let check = check("{{.base}}", "Authorization: Bearer {{.token}}", "");
    let commands = discovery.hints(&check, check.text.len());
    let commands = resolve(&mut discovery, &commands, "one");
    let tag = request(&commands).0.to_owned();
    discovery
        .reply(&tag, Ok(success(json!(["private_one"]))))
        .unwrap();
    assert_eq!(
        discovery.schema_of(&check).unwrap()["metrics"],
        json!(["private_one"])
    );
    assert!(
        language::complete(&check, check.text.len(), &discovery)
            .items
            .iter()
            .any(|i| i.insert == "private_one")
    );
    let commands = discovery.hints(&check, check.text.len());
    assert!(discovery.schema_of(&check).is_none());
    let commands = resolve(&mut discovery, &commands, "one");
    assert!(!commands.iter().any(|c| c.name == "driver.call"));
    let commands = discovery.hints(&check, check.text.len());
    let commands = resolve(&mut discovery, &commands, "two");
    assert_eq!(discovery.schema_of(&check).unwrap()["metrics"], json!([]));
    let tag = request(&commands).0.to_owned();
    discovery
        .reply(&tag, Ok(success(json!(["private_two"]))))
        .unwrap();
    assert_eq!(
        discovery.schema_of(&check).unwrap()["metrics"],
        json!(["private_two"])
    );
}

#[test]
fn stale_target_reply_cannot_contaminate_schema_or_status() {
    let mut discovery = Discovery::default();
    let old = check("https://old.invalid/prom", "", "");
    let new = check("https://new.invalid/prom", "", "");
    let commands = discovery.hints(&old, old.text.len());
    let commands = resolve(&mut discovery, &commands, "");
    let old_tag = request(&commands).0.to_owned();
    discovery.document(&new);
    let commands = discovery.hints(&new, new.text.len());
    let commands = resolve(&mut discovery, &commands, "");
    let new_tag = request(&commands).0.to_owned();
    assert!(
        discovery
            .reply(&old_tag, Ok(success(json!(["old"]))))
            .unwrap()
            .is_empty()
    );
    discovery
        .reply(&new_tag, Ok(success(json!(["new"]))))
        .unwrap();
    assert_eq!(
        discovery.schema_of(&new).unwrap()["metrics"],
        json!(["new"])
    );
    assert!(discovery.schema_of(&old).is_none());
}

#[test]
fn failures_are_visible_not_busy_and_manual_retries_same_context() {
    for answer in [
        Err(Failure::new("secret credential from driver")),
        Ok(json!({"status":503, "body":"secret server body"})),
        Ok(json!({"status":200, "body":"{invalid"})),
        Ok(json!({"status":200, "body":json!({"status":"error","error":"secret"}).to_string()})),
        Ok(success(json!([{"not":"a string"}]))),
    ] {
        let mut discovery = Discovery::default();
        let check = check("https://example.invalid/prom", "", "up{job=\"");
        let commands = discovery.hints(&check, check.text.len());
        let commands = resolve(&mut discovery, &commands, "");
        let tag = request(&commands).0.to_owned();
        let warnings = discovery.reply(&tag, answer).unwrap();
        let status = warnings
            .iter()
            .find(|c| c.name == "status.set")
            .unwrap()
            .args();
        assert_eq!(status["indicator"], "warning");
        assert!(!status.to_string().contains("secret"));
        let commands = discovery.hints(&check, check.text.len());
        let commands = resolve(&mut discovery, &commands, "");
        assert!(!commands.iter().any(|c| c.name == "driver.call"));
        let commands = discovery.manual(&check);
        let commands = resolve(&mut discovery, &commands, "");
        assert!(
            request(&commands).1["url"]
                .as_str()
                .unwrap()
                .contains("/label/job/values")
        );
    }
}

#[test]
fn discovery_does_not_fetch_for_duration_comment_or_placeholder() {
    let mut discovery = Discovery::default();
    for query in ["up[", "# comment", "up{job=\"{{.token"] {
        let check = check("https://example.invalid", "", query);
        assert!(
            discovery.hints(&check, check.text.len()).is_empty(),
            "{query}"
        );
    }
}

#[test]
fn labels_are_metric_scoped_and_proxy_api_paths_are_not_duplicated() {
    let mut discovery = Discovery::default();
    let check = check("https://example.invalid/proxy/api/v1/query", "", "up{jo");
    assert_eq!(
        language::context(&check, check.text.len()),
        Context::Labels {
            selector: Some("up{}".into())
        }
    );
    let commands = discovery.hints(&check, check.text.len());
    let commands = resolve(&mut discovery, &commands, "");
    let (tag, request) = request(&commands);
    let url = url::Url::parse(request["url"].as_str().unwrap()).unwrap();
    assert_eq!(url.path(), "/proxy/api/v1/labels");
    assert_eq!(
        url.query_pairs().find(|(k, _)| k == "match[]").unwrap().1,
        "up{}"
    );
    discovery
        .reply(tag, Ok(success(json!(["job", "instance"]))))
        .unwrap();
    assert!(
        language::complete(&check, check.text.len(), &discovery)
            .items
            .iter()
            .any(|i| i.insert == "job")
    );
}

#[test]
fn resolution_failures_and_unresolved_headers_never_call_driver() {
    let mut discovery = Discovery::default();
    let check = check("{{.base}}", "Authorization: {{.token}}", "");
    let commands = discovery.hints(&check, check.text.len());
    let resolve_command = commands.iter().find(|c| c.name == "vars.resolve").unwrap();
    let commands = discovery
        .reply(&resolve_command.tag, Err(Failure::new("secret")))
        .unwrap();
    assert_eq!(commands[0].args()["indicator"], "warning");
    let commands = discovery.manual(&check);
    let commands = resolve(&mut discovery, &commands, "{{unresolved}}");
    assert!(!commands.iter().any(|c| c.name == "driver.call"));
    assert_eq!(commands[0].args()["indicator"], "warning");
}

#[test]
fn encoded_proxy_mount_is_preserved_and_server_warnings_are_visible() {
    let mut discovery = Discovery::default();
    let check = check("https://example.invalid/proxy%20mount/api/v1", "", "");
    let commands = discovery.hints(&check, check.text.len());
    let commands = resolve(&mut discovery, &commands, "");
    let (tag, request) = request(&commands);
    assert!(
        request["url"]
            .as_str()
            .unwrap()
            .starts_with("https://example.invalid/proxy%20mount/api/v1/label/")
    );
    let data: Vec<_> = (0..1100).map(|i| format!("metric_{i}")).collect();
    let commands = discovery.reply(tag, Ok(json!({
        "status":200, "body":json!({"status":"success","data":data,"warnings":["server detail not logged"]}).to_string()
    }))).unwrap();
    assert_eq!(commands[0].args()["indicator"], "warning");
    assert_eq!(
        discovery.schema_of(&check).unwrap()["metrics"]
            .as_array()
            .unwrap()
            .len(),
        1000
    );
}

#[test]
fn values_older_than_ten_minutes_are_served_while_hints_refresh_them() {
    use prost::Message;
    use services_module_prometheus::Prometheus;
    use services_sdk::{Linked, wire};
    let module = Linked::<Prometheus>::default();
    let call = |kind: wire::request::Kind| {
        let bytes = wire::Request { kind: Some(kind) }.encode_to_vec();
        wire::Answer::decode(module.call(&bytes).as_slice())
            .unwrap()
            .kind
            .unwrap()
    };
    let update = |kind: wire::event::Kind, now: f64| {
        let event = Some(wire::Event {
            kind: Some(kind),
            now,
        });
        match call(wire::request::Kind::Update(wire::UpdateRequest {
            event,
            view: false,
        })) {
            wire::answer::Kind::Updated(updated) => updated.cmds,
            other => panic!("{other:?}"),
        }
    };
    let reply = |tag: &str, ok: Option<Value>, now: f64| {
        let reply = wire::event::Kind::Reply(wire::Reply {
            tag: tag.into(),
            error: ok.is_none().then(|| Failure::new("refused")),
            ok: ok.map(|value| serde_json::to_vec(&value).unwrap()),
        });
        update(reply, now)
    };
    let check = check("https://prom.invalid", "", "");
    // Hints resolve the connection; the driver call is made only if the values are due.
    let hints = |now: f64| {
        let hints = wire::event::Kind::Hints(wire::CompleteRequest {
            check: Some(check.clone()),
            pos: check.text.len() as u32,
        });
        let commands = update(hints, now);
        let resolve = commands.iter().find(|c| c.name == "vars.resolve").unwrap();
        let text = resolve.args()["text"].clone();
        let commands = reply(&resolve.tag, Some(text), now);
        commands
            .iter()
            .find(|c| c.name == "driver.call")
            .map(|c| c.tag.clone())
    };
    let metrics = || match call(wire::request::Kind::Schema(check.clone())) {
        wire::answer::Kind::Schema(schema) => {
            serde_json::from_slice::<Value>(&schema.json.unwrap()).unwrap()["metrics"].clone()
        }
        other => panic!("{other:?}"),
    };
    let tag = hints(1_000.0).unwrap();
    reply(&tag, Some(success(json!(["up"]))), 1_000.0);
    assert_eq!(hints(600_999.0), None);
    // Ten minutes on, hints refresh them; a failed refresh keeps the old values.
    let tag = hints(601_000.0).unwrap();
    assert_eq!(metrics(), json!(["up"]));
    reply(&tag, None, 601_000.0);
    assert_eq!(metrics(), json!(["up"]));
    assert_eq!(hints(602_000.0), None);
}

#[test]
fn metric_names_bring_their_types_and_a_server_without_metadata_still_discovers() {
    let mut discovery = Discovery::default();
    let check = check("{{.base}}", "", "");
    let commands = discovery.hints(&check, check.text.len());
    let commands = resolve(&mut discovery, &commands, "");
    let tag = request(&commands).0.to_owned();
    let commands = discovery
        .reply(&tag, Ok(success(json!(["requests_total", "up"]))))
        .unwrap();
    let (meta, sent) = request(&commands);
    let url = url::Url::parse(sent["url"].as_str().unwrap()).unwrap();
    assert_eq!(url.path(), "/proxy/prom/api/v1/metadata");
    let parameters: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
    assert_eq!(parameters["limit_per_metric"], "1");
    let commands = discovery
        .reply(
            meta,
            Ok(success(json!({
                "requests_total": [{"type": "counter", "help": "Requests.", "unit": ""}]
            }))),
        )
        .unwrap();
    assert!(commands.iter().any(|c| c.name == "editor.relight"));
    let details: Vec<_> = language::complete(&check, check.text.len(), &discovery)
        .items
        .into_iter()
        .filter(|i| i.kind == "variable")
        .map(|i| (i.label, i.detail))
        .collect();
    assert_eq!(
        details,
        [
            ("requests_total".into(), "Metric · counter".into()),
            ("up".into(), "Metric".into())
        ]
    );
    assert_eq!(
        discovery.schema_of(&check).unwrap()["types"]["requests_total"],
        json!({"type": "counter", "help": "Requests.", "unit": ""})
    );

    let mut discovery = Discovery::default();
    let commands = discovery.manual(&check);
    let commands = resolve(&mut discovery, &commands, "");
    let tag = request(&commands).0.to_owned();
    let commands = discovery.reply(&tag, Ok(success(json!(["up"])))).unwrap();
    let meta = request(&commands).0.to_owned();
    let failed = Failure::new("not found");
    assert!(discovery.reply(&meta, Err(failed)).unwrap().is_empty());
    assert_eq!(
        discovery.schema_of(&check).unwrap()["metrics"],
        json!(["up"])
    );
}

#[test]
fn metric_types_order_and_wrap_the_metrics_a_function_takes() {
    let mut discovery = Discovery::default();
    let check = check("{{.base}}", "", "");
    let commands = discovery.hints(&check, check.text.len());
    let commands = resolve(&mut discovery, &commands, "");
    let tag = request(&commands).0.to_owned();
    let names = json!(["memory_bytes", "latency_seconds_bucket", "requests_total"]);
    let commands = discovery.reply(&tag, Ok(success(names))).unwrap();
    let meta = request(&commands).0.to_owned();
    discovery
        .reply(
            &meta,
            Ok(success(json!({
                "memory_bytes": [{"type": "gauge", "help": "", "unit": ""}],
                "latency_seconds": [{"type": "histogram", "help": "", "unit": ""}],
                "requests_total": [{"type": "counter", "help": "", "unit": ""}]
            }))),
        )
        .unwrap();
    let complete = |query: &str| {
        let check = query_check(query);
        language::complete(&check, check.text.len(), &discovery).items
    };
    let inside = complete("rate(");
    assert_eq!(inside[0].insert, "requests_total[$0]");
    assert!(inside[0].next);
    assert_eq!(complete("avg_over_time(")[0].insert, "memory_bytes[$0]");
    assert_eq!(
        complete("histogram_quantile(0.9, ")[0].insert,
        "sum by (le) (rate(latency_seconds_bucket[$0]))"
    );
    let typed: Vec<_> = complete("req").into_iter().map(|i| i.insert).collect();
    assert_eq!(typed[..2], ["requests_total", "rate(requests_total[$0])"]);
}

fn query_check(query: &str) -> Check {
    check("{{.base}}", "", query)
}
