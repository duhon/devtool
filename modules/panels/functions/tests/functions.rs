use serde_json::json;
use services_module_functions::{Functions, Msg};
use services_sdk::{Cmd, Event, Module};

fn functions() -> Functions {
    Functions::with(
        "alpha",
        json!([{"source": "Local", "text": r#"{"*": "def g: 1;\n"}"#}]),
    )
}

fn edit(functions: &mut Functions, text: &str) -> Vec<Cmd> {
    functions.update(Event::Ui {
        msg: Msg::Edit,
        value: Some(text.into()),
    })
}

fn select(functions: &mut Functions, tab: &str) {
    functions.update(Event::Ui {
        msg: Msg::Tab(String::new()),
        value: Some(format!("functions-tab-{tab}")),
    });
}

fn status(commands: &[Cmd]) -> Option<(String, String)> {
    commands.iter().find(|c| c.name == "status.set").map(|c| {
        (
            c.args()["text"].as_str().unwrap_or_default().to_owned(),
            c.args()["indicator"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        )
    })
}

#[test]
fn service_edits_are_saved_and_contributed() {
    let mut functions = functions();
    let commands = edit(&mut functions, "def pick(k): .tree[k];");
    let saved = commands
        .iter()
        .find(|c| c.name == "sources.write")
        .expect("saved");
    assert_eq!(saved.args()["name"], "functions.json");
    let file: serde_json::Value =
        serde_json::from_str(saved.args()["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        file,
        json!({"*": "def g: 1;\n", "alpha": "def pick(k): .tree[k];"})
    );
    let contributed = commands
        .iter()
        .find(|c| c.name == "editor.contribute")
        .expect("contributed");
    assert_eq!(contributed.args()["functions"], file);
    assert!(status(&commands).is_none());
}

#[test]
fn errors_are_saved_and_shown_with_the_tab_and_its_own_line() {
    let mut functions = functions();
    let commands = edit(&mut functions, "def a: 1;\ndef b: nope;");
    assert!(commands.iter().any(|c| c.name == "sources.write"));
    let (text, indicator) = status(&commands).expect("status");
    assert_eq!(indicator, "error");
    assert!(text.starts_with("ERROR: alpha: Line 2"), "{text}");
    assert_eq!(functions.library()["alpha"], "def a: 1;\ndef b: nope;");

    // A service definition may use a global one.
    let commands = edit(&mut functions, "def b: g;");
    assert!(status(&commands).is_none());

    select(&mut functions, "@global");
    let commands = edit(&mut functions, "def g: (;");
    let (text, indicator) = status(&commands).expect("status");
    assert_eq!(indicator, "error");
    assert!(text.starts_with("ERROR: Global: Line 1"), "{text}");
    assert_eq!(functions.library()["*"], "def g: (;");
    assert_eq!(functions.library()["alpha"], "def b: g;");
}
