//! CONTRACT: rules every module obeys, checked against every entry of the
//! test catalog (`services_testkit::MODULES`). A new module is held to them
//! as soon as it is listed there; its own behavior goes to `tests/modules/`.

use serde_json::{Map, Value, json};
use services_testkit::{Host, Kind, MODULES, ModuleSpec, http, module, panels, services};
use tokio_util::sync::CancellationToken;

fn offline() -> impl Iterator<Item = &'static ModuleSpec> {
    MODULES.iter().filter(|module| !module.external)
}

/// Minimal service params pointing the service at `target`.
fn aimed(module: &ModuleSpec, target: &str) -> Value {
    Value::Object(Map::from_iter([(module.target.into(), json!(target))]))
}

fn cancelled() -> CancellationToken {
    let token = CancellationToken::new();
    token.cancel();
    token
}

#[test]
fn every_manifest_is_unambiguous_and_its_console_is_self_consistent() {
    use std::collections::BTreeSet;
    let host = Host::new();
    for manifest in host.runtime().registry.manifests().iter() {
        let id = &manifest.id;
        let names: BTreeSet<_> = manifest.params.iter().map(|p| &p.name).collect();
        assert_eq!(names.len(), manifest.params.len(), "{id}: duplicate param");
        let flags: Vec<_> = manifest
            .params
            .iter()
            .filter_map(|p| p.flag.as_ref())
            .collect();
        let unique: BTreeSet<_> = flags.iter().collect();
        assert_eq!(unique.len(), flags.len(), "{id}: duplicate flag");
        let console = &manifest.console;
        let sections: BTreeSet<_> = console.sections.iter().map(|s| &s.name).collect();
        assert_eq!(
            sections.len(),
            console.sections.len(),
            "{id}: duplicate section"
        );
        assert!(manifest.section("endpoint").is_some(), "{id}: no #endpoint");
        assert!(
            manifest.section(&console.payload).is_some(),
            "{id}: payload"
        );
        assert!(
            console.sections.iter().all(|s| !s.language.is_empty()),
            "{id}"
        );
        for section in &console.sections {
            let param = section.param.as_deref();
            assert!(
                param.is_some_and(|param| names.iter().any(|name| *name == param)),
                "{id}: #{} fills no declared param",
                section.name
            );
            assert!(
                !section.connection || console.discovery.is_some(),
                "{id}: #{} is a connection without discovery",
                section.name
            );
        }
        if let Some(discovery) = &console.discovery {
            assert!(
                manifest.section("endpoint").is_some_and(|s| s.connection),
                "{id}: discovery without a connection endpoint"
            );
            assert!(!discovery.schemes.is_empty(), "{id}: discovery schemes");
        }
        assert!(
            console.timeout.is_none_or(|seconds| seconds > 0),
            "{id}: timeout"
        );
    }
}

#[tokio::test]
async fn a_service_without_saved_hints_gets_its_manifest_defaults() {
    let host = Host::new();
    for manifest in host.runtime().registry.manifests().iter() {
        let reply = host
            .send(json!({"service":"hints","serviceName":manifest.id}))
            .await;
        assert_eq!(
            reply.ok()["text"],
            manifest.console.hints,
            "{}",
            manifest.id
        );
    }
    let defaults = |id: &str| {
        host.runtime()
            .registry
            .manifest(id)
            .unwrap()
            .console
            .hints
            .clone()
    };
    assert_eq!(defaults("mysql"), "#endpoint\nmysql://\n");
    assert_eq!(defaults("terminal"), "");
    for id in ["rest", "graphql", "grpc", "splunk"] {
        assert!(
            defaults(id).starts_with("#endpoint\nhttp://\nhttps://\n\n#headers\nAccept: */*\n"),
            "{id}"
        );
    }
    let reply = host
        .send(json!({"service":"hints","serviceName":"unknown"}))
        .await;
    assert_eq!(reply.ok()["text"], "");
}

#[tokio::test]
async fn the_console_reads_the_loaded_manifests_from_the_host() {
    let host = Host::new();
    let reply = host.send(json!({"service":"modules"})).await;
    let modules = reply.ok()["modules"].as_array().unwrap().clone();
    let expected = serde_json::to_value(host.runtime().registry.manifests().as_slice()).unwrap();
    assert_eq!(Value::Array(modules.clone()), expected);
    let ids: Vec<_> = modules.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        services().map(|module| module.name).collect::<Vec<_>>()
    );
    let rest = &modules[ids.iter().position(|id| *id == "rest").unwrap()];
    assert_eq!(rest["name"], "REST");
    assert_eq!(rest["execute"], true);
    assert_eq!(rest["console"]["payload"], "body");
    assert_eq!(rest["console"]["response"], "body-headers");
    assert_eq!(
        rest["console"]["sections"][1],
        json!({"name":"headers","shape":"pairs","language":"headers","param":"headers","connection":true})
    );
}

#[test]
fn the_catalog_lists_exactly_the_loaded_modules() {
    let host = Host::new();
    let loaded: Vec<&str> = host
        .runtime()
        .registry
        .manifests()
        .iter()
        .map(|manifest| manifest.id.as_str())
        .collect();
    let catalog: Vec<&str> = services().map(|module| module.name).collect();
    assert_eq!(catalog, loaded);
    // Undeclared params never reach a service: resolution drops them.
    for module in services() {
        let config = host.runtime().registry.manifest(module.name).unwrap();
        assert!(
            config
                .params
                .iter()
                .any(|param| param.name == module.target),
            "{}: target {:?} is not a declared param",
            module.name,
            module.target
        );
    }
    for module in MODULES {
        assert_eq!(
            MODULES
                .iter()
                .filter(|other| other.name == module.name)
                .count(),
            1,
            "{} is listed twice",
            module.name
        );
        assert_eq!(
            module.kind == Kind::Service,
            !module.target.is_empty(),
            "{}: services name their target, panels don't",
            module.name
        );
    }
}

#[tokio::test]
async fn every_request_gets_an_envelope_echoing_its_id() {
    let host = Host::new();
    for module in offline() {
        let mut requests = vec![json!({"service":module.name,"op":"unknown-operation"})];
        match module.kind {
            Kind::Service => {
                requests.push(json!({"service":module.name,"params":{}}));
                requests.push(json!({"type":"discover","service":module.name,"params":{}}));
            }
            Kind::Panel => {
                for (op, fields) in module.reads {
                    let mut request: Value = serde_json::from_str(fields).unwrap();
                    request["service"] = json!(module.name);
                    request["op"] = json!(op);
                    requests.push(request);
                }
            }
        }
        for (index, mut request) in requests.into_iter().enumerate() {
            request["id"] = json!(format!("{}-{index}", module.name));
            let reply = host.send(request.clone()).await;
            assert_eq!(reply["id"], request["id"], "{reply}");
            match reply["ok"].as_bool() {
                Some(true) => assert!(reply.get("result").is_some(), "{reply}"),
                Some(false) => assert!(!reply.error().is_empty(), "{reply}"),
                None => panic!("`ok` must be a boolean\n{reply}"),
            }
        }
    }
    let anonymous = host.call("rest", json!({})).await;
    assert_eq!(anonymous["id"], Value::Null, "{anonymous}");
}

#[tokio::test]
async fn requests_for_a_missing_module_fail_clearly() {
    let host = Host::new();
    let reply = host.call("opensearch", json!({})).await;
    assert_eq!(reply.error(), "Unknown service \"opensearch\"");
    let discovery = host.discover("opensearch", json!({})).await;
    assert_eq!(
        discovery.discovery_error().unwrap_or_default(),
        "Unknown service \"opensearch\""
    );
}

#[tokio::test]
async fn cancelled_requests_never_reach_a_service() {
    let host = Host::new();
    let (server, seen) = http::services().await;
    for module in services() {
        let params = aimed(module, &format!("{}/echo", server.url));
        let call = host
            .call_cancellable(module.name, params.clone(), cancelled())
            .await;
        assert_eq!(
            call.ok(),
            &json!({"cancelled":true,"phase":"before-dispatch","outcome":"Stopped"}),
            "{call}"
        );
        let discovery = host
            .send_cancellable(
                json!({"type":"discover","service":module.name,"params":params}),
                cancelled(),
            )
            .await;
        assert_eq!(discovery.ok(), &json!({"cancelled":true}), "{discovery}");
    }
    assert_eq!(*seen.lock().unwrap(), Vec::<Value>::new());
}

#[tokio::test]
async fn unresolved_variables_fail_before_the_service_runs() {
    let host = Host::new();
    let (server, seen) = http::services().await;
    for module in services() {
        let params = aimed(module, &format!("{}/echo?{{{{var:MISSING}}}}", server.url));
        let call = host.call(module.name, params.clone()).await;
        assert_eq!(
            call.error(),
            "Unknown variable \"MISSING\" in profile \"var\"",
            "{call}"
        );
        assert_eq!(call["debug"]["request"], params, "{call}");
        if module.discovery {
            let discovery = host.discover(module.name, params).await;
            assert_eq!(
                discovery.discovery_error().unwrap_or_default(),
                "Unknown variable \"MISSING\" in profile \"var\"",
                "{discovery}"
            );
        }
    }
    assert_eq!(*seen.lock().unwrap(), Vec::<Value>::new());
}

#[tokio::test]
async fn discovery_and_streaming_match_the_catalog() {
    let host = Host::new();
    for module in services() {
        let discovery = host.discover(module.name, json!({})).await;
        assert_eq!(
            discovery
                .discovery_error()
                .unwrap_or_default()
                .contains("doesn't support discovery"),
            !module.discovery,
            "{discovery}"
        );
        let call = host.call(module.name, json!({})).await;
        assert_eq!(
            call.is_ok() || !call.error().contains("is streaming-only"),
            !module.streaming,
            "{call}"
        );
        let streamed = host
            .stream(
                json!({"service":module.name,"params":{}}),
                CancellationToken::new(),
            )
            .await;
        let error = match &streamed.result {
            Ok(_) => "",
            Err(error) => error["error"].as_str().unwrap_or_default(),
        };
        assert_eq!(
            error.contains("is not streaming"),
            !module.streaming,
            "{}: {error}",
            module.name
        );
    }
}

#[tokio::test]
async fn panel_reads_succeed_on_empty_data() {
    let host = Host::new();
    for module in panels().filter(|module| !module.external) {
        assert!(!module.reads.is_empty(), "{} lists no reads", module.name);
        for (op, fields) in module.reads {
            let reply = host
                .op(module.name, op, serde_json::from_str(fields).unwrap())
                .await;
            reply.ok();
        }
    }
}

#[tokio::test]
async fn panel_state_survives_a_restart() {
    let host = Host::new();
    let cases = [
        (
            "vars",
            json!({"op":"save","name":"var","variables":{"TOKEN":"kept"}}),
            ("list", json!({})),
            "/profiles/var/TOKEN",
            json!("kept"),
        ),
        (
            "dictionary",
            json!({"op":"save","name":"shop","variables":{"HOST":"kept"}}),
            ("list", json!({})),
            "/profiles/shop/HOST",
            json!("kept"),
        ),
        (
            "templates",
            json!({"op":"save","serviceName":"rest","section":"body","text":"@kept\n{}","base":""}),
            ("get", json!({})),
            "/templates/rest/body",
            json!("@kept\n{}"),
        ),
        (
            "hints",
            json!({"op":"save","serviceName":"rest","text":"#kept\n"}),
            ("get", json!({"serviceName":"rest"})),
            "/text",
            json!("#kept\n"),
        ),
    ];
    // History keeps its own state (tests/core/history.rs); sessions derive theirs.
    let mut covered = vec!["history", "sessions"];
    for (panel, mut write, (op, fields), pointer, expected) in cases {
        module(panel);
        write["service"] = json!(panel);
        host.send(write).await.ok();
        let reply = host.reopen().op(panel, op, fields).await;
        assert_eq!(reply.ok().pointer(pointer), Some(&expected), "{reply}");
        covered.push(panel);
    }
    for module in panels().filter(|module| !module.external) {
        assert!(
            covered.contains(&module.name),
            "{} has no persistence case",
            module.name
        );
    }
}
