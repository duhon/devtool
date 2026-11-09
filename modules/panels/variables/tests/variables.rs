use serde_json::json;
use services_module_variables::{Msg, VAR, Variables};
use services_sdk::{Event, Module, cmd};

fn variables() -> Variables {
    Variables::with(
        json!({"host": "local.test", "user": "{{:login}}", "password": "p\"w", "token": "{{var:password}}"}),
        json!({"HOME": "/home/me", "host": "env.test"}),
    )
}

fn pass(variables: &Variables, text: &str, last: bool) -> Result<String, String> {
    variables
        .resolve(text, false, last)
        .expect("variables resolves")
        .map(|resolution| resolution.text)
}

#[test]
fn resolves_only_what_it_owns_until_the_last_pass() {
    let variables = variables();
    assert_eq!(
        pass(
            &variables,
            "{{:host}} {{var:host}} {{env:host}} {{dev:x}} {{:HOME}}",
            false
        )
        .unwrap(),
        "local.test local.test env.test {{dev:x}} {{:HOME}}"
    );
    // A value's foreign reference stays for the panel that owns it.
    assert_eq!(
        pass(&variables, "{{var:user}}", false).unwrap(),
        "{{:login}}"
    );
    assert_eq!(pass(&variables, "{{:HOME}}", true).unwrap(), "/home/me");
    assert_eq!(
        pass(&variables, "{{:nope}} {{env:NOPE}}", true),
        Err("Missing environment variable(s): nope, NOPE".into())
    );
    assert_eq!(
        pass(&variables, "{{var:nope}}", false),
        Err("Unknown variable \"nope\" in var".into())
    );
}

#[test]
fn sensitive_values_come_back_as_secrets_and_json_mode_escapes() {
    let resolution = variables()
        .resolve("{\"p\":\"{{var:token}}\"}", true, false)
        .unwrap()
        .unwrap();
    assert_eq!(resolution.text, "{\"p\":\"p\\\"w\"}");
    assert_eq!(resolution.secrets, ["p\"w"]);
}

#[test]
fn circular_references_fail() {
    let variables = Variables::with(json!({"a": "{{var:b}}", "b": "{{:a}}"}), json!({}));
    let error = pass(&variables, "{{:a}}", false).unwrap_err();
    assert!(error.starts_with("Circular variable reference:"), "{error}");
}

#[test]
fn edits_save_the_session_variables_or_keep_a_draft() {
    let mut variables = Variables::default();
    let commands = variables.update(Event::Init {
        settings: json!({}),
    });
    assert!(commands.contains(&cmd::kv_get(VAR, VAR)));
    assert!(commands.contains(&cmd::env("env")));
    let commands = variables.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("A: 1".into()),
    });
    assert_eq!(
        commands[0],
        cmd::kv_put("save", VAR, &json!({"A": "1"}), true)
    );
    assert_eq!(pass(&variables, "{{var:A}}", false).unwrap(), "1");
    let commands = variables.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("missing delimiter".into()),
    });
    assert_eq!(
        commands[1],
        cmd::status(
            "Variables validation · ERROR · Line 1: expected NAME: value.",
            "error"
        )
    );
    // The last saved variables still resolve.
    assert_eq!(pass(&variables, "{{var:A}}", false).unwrap(), "1");
}
