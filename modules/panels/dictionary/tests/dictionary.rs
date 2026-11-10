use serde_json::{Value, json};
use services_module_dictionary::{Dictionary, FILE, Msg};
use services_sdk::{Cmd, Event, Module, cmd};

fn files(local: &Value) -> Value {
    json!([{"source": "Local", "text": local.to_string()}])
}

fn dictionary() -> Dictionary {
    Dictionary::with(files(&local()))
}

fn pass(dictionary: &Dictionary, text: &str) -> Result<String, String> {
    dictionary
        .resolve(text, false, false)
        .expect("dictionary resolves")
        .map(|resolution| resolution.text)
}

#[test]
fn sets_resolve_by_name_and_from_the_first_set_with_the_variable() {
    let dictionary = dictionary();
    assert_eq!(
        pass(
            &dictionary,
            "{{dev:URL}} {{stage:HOST}} {{mine:A}} {{:HOST}}"
        )
        .unwrap(),
        "https://dev.test/{{var:path}} stage.test dev.test dev.test"
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
    assert_eq!(fresh, [cmd::sources_read("fresh", FILE)]);
    let mut commands = dictionary.update(Event::Reply {
        tag: "fresh".into(),
        result: Ok(files(&stored)),
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
    let text = commands
        .iter()
        .find(|command| command.name == "sources.write" && command.tag == "save")
        .expect("saved")
        .args()["text"]
        .clone();
    serde_json::from_str(text.as_str().unwrap()).unwrap()
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
    json!({"order": ["dev", "stage", "mine"], "sets": {
        "dev": {"HOST": "dev.test", "URL": "https://{{dev:HOST}}/{{var:path}}"},
        "stage": {"HOST": "stage.test", "token": "t0k"},
        "mine": {"A": "{{:HOST}}"}}})
}

#[test]
fn edits_are_saved_at_once_into_the_local_file() {
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
        json!({"HOST": "edited.test", "URL": "https://{{dev:HOST}}/{{var:path}}", "PORT": "1"})
    );
    assert_eq!(status(&commands), "Dictionary save · OK");
    assert_eq!(pass(&dictionary, "{{dev:HOST}}").unwrap(), "edited.test");

    let commands = ui(&mut dictionary, Msg::Edit, "broken");
    assert!(commands.contains(&cmd::status(
        "ERROR: Line 1: expected NAME: value.",
        "error"
    )));
    assert_eq!(pass(&dictionary, "{{dev:HOST}}").unwrap(), "edited.test");
    let view = format!("{:?}", dictionary.view());
    assert!(
        !view.contains("Reset") && !view.contains("From source"),
        "{view}"
    );
}

#[test]
fn sets_are_renamed_and_reordered() {
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
    assert_eq!(saved(&commands)["order"], json!(["dev", "stage", "ours"]));
    let stored = saved(&commands);

    let commands = ui(&mut dictionary, Msg::Rename("ours".into()), "dev");
    assert_eq!(
        status(&commands),
        "Dictionary rename · ERROR · A set named \"dev\" already exists"
    );
    let view = format!("{:?}", dictionary.view());
    assert!(
        view.contains("on_rename: Some"),
        "sets rename in place: {view}"
    );

    ui(
        &mut dictionary,
        Msg::Reorder,
        r#"["dictionary-set-ours","dictionary-set-dev"]"#,
    );
    let commands = transact(&mut dictionary, stored);
    assert_eq!(status(&commands), "Dictionary reorder · OK");
    assert_eq!(saved(&commands)["order"], json!(["ours", "dev", "stage"]));
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
        tag: "sources".into(),
        result: Ok(files(&other)),
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
    assert!(commands.contains(&cmd::sources_read("sources", FILE)));
    dictionary.update(Event::Reply {
        tag: "sources".into(),
        result: Ok(files(&other)),
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
        json!(["dev", "stage", "mine", "qa"])
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
    dictionary.update(Event::Reply {
        tag: "sources".into(),
        result: Ok(files(
            &json!({"order": ["mine"], "sets": {"mine": {"A": "other"}}}),
        )),
    });
    let view = format!("{:?}", dictionary.view());
    assert!(view.contains("\"A: other\""), "{view}");
}
