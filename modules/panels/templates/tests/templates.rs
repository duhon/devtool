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
fn unparsable_edits_are_saved_with_their_error() {
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
    assert!(
        commands
            .iter()
            .any(|command| command.name == "sources.write")
    );
    assert_eq!(templates.library()["rest"]["headers"], "no name");
}

fn select(templates: &mut Templates, tab: &str) {
    templates.update(Event::Ui {
        msg: Msg::Section(String::new()),
        value: Some(format!("templates-section-{tab}")),
    });
}

#[test]
fn global_edits_are_stored_under_star() {
    let mut templates = templates();
    select(&mut templates, "@global");
    let commands = templates.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("@host(service: {{:services}}) Host\n{{service}}".into()),
    });
    let library = templates.library();
    assert!(!library["rest"].contains_key("*"));
    assert_eq!(
        library["*"]["*"],
        "@host(service: {{:services}}) Host\n{{service}}"
    );
    assert_eq!(library["rest"]["requests"], "@list Lists\nGET /items");
    let contributed = commands
        .iter()
        .find(|command| command.name == "editor.contribute")
        .expect("contributed");
    assert_eq!(contributed.args()["templates"]["*"]["*"], library["*"]["*"]);
}

#[test]
fn global_syntax_errors_are_saved_and_shown_with_a_line_number() {
    let mut templates = templates();
    select(&mut templates, "@global");
    let text = "@ok Fine\nx\n@bad(a: ) Broken";
    let commands = templates.update(Event::Ui {
        msg: Msg::Edit,
        value: Some(text.into()),
    });
    let status = commands
        .iter()
        .find(|command| command.args()["indicator"] == "error")
        .expect("error");
    assert!(status.args()["text"].as_str().unwrap().contains("Line 3"));
    assert_eq!(templates.library()["*"]["*"], text);
    let saved = templates.update(Event::Reply {
        tag: "save".into(),
        result: Ok(serde_json::Value::Null),
    });
    assert_eq!(saved[0].args()["indicator"], "error");
}
