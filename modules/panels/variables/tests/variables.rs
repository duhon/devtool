use serde_json::{Value, json};
use services_module_variables::{Msg, VAR, Variables};
use services_sdk::{Event, Module, cmd};

fn tree(commands: &[services_sdk::Cmd]) -> Value {
    let contribute = commands
        .iter()
        .find(|command| command.name == "editor.contribute")
        .expect("contributes");
    serde_json::from_slice::<Value>(&contribute.args).unwrap()["tree"].clone()
}

#[test]
fn it_contributes_var_first_and_env_last() {
    let mut variables = Variables::default();
    variables.update(Event::Init {
        settings: json!({}),
    });
    let commands = variables.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("host: a.test\nhosts:\n  catalog: c.test".into()),
    });
    assert_eq!(
        commands[0],
        cmd::kv_put(
            "save",
            VAR,
            &json!({"host": "a.test", "hosts": {"catalog": "c.test"}}),
            true
        )
    );
    assert_eq!(
        tree(&commands),
        json!({
            "first": [["var", {"host": "a.test", "hosts": {"catalog": "c.test"}}]],
            "last": [["env", {}]],
        })
    );
}

#[test]
fn the_environment_is_the_last_panel() {
    let mut variables = Variables::default();
    let commands = variables.update(Event::Reply {
        tag: "env".into(),
        result: Ok(json!({"HOME": "/home/me"})),
    });
    assert_eq!(
        tree(&commands)["last"],
        json!([["env", {"HOME": "/home/me"}]])
    );
}

#[test]
fn a_draft_that_does_not_parse_is_kept_and_the_last_variables_stay() {
    let mut variables = Variables::default();
    variables.update(Event::Init {
        settings: json!({}),
    });
    variables.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("A: 1".into()),
    });
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
}
