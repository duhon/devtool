#![allow(dead_code)]
mod common;
mod support;

use common::check;
use serde_json::{Value, json};
use services_sdk::wire::{answer, request};
use support::Cluster;

const TEXT: &str = "#target\nk8s://dev/default\n#action\nget pods/";

fn statuses(cmds: &[services_sdk::Cmd]) -> Vec<String> {
    cmds.iter()
        .filter(|cmd| cmd.name == "status.set" || cmd.name == "status")
        .map(|cmd| cmd.args().to_string())
        .collect()
}

#[test]
fn hints_discover_what_the_cursor_needs_once() {
    let mut cluster = Cluster::default();
    cluster.hints(&format!("{TEXT}|"));
    let paths = cluster.paths();
    for wanted in [
        "contexts",
        "/api/v1/namespaces",
        "/api",
        "/apis",
        "/api/v1/namespaces/default/pods",
    ] {
        assert_eq!(
            paths.iter().filter(|path| *path == wanted).count(),
            1,
            "{wanted} in {paths:?}"
        );
    }
    let before = cluster.requests.len();
    cluster.hints(&format!("{TEXT}|"));
    assert_eq!(cluster.requests.len(), before, "{:?}", cluster.paths());
    let list = &cluster.requests[paths.iter().position(|p| p.ends_with("/pods")).unwrap()];
    assert_eq!(list["query"]["limit"], "500");
    assert!(
        list["accept"]
            .as_str()
            .unwrap()
            .contains("PartialObjectMetadataList")
    );
}

#[test]
fn the_cursor_in_the_context_fetches_no_more_than_the_contexts() {
    let mut cluster = Cluster::default();
    cluster.hints("#target\nk8s://de|");
    assert_eq!(cluster.paths(), ["contexts"]);
}

#[test]
fn discover_drops_the_cache_of_the_context_and_asks_again() {
    let mut cluster = Cluster::default();
    cluster.hints(&format!("{TEXT}|"));
    let before = cluster.requests.len();
    let cmds = cluster.discover(TEXT);
    let again = &cluster.paths()[before..];
    for wanted in [
        "contexts",
        "/api",
        "/apis",
        "/api/v1/namespaces/default/pods",
    ] {
        assert!(
            again.iter().any(|path| path == wanted),
            "{wanted} in {again:?}"
        );
    }
    assert!(
        statuses(&cmds)
            .iter()
            .any(|status| status.contains("Discovery")),
        "{cmds:?}"
    );
}

#[test]
fn a_workspace_event_forgets_everything() {
    let mut cluster = Cluster::default();
    cluster.hints(&format!("{TEXT}|"));
    let cmds = cluster
        .module
        .send(services_sdk::wire::event::Kind::Workspace(
            Default::default(),
        ));
    assert!(
        cmds.iter().any(|cmd| cmd.name.contains("relight")),
        "{cmds:?}"
    );
    let before = cluster.requests.len();
    cluster.hints(&format!("{TEXT}|"));
    assert!(cluster.requests.len() > before);
}

#[test]
fn a_failed_discovery_is_reported_and_editing_goes_on() {
    let mut cluster = Cluster::default();
    let text = "#target\nk8s://dev/default\n#action\nget pods\n#metadata\n";
    let cmds = cluster.module.send(services_sdk::wire::event::Kind::Hints(
        services_sdk::wire::CompleteRequest {
            check: Some(check(text)),
            pos: text.len() as u32,
        },
    ));
    let mut rest = Vec::new();
    for cmd in cmds.into_iter().filter(|cmd| cmd.name == "driver.call") {
        let failure = json!({"status": 500, "body": {}, "error": {"message": "boom", "code": 500}});
        let reply = if cmd.args()["request"]["op"] == "contexts" {
            json!({"contexts": [{"name": "dev", "cluster": "dev", "server": "s"}]})
        } else {
            failure
        };
        rest.extend(cluster.module.reply(&cmd.tag, reply));
    }
    let rest = cluster.pump(rest);
    assert!(
        statuses(&rest)
            .iter()
            .any(|status| status.contains("WARNING") || status.contains("boom")),
        "{rest:?}"
    );
    let labels = cluster.labels(&format!("{text}|"));
    assert!(
        labels.iter().any(|label| label == "Selector: "),
        "{labels:?}"
    );
    assert!(cluster.marked(text).is_empty());
}

#[test]
fn variables_of_the_target_are_resolved_before_discovery() {
    let mut cluster = Cluster::default();
    let text = "#target\nk8s://{{cluster}}/default\n#action\nget pods\n";
    let cmds = cluster.hints(&format!("{text}|"));
    let resolve = cmds
        .iter()
        .find(|cmd| cmd.name == "vars.resolve" || cmd.name.contains("resolve"))
        .unwrap_or_else(|| panic!("{cmds:?}"));
    assert!(resolve.tag.starts_with("resolve:"));
    assert_eq!(cluster.paths(), ["contexts"]);
    let cmds = cluster
        .module
        .reply(&resolve.tag, Value::String("k8s://dev/default".into()));
    cluster.pump(cmds);
    assert!(
        cluster
            .paths()
            .iter()
            .any(|path| path == "/api/v1/namespaces/default/pods" || path == "/api")
    );
}

#[test]
fn the_schema_summarises_the_target_for_the_agent() {
    let mut cluster = Cluster::default();
    let text = "#target\nk8s://dev/default\n#action\nget pods";
    let answer::Kind::Schema(none) = cluster.module.call(request::Kind::Schema(check(text))) else {
        panic!("Expected a schema")
    };
    assert!(none.json.is_none());
    cluster.hints(&format!("{text}|"));
    let answer::Kind::Schema(found) = cluster.module.call(request::Kind::Schema(check(text)))
    else {
        panic!("Expected a schema")
    };
    let schema: Value = serde_json::from_slice(&found.json.unwrap()).unwrap();
    assert_eq!(schema["context"], "dev");
    assert_eq!(schema["contexts"], json!(["dev", "prod"]));
    assert_eq!(schema["namespaces"], json!(["default", "kube-system"]));
    let names: Vec<&str> = schema["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"pods") && names.contains(&"deployments"),
        "{names:?}"
    );
}

#[test]
fn stale_discovery_is_still_served_and_refreshed_by_the_next_hints() {
    let mut cluster = Cluster::default();
    let text = format!("{TEXT}|");
    assert!(cluster.offered(&text).iter().any(|label| label == "web-1"));
    cluster.module.now += 600_000.0;
    let (source, pos) = support::cursor(&text);
    let cmds = cluster.module.send(services_sdk::wire::event::Kind::Hints(
        services_sdk::wire::CompleteRequest {
            check: Some(check(&source)),
            pos: pos as u32,
        },
    ));
    let calls: Vec<_> = cmds
        .iter()
        .filter(|cmd| cmd.name == "driver.call")
        .collect();
    let paths: Vec<_> = calls
        .iter()
        .map(|cmd| cmd.args()["request"]["path"].clone())
        .collect();
    assert!(
        paths.contains(&json!("/api/v1/namespaces/default/pods")),
        "{paths:?}"
    );
    for call in calls {
        cluster.module.fail(&call.tag, "boom");
    }
    assert!(cluster.labels(&text).iter().any(|label| label == "web-1"));
}
