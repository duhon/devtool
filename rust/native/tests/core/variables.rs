use serde_json::{Value, json};
use services_testkit::DataDir;

#[tokio::test]
async fn session_variables_are_request_local_and_never_replace_stored_ones() {
    let data = DataDir::new();
    let runtime = data.runtime();
    runtime
        .variable_store
        .dispatch(&json!({"op":"save","name":"var","variables":{"TOKEN":"legacy"}}))
        .unwrap();
    runtime
        .dictionary_store
        .dispatch(&json!({"op":"save","name":"shared","variables":{"REF":"{{var:TOKEN}}"}}))
        .unwrap();
    let a = json!({"TOKEN":"session-a","API_TOKEN":"session-secret-a","NESTED":{"API_TOKEN":"nested-session-secret"}});
    let b = json!({"TOKEN":"session-b"});
    for (variables, expected) in [(&a, "session-a"), (&b, "session-b"), (&a, "session-a")] {
        let view = runtime
            .variable_store
            .session_dispatch(&json!({"op":"list"}), variables)
            .unwrap();
        assert_eq!(view["profiles"]["var"], *variables);
        assert_eq!(
            view["context"]["resolvedProfiles"]["shared"]["REF"],
            expected
        );
        let params = runtime.registry.resolve("terminal",&json!({
            "endpoint":"sh -c","command":"printf '{{var:TOKEN}} {{shared:REF}}'","__consoleVariables":variables
        })).unwrap();
        assert_eq!(params["command"], format!("printf '{expected} {expected}'"));
        assert!(params.get("__consoleVariables").is_none());
    }
    assert_eq!(
        runtime
            .variable_store
            .session_dispatch(
                &json!({"op":"save","name":"var","variables":{"TOKEN":"new"}}),
                &a
            )
            .unwrap()["profiles"]["var"]["TOKEN"],
        "new"
    );
    assert_eq!(
        runtime.variable_store.list().unwrap()["profiles"]["var"]["TOKEN"],
        "legacy"
    );
    assert_eq!(
        runtime.dictionary_store.list().unwrap()["profiles"]["shared"]["REF"],
        "{{var:TOKEN}}"
    );
    assert!(
        runtime
            .agent_session_sensitive_values(&json!({"__consoleVariables":a}))
            .unwrap()
            .contains(&"session-secret-a".to_owned())
    );
    assert!(
        runtime
            .agent_session_sensitive_values(&json!({"__consoleVariables":a}))
            .unwrap()
            .contains(&"nested-session-secret".to_owned())
    );
    assert!(
        runtime
            .registry
            .resolve(
                "terminal",
                &json!({"command":"{{var:TOKEN}}","__consoleVariables":{}})
            )
            .is_err()
    );
    assert!(
        runtime
            .variable_store
            .session_dispatch(&json!({"op":"list"}), &json!([]))
            .is_err()
    );
    let boot = runtime
        .dispatch(&json!({"service":"sessions","op":"bootstrap"}))
        .await;
    assert_eq!(boot["result"]["variables"]["TOKEN"], "legacy");
    assert!(boot["result"]["hints"]["rest"].is_string());
}

#[test]
fn nested_json_keeps_keys_and_types_and_reports_resolution_errors() {
    let data = DataDir::new();
    let rt = data.runtime();
    let sku = "SKU-\"blue\"\\path\nnext";
    rt.variable_store
        .dispatch(&json!({"op":"save","name":"var","variables":{
            "sku":sku,"enabled":true,"count":42
        }}))
        .unwrap();
    let input = json!({
        "skus":["{{var:sku}}"],
        "filter":{"items":[{"sku":"prefix-{{:sku}}","nested":[["{{var:sku}}"]]}]},
        "enabled":true,"count":42,"nothing":null,
        "booleanText":"{{var:enabled}}","numberText":"{{var:count}}",
        "{{var:sku}}":"literal key"
    });
    let expected = json!({
        "skus":[sku],
        "filter":{"items":[{"sku":format!("prefix-{sku}"),"nested":[[sku]]}]},
        "enabled":true,"count":42,"nothing":null,
        "booleanText":"true","numberText":"42",
        "{{var:sku}}":"literal key"
    });
    for (service, field) in [
        ("graphql", "variables"),
        ("rest", "headers"),
        ("grpc", "headers"),
        ("grpc", "requestMessage"),
        ("splunk", "headers"),
        ("terminal", "env"),
    ] {
        let mut params = json!({});
        params[field] = input.clone();
        assert_eq!(
            rt.registry.resolve(service, &params).unwrap()[field],
            expected,
            "{service}.{field}"
        );
        if field != "requestMessage" {
            params[field] = json!(input.to_string());
            let resolved = rt.registry.resolve(service, &params).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(resolved[field].as_str().unwrap()).unwrap(),
                expected,
                "{service}.{field} JSON string"
            );
        }
    }
    assert_eq!(
        rt.registry
            .resolve("graphql", &json!({"variables":""}))
            .unwrap()["variables"],
        ""
    );
    assert_eq!(
        rt.registry
            .resolve("graphql", &json!({"variables":null}))
            .unwrap()["variables"],
        Value::Null
    );
    assert_eq!(
        rt.registry
            .resolve("splunk", &json!({"count":"{{var:count}}"}))
            .unwrap()["count"],
        "{{var:count}}"
    );
    let error = rt
        .registry
        .resolve(
            "graphql",
            &json!({"variables":{"items":[{"sku":"{{var:missing}}"}]}}),
        )
        .unwrap_err();
    assert!(error.message.contains("missing") && error.message.contains("var"));
    let error = rt
        .registry
        .resolve(
            "graphql",
            &json!({"variables":{"items":[{"sku":"{{unknown:sku}}"}]}}),
        )
        .unwrap_err();
    assert!(error.message.contains("Unknown profile"));
    let error = rt
        .registry
        .resolve("graphql", &json!({"variables":"{\"skus\":["}))
        .unwrap_err();
    assert!(error.message.starts_with("variables must be valid JSON"));
}

#[test]
fn cycles_display_tolerantly_and_fall_back_to_globals() {
    let data = DataDir::new();
    let runtime = data.runtime();
    let vars = runtime.dictionary_store.clone();
    let rest = runtime.registry.params("rest").unwrap();
    vars.dispatch(&json!({"op":"save","name":"default","variables":{"value":"{{default:value}}"}}))
        .unwrap();
    vars.dispatch(
        &json!({"op":"save","name":"fallback","variables":{"value":"ok","indirect":"{{:value}}"}}),
    )
    .unwrap();
    let list = vars.list().unwrap();
    assert!(
        list["validation"]["default"]
            .as_str()
            .unwrap()
            .starts_with("Circular profile reference")
    );
    assert!(
        list["resolvedProfiles"]["default"]["value"]
            .as_str()
            .unwrap()
            .starts_with("⚠ ")
    );
    assert_eq!(list["resolvedProfiles"]["fallback"]["indirect"], "ok");
    assert!(
        vars.resolve_params(rest, &json!({"url":"literal"}))
            .is_err()
    );
    vars.dispatch(&json!({"op":"reorder","order":["fallback","default"]}))
        .unwrap();
    assert_eq!(
        vars.resolve_params(rest, &json!({"url":"{{:value}}"}))
            .unwrap()["url"],
        "ok"
    );
}

#[test]
fn dictionary_save_with_a_stale_base_keeps_another_windows_edit() {
    let data = DataDir::new();
    let dictionary = data.runtime().dictionary_store.clone();
    let save = |request: Value| dictionary.dispatch(&request);
    save(json!({"op":"save","name":"shared","variables":{"A":"1"},"base":null})).unwrap();
    assert!(
        save(json!({"op":"save","name":"shared","variables":{"A":"x"},"base":null}))
            .unwrap_err()
            .message
            .starts_with("CONFLICT: profile shared was created"),
        "Creating a profile that another window created must conflict"
    );
    save(json!({"op":"save","name":"shared","variables":{"A":"2"},"base":{"A":"1"}})).unwrap();
    let stale = save(json!({"op":"save","name":"shared","variables":{"A":"3"},"base":{"A":"1"}}));
    assert!(
        stale
            .unwrap_err()
            .message
            .contains("changed in another window")
    );
    assert_eq!(dictionary.list().unwrap()["profiles"]["shared"]["A"], "2");
    save(json!({"op":"save","name":"shared","variables":{"A":"2"},"base":{"A":"1"}}))
        .expect("A draft that already matches the saved profile is not a conflict");
    save(json!({"op":"save","name":"shared","variables":{"A":"4"}}))
        .expect("Saves without a base overwrite, as the CLI and Agent expect");
    assert_eq!(dictionary.list().unwrap()["profiles"]["shared"]["A"], "4");
}
