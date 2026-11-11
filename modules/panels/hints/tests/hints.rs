use serde_json::json;
use services_module_hints::{Hints, Msg, validate};
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
fn hints_are_read_from_and_saved_to_the_local_source() {
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
        json!([{"source": "Local", "text": "# Paths\n/users"}]),
    );
    assert_eq!(contributed(&commands), json!({"rest": "# Paths\n/users"}));

    let commands = hints.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("# Mine\n/me".into()),
    });
    let save = commands
        .iter()
        .find(|command| command.name == "sources.write")
        .expect("saves into Local");
    assert_eq!(save.args()["name"], "hints/rest.md");
    assert_eq!(save.args()["text"], "# Mine\n/me");
    assert_eq!(contributed(&commands), json!({"rest": "# Mine\n/me"}));
    let saved = |commands: &[services_sdk::Cmd]| {
        commands.iter().any(|command| {
            command.name == "status.set" && command.args()["text"] == "Hints save · OK"
        })
    };
    let tags = |commands: &[services_sdk::Cmd]| -> Vec<String> {
        commands
            .iter()
            .filter(|command| {
                ["sources.write", "kv.delete", "kv.put"].contains(&command.name.as_str())
            })
            .map(|command| command.tag.clone())
            .collect()
    };
    let first = tags(&commands);
    let second = tags(&hints.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("# Mine\n/me\n/you".into()),
    }));
    assert_eq!(first.len() + second.len(), 4);
    assert!(
        first.iter().all(|tag| !second.contains(tag)),
        "Saves in flight together must not share tags, or one's answers are lost"
    );
    for tag in first.iter().chain(&second[..1]) {
        assert!(!saved(&reply(&mut hints, tag, json!(null))));
    }
    assert!(
        saved(&reply(&mut hints, &second[1], json!(null))),
        "OK shows once the files and the session have every save"
    );

    let commands = hints.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("## rest".into()),
    });
    assert!(
        commands
            .iter()
            .all(|command| command.name != "editor.contribute")
    );
    assert_eq!(hints.effective("rest"), "# Mine\n/me\n/you");
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
