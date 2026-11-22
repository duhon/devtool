#![allow(dead_code)]

use serde_json::json;
use services_module_kubernetes::{action, config, editor, payload, resources, target};

#[test]
fn a_target_needs_a_context_and_a_namespace() {
    let target = target::parse("k8s://arn:aws:eks:eu/prod:cluster/shop").unwrap();
    assert_eq!(target.context, "arn:aws:eks:eu/prod:cluster");
    assert_eq!(target.namespace, "shop");
    for bad in [
        "",
        "http://dev/shop",
        "k8s://dev",
        "k8s://dev/",
        "k8s:///shop",
    ] {
        assert!(target::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn an_action_has_one_name_in_either_form() {
    let action = action::parse("get deploy/feed").unwrap();
    assert_eq!(
        (action.resource.as_str(), action.name.as_deref()),
        ("deploy", Some("feed"))
    );
    assert_eq!(
        action::parse("get deploy feed").unwrap().name.as_deref(),
        Some("feed")
    );
    assert!(action::parse("get deploy a b").is_err());
    assert!(action::parse("describe deploy").is_err());
    let can = action::parse("auth can-i create pods").unwrap();
    assert_eq!(
        (can.verb.as_str(), can.args[0].as_str()),
        ("auth can-i", "create")
    );
    let label = action::parse("label pod/feed tier=web old-").unwrap();
    assert_eq!(label.args, ["tier=web", "old-"]);
    assert_eq!(
        action::parse("rollout undo deploy/feed").unwrap().verb,
        "rollout undo"
    );
    assert!(action::parse("rollout nope deploy/feed").is_err());
}

#[test]
fn metadata_is_checked_for_the_action_and_options_have_defaults() {
    let config = config::parse("Tail: 20\nSince: 5m\nContainer: app", "").unwrap();
    config.validate("logs").unwrap();
    assert!(config.validate("get").is_err());
    assert!(config::parse("Nope: 1", "").is_err());
    assert!(config::parse("Since: later", "").is_err());
    let options = config::parse("", "").unwrap().options;
    assert_eq!((options.count, options.offset), (100, 0));
    assert!(config::parse("", "Format: xml").is_err());
    assert!(config::parse("", "Grep: (").is_err());
}

#[test]
fn a_payload_is_yaml_or_json_of_objects_for_the_target_namespace() {
    let yaml = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: a\n---\napiVersion: v1\nkind: Secret\nmetadata:\n  name: b\n";
    assert_eq!(payload::objects(yaml, "shop").unwrap().len(), 2);
    let json = r#"{"apiVersion":"v1","kind":"List","items":[{"apiVersion":"v1","kind":"Pod","metadata":{"name":"p"}}]}"#;
    assert_eq!(payload::objects(json, "shop").unwrap().len(), 1);
    assert!(payload::objects("kind: Pod", "shop").is_err());
    let other = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: p\n  namespace: other\n";
    assert!(payload::objects(other, "shop").is_err());
}

#[test]
fn resources_resolve_by_every_name_and_build_paths() {
    let list = json!({"kind": "APIResourceList", "groupVersion": "apps/v1", "resources": [
        {"name": "deployments", "singularName": "deployment", "namespaced": true,
            "kind": "Deployment", "shortNames": ["deploy"], "verbs": ["get"]},
        {"name": "deployments/scale", "namespaced": true, "kind": "Scale", "verbs": ["patch"]}]});
    let catalog = resources::parse_list(&list);
    for name in [
        "deployments",
        "Deployment",
        "deploy",
        "deployments.apps",
        "DEPLOY",
    ] {
        let found = resources::resolve(&catalog, name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(found.kind, "Deployment");
    }
    let deployment = &catalog[0];
    assert!(deployment.has_subresource("scale"));
    assert_eq!(
        deployment.subresource_path("shop", "feed", "scale"),
        "/apis/apps/v1/namespaces/shop/deployments/feed/scale"
    );
    assert_eq!(deployment.collection_path(""), "/apis/apps/v1/deployments");
}

#[test]
fn path_segments_are_percent_encoded() {
    let catalog = resources::parse_list(&json!({
        "groupVersion": "v1",
        "resources": [{"name": "pods", "kind": "Pod", "namespaced": true}]
    }));
    let pods = &catalog[0];
    assert_eq!(
        pods.object_path("a/b", "x?y#z %/.."),
        "/api/v1/namespaces/a%2Fb/pods/x%3Fy%23z%20%25%2F.."
    );
    assert_eq!(
        pods.subresource_path("shop", "system:node.a", "log"),
        "/api/v1/namespaces/shop/pods/system:node.a/log"
    );
}

#[test]
fn namespaces_and_names_that_change_a_path_are_refused() {
    for bad in [
        "k8s://dev/a?b",
        "k8s://dev/Shop",
        "k8s://dev/-a",
        "k8s://dev/a.b",
    ] {
        assert!(target::parse(bad).is_err(), "{bad}");
    }
    for bad in [
        "get pods/..",
        "get pods/a?watch=1",
        "get pods/a#b",
        "get pods/a%2Fb",
    ] {
        assert!(action::parse(bad).is_err(), "{bad}");
    }
    assert!(action::parse("get pods/system:node.a").is_ok());
    let bad = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: a?b\n";
    assert!(payload::objects(bad, "shop").is_err());
}

#[test]
fn the_editor_reads_complete_actions_like_the_run() {
    let catalog = resources::parse_list(&json!({
        "groupVersion": "v1",
        "resources": [
            {"name": "pods", "kind": "Pod", "namespaced": true},
            {"name": "nodes", "kind": "Node"},
        ]
    }));
    let cases = [
        ("get pods web", "get", "pods", Some("web")),
        ("get pods/web", "get", "pods", Some("web")),
        ("describe pods/x", "describe", "pods", Some("x")),
        (
            "rollout status deploy/x",
            "rollout status",
            "deploy",
            Some("x"),
        ),
        ("label pods/web a=b", "label", "pods", Some("web")),
        ("label pods web a=b", "label", "pods", Some("web")),
        ("auth can-i get pods", "auth can-i", "pods", None),
        ("apply", "apply", "", None),
        ("create", "create", "", None),
        ("create pods", "create", "pods", None),
        ("top pods", "top", "pods", None),
        ("api-resources", "api-resources", "", None),
        ("events deploy/x", "events", "deploy", Some("x")),
    ];
    for (text, verb, resource, name) in cases {
        let run = action::parse(text).unwrap();
        let read = action::shape(text);
        let read_resource = read.resource.map_or("", |(_, text)| text);
        let read_name = read.name.map(|(_, text)| text);
        assert_eq!(
            (
                run.verb.as_str(),
                run.resource.as_str(),
                run.name.as_deref()
            ),
            (verb, resource, name),
            "run: {text}"
        );
        assert_eq!(
            (read.verb.as_str(), read_resource, read_name),
            (verb, resource, name),
            "editor: {text}"
        );
    }
    let found = editor::action_resource("get pods web", &catalog).unwrap();
    assert_eq!(
        (found.0.plural.as_str(), found.1.as_deref()),
        ("pods", Some("web"))
    );
    // Typing in progress stays lenient.
    let partial = action::shape("get pods/");
    assert!(partial.slash && partial.resource.is_some() && partial.name.is_none());
    assert!(action::shape("rollout").verb_words == 1);
}
