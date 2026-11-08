use serde_json::json;
use services_module_hints::{Hints, Msg, OWN, validate};
use services_sdk::{Event, Module};

fn reply(hints: &mut Hints, tag: &str, value: serde_json::Value) -> Vec<services_sdk::Cmd> {
    hints.update(Event::Reply {
        tag: tag.into(),
        result: Ok(value),
    })
}

fn contributed(commands: &[services_sdk::Cmd]) -> serde_json::Value {
    commands
        .iter()
        .find(|command| command.name == "editor.contribute")
        .map(|command| command.args()["hints"].clone())
        .expect("contributes hints")
}

#[test]
fn own_hints_replace_the_source_until_reset() {
    let mut hints = Hints::default();
    let commands = reply(
        &mut hints,
        "document",
        json!({"service": "rest", "services": [{"id": "rest", "name": "REST"}]}),
    );
    assert_eq!(commands[0].args()["name"], "hints/rest.md");
    let commands = reply(
        &mut hints,
        "source:rest",
        json!([{"source": "Team", "text": "# Paths\n/users"}]),
    );
    assert_eq!(contributed(&commands), json!({"rest": "# Paths\n/users"}));

    let commands = hints.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("# Mine\n/me".into()),
    });
    assert!(
        commands
            .iter()
            .any(|command| command.name == "kv.put" && command.args()["key"] == OWN)
    );
    assert_eq!(contributed(&commands), json!({"rest": "# Mine\n/me"}));

    let commands = hints.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("## rest".into()),
    });
    assert!(
        commands
            .iter()
            .all(|command| command.name != "editor.contribute")
    );
    assert_eq!(hints.effective("rest"), "# Mine\n/me");

    let commands = hints.update(Event::Ui {
        msg: Msg::Reset,
        value: None,
    });
    assert_eq!(contributed(&commands), json!({"rest": "# Paths\n/users"}));
}

#[test]
fn validation_reads_like_completion() {
    assert!(validate("# Section\n{{:host}}/users").is_ok());
    assert!(
        validate("{{host}}")
            .unwrap_err()
            .contains("invalid variable reference")
    );
    assert!(validate("{{:host").unwrap_err().contains("incomplete"));
}
