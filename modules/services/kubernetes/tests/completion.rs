#![allow(dead_code)]
mod common;
mod support;

use support::Cluster;

const DEFAULT: &str = "#target\nk8s://dev/default\n#action\n";

fn has(labels: &[String], wanted: &[&str]) {
    for label in wanted {
        assert!(labels.iter().any(|l| l == label), "{label} in {labels:?}");
    }
}

fn lacks(labels: &[String], unwanted: &[&str]) {
    for label in unwanted {
        assert!(!labels.iter().any(|l| l == label), "{label} in {labels:?}");
    }
}

#[test]
fn the_target_offers_the_scheme_then_contexts_then_namespaces() {
    let mut cluster = Cluster::default();
    has(&cluster.offered("#target\n|"), &["k8s://"]);
    let contexts = cluster.offered("#target\nk8s://|");
    has(&contexts, &["dev/", "prod/"]);
    let found = cluster.module.complete("#target\nk8s://", 14);
    assert!(found.items.iter().all(|item| item.next));
    has(
        &cluster.offered("#target\nk8s://dev/|"),
        &["default", "kube-system"],
    );
    assert_eq!(cluster.labels("#target\nk8s://dev/kube|"), ["kube-system"]);
}

#[test]
fn the_action_offers_verbs_resources_and_names() {
    let mut cluster = Cluster::default();
    let verbs = cluster.offered(&format!("{DEFAULT}|"));
    has(
        &verbs,
        &["get ", "logs ", "rollout status ", "auth can-i ", "apply"],
    );
    has(
        &cluster.labels(&format!("{DEFAULT}rollout |")),
        &["status ", "undo "],
    );
    let resources = cluster.offered(&format!("{DEFAULT}get |"));
    has(&resources, &["pods", "po", "deployments", "deploy", "svc"]);
    let logs = cluster.labels(&format!("{DEFAULT}logs |"));
    has(&logs, &["pods", "deployments"]);
    lacks(&logs, &["services"]);
    has(
        &cluster.labels(&format!("{DEFAULT}scale |")),
        &["deployments"],
    );
    lacks(&cluster.labels(&format!("{DEFAULT}scale |")), &["pods"]);
    has(
        &cluster.labels(&format!("{DEFAULT}top |")),
        &["pods", "nodes"],
    );
    has(
        &cluster.offered(&format!("{DEFAULT}get pods/|")),
        &["web-1", "web-2", "db-1"],
    );
    assert_eq!(cluster.labels(&format!("{DEFAULT}get po/db|")), ["db-1"]);
}

#[test]
fn metadata_keys_are_those_of_the_action_and_values_come_from_discovery() {
    let mut cluster = Cluster::default();
    let logs = cluster.offered(&format!("{DEFAULT}logs pods/web-1\n#metadata\n|"));
    has(&logs, &["Container: ", "Tail: ", "Selector: "]);
    lacks(
        &logs,
        &["Replicas: ", "Patch-Type: ", "Repeat: ", "Timeout: "],
    );
    has(
        &cluster.labels(&format!("{DEFAULT}scale deployments/web\n#metadata\n|")),
        &["Replicas: ", "Dry-Run: "],
    );
    let text = format!("{DEFAULT}logs pods/web-1\n#metadata\nContainer: |");
    has(&cluster.offered(&text), &["app", "sidecar"]);
    let text = format!("{DEFAULT}get pods\n#metadata\nSelector: |");
    has(&cluster.offered(&text), &["app=", "tier="]);
    let text = format!("{DEFAULT}get pods\n#metadata\nSelector: tier=front,app=|");
    assert_eq!(cluster.offered(&text), ["db", "web"]);
    let text = format!("{DEFAULT}patch pods/web-1\n#metadata\nPatch-Type: |");
    has(
        &cluster.labels(&text),
        &["merge", "json", "strategic", "apply"],
    );
    // Keys already written are not offered again; a blank line then offers the next section.
    let text = format!("{DEFAULT}logs pods/web-1\n#metadata\nTail: 10\n|");
    let labels = cluster.labels(&text);
    lacks(&labels, &["Tail: "]);
    has(&labels, &["#options", "#payload"]);
}

#[test]
fn options_are_the_modules_and_never_the_consoles() {
    let mut cluster = Cluster::default();
    let keys = cluster.labels("#options\n|");
    has(
        &keys,
        &["Format: ", "Columns: ", "Table-Sort: ", "Logs: ", "Grep: "],
    );
    lacks(&keys, &["Repeat: ", "Repeat-Until: ", "Timeout: "]);
    has(
        &cluster.labels("#options\nFormat: |"),
        &["raw", "table", "json", "yaml"],
    );
    has(&cluster.labels("#options\nLogs: |"), &["raw", "json"]);
}

const APPLY: &str = "#target\nk8s://dev/default\n#action\napply\n#payload\n";

#[test]
fn the_payload_completes_by_the_schema_at_the_yaml_path() {
    let mut cluster = Cluster::default();
    let top = cluster.offered(&format!("{APPLY}|"));
    has(&top, &["apiVersion: ", "kind: ", "metadata: ", "spec: "]);
    has(
        &cluster.labels(&format!("{APPLY}apiVersion: |")),
        &["v1", "apps/v1"],
    );
    has(
        &cluster.labels(&format!("{APPLY}apiVersion: apps/v1\nkind: |")),
        &["Deployment", "StatefulSet"],
    );
    lacks(
        &cluster.labels(&format!("{APPLY}apiVersion: apps/v1\nkind: |")),
        &["Pod"],
    );
    let head = format!("{APPLY}apiVersion: apps/v1\nkind: Deployment\n");
    let text = format!("{head}spec:\n  |");
    let fields = cluster.offered(&text);
    has(&fields, &["selector:", "replicas: ", "strategy:"]);
    // A key already in the mapping is not offered again.
    let text = format!("{head}spec:\n  replicas: 2\n  |");
    lacks(&cluster.labels(&text), &["replicas: "]);
    let text = format!("{head}spec:\n  strategy:\n    type: |");
    assert_eq!(cluster.labels(&text), ["Recreate", "RollingUpdate"]);
    let text = format!("{head}metadata:\n  namespace: |");
    has(&cluster.labels(&text), &["default", "kube-system"]);
}

#[test]
fn json_payloads_complete_too_and_other_verbs_have_no_payload_hints() {
    let mut cluster = Cluster::default();
    let head = format!("{APPLY}{{\"apiVersion\": \"apps/v1\", \"kind\": \"Deployment\", ");
    let fields = cluster.offered(&format!("{head}\"|"));
    has(&fields, &["\"spec\": ", "\"metadata\": "]);
    lacks(&fields, &["\"kind\": "]);
    let get = "#target\nk8s://dev/default\n#action\nget pods\n#payload\n|";
    assert!(cluster.offered(get).is_empty());
}

#[test]
fn marks_show_what_discovery_knows_is_wrong() {
    let mut cluster = Cluster::default();
    let text = "#target\nk8s://qa/default\n#action\nget pods";
    cluster.hints(&format!("{text}|"));
    assert_eq!(cluster.marked(text), ["qa"]);
    let text = "#target\nk8s://dev/nowhere\n#action\nget pods/zzz";
    cluster.hints(&format!("{text}|"));
    assert_eq!(cluster.marked(text), ["nowhere"]);
    let text = "#target\nk8s://dev/default\n#action\nget widgets";
    cluster.hints(&format!("{text}|"));
    assert_eq!(cluster.marked(text), ["widgets"]);
    let text = "#target\nk8s://dev/default\n#action\nget pods/zzz";
    cluster.hints(&format!("{text}|"));
    assert_eq!(cluster.marked(text), ["zzz"]);
    let text = "#target\nk8s://dev/default\n#action\nget pods/web-1";
    assert!(cluster.marked(text).is_empty());
    let text = "#target\nk8s://dev/default\n#action\nget pods\n#metadata\nReplicas: 2\nSelector: a=b\nTimeout: 5";
    assert_eq!(cluster.marked(text), ["Replicas"]);
    let text = "#target\nk8s://dev/default\n#action\nlogs pods/web-1\n#metadata\nContainer: nope";
    cluster.hints(&format!("{text}|"));
    assert_eq!(cluster.marked(text), ["nope"]);
}

#[test]
fn payload_marks_a_foreign_namespace_and_unknown_fields() {
    let mut cluster = Cluster::default();
    let text = format!(
        "{APPLY}apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n  namespace: other\nspec:\n  replcas: 2\n  replicas: 2"
    );
    cluster.hints(&format!("{text}|"));
    assert_eq!(cluster.marked(&text), ["other", "replcas"]);
}
