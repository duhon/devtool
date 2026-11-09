use serde_json::{Value, json};
use services_module_dictionary::{Dictionary, LOCAL, Msg};
use services_sdk::{Cmd, Event, Module, cmd};

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

/// Runs the queued changes: the lock, the fresh layer (`stored`), the save.
fn transact(dictionary: &mut Dictionary, stored: Value) -> Vec<Cmd> {
    let fresh = dictionary.update(Event::Reply {
        tag: "dictionary".into(),
        result: Ok(Value::Null),
    });
    assert_eq!(fresh, [cmd::kv_get("fresh", LOCAL)]);
    let mut commands = dictionary.update(Event::Reply {
        tag: "fresh".into(),
        result: Ok(stored),
    });
    if commands.iter().any(|command| command.tag == "save") {
        commands.extend(dictionary.update(Event::Reply {
            tag: "save".into(),
            result: Ok(Value::Null),
        }));
    }
    commands
}

fn saved(commands: &[Cmd]) -> Value {
    commands
        .iter()
        .find(|command| command.name == "kv.put" && command.tag == "save")
        .expect("saved")
        .args()["value"]
        .clone()
}

fn status(commands: &[Cmd]) -> String {
    commands
        .iter()
        .rev()
        .find(|command| command.name == "status.set")
        .map(|command| command.args()["text"].as_str().unwrap().to_owned())
        .unwrap_or_default()
}

fn ui(dictionary: &mut Dictionary, msg: Msg, value: &str) -> Vec<Cmd> {
    dictionary.update(Event::Ui {
        msg,
        value: Some(value.into()),
    })
}

fn local() -> Value {
    json!({"order": ["mine"], "sets": {"stage": {"HOST": "mine.test", "token": "t0k"}, "mine": {"A": "{{:HOST}}"}}})
}

#[test]
fn edits_keep_only_overrides_of_a_source_set_and_reset_restores_it() {
    let mut dictionary = dictionary();
    ui(
        &mut dictionary,
        Msg::Tab(String::new()),
        "dictionary-set-dev",
    );
    let commands = ui(
        &mut dictionary,
        Msg::Edit,
        "HOST: edited.test\nURL: https://{{dev:HOST}}/{{var:path}}\nPORT: 1",
    );
    assert!(commands.contains(&cmd::lock("dictionary", "dictionary")));
    let commands = transact(&mut dictionary, local());
    assert_eq!(
        saved(&commands)["sets"]["dev"],
        json!({"HOST": "edited.test", "PORT": "1"}),
        "only what differs from the source is kept"
    );
    assert_eq!(status(&commands), "Dictionary save · OK");
    assert_eq!(pass(&dictionary, "{{dev:HOST}}").unwrap(), "edited.test");
    let view = format!("{:?}", dictionary.view());
    assert!(
        view.contains("dictionary-reset-variable-HOST")
            && view.contains("dictionary-reset-variable-PORT")
            && !view.contains("dictionary-reset-variable-URL"),
        "{view}"
    );

    let commands = ui(&mut dictionary, Msg::Edit, "broken");
    assert!(commands.contains(&cmd::status(
        "ERROR: Line 1: expected NAME: value.",
        "error"
    )));
    assert_eq!(pass(&dictionary, "{{dev:HOST}}").unwrap(), "edited.test");

    // Deleting a source's variable is refused.
    ui(&mut dictionary, Msg::Edit, "HOST: x");
    let stored = json!({"order": ["mine"], "sets": {"dev": {"HOST": "edited.test", "PORT": "1"}}});
    let commands = transact(&mut dictionary, stored.clone());
    assert_eq!(
        status(&commands),
        "Dictionary save · ERROR · dev:URL comes from source Team; reset it instead of deleting it"
    );

    // One variable goes back to the source; one the source lacks goes.
    dictionary.update(Event::Ui {
        msg: Msg::Reset(Some("PORT".into())),
        value: None,
    });
    let commands = transact(&mut dictionary, stored);
    assert_eq!(
        saved(&commands)["sets"]["dev"],
        json!({"HOST": "edited.test"})
    );
    assert_eq!(status(&commands), "Dictionary reset · OK");
    dictionary.update(Event::Ui {
        msg: Msg::Reset(None),
        value: None,
    });
    let commands = transact(&mut dictionary, saved(&commands));
    assert_eq!(saved(&commands)["sets"].get("dev"), None);
    assert_eq!(pass(&dictionary, "{{dev:HOST}}").unwrap(), "dev.test");
}

#[test]
fn own_sets_are_renamed_and_reordered_but_source_sets_are_not() {
    let mut dictionary = dictionary();
    ui(
        &mut dictionary,
        Msg::Tab(String::new()),
        "dictionary-set-mine",
    );
    ui(&mut dictionary, Msg::Rename("mine".into()), "ours");
    assert_eq!(pass(&dictionary, "{{ours:A}}").unwrap(), "dev.test");
    let commands = transact(&mut dictionary, local());
    assert_eq!(status(&commands), "Dictionary rename · OK");
    assert_eq!(saved(&commands)["order"], json!(["ours", "dev", "stage"]));
    let stored = saved(&commands);

    let commands = ui(&mut dictionary, Msg::Rename("ours".into()), "dev");
    assert_eq!(
        status(&commands),
        "Dictionary rename · ERROR · A set named \"dev\" already exists"
    );
    let view = format!("{:?}", dictionary.view());
    assert!(
        view.contains("on_rename: Some"),
        "own sets rename in place: {view}"
    );

    ui(
        &mut dictionary,
        Msg::Reorder,
        r#"["dictionary-set-stage","dictionary-set-ours"]"#,
    );
    let commands = transact(&mut dictionary, stored);
    assert_eq!(status(&commands), "Dictionary reorder · OK");
    assert_eq!(saved(&commands)["order"], json!(["stage", "ours", "dev"]));
}

#[test]
fn a_draft_started_before_another_window_saved_conflicts_until_discarded() {
    let mut dictionary = dictionary();
    ui(
        &mut dictionary,
        Msg::Tab(String::new()),
        "dictionary-set-mine",
    );
    // An invalid draft is not saved: it starts from the set as it was.
    ui(
        &mut dictionary,
        Msg::Edit,
        "A: {{:HOST}}\nmissing delimiter",
    );
    let other = json!({"order": ["mine"], "sets": {"mine": {"A": "other window"}}});
    dictionary.update(Event::Reply {
        tag: LOCAL.into(),
        result: Ok(other.clone()),
    });
    let commands = ui(&mut dictionary, Msg::Edit, "A: stale");
    assert!(commands.contains(&cmd::lock("dictionary", "dictionary")));
    let commands = transact(&mut dictionary, other.clone());
    assert!(
        status(&commands).contains("CONFLICT: set mine changed in another window"),
        "{commands:?}"
    );
    assert!(!commands.iter().any(|command| command.tag == "save"));
    let view = format!("{:?}", dictionary.view());
    assert!(
        view.contains("A: stale") && view.contains("console-dictionary-discard"),
        "the draft stays: {view}"
    );
    // Editing it again still conflicts.
    ui(&mut dictionary, Msg::Edit, "A: stale again");
    let commands = transact(&mut dictionary, other.clone());
    assert!(status(&commands).contains("CONFLICT"), "{commands:?}");

    let commands = dictionary.update(Event::Ui {
        msg: Msg::Discard,
        value: None,
    });
    assert!(commands.contains(&cmd::kv_get(LOCAL, LOCAL)));
    dictionary.update(Event::Reply {
        tag: LOCAL.into(),
        result: Ok(other),
    });
    let view = format!("{:?}", dictionary.view());
    assert!(
        view.contains("A: other window") && !view.contains("console-dictionary-discard"),
        "{view}"
    );
}

#[test]
fn new_sets_are_named_like_references() {
    let mut dictionary = dictionary();
    ui(&mut dictionary, Msg::Add, "qa");
    let commands = transact(&mut dictionary, local());
    assert_eq!(status(&commands), "Dictionary add · OK");
    assert_eq!(
        saved(&commands)["order"],
        json!(["mine", "dev", "stage", "qa"])
    );
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
        value: Some("dictionary-set-mine".into()),
    });
    let typed = "A: edited.test\n\nPORT: ";
    dictionary.update(Event::Ui {
        msg: Msg::Edit,
        value: Some(typed.into()),
    });
    let view = format!("{:?}", dictionary.view());
    assert!(view.contains(&format!("{typed:?}")), "{view}");
    assert_eq!(pass(&dictionary, "{{mine:A}}").unwrap(), "edited.test");
}

#[test]
fn another_windows_save_replaces_the_shown_set() {
    let mut dictionary = dictionary();
    ui(
        &mut dictionary,
        Msg::Tab(String::new()),
        "dictionary-set-mine",
    );
    let commands = dictionary.update(Event::Storage { key: LOCAL.into() });
    assert_eq!(commands, [cmd::kv_get(LOCAL, LOCAL)]);
    dictionary.update(Event::Reply {
        tag: LOCAL.into(),
        result: Ok(json!({"order": ["mine"], "sets": {"mine": {"A": "other"}}})),
    });
    let view = format!("{:?}", dictionary.view());
    assert!(view.contains("\"A: other\""), "{view}");
}
