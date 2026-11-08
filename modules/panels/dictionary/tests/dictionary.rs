use serde_json::{Value, json};
use services_module_dictionary::{Dictionary, LOCAL, Msg};
use services_sdk::{Event, Module, cmd};

fn dictionary() -> Dictionary {
    Dictionary::with(
        json!([
            {"source": "Base", "text": r#"{"profiles": {"dev": {"HOST": "base.test"}}}"#},
            {"source": "Team", "text": r#"{"sets": {"dev": {"HOST": "dev.test", "URL": "https://{{dev:HOST}}/{{var:path}}"}, "stage": {"HOST": "stage.test", "token": "s3"}}}"#},
        ]),
        json!({"order": ["mine"], "sets": {"stage": {"HOST": "mine.test", "token": "t0k"}, "mine": {"A": "{{:HOST}}"}}}),
    )
}

fn pass(dictionary: &Dictionary, text: &str) -> Result<String, String> {
    dictionary
        .resolve(text, false, false)
        .expect("dictionary resolves")
        .map(|resolution| resolution.text)
}

#[test]
fn sets_layer_sources_then_the_users_own() {
    let dictionary = dictionary();
    assert_eq!(
        pass(
            &dictionary,
            "{{dev:URL}} {{stage:HOST}} {{mine:A}} {{:HOST}}"
        )
        .unwrap(),
        "https://dev.test/{{var:path}} mine.test dev.test dev.test"
    );
    assert_eq!(
        pass(&dictionary, "{{var:x}} {{:nope}}").unwrap(),
        "{{var:x}} {{:nope}}"
    );
    assert_eq!(
        pass(&dictionary, "{{dev:nope}}"),
        Err("Unknown variable \"nope\" in dev".into())
    );
    let resolution = dictionary
        .resolve("{{stage:token}}", false, false)
        .unwrap()
        .unwrap();
    assert_eq!(resolution.secrets, ["t0k"]);
}

#[test]
fn edits_replace_the_set_and_reset_restores_the_source() {
    let mut dictionary = dictionary();
    dictionary.update(Event::Ui {
        msg: Msg::Tab(String::new()),
        value: Some("dictionary-set-dev".into()),
    });
    let commands = dictionary.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("HOST: edited.test".into()),
    });
    assert!(
        commands
            .iter()
            .any(|command| command.name == "kv.put" && command.tag == "save")
    );
    assert_eq!(pass(&dictionary, "{{dev:HOST}}").unwrap(), "edited.test");
    let commands = dictionary.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("broken".into()),
    });
    assert!(commands.contains(&cmd::status(
        "ERROR: Line 1: expected NAME: value.",
        "error"
    )));
    assert_eq!(pass(&dictionary, "{{dev:HOST}}").unwrap(), "edited.test");
    dictionary.update(Event::Ui {
        msg: Msg::Reset,
        value: None,
    });
    assert_eq!(pass(&dictionary, "{{dev:HOST}}").unwrap(), "dev.test");
}

#[test]
fn new_sets_are_named_like_references() {
    let mut dictionary = dictionary();
    let commands = dictionary.update(Event::Ui {
        msg: Msg::Add,
        value: Some("qa".into()),
    });
    let saved = commands
        .iter()
        .find(|command| command.tag == "add")
        .expect("saved");
    assert_eq!(saved.args()["key"], LOCAL);
    assert_eq!(saved.args()["value"]["order"], json!(["mine", "qa"]));
    let commands = dictionary.update(Event::Ui {
        msg: Msg::Add,
        value: Some("bad name".into()),
    });
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].args()["indicator"], Value::from("error"));
}

#[test]
fn the_editor_keeps_the_text_as_typed() {
    let mut dictionary = dictionary();
    dictionary.update(Event::Ui {
        msg: Msg::Tab(String::new()),
        value: Some("dictionary-set-dev".into()),
    });
    let typed = "HOST: edited.test\n\nPORT: ";
    dictionary.update(Event::Ui {
        msg: Msg::Edit,
        value: Some(typed.into()),
    });
    let view = format!("{:?}", dictionary.view());
    assert!(view.contains(&format!("{typed:?}")), "{view}");
    assert_eq!(pass(&dictionary, "{{dev:HOST}}").unwrap(), "edited.test");
}

#[test]
fn another_windows_save_replaces_the_typed_text() {
    let mut dictionary = dictionary();
    dictionary.update(Event::Ui {
        msg: Msg::Tab(String::new()),
        value: Some("dictionary-set-mine".into()),
    });
    dictionary.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("A: typed\n\n".into()),
    });
    let commands = dictionary.update(Event::Storage { key: LOCAL.into() });
    assert_eq!(commands, [cmd::kv_get(LOCAL, LOCAL)]);
    dictionary.update(Event::Reply {
        tag: LOCAL.into(),
        result: Ok(json!({"order": ["mine"], "sets": {"mine": {"A": "other"}}})),
    });
    let view = format!("{:?}", dictionary.view());
    assert!(view.contains("\"A: other\""), "{view}");
}
