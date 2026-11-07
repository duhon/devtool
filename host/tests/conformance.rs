//! CONTRACT: rules every module obeys, checked against every entry of the
//! test catalog (`services_testkit::MODULES`). A new module is held to them
//! as soon as it is listed there; its own behavior goes to `tests/modules/`.

use serde_json::{Value, json};
use services_testkit::{Host, Kind, MODULES, ModuleSpec, module, panels, services};

fn offline() -> impl Iterator<Item = &'static ModuleSpec> {
    MODULES.iter().filter(|module| !module.external)
}

/// The service modules the host serves, in their `order`; wasm panels
/// (Variables, History) carry no drivers and are not in the service catalog.
fn served(host: &Host) -> Vec<services_sdk::manifest::Manifest> {
    let mut modules: Vec<_> = host
        .runtime()
        .registry
        .wasm()
        .iter()
        .map(|module| module.manifest.clone())
        .filter(|manifest| manifest.kind == services_sdk::manifest::Kind::Service)
        .collect();
    modules.sort_by_key(|manifest| (manifest.order.unwrap_or(u32::MAX), manifest.id.clone()));
    modules
}

#[test]
fn every_manifest_is_valid_and_names_known_drivers() {
    let host = Host::new();
    for manifest in served(&host) {
        let id = &manifest.id;
        assert_eq!(manifest.abi, 1, "{id}");
        assert!(!manifest.drivers.is_empty(), "{id}: no drivers");
        for driver in manifest.drivers.keys() {
            assert!(
                ["http", "grpc", "mysql", "process", "tcp"].contains(&driver.as_str()),
                "{id}: unknown driver {driver}"
            );
        }
    }
}

/// The Agent's runtime silently skips a skill whose frontmatter is not valid YAML.
#[test]
fn every_module_brings_a_skill_the_agent_can_load() {
    use services_contracts::package::{skill_frontmatter, skill_name};
    let modules = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../modules/services");
    for module in services() {
        let file = modules.join(module.name).join("skill/SKILL.md");
        let text =
            std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let (name, _) =
            skill_frontmatter(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        assert_eq!(name, skill_name(module.name), "{}", file.display());
    }
}

#[tokio::test]
async fn a_service_without_saved_hints_gets_empty_hints() {
    let host = Host::new();
    for name in services().map(|module| module.name).chain(["unknown"]) {
        let reply = host
            .send(json!({"service":"hints","serviceName":name}))
            .await;
        assert_eq!(reply.ok()["text"], "", "{name}");
    }
}

#[tokio::test]
async fn the_console_reads_the_served_modules_from_the_host() {
    let host = Host::new();
    let reply = host.send(json!({"service":"modules"})).await;
    let wasm = reply.ok()["wasm"].as_array().unwrap().clone();
    let mut ids: Vec<_> = wasm
        .iter()
        .filter(|m| m["kind"] == "service")
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    ids.sort();
    let mut catalog: Vec<_> = services().map(|module| module.name).collect();
    catalog.sort();
    assert_eq!(ids, catalog);
    assert!(reply.ok().get("failed").is_none(), "{reply}");
}

#[test]
fn the_catalog_lists_exactly_the_served_modules() {
    let host = Host::new();
    let loaded: Vec<String> = served(&host).into_iter().map(|m| m.id).collect();
    let catalog: Vec<&str> = services().map(|module| module.name).collect();
    assert_eq!(catalog, loaded);
    for manifest in served(&host) {
        assert_eq!(module(&manifest.id).title, manifest.name, "{}", manifest.id);
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
    let anonymous = host.send(json!({"service":"artemis","params":{}})).await;
    assert_eq!(anonymous["id"], Value::Null, "{anonymous}");
}

/// Services run in the App: the host answers a service request like any unknown one.
#[tokio::test]
async fn requests_for_a_service_or_a_missing_module_fail_clearly() {
    let host = Host::new();
    for name in ["kafka", "artemis"] {
        let reply = host.send(json!({"service":name,"params":{}})).await;
        assert_eq!(reply.error(), format!("Unknown service \"{name}\""));
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
            "hints",
            json!({"op":"save","serviceName":"artemis","text":"#kept\n"}),
            ("get", json!({"serviceName":"artemis"})),
            "/text",
            json!("#kept\n"),
        ),
        (
            "sources",
            json!({"op":"save","sources":[{"name":"Team","kind":"folder","path":std::env::temp_dir()}]}),
            ("get", json!({})),
            "/sources/0/name",
            json!("Team"),
        ),
    ];
    // History keeps its own state (tests/core/history.rs); sessions and agent_sessions derive
    // theirs from other services' storage; templates only read until modules declare
    // sections (tests/modules/templates.rs).
    let mut covered = vec!["history", "sessions", "agent_sessions", "templates"];
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
