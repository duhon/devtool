use serde_json::json;
use services_module_templates::{LIBRARY, Msg, Templates};
use services_sdk::{Event, Module};

fn templates() -> Templates {
    Templates::with(
        "rest",
        &["requests", "headers"],
        json!([{"source": "Team", "text": r#"{"rest": {"requests": "@list Lists\nGET /items"}}"#}]),
        json!({}),
    )
}

#[test]
fn edits_replace_the_section_and_reset_restores_the_source() {
    let mut templates = templates();
    let commands = templates.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("@mine Mine\nGET /mine".into()),
    });
    let saved = commands
        .iter()
        .find(|command| command.tag == "save")
        .expect("saved");
    assert_eq!(saved.args()["key"], LIBRARY);
    assert_eq!(
        templates.library()["rest"]["requests"],
        "@mine Mine\nGET /mine"
    );

    templates.update(Event::Ui {
        msg: Msg::Reset,
        value: None,
    });
    assert_eq!(
        templates.library()["rest"]["requests"],
        "@list Lists\nGET /items"
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
