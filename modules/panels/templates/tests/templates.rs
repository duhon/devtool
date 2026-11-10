use serde_json::json;
use services_module_templates::{Msg, Templates};
use services_sdk::{Event, Module};

fn templates() -> Templates {
    Templates::with(
        "rest",
        &["requests", "headers"],
        json!([{"source": "Local", "text": r#"{"rest": {"requests": "@list Lists\nGET /items"}}"#}]),
    )
}

#[test]
fn edits_are_saved_at_once_into_the_local_file() {
    let mut templates = templates();
    let commands = templates.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("@mine Mine\nGET /mine".into()),
    });
    let saved = commands
        .iter()
        .find(|command| command.name == "sources.write")
        .expect("saved");
    assert_eq!(saved.args()["name"], "templates.json");
    let file: serde_json::Value =
        serde_json::from_str(saved.args()["text"].as_str().unwrap()).unwrap();
    assert_eq!(file["rest"]["requests"], "@mine Mine\nGET /mine");
    assert_eq!(
        templates.library()["rest"]["requests"],
        "@mine Mine\nGET /mine"
    );
}

#[test]
fn unparsable_edits_stay_drafts_of_their_section() {
    let mut templates = templates();
    templates.update(Event::Ui {
        msg: Msg::Section(String::new()),
        value: Some("templates-section-headers".into()),
    });
    let commands = templates.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("no name".into()),
    });
    assert!(
        commands
            .iter()
            .any(|command| command.args()["indicator"] == "error")
    );
    assert!(!templates.library()["rest"].contains_key("headers"));
}
