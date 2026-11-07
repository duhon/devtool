use serde_json::json;
use services_module_variables::{Variables, profiles};
use services_sdk::{DriverCall, Event, Module, cmd};

fn variables() -> Variables {
    Variables::with(profiles(
        &["var", "dev"],
        json!({
            "var": {"host": "local.test", "user": "{{dev:user}}"},
            "dev": {"host": "dev.test", "user": "admin", "api_token": "s3cr3t", "auth": "Bearer {{dev:api_token}}"},
        }),
        json!({"HOME": "/home/me", "host": "env.test"}),
    ))
}

fn resolve(variables: &Variables, text: &str) -> Result<String, String> {
    variables
        .resolve(text, false)
        .expect("variables resolves")
        .map(|resolution| resolution.text)
}

#[test]
fn references_take_the_profile_the_environment_or_the_first_profile_that_has_the_name() {
    let variables = variables();
    assert_eq!(
        resolve(
            &variables,
            "{{:host}} {{dev:host}} {{env:host}} {{var:user}} {{:HOME}}"
        )
        .unwrap(),
        "local.test dev.test env.test admin /home/me"
    );
    assert_eq!(
        resolve(&variables, "plain {{ text }}").unwrap(),
        "plain {{ text }}"
    );
    assert_eq!(
        resolve(&variables, "{{prod:host}}"),
        Err("Unknown profile: prod".into())
    );
    assert_eq!(
        resolve(&variables, "{{dev:nope}}"),
        Err("Unknown variable \"nope\" in profile \"dev\"".into())
    );
    assert_eq!(
        resolve(&variables, "{{env:NOPE}}"),
        Err("Missing environment variable(s): NOPE".into())
    );
}

#[test]
fn sensitive_values_come_back_as_secrets_and_json_mode_escapes() {
    let secret = Variables::with(profiles(
        &["var"],
        json!({"var": {"password": "p\"w", "token": "{{var:password}}"}}),
        json!({}),
    ));
    let resolution = secret
        .resolve("{\"p\":\"{{var:token}}\"}", true)
        .unwrap()
        .unwrap();
    assert_eq!(resolution.text, "{\"p\":\"p\\\"w\"}");
    assert_eq!(resolution.secrets, ["p\"w"]);
    let resolution = variables().resolve("{{dev:auth}}", false).unwrap().unwrap();
    assert_eq!(resolution.text, "Bearer s3cr3t");
    assert_eq!(resolution.secrets, ["s3cr3t"]);
}

#[test]
fn circular_references_fail_every_resolution_of_a_first_profile() {
    let variables = Variables::with(profiles(
        &["var"],
        json!({"var": {"a": "{{var:b}}", "b": "{{var:a}}"}}),
        json!({}),
    ));
    let error = resolve(&variables, "x").unwrap_err();
    assert!(error.starts_with("Circular profile reference:"), "{error}");
}

#[test]
fn requests_resolve_every_string_before_they_go() {
    let call = DriverCall::new(
        "http",
        &json!({"url": "https://{{:host}}/a", "headers": [["Authorization", "{{dev:auth}}"]], "n": 1}),
    );
    let call = variables().before_request(call).unwrap();
    assert_eq!(
        call.request(),
        json!({"url": "https://local.test/a", "headers": [["Authorization", "Bearer s3cr3t"]], "n": 1})
    );
    let call = DriverCall::new("http", &json!({"url": "{{nope:x}}"}));
    assert_eq!(
        variables().before_request(call).err(),
        Some("Unknown profile: nope".into())
    );
}

#[test]
fn profiles_load_on_start_and_again_when_they_change() {
    let mut variables = Variables::default();
    let commands = variables.update(Event::Init {
        settings: json!({}),
    });
    assert_eq!(
        commands,
        [cmd::kv_watch("profiles"), cmd::vars_profiles("profiles")]
    );
    variables.update(Event::Reply {
        tag: "profiles".into(),
        result: Ok(json!({"context": {
            "order": ["var"],
            "profiles": {"var": {"host": "x.test"}, "env": {"HOME": "/h"}},
        }})),
    });
    assert_eq!(
        resolve(&variables, "{{var:host}}{{env:HOME}}").unwrap(),
        "x.test/h"
    );
    assert_eq!(
        variables.update(Event::Storage {
            key: "profiles".into()
        }),
        [cmd::vars_profiles("profiles")]
    );
}
