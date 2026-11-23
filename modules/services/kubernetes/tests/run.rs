#![allow(dead_code)]
mod common;

use common::*;
use serde_json::{Value, json};
use services_sdk::Cmd;

fn reply_of(path: &str, query: &Value, request: &Value) -> Value {
    let status = |code: u16, body: Value| json!({"status": code, "body": body});
    let name = |n: &str| json!({"metadata": {"name": n}});
    let objects = query["includeObject"] == "Object";
    let pod = |n: &str, version: &str| {
        if objects {
            json!({"metadata": {"name": n}, "spec": {"containers": [{"name": "app",
                "env": [{"name": "APP_VERSION", "value": version}]}]}})
        } else {
            json!({"metadata": {"name": n}})
        }
    };
    match path {
        "/api" => status(200, json!({"kind": "APIVersions", "versions": ["v1"]})),
        "/apis" => status(
            200,
            json!({"kind": "APIGroupList", "groups": [{"name": "apps",
                "preferredVersion": {"groupVersion": "apps/v1", "version": "v1"}}]}),
        ),
        "/api/v1" => status(
            200,
            json!({"kind": "APIResourceList", "groupVersion": "v1", "resources": [
                {"name": "pods", "singularName": "pod", "namespaced": true, "kind": "Pod",
                    "shortNames": ["po"], "verbs": ["get", "list"]},
                {"name": "events", "singularName": "event", "namespaced": true, "kind": "Event",
                    "verbs": ["get", "list"]},
                {"name": "pods/log", "namespaced": true, "kind": "Pod", "verbs": ["get"]}]}),
        ),
        "/apis/apps/v1" => status(
            200,
            json!({"kind": "APIResourceList", "groupVersion": "apps/v1", "resources": [
                {"name": "deployments", "singularName": "deployment", "namespaced": true,
                    "kind": "Deployment", "shortNames": ["deploy"], "verbs": ["get", "list", "patch"]},
                {"name": "deployments/scale", "namespaced": true, "kind": "Scale", "verbs": ["patch"]},
                {"name": "replicasets", "singularName": "replicaset", "namespaced": true,
                    "kind": "ReplicaSet", "verbs": ["get", "list"]}]}),
        ),
        "/api/v1/namespaces/shop/pods" if query["labelSelector"].is_string() => status(
            200,
            json!({"items": [
                {"metadata": {"name": "feed-1"}, "spec": {"containers": [{"name": "app"}]}},
                {"metadata": {"name": "feed-2"}, "spec": {"containers": [{"name": "app"}]}}]}),
        ),
        "/api/v1/namespaces/shop/pods" => status(
            200,
            json!({"kind": "Table", "columnDefinitions": [
                {"name": "Name", "type": "string", "priority": 0},
                {"name": "Status", "type": "string", "priority": 0},
                {"name": "Node", "type": "string", "priority": 1}],
                "rows": [{"cells": ["feed-1", "Running", "n1"], "object": pod("feed-1", "1.42.0")},
                    {"cells": ["web", "Pending", "n2"], "object": pod("web", "2.0.0")}]}),
        ),
        "/api/v1/namespaces/shop/pods/missing" => json!({"status": 404, "body": {},
            "error": {"reason": "NotFound", "code": 404,
                "message": "pods \"missing\" not found"}}),
        "/apis/apps/v1/namespaces/shop/deployments/feed" if request["method"] == "GET" => status(
            200,
            json!({"kind": "Deployment", "metadata": {"name": "feed"},
                "spec": {"selector": {"matchLabels": {"app": "feed"}}}}),
        ),
        "/api/v1/namespaces/shop/pods/feed-1/log" => status(200, json!("one\ntwo")),
        "/api/v1/namespaces/shop/pods/feed-2/log" => status(200, json!("three")),
        "/apis/apps/v1/namespaces/shop/deployments/feed/scale" => status(200, name("feed")),
        "/apis/apps/v1/namespaces/shop/deployments/feed" => status(200, name("feed")),
        other => json!({"status": 404, "body": {}, "error": {"reason": "NotFound",
            "message": format!("no fake for {other}"), "code": 404}}),
    }
}

/// Runs `text` against the fake cluster; returns the final cmd and the driver requests made.
fn serve(text: &str) -> (Cmd, Vec<Value>) {
    serve_shaped(text, None)
}

/// Like [`serve`], for a run with a `Transform` that turns the data the run hands into
/// what `shape` gives back.
fn serve_shaped(text: &str, shape: Option<&dyn Fn(Value) -> Value>) -> (Cmd, Vec<Value>) {
    let mut module = Harness::default();
    module.transform = shape.is_some();
    let mut pending = module.run(text);
    let mut requests = Vec::new();
    for _ in 0..40 {
        let mut next = Vec::new();
        for cmd in pending {
            match cmd.name.as_str() {
                "vars.resolve" => {
                    let text = cmd.args()["text"].clone();
                    next.extend(module.reply(&cmd.tag, text));
                }
                "driver.call" => {
                    let request = cmd.args()["request"].clone();
                    requests.push(request.clone());
                    let path = request["path"].as_str().unwrap_or_default();
                    let reply = reply_of(path, &json!(request["query"]), &request);
                    next.extend(module.reply(&cmd.tag, reply));
                }
                "run.data" => {
                    let shape = shape.expect("a Transform");
                    let data = shape(cmd.args()["data"].clone());
                    next.extend(module.reply(&format!("{}:data", cmd.tag), data));
                }
                "run.result" => return (cmd, requests),
                _ => {}
            }
        }
        pending = next;
    }
    panic!("the run did not finish: {requests:?}");
}

fn check(action: &str, metadata: &str, options: &str) -> String {
    format!(
        "#target\nk8s://dev/shop\n#action\n{action}\n#metadata\n{metadata}\n#options\n{options}"
    )
}

fn body(cmd: &Cmd) -> String {
    cmd.args()["body"].as_str().unwrap_or_default().to_owned()
}

fn error(cmd: &Cmd) -> String {
    cmd.args()["error"].as_str().unwrap_or_default().to_owned()
}

#[test]
fn get_prints_the_server_table_like_kubectl() {
    let (result, requests) = serve(&check("get pods", "", ""));
    assert_eq!(
        body(&result),
        "NAME     STATUS\nfeed-1   Running\nweb      Pending"
    );
    let last = requests.last().unwrap();
    assert!(
        last["accept"].as_str().unwrap().contains("as=Table"),
        "{last}"
    );
}

#[test]
fn get_by_name_shows_the_servers_status_like_kubectl() {
    let (result, _) = serve(&check("get pods/missing", "", ""));
    assert_eq!(
        error(&result),
        "Error from server (NotFound): pods \"missing\" not found"
    );
}

#[test]
fn logs_of_a_workload_read_all_its_pods_and_prefix_the_lines() {
    let (result, requests) = serve(&check("logs deploy/feed", "Tail: 10", ""));
    assert_eq!(
        body(&result),
        "[feed-1/app] one\n[feed-1/app] two\n[feed-2/app] three"
    );
    let log = requests
        .iter()
        .find(|r| r["path"] == "/api/v1/namespaces/shop/pods/feed-1/log")
        .unwrap();
    assert_eq!(log["query"]["tailLines"], "10");
    assert_eq!(log["query"]["container"], "app");
}

#[test]
fn a_rollout_restart_patches_the_pod_template_annotation() {
    let (result, requests) = serve(&check("rollout restart deploy/feed", "", ""));
    assert_eq!(body(&result), "deployment.apps/feed restarted");
    let patch = requests.iter().find(|r| r["method"] == "PATCH").unwrap();
    assert_eq!(patch["contentType"], "application/merge-patch+json");
    assert!(
        patch["body"]
            .as_str()
            .unwrap()
            .contains("kubectl.kubernetes.io/restartedAt")
    );
}

#[test]
fn scale_uses_the_scale_subresource() {
    let (result, requests) = serve(&check("scale deploy/feed", "Replicas: 3", ""));
    assert_eq!(body(&result), "deployment.apps/feed scaled");
    let patch = requests.iter().find(|r| r["method"] == "PATCH").unwrap();
    assert_eq!(patch["body"], "{\"spec\":{\"replicas\":3}}");
}

#[test]
fn apply_sends_a_server_side_apply_per_object() {
    let text = format!(
        "{}\n#payload\napiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: feed\n",
        check("apply", "Force: true", "")
    );
    let (result, requests) = serve(&text);
    assert_eq!(body(&result), "deployment.apps/feed serverside-applied");
    let patch = requests.iter().find(|r| r["method"] == "PATCH").unwrap();
    assert_eq!(patch["contentType"], "application/apply-patch+yaml");
    assert_eq!(patch["query"]["fieldManager"], "services-devtools");
    assert_eq!(patch["query"]["force"], "true");
}

#[test]
fn a_payload_for_another_namespace_is_refused() {
    let text = format!(
        "{}\n#payload\napiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: feed\n  namespace: other\n",
        check("apply", "", "")
    );
    let (result, requests) = serve(&text);
    assert!(error(&result).contains("namespace"), "{}", error(&result));
    assert!(requests.is_empty());
}

#[test]
fn interactive_commands_are_refused_with_a_clear_error() {
    for action in ["exec pod/feed", "port-forward pod/feed", "edit deploy/feed"] {
        let (result, requests) = serve(&check(action, "", ""));
        assert!(
            error(&result).contains("not supported"),
            "{action}: {}",
            error(&result)
        );
        assert!(requests.is_empty());
    }
}

#[test]
fn metadata_keys_are_checked_against_the_action() {
    let (result, requests) = serve(&check("get pods", "Replicas: 2", ""));
    assert!(error(&result).contains("Replicas"), "{}", error(&result));
    assert!(requests.is_empty());
}

#[test]
fn the_table_format_renders_columns_and_greps_rows() {
    let (result, _) = serve(&check("get pods", "", "Format: table\nGrep: Pend"));
    assert_eq!(result.args()["language"], "kubernetes");
    assert!(body(&result).contains("web"), "{}", body(&result));
    assert!(!body(&result).contains("feed-1"), "{}", body(&result));
}

#[test]
fn the_table_format_shows_the_wide_columns_too() {
    let (result, _) = serve(&check("get pods", "", "Format: table"));
    assert!(
        body(&result).contains("| NAME   | STATUS  | NODE |"),
        "{}",
        body(&result)
    );
    let (result, _) = serve(&check("get pods", "", ""));
    assert!(!body(&result).contains("NODE"), "{}", body(&result));
}

#[test]
fn a_transform_reshapes_the_rows_logs_and_json_before_they_show() {
    let rows = |data: Value| {
        let pod = |n: &str, v: &str| {
            json!({"metadata": {"name": n}, "spec": {"containers": [
            {"name": "app", "env": [{"name": "APP_VERSION", "value": v}]}]}})
        };
        assert_eq!(
            data,
            json!([{"NAME": "feed-1", "STATUS": "Running", "object": pod("feed-1", "1.42.0")},
                {"NAME": "web", "STATUS": "Pending", "object": pod("web", "2.0.0")}])
        );
        json!([{"a": "one", "x": 1}, {"a": "two", "x": 22}])
    };
    let (result, _) = serve_shaped(&check("get pods", "", ""), Some(&rows));
    assert_eq!(body(&result), "a     x\none   1\ntwo   22");

    let lines = |data: Value| {
        assert_eq!(
            data,
            json!(["[feed-1/app] one", "[feed-1/app] two", "[feed-2/app] three"])
        );
        json!(["one", "two"])
    };
    let (result, _) = serve_shaped(
        &check("logs deploy/feed", "Tail: 10", "Grep: o"),
        Some(&lines),
    );
    assert_eq!(body(&result), "one\ntwo");

    let (result, _) = serve_shaped(
        &check("get pods", "", "Format: json"),
        Some(&|_| json!({"a": 1})),
    );
    assert_eq!(body(&result), "{\n  \"a\": 1\n}");
    let (result, _) = serve_shaped(&check("get pods", "", ""), Some(&|_| json!(1)));
    assert_eq!(body(&result), "1");
}

#[test]
fn a_transform_gets_the_whole_object_behind_each_row() {
    let version = |data: Value| {
        let rows = data.as_array().unwrap().iter().map(|row| {
            let env = &row["object"]["spec"]["containers"][0]["env"][0];
            json!({"NAME": row["NAME"], "VERSION": env["value"]})
        });
        Value::Array(rows.collect())
    };
    let (result, requests) = serve_shaped(&check("get pods", "", ""), Some(&version));
    let list = requests
        .iter()
        .find(|request| request["path"] == "/api/v1/namespaces/shop/pods")
        .expect("the pods list");
    assert_eq!(list["query"]["includeObject"], "Object");
    assert_eq!(
        body(&result),
        "NAME     VERSION\nfeed-1   1.42.0\nweb      2.0.0"
    );

    let (_, requests) = serve(&check("get pods", "", ""));
    let list = requests
        .iter()
        .find(|request| request["path"] == "/api/v1/namespaces/shop/pods")
        .expect("the pods list");
    assert!(list["query"]["includeObject"].is_null(), "{list}");
}
